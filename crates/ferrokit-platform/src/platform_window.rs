// SPDX-License-Identifier: LGPL-3.0-only
use bitflags::bitflags;

bitflags! {
    /// Edges of a window used for border resizing, exactly matching Qt::Edges.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct WindowEdges: u32 {
        const LEFT = 0x01;
        const TOP = 0x02;
        const RIGHT = 0x04;
        const BOTTOM = 0x08;
        const TOP_LEFT = Self::TOP.bits() | Self::LEFT.bits();
        const TOP_RIGHT = Self::TOP.bits() | Self::RIGHT.bits();
        const BOTTOM_LEFT = Self::BOTTOM.bits() | Self::LEFT.bits();
        const BOTTOM_RIGHT = Self::BOTTOM.bits() | Self::RIGHT.bits();
    }
}

pub type WindowEdge = WindowEdges;

pub trait PlatformWindow: 'static {
    fn show(&self);
    fn hide(&self);
    /// Whether this window is the active window (`QPlatformWindow::isActive`, which on Windows is
    /// the foreground-window check in `qwindowswindow.cpp:1904-1910`). Required, with no default:
    /// the answer is platform state that a backend must report, not a value to assume. A hidden
    /// window is never active, and a `WindowFlags::TOOLTIP` window never becomes active by being
    /// shown.
    fn is_active(&self) -> bool;
    /// `QPlatformWindow::raise`: puts the window above its sibling top-level windows in the
    /// window system's stacking order (the topmost band stays above the others). It does not
    /// activate the window or give it the keyboard focus; that is
    /// [`request_activate_window`](Self::request_activate_window). A backend that does not
    /// implement it does nothing, as `QPlatformWindow::raise` does by default.
    fn raise(&self) {}
    /// `QPlatformWindow::requestActivateWindow`: asks the window system to make this window the
    /// active one. It is a request: the window becomes active ([`is_active`](Self::is_active))
    /// only if the window system grants it, which may happen after this call returns. A hidden
    /// window and a `WindowFlags::TOOLTIP` window, which never takes activation, are not asked.
    ///
    /// On Windows this is `SetForegroundWindow` and nothing more (no `AttachThreadInput` or
    /// input-injection tricks), so the system's foreground lock applies: a process that is not the
    /// foreground process, and has not received the last input event, cannot take the foreground,
    /// and the window's taskbar button flashes instead. That is the behaviour `QWidget::activateWindow`
    /// documents. A minimized window is not restored. A backend that does not implement it does
    /// nothing, as `QPlatformWindow::requestActivateWindow` does by default.
    fn request_activate_window(&self) {}
    fn geometry(&self) -> Rect;
    fn set_geometry(&mut self, rect: Rect);
    fn set_stays_on_top(&mut self, enabled: bool);
    fn set_click_through(&mut self, enabled: bool);
    /// Sets or clears a window flag (`QWidget::setWindowFlag`, qwidget.cpp:10641-10648).
    fn set_window_flag(&mut self, flag: WindowFlags, on: bool) {
        if flag.contains(WindowFlags::STAYS_ON_TOP) {
            self.set_stays_on_top(on);
        }
        if flag.contains(WindowFlags::CLICK_THROUGH) {
            self.set_click_through(on);
        }
    }
    /// Current window flags (`QWidget::windowFlags`).
    fn flags(&self) -> WindowFlags {
        WindowFlags::NORMAL
    }
    fn start_system_drag(&self) {
        let _ = self.start_system_move();
    }
    fn start_system_move(&self) -> bool {
        self.start_system_drag();
        true
    }
    fn start_system_resize(&self, _edges: WindowEdges) -> bool {
        false
    }
    fn set_cursor(&mut self, _shape: crate::cursor::CursorShape) {}
    /// `QWindow::unsetCursor`: the window no longer asks for a cursor of its own and shows the
    /// default one (the application override, or the arrow).
    fn unset_cursor(&mut self) {}
    /// `QPlatformWindow::setMouseGrabEnabled`: while grabbed the window receives every mouse event
    /// of the application, wherever the pointer is. Returns `grab` when it took effect, `false`
    /// when the window cannot grab (a hidden window, or a backend without grabs: Qt's default
    /// `QPlatformWindow::setMouseGrabEnabled` also returns `false`, qplatformwindow.cpp:443-448).
    fn set_mouse_grab_enabled(&mut self, _grab: bool) -> bool {
        false
    }
    /// Whether the window holds the mouse grab ([`set_mouse_grab_enabled`](Self::set_mouse_grab_enabled)
    /// or the automatic grab of a button press; `QWindowsWindow::hasMouseCapture`).
    fn has_mouse_grab(&self) -> bool {
        false
    }
    fn set_opacity(&mut self, opacity: f32);
    fn opacity(&self) -> f32;
    /// `QWindow::setMinimumSize` as `QPlatformWindow::propagateSizeHints` hands it to the window
    /// system: logical pixels, each axis clamped to `0..=WINDOW_SIZE_MAX`; 0 on an axis is no
    /// minimum. A backend that has a window system must pass it on and say so; one that only
    /// stores the value (`GenericWindow`, the X11 and Wayland windows today) does not enforce it.
    fn set_minimum_size(&mut self, min_w: i32, min_h: i32);
    fn minimum_size(&self) -> (i32, i32);
    /// `QWindow::setMaximumSize`, same contract as `set_minimum_size`; `WINDOW_SIZE_MAX` on an axis
    /// is no maximum, which is also how a maximum is removed. Required with no default for the same
    /// reason as `is_active`: whether the limit reaches the window system is a backend's to answer.
    fn set_maximum_size(&mut self, max_w: i32, max_h: i32);
    fn maximum_size(&self) -> (i32, i32);
    /// Ratio between this window's physical (native) pixels and its device-independent pixels,
    /// taken from the screen the window is on (`QPlatformWindow::devicePixelRatio`, which
    /// `QWindowPrivate::updateDevicePixelRatio` multiplies by the window's screen scale factor,
    /// `qwindow.cpp:1436-1443`). Required: it differs between windows on different screens.
    fn device_pixel_ratio(&self) -> f32;
    fn present(&mut self, pixmap: &mut Pixmap, opacity: f32) -> Result<(), &'static str>;
    fn present_dirty(
        &mut self,
        pixmap: &mut Pixmap,
        opacity: f32,
        _dirty: Rect,
    ) -> Result<(), &'static str> {
        self.present(pixmap, opacity)
    }
    fn present_region(
        &mut self,
        pixmap: &Pixmap,
        dirty: &ferrokit_gui::geometry::Region,
    ) -> Result<(), &'static str> {
        let mut clone = (*pixmap).clone();
        let opacity = self.opacity();
        self.present_dirty(&mut clone, opacity, dirty.bounding_rect())
    }
    fn set_event_handler(&mut self, handler: Box<dyn WindowSystemEventHandler>);
    /// Whether the native window delivers `MouseDoubleClick` itself. When false, the widget
    /// window synthesizes double clicks from press pairs, as Qt's `QGuiApplication` does for
    /// platforms without native double clicks (`qguiapplication.cpp:2419-2426`).
    fn reports_native_double_clicks(&self) -> bool {
        false
    }
    /// The native desktop position the next `present_region` moves a layered window to.
    fn set_target_pos(&mut self, _pos: Option<ferrokit_gui::geometry::Point>) {}
    fn native_handle(&self) -> isize;
    fn poll_events(&mut self) -> usize {
        0
    }
    fn set_backdrop(&mut self, _backdrop: crate::backdrop::BackdropType, _dark_mode: bool) -> bool {
        false
    }
    /// Replaces an applied Acrylic blur with the flat tint of the same colour (used while the
    /// window is dragged or resized). Returns whether the applied backdrop changed.
    fn suspend_backdrop(&mut self) -> bool {
        false
    }
    /// Restores a backdrop suspended by `suspend_backdrop`. Returns whether the applied
    /// backdrop changed.
    fn resume_backdrop(&mut self) -> bool {
        false
    }
    /// `QPlatformWindow::setMask` (`qplatformwindow.cpp`): clips the window to `region`, in
    /// native pixels of the window's client area (`QWindow::setMask` hands it the region scaled
    /// by `QHighDpi::toNativeLocalRegion`). An empty region removes the mask. Windows applies it
    /// with `SetWindowRgn` (`QWindowsWindow::setMask`); Cocoa clips the content layer to it (a
    /// layer mask, which makes the window non-opaque while it is set). A backend without window
    /// regions ignores it, as `QPlatformWindow::setMask` does by default.
    fn set_mask(&mut self, _region: &ferrokit_gui::geometry::Region) {}
    /// `QWindow::setTransientParent` as the window system sees it: the native handle
    /// (`native_handle`) of the window that owns this top-level window, or `None` to remove the
    /// owner. On Windows this is the owner (`GWLP_HWNDPARENT`, `QWindowsWindow::updateTransientParent`,
    /// `qwindowswindow.cpp:1939-1966`): the system hides, minimizes and shows the owned window with
    /// its owner and keeps it above it, and gives it no taskbar button. Backends without such a
    /// relationship only remember it.
    fn set_transient_parent(&mut self, _parent: Option<isize>) {}
    /// The owner handed to `set_transient_parent`, if it is still in place.
    fn transient_parent(&self) -> Option<isize> {
        None
    }
    fn set_ime_focus(&mut self, _pos: ferrokit_gui::geometry::primitives::Point) {}
    fn enable_drop_target(&mut self, _enabled: bool) -> bool {
        false
    }
    fn state_flags(&self) -> crate::window::PlatformWindowStateFlags {
        crate::window::PlatformWindowStateFlags::NONE
    }
    fn owner_thread(&self) -> std::thread::ThreadId {
        std::thread::current().id()
    }
    /// The window's current size in native (physical) pixels, if the platform can report it.
    ///
    /// The paint surface is sized to this instead of `round(logical * dpr)`, so the content and the
    /// native window always have the same pixel size (the logical round trip is off by 1 px at
    /// fractional scale factors, e.g. 427 -> 342 -> 428 at 125%).
    fn native_size(&self) -> Option<(u32, u32)> {
        None
    }
    fn is_within_set_geometry(&self) -> bool {
        self.state_flags()
            .contains(crate::window::PlatformWindowStateFlags::WITHIN_SET_GEOMETRY)
    }
}
use crate::window::WindowFlags;
use crate::window_system_interface::WindowSystemEventHandler;
use ferrokit_gui::geometry::primitives::Rect;
use ferrokit_gui::paint::Pixmap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

pub struct GenericWindow {
    /// Set by `FocusIn` and cleared by `FocusOut`/`hide`, from the events the window system
    /// delivers (this backend has no OS window to query).
    active: AtomicBool,
    geometry: Rect,
    visible: AtomicBool,
    stays_on_top: bool,
    click_through: bool,
    flags: WindowFlags,
    handler: Option<Box<dyn WindowSystemEventHandler>>,
    pending_events: Mutex<Vec<crate::window_system_interface::WindowSystemEvent>>,
    last_pixmap: Mutex<Option<Pixmap>>,
    opacity: f32,
    size_limits: crate::size_limits::SizeLimits,
    owner_thread: std::thread::ThreadId,
    state_flags: std::cell::Cell<crate::window::PlatformWindowStateFlags>,
    mask: ferrokit_gui::geometry::Region,
    transient_parent: Option<isize>,
}
impl GenericWindow {
    pub fn new(_title: &str, rect: Rect, flags: WindowFlags) -> Self {
        Self {
            active: AtomicBool::new(false),
            geometry: rect,
            visible: AtomicBool::new(false),
            stays_on_top: flags.contains(WindowFlags::STAYS_ON_TOP)
                || flags.contains(WindowFlags::TOOLTIP),
            click_through: flags.contains(WindowFlags::CLICK_THROUGH),
            flags,
            handler: None,
            pending_events: Mutex::new(Vec::new()),
            last_pixmap: Mutex::new(None),
            opacity: 1.0,
            size_limits: crate::size_limits::SizeLimits::default(),
            owner_thread: std::thread::current().id(),
            state_flags: std::cell::Cell::new(crate::window::PlatformWindowStateFlags::NONE),
            mask: ferrokit_gui::geometry::Region::new(),
            transient_parent: None,
        }
    }

    pub fn is_visible(&self) -> bool {
        self.visible.load(Ordering::Acquire)
    }

    pub fn queue_event(&self, event: crate::window_system_interface::WindowSystemEvent) {
        self.pending_events.lock().unwrap().push(event);
    }

    /// The mask `set_mask` stored (this backend has no window system to apply it to).
    pub fn mask(&self) -> &ferrokit_gui::geometry::Region {
        &self.mask
    }
}

impl PlatformWindow for GenericWindow {
    fn state_flags(&self) -> crate::window::PlatformWindowStateFlags {
        self.state_flags.get()
    }
    fn owner_thread(&self) -> std::thread::ThreadId {
        self.owner_thread
    }
    fn show(&self) {
        self.visible.store(true, Ordering::Release);
    }

    fn hide(&self) {
        self.visible.store(false, Ordering::Release);
        self.active.store(false, Ordering::Release);
    }

    fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    fn geometry(&self) -> Rect {
        self.geometry
    }

    fn set_geometry(&mut self, rect: Rect) {
        self.geometry = rect;
    }

    fn set_stays_on_top(&mut self, enabled: bool) {
        self.stays_on_top = enabled;
        self.flags.set(WindowFlags::STAYS_ON_TOP, enabled);
    }

    fn set_click_through(&mut self, enabled: bool) {
        self.click_through = enabled;
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

    fn start_system_resize(&self, _edges: WindowEdges) -> bool {
        true
    }

    fn set_opacity(&mut self, opacity: f32) {
        self.opacity = opacity.clamp(0.0, 1.0);
    }

    fn opacity(&self) -> f32 {
        self.opacity
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
        self.size_limits.min = crate::size_limits::clamp_size_limit(min_w, min_h);
    }

    fn minimum_size(&self) -> (i32, i32) {
        self.size_limits.min
    }

    /// Stored and reported only: this backend has no window system to hand the limit to.
    fn set_maximum_size(&mut self, max_w: i32, max_h: i32) {
        self.size_limits.max = crate::size_limits::clamp_size_limit(max_w, max_h);
    }

    fn maximum_size(&self) -> (i32, i32) {
        self.size_limits.max
    }

    /// `GenericWindow` has no screen of its own: it reports the primary screen's ratio.
    fn device_pixel_ratio(&self) -> f32 {
        crate::platform().primary_screen().device_pixel_ratio()
    }

    fn present(&mut self, pixmap: &mut Pixmap, opacity: f32) -> Result<(), &'static str> {
        let mut guard = self.last_pixmap.lock().unwrap();
        *guard = Some((*pixmap).clone());
        self.opacity = opacity;
        Ok(())
    }

    fn set_event_handler(&mut self, handler: Box<dyn WindowSystemEventHandler>) {
        self.handler = Some(handler);
    }

    fn native_handle(&self) -> isize {
        0
    }

    fn poll_events(&mut self) -> usize {
        let events = std::mem::take(&mut *self.pending_events.lock().unwrap());
        let count = events.len();
        for event in &events {
            match event {
                crate::window_system_interface::WindowSystemEvent::FocusIn => {
                    self.active.store(true, Ordering::Release)
                }
                crate::window_system_interface::WindowSystemEvent::FocusOut => {
                    self.active.store(false, Ordering::Release)
                }
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
