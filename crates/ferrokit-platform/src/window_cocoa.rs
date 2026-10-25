// SPDX-License-Identifier: LGPL-3.0-only
use ferrokit_core::object::ThreadContext;
use ferrokit_gui::geometry::primitives::{Point, Rect};
use ferrokit_gui::geometry::Region;
use ferrokit_gui::paint::Pixmap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use crate::backdrop::BackdropState;
use crate::cursor::{CocoaCursor, CursorShape};
use crate::objc_runtime::appkit;
use crate::objc_runtime::{
    CGFloat, CGRect, CGSize, Class, Id, NSInteger, NSUInteger, ObjcMsg, Sel,
    NS_BACKING_STORE_BUFFERED, NS_FLOATING_WINDOW_LEVEL, NS_MODAL_PANEL_WINDOW_LEVEL,
    NS_NORMAL_WINDOW_LEVEL, NS_SCREEN_SAVER_WINDOW_LEVEL, NS_WINDOW_STYLE_MASK_BORDERLESS,
    NS_WINDOW_STYLE_MASK_CLOSABLE, NS_WINDOW_STYLE_MASK_MINIATURIZABLE,
    NS_WINDOW_STYLE_MASK_RESIZABLE, NS_WINDOW_STYLE_MASK_TITLED,
};
use crate::platform_window::PlatformWindow;
use crate::surface::{CocoaLayerSurface, PlatformSurface};
use crate::window::WindowFlags;
use crate::window_cocoa_input as input;
use crate::window_system_interface::WindowSystemEventHandler;

/// `NSWindowAbove`: the child window is ordered above its parent.
const NS_WINDOW_ABOVE: NSInteger = 1;
/// `NSVisualEffectStateActive` / `NSVisualEffectStateInactive`: an inactive effect view draws
/// its material as a flat fill instead of blurring what is behind the window.
const NS_VISUAL_EFFECT_STATE_ACTIVE: NSInteger = 1;
const NS_VISUAL_EFFECT_STATE_INACTIVE: NSInteger = 2;

/// An input or window event for `CocoaNativeWindow::dispatch_cocoa_event`, in view coordinates:
/// logical pixels with the origin at the top left (the content view is flipped), as Qt reports
/// them. `modifiers` are `Qt::KeyboardModifiers` bits and `key` is a `Qt::Key`.
#[derive(Debug, Clone, PartialEq)]
pub enum CocoaNativeEvent {
    MouseDown {
        x: f64,
        y: f64,
        /// `-[NSEvent buttonNumber]`: 0 left, 1 right, 2 middle, 3 and up extra buttons.
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
    /// The pointer entered the window; reported to the handler as a move to the entry position.
    MouseEntered {
        x: f64,
        y: f64,
        modifiers: u32,
    },
    /// The pointer left the window.
    MouseExited,
    /// `delta_x` and `delta_y` are Qt angle deltas (1/8 degree, 120 per wheel notch).
    ScrollWheel {
        x: f64,
        y: f64,
        delta_x: f64,
        delta_y: f64,
        modifiers: u32,
    },
    KeyDown {
        key: u32,
        modifiers: u32,
        is_repeat: bool,
    },
    KeyUp {
        key: u32,
        modifiers: u32,
    },
    /// Text from the input method: `commit` is final, `preedit` is the composition in progress
    /// with the caret at `cursor` (UTF-16 units).
    TextInput {
        commit: String,
        preedit: String,
        cursor: i32,
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
    surface: CocoaLayerSurface,
    pending_events: Mutex<Vec<CocoaNativeEvent>>,
    opacity: f32,
    size_limits: crate::size_limits::SizeLimits,
    vibrancy_view: Option<Id>,
    vibrancy_material: Option<CocoaVibrancyMaterial>,
    /// The `NSVisualEffectView` added by `set_backdrop`, removed by `set_backdrop(None)`.
    backdrop_view: Option<Id>,
    /// The requested backdrop and whether its blur is suspended (`suspend_backdrop`).
    backdrop_state: BackdropState,
    /// Where the next `present_region` moves the window to (`set_target_pos`).
    target_pos: Option<Point>,
    /// `set_mask` made the window non-opaque (it was opaque), so removing the mask restores it.
    mask_made_transparent: bool,
}

// Deliberately neither `Send` nor `Sync`: AppKit objects (`Id`) may only be messaged from the main
// thread, and the boxed `WindowSystemEventHandler` has no thread-safety bound.

/// Backing-surface extent for a window dimension. Qt accepts empty and negative window sizes,
/// so they get a 1px surface (the buffer cannot be empty) instead of failing after the NSWindow
/// is already allocated.
fn surface_dim(v: i32) -> u32 {
    v.max(1) as u32
}

impl CocoaNativeWindow {
    pub fn new(title: &str, rect: Rect, flags: WindowFlags) -> Result<Self, &'static str> {
        ThreadContext::assert_main_thread("CocoaNativeWindow::new");

        #[cfg(target_os = "macos")]
        let window_class = if flags.contains(WindowFlags::TOOLTIP) {
            // A tooltip never takes the keyboard focus: a plain borderless NSWindow refuses to
            // become key, which is what `Qt::ToolTip` wants.
            Class::get("NSWindow").ok_or("Cannot find NSWindow class")?
        } else {
            // Windows that can take focus need `QNSWindow`: AppKit refuses key status to a
            // plain NSWindow without a title bar.
            appkit::qnswindow_class()
        };
        #[cfg(not(target_os = "macos"))]
        let window_class = Class::get("NSWindow").ok_or("Cannot find NSWindow class")?;
        // Qt's content view is a QNSView, whose -isFlipped returns YES (qnsview_drawing.mm:67-70).
        #[cfg(target_os = "macos")]
        let view_class = crate::objc_runtime::qnsview_class();
        #[cfg(not(target_os = "macos"))]
        let view_class = Class::get("QNSView").ok_or("Cannot find class QNSView")?;

        let style_mask = cocoa_style_mask(flags);

        let ns_window_alloc =
            unsafe { ObjcMsg::send_class_0(window_class, Sel::register("alloc")) };
        if ns_window_alloc.is_nil() {
            return Err("NSWindow allocation failed");
        }

        let cg_rect = native_rect(rect);

        let ns_window = unsafe {
            ObjcMsg::send_window_init(
                ns_window_alloc,
                Sel::register("initWithContentRect:styleMask:backing:defer:"),
                cg_rect,
                style_mask,
                NS_BACKING_STORE_BUFFERED,
                false,
            )
        };

        // `releasedWhenClosed` defaults to YES: a user close would free the window we still own
        // and `Drop` would then message freed memory. Qt: qcocoawindow.mm:2079.
        unsafe { ObjcMsg::send_bool(ns_window, Sel::register("setReleasedWhenClosed:"), false) };
        unsafe { ObjcMsg::send_str(ns_window, Sel::register("setTitle:"), title) };

        let ns_view_alloc = unsafe { ObjcMsg::send_class_0(view_class, Sel::register("alloc")) };
        let view_rect = CGRect::new(0.0, 0.0, rect.width as f64, rect.height as f64);
        let ns_view = unsafe {
            ObjcMsg::send_rect(ns_view_alloc, Sel::register("initWithFrame:"), view_rect)
        };

        unsafe { ObjcMsg::send_id(ns_window, Sel::register("setContentView:"), ns_view) };
        // Key events go to the first responder; the content view is the one that handles them.
        unsafe {
            ObjcMsg::send_id_logged(ns_window, Sel::register("makeFirstResponder:"), ns_view)
        };
        // `mouseMoved:` is delivered to the view's tracking area, but a window that does not
        // accept moved events also drops them for any grab-style handler.
        unsafe {
            ObjcMsg::send_bool_logged(ns_window, Sel::register("setAcceptsMouseMovedEvents:"), true)
        };
        // Qt: QNSView makes itself layer-backed (qnsview_drawing.mm:57) and the backing store
        // flushes into the view's layer (QCocoaWindow::contentLayer, qcocoawindow.mm:2235-2241;
        // qcocoabackingstore.mm:392), not into the view.
        unsafe { ObjcMsg::send_bool(ns_view, Sel::register("setWantsLayer:"), true) };
        let layer = unsafe { ObjcMsg::send_0(ns_view, Sel::register("layer")) };

        unsafe {
            ObjcMsg::send_int(
                ns_window,
                Sel::register("setLevel:"),
                cocoa_window_level(flags),
            )
        };
        let stays_on_top =
            flags.contains(WindowFlags::STAYS_ON_TOP) || flags.contains(WindowFlags::TOOLTIP);

        let click_through = flags.contains(WindowFlags::CLICK_THROUGH);
        if click_through {
            unsafe { ObjcMsg::send_bool(ns_window, Sel::register("setIgnoresMouseEvents:"), true) };
        }

        // Qt: `hidesOnDeactivate` for `Qt::Tool` (qcocoawindow.mm `setWindowFlags`).
        if flags.contains(WindowFlags::TOOL) {
            unsafe {
                ObjcMsg::send_bool_logged(ns_window, Sel::register("setHidesOnDeactivate:"), true)
            };
        }

        let surface = match CocoaLayerSurface::new(
            layer.as_ptr() as usize,
            surface_dim(rect.width),
            surface_dim(rect.height),
        ) {
            Ok(surface) => surface,
            Err(err) => {
                // Nothing owns the window yet: undo the `alloc`/`init` and the view's +1.
                unsafe { ObjcMsg::send_0(ns_view, Sel::register("release")) };
                unsafe { ObjcMsg::send_0(ns_window, Sel::register("release")) };
                return Err(err);
            }
        };

        // From here AppKit may call the view's methods, which find their window by this slot.
        input::register_view(ns_view, ns_window);
        input::update_tracking_area(ns_view);

        Ok(Self {
            ns_window,
            ns_view,
            title: title.to_string(),
            geometry: rect,
            flags,
            stays_on_top,
            click_through,
            visible: AtomicBool::new(false),
            surface,
            pending_events: Mutex::new(Vec::new()),
            opacity: 1.0,
            size_limits: crate::size_limits::SizeLimits::default(),
            vibrancy_view: None,
            vibrancy_material: None,
            backdrop_view: None,
            backdrop_state: BackdropState::new(),
            target_pos: None,
            mask_made_transparent: false,
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
            unsafe { ObjcMsg::send_bool(self.ns_window, Sel::register("setOpaque:"), false) };
            let color_class = Class::get("NSColor").unwrap_or(Class::NIL);
            let clear_color =
                unsafe { ObjcMsg::send_class_0(color_class, Sel::register("clearColor")) };
            unsafe {
                ObjcMsg::send_id(
                    self.ns_window,
                    Sel::register("setBackgroundColor:"),
                    clear_color,
                )
            };

            // 2. Instantiate or configure NSVisualEffectView
            if self.vibrancy_view.is_none() {
                let effect_class = Class::get("NSVisualEffectView").unwrap_or(Class::NIL);
                let effect_alloc =
                    unsafe { ObjcMsg::send_class_0(effect_class, Sel::register("alloc")) };
                let view_rect = CGRect::new(
                    0.0,
                    0.0,
                    self.geometry.width as f64,
                    self.geometry.height as f64,
                );
                let effect_view = unsafe {
                    ObjcMsg::send_rect(effect_alloc, Sel::register("initWithFrame:"), view_rect)
                };
                if !effect_view.is_nil() {
                    // NSVisualEffectBlendingModeBehindWindow = 0
                    unsafe { ObjcMsg::send_int(effect_view, Sel::register("setBlendingMode:"), 0) };
                    // NSVisualEffectStateActive = 1
                    unsafe { ObjcMsg::send_int(effect_view, Sel::register("setState:"), 1) };
                    unsafe {
                        ObjcMsg::send_id(self.ns_view, Sel::register("addSubview:"), effect_view)
                    };
                    self.vibrancy_view = Some(effect_view);
                }
            }

            if let Some(effect_view) = self.vibrancy_view {
                unsafe {
                    ObjcMsg::send_int(effect_view, Sel::register("setMaterial:"), mat as isize)
                };
            }
        } else {
            unsafe { ObjcMsg::send_bool(self.ns_window, Sel::register("setOpaque:"), true) };
            if let Some(effect_view) = self.vibrancy_view.take() {
                unsafe { ObjcMsg::send_0(effect_view, Sel::register("removeFromSuperview")) };
                // Drop our +1 from `alloc`/`init`.
                unsafe { ObjcMsg::send_0(effect_view, Sel::register("release")) };
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

    /// Delivers `event` to the window's event handler, as AppKit's native input does. False when
    /// no handler is installed. Positions are view coordinates; a global position is the
    /// window's `geometry()` origin plus the local one.
    pub fn dispatch_cocoa_event(&mut self, event: CocoaNativeEvent) -> bool {
        if !input::has_handler(self.ns_view) {
            return false;
        }
        if let CocoaNativeEvent::WindowResized { width, height } = &event {
            self.geometry.width = *width as i32;
            self.geometry.height = *height as i32;
            let _ = self
                .surface
                .resize(surface_dim(*width as i32), surface_dim(*height as i32));
        }
        let origin = Point::new(self.geometry.x, self.geometry.y);
        input::dispatch(self.ns_view, event, origin)
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
        unsafe { ObjcMsg::send_bool_return(self.ns_view, Sel::register("isFlipped")) }
    }

    /// The backing surface the window presents into.
    #[inline]
    pub fn surface(&self) -> &CocoaLayerSurface {
        &self.surface
    }

    /// `NSVisualEffectView` state of the applied backdrop: inactive draws the material as a flat
    /// fill of the same colour, which is what a suspended blur is replaced with. False when no
    /// effect view is applied.
    fn set_backdrop_effect_active(&mut self, active: bool) -> bool {
        let Some(view) = self.backdrop_view else {
            return false;
        };
        let state = if active {
            NS_VISUAL_EFFECT_STATE_ACTIVE
        } else {
            NS_VISUAL_EFFECT_STATE_INACTIVE
        };
        unsafe { ObjcMsg::send_int_logged(view, Sel::register("setState:"), state) };
        true
    }

    /// Clips the content layer to `region` (client-area points, origin top left) with a
    /// `CAShapeLayer` mask. A masked-out area is only see-through, and lets clicks through, when
    /// the window itself is not opaque, so an opaque window is made transparent while it has a
    /// mask. An empty region removes the mask.
    fn apply_mask(&mut self, region: &Region) {
        let window = self.ns_window;
        let layer = unsafe { ObjcMsg::send_0(self.ns_view, Sel::register("layer")) };
        if layer.is_nil() {
            return;
        }
        if region.is_empty() {
            unsafe { ObjcMsg::send_id_logged(layer, Sel::register("setMask:"), Id::NIL) };
            self.restore_opaque_after_mask();
            unsafe { ObjcMsg::send_0_logged(window, Sel::register("invalidateShadow")) };
            return;
        }
        let rects: Vec<CGRect> = region
            .rects()
            .iter()
            .map(|r| CGRect::new(r.x as f64, r.y as f64, r.width as f64, r.height as f64))
            .collect();
        // SAFETY: `layer` is the live backing layer of the content view; the shape layer is
        // created (+1) here, and the layer's `setMask:` retains it, so the +1 is released.
        unsafe {
            let bounds = ObjcMsg::send_rect_return(layer, Sel::register("bounds"));
            let flipped = ObjcMsg::send_bool_prop(layer, Sel::register("isGeometryFlipped"));
            let shape = appkit::make_region_mask_layer(&rects, bounds, flipped);
            if shape.is_nil() {
                return;
            }
            ObjcMsg::send_id_logged(layer, Sel::register("setMask:"), shape);
            appkit::release(shape);
        }
        if !self.mask_made_transparent
            && self.vibrancy_material.is_none()
            && self.backdrop_view.is_none()
            && unsafe { ObjcMsg::send_bool_prop(window, Sel::register("isOpaque")) }
        {
            let clear = unsafe {
                ObjcMsg::send_class_0(
                    Class::get("NSColor").unwrap_or(Class::NIL),
                    Sel::register("clearColor"),
                )
            };
            unsafe {
                ObjcMsg::send_bool_logged(window, Sel::register("setOpaque:"), false);
                ObjcMsg::send_id_logged(window, Sel::register("setBackgroundColor:"), clear);
            }
            self.mask_made_transparent = true;
        }
        unsafe { ObjcMsg::send_0_logged(window, Sel::register("invalidateShadow")) };
    }

    /// Undoes what `apply_mask` did to the window's opacity, unless vibrancy or a backdrop
    /// needs the window transparent.
    fn restore_opaque_after_mask(&mut self) {
        if !std::mem::take(&mut self.mask_made_transparent) {
            return;
        }
        if self.vibrancy_material.is_some() || self.backdrop_view.is_some() {
            return;
        }
        let window = self.ns_window;
        let color = unsafe {
            ObjcMsg::send_class_0(
                Class::get("NSColor").unwrap_or(Class::NIL),
                Sel::register("windowBackgroundColor"),
            )
        };
        unsafe {
            ObjcMsg::send_bool_logged(window, Sel::register("setOpaque:"), true);
            ObjcMsg::send_id_logged(window, Sel::register("setBackgroundColor:"), color);
        }
    }

    /// `present_region`'s move: the window takes the position `set_target_pos` gave and the
    /// logical size of the pixmap it is about to show, in one frame change before the contents
    /// are replaced (a layered window such as a popup menu moves with its picture).
    fn move_to_target(&mut self, pos: Point, pixmap: &Pixmap) {
        let target = Rect::new(
            pos.x,
            pos.y,
            pixmap.logical_width().round() as i32,
            pixmap.logical_height().round() as i32,
        );
        if target == self.geometry {
            return;
        }
        self.geometry = target;
        unsafe {
            ObjcMsg::send_rect_display(
                self.ns_window,
                Sel::register("setFrame:display:"),
                native_rect(target),
                false,
            )
        };
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
        unsafe { ObjcMsg::send_id(self.ns_window, Sel::register(order), Id::NIL) };
        self.visible.store(true, Ordering::Release);
    }

    fn is_active(&self) -> bool {
        unsafe { ObjcMsg::send_bool_return(self.ns_window, Sel::register("isKeyWindow")) }
    }

    fn hide(&self) {
        unsafe { ObjcMsg::send_id(self.ns_window, Sel::register("orderOut:"), Id::NIL) };
        self.visible.store(false, Ordering::Release);
    }

    fn geometry(&self) -> Rect {
        self.geometry
    }

    fn set_geometry(&mut self, rect: Rect) {
        self.geometry = rect;
        let cg_rect = native_rect(rect);
        unsafe {
            ObjcMsg::send_rect_display(
                self.ns_window,
                Sel::register("setFrame:display:"),
                cg_rect,
                true,
            )
        };
        let _ = self
            .surface
            .resize(surface_dim(rect.width), surface_dim(rect.height));
    }

    fn set_stays_on_top(&mut self, enabled: bool) {
        // Qt changes the hint through setWindowFlags, which recomputes the level from all
        // flags (qcocoawindow.mm:749).
        self.stays_on_top = enabled;
        self.flags.set(WindowFlags::STAYS_ON_TOP, enabled);
        unsafe {
            ObjcMsg::send_int(
                self.ns_window,
                Sel::register("setLevel:"),
                cocoa_window_level(self.flags),
            )
        };
    }

    fn set_click_through(&mut self, enabled: bool) {
        self.click_through = enabled;
        unsafe {
            ObjcMsg::send_bool(
                self.ns_window,
                Sel::register("setIgnoresMouseEvents:"),
                enabled,
            )
        };
    }

    fn flags(&self) -> WindowFlags {
        self.flags
    }

    /// `QCocoaWindow::setWindowFlags` (qcocoawindow.mm): the style mask follows the frame
    /// flags, the level the stacking flags, `hidesOnDeactivate` follows `Qt::Tool`.
    /// `LAYERED` has no AppKit counterpart and is only remembered.
    fn set_window_flag(&mut self, flag: WindowFlags, on: bool) {
        let old = self.flags;
        self.flags.set(flag, on);
        let changed = old ^ self.flags;
        if changed.is_empty() {
            return;
        }
        if changed.intersects(
            WindowFlags::FRAMELESS | WindowFlags::TOOLTIP | WindowFlags::CUSTOM_FRAMELESS,
        ) {
            unsafe {
                ObjcMsg::send_uint_logged(
                    self.ns_window,
                    Sel::register("setStyleMask:"),
                    cocoa_style_mask(self.flags),
                )
            };
        }
        if changed.intersects(WindowFlags::STAYS_ON_TOP | WindowFlags::TOOL | WindowFlags::TOOLTIP)
        {
            self.stays_on_top = self
                .flags
                .intersects(WindowFlags::STAYS_ON_TOP | WindowFlags::TOOLTIP);
            unsafe {
                ObjcMsg::send_int_logged(
                    self.ns_window,
                    Sel::register("setLevel:"),
                    cocoa_window_level(self.flags),
                )
            };
        }
        if changed.contains(WindowFlags::TOOL) {
            unsafe {
                ObjcMsg::send_bool_logged(
                    self.ns_window,
                    Sel::register("setHidesOnDeactivate:"),
                    self.flags.contains(WindowFlags::TOOL),
                )
            };
        }
        if changed.contains(WindowFlags::CLICK_THROUGH) {
            self.click_through = self.flags.contains(WindowFlags::CLICK_THROUGH);
            unsafe {
                ObjcMsg::send_bool_logged(
                    self.ns_window,
                    Sel::register("setIgnoresMouseEvents:"),
                    self.click_through,
                )
            };
        }
    }

    /// `QCocoaCursor::changeCursor` (qcocoacursor.mm): the window cursor is kept by the content
    /// view, which shows it through a cursor rect and `cursorUpdate:` while the pointer is over
    /// the window; AppKit shows it from the next pointer movement.
    fn set_cursor(&mut self, shape: CursorShape) {
        let selector = CocoaCursor::ns_cursor_selector(shape);
        // SAFETY: the standard cursors are shared, immortal AppKit objects.
        let cursor = unsafe { appkit::standard_cursor(selector) };
        input::replace_cursor(self.ns_view, cursor);
    }

    /// `QWindow::unsetCursor`: no cursor rect, so AppKit shows the arrow (or what the view
    /// under the pointer asks for).
    fn unset_cursor(&mut self) {
        input::replace_cursor(self.ns_view, Id::NIL);
    }

    /// AppKit has no call that routes the whole application's mouse to one window, and
    /// `QCocoaWindow` implements none either (`QPlatformWindow::setMouseGrabEnabled` returns
    /// false there). What a window gets natively is the implicit grab of a pressed button: the
    /// view receives the drag and the release wherever the pointer goes. An explicit grab
    /// therefore holds for a visible window, and is reported by `has_mouse_grab`, but does not
    /// capture a pointer that is over other windows or applications.
    fn set_mouse_grab_enabled(&mut self, grab: bool) -> bool {
        if grab && !self.is_visible() {
            return false;
        }
        input::with_slot(self.ns_view, |slot| slot.mouse_grab = grab).is_some() && grab
    }

    fn has_mouse_grab(&self) -> bool {
        input::with_slot(self.ns_view, |slot| {
            slot.mouse_grab || !slot.pressed.buttons().is_empty()
        })
        .unwrap_or(false)
    }

    fn set_target_pos(&mut self, pos: Option<Point>) {
        self.target_pos = pos;
    }

    fn set_mask(&mut self, region: &Region) {
        self.apply_mask(region);
    }

    /// `-[NSWindow addChildWindow:ordered:]`: the window follows its parent when it moves,
    /// stays above it, and is hidden, miniaturized and ordered out with it (what Qt does for a
    /// popup or tool window with a transient parent). `None` detaches it. The parent is the
    /// `native_handle` of another `CocoaNativeWindow`, which must outlive the attachment or
    /// detach it first, like an owner `HWND`; a window is never its own parent.
    fn set_transient_parent(&mut self, parent: Option<isize>) {
        let me = self.ns_window;
        let wanted = parent
            .filter(|&handle| handle != 0 && handle != me.0 as isize)
            .map(|handle| Id(handle as *mut std::ffi::c_void))
            .unwrap_or(Id::NIL);
        // SAFETY: `me` is alive; `parentWindow` is AppKit's own link, so the current parent
        // is a live window or nil. `wanted` is the caller's live window, per the contract above.
        unsafe {
            let current = ObjcMsg::send_id_prop(me, Sel::register("parentWindow"));
            if current == wanted {
                return;
            }
            if !current.is_nil() {
                ObjcMsg::send_id_logged(current, Sel::register("removeChildWindow:"), me);
            }
            if !wanted.is_nil() {
                ObjcMsg::send_id_int_logged(
                    wanted,
                    Sel::register("addChildWindow:ordered:"),
                    me,
                    NS_WINDOW_ABOVE,
                );
            }
        }
    }

    fn transient_parent(&self) -> Option<isize> {
        // SAFETY: `ns_window` is alive for the lifetime of `self`.
        let parent =
            unsafe { ObjcMsg::send_id_prop(self.ns_window, Sel::register("parentWindow")) };
        (!parent.is_nil()).then_some(parent.0 as isize)
    }

    /// Where the input method shows its candidate window (view coordinates), and a note to the
    /// input context that the caret moved.
    fn set_ime_focus(&mut self, pos: Point) {
        input::with_slot(self.ns_view, |slot| slot.ime_pos = Some(pos));
        // SAFETY: `ns_view` is alive; `inputContext` is nil unless the view conforms to
        // NSTextInputClient, and the context belongs to the view.
        unsafe {
            let context = ObjcMsg::send_id_prop(self.ns_view, Sel::register("inputContext"));
            ObjcMsg::send_0_logged(context, Sel::register("invalidateCharacterCoordinates"));
        }
    }

    fn start_system_drag(&self) {
        let _ = self.start_system_move();
    }

    fn start_system_move(&self) -> bool {
        // Qt: only while the left button alone is pressed; otherwise the move is refused
        // (QCocoaWindow::startSystemMove, qcocoawindow.mm:366-370).
        let ns_event = Id(Class::get("NSEvent").unwrap_or(Class::NIL).0);
        if unsafe { ObjcMsg::send_usize_return(ns_event, Sel::register("pressedMouseButtons")) }
            != 1
        {
            return false;
        }
        let nsapp = unsafe {
            ObjcMsg::send_class_0(
                Class::get("NSApplication").unwrap_or(Class::NIL),
                Sel::register("sharedApplication"),
            )
        };
        if !nsapp.is_nil() {
            let current_event = unsafe { ObjcMsg::send_0(nsapp, Sel::register("currentEvent")) };
            if !current_event.is_nil() {
                unsafe {
                    ObjcMsg::send_id(
                        self.ns_window,
                        Sel::register("performWindowDragWithEvent:"),
                        current_event,
                    )
                };
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
        unsafe {
            ObjcMsg::send_length(
                self.ns_window,
                Sel::register("setAlphaValue:"),
                self.opacity as CGFloat,
            )
        };
    }

    fn opacity(&self) -> f32 {
        self.opacity
    }

    fn set_minimum_size(&mut self, min_w: i32, min_h: i32) {
        // Qt: QCocoaWindow::propagateSizeHints sets window.contentMinSize, the minimum of the
        // content area (qcocoawindow.mm:1181-1185), not the frame's minSize.
        self.size_limits.min = crate::size_limits::clamp_size_limit(min_w, min_h);
        unsafe {
            ObjcMsg::send_size(
                self.ns_window,
                Sel::register("setContentMinSize:"),
                CGSize::new(
                    self.size_limits.min.0 as CGFloat,
                    self.size_limits.min.1 as CGFloat,
                ),
            )
        };
    }

    fn minimum_size(&self) -> (i32, i32) {
        self.size_limits.min
    }

    fn set_maximum_size(&mut self, max_w: i32, max_h: i32) {
        // Qt: the same `propagateSizeHints` sets window.contentMaxSize to `windowMaximumSize()`
        // as is, `QWINDOWSIZE_MAX` included for an axis without a maximum (qcocoawindow.mm:1187-1188).
        self.size_limits.max = crate::size_limits::clamp_size_limit(max_w, max_h);
        unsafe {
            ObjcMsg::send_size(
                self.ns_window,
                Sel::register("setContentMaxSize:"),
                CGSize::new(
                    self.size_limits.max.0 as CGFloat,
                    self.size_limits.max.1 as CGFloat,
                ),
            )
        };
    }

    fn maximum_size(&self) -> (i32, i32) {
        self.size_limits.max
    }

    fn present(&mut self, pixmap: &mut Pixmap, opacity: f32) -> Result<(), &'static str> {
        self.surface.present(pixmap, opacity)
    }

    fn present_dirty(
        &mut self,
        pixmap: &mut Pixmap,
        opacity: f32,
        dirty: Rect,
    ) -> Result<(), &'static str> {
        // The surface copies `dirty` into a buffer of its own size: a pixmap of another size
        // needs the whole frame (which also resizes the surface).
        if pixmap.physical_width() != self.surface.width()
            || pixmap.physical_height() != self.surface.height()
        {
            return self.surface.present(pixmap, opacity);
        }
        self.surface.present_dirty(pixmap, opacity, dirty)
    }

    fn present_region(&mut self, pixmap: &Pixmap, dirty: &Region) -> Result<(), &'static str> {
        if let Some(pos) = self.target_pos.take() {
            self.move_to_target(pos, pixmap);
        }
        self.surface.present_region(pixmap, dirty.rects())
    }

    fn set_event_handler(&mut self, handler: Box<dyn WindowSystemEventHandler>) {
        input::set_handler(self.ns_view, handler);
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
        self.backdrop_state.request(backdrop, dark_mode);
        // The requested material is applied even while suspended: a suspension only turns the
        // effect view's state inactive (below), it never removes the view.
        let applied = crate::backdrop::set_cocoa_window_backdrop(
            self.ns_window,
            self.ns_view,
            backdrop,
            dark_mode,
            &mut self.backdrop_view,
        );
        if self.backdrop_state.is_suspended() {
            self.set_backdrop_effect_active(false);
        }
        applied
    }

    /// Suspends an Acrylic blur by making the effect view inactive, which AppKit draws as a flat
    /// fill of the same material (no blur of what is behind the window) without removing the
    /// view, so there is no flash while the window is dragged or resized. Returns whether the
    /// applied backdrop changed.
    fn suspend_backdrop(&mut self) -> bool {
        let changed = self.backdrop_state.suspend();
        changed && self.set_backdrop_effect_active(false)
    }

    fn resume_backdrop(&mut self) -> bool {
        let changed = self.backdrop_state.resume();
        changed && self.set_backdrop_effect_active(true)
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

impl Drop for CocoaNativeWindow {
    /// Qt's `~QCocoaWindow` takes the window off screen and releases its views and the window.
    fn drop(&mut self) {
        // A child window is attached to its parent, which retains it and may still be alive.
        // SAFETY: `ns_window` is alive until the releases below; `parentWindow` is AppKit's own
        // link, cleared when the parent goes away first.
        unsafe {
            let parent = ObjcMsg::send_id_prop(self.ns_window, Sel::register("parentWindow"));
            if !parent.is_nil() {
                ObjcMsg::send_id_logged(
                    parent,
                    Sel::register("removeChildWindow:"),
                    self.ns_window,
                );
            }
        }
        // From here AppKit callbacks must not find the window: the slot goes first, and takes
        // its tracking area and cursor with it while the view is still alive.
        input::teardown_view(self.ns_view);
        // `release` below is the only release; stop `close` from releasing the window too.
        unsafe {
            ObjcMsg::send_bool(self.ns_window, Sel::register("setReleasedWhenClosed:"), false)
        };
        unsafe { ObjcMsg::send_id(self.ns_window, Sel::register("orderOut:"), Id::NIL) };
        unsafe { ObjcMsg::send_0(self.ns_window, Sel::register("close")) };
        // Each effect view is a subview (retained by `ns_view`) that was also `alloc`ed here.
        for view in [self.vibrancy_view.take(), self.backdrop_view.take()]
            .into_iter()
            .flatten()
        {
            unsafe { ObjcMsg::send_0(view, Sel::register("release")) };
        }
        // `setContentView:` retained `ns_view`; the window releases that when it deallocates.
        unsafe { ObjcMsg::send_0(self.ns_view, Sel::register("release")) };
        unsafe { ObjcMsg::send_0(self.ns_window, Sel::register("release")) };
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

/// `QCocoaWindow::windowStyleMask` (qcocoawindow.mm): a frameless window or a tooltip is
/// borderless, any other window has the title bar, close, miniaturize and resize controls; a
/// custom frame draws its own but still resizes.
pub fn cocoa_style_mask(flags: WindowFlags) -> NSUInteger {
    let mut style_mask =
        if flags.contains(WindowFlags::FRAMELESS) || flags.contains(WindowFlags::TOOLTIP) {
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
    style_mask
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
