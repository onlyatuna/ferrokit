// SPDX-License-Identifier: LGPL-3.0-only
#[cfg(target_os = "linux")]
use std::os::unix::io::AsRawFd;
#[cfg(target_os = "linux")]
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use ferrokit_core::event_loop::{EventDispatcher, SocketDescriptor, SocketEvent, SocketNotifier};
use ferrokit_gui::geometry::primitives::{Point, Rect, Size};
use ferrokit_gui::paint::Pixmap;

use crate::platform_window::PlatformWindow;
use crate::surface::wayland::WaylandShmSurface;
use crate::surface::PlatformSurface;
use crate::window::WindowFlags;
use crate::window_system_interface::{
    KeyboardModifiers, MouseButton, PressedButtons, WheelDelta, WindowSystemEvent,
    WindowSystemEventHandler,
};

#[derive(Debug, Clone)]
pub enum WaylandEvent {
    Configure {
        width: i32,
        height: i32,
    },
    PointerMotion {
        surface_x: i32,
        surface_y: i32,
        modifiers: u32,
    },
    PointerButton {
        button: u32,
        state: u32,
        modifiers: u32,
    },
    PointerAxis {
        value: i32,
        modifiers: u32,
    },
    KeyboardKey {
        key: u32,
        state: u32,
        modifiers: u32,
    },
    /// `wl_keyboard.enter`: the surface gained keyboard focus.
    KeyboardEnter,
    /// `wl_keyboard.leave`: the surface lost keyboard focus.
    KeyboardLeave,
    CloseRequest,
    BufferRelease,
}

/// Anchor edges for Wayland layer-shell surfaces (`zwlr_layer_shell_v1`).
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaylandLayerAnchor {
    Top = 1,
    Bottom = 2,
    Left = 4,
    Right = 8,
    TopRight = 1 | 8,
    BottomRight = 2 | 8,
    TopLeft = 1 | 4,
    BottomLeft = 2 | 4,
}

/// Z-stacking level for Wayland layer-shell surfaces.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaylandLayerLevel {
    Background = 0,
    Bottom = 1,
    Top = 2,
    Overlay = 3,
}
/// Not `Send`/`Sync`: owns a `Box<dyn WindowSystemEventHandler>`, which carries no thread-safety
/// bound, so the window stays on the thread that created it.
pub struct WaylandNativeWindow {
    surface_id: u32,
    title: String,
    display_name: String,
    geometry: Rect,
    flags: WindowFlags,
    is_layer_shell: bool,
    layer_level: WaylandLayerLevel,
    layer_anchor: WaylandLayerAnchor,
    layer_margin: (i32, i32, i32, i32),
    layer_shell_fallback: bool,
    stays_on_top: bool,
    click_through: bool,
    visible: AtomicBool,
    /// Keyboard focus (`wl_keyboard.enter`/`leave`); cleared when the surface is hidden.
    active: AtomicBool,
    /// Buttons pressed and not yet released, reported on every `MouseMove`.
    pressed: PressedButtons,
    surface: Mutex<Option<WaylandShmSurface>>,
    connection_fd: SocketDescriptor,
    socket_notifier: Option<Arc<SocketNotifier>>,
    pending_events: Mutex<Vec<WaylandEvent>>,
    event_handler: Option<Box<dyn WindowSystemEventHandler>>,
    opacity: f32,
    size_limits: crate::size_limits::SizeLimits,
    #[cfg(target_os = "linux")]
    /// Owns the compositor connection for the window's lifetime (closed on drop).
    _stream: Option<UnixStream>,
}

static WAYLAND_SURFACE_ID_COUNTER: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(1);

impl WaylandNativeWindow {
    /// Whether the compositor named by `WAYLAND_DISPLAY` accepts a connection.
    pub fn display_reachable() -> bool {
        #[cfg(target_os = "linux")]
        {
            let display_name =
                std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "wayland-0".to_string());
            let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
                .unwrap_or_else(|_| format!("/run/user/{}", crate::current_uid()));
            UnixStream::connect(std::path::Path::new(&runtime_dir).join(display_name)).is_ok()
        }
        #[cfg(not(target_os = "linux"))]
        {
            false
        }
    }
}

impl WaylandNativeWindow {
    /// Opens a surface on the compositor named by `WAYLAND_DISPLAY` (default `wayland-0`, relative
    /// to `XDG_RUNTIME_DIR`). Fails if the compositor socket cannot be reached, and on platforms
    /// without Wayland. Use [`new_offscreen`](Self::new_offscreen) for a surface with no
    /// compositor.
    pub fn new(title: &str, rect: Rect, flags: WindowFlags) -> Result<Self, &'static str> {
        #[cfg(target_os = "linux")]
        {
            let display_name =
                std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "wayland-0".to_string());
            let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
                .unwrap_or_else(|_| format!("/run/user/{}", crate::current_uid()));
            // An absolute WAYLAND_DISPLAY is the socket path itself (`Path::join` replaces).
            let socket_path = std::path::Path::new(&runtime_dir).join(&display_name);
            let stream = UnixStream::connect(socket_path)
                .map_err(|_| "Failed to connect to the Wayland display")?;
            let _ = stream.set_nonblocking(true);
            Self::build(title, rect, flags, display_name, Some(stream))
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (title, rect, flags);
            Err("Wayland windows are unavailable on this platform")
        }
    }

    /// A surface with no compositor behind it, driven only through `queue_wayland_event` (the
    /// analogue of Qt's `offscreen` platform plugin). It owns no socket, so `connection_fd()` is
    /// `-1` and `bind_event_dispatcher` registers nothing.
    pub fn new_offscreen(
        title: &str,
        rect: Rect,
        flags: WindowFlags,
    ) -> Result<Self, &'static str> {
        let display_name =
            std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "wayland-0".to_string());
        Self::build(
            title,
            rect,
            flags,
            display_name,
            #[cfg(target_os = "linux")]
            None,
        )
    }

    fn build(
        title: &str,
        rect: Rect,
        flags: WindowFlags,
        display_name: String,
        #[cfg(target_os = "linux")] stream: Option<UnixStream>,
    ) -> Result<Self, &'static str> {
        let surface_id = WAYLAND_SURFACE_ID_COUNTER.fetch_add(1, Ordering::SeqCst);
        #[cfg(target_os = "linux")]
        let connection_fd = stream
            .as_ref()
            .map_or(-1, |s| s.as_raw_fd() as SocketDescriptor);
        #[cfg(not(target_os = "linux"))]
        let connection_fd: SocketDescriptor = -1;
        let is_layer_shell =
            flags.contains(WindowFlags::FRAMELESS) || flags.contains(WindowFlags::STAYS_ON_TOP);
        let stays_on_top =
            flags.contains(WindowFlags::STAYS_ON_TOP) || flags.contains(WindowFlags::TOOLTIP);
        let click_through = flags.contains(WindowFlags::CLICK_THROUGH);

        let surface = if rect.width > 0 && rect.height > 0 {
            Some(WaylandShmSurface::new(
                surface_id,
                rect.width as u32,
                rect.height as u32,
            )?)
        } else {
            None
        };

        Ok(Self {
            surface_id,
            title: title.to_string(),
            display_name,
            geometry: rect,
            flags,
            is_layer_shell,
            layer_level: WaylandLayerLevel::Overlay,
            layer_anchor: WaylandLayerAnchor::TopRight,
            layer_margin: (16, 16, 0, 0),
            layer_shell_fallback: false,
            stays_on_top,
            click_through,
            visible: AtomicBool::new(false),
            active: AtomicBool::new(false),
            pressed: PressedButtons::default(),
            surface: Mutex::new(surface),
            connection_fd,
            socket_notifier: None,
            pending_events: Mutex::new(Vec::new()),
            event_handler: None,
            opacity: 1.0,
            size_limits: crate::size_limits::SizeLimits::default(),
            #[cfg(target_os = "linux")]
            _stream: stream,
        })
    }

    /// Whether a compositor connection backs this surface (false for offscreen surfaces).
    #[inline]
    pub fn is_live_compositor(&self) -> bool {
        self.connection_fd >= 0
    }

    #[inline]
    pub fn surface_id(&self) -> u32 {
        self.surface_id
    }

    #[inline]
    pub fn is_buffer_busy(&self) -> bool {
        self.surface
            .lock()
            .unwrap()
            .as_ref()
            .map(|s| s.is_busy())
            .unwrap_or(false)
    }

    #[inline]
    pub fn title(&self) -> &str {
        &self.title
    }

    #[inline]
    pub fn flags(&self) -> WindowFlags {
        self.flags
    }

    #[inline]
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    #[inline]
    pub fn is_layer_shell(&self) -> bool {
        self.is_layer_shell
    }
    #[inline]
    pub fn layer_level(&self) -> WaylandLayerLevel {
        self.layer_level
    }

    pub fn set_layer_level(&mut self, level: WaylandLayerLevel) {
        self.layer_level = level;
    }

    #[inline]
    pub fn layer_anchor(&self) -> WaylandLayerAnchor {
        self.layer_anchor
    }

    pub fn set_layer_anchor(&mut self, anchor: WaylandLayerAnchor) {
        self.layer_anchor = anchor;
    }

    #[inline]
    pub fn layer_margin(&self) -> (i32, i32, i32, i32) {
        self.layer_margin
    }

    pub fn set_layer_margin(&mut self, top: i32, right: i32, bottom: i32, left: i32) {
        self.layer_margin = (top, right, bottom, left);
    }

    #[inline]
    pub fn is_layer_shell_fallback(&self) -> bool {
        self.layer_shell_fallback
    }

    /// Degrades layer-shell surface to standard xdg_toplevel if compositor rejects protocol.
    pub fn fallback_to_xdg_toplevel(&mut self) {
        self.is_layer_shell = false;
        self.layer_shell_fallback = true;
    }

    #[inline]
    pub fn connection_fd(&self) -> SocketDescriptor {
        self.connection_fd
    }

    /// Watches the compositor socket; an offscreen surface has none, so nothing is registered.
    pub fn bind_event_dispatcher(&mut self, dispatcher: &mut dyn EventDispatcher) {
        if self.connection_fd < 0 {
            return;
        }
        let notifier = Arc::new(SocketNotifier::new(self.connection_fd, SocketEvent::Read));
        dispatcher.register_socket_notifier(&notifier);
        self.socket_notifier = Some(notifier);
    }

    pub fn queue_wayland_event(&self, event: WaylandEvent) {
        self.pending_events.lock().unwrap().push(event);
    }

    pub fn dispatch_wayland_event(&mut self, event: WaylandEvent) -> bool {
        match event {
            WaylandEvent::KeyboardEnter => self.active.store(true, Ordering::Release),
            WaylandEvent::KeyboardLeave => self.active.store(false, Ordering::Release),
            _ => {}
        }

        if let WaylandEvent::BufferRelease = event {
            if let Some(surface) = self.surface.lock().unwrap().as_ref() {
                surface.on_buffer_release();
            }
            return true;
        }

        let Some(handler) = self.event_handler.as_mut() else {
            return false;
        };
        let origin_x = self.geometry.x;
        let origin_y = self.geometry.y;

        match event {
            WaylandEvent::Configure { width, height } => {
                self.geometry.width = width;
                self.geometry.height = height;
                handler.handle_window_event(WindowSystemEvent::Resize {
                    size: Size::new(width, height),
                });
            }
            WaylandEvent::PointerMotion {
                surface_x,
                surface_y,
                modifiers: _,
            } => {
                handler.handle_window_event(WindowSystemEvent::MouseMove {
                    pos: Point::new(surface_x, surface_y),
                    global_pos: Point::new(origin_x + surface_x, origin_y + surface_y),
                    buttons: self.pressed.buttons(),
                });
            }
            WaylandEvent::PointerButton {
                button,
                state,
                modifiers,
            } => {
                let btn = match button {
                    0x110 => MouseButton::Left,
                    0x111 => MouseButton::Right,
                    0x112 => MouseButton::Middle,
                    _ => MouseButton::Left,
                };
                let local_pos = Point::new(0, 0);
                let global_pos = Point::new(origin_x, origin_y);
                let mods = KeyboardModifiers::from_bits(modifiers);

                if state == 1 {
                    self.pressed.press(btn);
                    handler.handle_window_event(WindowSystemEvent::MousePress {
                        pos: local_pos,
                        global_pos,
                        button: btn,
                        modifiers: mods,
                        // `WaylandEvent::PointerButton` carries no time; the press is stamped on arrival.
                        timestamp: crate::window_system_interface::event_time_ms(),
                    });
                } else {
                    self.pressed.release(btn);
                    handler.handle_window_event(WindowSystemEvent::MouseRelease {
                        pos: local_pos,
                        global_pos,
                        button: btn,
                        modifiers: mods,
                    });
                }
            }
            WaylandEvent::PointerAxis { value, modifiers } => {
                handler.handle_window_event(WindowSystemEvent::Wheel {
                    pos: Point::new(0, 0),
                    global_pos: Point::new(origin_x, origin_y),
                    delta: WheelDelta::vertical(-value * 12),
                    modifiers: KeyboardModifiers::from_bits(modifiers),
                });
            }
            WaylandEvent::KeyboardKey {
                key,
                state,
                modifiers,
            } => {
                let mods = KeyboardModifiers::from_bits(modifiers);
                if state == 1 {
                    handler.handle_window_event(WindowSystemEvent::KeyPress {
                        key,
                        modifiers: mods,
                        is_repeat: false,
                    });
                } else {
                    handler.handle_window_event(WindowSystemEvent::KeyRelease {
                        key,
                        modifiers: mods,
                    });
                }
            }
            WaylandEvent::KeyboardEnter => {
                handler.handle_window_event(WindowSystemEvent::FocusIn);
            }
            WaylandEvent::KeyboardLeave => {
                handler.handle_window_event(WindowSystemEvent::FocusOut);
            }
            WaylandEvent::CloseRequest => {
                handler.handle_window_event(WindowSystemEvent::CloseRequest);
            }
            WaylandEvent::BufferRelease => {
                if let Some(surface) = self.surface.lock().unwrap().as_ref() {
                    surface.on_buffer_release();
                }
            }
        }
        true
    }
}

impl PlatformWindow for WaylandNativeWindow {
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
        let mut surface_guard = self.surface.lock().unwrap();
        if let Some(surf) = surface_guard.as_mut() {
            if rect.width > 0 && rect.height > 0 {
                let _ = surf.resize(rect.width as u32, rect.height as u32);
            }
        }
    }

    fn set_stays_on_top(&mut self, enabled: bool) {
        self.stays_on_top = enabled;
        self.flags.set(WindowFlags::STAYS_ON_TOP, enabled);
    }

    fn set_click_through(&mut self, enabled: bool) {
        self.click_through = enabled;
        self.flags.set(WindowFlags::CLICK_THROUGH, enabled);
        // On Wayland, setting an empty input region via wl_surface.set_input_region(NULL)
        // makes pointer events pass through to underlying windows
    }

    fn flags(&self) -> WindowFlags {
        self.flags
    }

    fn set_backdrop(&mut self, backdrop: crate::backdrop::BackdropType, _dark_mode: bool) -> bool {
        match backdrop {
            crate::backdrop::BackdropType::None => true,
            _ => true,
        }
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
        self.opacity = opacity.clamp(0.0, 1.0);
    }

    fn opacity(&self) -> f32 {
        self.opacity
    }

    /// Stored and reported only: this backend does not pass size hints to the compositor
    /// (`xdg_toplevel.set_min_size`/`set_max_size`), so a minimum or maximum is not enforced on Wayland.
    fn set_minimum_size(&mut self, min_w: i32, min_h: i32) {
        self.size_limits.min = crate::size_limits::clamp_size_limit(min_w, min_h);
    }

    fn minimum_size(&self) -> (i32, i32) {
        self.size_limits.min
    }

    fn set_maximum_size(&mut self, max_w: i32, max_h: i32) {
        self.size_limits.max = crate::size_limits::clamp_size_limit(max_w, max_h);
    }

    fn maximum_size(&self) -> (i32, i32) {
        self.size_limits.max
    }

    fn present(&mut self, pixmap: &mut Pixmap, opacity: f32) -> Result<(), &'static str> {
        let mut surface_guard = self.surface.lock().unwrap();
        if let Some(surf) = surface_guard.as_mut() {
            surf.present(pixmap, opacity)?;
        }
        Ok(())
    }

    fn set_event_handler(&mut self, handler: Box<dyn WindowSystemEventHandler>) {
        self.event_handler = Some(handler);
    }

    fn native_handle(&self) -> isize {
        self.surface_id as isize
    }

    /// Per-window screen lookup is not implemented on this backend yet: the primary screen's
    /// ratio (what every window used before `PlatformWindow::device_pixel_ratio` existed).
    fn device_pixel_ratio(&self) -> f32 {
        crate::platform().primary_screen().device_pixel_ratio()
    }

    fn poll_events(&mut self) -> usize {
        let events = std::mem::take(&mut *self.pending_events.lock().unwrap());
        let count = events.len();
        for event in events {
            self.dispatch_wayland_event(event);
        }
        count
    }
}
