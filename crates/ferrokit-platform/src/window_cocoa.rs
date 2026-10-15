// SPDX-License-Identifier: LGPL-3.0-only
use ferrokit_core::object::ThreadContext;
use ferrokit_gui::geometry::primitives::{Point, Rect, Size};
use ferrokit_gui::paint::Pixmap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use crate::objc_runtime::{
    CGFloat, CGRect, CGSize, Class, Id, ObjcMsg, Sel, NS_BACKING_STORE_BUFFERED,
    NS_FLOATING_WINDOW_LEVEL, NS_MODAL_PANEL_WINDOW_LEVEL, NS_NORMAL_WINDOW_LEVEL,
    NS_SCREEN_SAVER_WINDOW_LEVEL, NS_WINDOW_STYLE_MASK_BORDERLESS, NS_WINDOW_STYLE_MASK_CLOSABLE,
    NS_WINDOW_STYLE_MASK_MINIATURIZABLE, NS_WINDOW_STYLE_MASK_RESIZABLE,
    NS_WINDOW_STYLE_MASK_TITLED,
};
use crate::platform_window::PlatformWindow;
use crate::surface::{CocoaLayerSurface, PlatformSurface};
use crate::window::WindowFlags;
use crate::window_system_interface::{
    KeyboardModifiers, MouseButton, PressedButtons, WheelDelta, WindowSystemEvent,
    WindowSystemEventHandler,
};

/// An input or window event for `CocoaNativeWindow::dispatch_cocoa_event`, in view coordinates.
#[derive(Debug, Clone, PartialEq)]
pub enum CocoaNativeEvent {
    MouseDown {
        x: f64,
        y: f64,
        button: u16,
        modifiers: u32,
    },
    MouseUp {
        x: f64,
        y: f64,
        button: u16,
        modifiers: u32,
    },
    MouseMoved {
        x: f64,
        y: f64,
        modifiers: u32,
    },
    ScrollWheel {
        x: f64,
        y: f64,
        delta_x: f64,
        delta_y: f64,
    },
    KeyDown {
        key_code: u16,
        modifiers: u32,
        is_repeat: bool,
    },
    KeyUp {
        key_code: u16,
        modifiers: u32,
    },
    WindowResized {
        width: f64,
        height: f64,
    },
    WindowCloseRequested,
}

/// Visual effect materials for macOS translucent vibrancy backgrounds (`NSVisualEffectView`).
#[repr(i64)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CocoaVibrancyMaterial {
    Titlebar = 3,
    Selection = 4,
    Menu = 5,
    Popover = 6,
    Sidebar = 7,
    HeaderView = 10,
    Sheet = 11,
    WindowBackground = 12,
    HudWindow = 13,
    FullScreenUI = 15,
    ToolTip = 17,
    ContentBackground = 18,
    UnderWindowBackground = 21,
    UnderPageBackground = 22,
}
pub struct CocoaNativeWindow {
    ns_window: Id,
    ns_view: Id,
    title: String,
    geometry: Rect,
    flags: WindowFlags,
    stays_on_top: bool,
    click_through: bool,
    visible: AtomicBool,
    /// Buttons pressed and not yet released, reported on every `MouseMove`.
    pressed: PressedButtons,
    surface: CocoaLayerSurface,
    pending_events: Mutex<Vec<CocoaNativeEvent>>,
    event_handler: Option<Box<dyn WindowSystemEventHandler>>,
    opacity: f32,
    size_limits: crate::size_limits::SizeLimits,
    vibrancy_view: Option<Id>,
    vibrancy_material: Option<CocoaVibrancyMaterial>,
    /// The `NSVisualEffectView` added by `set_backdrop`, removed by `set_backdrop(None)`.
    backdrop_view: Option<Id>,
}

unsafe impl Send for CocoaNativeWindow {}
unsafe impl Sync for CocoaNativeWindow {}

impl CocoaNativeWindow {
    pub fn new(title: &str, rect: Rect, flags: WindowFlags) -> Result<Self, &'static str> {
        ThreadContext::assert_main_thread("CocoaNativeWindow::new");

        let window_class = Class::get("NSWindow").ok_or("Cannot find NSWindow class")?;
        // Qt's content view is a QNSView, whose -isFlipped returns YES (qnsview_drawing.mm:67-70).
        #[cfg(target_os = "macos")]
        let view_class = crate::objc_runtime::qnsview_class();
        #[cfg(not(target_os = "macos"))]
        let view_class = Class::get("QNSView").ok_or("Cannot find class QNSView")?;

        let mut style_mask = if flags.contains(WindowFlags::FRAMELESS)
            || flags.contains(WindowFlags::TOOLTIP)
        {
            NS_WINDOW_STYLE_MASK_BORDERLESS
        } else {
            NS_WINDOW_STYLE_MASK_TITLED
                | NS_WINDOW_STYLE_MASK_CLOSABLE
                | NS_WINDOW_STYLE_MASK_MINIATURIZABLE
                | NS_WINDOW_STYLE_MASK_RESIZABLE
        };

        if flags.contains(WindowFlags::CUSTOM_FRAMELESS) {
            style_mask |= NS_WINDOW_STYLE_MASK_RESIZABLE;
        }

        let ns_window_alloc = ObjcMsg::send_class_0(window_class, Sel::register("alloc"));
        if ns_window_alloc.is_nil() {
            return Err("NSWindow allocation failed");
        }

        let cg_rect = native_rect(rect);

        let ns_window = ObjcMsg::send_window_init(
            ns_window_alloc,
            Sel::register("initWithContentRect:styleMask:backing:defer:"),
            cg_rect,
            style_mask,
            NS_BACKING_STORE_BUFFERED,
            false,
        );

        ObjcMsg::send_str(ns_window, Sel::register("setTitle:"), title);

        let ns_view_alloc = ObjcMsg::send_class_0(view_class, Sel::register("alloc"));
        let view_rect = CGRect::new(0.0, 0.0, rect.width as f64, rect.height as f64);
        let ns_view = ObjcMsg::send_window_init(
            ns_view_alloc,
            Sel::register("initWithFrame:"),
            view_rect,
            0,
            0,
            false,
        );

        ObjcMsg::send_id(ns_window, Sel::register("setContentView:"), ns_view);
        // Qt: QNSView makes itself layer-backed (qnsview_drawing.mm:57) and the backing store
        // flushes into the view's layer (QCocoaWindow::contentLayer, qcocoawindow.mm:2235-2241;
        // qcocoabackingstore.mm:392), not into the view.
        ObjcMsg::send_bool(ns_view, Sel::register("setWantsLayer:"), true);
        let layer = ObjcMsg::send_0(ns_view, Sel::register("layer"));

        ObjcMsg::send_int(
            ns_window,
            Sel::register("setLevel:"),
            cocoa_window_level(flags),
        );
        let stays_on_top =
            flags.contains(WindowFlags::STAYS_ON_TOP) || flags.contains(WindowFlags::TOOLTIP);

        let click_through = flags.contains(WindowFlags::CLICK_THROUGH);
        if click_through {
            ObjcMsg::send_bool(ns_window, Sel::register("setIgnoresMouseEvents:"), true);
        }

        let surface = CocoaLayerSurface::new(
            layer.as_ptr() as usize,
            rect.width as u32,
            rect.height as u32,
        )?;

        Ok(Self {
            ns_window,
            ns_view,
            title: title.to_string(),
            geometry: rect,
            flags,
            stays_on_top,
            click_through,
            visible: AtomicBool::new(false),
            pressed: PressedButtons::default(),
            surface,
            pending_events: Mutex::new(Vec::new()),
            event_handler: None,
            opacity: 1.0,
            size_limits: crate::size_limits::SizeLimits::default(),
            vibrancy_view: None,
            vibrancy_material: None,
            backdrop_view: None,
        })
    }

    #[inline]
    pub fn ns_window(&self) -> Id {
        self.ns_window
    }

    #[inline]
    pub fn ns_view(&self) -> Id {
        self.ns_view
    }
    #[inline]
    pub fn vibrancy_material(&self) -> Option<CocoaVibrancyMaterial> {
        self.vibrancy_material
    }

    /// Configures native macOS vibrancy material (`NSVisualEffectView`).
    pub fn set_vibrancy(&mut self, material: Option<CocoaVibrancyMaterial>) {
        self.vibrancy_material = material;
        if let Some(mat) = material {
            // 1. Make window transparent
            ObjcMsg::send_bool(self.ns_window, Sel::register("setOpaque:"), false);
            let color_class = Class::get("NSColor").unwrap_or(Class::NIL);
            let clear_color = ObjcMsg::send_class_0(color_class, Sel::register("clearColor"));
            ObjcMsg::send_id(self.ns_window, Sel::register("setBackgroundColor:"), clear_color);

            // 2. Instantiate or configure NSVisualEffectView
            if self.vibrancy_view.is_none() {
                let effect_class = Class::get("NSVisualEffectView").unwrap_or(Class::NIL);
                let effect_alloc = ObjcMsg::send_class_0(effect_class, Sel::register("alloc"));
                let view_rect = CGRect::new(
                    0.0,
                    0.0,
                    self.geometry.width as f64,
                    self.geometry.height as f64,
                );
                let effect_view = ObjcMsg::send_window_init(
                    effect_alloc,
                    Sel::register("initWithFrame:"),
                    view_rect,
                    0,
                    0,
                    false,
                );
                if !effect_view.is_nil() {
                    // NSVisualEffectBlendingModeBehindWindow = 0
                    ObjcMsg::send_int(effect_view, Sel::register("setBlendingMode:"), 0);
                    // NSVisualEffectStateActive = 1
                    ObjcMsg::send_int(effect_view, Sel::register("setState:"), 1);
                    ObjcMsg::send_id(self.ns_view, Sel::register("addSubview:"), effect_view);
                    self.vibrancy_view = Some(effect_view);
                }
            }

            if let Some(effect_view) = self.vibrancy_view {
                ObjcMsg::send_int(effect_view, Sel::register("setMaterial:"), mat as isize);
            }
        } else {
            ObjcMsg::send_bool(self.ns_window, Sel::register("setOpaque:"), true);
            if let Some(effect_view) = self.vibrancy_view.take() {
                ObjcMsg::send_0(effect_view, Sel::register("removeFromSuperview"));
            }
        }
    }

    #[inline]
    pub fn title(&self) -> &str {
        &self.title
    }

    #[inline]
    pub fn flags(&self) -> WindowFlags {
        self.flags
    }

    pub fn dispatch_cocoa_event(&mut self, event: CocoaNativeEvent) -> bool {
        let Some(handler) = self.event_handler.as_mut() else {
            return false;
        };

        let origin_x = self.geometry.x;
        let origin_y = self.geometry.y;

        match event {
            CocoaNativeEvent::MouseDown {
                x,
                y,
                button,
                modifiers,
            } => {
                let local_pos = Point::new(x as i32, y as i32);
                let global_pos = Point::new(origin_x + local_pos.x, origin_y + local_pos.y);
                let btn = match button {
                    0 => MouseButton::Left,
                    1 => MouseButton::Right,
                    2 => MouseButton::Middle,
                    _ => MouseButton::Left,
                };
                self.pressed.press(btn);
                handler.handle_window_event(WindowSystemEvent::MousePress {
                    pos: local_pos,
                    global_pos,
                    button: btn,
                    modifiers: KeyboardModifiers::from_bits(modifiers),
                    // `CocoaNativeEvent::MouseDown` carries no time; the press is stamped on arrival.
                    timestamp: crate::window_system_interface::event_time_ms(),
                });
            }
            CocoaNativeEvent::MouseUp {
                x,
                y,
                button,
                modifiers,
            } => {
                let local_pos = Point::new(x as i32, y as i32);
                let global_pos = Point::new(origin_x + local_pos.x, origin_y + local_pos.y);
                let btn = match button {
                    0 => MouseButton::Left,
                    1 => MouseButton::Right,
                    2 => MouseButton::Middle,
                    _ => MouseButton::Left,
                };
                self.pressed.release(btn);
                handler.handle_window_event(WindowSystemEvent::MouseRelease {
                    pos: local_pos,
                    global_pos,
                    button: btn,
                    modifiers: KeyboardModifiers::from_bits(modifiers),
                });
            }
            CocoaNativeEvent::MouseMoved { x, y, modifiers: _ } => {
                let local_pos = Point::new(x as i32, y as i32);
                let global_pos = Point::new(origin_x + local_pos.x, origin_y + local_pos.y);
                handler.handle_window_event(WindowSystemEvent::MouseMove {
                    pos: local_pos,
                    global_pos,
                    buttons: self.pressed.buttons(),
                });
            }
            CocoaNativeEvent::ScrollWheel {
                x,
                y,
                delta_x,
                delta_y,
            } => {
                let local_pos = Point::new(x as i32, y as i32);
                let global_pos = Point::new(origin_x + local_pos.x, origin_y + local_pos.y);
                handler.handle_window_event(WindowSystemEvent::Wheel {
                    pos: local_pos,
                    global_pos,
                    delta: WheelDelta::new(delta_x as i32, delta_y as i32),
                    modifiers: KeyboardModifiers::default(),
                });
            }
            CocoaNativeEvent::KeyDown {
                key_code,
                modifiers,
                is_repeat,
            } => {
                handler.handle_window_event(WindowSystemEvent::KeyPress {
                    key: key_code as u32,
                    modifiers: KeyboardModifiers::from_bits(modifiers),
                    is_repeat,
                });
            }
            CocoaNativeEvent::KeyUp {
                key_code,
                modifiers,
            } => {
                handler.handle_window_event(WindowSystemEvent::KeyRelease {
                    key: key_code as u32,
                    modifiers: KeyboardModifiers::from_bits(modifiers),
                });
            }
            CocoaNativeEvent::WindowResized { width, height } => {
                self.geometry.width = width as i32;
                self.geometry.height = height as i32;
                let _ = self.surface.resize(width as u32, height as u32);
                handler.handle_window_event(WindowSystemEvent::Resize {
                    size: Size::new(width as i32, height as i32),
                });
            }
            CocoaNativeEvent::WindowCloseRequested => {
                handler.handle_window_event(WindowSystemEvent::CloseRequest);
            }
        }
        true
    }

    pub fn queue_cocoa_event(&self, event: CocoaNativeEvent) {
        self.pending_events.lock().unwrap().push(event);
    }

    #[inline]
    pub fn is_visible(&self) -> bool {
        self.visible.load(Ordering::Acquire)
    }

    #[inline]
    pub fn is_flipped(&self) -> bool {
        ObjcMsg::send_bool_return(self.ns_view, Sel::register("isFlipped"))
    }
}

impl PlatformWindow for CocoaNativeWindow {
    fn show(&self) {
        // A tooltip is ordered front without becoming the key window (`Qt::ToolTip` does not
        // take activation); every other window becomes key.
        let order = if self.flags.contains(WindowFlags::TOOLTIP) {
            "orderFront:"
        } else {
            "makeKeyAndOrderFront:"
        };
        ObjcMsg::send_id(self.ns_window, Sel::register(order), Id::NIL);
        self.visible.store(true, Ordering::Release);
    }

    fn is_active(&self) -> bool {
        ObjcMsg::send_bool_return(self.ns_window, Sel::register("isKeyWindow"))
    }

    fn hide(&self) {
        ObjcMsg::send_id(self.ns_window, Sel::register("orderOut:"), Id::NIL);
        self.visible.store(false, Ordering::Release);
    }

    fn geometry(&self) -> Rect {
        self.geometry
    }

    fn set_geometry(&mut self, rect: Rect) {
        self.geometry = rect;
        let cg_rect = native_rect(rect);
        ObjcMsg::send_window_init(
            self.ns_window,
            Sel::register("setFrame:display:"),
            cg_rect,
            0,
            0,
            true,
        );
        let _ = self.surface.resize(rect.width as u32, rect.height as u32);
    }

    fn set_stays_on_top(&mut self, enabled: bool) {
        // Qt changes the hint through setWindowFlags, which recomputes the level from all
        // flags (qcocoawindow.mm:749).
        self.stays_on_top = enabled;
        self.flags.set(WindowFlags::STAYS_ON_TOP, enabled);
        ObjcMsg::send_int(
            self.ns_window,
            Sel::register("setLevel:"),
            cocoa_window_level(self.flags),
        );
    }

    fn set_click_through(&mut self, enabled: bool) {
        self.click_through = enabled;
        ObjcMsg::send_bool(
            self.ns_window,
            Sel::register("setIgnoresMouseEvents:"),
            enabled,
        );
    }

    fn start_system_drag(&self) {
        let _ = self.start_system_move();
    }

    fn start_system_move(&self) -> bool {
        // Qt: only while the left button alone is pressed; otherwise the move is refused
        // (QCocoaWindow::startSystemMove, qcocoawindow.mm:366-370).
        let ns_event = Id(Class::get("NSEvent").unwrap_or(Class::NIL).0);
        if ObjcMsg::send_usize_return(ns_event, Sel::register("pressedMouseButtons")) != 1 {
            return false;
        }
        let nsapp = ObjcMsg::send_class_0(
            Class::get("NSApplication").unwrap_or(Class::NIL),
            Sel::register("sharedApplication"),
        );
        if !nsapp.is_nil() {
            let current_event = ObjcMsg::send_0(nsapp, Sel::register("currentEvent"));
            if !current_event.is_nil() {
                ObjcMsg::send_id(
                    self.ns_window,
                    Sel::register("performWindowDragWithEvent:"),
                    current_event,
                );
            }
        }
        true
    }

    /// `QCocoaWindow` has no system resize; `QPlatformWindow::startSystemResize` returns false
    /// (qplatformwindow.cpp:495-498).
    fn start_system_resize(&self, _edges: crate::platform_window::WindowEdges) -> bool {
        false
    }

    fn set_opacity(&mut self, opacity: f32) {
        self.opacity = opacity.clamp(0.0, 1.0);
        ObjcMsg::send_length(
            self.ns_window,
            Sel::register("setAlphaValue:"),
            self.opacity as CGFloat,
        );
    }

    fn opacity(&self) -> f32 {
        self.opacity
    }

    fn set_minimum_size(&mut self, min_w: i32, min_h: i32) {
        // Qt: QCocoaWindow::propagateSizeHints sets window.contentMinSize, the minimum of the
        // content area (qcocoawindow.mm:1181-1185), not the frame's minSize.
        self.size_limits.min = crate::size_limits::clamp_size_limit(min_w, min_h);
        ObjcMsg::send_size(
            self.ns_window,
            Sel::register("setContentMinSize:"),
            CGSize::new(
                self.size_limits.min.0 as CGFloat,
                self.size_limits.min.1 as CGFloat,
            ),
        );
    }

    fn minimum_size(&self) -> (i32, i32) {
        self.size_limits.min
    }

    fn set_maximum_size(&mut self, max_w: i32, max_h: i32) {
        // Qt: the same `propagateSizeHints` sets window.contentMaxSize to `windowMaximumSize()`
        // as is, `QWINDOWSIZE_MAX` included for an axis without a maximum (qcocoawindow.mm:1187-1188).
        self.size_limits.max = crate::size_limits::clamp_size_limit(max_w, max_h);
        ObjcMsg::send_size(
            self.ns_window,
            Sel::register("setContentMaxSize:"),
            CGSize::new(
                self.size_limits.max.0 as CGFloat,
                self.size_limits.max.1 as CGFloat,
            ),
        );
    }

    fn maximum_size(&self) -> (i32, i32) {
        self.size_limits.max
    }

    fn present(&mut self, pixmap: &mut Pixmap, opacity: f32) -> Result<(), &'static str> {
        self.surface.present(pixmap, opacity)
    }

    fn set_event_handler(&mut self, handler: Box<dyn WindowSystemEventHandler>) {
        self.event_handler = Some(handler);
    }

    fn native_handle(&self) -> isize {
        self.ns_window.0 as isize
    }

    /// Per-window screen lookup is not implemented on this backend yet: the primary screen's
    /// ratio (what every window used before `PlatformWindow::device_pixel_ratio` existed).
    fn device_pixel_ratio(&self) -> f32 {
        crate::platform().primary_screen().device_pixel_ratio()
    }

    fn set_backdrop(&mut self, backdrop: crate::backdrop::BackdropType, dark_mode: bool) -> bool {
        crate::backdrop::set_cocoa_window_backdrop(
            self.ns_window,
            self.ns_view,
            backdrop,
            dark_mode,
            &mut self.backdrop_view,
        )
    }

    fn poll_events(&mut self) -> usize {
        let events = std::mem::take(&mut *self.pending_events.lock().unwrap());
        let count = events.len();
        for event in events {
            self.dispatch_cocoa_event(event);
        }
        count
    }
}

/// `QCocoaWindow::windowLevel` (qcocoawindow.mm:548-564): Tool floats, StaysOnTop goes above
/// Tool windows, ToolTip goes above StaysOnTop windows.
pub fn cocoa_window_level(flags: WindowFlags) -> isize {
    if flags.contains(WindowFlags::TOOLTIP) {
        NS_SCREEN_SAVER_WINDOW_LEVEL
    } else if flags.contains(WindowFlags::STAYS_ON_TOP) {
        NS_MODAL_PANEL_WINDOW_LEVEL
    } else if flags.contains(WindowFlags::TOOL) {
        NS_FLOATING_WINDOW_LEVEL
    } else {
        NS_NORMAL_WINDOW_LEVEL
    }
}

#[inline]
pub fn qt_mac_flip_point(pos: Point, reference_height: i32) -> Point {
    Point::new(pos.x, reference_height - pos.y)
}

#[inline]
pub fn qt_mac_flip_rect(rect: Rect, reference_height: i32) -> Rect {
    Rect::new(
        rect.x,
        reference_height - (rect.y + rect.height),
        rect.width,
        rect.height,
    )
}

/// Height of the primary screen (the one with the menu bar, `[NSScreen screens][0]`), the
/// reference of every Qt top-left <-> Cocoa bottom-left conversion (qcocoascreen.mm:226-227).
#[inline]
pub fn qt_mac_primary_screen_height() -> i32 {
    use crate::screen::PlatformScreen;
    crate::screen::CocoaScreen::screens()[0].geometry().height
}

/// `QCocoaScreen::mapToNative` (qcocoascreen.mm:815-818): Qt's top-left global rect as the
/// bottom-left rect AppKit expects.
fn native_rect(rect: Rect) -> CGRect {
    let flipped = qt_mac_flip_global_rect(rect);
    CGRect::new(
        flipped.x as f64,
        flipped.y as f64,
        flipped.width as f64,
        flipped.height as f64,
    )
}

#[inline]
pub fn qt_mac_flip_global_point(pos: Point) -> Point {
    qt_mac_flip_point(pos, qt_mac_primary_screen_height())
}

#[inline]
pub fn qt_mac_flip_global_rect(rect: Rect) -> Rect {
    qt_mac_flip_rect(rect, qt_mac_primary_screen_height())
}
