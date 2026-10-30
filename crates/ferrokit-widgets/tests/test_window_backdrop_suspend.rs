// SPDX-License-Identifier: LGPL-3.0-only
//! `Window::suspend_backdrop` / `resume_backdrop` forward to the platform window, whose applied
//! backdrop swaps Acrylic for the flat tint and back. The fake window drives its applied
//! backdrop through `BackdropState`, like the native Windows window does.

use ferrokit_gui::geometry::primitives::{Point, Rect};
use ferrokit_gui::paint::Pixmap;
use ferrokit_platform::backdrop::{BackdropState, BackdropType};
use ferrokit_platform::platform_window::PlatformWindow;
use ferrokit_platform::size_limits::WINDOW_SIZE_MAX;
use ferrokit_platform::window_system_interface::WindowSystemEventHandler;
use ferrokit_platform::{
    set_platform_integration, GenericPlatformIntegration, PlatformIntegration, WindowFlags,
};
use ferrokit_widgets::window::Window;
use std::cell::RefCell;
use std::sync::{Arc, Mutex};

thread_local! {
    /// Backdrops applied to the fake windows, in order, with the tint they were applied with.
    static APPLIED: RefCell<Vec<(BackdropType, bool)>> = const { RefCell::new(Vec::new()) };
}

fn applied() -> Vec<(BackdropType, bool)> {
    APPLIED.with(|a| a.borrow().clone())
}

struct FakeWindow {
    geometry: Rect,
    opacity: f32,
    min: (i32, i32),
    max: (i32, i32),
    state: BackdropState,
}

impl FakeWindow {
    fn apply(&self) {
        APPLIED.with(|a| a.borrow_mut().push((self.state.effective(), self.state.dark_mode())));
    }
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
    fn set_event_handler(&mut self, _handler: Box<dyn WindowSystemEventHandler>) {}
    fn native_handle(&self) -> isize {
        0
    }
    fn set_backdrop(&mut self, backdrop: BackdropType, dark_mode: bool) -> bool {
        self.state.request(backdrop, dark_mode);
        self.apply();
        true
    }
    fn suspend_backdrop(&mut self) -> bool {
        let changed = self.state.suspend();
        if changed {
            self.apply();
        }
        changed
    }
    fn resume_backdrop(&mut self) -> bool {
        let changed = self.state.resume();
        if changed {
            self.apply();
        }
        changed
    }
}

/// A platform window that keeps the trait's default backdrop methods.
struct PlainWindow(FakeWindow);

impl PlatformWindow for PlainWindow {
    fn show(&self) {}
    fn hide(&self) {}
    fn is_active(&self) -> bool {
        false
    }
    fn geometry(&self) -> Rect {
        self.0.geometry
    }
    fn set_geometry(&mut self, rect: Rect) {
        self.0.geometry = rect;
    }
    fn set_stays_on_top(&mut self, _enabled: bool) {}
    fn set_click_through(&mut self, _enabled: bool) {}
    fn set_opacity(&mut self, opacity: f32) {
        self.0.opacity = opacity;
    }
    fn opacity(&self) -> f32 {
        self.0.opacity
    }
    fn set_minimum_size(&mut self, w: i32, h: i32) {
        self.0.min = (w, h);
    }
    fn minimum_size(&self) -> (i32, i32) {
        self.0.min
    }
    fn set_maximum_size(&mut self, w: i32, h: i32) {
        self.0.max = (w, h);
    }
    fn maximum_size(&self) -> (i32, i32) {
        self.0.max
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
    plain: bool,
}

impl PlatformIntegration for FakePlatform {
    fn create_window(
        &self,
        _title: &str,
        rect: Rect,
        _flags: WindowFlags,
    ) -> Result<Box<dyn PlatformWindow>, &'static str> {
        let window = FakeWindow {
            geometry: rect,
            opacity: 1.0,
            min: (0, 0),
            max: (WINDOW_SIZE_MAX, WINDOW_SIZE_MAX),
            state: BackdropState::new(),
        };
        if self.plain {
            Ok(Box::new(PlainWindow(window)))
        } else {
            Ok(Box::new(window))
        }
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

fn install(plain: bool) -> std::sync::MutexGuard<'static, ()> {
    let guard = PLATFORM_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    set_platform_integration(Arc::new(FakePlatform {
        inner: GenericPlatformIntegration::default(),
        plain,
    }));
    APPLIED.with(|a| a.borrow_mut().clear());
    guard
}

fn new_window() -> Window {
    Window::new("backdrop", Rect::new(0, 0, 200, 100), WindowFlags::FRAMELESS).unwrap()
}

#[test]
fn suspend_and_resume_forward_to_the_platform_window() {
    let _g = install(false);
    let mut win = new_window();

    assert!(win.set_backdrop(BackdropType::Acrylic, true));
    assert!(win.suspend_backdrop());
    assert!(!win.suspend_backdrop(), "already suspended");
    assert!(win.resume_backdrop());
    assert!(!win.resume_backdrop(), "already resumed");

    assert_eq!(
        applied(),
        [
            (BackdropType::Acrylic, true),
            (BackdropType::FlatTint, true),
            (BackdropType::Acrylic, true),
        ]
    );
}

#[test]
fn suspend_leaves_a_non_acrylic_backdrop_alone() {
    let _g = install(false);
    let mut win = new_window();

    win.set_backdrop(BackdropType::Mica, false);
    assert!(!win.suspend_backdrop());
    assert!(!win.resume_backdrop());

    assert_eq!(applied(), [(BackdropType::Mica, false)]);
}

#[test]
fn a_new_request_while_suspended_drops_the_suspension_unless_acrylic() {
    let _g = install(false);
    let mut win = new_window();

    win.set_backdrop(BackdropType::Acrylic, true);
    assert!(win.suspend_backdrop());
    win.set_backdrop(BackdropType::Acrylic, false);
    win.set_backdrop(BackdropType::Mica, false);
    assert!(!win.resume_backdrop(), "the Mica request ended the suspension");

    assert_eq!(
        applied(),
        [
            (BackdropType::Acrylic, true),
            (BackdropType::FlatTint, true),
            (BackdropType::FlatTint, false),
            (BackdropType::Mica, false),
        ]
    );
}

#[test]
fn a_platform_window_without_backdrop_support_reports_no_change() {
    let _g = install(true);
    let mut win = new_window();

    assert!(!win.set_backdrop(BackdropType::Acrylic, true));
    assert!(!win.suspend_backdrop());
    assert!(!win.resume_backdrop());
    assert!(applied().is_empty());
}
