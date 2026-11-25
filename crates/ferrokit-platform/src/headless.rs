// SPDX-License-Identifier: LGPL-3.0-only
//! In-memory headless platform backend for deterministic UI and layout testing without a native
//! window system.

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use ferrokit_core::poison::{lock_unpoisoned, read_unpoisoned, write_unpoisoned};
use ferrokit_core::signal::Signal;
use ferrokit_gui::geometry::primitives::{Point, Rect};
use ferrokit_gui::image::Icon;
use ferrokit_gui::paint::Pixmap;

use crate::clipboard::{GenericClipboard, MimeData, PlatformClipboard};
use crate::cursor::{GenericCursor, PlatformCursor};
use crate::hotkey::{GenericHotkeyManager, PlatformHotkeyManager};
use crate::integration::PlatformIntegration;
use crate::platform_tray::{GenericTrayIcon, PlatformTrayIcon};
use crate::platform_window::PlatformWindow;
use crate::screen::{GenericScreen, PlatformScreen};
use crate::size_limits::{SizeLimits, clamp_size_limit};
use crate::theme::{GenericTheme, PlatformTheme};
use crate::window::WindowFlags;
use crate::window_system_interface::{WindowSystemEvent, WindowSystemEventHandler};

struct HeadlessClipboard(Arc<GenericClipboard>);

impl PlatformClipboard for HeadlessClipboard {
    fn set_mime_data(&self, data: &MimeData) -> Result<(), &'static str> {
        self.0.set_mime_data(data)
    }

    fn mime_data(&self) -> Result<MimeData, &'static str> {
        self.0.mime_data()
    }

    fn clear(&self) -> Result<(), &'static str> {
        self.0.clear()
    }
}

struct HeadlessWindowState {
    handle: isize,
    title: Mutex<String>,
    geometry: Mutex<Rect>,
    visible: AtomicBool,
    active: AtomicBool,
    opacity: Mutex<f32>,
    last_pixmap: Mutex<Option<Pixmap>>,
    pending_events: Mutex<Vec<WindowSystemEvent>>,
}

/// Read-only inspection and deterministic event-injection handle for a window created through
/// [`HeadlessIntegration`].
#[derive(Clone)]
pub struct HeadlessWindowHandle {
    state: Arc<HeadlessWindowState>,
}

impl HeadlessWindowHandle {
    pub fn handle(&self) -> isize {
        self.state.handle
    }

    pub fn title(&self) -> String {
        lock_unpoisoned(&self.state.title).clone()
    }

    pub fn geometry(&self) -> Rect {
        *lock_unpoisoned(&self.state.geometry)
    }

    pub fn is_visible(&self) -> bool {
        self.state.visible.load(Ordering::Acquire)
    }

    pub fn is_active(&self) -> bool {
        self.state.active.load(Ordering::Acquire)
    }

    pub fn opacity(&self) -> f32 {
        *lock_unpoisoned(&self.state.opacity)
    }

    pub fn last_pixmap(&self) -> Option<Pixmap> {
        lock_unpoisoned(&self.state.last_pixmap).clone()
    }

    pub fn push_event(&self, event: WindowSystemEvent) {
        lock_unpoisoned(&self.state.pending_events).push(event);
    }
}

/// In-memory [`PlatformWindow`] created by [`HeadlessIntegration`].
pub struct HeadlessWindow {
    state: Arc<HeadlessWindowState>,
    screens: Arc<RwLock<Vec<GenericScreen>>>,
    flags: WindowFlags,
    size_limits: SizeLimits,
    owner_thread: std::thread::ThreadId,
    state_flags: std::cell::Cell<crate::window::PlatformWindowStateFlags>,
    mask: ferrokit_gui::geometry::Region,
    transient_parent: Option<isize>,
    handler: Option<Box<dyn WindowSystemEventHandler>>,
}

impl HeadlessWindow {
    pub fn inspect(&self) -> HeadlessWindowHandle {
        HeadlessWindowHandle {
            state: Arc::clone(&self.state),
        }
    }
}

impl PlatformWindow for HeadlessWindow {
    fn state_flags(&self) -> crate::window::PlatformWindowStateFlags {
        self.state_flags.get()
    }

    fn owner_thread(&self) -> std::thread::ThreadId {
        self.owner_thread
    }

    fn show(&self) {
        self.state.visible.store(true, Ordering::Release);
    }

    fn hide(&self) {
        self.state.visible.store(false, Ordering::Release);
        self.state.active.store(false, Ordering::Release);
    }

    fn is_active(&self) -> bool {
        self.state.active.load(Ordering::Acquire)
    }

    fn geometry(&self) -> Rect {
        *lock_unpoisoned(&self.state.geometry)
    }

    fn set_geometry(&mut self, rect: Rect) {
        *lock_unpoisoned(&self.state.geometry) = rect;
    }

    fn set_stays_on_top(&mut self, enabled: bool) {
        self.flags.set(WindowFlags::STAYS_ON_TOP, enabled);
    }

    fn set_click_through(&mut self, enabled: bool) {
        self.flags.set(WindowFlags::CLICK_THROUGH, enabled);
    }

    fn flags(&self) -> WindowFlags {
        self.flags
    }

    fn start_system_drag(&self) {
        let _ = self.start_system_move();
    }

    fn start_system_move(&self) -> bool {
        true
    }

    fn start_system_resize(&self, _edges: crate::platform_window::WindowEdges) -> bool {
        true
    }

    fn set_opacity(&mut self, opacity: f32) {
        *lock_unpoisoned(&self.state.opacity) = opacity.clamp(0.0, 1.0);
    }

    fn opacity(&self) -> f32 {
        *lock_unpoisoned(&self.state.opacity)
    }

    fn set_mask(&mut self, region: &ferrokit_gui::geometry::Region) {
        self.mask = region.clone();
    }

    fn set_transient_parent(&mut self, parent: Option<isize>) {
        self.transient_parent = parent;
    }

    fn transient_parent(&self) -> Option<isize> {
        self.transient_parent
    }

    fn set_minimum_size(&mut self, min_w: i32, min_h: i32) {
        self.size_limits.min = clamp_size_limit(min_w, min_h);
    }

    fn minimum_size(&self) -> (i32, i32) {
        self.size_limits.min
    }

    fn set_maximum_size(&mut self, max_w: i32, max_h: i32) {
        self.size_limits.max = clamp_size_limit(max_w, max_h);
    }

    fn maximum_size(&self) -> (i32, i32) {
        self.size_limits.max
    }

    fn device_pixel_ratio(&self) -> f32 {
        let geom = self.geometry();
        let center = Point::new(geom.x + geom.width / 2, geom.y + geom.height / 2);
        let screens = read_unpoisoned(&self.screens);
        screens
            .iter()
            .find(|s| s.geometry().contains(center))
            .or_else(|| screens.iter().find(|s| s.is_primary()))
            .or_else(|| screens.first())
            .map(PlatformScreen::device_pixel_ratio)
            .unwrap_or(1.0)
    }

    fn present(&mut self, pixmap: &mut Pixmap, opacity: f32) -> Result<(), &'static str> {
        *lock_unpoisoned(&self.state.last_pixmap) = Some(pixmap.clone());
        *lock_unpoisoned(&self.state.opacity) = opacity.clamp(0.0, 1.0);
        Ok(())
    }

    fn set_event_handler(&mut self, handler: Box<dyn WindowSystemEventHandler>) {
        self.handler = Some(handler);
    }

    fn native_handle(&self) -> isize {
        self.state.handle
    }

    fn poll_events(&mut self) -> usize {
        let events = std::mem::take(&mut *lock_unpoisoned(&self.state.pending_events));
        let count = events.len();
        for event in &events {
            match event {
                WindowSystemEvent::FocusIn => self.state.active.store(true, Ordering::Release),
                WindowSystemEvent::FocusOut => self.state.active.store(false, Ordering::Release),
                _ => {}
            }
        }
        if let Some(handler) = self.handler.as_mut() {
            for event in events {
                handler.handle_window_event(event);
            }
        }
        count
    }
}

/// Deterministic, in-memory [`PlatformIntegration`] with configurable screens, shared clipboard,
/// and window inspection/event injection handles.
pub struct HeadlessIntegration {
    screens: Arc<RwLock<Vec<GenericScreen>>>,
    screen_changed_signal: Signal<()>,
    theme: Arc<GenericTheme>,
    clipboard: Arc<GenericClipboard>,
    next_handle: AtomicIsize,
    windows: Mutex<Vec<Arc<HeadlessWindowState>>>,
}

impl Default for HeadlessIntegration {
    fn default() -> Self {
        Self::new()
    }
}

impl HeadlessIntegration {
    pub fn new() -> Self {
        Self::with_screens(vec![GenericScreen::default_primary()])
    }

    pub fn with_screens(screens: Vec<GenericScreen>) -> Self {
        let screens = if screens.is_empty() {
            vec![GenericScreen::default_primary()]
        } else {
            screens
        };
        Self {
            screens: Arc::new(RwLock::new(screens)),
            screen_changed_signal: Signal::new(),
            theme: Arc::new(GenericTheme::default()),
            clipboard: Arc::new(GenericClipboard::new()),
            next_handle: AtomicIsize::new(1),
            windows: Mutex::new(Vec::new()),
        }
    }

    pub fn set_screens(&self, screens: Vec<GenericScreen>) {
        let screens = if screens.is_empty() {
            vec![GenericScreen::default_primary()]
        } else {
            screens
        };
        *write_unpoisoned(&self.screens) = screens;
        self.screen_changed_signal.emit(&());
    }

    pub fn windows(&self) -> Vec<HeadlessWindowHandle> {
        lock_unpoisoned(&self.windows)
            .iter()
            .map(|state| HeadlessWindowHandle {
                state: Arc::clone(state),
            })
            .collect()
    }

    pub fn window(&self, handle: isize) -> Option<HeadlessWindowHandle> {
        lock_unpoisoned(&self.windows)
            .iter()
            .find(|state| state.handle == handle)
            .map(|state| HeadlessWindowHandle {
                state: Arc::clone(state),
            })
    }

    pub fn window_count(&self) -> usize {
        lock_unpoisoned(&self.windows).len()
    }
}

impl PlatformIntegration for HeadlessIntegration {
    fn create_window(
        &self,
        title: &str,
        rect: Rect,
        flags: WindowFlags,
    ) -> Result<Box<dyn PlatformWindow>, &'static str> {
        let handle = self.next_handle.fetch_add(1, Ordering::Relaxed);
        let state = Arc::new(HeadlessWindowState {
            handle,
            title: Mutex::new(title.to_string()),
            geometry: Mutex::new(rect),
            visible: AtomicBool::new(false),
            active: AtomicBool::new(false),
            opacity: Mutex::new(1.0),
            last_pixmap: Mutex::new(None),
            pending_events: Mutex::new(Vec::new()),
        });
        lock_unpoisoned(&self.windows).push(Arc::clone(&state));
        Ok(Box::new(HeadlessWindow {
            state,
            screens: Arc::clone(&self.screens),
            flags,
            size_limits: SizeLimits::default(),
            owner_thread: std::thread::current().id(),
            state_flags: std::cell::Cell::new(crate::window::PlatformWindowStateFlags::NONE),
            mask: ferrokit_gui::geometry::Region::new(),
            transient_parent: None,
            handler: None,
        }))
    }

    fn create_tray_icon(
        &self,
        tooltip: &str,
        icon: &Icon,
    ) -> Result<Box<dyn PlatformTrayIcon>, &'static str> {
        Ok(Box::new(GenericTrayIcon::new(tooltip, icon)))
    }

    fn primary_screen(&self) -> Box<dyn PlatformScreen> {
        let screens = read_unpoisoned(&self.screens);
        let screen = screens
            .iter()
            .find(|s| s.is_primary())
            .or_else(|| screens.first())
            .cloned()
            .unwrap_or_else(GenericScreen::default_primary);
        Box::new(screen)
    }

    fn screens(&self) -> Vec<Box<dyn PlatformScreen>> {
        read_unpoisoned(&self.screens)
            .iter()
            .cloned()
            .map(|s| Box::new(s) as Box<dyn PlatformScreen>)
            .collect()
    }

    fn screen_at(&self, pos: Point) -> Option<Box<dyn PlatformScreen>> {
        read_unpoisoned(&self.screens)
            .iter()
            .find(|s| s.geometry().contains(pos))
            .cloned()
            .map(|s| Box::new(s) as Box<dyn PlatformScreen>)
    }

    fn screen_changed(&self) -> &Signal<()> {
        &self.screen_changed_signal
    }

    fn theme(&self) -> Arc<dyn PlatformTheme> {
        Arc::clone(&self.theme) as Arc<dyn PlatformTheme>
    }

    fn clipboard(&self) -> Box<dyn PlatformClipboard> {
        Box::new(HeadlessClipboard(Arc::clone(&self.clipboard)))
    }

    fn create_hotkey_manager(&self) -> Result<Box<dyn PlatformHotkeyManager>, &'static str> {
        Ok(Box::new(GenericHotkeyManager::new()))
    }

    fn cursor(&self) -> Box<dyn PlatformCursor> {
        Box::new(GenericCursor::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrokit_gui::PaintDevice;
    use raw_window_handle::HandleError;

    struct RecordingHandler(Arc<Mutex<Vec<WindowSystemEvent>>>);

    impl WindowSystemEventHandler for RecordingHandler {
        fn handle_window_event(&mut self, event: WindowSystemEvent) {
            lock_unpoisoned(&self.0).push(event);
        }
    }

    #[test]
    fn headless_window_lifecycle_present_and_event_injection() {
        let backend = HeadlessIntegration::new();
        let mut w1 = backend
            .create_window("First", Rect::new(10, 20, 320, 240), WindowFlags::NORMAL)
            .unwrap();
        let w2 = backend
            .create_window("Second", Rect::new(0, 0, 100, 100), WindowFlags::NORMAL)
            .unwrap();

        assert_ne!(w1.native_handle(), 0);
        assert_ne!(w1.native_handle(), w2.native_handle());
        assert_eq!(backend.window_count(), 2);
        assert!(matches!(
            w1.raw_window_handle(),
            Err(HandleError::Unavailable)
        ));
        assert!(matches!(
            w1.raw_display_handle(),
            Err(HandleError::Unavailable)
        ));

        let handle = backend.window(w1.native_handle()).expect("window handle");
        assert_eq!(handle.title(), "First");
        assert!(!handle.is_visible());
        w1.show();
        assert!(handle.is_visible());

        let mut pixmap = Pixmap::new(16, 16).unwrap();
        w1.present(&mut pixmap, 0.75).unwrap();
        assert_eq!(handle.opacity(), 0.75);
        assert_eq!(
            handle.last_pixmap().map(|p| (p.width(), p.height())),
            Some((16.0, 16.0))
        );

        let seen = Arc::new(Mutex::new(Vec::new()));
        w1.set_event_handler(Box::new(RecordingHandler(Arc::clone(&seen))));
        handle.push_event(WindowSystemEvent::FocusIn);
        handle.push_event(WindowSystemEvent::FocusOut);
        assert_eq!(w1.poll_events(), 2);
        assert!(!handle.is_active());
        assert_eq!(lock_unpoisoned(&seen).len(), 2);
    }

    #[test]
    fn headless_clipboard_persists_and_screens_emit_signal() {
        let backend = HeadlessIntegration::with_screens(vec![GenericScreen::new(
            "Main",
            Rect::new(0, 0, 1280, 720),
            Rect::new(0, 0, 1280, 720),
            true,
            1.5,
        )]);
        assert_eq!(backend.primary_screen().device_pixel_ratio(), 1.5);

        let cb1 = backend.clipboard();
        let cb2 = backend.clipboard();
        let mut mime = MimeData::new();
        mime.set_text("hello headless");
        cb1.set_mime_data(&mime).unwrap();
        assert_eq!(
            cb2.mime_data().unwrap().text().as_deref(),
            Some("hello headless")
        );

        let changes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let changes_cb = Arc::clone(&changes);
        backend.screen_changed().connect(move |_| {
            changes_cb.fetch_add(1, Ordering::Relaxed);
        });
        backend.set_screens(vec![GenericScreen::new(
            "HiDPI",
            Rect::new(0, 0, 3840, 2160),
            Rect::new(0, 0, 3840, 2160),
            true,
            2.0,
        )]);
        assert_eq!(changes.load(Ordering::Relaxed), 1);
        assert_eq!(backend.primary_screen().device_pixel_ratio(), 2.0);
        assert!(backend.screen_at(Point::new(2000, 1000)).is_some());
    }
}
