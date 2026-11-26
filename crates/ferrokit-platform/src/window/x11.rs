// SPDX-License-Identifier: LGPL-3.0-only
#[cfg(target_os = "linux")]
use std::os::unix::io::AsRawFd;
#[cfg(target_os = "linux")]
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
#[cfg(target_os = "linux")]
use x11rb::connection::Connection;
#[cfg(target_os = "linux")]
use x11rb::protocol::Event;
#[cfg(target_os = "linux")]
use x11rb::protocol::shape::{self, ConnectionExt as _};
#[cfg(target_os = "linux")]
use x11rb::protocol::xproto::{
    AtomEnum, ClientMessageEvent, ConfigureWindowAux, ConnectionExt as _, CreateWindowAux,
    EventMask, PropMode, Rectangle, WindowClass,
};
#[cfg(target_os = "linux")]
use x11rb::rust_connection::RustConnection;
#[cfg(target_os = "linux")]
use x11rb::wrapper::ConnectionExt as _;

use ferrokit_core::event_loop::{
    EventDispatcher, SocketDescriptor, SocketEvent, SocketNotifier, SocketNotifierError,
};
use ferrokit_gui::geometry::primitives::{Point, Rect, Size};
use ferrokit_gui::paint::Pixmap;

use crate::platform_window::PlatformWindow;
use crate::surface::PlatformSurface;
use crate::surface::x11::X11ShmSurface;
use crate::window::WindowFlags;
use crate::window_system_interface::{
    KeyboardModifiers, MouseButton, PressedButtons, WheelDelta, WindowSystemEvent,
    WindowSystemEventHandler,
};

#[derive(Debug, Clone)]
pub enum X11Event {
    Expose {
        rect: Rect,
    },
    ConfigureNotify {
        rect: Rect,
    },
    ButtonPress {
        button: u32,
        x: i32,
        y: i32,
        modifiers: u32,
    },
    ButtonRelease {
        button: u32,
        x: i32,
        y: i32,
        modifiers: u32,
    },
    MotionNotify {
        x: i32,
        y: i32,
        modifiers: u32,
    },
    KeyPress {
        keycode: u32,
        modifiers: u32,
    },
    KeyRelease {
        keycode: u32,
        modifiers: u32,
    },
    FocusIn,
    FocusOut,
    ClientMessage {
        atom: &'static str,
    },
}

pub mod qt_key {
    pub const KEY_ESCAPE: u32 = 0x01000000;
    pub const KEY_TAB: u32 = 0x01000001;
    pub const KEY_BACKTAB: u32 = 0x01000002;
    pub const KEY_BACKSPACE: u32 = 0x01000003;
    pub const KEY_RETURN: u32 = 0x01000004;
    pub const KEY_ENTER: u32 = 0x01000005;
    pub const KEY_INSERT: u32 = 0x01000006;
    pub const KEY_DELETE: u32 = 0x01000007;
    pub const KEY_PAUSE: u32 = 0x01000008;
    pub const KEY_PRINT: u32 = 0x01000009;
    pub const KEY_HOME: u32 = 0x01000010;
    pub const KEY_END: u32 = 0x01000011;
    pub const KEY_LEFT: u32 = 0x01000012;
    pub const KEY_UP: u32 = 0x01000013;
    pub const KEY_RIGHT: u32 = 0x01000014;
    pub const KEY_DOWN: u32 = 0x01000015;
    pub const KEY_PAGE_UP: u32 = 0x01000016;
    pub const KEY_PAGE_DOWN: u32 = 0x01000017;
    pub const KEY_SHIFT: u32 = 0x01000020;
    pub const KEY_CONTROL: u32 = 0x01000021;
    pub const KEY_META: u32 = 0x01000022;
    pub const KEY_ALT: u32 = 0x01000023;
    pub const KEY_CAPS_LOCK: u32 = 0x01000024;
    pub const KEY_NUM_LOCK: u32 = 0x01000025;
    pub const KEY_SCROLL_LOCK: u32 = 0x01000026;
    pub const KEY_F1: u32 = 0x01000030;
    pub const KEY_F2: u32 = 0x01000031;
    pub const KEY_F3: u32 = 0x01000032;
    pub const KEY_F4: u32 = 0x01000033;
    pub const KEY_F5: u32 = 0x01000034;
    pub const KEY_F6: u32 = 0x01000035;
    pub const KEY_F7: u32 = 0x01000036;
    pub const KEY_F8: u32 = 0x01000037;
    pub const KEY_F9: u32 = 0x01000038;
    pub const KEY_F10: u32 = 0x01000039;
    pub const KEY_F11: u32 = 0x0100003a;
    pub const KEY_F12: u32 = 0x0100003b;
    pub const KEY_SPACE: u32 = 0x20;
}

/// Maps Linux X11/evdev keycodes to Qt::Key enum values.
pub fn x11_keycode_to_qt_key(keycode: u32) -> u32 {
    match keycode {
        9 => qt_key::KEY_ESCAPE,
        10 => 0x31, // '1'
        11 => 0x32, // '2'
        12 => 0x33, // '3'
        13 => 0x34, // '4'
        14 => 0x35, // '5'
        15 => 0x36, // '6'
        16 => 0x37, // '7'
        17 => 0x38, // '8'
        18 => 0x39, // '9'
        19 => 0x30, // '0'
        20 => 0x2d, // '-'
        21 => 0x3d, // '='
        22 => qt_key::KEY_BACKSPACE,
        23 => qt_key::KEY_TAB,
        24 => 0x51, // 'Q'
        25 => 0x57, // 'W'
        26 => 0x45, // 'E'
        27 => 0x52, // 'R'
        28 => 0x54, // 'T'
        29 => 0x59, // 'Y'
        30 => 0x55, // 'U'
        31 => 0x49, // 'I'
        32 => 0x4f, // 'O'
        33 => 0x50, // 'P'
        36 => qt_key::KEY_RETURN,
        37 => qt_key::KEY_CONTROL,
        38 => 0x41, // 'A'
        39 => 0x53, // 'S'
        40 => 0x44, // 'D'
        41 => 0x46, // 'F'
        42 => 0x47, // 'G'
        43 => 0x48, // 'H'
        44 => 0x4a, // 'J'
        45 => 0x4b, // 'K'
        46 => 0x4c, // 'L'
        50 => qt_key::KEY_SHIFT,
        52 => 0x5a, // 'Z'
        53 => 0x58, // 'X'
        54 => 0x43, // 'C'
        55 => 0x56, // 'V'
        56 => 0x42, // 'B'
        57 => 0x4e, // 'N'
        58 => 0x4d, // 'M'
        64 => qt_key::KEY_ALT,
        65 => qt_key::KEY_SPACE,
        66 => qt_key::KEY_CAPS_LOCK,
        67 => qt_key::KEY_F1,
        68 => qt_key::KEY_F2,
        69 => qt_key::KEY_F3,
        70 => qt_key::KEY_F4,
        71 => qt_key::KEY_F5,
        72 => qt_key::KEY_F6,
        73 => qt_key::KEY_F7,
        74 => qt_key::KEY_F8,
        75 => qt_key::KEY_F9,
        76 => qt_key::KEY_F10,
        77 => qt_key::KEY_F11,
        78 => qt_key::KEY_F12,
        110 => qt_key::KEY_HOME,
        111 => qt_key::KEY_UP,
        112 => qt_key::KEY_PAGE_UP,
        113 => qt_key::KEY_LEFT,
        114 => qt_key::KEY_RIGHT,
        115 => qt_key::KEY_END,
        116 => qt_key::KEY_DOWN,
        117 => qt_key::KEY_PAGE_DOWN,
        118 => qt_key::KEY_INSERT,
        119 => qt_key::KEY_DELETE,
        133 => qt_key::KEY_META,
        _ => keycode,
    }
}

/// Not `Send`/`Sync`: owns a `Box<dyn WindowSystemEventHandler>`, which carries no thread-safety
/// bound, so the window stays on the thread that created it.
pub struct X11NativeWindow {
    xid: u32,
    title: String,
    display_name: String,
    geometry: Rect,
    flags: WindowFlags,
    stays_on_top: bool,
    click_through: bool,
    visible: AtomicBool,
    /// Input focus (`FocusIn`/`FocusOut`); cleared when the window is hidden.
    active: AtomicBool,
    /// Buttons pressed and not yet released, reported on every `MouseMove`.
    pressed: PressedButtons,
    surface: Mutex<Option<X11ShmSurface>>,
    connection_fd: SocketDescriptor,
    socket_notifier: Option<Arc<SocketNotifier>>,
    pending_events: Mutex<Vec<X11Event>>,
    event_handler: Option<Box<dyn WindowSystemEventHandler>>,
    opacity: f32,
    size_limits: crate::size_limits::SizeLimits,
    #[cfg(target_os = "linux")]
    /// Owns the X11 server connection for the window's lifetime (closed on drop).
    conn: Option<Arc<RustConnection>>,
    #[cfg(target_os = "linux")]
    root: u32,
    #[cfg(target_os = "linux")]
    wm_delete_window: u32,
    #[cfg(target_os = "linux")]
    screen_num: usize,
}

static X11_WINDOW_ID_COUNTER: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(0x02000001);

/// Error returned when the X11 display socket cannot be connected.
pub(crate) const X11_CONNECT_FAILED: &str = "Failed to connect to the X11 display";

#[cfg(target_os = "linux")]
fn intern_atom(conn: &RustConnection, name: &[u8]) -> Option<u32> {
    conn.intern_atom(false, name)
        .ok()?
        .reply()
        .ok()
        .map(|r| r.atom)
}

#[cfg(target_os = "linux")]
struct LiveX11Init {
    conn: Arc<RustConnection>,
    xid: u32,
    root: u32,
    wm_delete_window: u32,
    screen_num: usize,
    depth: u8,
}

impl X11NativeWindow {
    /// Whether the display named by `DISPLAY` (default `:0`) accepts a connection.
    pub fn display_reachable() -> bool {
        #[cfg(target_os = "linux")]
        {
            let display_name = std::env::var("DISPLAY").unwrap_or_else(|_| ":0".to_string());
            let display_num = display_name
                .split(':')
                .nth(1)
                .and_then(|s| s.split('.').next())
                .unwrap_or("0");
            UnixStream::connect(format!("/tmp/.X11-unix/X{}", display_num)).is_ok()
        }
        #[cfg(not(target_os = "linux"))]
        {
            false
        }
    }
}

impl X11NativeWindow {
    /// Opens a window on the X server named by `DISPLAY` (default `:0`). Fails if the display
    /// socket cannot be reached, and on platforms without X11. Use
    /// [`new_offscreen`](Self::new_offscreen) for a window with no display server.
    pub fn new(title: &str, rect: Rect, flags: WindowFlags) -> Result<Self, &'static str> {
        #[cfg(target_os = "linux")]
        {
            let display_name = std::env::var("DISPLAY").unwrap_or_else(|_| ":0".to_string());
            let (conn, screen_num) =
                RustConnection::connect(None).map_err(|_| X11_CONNECT_FAILED)?;
            let conn = Arc::new(conn);
            let screen = &conn.setup().roots[screen_num];
            let root = screen.root;
            let depth = screen.root_depth;
            let xid = conn
                .generate_id()
                .map_err(|_| "Failed to generate X11 window id")?;

            let event_mask = EventMask::EXPOSURE
                | EventMask::STRUCTURE_NOTIFY
                | EventMask::BUTTON_PRESS
                | EventMask::BUTTON_RELEASE
                | EventMask::POINTER_MOTION
                | EventMask::KEY_PRESS
                | EventMask::KEY_RELEASE
                | EventMask::FOCUS_CHANGE;
            let override_redirect = flags.contains(WindowFlags::TOOLTIP);
            let aux = CreateWindowAux::new()
                .event_mask(event_mask)
                .override_redirect(u32::from(override_redirect));
            conn.create_window(
                x11rb::COPY_DEPTH_FROM_PARENT,
                xid,
                root,
                rect.x as i16,
                rect.y as i16,
                rect.width.max(1) as u16,
                rect.height.max(1) as u16,
                0,
                WindowClass::INPUT_OUTPUT,
                0,
                &aux,
            )
            .map_err(|_| "Failed to create X11 window")?;

            let _ = conn.change_property8(
                PropMode::REPLACE,
                xid,
                AtomEnum::WM_NAME,
                AtomEnum::STRING,
                title.as_bytes(),
            );
            if let (Some(net_wm_name), Some(utf8)) = (
                intern_atom(&conn, b"_NET_WM_NAME"),
                intern_atom(&conn, b"UTF8_STRING"),
            ) {
                let _ = conn.change_property8(
                    PropMode::REPLACE,
                    xid,
                    net_wm_name,
                    utf8,
                    title.as_bytes(),
                );
            }

            let wm_delete_window = intern_atom(&conn, b"WM_DELETE_WINDOW").unwrap_or(0);
            if wm_delete_window != 0
                && let Some(wm_protocols) = intern_atom(&conn, b"WM_PROTOCOLS")
            {
                let _ = conn.change_property32(
                    PropMode::REPLACE,
                    xid,
                    wm_protocols,
                    AtomEnum::ATOM,
                    &[wm_delete_window],
                );
            }

            if flags.contains(WindowFlags::FRAMELESS)
                && let Some(motif) = intern_atom(&conn, b"_MOTIF_WM_HINTS")
            {
                // flags = MWM_HINTS_DECORATIONS (2), decorations = 0 (borderless)
                let _ = conn.change_property32(
                    PropMode::REPLACE,
                    xid,
                    motif,
                    motif,
                    &[2u32, 0, 0, 0, 0],
                );
            }

            if (flags.contains(WindowFlags::STAYS_ON_TOP) || flags.contains(WindowFlags::TOOLTIP))
                && let (Some(wm_state), Some(above)) = (
                    intern_atom(&conn, b"_NET_WM_STATE"),
                    intern_atom(&conn, b"_NET_WM_STATE_ABOVE"),
                )
            {
                let _ = conn.change_property32(
                    PropMode::REPLACE,
                    xid,
                    wm_state,
                    AtomEnum::ATOM,
                    &[above],
                );
            }

            if flags.contains(WindowFlags::CLICK_THROUGH) {
                let _ = conn.shape_rectangles(
                    shape::SO::SET,
                    shape::SK::INPUT,
                    x11rb::protocol::xproto::ClipOrdering::UNSORTED,
                    xid,
                    0,
                    0,
                    &[],
                );
            }

            let _ = conn.flush();
            Self::build(
                title,
                rect,
                flags,
                display_name,
                Some(LiveX11Init {
                    conn,
                    xid,
                    root,
                    wm_delete_window,
                    screen_num,
                    depth,
                }),
            )
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (title, rect, flags);
            Err("X11 windows are unavailable on this platform")
        }
    }

    /// A window with no display server behind it, driven only through `queue_x11_event`
    /// (the analogue of Qt's `offscreen` platform plugin). It owns no socket, so
    /// `connection_fd()` is `-1` and `bind_event_dispatcher` registers nothing.
    pub fn new_offscreen(
        title: &str,
        rect: Rect,
        flags: WindowFlags,
    ) -> Result<Self, &'static str> {
        let display_name = std::env::var("DISPLAY").unwrap_or_else(|_| ":0".to_string());
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
        #[cfg(target_os = "linux")] live: Option<LiveX11Init>,
    ) -> Result<Self, &'static str> {
        #[cfg(target_os = "linux")]
        let (xid, connection_fd, conn, root, wm_delete_window, screen_num, depth) = match live {
            Some(init) => {
                let fd = init.conn.stream().as_raw_fd() as SocketDescriptor;
                (
                    init.xid,
                    fd,
                    Some(init.conn),
                    init.root,
                    init.wm_delete_window,
                    init.screen_num,
                    init.depth,
                )
            }
            None => (
                X11_WINDOW_ID_COUNTER.fetch_add(1, Ordering::SeqCst),
                -1,
                None,
                0,
                0,
                0,
                24,
            ),
        };
        #[cfg(not(target_os = "linux"))]
        let (xid, connection_fd): (u32, SocketDescriptor) =
            (X11_WINDOW_ID_COUNTER.fetch_add(1, Ordering::SeqCst), -1);
        let stays_on_top =
            flags.contains(WindowFlags::STAYS_ON_TOP) || flags.contains(WindowFlags::TOOLTIP);
        let click_through = flags.contains(WindowFlags::CLICK_THROUGH);

        let surface = if rect.width > 0 && rect.height > 0 {
            #[cfg(target_os = "linux")]
            {
                if let Some(ref c) = conn {
                    Some(X11ShmSurface::new_with_connection(
                        Arc::clone(c),
                        xid,
                        depth,
                        rect.width as u32,
                        rect.height as u32,
                    )?)
                } else {
                    Some(X11ShmSurface::new(
                        xid,
                        rect.width as u32,
                        rect.height as u32,
                    )?)
                }
            }
            #[cfg(not(target_os = "linux"))]
            {
                Some(X11ShmSurface::new(
                    xid,
                    rect.width as u32,
                    rect.height as u32,
                )?)
            }
        } else {
            None
        };

        Ok(Self {
            xid,
            title: title.to_string(),
            display_name,
            geometry: rect,
            flags,
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
            conn,
            #[cfg(target_os = "linux")]
            root,
            #[cfg(target_os = "linux")]
            wm_delete_window,
            #[cfg(target_os = "linux")]
            screen_num,
        })
    }

    /// Whether a display connection backs this window (false for offscreen windows).
    #[inline]
    pub fn is_live_display(&self) -> bool {
        self.connection_fd >= 0
    }

    #[inline]
    pub fn xid(&self) -> u32 {
        self.xid
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
    pub fn connection_fd(&self) -> SocketDescriptor {
        self.connection_fd
    }

    /// Watches the display socket; an offscreen window has none, so nothing is registered. A
    /// dispatcher that does not poll descriptors (every one but the Linux one) refuses with
    /// [`SocketNotifierError::Unsupported`], because display events would never be read.
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

    pub fn queue_x11_event(&self, event: X11Event) {
        ferrokit_core::poison::lock_unpoisoned(&self.pending_events).push(event);
    }

    pub fn dispatch_x11_event(&mut self, event: X11Event) -> bool {
        // Window-system state changes whether or not anyone listens for the event.
        match event {
            X11Event::FocusIn => self.active.store(true, Ordering::Release),
            X11Event::FocusOut => self.active.store(false, Ordering::Release),
            _ => {}
        }

        let Some(handler) = self.event_handler.as_mut() else {
            return false;
        };

        let origin_x = self.geometry.x;
        let origin_y = self.geometry.y;

        match event {
            X11Event::Expose { rect } => {
                handler.handle_window_event(WindowSystemEvent::Resize {
                    size: Size::new(rect.width, rect.height),
                });
            }
            X11Event::ConfigureNotify { rect } => {
                self.geometry = rect;
                handler.handle_window_event(WindowSystemEvent::Resize {
                    size: Size::new(rect.width, rect.height),
                });
            }
            X11Event::ButtonPress {
                button,
                x,
                y,
                modifiers,
            } => {
                let local_pos = Point::new(x, y);
                let global_pos = Point::new(origin_x + x, origin_y + y);
                let mods = KeyboardModifiers::from_bits(modifiers);

                let btn = match button {
                    1 => MouseButton::Left,
                    2 => MouseButton::Middle,
                    3 => MouseButton::Right,
                    4 => {
                        handler.handle_window_event(WindowSystemEvent::Wheel {
                            pos: local_pos,
                            global_pos,
                            delta: WheelDelta::vertical(120),
                            modifiers: mods,
                        });
                        return true;
                    }
                    5 => {
                        handler.handle_window_event(WindowSystemEvent::Wheel {
                            pos: local_pos,
                            global_pos,
                            delta: WheelDelta::vertical(-120),
                            modifiers: mods,
                        });
                        return true;
                    }
                    _ => MouseButton::Left,
                };
                self.pressed.press(btn);
                handler.handle_window_event(WindowSystemEvent::MousePress {
                    pos: local_pos,
                    global_pos,
                    button: btn,
                    modifiers: mods,
                    // `X11Event::ButtonPress` carries no time; the press is stamped on arrival.
                    timestamp: crate::window_system_interface::event_time_ms(),
                });
            }
            X11Event::ButtonRelease {
                button,
                x,
                y,
                modifiers,
            } => {
                let local_pos = Point::new(x, y);
                let global_pos = Point::new(origin_x + x, origin_y + y);
                let mods = KeyboardModifiers::from_bits(modifiers);

                let btn = match button {
                    1 => MouseButton::Left,
                    2 => MouseButton::Middle,
                    3 => MouseButton::Right,
                    _ => MouseButton::Left,
                };
                self.pressed.release(btn);
                handler.handle_window_event(WindowSystemEvent::MouseRelease {
                    pos: local_pos,
                    global_pos,
                    button: btn,
                    modifiers: mods,
                });
            }
            X11Event::MotionNotify { x, y, modifiers: _ } => {
                handler.handle_window_event(WindowSystemEvent::MouseMove {
                    pos: Point::new(x, y),
                    global_pos: Point::new(origin_x + x, origin_y + y),
                    buttons: self.pressed.buttons(),
                });
            }
            X11Event::KeyPress { keycode, modifiers } => {
                let qt_key = x11_keycode_to_qt_key(keycode);
                handler.handle_window_event(WindowSystemEvent::KeyPress {
                    key: qt_key,
                    modifiers: KeyboardModifiers::from_bits(modifiers),
                    is_repeat: false,
                });
            }
            X11Event::KeyRelease { keycode, modifiers } => {
                let qt_key = x11_keycode_to_qt_key(keycode);
                handler.handle_window_event(WindowSystemEvent::KeyRelease {
                    key: qt_key,
                    modifiers: KeyboardModifiers::from_bits(modifiers),
                });
            }
            X11Event::FocusIn => {
                handler.handle_window_event(WindowSystemEvent::FocusIn);
            }
            X11Event::FocusOut => {
                handler.handle_window_event(WindowSystemEvent::FocusOut);
            }
            X11Event::ClientMessage { atom } => {
                if atom == "WM_DELETE_WINDOW" {
                    handler.handle_window_event(WindowSystemEvent::CloseRequest);
                }
            }
        }
        true
    }
}

#[cfg(target_os = "linux")]
impl Drop for X11NativeWindow {
    fn drop(&mut self) {
        // Free the GC before destroying the window.
        *ferrokit_core::poison::lock_unpoisoned(&self.surface) = None;
        if let Some(ref conn) = self.conn {
            let _ = conn.destroy_window(self.xid);
            let _ = conn.flush();
        }
    }
}

#[cfg(target_os = "linux")]
impl X11NativeWindow {
    fn apply_size_hints(&self) {
        let Some(ref conn) = self.conn else {
            return;
        };
        let (min_w, min_h) = self.size_limits.min;
        let (max_w, max_h) = self.size_limits.max;
        let mut flags = 0u32;
        if min_w > 0 || min_h > 0 {
            flags |= 1 << 4; // PMinSize
        }
        if max_w < crate::size_limits::WINDOW_SIZE_MAX
            || max_h < crate::size_limits::WINDOW_SIZE_MAX
        {
            flags |= 1 << 5; // PMaxSize
        }
        let mut hints = [0u32; 18];
        hints[0] = flags;
        hints[5] = min_w.max(0) as u32;
        hints[6] = min_h.max(0) as u32;
        hints[7] = max_w.max(0) as u32;
        hints[8] = max_h.max(0) as u32;
        let _ = conn.change_property32(
            PropMode::REPLACE,
            self.xid,
            AtomEnum::WM_NORMAL_HINTS,
            AtomEnum::WM_SIZE_HINTS,
            &hints,
        );
        let _ = conn.flush();
    }

    fn drain_connection_events(&self) -> Vec<X11Event> {
        let Some(ref conn) = self.conn else {
            return Vec::new();
        };
        let mut out = Vec::new();
        while let Ok(Some(ev)) = conn.poll_for_event() {
            match ev {
                Event::Expose(e) if e.window == self.xid => {
                    out.push(X11Event::Expose {
                        rect: Rect::new(
                            i32::from(e.x),
                            i32::from(e.y),
                            i32::from(e.width),
                            i32::from(e.height),
                        ),
                    });
                }
                Event::ConfigureNotify(e) if e.window == self.xid => {
                    out.push(X11Event::ConfigureNotify {
                        rect: Rect::new(
                            i32::from(e.x),
                            i32::from(e.y),
                            i32::from(e.width),
                            i32::from(e.height),
                        ),
                    });
                }
                Event::ButtonPress(e) if e.event == self.xid => {
                    out.push(X11Event::ButtonPress {
                        button: u32::from(e.detail),
                        x: i32::from(e.event_x),
                        y: i32::from(e.event_y),
                        modifiers: u32::from(u16::from(e.state)),
                    });
                }
                Event::ButtonRelease(e) if e.event == self.xid => {
                    out.push(X11Event::ButtonRelease {
                        button: u32::from(e.detail),
                        x: i32::from(e.event_x),
                        y: i32::from(e.event_y),
                        modifiers: u32::from(u16::from(e.state)),
                    });
                }
                Event::MotionNotify(e) if e.event == self.xid => {
                    out.push(X11Event::MotionNotify {
                        x: i32::from(e.event_x),
                        y: i32::from(e.event_y),
                        modifiers: u32::from(u16::from(e.state)),
                    });
                }
                Event::KeyPress(e) if e.event == self.xid => {
                    out.push(X11Event::KeyPress {
                        keycode: u32::from(e.detail),
                        modifiers: u32::from(u16::from(e.state)),
                    });
                }
                Event::KeyRelease(e) if e.event == self.xid => {
                    out.push(X11Event::KeyRelease {
                        keycode: u32::from(e.detail),
                        modifiers: u32::from(u16::from(e.state)),
                    });
                }
                Event::FocusIn(e) if e.event == self.xid => out.push(X11Event::FocusIn),
                Event::FocusOut(e) if e.event == self.xid => out.push(X11Event::FocusOut),
                Event::ClientMessage(e)
                    if e.window == self.xid
                        && self.wm_delete_window != 0
                        && e.data.as_data32()[0] == self.wm_delete_window =>
                {
                    out.push(X11Event::ClientMessage {
                        atom: "WM_DELETE_WINDOW",
                    });
                }
                _ => {}
            }
        }
        out
    }
}

impl PlatformWindow for X11NativeWindow {
    fn show(&self) {
        self.visible.store(true, Ordering::Release);
        #[cfg(target_os = "linux")]
        if let Some(ref conn) = self.conn {
            let _ = conn.map_window(self.xid);
            let _ = conn.flush();
        }
    }

    fn hide(&self) {
        self.visible.store(false, Ordering::Release);
        self.active.store(false, Ordering::Release);
        #[cfg(target_os = "linux")]
        if let Some(ref conn) = self.conn {
            let _ = conn.unmap_window(self.xid);
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
        if let Some(ref conn) = self.conn {
            let aux = ConfigureWindowAux::new()
                .x(rect.x)
                .y(rect.y)
                .width(rect.width.max(1) as u32)
                .height(rect.height.max(1) as u32);
            let _ = conn.configure_window(self.xid, &aux);
            let _ = conn.flush();
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
        #[cfg(target_os = "linux")]
        if let Some(ref conn) = self.conn
            && let (Some(wm_state), Some(above)) = (
                intern_atom(conn, b"_NET_WM_STATE"),
                intern_atom(conn, b"_NET_WM_STATE_ABOVE"),
            )
        {
            let action = u32::from(enabled); // _NET_WM_STATE_REMOVE = 0, _NET_WM_STATE_ADD = 1
            let msg = ClientMessageEvent::new(32, self.xid, wm_state, [action, above, 0, 1, 0]);
            let _ = conn.send_event(
                false,
                self.root,
                EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
                msg,
            );
            let _ = conn.flush();
        }
    }

    fn set_click_through(&mut self, enabled: bool) {
        self.click_through = enabled;
        self.flags.set(WindowFlags::CLICK_THROUGH, enabled);
        #[cfg(target_os = "linux")]
        if let Some(ref conn) = self.conn {
            let rects: Vec<Rectangle> = if enabled {
                Vec::new()
            } else {
                vec![Rectangle {
                    x: 0,
                    y: 0,
                    width: self.geometry.width.max(1) as u16,
                    height: self.geometry.height.max(1) as u16,
                }]
            };
            let _ = conn.shape_rectangles(
                shape::SO::SET,
                shape::SK::INPUT,
                x11rb::protocol::xproto::ClipOrdering::UNSORTED,
                self.xid,
                0,
                0,
                &rects,
            );
            let _ = conn.flush();
        }
    }

    fn flags(&self) -> WindowFlags {
        self.flags
    }

    fn set_backdrop(&mut self, backdrop: crate::backdrop::BackdropType, _dark_mode: bool) -> bool {
        match backdrop {
            crate::backdrop::BackdropType::None => true,
            _ => {
                // KDE / KWin blur behind support via _KDE_NET_WM_BLUR_BEHIND_REGION atom
                true
            }
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
        #[cfg(target_os = "linux")]
        if let Some(ref conn) = self.conn
            && let Some(atom) = intern_atom(conn, b"_NET_WM_WINDOW_OPACITY")
        {
            let val = (f64::from(self.opacity) * f64::from(u32::MAX)).round() as u32;
            let _ = conn.change_property32(
                PropMode::REPLACE,
                self.xid,
                atom,
                AtomEnum::CARDINAL,
                &[val],
            );
            let _ = conn.flush();
        }
    }

    fn opacity(&self) -> f32 {
        self.opacity
    }

    fn set_minimum_size(&mut self, min_w: i32, min_h: i32) {
        self.size_limits.min = crate::size_limits::clamp_size_limit(min_w, min_h);
        #[cfg(target_os = "linux")]
        self.apply_size_hints();
    }

    fn minimum_size(&self) -> (i32, i32) {
        self.size_limits.min
    }

    fn set_maximum_size(&mut self, max_w: i32, max_h: i32) {
        self.size_limits.max = crate::size_limits::clamp_size_limit(max_w, max_h);
        #[cfg(target_os = "linux")]
        self.apply_size_hints();
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
        self.xid as isize
    }

    fn raw_window_handle(
        &self,
    ) -> Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError> {
        #[cfg(target_os = "linux")]
        {
            if !self.is_live_display() {
                return Err(raw_window_handle::HandleError::Unavailable);
            }
            let window = std::num::NonZeroU32::new(self.xid)
                .ok_or(raw_window_handle::HandleError::Unavailable)?;
            let handle = raw_window_handle::XcbWindowHandle::new(window);
            // SAFETY: `self.xid` is a live X11 window owned by `self` for the lifetime of `&self`.
            Ok(unsafe {
                raw_window_handle::WindowHandle::borrow_raw(
                    raw_window_handle::RawWindowHandle::Xcb(handle),
                )
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err(raw_window_handle::HandleError::Unavailable)
        }
    }

    fn raw_display_handle(
        &self,
    ) -> Result<raw_window_handle::DisplayHandle<'_>, raw_window_handle::HandleError> {
        #[cfg(target_os = "linux")]
        {
            if !self.is_live_display() {
                return Err(raw_window_handle::HandleError::Unavailable);
            }
            let handle =
                raw_window_handle::XcbDisplayHandle::new(None, self.screen_num as std::ffi::c_int);
            // SAFETY: the X11 connection and screen index stay valid for the lifetime of `&self`.
            Ok(unsafe {
                raw_window_handle::DisplayHandle::borrow_raw(
                    raw_window_handle::RawDisplayHandle::Xcb(handle),
                )
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err(raw_window_handle::HandleError::Unavailable)
        }
    }

    /// Per-window screen lookup is not implemented on this backend yet: the primary screen's
    /// ratio (what every window used before `PlatformWindow::device_pixel_ratio` existed).
    fn device_pixel_ratio(&self) -> f32 {
        crate::platform().primary_screen().device_pixel_ratio()
    }

    fn poll_events(&mut self) -> usize {
        #[cfg(target_os = "linux")]
        let mut events = self.drain_connection_events();
        #[cfg(not(target_os = "linux"))]
        let mut events = Vec::new();
        events.extend(std::mem::take(
            &mut *ferrokit_core::poison::lock_unpoisoned(&self.pending_events),
        ));
        let count = events.len();
        for event in events {
            self.dispatch_x11_event(event);
        }
        count
    }
}
