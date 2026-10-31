// SPDX-License-Identifier: LGPL-3.0-only
//! `activate_window` called from a `Show` handler: the native window is still hidden while the
//! handlers run, so the request is deferred until it is shown. The fake platform window logs the
//! calls it receives.

use ferrokit_core::event::{Event, EventKind};
use ferrokit_gui::geometry::primitives::{Point, Rect};
use ferrokit_gui::paint::Pixmap;
use ferrokit_platform::platform_window::PlatformWindow;
use ferrokit_platform::size_limits::WINDOW_SIZE_MAX;
use ferrokit_platform::window_system_interface::WindowSystemEventHandler;
use ferrokit_platform::{
    set_platform_integration, GenericPlatformIntegration, PlatformIntegration, WindowFlags,
};
use ferrokit_widgets::command::WidgetCommandQueue;
use ferrokit_widgets::window::Window;
use std::cell::RefCell;
use std::sync::{Arc, Mutex};

thread_local! {
    static LOG: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
}

fn log(entry: &'static str) {
    LOG.with(|l| l.borrow_mut().push(entry));
}

fn logged() -> Vec<&'static str> {
    LOG.with(|l| l.borrow().clone())
}

struct FakeWindow {
    geometry: Rect,
    opacity: f32,
    min: (i32, i32),
    max: (i32, i32),
}

impl PlatformWindow for FakeWindow {
    fn show(&self) {
        log("native show");
    }
    fn hide(&self) {}
    fn is_active(&self) -> bool {
        false
    }
    fn request_activate_window(&self) {
        log("request activate");
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
    fn set_event_handler(&mut self, _handler: Box<dyn WindowSystemEventHandler>) {}
    fn native_handle(&self) -> isize {
        0
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
        icon: &ferrokit_gui::image::Icon,
    ) -> Result<Box<dyn ferrokit_platform::platform_tray::PlatformTrayIcon>, &'static str> {
        self.inner.create_tray_icon(tooltip, icon)
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

static PLATFORM_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn activate_window_from_a_show_handler_reaches_the_platform_window_after_it_is_shown() {
    let _g = PLATFORM_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    set_platform_integration(Arc::new(FakePlatform {
        inner: GenericPlatformIntegration::default(),
    }));
    LOG.with(|l| l.borrow_mut().clear());

    let mut win = Window::new("a", Rect::new(0, 0, 200, 100), WindowFlags::FRAMELESS).unwrap();
    let root = win.root_widget();
    win.set_window_event_handler(move |ev: &mut Event| {
        if matches!(ev.kind, EventKind::Show) {
            root.borrow().activate_window();
        }
    });

    win.show();
    WidgetCommandQueue::flush();

    assert_eq!(logged(), ["native show", "request activate"]);
    // Shown windows are asked at once.
    win.activate_window();
    assert_eq!(
        logged(),
        ["native show", "request activate", "request activate"]
    );
}
