// SPDX-License-Identifier: LGPL-3.0-only
//! A popup menu is kept inside the work area of the screen under the point it opens at, not the
//! primary screen's: `QMenuPrivate::popup` clamps to `popupGeometry(QGuiApplication::screenAt(p))`
//! (qmenu.cpp:2382, 292-308, 2489-2502). A fake platform with two side-by-side screens shows a
//! menu that consults the primary screen without a second monitor.
#![cfg(windows)]

use ferrokit_gui::geometry::primitives::{Point, Rect};
use ferrokit_gui::paint::Pixmap;
use ferrokit_platform::platform_window::PlatformWindow;
use ferrokit_platform::screen::PlatformScreen;
use ferrokit_platform::size_limits::WINDOW_SIZE_MAX;
use ferrokit_platform::window_system_interface::WindowSystemEventHandler;
use ferrokit_platform::{
    set_platform_integration, GenericPlatformIntegration, PlatformIntegration, WindowFlags,
};
use ferrokit_widgets::{Action, Menu, Widget};
use std::sync::{Arc, Mutex};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::WindowsAndMessaging::{PostThreadMessageW, WM_CANCELMODE};

const TASKBAR: i32 = 40;

struct FakeScreen {
    geometry: Rect,
    primary: bool,
}

impl PlatformScreen for FakeScreen {
    fn name(&self) -> String {
        if self.primary { "primary" } else { "secondary" }.to_string()
    }
    fn geometry(&self) -> Rect {
        self.geometry
    }
    fn available_geometry(&self) -> Rect {
        let g = self.geometry;
        Rect::new(g.x, g.y, g.width, g.height - TASKBAR)
    }
    fn is_primary(&self) -> bool {
        self.primary
    }
    fn device_pixel_ratio(&self) -> f32 {
        1.0
    }
}

struct FakeWindow {
    geometry: Rect,
    min: (i32, i32),
    max: (i32, i32),
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
        1.0
    }
    fn present(&mut self, _pixmap: &mut Pixmap, _opacity: f32) -> Result<(), &'static str> {
        Ok(())
    }
    fn set_event_handler(&mut self, _handler: Box<dyn WindowSystemEventHandler>) {}
    fn native_handle(&self) -> isize {
        0
    }
}

/// The primary screen at (0, 0) and a secondary one at `secondary`; every created window's rect
/// is logged.
struct FakePlatform {
    inner: GenericPlatformIntegration,
    secondary: Rect,
    created: Arc<Mutex<Vec<Rect>>>,
}

impl FakePlatform {
    fn screen(&self, primary: bool) -> Box<dyn PlatformScreen> {
        let geometry = if primary {
            Rect::new(0, 0, 1920, 1080)
        } else {
            self.secondary
        };
        Box::new(FakeScreen { geometry, primary })
    }
}

impl PlatformIntegration for FakePlatform {
    fn create_window(
        &self,
        _title: &str,
        rect: Rect,
        _flags: WindowFlags,
    ) -> Result<Box<dyn PlatformWindow>, &'static str> {
        self.created.lock().unwrap().push(rect);
        Ok(Box::new(FakeWindow {
            geometry: rect,
            min: (0, 0),
            max: (WINDOW_SIZE_MAX, WINDOW_SIZE_MAX),
        }))
    }
    fn create_tray_icon(
        &self,
        tooltip: &str,
        pixmap: &Pixmap,
    ) -> Result<Box<dyn ferrokit_platform::platform_tray::PlatformTrayIcon>, &'static str> {
        self.inner.create_tray_icon(tooltip, pixmap)
    }
    fn primary_screen(&self) -> Box<dyn PlatformScreen> {
        self.screen(true)
    }
    fn screens(&self) -> Vec<Box<dyn PlatformScreen>> {
        vec![self.screen(true), self.screen(false)]
    }
    fn screen_at(&self, pos: Point) -> Option<Box<dyn PlatformScreen>> {
        self.screens()
            .into_iter()
            .find(|s| s.geometry().contains(pos))
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

/// The platform integration is process-global: tests take turns.
static PLATFORM_LOCK: Mutex<()> = Mutex::new(());

/// Opens a two-item menu at `pos` with the secondary screen at `secondary`, cancels it at once,
/// and returns the popup window's rect and the menu's size.
fn popup_rect(secondary: Rect, pos: Point) -> (Rect, (i32, i32)) {
    let _guard = PLATFORM_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let created = Arc::new(Mutex::new(Vec::new()));
    set_platform_integration(Arc::new(FakePlatform {
        inner: GenericPlatformIntegration::default(),
        secondary,
        created: Arc::clone(&created),
    }));

    let mut menu = Menu::new("ScreenTest");
    menu.add_action(Action::new_ref("First"));
    menu.add_action(Action::new_ref("Second"));
    let size = menu.size_hint();
    // The popup's own loop ends on the first `WM_CANCELMODE` it reads.
    unsafe { PostThreadMessageW(GetCurrentThreadId(), WM_CANCELMODE, 0, 0) };
    assert!(Menu::exec_popup(&std::rc::Rc::new(std::cell::RefCell::new(menu)), pos).is_none());

    let rect = *created.lock().unwrap().first().expect("the popup window");
    (rect, (size.width, size.height))
}

#[test]
fn a_menu_on_a_screen_right_of_the_primary_opens_at_the_point() {
    let (rect, (w, h)) = popup_rect(Rect::new(1920, 0, 1920, 1080), Point::new(2500, 400));
    assert_eq!(rect, Rect::new(2500, 400, w, h));
}

#[test]
fn a_menu_on_a_screen_left_of_the_primary_opens_at_the_point() {
    let (rect, (w, h)) = popup_rect(Rect::new(-1920, 0, 1920, 1080), Point::new(-1000, 400));
    assert_eq!(rect, Rect::new(-1000, 400, w, h));
}

#[test]
fn a_menu_is_kept_inside_the_screen_it_opens_on() {
    let secondary = Rect::new(1920, 0, 1920, 1080);
    let (rect, (w, h)) = popup_rect(secondary, Point::new(3835, 400));
    // Too close to the secondary screen's right edge: pulled left to 8 px inside it
    // (qmenu.cpp:2489-2490, `desktopFrame`), not to the primary screen's edge.
    assert_eq!(rect, Rect::new(1920 + 1920 - 8 - w, 400, w, h));
}
