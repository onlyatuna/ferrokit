// SPDX-License-Identifier: LGPL-3.0-only
//! `QWindow::devicePixelRatio` is a per-window value taken from the screen the window is on
//! (`qwindow.cpp:1425-1459`), not the primary screen's. These tests run against a fake platform
//! whose primary screen is 1.0 and whose windows sit on a 2.0 screen, so a window that still
//! consults the primary screen is observable without a second monitor.

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::object::QObject;
use ferrokit_gui::geometry::primitives::{Point, Rect};
use ferrokit_gui::paint::Pixmap;
use ferrokit_platform::platform_window::PlatformWindow;
use ferrokit_platform::size_limits::WINDOW_SIZE_MAX;
use ferrokit_platform::window_system_interface::WindowSystemEventHandler;
use ferrokit_platform::{
    set_platform_integration, GenericPlatformIntegration, PlatformIntegration, WindowFlags,
};
use ferrokit_widgets::*;
use std::sync::{Arc, Mutex};

/// The ratio of the screen the fake windows are on. The fake primary screen is 1.0.
const WINDOW_SCREEN_DPR: f32 = 2.0;

#[derive(Default)]
struct Log {
    /// Every native rect passed to `PlatformWindow::set_geometry`, in order.
    native_rects: Vec<Rect>,
    /// Physical width of every pixmap presented.
    presented_widths: Vec<u32>,
}

struct FakeWindow {
    geometry: Rect,
    opacity: f32,
    min: (i32, i32),
    max: (i32, i32),
    log: Arc<Mutex<Log>>,
}

impl PlatformWindow for FakeWindow {
    fn show(&self) {}
    fn hide(&self) {}
    /// This double only exercises the device pixel ratio; it does not model activation.
    fn is_active(&self) -> bool {
        false
    }
    fn geometry(&self) -> Rect {
        self.geometry
    }
    fn set_geometry(&mut self, rect: Rect) {
        self.geometry = rect;
        self.log.lock().unwrap().native_rects.push(rect);
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
        WINDOW_SCREEN_DPR
    }
    fn present(&mut self, pixmap: &mut Pixmap, _opacity: f32) -> Result<(), &'static str> {
        self.log
            .lock()
            .unwrap()
            .presented_widths
            .push(pixmap.physical_width());
        Ok(())
    }
    fn set_event_handler(&mut self, _handler: Box<dyn WindowSystemEventHandler>) {}
    fn native_handle(&self) -> isize {
        0
    }
}

/// Everything but `create_window` is the generic platform, whose primary screen is 1.0.
struct FakePlatform {
    inner: GenericPlatformIntegration,
    log: Arc<Mutex<Log>>,
}

impl PlatformIntegration for FakePlatform {
    fn create_window(
        &self,
        _title: &str,
        rect: Rect,
        _flags: WindowFlags,
    ) -> Result<Box<dyn PlatformWindow>, &'static str> {
        self.log.lock().unwrap().native_rects.push(rect);
        Ok(Box::new(FakeWindow {
            geometry: rect,
            opacity: 1.0,
            min: (0, 0),
            max: (WINDOW_SIZE_MAX, WINDOW_SIZE_MAX),
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

fn window_on_second_screen(
    rect: Rect,
) -> (Window, Arc<Mutex<Log>>, std::sync::MutexGuard<'static, ()>) {
    let guard = PLATFORM_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let log = Arc::new(Mutex::new(Log::default()));
    set_platform_integration(Arc::new(FakePlatform {
        inner: GenericPlatformIntegration::default(),
        log: Arc::clone(&log),
    }));
    assert_eq!(
        ferrokit_platform::platform()
            .primary_screen()
            .device_pixel_ratio(),
        1.0,
        "the fake primary screen must differ from the window's screen"
    );
    let window = Window::new("dpr", rect, WindowFlags::FRAMELESS).expect("window");
    (window, log, guard)
}

fn dpi_changed(window: &mut Window, dpi: u32) {
    let mut ev = Event::new_spontaneous(EventKind::DpiChanged {
        dpi_x: dpi,
        dpi_y: dpi,
    });
    assert!(window.event(&mut ev));
}

#[test]
fn a_window_takes_its_ratio_from_its_own_screen_not_the_primary_screen() {
    let (window, log, _g) = window_on_second_screen(Rect::new(10, 20, 400, 300));

    assert_eq!(window.device_pixel_ratio(), WINDOW_SCREEN_DPR);
    // The native window has the physical size of its own screen's ratio: 400x300 logical at 2.0.
    let native = *log.lock().unwrap().native_rects.last().unwrap();
    assert_eq!((native.width, native.height), (800, 600));

    let mut window = window;
    window.render_and_present();
    let bs = window.backing_store();
    assert_eq!(bs.device_pixel_ratio(), WINDOW_SCREEN_DPR);
    assert_eq!((bs.physical_width(), bs.physical_height()), (800, 600));
}

#[test]
fn a_dpi_change_is_not_undone_by_the_next_render() {
    let (mut window, _log, _g) = window_on_second_screen(Rect::new(0, 0, 400, 300));

    dpi_changed(&mut window, 168); // 1.75
    assert_eq!(window.device_pixel_ratio(), 1.75);
    assert_eq!(window.backing_store().device_pixel_ratio(), 1.75);

    // Each of these used to resize the store back to the primary screen's ratio.
    window.root_widget().borrow_mut().update();
    window.render_and_present();
    assert_eq!(window.backing_store().device_pixel_ratio(), 1.75);

    window.set_geometry(Rect::new(0, 0, 200, 100));
    assert_eq!(window.backing_store().device_pixel_ratio(), 1.75);
    assert_eq!(window.backing_store().physical_width(), 350);
}

#[test]
fn custom_presentation_uses_the_window_ratio() {
    let (mut window, log, _g) = window_on_second_screen(Rect::new(0, 0, 400, 300));

    window.present_custom(|_| {});
    assert_eq!(*log.lock().unwrap().presented_widths.last().unwrap(), 800);

    window.present_custom_at(Rect::new(5, 5, 100, 50), |_| {});
    assert_eq!(*log.lock().unwrap().presented_widths.last().unwrap(), 200);

    dpi_changed(&mut window, 168);
    window.present_custom(|_| {});
    assert_eq!(
        *log.lock().unwrap().presented_widths.last().unwrap(),
        100 * 175 / 100
    );
}
