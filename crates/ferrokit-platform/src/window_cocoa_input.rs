// SPDX-License-Identifier: LGPL-3.0-only
//! Native input and event delivery of [`CocoaNativeWindow`](crate::window_cocoa::CocoaNativeWindow).
//!
//! AppKit calls the methods of the content view (`QNSView`, see `objc_runtime::appkit`) with
//! `NSEvent`s. Those methods do not know the Rust window: the window struct moves (it is boxed
//! behind `PlatformWindow`), AppKit can call back while the window is borrowed, and the struct
//! owns the handler. So each window registers a [`ViewSlot`] under its view's address in a
//! thread-local table, and everything an AppKit callback needs lives there: the event handler,
//! the pressed buttons, the input-method state, the window cursor. This is the Win32 backend's
//! scheme (a handler registry keyed by `HWND`), including its re-entrancy rule: the handler is
//! taken out of the slot while it runs, and events raised meanwhile are queued behind it.
//!
//! [`handle_ns_event`] converts an `NSEvent` into a [`CocoaNativeEvent`] (logical coordinates,
//! y downward, Qt modifiers and keys) and [`dispatch`] turns that into the
//! [`WindowSystemEvent`] the handler sees. Every Objective-C message goes through `ObjcMsg`, so
//! the same code runs against the mock runtime with a mock `NSEvent` (see
//! `MockObjcRuntime::object_with`).

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};

use ferrokit_core::object::ThreadContext;
use ferrokit_gui::geometry::primitives::{Point, Rect, Size};

use crate::objc_runtime::appkit::{self, NS_NOT_FOUND, NSRange};
use crate::objc_runtime::{CGFloat, CGPoint, CGRect, Id, NSUInteger, ObjcMsg, Sel};
use crate::window_cocoa::{CocoaNativeEvent, qt_mac_primary_screen_height};
use crate::window_system_interface::{
    KeyboardModifiers, MouseButton, PressedButtons, WheelDelta, WindowSystemEvent,
    WindowSystemEventHandler,
};

pub const NS_EVENT_MODIFIER_FLAG_CAPS_LOCK: usize = 1 << 16;
pub const NS_EVENT_MODIFIER_FLAG_SHIFT: usize = 1 << 17;
pub const NS_EVENT_MODIFIER_FLAG_CONTROL: usize = 1 << 18;
pub const NS_EVENT_MODIFIER_FLAG_OPTION: usize = 1 << 19;
pub const NS_EVENT_MODIFIER_FLAG_COMMAND: usize = 1 << 20;

const QT_SHIFT_MODIFIER: u32 = 0x0200_0000;
const QT_CONTROL_MODIFIER: u32 = 0x0400_0000;
const QT_ALT_MODIFIER: u32 = 0x0800_0000;
const QT_META_MODIFIER: u32 = 0x1000_0000;

/// `NSTrackingMouseEnteredAndExited | NSTrackingMouseMoved | NSTrackingCursorUpdate |
/// NSTrackingActiveInActiveApp | NSTrackingInVisibleRect`, the options of `QNSView`'s tracking
/// area (qnsview_mouse.mm `updateTrackingAreas`).
const TRACKING_AREA_OPTIONS: NSUInteger = 0x01 | 0x02 | 0x04 | 0x40 | 0x200;

/// Which `NSResponder` method an `NSEvent` arrived through. The left, right and other mouse
/// buttons share a kind: the event's `buttonNumber` says which button it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NsEventKind {
    MouseDown,
    MouseUp,
    MouseDragged,
    RightMouseDown,
    RightMouseUp,
    RightMouseDragged,
    OtherMouseDown,
    OtherMouseUp,
    OtherMouseDragged,
    MouseMoved,
    MouseEntered,
    MouseExited,
    ScrollWheel,
    KeyDown,
    KeyUp,
    FlagsChanged,
}

/// `Qt::KeyboardModifiers` bits of `NSEvent.modifierFlags`. Like Qt on macOS, Command is
/// `ControlModifier` and Control is `MetaModifier`.
pub fn qt_modifiers_from_ns_flags(flags: usize) -> u32 {
    let mut modifiers = 0;
    if flags & NS_EVENT_MODIFIER_FLAG_SHIFT != 0 {
        modifiers |= QT_SHIFT_MODIFIER;
    }
    if flags & NS_EVENT_MODIFIER_FLAG_COMMAND != 0 {
        modifiers |= QT_CONTROL_MODIFIER;
    }
    if flags & NS_EVENT_MODIFIER_FLAG_OPTION != 0 {
        modifiers |= QT_ALT_MODIFIER;
    }
    if flags & NS_EVENT_MODIFIER_FLAG_CONTROL != 0 {
        modifiers |= QT_META_MODIFIER;
    }
    modifiers
}

// ---------------------------------------------------------------------------
// Keys
// ---------------------------------------------------------------------------

/// `Qt::Key` values (qnamespace.h).
mod qt_key {
    pub const ESCAPE: u32 = 0x0100_0000;
    pub const TAB: u32 = 0x0100_0001;
    pub const BACKTAB: u32 = 0x0100_0002;
    pub const BACKSPACE: u32 = 0x0100_0003;
    pub const RETURN: u32 = 0x0100_0004;
    pub const ENTER: u32 = 0x0100_0005;
    pub const INSERT: u32 = 0x0100_0006;
    pub const DELETE: u32 = 0x0100_0007;
    pub const PAUSE: u32 = 0x0100_0008;
    pub const PRINT: u32 = 0x0100_0009;
    pub const CLEAR: u32 = 0x0100_000b;
    pub const HOME: u32 = 0x0100_0010;
    pub const END: u32 = 0x0100_0011;
    pub const LEFT: u32 = 0x0100_0012;
    pub const UP: u32 = 0x0100_0013;
    pub const RIGHT: u32 = 0x0100_0014;
    pub const DOWN: u32 = 0x0100_0015;
    pub const PAGE_UP: u32 = 0x0100_0016;
    pub const PAGE_DOWN: u32 = 0x0100_0017;
    pub const SHIFT: u32 = 0x0100_0020;
    pub const CONTROL: u32 = 0x0100_0021;
    pub const META: u32 = 0x0100_0022;
    pub const ALT: u32 = 0x0100_0023;
    pub const CAPS_LOCK: u32 = 0x0100_0024;
    pub const SCROLL_LOCK: u32 = 0x0100_0026;
    pub const F1: u32 = 0x0100_0030;
    pub const MENU: u32 = 0x0100_0055;
    pub const HELP: u32 = 0x0100_0058;
    pub const UNKNOWN: u32 = 0x01ff_ffff;
}

/// `Qt::Key` of a macOS virtual key code (`kVK_*`, Events.h) whose key has no text of its own,
/// independent of the keyboard layout. `shift` turns Tab into Backtab, as in Qt.
pub fn special_qt_key(key_code: u16, shift: bool) -> Option<u32> {
    // `kVK_F1`..`kVK_F20`, in function-key order.
    const FUNCTION_KEYS: [u16; 20] = [
        0x7A, 0x78, 0x63, 0x76, 0x60, 0x61, 0x62, 0x64, 0x65, 0x6D, 0x67, 0x6F, 0x69, 0x6B, 0x71,
        0x6A, 0x40, 0x4F, 0x50, 0x5A,
    ];
    if let Some(n) = FUNCTION_KEYS.iter().position(|&code| code == key_code) {
        return Some(qt_key::F1 + n as u32);
    }
    Some(match key_code {
        0x24 => qt_key::RETURN,
        0x4C => qt_key::ENTER,
        0x30 if shift => qt_key::BACKTAB,
        0x30 => qt_key::TAB,
        0x33 => qt_key::BACKSPACE,
        0x75 => qt_key::DELETE,
        0x35 => qt_key::ESCAPE,
        0x72 => qt_key::HELP,
        0x73 => qt_key::HOME,
        0x77 => qt_key::END,
        0x74 => qt_key::PAGE_UP,
        0x79 => qt_key::PAGE_DOWN,
        0x7B => qt_key::LEFT,
        0x7C => qt_key::RIGHT,
        0x7D => qt_key::DOWN,
        0x7E => qt_key::UP,
        0x47 => qt_key::CLEAR,
        0x6E => qt_key::MENU,
        // Qt swaps the roles of Command and Control (see `qt_modifiers_from_ns_flags`).
        0x36 | 0x37 => qt_key::CONTROL,
        0x3B | 0x3E => qt_key::META,
        0x3A | 0x3D => qt_key::ALT,
        0x38 | 0x3C => qt_key::SHIFT,
        0x39 => qt_key::CAPS_LOCK,
        _ => return None,
    })
}

/// `Qt::Key` of a function-key character (`NSUpArrowFunctionKey`, ..., Private Use Area
/// `U+F700..U+F8FF` of `charactersIgnoringModifiers`), for the keys `special_qt_key` does not
/// know by code (the layout-dependent ones are all printable).
fn function_char_qt_key(c: char) -> Option<u32> {
    Some(match c as u32 {
        0xF700 => qt_key::UP,
        0xF701 => qt_key::DOWN,
        0xF702 => qt_key::LEFT,
        0xF703 => qt_key::RIGHT,
        n @ 0xF704..=0xF726 => qt_key::F1 + (n - 0xF704),
        0xF727 => qt_key::INSERT,
        0xF728 => qt_key::DELETE,
        0xF729 => qt_key::HOME,
        0xF72B => qt_key::END,
        0xF72C => qt_key::PAGE_UP,
        0xF72D => qt_key::PAGE_DOWN,
        0xF72E => qt_key::PRINT,
        0xF72F => qt_key::SCROLL_LOCK,
        0xF730 => qt_key::PAUSE,
        0xF735 => qt_key::MENU,
        0xF746 => qt_key::HELP,
        _ => return None,
    })
}

/// The `Qt::Key` of a key event: the key code decides for keys without text; otherwise it is
/// the first character of `charactersIgnoringModifiers` (so shortcuts follow the keyboard
/// layout, as in `QCocoaKeyMapper`), upper-cased like Qt's letter keys and the Win32 virtual-key
/// codes the widgets already understand (`'A'` for the A key).
pub fn qt_key_for_event(
    key_code: u16,
    characters_ignoring_modifiers: Option<&str>,
    shift: bool,
) -> u32 {
    if let Some(key) = special_qt_key(key_code, shift) {
        return key;
    }
    let Some(c) = characters_ignoring_modifiers.and_then(|s| s.chars().next()) else {
        return qt_key::UNKNOWN;
    };
    if let Some(key) = function_char_qt_key(c) {
        return key;
    }
    if ('\u{F700}'..='\u{F8FF}').contains(&c) || c.is_control() {
        return qt_key::UNKNOWN;
    }
    // `to_uppercase` can expand ('ß' -> "SS"); a key is one scalar, so keep the original then.
    let mut upper = c.to_uppercase();
    match (upper.next(), upper.next()) {
        (Some(u), None) => u as u32,
        _ => c as u32,
    }
}

/// For a modifier key's code: the Qt key and the `modifierFlags` bit that says whether it is
/// down after a `flagsChanged:`.
pub fn modifier_key(key_code: u16) -> Option<(u32, usize)> {
    Some(match key_code {
        0x36 | 0x37 => (qt_key::CONTROL, NS_EVENT_MODIFIER_FLAG_COMMAND),
        0x3B | 0x3E => (qt_key::META, NS_EVENT_MODIFIER_FLAG_CONTROL),
        0x3A | 0x3D => (qt_key::ALT, NS_EVENT_MODIFIER_FLAG_OPTION),
        0x38 | 0x3C => (qt_key::SHIFT, NS_EVENT_MODIFIER_FLAG_SHIFT),
        0x39 => (qt_key::CAPS_LOCK, NS_EVENT_MODIFIER_FLAG_CAPS_LOCK),
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Per-view state and event delivery
// ---------------------------------------------------------------------------

/// Everything an AppKit callback needs about one window, keyed by its content view.
pub(crate) struct ViewSlot {
    handler: Option<Box<dyn WindowSystemEventHandler>>,
    /// The handler is out of the slot, running; events that arrive meanwhile wait in `queued`.
    delivering: bool,
    queued: VecDeque<WindowSystemEvent>,
    /// Buttons pressed and not yet released, reported on every move.
    pub(crate) pressed: PressedButtons,
    /// A Control-click was sent as a right button press; its release is a right release too
    /// (Qt `m_sendUpAsRightButton`).
    send_up_as_right: bool,
    /// The input method's uncommitted text (`NSTextInputClient` marked text).
    marked_text: Option<String>,
    /// Cleared by the input-method methods when they consumed the key press being interpreted.
    send_key_event: bool,
    /// Key codes whose press the input method consumed and whose release is still to come.
    suppressed_keys: Vec<u16>,
    /// Where the input method puts its candidate window, in view coordinates.
    pub(crate) ime_pos: Option<Point>,
    /// The window cursor (a retained `NSCursor`), nil for the default one.
    pub(crate) cursor: Id,
    /// An explicit `set_mouse_grab_enabled(true)` is in effect.
    pub(crate) mouse_grab: bool,
    /// The window of the view; valid while the slot exists.
    pub(crate) ns_window: Id,
    /// The `NSTrackingArea` owned (+1) by the slot, nil before the first update.
    tracking_area: Id,
}

thread_local! {
    static VIEWS: RefCell<HashMap<usize, ViewSlot>> = RefCell::new(HashMap::new());
}

/// Registers `view` (the content view of `window`) so AppKit callbacks reach its window.
pub(crate) fn register_view(view: Id, window: Id) {
    let slot = ViewSlot {
        handler: None,
        delivering: false,
        queued: VecDeque::new(),
        pressed: PressedButtons::default(),
        send_up_as_right: false,
        marked_text: None,
        send_key_event: true,
        suppressed_keys: Vec::new(),
        ime_pos: None,
        cursor: Id::NIL,
        mouse_grab: false,
        ns_window: window,
        tracking_area: Id::NIL,
    };
    let _ = VIEWS.try_with(|views| views.borrow_mut().insert(view.0 as usize, slot));
}

/// Forgets `view` and releases what the slot owns: the tracking area (removed from the view
/// first) and the window cursor. The handler is dropped last, after every message is sent.
pub(crate) fn teardown_view(view: Id) {
    let slot = VIEWS
        .try_with(|views| views.borrow_mut().remove(&(view.0 as usize)))
        .ok()
        .flatten();
    let Some(slot) = slot else {
        return;
    };
    if !slot.tracking_area.is_nil() {
        // SAFETY: `view` is live: the only caller is `CocoaNativeWindow::drop`, which calls this
        // before it releases the view. The area was added to this very view by
        // `update_tracking_area` and the slot owns one reference of it, which the `release`
        // here balances once (the slot was removed from the table above, so no second release
        // can happen). `removeTrackingArea:` takes one NSTrackingArea.
        unsafe {
            ObjcMsg::send_id_logged(
                view,
                Sel::register("removeTrackingArea:"),
                slot.tracking_area,
            );
            appkit::release(slot.tracking_area);
        }
    }
    // SAFETY: `slot.cursor` is nil (a no-op) or the cursor `replace_cursor` retained for this
    // slot; the slot was removed from the table above, so this is the one release of that +1.
    unsafe { appkit::release(slot.cursor) };
    drop(slot);
}

/// Runs `f` on the slot of `view`, if there is one. `f` must not send Objective-C messages:
/// AppKit may call back into the slot table while they run.
pub(crate) fn with_slot<R>(view: Id, f: impl FnOnce(&mut ViewSlot) -> R) -> Option<R> {
    VIEWS
        .try_with(|views| views.borrow_mut().get_mut(&(view.0 as usize)).map(f))
        .ok()
        .flatten()
}

/// Installs `handler` as the receiver of the window's events; the previous handler is dropped.
pub(crate) fn set_handler(view: Id, handler: Box<dyn WindowSystemEventHandler>) {
    let previous = with_slot(view, |slot| slot.handler.replace(handler)).flatten();
    drop(previous);
}

/// Whether events for `view` have somewhere to go.
pub(crate) fn has_handler(view: Id) -> bool {
    with_slot(view, |slot| slot.handler.is_some() || slot.delivering).unwrap_or(false)
}

/// Puts the handler back, also when it panicked, unless it was replaced meanwhile.
struct Delivery {
    key: usize,
    handler: Option<Box<dyn WindowSystemEventHandler>>,
}

impl Drop for Delivery {
    fn drop(&mut self) {
        let Some(handler) = self.handler.take() else {
            return;
        };
        let key = self.key;
        let leftover = VIEWS
            .try_with(|views| match views.borrow_mut().get_mut(&key) {
                Some(slot) => {
                    slot.delivering = false;
                    if slot.handler.is_none() {
                        slot.handler = Some(handler);
                        None
                    } else {
                        // Replaced while it ran.
                        Some(handler)
                    }
                }
                // The window went away while its handler ran.
                None => Some(handler),
            })
            .unwrap_or(None);
        drop(leftover);
    }
}

/// Delivers `event` to the handler of `view`: false when the view has no slot or handler.
/// `origin` is the global (top-left) position of the view's origin, which makes a local
/// position global.
///
/// Called while the handler runs (a handler that creates a window, pumps events, ...), the event
/// waits and is delivered right after the running call, never nested.
pub(crate) fn dispatch(view: Id, event: CocoaNativeEvent, origin: Point) -> bool {
    enum Next {
        Ignore,
        Queued,
        Run(Box<dyn WindowSystemEventHandler>, WindowSystemEvent),
    }
    let key = view.0 as usize;
    let next = VIEWS
        .try_with(|views| {
            let mut views = views.borrow_mut();
            let Some(slot) = views.get_mut(&key) else {
                return Next::Ignore;
            };
            if slot.delivering {
                let event = translate(slot, event, origin);
                slot.queued.push_back(event);
                return Next::Queued;
            }
            let Some(handler) = slot.handler.take() else {
                return Next::Ignore;
            };
            slot.delivering = true;
            let event = translate(slot, event, origin);
            Next::Run(handler, event)
        })
        .unwrap_or(Next::Ignore);
    match next {
        Next::Ignore => false,
        Next::Queued => true,
        Next::Run(handler, first) => {
            let mut delivery = Delivery {
                key,
                handler: Some(handler),
            };
            let mut event = Some(first);
            while let Some(current) = event.take() {
                if let Some(handler) = delivery.handler.as_mut() {
                    handler.handle_window_event(current);
                }
                event = VIEWS
                    .try_with(|views| {
                        views
                            .borrow_mut()
                            .get_mut(&key)
                            .and_then(|slot| slot.queued.pop_front())
                    })
                    .ok()
                    .flatten();
            }
            true
        }
    }
}

fn mouse_button(button: u16) -> MouseButton {
    match button {
        0 => MouseButton::Left,
        1 => MouseButton::Right,
        2 => MouseButton::Middle,
        other => MouseButton::Other(other),
    }
}

/// Qt's `QPoint` of a view position: the integer part toward negative infinity, so a drag past
/// the top or left edge reports -1, not 0.
fn local_point(x: f64, y: f64) -> Point {
    Point::new(x.floor() as i32, y.floor() as i32)
}

/// The window event of `event`, keeping the slot's pressed-button state up to date.
fn translate(slot: &mut ViewSlot, event: CocoaNativeEvent, origin: Point) -> WindowSystemEvent {
    let global = |local: Point| Point::new(origin.x + local.x, origin.y + local.y);
    match event {
        CocoaNativeEvent::MouseDown {
            x,
            y,
            button,
            modifiers,
        } => {
            let pos = local_point(x, y);
            let button = mouse_button(button);
            slot.pressed.press(button);
            WindowSystemEvent::MousePress {
                pos,
                global_pos: global(pos),
                button,
                modifiers: KeyboardModifiers::from_bits(modifiers),
                // `CocoaNativeEvent::MouseDown` carries no time; the press is stamped on arrival.
                timestamp: crate::window_system_interface::event_time_ms(),
            }
        }
        CocoaNativeEvent::MouseUp {
            x,
            y,
            button,
            modifiers,
        } => {
            let pos = local_point(x, y);
            let button = mouse_button(button);
            slot.pressed.release(button);
            WindowSystemEvent::MouseRelease {
                pos,
                global_pos: global(pos),
                button,
                modifiers: KeyboardModifiers::from_bits(modifiers),
            }
        }
        CocoaNativeEvent::MouseMoved { x, y, modifiers: _ }
        | CocoaNativeEvent::MouseEntered { x, y, modifiers: _ } => {
            let pos = local_point(x, y);
            WindowSystemEvent::MouseMove {
                pos,
                global_pos: global(pos),
                buttons: slot.pressed.buttons(),
            }
        }
        CocoaNativeEvent::MouseExited => WindowSystemEvent::MouseLeave,
        CocoaNativeEvent::ScrollWheel {
            x,
            y,
            delta_x,
            delta_y,
            modifiers,
        } => {
            let pos = local_point(x, y);
            WindowSystemEvent::Wheel {
                pos,
                global_pos: global(pos),
                delta: WheelDelta::new(delta_x as i32, delta_y as i32),
                modifiers: KeyboardModifiers::from_bits(modifiers),
            }
        }
        CocoaNativeEvent::KeyDown {
            key,
            modifiers,
            is_repeat,
        } => WindowSystemEvent::KeyPress {
            key,
            modifiers: KeyboardModifiers::from_bits(modifiers),
            is_repeat,
        },
        CocoaNativeEvent::KeyUp { key, modifiers } => WindowSystemEvent::KeyRelease {
            key,
            modifiers: KeyboardModifiers::from_bits(modifiers),
        },
        CocoaNativeEvent::TextInput {
            commit,
            preedit,
            cursor,
        } => WindowSystemEvent::InputMethod {
            commit_string: commit,
            preedit_string: preedit,
            cursor_position: cursor,
        },
        CocoaNativeEvent::WindowResized { width, height } => WindowSystemEvent::Resize {
            size: Size::new(width as i32, height as i32),
        },
        CocoaNativeEvent::WindowCloseRequested => WindowSystemEvent::CloseRequest,
        CocoaNativeEvent::WindowFocusIn => WindowSystemEvent::FocusIn,
        CocoaNativeEvent::WindowFocusOut => WindowSystemEvent::FocusOut,
        CocoaNativeEvent::WindowMoved { geometry } => {
            WindowSystemEvent::GeometryChange { geometry }
        }
        CocoaNativeEvent::WindowDpiChanged { dpi_x, dpi_y } => {
            WindowSystemEvent::DpiChanged { dpi_x, dpi_y }
        }
    }
}

/// Delivers a window delegate notification (`NSWindowDidBecomeKeyNotification`, etc.) to `view`'s handler.
pub fn dispatch_notification(view: Id, window: Id, name: &str) -> bool {
    ThreadContext::assert_main_thread("dispatch_notification");
    if !has_handler(view) {
        return false;
    }
    let event = match name {
        "NSWindowDidBecomeKeyNotification" => CocoaNativeEvent::WindowFocusIn,
        "NSWindowDidResignKeyNotification" => CocoaNativeEvent::WindowFocusOut,
        "NSWindowWillCloseNotification" => CocoaNativeEvent::WindowCloseRequested,
        "NSWindowDidResizeNotification" => {
            // SAFETY: `view` is the content view; `frame` takes no arguments and returns NSRect.
            let rect = unsafe { ObjcMsg::send_rect_return(view, Sel::register("frame")) };
            CocoaNativeEvent::WindowResized {
                width: rect.size.width,
                height: rect.size.height,
            }
        }
        "NSWindowDidMoveNotification" => {
            // SAFETY: `window` is the NSWindow; `frame` takes no arguments and returns NSRect in
            // Cocoa bottom-left desktop coordinates.
            let rect = unsafe { ObjcMsg::send_rect_return(window, Sel::register("frame")) };
            let cocoa_rect = Rect::new(
                rect.origin.x as i32,
                rect.origin.y as i32,
                rect.size.width as i32,
                rect.size.height as i32,
            );
            let geometry = crate::window_cocoa::qt_mac_flip_global_rect(cocoa_rect);
            CocoaNativeEvent::WindowMoved { geometry }
        }
        "NSWindowDidChangeBackingPropertiesNotification" => {
            // SAFETY: `window` is the NSWindow; `backingScaleFactor` returns CGFloat.
            let scale =
                unsafe { ObjcMsg::send_f64_return(window, Sel::register("backingScaleFactor")) };
            let scale = if scale > 0.0 { scale } else { 1.0 };
            let dpi = (scale * 96.0).round() as u32;
            CocoaNativeEvent::WindowDpiChanged {
                dpi_x: dpi,
                dpi_y: dpi,
            }
        }
        _ => return false,
    };
    dispatch(view, event, Point::new(0, 0))
}

// ---------------------------------------------------------------------------
// NSEvent -> CocoaNativeEvent
// ---------------------------------------------------------------------------

fn sel(name: &str) -> Sel {
    Sel::register(name)
}

/// Where `event` happened: its position in `view` (flipped, so y grows downward and the unit is
/// the logical pixel: points) and the global position of the view's origin in Qt's top-left
/// desktop coordinates. `None` when the view is not registered.
fn locate(view: Id, event: Id) -> Option<(CGPoint, Point)> {
    let window = with_slot(view, |slot| slot.ns_window)?;
    // SAFETY: `event` is the NSEvent AppKit passed to the view method on the main thread, live
    // for that call; `view` is its receiver, which `with_slot` just found registered, so the
    // owning `CocoaNativeWindow` has not been dropped; `window` is that window's `ns_window`,
    // live for the same reason. `locationInWindow` returns NSPoint, `convertPoint:fromView:`
    // and `convertPointToScreen:` take and return NSPoint, matching the `send_*point*` senders.
    unsafe {
        let in_window = ObjcMsg::send_point_return(event, sel("locationInWindow"));
        let local =
            ObjcMsg::send_convert_point(view, sel("convertPoint:fromView:"), in_window, Id::NIL);
        let screen =
            ObjcMsg::send_point_arg_return(window, sel("convertPointToScreen:"), in_window);
        // Cocoa's screen y grows upward from the primary screen's bottom edge.
        let global_x = screen.x.floor() as i32;
        let global_y = (qt_mac_primary_screen_height() as CGFloat - screen.y).floor() as i32;
        let local_int = local_point(local.x, local.y);
        Some((
            local,
            Point::new(global_x - local_int.x, global_y - local_int.y),
        ))
    }
}

/// `-[NSEvent buttonNumber]`: 0 left, 1 right, 2 middle, 3 and up the extra buttons.
fn button_number(event: Id) -> u16 {
    // SAFETY: `event` is a live mouse NSEvent (callers only pass the mouse kinds from
    // `handle_ns_event`), for which `buttonNumber` (NSInteger, no arguments) is defined per the
    // NSEvent documentation [inferred].
    let number = unsafe { ObjcMsg::send_isize_return(event, sel("buttonNumber")) };
    number.clamp(0, u16::MAX as isize) as u16
}

/// Converts the `NSEvent` AppKit sent to the view method `kind` into window events and delivers
/// them. Returns whether the view's window took the event (false: no such window, no handler,
/// or an event with nothing to report); the caller then lets AppKit's default handling run.
///
/// Must run on the main thread, like every AppKit callback.
///
/// # Safety
/// `event` is nil or a live `NSEvent` of the kind `kind` names: the event AppKit passed to the
/// `QNSView` method `kind` stands for (`mouseDown:` for [`NsEventKind::MouseDown`], `keyDown:`
/// for [`NsEventKind::KeyDown`], ...), since each kind sends the selectors defined only for its
/// events. `view` needs no guarantee: it is only used once found in the table of live views.
/// The mock runtime of other targets never dereferences an `Id`, so there any value will do.
pub unsafe fn handle_ns_event(view: Id, kind: NsEventKind, event: Id) -> bool {
    ThreadContext::assert_main_thread("handle_ns_event");
    if with_slot(view, |_| ()).is_none() || event.is_nil() {
        return false;
    }
    // SAFETY: `event` is a non-nil live NSEvent (checked above; the rest is this function's
    // contract); `modifierFlags` is defined for every NSEvent and returns NSUInteger, matching
    // `send_nsuinteger_return`.
    let flags = unsafe { ObjcMsg::send_nsuinteger_return(event, sel("modifierFlags")) };
    let modifiers = qt_modifiers_from_ns_flags(flags);
    match kind {
        NsEventKind::MouseDown
        | NsEventKind::MouseUp
        | NsEventKind::RightMouseDown
        | NsEventKind::RightMouseUp
        | NsEventKind::OtherMouseDown
        | NsEventKind::OtherMouseUp => {
            let Some((local, origin)) = locate(view, event) else {
                return false;
            };
            // The method the event came through names the button; `buttonNumber` is only
            // consulted for the extra buttons (it is not reliable for every kind of event).
            let mut button = match kind {
                NsEventKind::MouseDown | NsEventKind::MouseUp => 0,
                NsEventKind::RightMouseDown | NsEventKind::RightMouseUp => 1,
                _ => button_number(event),
            };
            let pressed = matches!(
                kind,
                NsEventKind::MouseDown | NsEventKind::RightMouseDown | NsEventKind::OtherMouseDown
            );
            // Control-click is a right click on macOS; its release must match (Qt mouseDown:).
            let control = flags & NS_EVENT_MODIFIER_FLAG_CONTROL != 0;
            let ctrl_as_right = with_slot(view, |slot| {
                if pressed {
                    slot.send_up_as_right = button == 0 && control;
                    slot.send_up_as_right
                } else {
                    std::mem::take(&mut slot.send_up_as_right) && button == 0
                }
            })
            .unwrap_or(false);
            if ctrl_as_right {
                button = 1;
            }
            let native = if pressed {
                CocoaNativeEvent::MouseDown {
                    x: local.x,
                    y: local.y,
                    button,
                    modifiers,
                }
            } else {
                CocoaNativeEvent::MouseUp {
                    x: local.x,
                    y: local.y,
                    button,
                    modifiers,
                }
            };
            dispatch(view, native, origin)
        }
        NsEventKind::MouseDragged
        | NsEventKind::RightMouseDragged
        | NsEventKind::OtherMouseDragged
        | NsEventKind::MouseMoved
        | NsEventKind::MouseEntered => {
            let Some((local, origin)) = locate(view, event) else {
                return false;
            };
            let native = if kind == NsEventKind::MouseEntered {
                CocoaNativeEvent::MouseEntered {
                    x: local.x,
                    y: local.y,
                    modifiers,
                }
            } else {
                CocoaNativeEvent::MouseMoved {
                    x: local.x,
                    y: local.y,
                    modifiers,
                }
            };
            dispatch(view, native, origin)
        }
        NsEventKind::MouseExited => dispatch(view, CocoaNativeEvent::MouseExited, Point::new(0, 0)),
        NsEventKind::ScrollWheel => {
            let Some((local, origin)) = locate(view, event) else {
                return false;
            };
            let (delta_x, delta_y) = wheel_angle_delta(event);
            dispatch(
                view,
                CocoaNativeEvent::ScrollWheel {
                    x: local.x,
                    y: local.y,
                    delta_x,
                    delta_y,
                    modifiers,
                },
                origin,
            )
        }
        NsEventKind::KeyDown => handle_key_down(view, event, flags, modifiers),
        NsEventKind::KeyUp => {
            let (key, key_code) = key_of_event(event, flags);
            // The press was consumed by the input method (composition, committed text): there
            // is no press at the window to release.
            let suppressed = with_slot(view, |slot| {
                let before = slot.suppressed_keys.len();
                slot.suppressed_keys.retain(|&code| code != key_code);
                slot.suppressed_keys.len() != before
            })
            .unwrap_or(false);
            if suppressed {
                return true;
            }
            dispatch(
                view,
                CocoaNativeEvent::KeyUp { key, modifiers },
                Point::new(0, 0),
            )
        }
        NsEventKind::FlagsChanged => {
            // SAFETY: `event` is the live flagsChanged NSEvent; `keyCode` (unsigned short, no
            // arguments) is defined for key-type events, which include flagsChanged
            // [inferred: NSEvent documentation].
            let key_code = unsafe { ObjcMsg::send_u16_return(event, sel("keyCode")) };
            let Some((key, flag)) = modifier_key(key_code) else {
                return false;
            };
            let native = if flags & flag != 0 {
                CocoaNativeEvent::KeyDown {
                    key,
                    modifiers,
                    is_repeat: false,
                }
            } else {
                CocoaNativeEvent::KeyUp { key, modifiers }
            };
            dispatch(view, native, Point::new(0, 0))
        }
    }
}

/// The wheel delta of a scroll event in Qt's angle units (1/8 degree; 120 per wheel notch).
/// A trackpad's pixel deltas become 1/4 degree per pixel; a wheel's accelerated delta is
/// reduced to a single notch per event (qnsview_mouse.mm `scrollWheel:`).
fn wheel_angle_delta(event: Id) -> (f64, f64) {
    // SAFETY: `event` is the live scroll-wheel NSEvent `handle_ns_event` passed; the selectors
    // used (`hasPreciseScrollingDeltas` BOOL; `scrollingDeltaX/Y`, `deltaX/Y` CGFloat; none take
    // arguments) are defined for scroll-wheel events [inferred: NSEvent documentation], and each
    // matches the sender used for it.
    unsafe {
        if ObjcMsg::send_bool_prop(event, sel("hasPreciseScrollingDeltas")) {
            let pixels_to_degrees = 2.0;
            (
                ObjcMsg::send_cgfloat_return(event, sel("scrollingDeltaX")) * pixels_to_degrees,
                ObjcMsg::send_cgfloat_return(event, sel("scrollingDeltaY")) * pixels_to_degrees,
            )
        } else {
            let notch = |delta: CGFloat| ((delta * 10000.0) as i32).clamp(-120, 120) as f64;
            (
                notch(ObjcMsg::send_cgfloat_return(event, sel("deltaX"))),
                notch(ObjcMsg::send_cgfloat_return(event, sel("deltaY"))),
            )
        }
    }
}

/// The `Qt::Key` and key code of a key event.
fn key_of_event(event: Id, flags: usize) -> (u32, u16) {
    // SAFETY: `event` is the live keyDown/keyUp NSEvent from `handle_ns_event`; `keyCode`
    // (unsigned short) and `charactersIgnoringModifiers` (NSString or nil, copied by
    // `send_string_prop`) are defined for key events, and the latter raises for flagsChanged,
    // which is handled in its own arm and never reaches here [inferred: NSEvent documentation].
    let (key_code, characters) = unsafe {
        (
            ObjcMsg::send_u16_return(event, sel("keyCode")),
            ObjcMsg::send_string_prop(event, sel("charactersIgnoringModifiers")),
        )
    };
    let shift = flags & NS_EVENT_MODIFIER_FLAG_SHIFT != 0;
    (
        qt_key_for_event(key_code, characters.as_deref(), shift),
        key_code,
    )
}

/// `keyDown:` (qnsview_keys.mm `handleKeyEvent:`): the input method sees the key first
/// (`interpretKeyEvents:`), because it may turn it into composition or committed text; the key
/// press reaches the window unless the input method consumed it. Command and Control chords are
/// shortcuts, not text, and skip the input method.
fn handle_key_down(view: Id, event: Id, flags: usize, modifiers: u32) -> bool {
    let (key, key_code) = key_of_event(event, flags);
    // SAFETY: `event` is the live keyDown NSEvent; `isARepeat` (BOOL, no arguments) is defined
    // for keyDown/keyUp and raises for flagsChanged, which never reaches here [inferred:
    // NSEvent documentation].
    let is_repeat = unsafe { ObjcMsg::send_bool_prop(event, sel("isARepeat")) };
    let was_composing = has_marked_text(view);
    let chord = flags & (NS_EVENT_MODIFIER_FLAG_COMMAND | NS_EVENT_MODIFIER_FLAG_CONTROL) != 0;
    if !chord {
        with_slot(view, |slot| slot.send_key_event = true);
        // SAFETY: `view` is the live receiver of the keyDown being handled (found in the slot
        // table by `handle_ns_event`); `array_with_object` builds an autoreleased one-element
        // NSArray holding the live `event`, and `interpretKeyEvents:` takes exactly one NSArray
        // of NSEvents. AppKit calls back into this view's NSTextInputClient methods before it
        // returns, which is why no slot borrow is held across the call.
        unsafe {
            let events = appkit::array_with_object(event);
            ObjcMsg::send_id_logged(view, sel("interpretKeyEvents:"), events);
        }
    }
    let send = with_slot(view, |slot| slot.send_key_event).unwrap_or(false);
    if was_composing || !send {
        // The input method consumed the press: its release must not reach the window alone.
        with_slot(view, |slot| {
            if !slot.suppressed_keys.contains(&key_code) {
                slot.suppressed_keys.push(key_code);
            }
        });
        return true;
    }
    with_slot(view, |slot| {
        slot.suppressed_keys.retain(|&code| code != key_code)
    });
    dispatch(
        view,
        CocoaNativeEvent::KeyDown {
            key,
            modifiers,
            is_repeat,
        },
        Point::new(0, 0),
    )
}

// ---------------------------------------------------------------------------
// NSTextInputClient
// ---------------------------------------------------------------------------

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// `-insertText:replacementRange:` with an `NSString` or `NSAttributedString`.
///
/// # Safety
/// `text` is nil or a live `NSString` or `NSAttributedString`, as AppKit passes to
/// `insertText:replacementRange:`. The mock runtime of other targets never dereferences an
/// `Id`, so there any value will do.
pub unsafe fn text_input_insert(view: Id, text: Id) {
    // SAFETY: `text` is nil or a live NSString or NSAttributedString (this function's contract);
    // `ns_text_to_string` accepts exactly that.
    if let Some(text) = unsafe { appkit::ns_text_to_string(text) } {
        text_input_insert_str(view, &text);
    }
}

/// Text the input method inserted. A plain printable ASCII character typed with no composition
/// is not delivered: its key press already says which character it is (the same information the
/// Win32 backend gives for such a key). Anything else, composed text, other scripts, dead-key
/// results, emoji, reaches the window as the commit string of an input-method event.
pub fn text_input_insert_str(view: Id, text: &str) {
    let was_composing = with_slot(view, |slot| slot.marked_text.take().is_some());
    let Some(was_composing) = was_composing else {
        return;
    };
    let mut chars = text.chars();
    let plain_key_text = matches!(
        (chars.next(), chars.next()),
        (Some(c), None) if c.is_ascii_graphic() || c == ' '
    );
    if !was_composing && plain_key_text {
        return;
    }
    with_slot(view, |slot| slot.send_key_event = false);
    dispatch(
        view,
        CocoaNativeEvent::TextInput {
            commit: text.to_string(),
            preedit: String::new(),
            cursor: 0,
        },
        Point::new(0, 0),
    );
}

/// `-setMarkedText:selectedRange:replacementRange:`: the composition changed.
///
/// # Safety
/// `text` is nil or a live `NSString` or `NSAttributedString`, as AppKit passes to
/// `setMarkedText:selectedRange:replacementRange:`. The mock runtime of other targets never
/// dereferences an `Id`, so there any value will do.
pub unsafe fn text_input_set_marked(view: Id, text: Id, selected: NSRange) {
    // SAFETY: `text` is nil or a live NSString or NSAttributedString (this function's contract);
    // `ns_text_to_string` accepts exactly that.
    let text = unsafe { appkit::ns_text_to_string(text) }.unwrap_or_default();
    text_input_set_marked_str(view, &text, selected);
}

/// The composition is now `text` (empty: cancelled), with the caret at `selected.location`
/// (UTF-16 units, the end when the input method gave none).
pub fn text_input_set_marked_str(view: Id, text: &str, selected: NSRange) {
    let was_composing = with_slot(view, |slot| {
        slot.send_key_event = false;
        let was = slot.marked_text.is_some();
        slot.marked_text = (!text.is_empty()).then(|| text.to_string());
        was
    });
    let Some(was_composing) = was_composing else {
        return;
    };
    if text.is_empty() && !was_composing {
        return;
    }
    let length = utf16_len(text);
    let cursor = if selected.location == NS_NOT_FOUND {
        length
    } else {
        selected.location.min(length)
    };
    dispatch(
        view,
        CocoaNativeEvent::TextInput {
            commit: String::new(),
            preedit: text.to_string(),
            cursor: cursor as i32,
        },
        Point::new(0, 0),
    );
}

/// `-unmarkText`: the input method accepts the composition as it stands, which commits it
/// (qnsview_keys.mm `unmarkText`).
pub fn text_input_unmark(view: Id) {
    let marked = with_slot(view, |slot| slot.marked_text.take()).flatten();
    if let Some(text) = marked {
        dispatch(
            view,
            CocoaNativeEvent::TextInput {
                commit: text,
                preedit: String::new(),
                cursor: 0,
            },
            Point::new(0, 0),
        );
    }
}

pub fn text_input_has_marked_text(view: Id) -> bool {
    has_marked_text(view)
}

fn has_marked_text(view: Id) -> bool {
    with_slot(view, |slot| slot.marked_text.is_some()).unwrap_or(false)
}

/// `-markedRange`: the whole composition, or `{NSNotFound, 0}`.
pub fn text_input_marked_range(view: Id) -> NSRange {
    with_slot(view, |slot| {
        slot.marked_text.as_ref().map(|text| NSRange {
            location: 0,
            length: utf16_len(text),
        })
    })
    .flatten()
    .unwrap_or(NSRange::NOT_FOUND)
}

/// Height of the text line the input method's candidate window avoids, in logical pixels:
/// `set_ime_focus` gives the caret position but no caret height.
pub const IME_LINE_HEIGHT: i32 = 20;

/// `-firstRectForCharacterRange:actualRange:`: the caret line, in screen coordinates (bottom-left
/// origin), where the input method shows its candidate window. `set_ime_focus` gives the caret's
/// top-left position (the view's top-left corner until then); the rectangle is one point wide
/// and `IME_LINE_HEIGHT` high from there downward, so its origin, which AppKit puts the window
/// below, is at the line's bottom edge and the candidates do not cover the typed text.
pub fn text_input_first_rect(view: Id) -> CGRect {
    let Some((window, pos)) = with_slot(view, |slot| {
        (slot.ns_window, slot.ime_pos.unwrap_or(Point::new(0, 0)))
    }) else {
        return CGRect::ZERO;
    };
    // SAFETY: `view` is the receiver of the NSTextInputClient call being handled, live for that
    // call, and `window` is its slot's window (kept alive by the owning `CocoaNativeWindow`);
    // `convertPoint:toView:` and `convertPointToScreen:` take and return NSPoint, matching the
    // senders used.
    let bottom = unsafe {
        let in_window = ObjcMsg::send_convert_point(
            view,
            sel("convertPoint:toView:"),
            CGPoint::new(pos.x as CGFloat, (pos.y + IME_LINE_HEIGHT) as CGFloat),
            Id::NIL,
        );
        ObjcMsg::send_point_arg_return(window, sel("convertPointToScreen:"), in_window)
    };
    CGRect::new(bottom.x, bottom.y, 1.0, IME_LINE_HEIGHT as CGFloat)
}

// ---------------------------------------------------------------------------
// Tracking area and window cursor
// ---------------------------------------------------------------------------

/// Replaces the view's tracking area with a fresh one covering its visible rect (Qt
/// `updateTrackingAreas`): it delivers `mouseEntered:`, `mouseExited:`, `mouseMoved:` and
/// `cursorUpdate:` while the application is active, without the view being first responder.
pub fn update_tracking_area(view: Id) {
    let Some(old) = with_slot(view, |slot| {
        std::mem::replace(&mut slot.tracking_area, Id::NIL)
    }) else {
        return;
    };
    // SAFETY: `view` is live: AppKit invoked this from the view's own method, or the window
    // just registered it. `old` was taken out of the slot just above, so it is the area this
    // slot added to the view and holds one reference to; it is removed and released once here.
    // The new area comes from `alloc` + `initWithRect:options:owner:userInfo:` (+1) with the
    // view's own bounds and the view as owner, and is either stored in the slot (which then
    // owns the +1) or removed and released again, so each path balances. `bounds` returns
    // NSRect; `addTrackingArea:` / `removeTrackingArea:` take one NSTrackingArea.
    unsafe {
        if !old.is_nil() {
            ObjcMsg::send_id_logged(view, sel("removeTrackingArea:"), old);
            appkit::release(old);
        }
        let class = crate::objc_runtime::objc_get_class("NSTrackingArea");
        let alloc = ObjcMsg::send_class_0(class, sel("alloc"));
        let bounds = ObjcMsg::send_rect_return(view, sel("bounds"));
        let area = ObjcMsg::send_tracking_area_init(alloc, bounds, TRACKING_AREA_OPTIONS, view);
        if area.is_nil() {
            return;
        }
        ObjcMsg::send_id_logged(view, sel("addTrackingArea:"), area);
        let kept = with_slot(view, |slot| slot.tracking_area = area).is_some();
        if !kept {
            ObjcMsg::send_id_logged(view, sel("removeTrackingArea:"), area);
            appkit::release(area);
        }
    }
}

/// `-resetCursorRects` (Qt `QNSView`): the window cursor covers the visible rect. False when the
/// window has no cursor of its own, so the caller runs the default implementation.
pub fn reset_cursor_rects(view: Id) -> bool {
    let cursor = with_slot(view, |slot| slot.cursor).unwrap_or(Id::NIL);
    if cursor.is_nil() {
        return false;
    }
    // SAFETY: `view` is the receiver of the AppKit call being handled, live for that call;
    // `cursor` is non-nil (checked above) and was retained by the slot (`replace_cursor`), so
    // it outlives `addCursorRect:cursor:` (AppKit retains it for the rect too) [inferred].
    // `visibleRect` returns NSRect; `discardCursorRects` takes no arguments.
    unsafe {
        ObjcMsg::send_0_logged(view, sel("discardCursorRects"));
        let visible = ObjcMsg::send_rect_return(view, sel("visibleRect"));
        ObjcMsg::send_rect_id_logged(view, sel("addCursorRect:cursor:"), visible, cursor);
    }
    true
}

/// `-cursorUpdate:` (Qt `QNSView`): the pointer entered the view, so the window cursor shows.
/// False when the window has none, so the caller runs the default implementation.
pub fn cursor_update(view: Id) -> bool {
    let cursor = with_slot(view, |slot| slot.cursor).unwrap_or(Id::NIL);
    if cursor.is_nil() {
        return false;
    }
    // SAFETY: `cursor` is the non-nil NSCursor retained by the slot (`replace_cursor`), live
    // here; `set` takes no arguments.
    unsafe { ObjcMsg::send_0_logged(cursor, sel("set")) };
    true
}

/// Makes `cursor` (a shared `NSCursor`, or nil for the default one) the window cursor and has
/// AppKit rebuild the view's cursor rects. The slot keeps its own reference.
pub(crate) fn replace_cursor(view: Id, cursor: Id) {
    // SAFETY: `cursor` is nil (retain on nil is a no-op) or a shared NSCursor; retaining it
    // takes the +1 the slot will own, released when the cursor is replaced (below), when the
    // slot is torn down, or in the early-return branch if there is no slot.
    unsafe { appkit::retain(cursor) };
    let slot = with_slot(view, |slot| {
        (std::mem::replace(&mut slot.cursor, cursor), slot.ns_window)
    });
    let Some((previous, window)) = slot else {
        // SAFETY: balances the `retain` above: no slot took ownership, so this releases the +1
        // taken a few lines up and nothing else holds that reference.
        unsafe { appkit::release(cursor) };
        return;
    };
    // SAFETY: `previous` is nil or the cursor this slot retained earlier, swapped out of the
    // slot above, so this is the one release of that +1. `window` is the slot's `ns_window`,
    // live while the slot exists (the window removes the slot before releasing the window);
    // `invalidateCursorRectsForView:` takes one NSView, here `view`.
    unsafe {
        appkit::release(previous);
        ObjcMsg::send_id_logged(window, sel("invalidateCursorRectsForView:"), view);
    }
}
