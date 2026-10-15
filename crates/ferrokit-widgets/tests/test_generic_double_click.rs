// SPDX-License-Identifier: LGPL-3.0-only
//! Headless generic double-click tests (`qguiapplication.cpp:2401-2426`). The fake platform
//! hands each window's `WindowSystemEventHandler` to a thread-local slot; the tests feed events
//! directly. No real HWND, no mouse moves, no real_windows.

use ferrokit_core::event::{Event, EventKind, MouseButtons};
use ferrokit_gui::application::{GuiApplication, StyleHints};
use ferrokit_gui::geometry::primitives::{Point, Rect};
use ferrokit_gui::paint::Pixmap;
use ferrokit_platform::platform_window::PlatformWindow;
use ferrokit_platform::size_limits::WINDOW_SIZE_MAX;
use ferrokit_platform::window_system_interface::{
    KeyboardModifiers, WindowSystemEvent, WindowSystemEventHandler,
};
use ferrokit_platform::{
    set_platform_integration, GenericPlatformIntegration, MouseButton, PlatformIntegration,
    WindowFlags,
};
use ferrokit_widgets::*;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

/// Whether the fake native window reports its own double clicks (Win32) or not.
static NATIVE_DOUBLE_CLICKS: AtomicBool = AtomicBool::new(false);

thread_local! {
    static HANDLERS: RefCell<Vec<Box<dyn WindowSystemEventHandler>>> = RefCell::new(Vec::new());
}

struct FakeWindow {
    geometry: Rect,
    opacity: f32,
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
    fn set_opacity(&mut self, opacity: f32) {
        self.opacity = opacity;
    }
    fn opacity(&self) -> f32 {
        self.opacity
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
    fn set_event_handler(&mut self, handler: Box<dyn WindowSystemEventHandler>) {
        HANDLERS.with(|h| h.borrow_mut().push(handler));
    }
    fn native_handle(&self) -> isize {
        0
    }
    fn reports_native_double_clicks(&self) -> bool {
        NATIVE_DOUBLE_CLICKS.load(Ordering::SeqCst)
    }
}

struct FakePlatform {
    inner: GenericPlatformIntegration,
}

impl PlatformIntegration for FakePlatform {
    fn create_window(
        &self,
        _title: &str,
        rect: Rect,
        _flags: WindowFlags,
    ) -> Result<Box<dyn PlatformWindow>, &'static str> {
        Ok(Box::new(FakeWindow {
            geometry: rect,
            opacity: 1.0,
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
    fn primary_screen(&self) -> Box<dyn ferrokit_platform::screen::PlatformScreen> {
        self.inner.primary_screen()
    }
    fn screens(&self) -> Vec<Box<dyn ferrokit_platform::screen::PlatformScreen>> {
        self.inner.screens()
    }
    fn screen_at(&self, pos: Point) -> Option<Box<dyn ferrokit_platform::screen::PlatformScreen>> {
        self.inner.screen_at(pos)
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

/// The platform integration and the style hints are process-global: tests take turns.
static PLATFORM_LOCK: Mutex<()> = Mutex::new(());

fn setup(native: bool) -> MutexGuard<'static, ()> {
    let guard = PLATFORM_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    NATIVE_DOUBLE_CLICKS.store(native, Ordering::SeqCst);
    GuiApplication::set_style_hints(StyleHints::default());
    set_platform_integration(Arc::new(FakePlatform {
        inner: GenericPlatformIntegration::default(),
    }));
    guard
}

#[derive(Clone, Default)]
struct Probe {
    presses: Rc<RefCell<Vec<(i32, i32)>>>,
    doubles: Rc<RefCell<Vec<(i32, i32)>>>,
}

impl Probe {
    fn presses(&self) -> Vec<(i32, i32)> {
        self.presses.borrow().clone()
    }
    fn doubles(&self) -> Vec<(i32, i32)> {
        self.doubles.borrow().clone()
    }
}

/// Returns the window and the index of its handler in `HANDLERS`.
fn open(probe: &Probe) -> (Window, usize) {
    let mut w =
        Window::new("dc", Rect::new(0, 0, 400, 300), WindowFlags::FRAMELESS).expect("window");
    let presses = Rc::clone(&probe.presses);
    w.set_mouse_press_handler(move |pos, _button| {
        presses.borrow_mut().push((pos.x, pos.y));
        false // unhandled, so the press reaches the window's fallback
    });
    let doubles = Rc::clone(&probe.doubles);
    w.set_window_event_handler(move |ev: &mut Event| {
        if let EventKind::MouseButtonDblClick { x, y, .. } = &ev.kind {
            doubles.borrow_mut().push((*x, *y));
            ev.accept(); // accepted, so no press fallback
        }
    });
    let index = HANDLERS.with(|h| h.borrow().len()) - 1;
    (w, index)
}

fn send(index: usize, ev: WindowSystemEvent) {
    HANDLERS.with(|h| h.borrow_mut()[index].handle_window_event(ev));
}

fn global(x: i32, y: i32) -> Point {
    Point::new(x + 500, y + 400)
}

fn press(i: usize, x: i32, y: i32, button: MouseButton, t: u32) {
    send(
        i,
        WindowSystemEvent::MousePress {
            pos: Point::new(x, y),
            global_pos: global(x, y),
            button,
            modifiers: KeyboardModifiers::default(),
            timestamp: t,
        },
    );
}

fn release(i: usize, x: i32, y: i32, button: MouseButton) {
    send(
        i,
        WindowSystemEvent::MouseRelease {
            pos: Point::new(x, y),
            global_pos: global(x, y),
            button,
            modifiers: KeyboardModifiers::default(),
        },
    );
}

fn motion(i: usize, x: i32, y: i32) {
    send(
        i,
        WindowSystemEvent::MouseMove {
            pos: Point::new(x, y),
            global_pos: global(x, y),
            buttons: MouseButtons(0),
        },
    );
}

fn native_double(i: usize, x: i32, y: i32, button: MouseButton) {
    send(
        i,
        WindowSystemEvent::MouseDoubleClick {
            pos: Point::new(x, y),
            global_pos: global(x, y),
            button,
            modifiers: KeyboardModifiers::default(),
        },
    );
}

#[test]
fn second_press_within_interval_is_a_double_click_not_a_press() {
    let _g = setup(false);
    let p = Probe::default();
    let (_w, h) = open(&p);
    press(h, 60, 40, MouseButton::Left, 1000);
    release(h, 60, 40, MouseButton::Left);
    press(h, 60, 40, MouseButton::Left, 1200);
    release(h, 60, 40, MouseButton::Left);
    assert_eq!(p.presses(), vec![(60, 40)]);
    assert_eq!(p.doubles(), vec![(60, 40)]);
}

#[test]
fn move_of_exactly_the_distance_keeps_the_pending_double_click() {
    let _g = setup(false);
    let p = Probe::default();
    let (_w, h) = open(&p);
    press(h, 60, 40, MouseButton::Left, 1000);
    release(h, 60, 40, MouseButton::Left);
    motion(h, 65, 40); // dx = 5 is not > 5
    press(h, 65, 40, MouseButton::Left, 1200);
    assert_eq!(p.presses(), vec![(60, 40)]);
    assert_eq!(p.doubles(), vec![(65, 40)]);
}

#[test]
fn move_past_the_distance_cancels_the_pending_double_click() {
    let _g = setup(false);
    let p = Probe::default();
    let (_w, h) = open(&p);
    press(h, 60, 40, MouseButton::Left, 1000);
    release(h, 60, 40, MouseButton::Left);
    motion(h, 66, 40); // dx = 6 > 5
    press(h, 66, 40, MouseButton::Left, 1200);
    assert_eq!(p.presses(), vec![(60, 40), (66, 40)]);
    assert_eq!(p.doubles(), Vec::<(i32, i32)>::new());
}

#[test]
fn press_exactly_the_interval_after_is_a_single_click() {
    let _g = setup(false);
    let p = Probe::default();
    let (_w, h) = open(&p);
    press(h, 60, 40, MouseButton::Left, 1000);
    release(h, 60, 40, MouseButton::Left);
    press(h, 60, 40, MouseButton::Left, 1400); // delta == 400, not < 400
    assert_eq!(p.presses(), vec![(60, 40), (60, 40)]);
    assert_eq!(p.doubles(), Vec::<(i32, i32)>::new());
}

#[test]
fn double_press_cycle_is_double_single_double() {
    let _g = setup(false);
    let p = Probe::default();
    let (_w, h) = open(&p);
    press(h, 60, 40, MouseButton::Left, 1000);
    release(h, 60, 40, MouseButton::Left);
    press(h, 60, 40, MouseButton::Left, 1100); // double
    release(h, 60, 40, MouseButton::Left);
    // Qt updates the press record on every press (qguiapplication.cpp:2424-2426), so 1200 would
    // pair with the double at 1100. A 500 ms gap is what breaks the chain.
    press(h, 60, 40, MouseButton::Left, 1600); // single: delta 500 from 1100
    release(h, 60, 40, MouseButton::Left);
    press(h, 60, 40, MouseButton::Left, 1700); // double
    release(h, 60, 40, MouseButton::Left);
    assert_eq!(p.presses(), vec![(60, 40), (60, 40)]);
    assert_eq!(p.doubles(), vec![(60, 40), (60, 40)]);
}

#[test]
fn native_double_click_resets_so_the_next_press_is_single() {
    let _g = setup(false);
    let p = Probe::default();
    let (_w, h) = open(&p);
    press(h, 60, 40, MouseButton::Left, 1000);
    release(h, 60, 40, MouseButton::Left);
    native_double(h, 60, 40, MouseButton::Left);
    release(h, 60, 40, MouseButton::Left);
    press(h, 60, 40, MouseButton::Left, 1150);
    assert_eq!(p.presses(), vec![(60, 40), (60, 40)]);
    assert_eq!(p.doubles(), vec![(60, 40)]);
}

#[test]
fn platform_with_native_double_clicks_never_synthesizes_one() {
    let _g = setup(true);
    let p = Probe::default();
    let (_w, h) = open(&p);
    press(h, 60, 40, MouseButton::Left, 1000);
    release(h, 60, 40, MouseButton::Left);
    press(h, 60, 40, MouseButton::Left, 1200);
    release(h, 60, 40, MouseButton::Left);
    assert_eq!(p.presses(), vec![(60, 40), (60, 40)]);
    assert_eq!(p.doubles(), Vec::<(i32, i32)>::new());
}

#[test]
fn different_button_or_window_is_not_a_double_click() {
    let _g = setup(false);
    let (pa, pb) = (Probe::default(), Probe::default());
    let (_wa, a) = open(&pa);
    let (_wb, b) = open(&pb);
    press(a, 60, 40, MouseButton::Left, 1000);
    release(a, 60, 40, MouseButton::Left);
    press(a, 60, 40, MouseButton::Right, 1100);
    release(a, 60, 40, MouseButton::Right);
    press(a, 60, 40, MouseButton::Left, 1150);
    press(b, 60, 40, MouseButton::Left, 1160);
    press(a, 60, 40, MouseButton::Left, 1170); // A, B, A: last press was in B
    assert_eq!(pa.presses(), vec![(60, 40), (60, 40), (60, 40), (60, 40)]);
    assert_eq!(pa.doubles(), Vec::<(i32, i32)>::new());
    assert_eq!(pb.presses(), vec![(60, 40)]);
}

#[test]
fn interval_comes_from_the_style_hints() {
    let _g = setup(false);
    GuiApplication::set_style_hints(StyleHints {
        mouse_double_click_interval_ms: 500,
        ..StyleHints::default()
    });
    let p = Probe::default();
    let (_w, h) = open(&p);
    press(h, 60, 40, MouseButton::Left, 1000);
    release(h, 60, 40, MouseButton::Left);
    press(h, 60, 40, MouseButton::Left, 1300); // delta 300 < 500
    assert_eq!(p.presses(), vec![(60, 40)]);
    assert_eq!(p.doubles(), vec![(60, 40)]);
}
