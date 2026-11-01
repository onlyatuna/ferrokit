// SPDX-License-Identifier: LGPL-3.0-only
//! A submenu that opens to the left grows the popup window leftwards. The parent's
//! pixels must then sit at a whole-device-pixel offset from the native window origin, so they match
//! the frame drawn with the submenu closed. `Menu::present_popup` moves the parent by
//! `native_root - native_win` in device px (`translate_device`); a logical `translate` would scale
//! the same offset by dpr and shift the parent by a fractional device amount.
//! qtbase/src/widgets/widgets/qmenu.cpp:3677-3688 places the submenu at a logical point and
//! qtbase/src/gui/kernel/qhighdpiscaling_p.h:150-153 converts each native window origin to
//! whole device px, so the parent never moves by a fractional device amount.
#![cfg(windows)]

use ferrokit_gui::geometry::primitives::{Point, Rect};
use ferrokit_gui::paint::Pixmap;
use ferrokit_gui::text::font_database::set_application_device_pixel_ratio;
use ferrokit_platform::platform_window::PlatformWindow;
use ferrokit_platform::screen::PlatformScreen;
use ferrokit_platform::size_limits::WINDOW_SIZE_MAX;
use ferrokit_platform::window_system_interface::WindowSystemEventHandler;
use ferrokit_platform::{
    GenericPlatformIntegration, PlatformIntegration, WindowFlags, set_platform_integration,
};
use ferrokit_widgets::{Action, Menu, Widget};
use parking_lot::Mutex;
use std::sync::Arc;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{VK_DOWN, VK_RIGHT};
use windows_sys::Win32::UI::WindowsAndMessaging::{PostThreadMessageW, WM_CANCELMODE, WM_KEYDOWN};

/// `SUBMENU_OVERLAP` (menu.rs:35).
const SUBMENU_OVERLAP: i32 = 2;
/// Logical columns at the parent's left edge that the open submenu may cover; not compared.
const MARGIN: i32 = 8;
const TASKBAR: i32 = 40;

fn primary_rect() -> Rect {
    Rect::new(0, 0, 1920, 1080)
}

/// Restores the application dpr even if an assertion panics.
struct DprGuard;

impl DprGuard {
    fn set(dpr: f32) -> Self {
        set_application_device_pixel_ratio(dpr);
        DprGuard
    }
}

impl Drop for DprGuard {
    fn drop(&mut self) {
        set_application_device_pixel_ratio(1.0);
    }
}

#[derive(Clone)]
struct Frame {
    w: i32,
    h: i32,
    px: Vec<u8>,
    /// Native origin of the window at the time this frame was presented.
    target: Option<Point>,
}

#[derive(Default)]
struct Log {
    frames: Vec<Frame>,
    created: Vec<Rect>,
}

struct FakeScreen {
    dpr: f32,
}

impl PlatformScreen for FakeScreen {
    fn name(&self) -> String {
        "primary".to_string()
    }
    fn geometry(&self) -> Rect {
        primary_rect()
    }
    fn available_geometry(&self) -> Rect {
        let g = primary_rect();
        Rect::new(g.x, g.y, g.width, g.height - TASKBAR)
    }
    fn is_primary(&self) -> bool {
        true
    }
    fn device_pixel_ratio(&self) -> f32 {
        self.dpr
    }
}

struct FakeWindow {
    geometry: Rect,
    min: (i32, i32),
    max: (i32, i32),
    dpr: f32,
    target: Option<Point>,
    log: Arc<Mutex<Log>>,
}

impl PlatformWindow for FakeWindow {
    fn show(&self) {}
    fn hide(&self) {}
    fn is_active(&self) -> bool {
        false
    }
    fn geometry(&self) -> Rect {
        self.geometry
    }
    fn set_geometry(&mut self, rect: Rect) {
        self.geometry = rect;
    }
    fn set_stays_on_top(&mut self, _enabled: bool) {}
    fn set_click_through(&mut self, _enabled: bool) {}
    fn set_opacity(&mut self, _opacity: f32) {}
    fn opacity(&self) -> f32 {
        1.0
    }
    fn set_minimum_size(&mut self, w: i32, h: i32) {
        self.min = (w, h);
    }
    fn minimum_size(&self) -> (i32, i32) {
        self.min
    }
    fn set_maximum_size(&mut self, w: i32, h: i32) {
        self.max = (w, h);
    }
    fn maximum_size(&self) -> (i32, i32) {
        self.max
    }
    fn device_pixel_ratio(&self) -> f32 {
        self.dpr
    }
    fn set_target_pos(&mut self, pos: Option<ferrokit_gui::geometry::Point>) {
        self.target = pos;
    }
    fn present(&mut self, pixmap: &mut Pixmap, _opacity: f32) -> Result<(), &'static str> {
        self.log.lock().frames.push(Frame {
            w: pixmap.physical_width() as i32,
            h: pixmap.physical_height() as i32,
            px: pixmap.data().to_vec(),
            target: self.target,
        });
        Ok(())
    }
    fn set_event_handler(&mut self, _handler: Box<dyn WindowSystemEventHandler>) {}
    fn native_handle(&self) -> isize {
        0
    }
}

struct FakePlatform {
    inner: GenericPlatformIntegration,
    dpr: f32,
    log: Arc<Mutex<Log>>,
}

impl PlatformIntegration for FakePlatform {
    fn create_window(
        &self,
        _title: &str,
        rect: Rect,
        _flags: WindowFlags,
    ) -> Result<Box<dyn PlatformWindow>, &'static str> {
        self.log.lock().created.push(rect);
        Ok(Box::new(FakeWindow {
            geometry: rect,
            min: (0, 0),
            max: (WINDOW_SIZE_MAX, WINDOW_SIZE_MAX),
            dpr: self.dpr,
            target: None,
            log: Arc::clone(&self.log),
        }))
    }
    fn create_tray_icon(
        &self,
        tooltip: &str,
        icon: &ferrokit_gui::image::Icon,
    ) -> Result<Box<dyn ferrokit_platform::platform_tray::PlatformTrayIcon>, &'static str> {
        self.inner.create_tray_icon(tooltip, icon)
    }
    fn primary_screen(&self) -> Box<dyn PlatformScreen> {
        Box::new(FakeScreen { dpr: self.dpr })
    }
    fn screens(&self) -> Vec<Box<dyn PlatformScreen>> {
        vec![self.primary_screen()]
    }
    fn screen_at(&self, pos: Point) -> Option<Box<dyn PlatformScreen>> {
        let s = self.primary_screen();
        if s.geometry().contains(pos) {
            Some(s)
        } else {
            None
        }
    }
    fn screen_changed(&self) -> &ferrokit_core::signal::Signal<()> {
        self.inner.screen_changed()
    }
    fn theme(&self) -> Arc<dyn ferrokit_platform::theme::PlatformTheme> {
        self.inner.theme()
    }
    fn clipboard(&self) -> Box<dyn ferrokit_platform::clipboard::PlatformClipboard> {
        self.inner.clipboard()
    }
    fn create_hotkey_manager(
        &self,
    ) -> Result<Box<dyn ferrokit_platform::hotkey::PlatformHotkeyManager>, &'static str> {
        self.inner.create_hotkey_manager()
    }
    fn cursor(&self) -> Box<dyn ferrokit_platform::cursor::PlatformCursor> {
        self.inner.cursor()
    }
}

/// The platform integration and the application dpr are process-global: tests take turns.
static PLATFORM_LOCK: Mutex<()> = Mutex::new(());

/// Opens a menu at (1900, 100). Row 0 is a submenu ("More"), so one DOWN highlights it; a RIGHT
/// opens it. Returns every presented frame and the logical rect of the parent popup window.
fn run(dpr: f32, sub_label: &str, open_sub: bool) -> (Vec<Frame>, Rect) {
    let log = Arc::new(Mutex::new(Log::default()));
    set_platform_integration(Arc::new(FakePlatform {
        inner: GenericPlatformIntegration::default(),
        dpr,
        log: Arc::clone(&log),
    }));

    let mut menu = Menu::new("ParentMenu");
    let sub = menu.add_menu("More");
    sub.borrow_mut().add_action(Action::new_ref(sub_label));
    menu.add_action(Action::new_ref("Second row"));
    menu.add_action(Action::new_ref("Third row"));

    // The popup's own loop reads these in FIFO order and stops on the WM_CANCELMODE.
    // SAFETY: `PostThreadMessageW` takes integers only (this thread's id from
    // `GetCurrentThreadId`, message ids and parameters), no pointer and no handle; the id is
    // that of the running thread, which has a message queue as soon as it runs a loop, and a
    // failed post would only make the assertion below fail.
    unsafe {
        PostThreadMessageW(GetCurrentThreadId(), WM_KEYDOWN, usize::from(VK_DOWN), 0);
        if open_sub {
            PostThreadMessageW(GetCurrentThreadId(), WM_KEYDOWN, usize::from(VK_RIGHT), 0);
        }
        PostThreadMessageW(GetCurrentThreadId(), WM_CANCELMODE, 0, 0);
    }
    assert!(
        Menu::exec_popup(
            &std::rc::Rc::new(std::cell::RefCell::new(menu)),
            Point::new(1900, 100)
        )
        .is_none()
    );

    let log = log.lock();
    (log.frames.clone(), log.created[0])
}

/// A submenu label whose open offset `d = submenu width - SUBMENU_OVERLAP` is fractional in
/// device px at `dpr`. Without this, the revert would produce an integer shift and pass.
fn fractional_label(dpr: f32) -> (String, i32) {
    for n in 1..=24 {
        let label = "W".repeat(n);
        let mut probe = Menu::new("Probe");
        probe.add_action(Action::new_ref(label.clone()));
        let d = probe.size_hint().width - SUBMENU_OVERLAP;
        let frac = (d as f32 * dpr).fract();
        if frac > 0.1 && frac < 0.9 {
            return (label, d);
        }
    }
    panic!("no submenu label gives a fractional open offset at dpr {dpr}");
}

fn px(f: &Frame, x: i32, y: i32) -> &[u8] {
    let i = ((y * f.w + x) * 4) as usize;
    &f.px[i..i + 4]
}

fn assert_parent_pixels_unchanged(dpr: f32) {
    let _lock = PLATFORM_LOCK.lock();
    let _dpr = DprGuard::set(dpr);
    let (label, d) = fractional_label(dpr);

    let (closed, _) = run(dpr, &label, false);
    let (open, _) = run(dpr, &label, true);
    let a = closed.last().expect("a frame with the submenu closed");
    let b = open.last().expect("a frame with the submenu open");

    // The submenu really opened to the left: the window grew by about d*dpr device px.
    let grew = b.w - a.w;
    let expected = (d as f32 * dpr).round() as i32;
    assert!(
        (grew - expected).abs() <= 1,
        "the submenu did not open left: window grew {grew} device px, expected about {expected}"
    );

    // Native origins are integers. The parent's native offset inside the open window is the
    // difference of the two origins, so it is a whole number of device px.
    let a_origin = a.target.expect("the closed frame's window origin");
    let b_origin = b.target.expect("the open frame's window origin");
    let (sx, sy) = (a_origin.x - b_origin.x, a_origin.y - b_origin.y);
    assert!(
        sx >= 0 && sy >= 0 && sx + a.w <= b.w && sy + a.h <= b.h,
        "parent region ({sx},{sy})+{}x{} lies outside the open frame {}x{}",
        a.w,
        a.h,
        b.w,
        b.h
    );

    // Compare the parent interior. The submenu covers only the left MARGIN logical px.
    let m = (MARGIN as f32 * dpr).ceil() as i32;
    let mut differing = 0;
    let mut first = None;
    for y in m..a.h - m {
        for x in m..a.w - m {
            if px(a, x, y) != px(b, x + sx, y + sy) {
                differing += 1;
                first.get_or_insert((x, y));
            }
        }
    }
    assert!(
        differing == 0,
        "parent pixels changed when the submenu opened left: {differing} of {} device px differ \
         (first at {first:?}); dpr {dpr}, submenu offset d = {d} logical px = {:.2} device px, \
         native parent shift ({sx}, {sy})",
        (a.w - 2 * m) * (a.h - 2 * m),
        d as f32 * dpr,
    );
}

#[test]
fn submenu_open_left_keeps_parent_pixels_at_dpr_1_25() {
    assert_parent_pixels_unchanged(1.25);
}

#[test]
fn submenu_open_left_keeps_parent_pixels_at_dpr_1_75() {
    assert_parent_pixels_unchanged(1.75);
}
