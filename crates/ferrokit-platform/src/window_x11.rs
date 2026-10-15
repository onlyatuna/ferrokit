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
use crate::surface::x11::X11ShmSurface;
use crate::surface::PlatformSurface;
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
    stream: Option<UnixStream>,
}

unsafe impl Send for X11NativeWindow {}
unsafe impl Sync for X11NativeWindow {}

static X11_WINDOW_ID_COUNTER: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(0x02000001);

impl X11NativeWindow {
    pub fn new(title: &str, rect: Rect, flags: WindowFlags) -> Result<Self, &'static str> {
        let display_name = std::env::var("DISPLAY").unwrap_or_else(|_| ":0".to_string());
        let xid = X11_WINDOW_ID_COUNTER.fetch_add(1, Ordering::SeqCst);

        let display_num = display_name
            .split(':')
            .nth(1)
            .and_then(|s| s.split('.').next())
            .unwrap_or("0");
        let _socket_path = format!("/tmp/.X11-unix/X{}", display_num);

        #[cfg(target_os = "linux")]
        let (real_stream, connection_fd) = match UnixStream::connect(&_socket_path) {
            Ok(s) => {
                let _ = s.set_nonblocking(true);
                let fd = s.as_raw_fd() as SocketDescriptor;
                (Some(s), fd)
            }
            Err(_) => {
                let fd = (xid % 1000 + 10) as SocketDescriptor;
                (None, fd)
            }
        };

        #[cfg(not(target_os = "linux"))]
        let connection_fd = (xid % 1000 + 10) as SocketDescriptor;
        let stays_on_top =
            flags.contains(WindowFlags::STAYS_ON_TOP) || flags.contains(WindowFlags::TOOLTIP);
        let click_through = flags.contains(WindowFlags::CLICK_THROUGH);

        let surface = if rect.width > 0 && rect.height > 0 {
            Some(X11ShmSurface::new(
                xid,
                rect.width as u32,
                rect.height as u32,
            )?)
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
            stream: real_stream,
        })
    }

    #[inline]
    pub fn is_live_display(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            self.stream.is_some()
        }
        #[cfg(not(target_os = "linux"))]
        {
            false
        }
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

    pub fn bind_event_dispatcher(&mut self, dispatcher: &mut dyn EventDispatcher) {
        let notifier = Arc::new(SocketNotifier::new(self.connection_fd, SocketEvent::Read));
        dispatcher.register_socket_notifier(&notifier);
        self.socket_notifier = Some(notifier);
    }

    pub fn queue_x11_event(&self, event: X11Event) {
        self.pending_events.lock().unwrap().push(event);
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

impl PlatformWindow for X11NativeWindow {
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
    }

    fn set_click_through(&mut self, enabled: bool) {
        self.click_through = enabled;
        // In X11, setting an empty XCB_SHAPE_SK_INPUT region makes mouse events pass through
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
    }

    fn opacity(&self) -> f32 {
        self.opacity
    }

    /// Stored and reported only: this backend does not pass size hints to the window system
    /// (`WM_NORMAL_HINTS`), so a minimum or maximum is not enforced on X11.
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
        self.xid as isize
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
            self.dispatch_x11_event(event);
        }
        count
    }
}
