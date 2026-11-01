// SPDX-License-Identifier: LGPL-3.0-only
//! Headless platform for tests: every window is a fake that hands its `WindowSystemEventHandler`
//! to a thread-local slot, so a test feeds native events straight into the widget layer without a
//! real HWND. Include with `#[path = "support/headless.rs"] mod headless;`.
#![allow(dead_code)]

use ferrokit_gui::geometry::primitives::{Point, Rect};
use ferrokit_gui::paint::Pixmap;
use ferrokit_platform::platform_window::PlatformWindow;
use ferrokit_platform::size_limits::WINDOW_SIZE_MAX;
use ferrokit_platform::window_system_interface::{WindowSystemEvent, WindowSystemEventHandler};
use ferrokit_platform::{
    GenericPlatformIntegration, PlatformIntegration, WindowFlags, set_platform_integration,
};
use std::cell::RefCell;
use std::sync::{Arc, Mutex, MutexGuard};

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

/// The platform integration is process-global: tests take turns.
static PLATFORM_LOCK: Mutex<()> = Mutex::new(());

/// Installs the headless platform; hold the guard for the whole test.
pub fn install() -> MutexGuard<'static, ()> {
    let guard = PLATFORM_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    set_platform_integration(Arc::new(FakePlatform {
        inner: GenericPlatformIntegration::default(),
    }));
    guard
}

/// Index of the handler the most recently created window registered.
pub fn last_handler() -> usize {
    HANDLERS.with(|h| h.borrow().len()) - 1
}

/// Feeds one native event to the window whose handler is at `index`.
pub fn send(index: usize, ev: WindowSystemEvent) {
    HANDLERS.with(|h| h.borrow_mut()[index].handle_window_event(ev));
}
