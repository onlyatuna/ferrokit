// SPDX-License-Identifier: LGPL-3.0-only
#[cfg(target_os = "linux")]
use std::os::unix::io::AsRawFd;
#[cfg(target_os = "linux")]
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[cfg(target_os = "linux")]
use wayland_client::protocol::{
    wl_buffer::{self, WlBuffer},
    wl_compositor::WlCompositor,
    wl_keyboard::{self, WlKeyboard},
    wl_pointer::{self, WlPointer},
    wl_region::WlRegion,
    wl_registry::{self, WlRegistry},
    wl_seat::{self, WlSeat},
    wl_shm::WlShm,
    wl_shm_pool::WlShmPool,
    wl_surface::WlSurface,
};
#[cfg(target_os = "linux")]
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum, delegate_noop};
#[cfg(target_os = "linux")]
use wayland_protocols::xdg::decoration::zv1::client::{
    zxdg_decoration_manager_v1::ZxdgDecorationManagerV1,
    zxdg_toplevel_decoration_v1::{self, ZxdgToplevelDecorationV1},
};
#[cfg(target_os = "linux")]
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::{self, XdgToplevel},
    xdg_wm_base::{self, XdgWmBase},
};
#[cfg(target_os = "linux")]
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{self, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{self, ZwlrLayerSurfaceV1},
};

use ferrokit_core::event_loop::{
    EventDispatcher, SocketDescriptor, SocketEvent, SocketNotifier, SocketNotifierError,
};
use ferrokit_gui::geometry::primitives::{Point, Rect, Size};
use ferrokit_gui::paint::Pixmap;

use crate::platform_window::PlatformWindow;
use crate::surface::PlatformSurface;
use crate::surface::wayland::WaylandShmSurface;
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
#[cfg(target_os = "linux")]
#[derive(Default)]
pub(crate) struct WaylandAppState {
    compositor: Option<WlCompositor>,
    shm: Option<WlShm>,
    wm_base: Option<XdgWmBase>,
    layer_shell: Option<ZwlrLayerShellV1>,
    decoration_manager: Option<ZxdgDecorationManagerV1>,
    seat: Option<WlSeat>,
    pointer: Option<WlPointer>,
    keyboard: Option<WlKeyboard>,
    last_pointer_serial: u32,
    modifiers: u32,
    events: Vec<WaylandEvent>,
}

#[cfg(target_os = "linux")]
impl Dispatch<WlRegistry, ()> for WaylandAppState {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_compositor" => {
                    state.compositor =
                        Some(registry.bind::<WlCompositor, _, _>(name, version.min(6), qh, ()));
                }
                "wl_shm" => {
                    state.shm = Some(registry.bind::<WlShm, _, _>(name, version.min(1), qh, ()));
                }
                "xdg_wm_base" => {
                    state.wm_base =
                        Some(registry.bind::<XdgWmBase, _, _>(name, version.min(6), qh, ()));
                }
                "zwlr_layer_shell_v1" => {
                    state.layer_shell =
                        Some(registry.bind::<ZwlrLayerShellV1, _, _>(name, version.min(4), qh, ()));
                }
                "zxdg_decoration_manager_v1" => {
                    state.decoration_manager =
                        Some(registry.bind::<ZxdgDecorationManagerV1, _, _>(
                            name,
                            version.min(1),
                            qh,
                            (),
                        ));
                }
                "wl_seat" => {
                    state.seat = Some(registry.bind::<WlSeat, _, _>(name, version.min(7), qh, ()));
                }
                _ => {}
            }
        }
    }
}

#[cfg(target_os = "linux")]
delegate_noop!(WaylandAppState: ignore WlCompositor);
#[cfg(target_os = "linux")]
delegate_noop!(WaylandAppState: ignore WlSurface);
#[cfg(target_os = "linux")]
delegate_noop!(WaylandAppState: ignore WlShm);
#[cfg(target_os = "linux")]
delegate_noop!(WaylandAppState: ignore WlShmPool);
#[cfg(target_os = "linux")]
delegate_noop!(WaylandAppState: ignore WlRegion);
#[cfg(target_os = "linux")]
delegate_noop!(WaylandAppState: ignore ZwlrLayerShellV1);
#[cfg(target_os = "linux")]
delegate_noop!(WaylandAppState: ignore ZxdgDecorationManagerV1);
#[cfg(target_os = "linux")]
delegate_noop!(WaylandAppState: ignore ZxdgToplevelDecorationV1);

#[cfg(target_os = "linux")]
impl Dispatch<WlBuffer, ()> for WaylandAppState {
    fn event(
        state: &mut Self,
        _: &WlBuffer,
        event: wl_buffer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_buffer::Event::Release = event {
            state.events.push(WaylandEvent::BufferRelease);
        }
    }
}

#[cfg(target_os = "linux")]
impl Dispatch<XdgWmBase, ()> for WaylandAppState {
    fn event(
        _: &mut Self,
        wm_base: &XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

#[cfg(target_os = "linux")]
impl Dispatch<XdgSurface, ()> for WaylandAppState {
    fn event(
        _: &mut Self,
        xdg_surface: &XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg_surface.ack_configure(serial);
        }
    }
}

#[cfg(target_os = "linux")]
impl Dispatch<XdgToplevel, ()> for WaylandAppState {
    fn event(
        state: &mut Self,
        _: &XdgToplevel,
        event: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            xdg_toplevel::Event::Configure { width, height, .. } if width > 0 && height > 0 => {
                state.events.push(WaylandEvent::Configure { width, height });
            }
            xdg_toplevel::Event::Close => {
                state.events.push(WaylandEvent::CloseRequest);
            }
            _ => {}
        }
    }
}

#[cfg(target_os = "linux")]
impl Dispatch<ZwlrLayerSurfaceV1, ()> for WaylandAppState {
    fn event(
        state: &mut Self,
        layer_surface: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure {
                serial,
                width,
                height,
            } => {
                layer_surface.ack_configure(serial);
                if width > 0 && height > 0 {
                    state.events.push(WaylandEvent::Configure {
                        width: width as i32,
                        height: height as i32,
                    });
                }
            }
            zwlr_layer_surface_v1::Event::Closed => {
                state.events.push(WaylandEvent::CloseRequest);
            }
            _ => {}
        }
    }
}

#[cfg(target_os = "linux")]
impl Dispatch<WlSeat, ()> for WaylandAppState {
    fn event(
        state: &mut Self,
        seat: &WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(caps),
        } = event
        {
            if caps.contains(wl_seat::Capability::Pointer) && state.pointer.is_none() {
                state.pointer = Some(seat.get_pointer(qh, ()));
            }
            if caps.contains(wl_seat::Capability::Keyboard) && state.keyboard.is_none() {
                state.keyboard = Some(seat.get_keyboard(qh, ()));
            }
        }
    }
}

#[cfg(target_os = "linux")]
impl Dispatch<WlPointer, ()> for WaylandAppState {
    fn event(
        state: &mut Self,
        _: &WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter { serial, .. } => {
                state.last_pointer_serial = serial;
            }
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => {
                state.events.push(WaylandEvent::PointerMotion {
                    surface_x: surface_x.round() as i32,
                    surface_y: surface_y.round() as i32,
                    modifiers: state.modifiers,
                });
            }
            wl_pointer::Event::Button {
                serial,
                button,
                state: btn_state,
                ..
            } => {
                state.last_pointer_serial = serial;
                let pressed = matches!(btn_state, WEnum::Value(wl_pointer::ButtonState::Pressed));
                state.events.push(WaylandEvent::PointerButton {
                    button,
                    state: u32::from(pressed),
                    modifiers: state.modifiers,
                });
            }
            wl_pointer::Event::Axis { value, .. } => {
                state.events.push(WaylandEvent::PointerAxis {
                    value: value.round() as i32,
                    modifiers: state.modifiers,
                });
            }
            _ => {}
        }
    }
}

#[cfg(target_os = "linux")]
impl Dispatch<WlKeyboard, ()> for WaylandAppState {
    fn event(
        state: &mut Self,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Enter { .. } => {
                state.events.push(WaylandEvent::KeyboardEnter);
            }
            wl_keyboard::Event::Leave { .. } => {
                state.events.push(WaylandEvent::KeyboardLeave);
            }
            wl_keyboard::Event::Key {
                key,
                state: key_state,
                ..
            } => {
                let pressed = matches!(key_state, WEnum::Value(wl_keyboard::KeyState::Pressed));
                state.events.push(WaylandEvent::KeyboardKey {
                    key,
                    state: u32::from(pressed),
                    modifiers: state.modifiers,
                });
            }
            wl_keyboard::Event::Modifiers {
                mods_depressed,
                mods_latched,
                mods_locked,
                ..
            } => {
                let xkb = mods_depressed | mods_latched | mods_locked;
                let mods = KeyboardModifiers {
                    shift: (xkb & (1 << 0)) != 0,
                    control: (xkb & (1 << 2)) != 0,
                    alt: (xkb & (1 << 3)) != 0,
                    meta: (xkb & (1 << 6)) != 0,
                };
                state.modifiers = mods.bits();
            }
            _ => {}
        }
    }
}

#[cfg(target_os = "linux")]
enum LiveRole {
    Xdg {
        xdg_surface: XdgSurface,
        toplevel: XdgToplevel,
        decoration: Option<ZxdgToplevelDecorationV1>,
    },
    Layer {
        layer_surface: ZwlrLayerSurfaceV1,
    },
}

#[cfg(target_os = "linux")]
struct WaylandLiveWindow {
    conn: Connection,
    event_queue: EventQueue<WaylandAppState>,
    state: WaylandAppState,
    wl_surface: WlSurface,
    role: LiveRole,
}

#[cfg(target_os = "linux")]
impl Drop for WaylandLiveWindow {
    fn drop(&mut self) {
        match &self.role {
            LiveRole::Xdg {
                xdg_surface,
                toplevel,
                decoration,
            } => {
                if let Some(dec) = decoration {
                    dec.destroy();
                }
                toplevel.destroy();
                xdg_surface.destroy();
            }
            LiveRole::Layer { layer_surface } => {
                layer_surface.destroy();
            }
        }
        self.wl_surface.destroy();
        let _ = self.conn.flush();
    }
}

#[cfg(target_os = "linux")]
impl WaylandLiveWindow {
    fn map_layer(level: WaylandLayerLevel) -> zwlr_layer_shell_v1::Layer {
        match level {
            WaylandLayerLevel::Background => zwlr_layer_shell_v1::Layer::Background,
            WaylandLayerLevel::Bottom => zwlr_layer_shell_v1::Layer::Bottom,
            WaylandLayerLevel::Top => zwlr_layer_shell_v1::Layer::Top,
            WaylandLayerLevel::Overlay => zwlr_layer_shell_v1::Layer::Overlay,
        }
    }

    fn map_anchor(anchor: WaylandLayerAnchor) -> zwlr_layer_surface_v1::Anchor {
        use zwlr_layer_surface_v1::Anchor;
        let bits = anchor as u32;
        let mut out = Anchor::empty();
        if (bits & 1) != 0 {
            out |= Anchor::Top;
        }
        if (bits & 2) != 0 {
            out |= Anchor::Bottom;
        }
        if (bits & 4) != 0 {
            out |= Anchor::Left;
        }
        if (bits & 8) != 0 {
            out |= Anchor::Right;
        }
        out
    }

    fn apply_click_through(&self, enabled: bool) {
        if enabled {
            if let Some(compositor) = self.state.compositor.as_ref() {
                let region = compositor.create_region(&self.event_queue.handle(), ());
                self.wl_surface.set_input_region(Some(&region));
                region.destroy();
            }
        } else {
            self.wl_surface.set_input_region(None);
        }
        self.wl_surface.commit();
        let _ = self.conn.flush();
    }

    fn poll_connection(&mut self, pending: &Mutex<Vec<WaylandEvent>>) {
        let _ = self.event_queue.dispatch_pending(&mut self.state);
        if let Some(guard) = self.event_queue.prepare_read() {
            let _ = guard.read();
        }
        let _ = self.event_queue.dispatch_pending(&mut self.state);
        if !self.state.events.is_empty() {
            ferrokit_core::poison::lock_unpoisoned(pending).append(&mut self.state.events);
        }
    }
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
    live: Option<WaylandLiveWindow>,
    #[cfg(target_os = "linux")]
    live_conn: Option<Connection>,
    #[cfg(target_os = "linux")]
    live_surface: Option<WlSurface>,
}

static WAYLAND_SURFACE_ID_COUNTER: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(1);

/// Error returned when the Wayland display socket cannot be connected.
pub(crate) const WAYLAND_CONNECT_FAILED: &str = "Failed to connect to the Wayland display";

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
            let stream = UnixStream::connect(socket_path).map_err(|_| WAYLAND_CONNECT_FAILED)?;
            let _ = stream.set_nonblocking(true);
            Self::connect_socket(title, rect, flags, display_name, stream)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (title, rect, flags);
            Err("Wayland windows are unavailable on this platform")
        }
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn connect_socket(
        title: &str,
        rect: Rect,
        flags: WindowFlags,
        display_name: String,
        stream: UnixStream,
    ) -> Result<Self, &'static str> {
        let _ = stream.set_nonblocking(true);
        let conn = Connection::from_socket(stream).map_err(|_| WAYLAND_CONNECT_FAILED)?;
        let mut event_queue = conn.new_event_queue::<WaylandAppState>();
        let qh = event_queue.handle();
        let _registry = conn.display().get_registry(&qh, ());
        let mut state = WaylandAppState::default();
        event_queue
            .roundtrip(&mut state)
            .map_err(|_| WAYLAND_CONNECT_FAILED)?;

        let compositor = state
            .compositor
            .as_ref()
            .ok_or("Wayland compositor missing wl_compositor")?;
        let wl_surface = compositor.create_surface(&qh, ());

        let mut is_layer_shell =
            flags.contains(WindowFlags::FRAMELESS) || flags.contains(WindowFlags::STAYS_ON_TOP);
        let mut layer_shell_fallback = false;
        let layer_level = WaylandLayerLevel::Overlay;
        let layer_anchor = WaylandLayerAnchor::TopRight;
        let layer_margin = (16, 16, 0, 0);

        let role = if is_layer_shell && let Some(layer_shell) = state.layer_shell.as_ref() {
            let layer_surface = layer_shell.get_layer_surface(
                &wl_surface,
                None,
                WaylandLiveWindow::map_layer(layer_level),
                title.to_string(),
                &qh,
                (),
            );
            layer_surface.set_anchor(WaylandLiveWindow::map_anchor(layer_anchor));
            layer_surface.set_size(rect.width.max(1) as u32, rect.height.max(1) as u32);
            layer_surface.set_margin(
                layer_margin.0,
                layer_margin.1,
                layer_margin.2,
                layer_margin.3,
            );
            layer_surface
                .set_keyboard_interactivity(zwlr_layer_surface_v1::KeyboardInteractivity::None);
            LiveRole::Layer { layer_surface }
        } else {
            if is_layer_shell {
                is_layer_shell = false;
                layer_shell_fallback = true;
            }
            let wm_base = state
                .wm_base
                .as_ref()
                .ok_or("Wayland compositor missing xdg_wm_base")?;
            let xdg_surface = wm_base.get_xdg_surface(&wl_surface, &qh, ());
            let toplevel = xdg_surface.get_toplevel(&qh, ());
            toplevel.set_title(title.to_string());
            let decoration = if flags.contains(WindowFlags::FRAMELESS)
                && let Some(dec_mgr) = state.decoration_manager.as_ref()
            {
                let dec = dec_mgr.get_toplevel_decoration(&toplevel, &qh, ());
                dec.set_mode(zxdg_toplevel_decoration_v1::Mode::ClientSide);
                Some(dec)
            } else {
                None
            };
            LiveRole::Xdg {
                xdg_surface,
                toplevel,
                decoration,
            }
        };

        if flags.contains(WindowFlags::CLICK_THROUGH) {
            let region = compositor.create_region(&qh, ());
            wl_surface.set_input_region(Some(&region));
            region.destroy();
        }

        wl_surface.commit();
        let _ = conn.flush();
        let _ = event_queue.roundtrip(&mut state);

        let connection_fd = conn.backend().poll_fd().as_raw_fd() as SocketDescriptor;
        let surface_id = WAYLAND_SURFACE_ID_COUNTER.fetch_add(1, Ordering::SeqCst);
        let surface = if rect.width > 0 && rect.height > 0 {
            if let Some(wl_shm) = state.shm.as_ref() {
                Some(WaylandShmSurface::new_with_wayland(
                    surface_id,
                    rect.width as u32,
                    rect.height as u32,
                    conn.clone(),
                    wl_surface.clone(),
                    wl_shm.clone(),
                    qh.clone(),
                )?)
            } else {
                Some(WaylandShmSurface::new(
                    surface_id,
                    rect.width as u32,
                    rect.height as u32,
                )?)
            }
        } else {
            None
        };

        let pending_events = Mutex::new(std::mem::take(&mut state.events));
        let live_conn = Some(conn.clone());
        let live_surface = Some(wl_surface.clone());
        let live = Some(WaylandLiveWindow {
            conn,
            event_queue,
            state,
            wl_surface,
            role,
        });

        Ok(Self {
            surface_id,
            title: title.to_string(),
            display_name,
            geometry: rect,
            flags,
            is_layer_shell,
            layer_level,
            layer_anchor,
            layer_margin,
            layer_shell_fallback,
            stays_on_top: flags.contains(WindowFlags::STAYS_ON_TOP)
                || flags.contains(WindowFlags::TOOLTIP),
            click_through: flags.contains(WindowFlags::CLICK_THROUGH),
            visible: AtomicBool::new(false),
            active: AtomicBool::new(false),
            pressed: PressedButtons::default(),
            surface: Mutex::new(surface),
            connection_fd,
            socket_notifier: None,
            pending_events,
            event_handler: None,
            opacity: 1.0,
            size_limits: crate::size_limits::SizeLimits::default(),
            live,
            live_conn,
            live_surface,
        })
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
        Self::build(title, rect, flags, display_name)
    }

    fn build(
        title: &str,
        rect: Rect,
        flags: WindowFlags,
        display_name: String,
    ) -> Result<Self, &'static str> {
        let surface_id = WAYLAND_SURFACE_ID_COUNTER.fetch_add(1, Ordering::SeqCst);
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
            live: None,
            #[cfg(target_os = "linux")]
            live_conn: None,
            #[cfg(target_os = "linux")]
            live_surface: None,
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
        ferrokit_core::poison::lock_unpoisoned(&self.surface)
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
        #[cfg(target_os = "linux")]
        if let Some(live) = self.live.as_ref()
            && let LiveRole::Layer { layer_surface } = &live.role
        {
            layer_surface.set_layer(WaylandLiveWindow::map_layer(level));
            live.wl_surface.commit();
            let _ = live.conn.flush();
        }
    }

    #[inline]
    pub fn layer_anchor(&self) -> WaylandLayerAnchor {
        self.layer_anchor
    }

    pub fn set_layer_anchor(&mut self, anchor: WaylandLayerAnchor) {
        self.layer_anchor = anchor;
        #[cfg(target_os = "linux")]
        if let Some(live) = self.live.as_ref()
            && let LiveRole::Layer { layer_surface } = &live.role
        {
            layer_surface.set_anchor(WaylandLiveWindow::map_anchor(anchor));
            live.wl_surface.commit();
            let _ = live.conn.flush();
        }
    }

    #[inline]
    pub fn layer_margin(&self) -> (i32, i32, i32, i32) {
        self.layer_margin
    }

    pub fn set_layer_margin(&mut self, top: i32, right: i32, bottom: i32, left: i32) {
        self.layer_margin = (top, right, bottom, left);
        #[cfg(target_os = "linux")]
        if let Some(live) = self.live.as_ref()
            && let LiveRole::Layer { layer_surface } = &live.role
        {
            layer_surface.set_margin(top, right, bottom, left);
            live.wl_surface.commit();
            let _ = live.conn.flush();
        }
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

    /// Watches the compositor socket; an offscreen surface has none, so nothing is registered. A
    /// dispatcher that does not poll descriptors (every one but the Linux one) refuses with
    /// [`SocketNotifierError::Unsupported`], because compositor events would never be read.
    pub fn bind_event_dispatcher(
        &mut self,
        dispatcher: &mut dyn EventDispatcher,
    ) -> Result<(), SocketNotifierError> {
        if self.connection_fd < 0 {
            return Ok(());
        }
        let notifier = Arc::new(SocketNotifier::new(self.connection_fd, SocketEvent::Read));
        dispatcher.register_socket_notifier(&notifier)?;
        self.socket_notifier = Some(notifier);
        Ok(())
    }

    pub fn queue_wayland_event(&self, event: WaylandEvent) {
        ferrokit_core::poison::lock_unpoisoned(&self.pending_events).push(event);
    }

    pub fn dispatch_wayland_event(&mut self, event: WaylandEvent) -> bool {
        match event {
            WaylandEvent::KeyboardEnter => self.active.store(true, Ordering::Release),
            WaylandEvent::KeyboardLeave => self.active.store(false, Ordering::Release),
            _ => {}
        }

        if let WaylandEvent::BufferRelease = event {
            if let Some(surface) = ferrokit_core::poison::lock_unpoisoned(&self.surface).as_ref() {
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
                if let Some(surface) =
                    ferrokit_core::poison::lock_unpoisoned(&self.surface).as_ref()
                {
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
        #[cfg(target_os = "linux")]
        if let (Some(surface), Some(conn)) = (self.live_surface.as_ref(), self.live_conn.as_ref()) {
            surface.commit();
            let _ = conn.flush();
        }
    }

    fn hide(&self) {
        self.visible.store(false, Ordering::Release);
        self.active.store(false, Ordering::Release);
        #[cfg(target_os = "linux")]
        if let (Some(surface), Some(conn)) = (self.live_surface.as_ref(), self.live_conn.as_ref()) {
            surface.attach(None, 0, 0);
            surface.commit();
            let _ = conn.flush();
        }
    }

    fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    fn geometry(&self) -> Rect {
        self.geometry
    }

    fn set_geometry(&mut self, rect: Rect) {
        self.geometry = rect;
        #[cfg(target_os = "linux")]
        if let Some(live) = self.live.as_ref()
            && let LiveRole::Layer { layer_surface } = &live.role
            && rect.width > 0
            && rect.height > 0
        {
            layer_surface.set_size(rect.width as u32, rect.height as u32);
            live.wl_surface.commit();
            let _ = live.conn.flush();
        }
        let mut surface_guard = ferrokit_core::poison::lock_unpoisoned(&self.surface);
        if let Some(surf) = surface_guard.as_mut()
            && rect.width > 0
            && rect.height > 0
        {
            let _ = surf.resize(rect.width as u32, rect.height as u32);
        }
    }

    fn set_stays_on_top(&mut self, enabled: bool) {
        self.stays_on_top = enabled;
        self.flags.set(WindowFlags::STAYS_ON_TOP, enabled);
    }

    fn set_click_through(&mut self, enabled: bool) {
        self.click_through = enabled;
        self.flags.set(WindowFlags::CLICK_THROUGH, enabled);
        #[cfg(target_os = "linux")]
        if let Some(live) = self.live.as_ref() {
            live.apply_click_through(enabled);
        }
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
        #[cfg(target_os = "linux")]
        if let Some(live) = self.live.as_ref()
            && let LiveRole::Xdg { toplevel, .. } = &live.role
            && let Some(seat) = live.state.seat.as_ref()
        {
            toplevel._move(seat, live.state.last_pointer_serial);
            let _ = live.conn.flush();
        }
        true
    }

    fn start_system_resize(&self, edges: crate::platform_window::WindowEdges) -> bool {
        #[cfg(target_os = "linux")]
        if let Some(live) = self.live.as_ref()
            && let LiveRole::Xdg { toplevel, .. } = &live.role
            && let Some(seat) = live.state.seat.as_ref()
        {
            use crate::platform_window::WindowEdges;
            let edge = if edges.contains(WindowEdges::TOP_LEFT) {
                xdg_toplevel::ResizeEdge::TopLeft
            } else if edges.contains(WindowEdges::TOP_RIGHT) {
                xdg_toplevel::ResizeEdge::TopRight
            } else if edges.contains(WindowEdges::BOTTOM_LEFT) {
                xdg_toplevel::ResizeEdge::BottomLeft
            } else if edges.contains(WindowEdges::BOTTOM_RIGHT) {
                xdg_toplevel::ResizeEdge::BottomRight
            } else if edges.contains(WindowEdges::TOP) {
                xdg_toplevel::ResizeEdge::Top
            } else if edges.contains(WindowEdges::BOTTOM) {
                xdg_toplevel::ResizeEdge::Bottom
            } else if edges.contains(WindowEdges::LEFT) {
                xdg_toplevel::ResizeEdge::Left
            } else if edges.contains(WindowEdges::RIGHT) {
                xdg_toplevel::ResizeEdge::Right
            } else {
                xdg_toplevel::ResizeEdge::None
            };
            toplevel.resize(seat, live.state.last_pointer_serial, edge);
            let _ = live.conn.flush();
        }
        #[cfg(not(target_os = "linux"))]
        let _ = edges;
        true
    }

    fn set_opacity(&mut self, opacity: f32) {
        self.opacity = opacity.clamp(0.0, 1.0);
    }

    fn opacity(&self) -> f32 {
        self.opacity
    }

    fn set_minimum_size(&mut self, min_w: i32, min_h: i32) {
        self.size_limits.min = crate::size_limits::clamp_size_limit(min_w, min_h);
        #[cfg(target_os = "linux")]
        if let Some(live) = self.live.as_ref()
            && let LiveRole::Xdg { toplevel, .. } = &live.role
        {
            toplevel.set_min_size(self.size_limits.min.0, self.size_limits.min.1);
            live.wl_surface.commit();
            let _ = live.conn.flush();
        }
    }

    fn minimum_size(&self) -> (i32, i32) {
        self.size_limits.min
    }

    fn set_maximum_size(&mut self, max_w: i32, max_h: i32) {
        self.size_limits.max = crate::size_limits::clamp_size_limit(max_w, max_h);
        #[cfg(target_os = "linux")]
        if let Some(live) = self.live.as_ref()
            && let LiveRole::Xdg { toplevel, .. } = &live.role
        {
            toplevel.set_max_size(self.size_limits.max.0, self.size_limits.max.1);
            live.wl_surface.commit();
            let _ = live.conn.flush();
        }
    }

    fn maximum_size(&self) -> (i32, i32) {
        self.size_limits.max
    }

    fn present(&mut self, pixmap: &mut Pixmap, opacity: f32) -> Result<(), &'static str> {
        let mut surface_guard = ferrokit_core::poison::lock_unpoisoned(&self.surface);
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
        #[cfg(target_os = "linux")]
        if let Some(live) = self.live.as_mut() {
            live.poll_connection(&self.pending_events);
        }
        let events = std::mem::take(&mut *ferrokit_core::poison::lock_unpoisoned(
            &self.pending_events,
        ));
        let count = events.len();
        for event in events {
            self.dispatch_wayland_event(event);
        }
        count
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::screen::{PlatformScreen, WaylandScreen};
    use crate::window_system_interface::ClosureWindowEventHandler;
    use std::sync::atomic::AtomicBool;
    use wayland_protocols::xdg::shell::server::{xdg_surface, xdg_toplevel, xdg_wm_base};
    use wayland_server::protocol::{
        wl_buffer, wl_compositor, wl_output, wl_region, wl_shm, wl_shm_pool, wl_surface,
    };
    use wayland_server::{Client, DataInit, Dispatch, Display, GlobalDispatch, New, Resource};

    struct TestClientData;
    impl wayland_server::backend::ClientData for TestClientData {}

    #[derive(Default)]
    struct ServerState {
        commits: usize,
        shm_buffers_attached: usize,
        min_size: (i32, i32),
        input_region_cleared: bool,
        toplevel: Option<xdg_toplevel::XdgToplevel>,
    }

    impl GlobalDispatch<wl_compositor::WlCompositor, ()> for ServerState {
        fn bind(
            _: &mut Self,
            _: &wayland_server::DisplayHandle,
            _: &Client,
            resource: New<wl_compositor::WlCompositor>,
            _: &(),
            data_init: &mut DataInit<'_, Self>,
        ) {
            data_init.init(resource, ());
        }
    }

    impl Dispatch<wl_compositor::WlCompositor, ()> for ServerState {
        fn request(
            _: &mut Self,
            _: &Client,
            _: &wl_compositor::WlCompositor,
            request: wl_compositor::Request,
            _: &(),
            _: &wayland_server::DisplayHandle,
            data_init: &mut DataInit<'_, Self>,
        ) {
            match request {
                wl_compositor::Request::CreateSurface { id } => {
                    data_init.init(id, ());
                }
                wl_compositor::Request::CreateRegion { id } => {
                    data_init.init(id, ());
                }
                _ => {}
            }
        }
    }

    impl Dispatch<wl_surface::WlSurface, ()> for ServerState {
        fn request(
            state: &mut Self,
            _: &Client,
            _: &wl_surface::WlSurface,
            request: wl_surface::Request,
            _: &(),
            _: &wayland_server::DisplayHandle,
            _: &mut DataInit<'_, Self>,
        ) {
            match request {
                wl_surface::Request::Attach { buffer, .. } if buffer.is_some() => {
                    state.shm_buffers_attached += 1;
                }
                wl_surface::Request::SetInputRegion { region } => {
                    state.input_region_cleared = region.is_some();
                }
                wl_surface::Request::Commit => {
                    state.commits += 1;
                }
                _ => {}
            }
        }
    }

    impl Dispatch<wl_region::WlRegion, ()> for ServerState {
        fn request(
            _: &mut Self,
            _: &Client,
            _: &wl_region::WlRegion,
            _: wl_region::Request,
            _: &(),
            _: &wayland_server::DisplayHandle,
            _: &mut DataInit<'_, Self>,
        ) {
        }
    }

    impl GlobalDispatch<wl_shm::WlShm, ()> for ServerState {
        fn bind(
            _: &mut Self,
            _: &wayland_server::DisplayHandle,
            _: &Client,
            resource: New<wl_shm::WlShm>,
            _: &(),
            data_init: &mut DataInit<'_, Self>,
        ) {
            let shm = data_init.init(resource, ());
            shm.format(wl_shm::Format::Argb8888);
        }
    }

    impl Dispatch<wl_shm::WlShm, ()> for ServerState {
        fn request(
            _: &mut Self,
            _: &Client,
            _: &wl_shm::WlShm,
            request: wl_shm::Request,
            _: &(),
            _: &wayland_server::DisplayHandle,
            data_init: &mut DataInit<'_, Self>,
        ) {
            if let wl_shm::Request::CreatePool { id, .. } = request {
                data_init.init(id, ());
            }
        }
    }

    impl Dispatch<wl_shm_pool::WlShmPool, ()> for ServerState {
        fn request(
            _: &mut Self,
            _: &Client,
            _: &wl_shm_pool::WlShmPool,
            request: wl_shm_pool::Request,
            _: &(),
            _: &wayland_server::DisplayHandle,
            data_init: &mut DataInit<'_, Self>,
        ) {
            if let wl_shm_pool::Request::CreateBuffer { id, .. } = request {
                data_init.init(id, ());
            }
        }
    }

    impl Dispatch<wl_buffer::WlBuffer, ()> for ServerState {
        fn request(
            _: &mut Self,
            _: &Client,
            _: &wl_buffer::WlBuffer,
            _: wl_buffer::Request,
            _: &(),
            _: &wayland_server::DisplayHandle,
            _: &mut DataInit<'_, Self>,
        ) {
        }
    }

    impl GlobalDispatch<xdg_wm_base::XdgWmBase, ()> for ServerState {
        fn bind(
            _: &mut Self,
            _: &wayland_server::DisplayHandle,
            _: &Client,
            resource: New<xdg_wm_base::XdgWmBase>,
            _: &(),
            data_init: &mut DataInit<'_, Self>,
        ) {
            data_init.init(resource, ());
        }
    }

    impl Dispatch<xdg_wm_base::XdgWmBase, ()> for ServerState {
        fn request(
            _: &mut Self,
            _: &Client,
            _: &xdg_wm_base::XdgWmBase,
            request: xdg_wm_base::Request,
            _: &(),
            _: &wayland_server::DisplayHandle,
            data_init: &mut DataInit<'_, Self>,
        ) {
            if let xdg_wm_base::Request::GetXdgSurface { id, .. } = request {
                data_init.init(id, ());
            }
        }
    }

    impl Dispatch<xdg_surface::XdgSurface, ()> for ServerState {
        fn request(
            _: &mut Self,
            _: &Client,
            xdg_surface: &xdg_surface::XdgSurface,
            request: xdg_surface::Request,
            _: &(),
            _: &wayland_server::DisplayHandle,
            data_init: &mut DataInit<'_, Self>,
        ) {
            if let xdg_surface::Request::GetToplevel { id } = request {
                let toplevel = data_init.init(id, ());
                toplevel.configure(640, 480, vec![]);
                xdg_surface.configure(1);
                // Store latest toplevel on server state in outer handler? Handled below via Dispatch<XdgToplevel>
            }
        }
    }

    impl Dispatch<xdg_toplevel::XdgToplevel, ()> for ServerState {
        fn request(
            state: &mut Self,
            _: &Client,
            toplevel: &xdg_toplevel::XdgToplevel,
            request: xdg_toplevel::Request,
            _: &(),
            _: &wayland_server::DisplayHandle,
            _: &mut DataInit<'_, Self>,
        ) {
            state.toplevel = Some(toplevel.clone());
            if let xdg_toplevel::Request::SetMinSize { width, height } = request {
                state.min_size = (width, height);
            }
        }
    }

    impl GlobalDispatch<wl_output::WlOutput, ()> for ServerState {
        fn bind(
            _: &mut Self,
            _: &wayland_server::DisplayHandle,
            _: &Client,
            resource: New<wl_output::WlOutput>,
            _: &(),
            data_init: &mut DataInit<'_, Self>,
        ) {
            let output = data_init.init(resource, ());
            output.geometry(
                0,
                0,
                600,
                340,
                wl_output::Subpixel::Unknown,
                "FerroTest".to_string(),
                "Virtual-1".to_string(),
                wl_output::Transform::Normal,
            );
            output.mode(wl_output::Mode::Current, 2560, 1440, 60_000);
            output.scale(2);
            if output.version() >= 4 {
                output.name("WL-Test-1".to_string());
            }
            output.done();
        }
    }

    impl Dispatch<wl_output::WlOutput, ()> for ServerState {
        fn request(
            _: &mut Self,
            _: &Client,
            _: &wl_output::WlOutput,
            _: wl_output::Request,
            _: &(),
            _: &wayland_server::DisplayHandle,
            _: &mut DataInit<'_, Self>,
        ) {
        }
    }

    #[test]
    fn live_wayland_window_shm_surface_and_screen_roundtrip() {
        let (win_server, win_client) = UnixStream::pair().expect("window socket pair");
        let (scr_server, scr_client) = UnixStream::pair().expect("screen socket pair");

        let stop = Arc::new(AtomicBool::new(false));
        let stop_srv = Arc::clone(&stop);
        let srv_state = Arc::new(Mutex::new(ServerState::default()));
        let srv_state_bg = Arc::clone(&srv_state);

        let handle = std::thread::spawn(move || {
            let mut display: Display<ServerState> = Display::new().expect("display");
            let mut dh = display.handle();
            dh.create_global::<ServerState, wl_compositor::WlCompositor, _>(4, ());
            dh.create_global::<ServerState, wl_shm::WlShm, _>(1, ());
            dh.create_global::<ServerState, xdg_wm_base::XdgWmBase, _>(2, ());
            dh.create_global::<ServerState, wl_output::WlOutput, _>(4, ());
            dh.insert_client(win_server, Arc::new(TestClientData))
                .expect("insert win client");
            dh.insert_client(scr_server, Arc::new(TestClientData))
                .expect("insert scr client");

            while !stop_srv.load(Ordering::Acquire) {
                let mut guard = ferrokit_core::poison::lock_unpoisoned(&srv_state_bg);
                let _ = display.dispatch_clients(&mut *guard);
                let _ = display.flush_clients();
                drop(guard);
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        });

        // 1. Query WaylandScreen from live wl_output
        let screens = WaylandScreen::query_from_socket(scr_client).expect("wl_output screens");
        assert_eq!(screens.len(), 1);
        assert_eq!(screens[0].name(), "WL-Test-1");
        assert_eq!(screens[0].device_pixel_ratio(), 2.0);
        assert_eq!(screens[0].geometry(), Rect::new(0, 0, 1280, 720));

        // 2. Connect live WaylandNativeWindow
        let mut win = WaylandNativeWindow::connect_socket(
            "LiveWaylandTest",
            Rect::new(0, 0, 320, 200),
            WindowFlags::NORMAL,
            "wayland-test".to_string(),
            win_client,
        )
        .expect("live WaylandNativeWindow");
        assert!(win.is_live_compositor());
        assert!(win.connection_fd() >= 0);

        let seen = Arc::new(Mutex::new(Vec::<WindowSystemEvent>::new()));
        let sink = Arc::clone(&seen);
        win.set_event_handler(Box::new(ClosureWindowEventHandler::new(move |ev| {
            ferrokit_core::poison::lock_unpoisoned(&sink).push(ev);
        })));

        // 3. Present SHM buffer + click-through + minimum size
        let mut pixmap = Pixmap::new(320, 200).expect("pixmap");
        pixmap.data_mut().fill(0xff);
        win.present(&mut pixmap, 1.0).expect("present SHM buffer");
        win.set_click_through(true);
        win.set_minimum_size(120, 90);
        win.show();

        // Give server thread a moment to process requests
        std::thread::sleep(std::time::Duration::from_millis(40));
        win.poll_events();

        {
            let guard = ferrokit_core::poison::lock_unpoisoned(&srv_state);
            assert!(guard.commits >= 2);
            assert!(guard.shm_buffers_attached >= 1);
            assert!(guard.input_region_cleared);
            assert_eq!(guard.min_size, (120, 90));
        }

        let events = ferrokit_core::poison::lock_unpoisoned(&seen);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, WindowSystemEvent::Resize { size } if size.width == 640 && size.height == 480)),
            "expected xdg_toplevel.configure resize event, got {events:?}"
        );

        stop.store(true, Ordering::Release);
        let _ = handle.join();
    }
}
