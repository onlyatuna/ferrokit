// SPDX-License-Identifier: LGPL-3.0-only
pub mod compressor;
pub use compressor::*;

pub mod event_filter;
pub mod pointer;
pub mod types;

use crate::object::ObjectId;
pub use pointer::*;
use std::any::Any;
use std::fmt;
pub use types::*;

pub use event_filter::*;

/// Reason for focus change (`Qt::FocusReason`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FocusReason {
    #[default]
    Other,
    Mouse,
    Tab,
    Backtab,
    ActiveWindow,
    Popup,
    Shortcut,
}

/// Cross-thread queued functor for `EventKind::MetaCall`.
pub type MetaCallFn = Box<dyn FnOnce(&mut dyn Any) + Send + 'static>;

/// Cross-thread queued functor for `EventKind::MetaCallNoTarget`.
pub type MetaCallNoTargetFn = Box<dyn FnOnce() + Send + 'static>;

/// Event type and payload enumeration: `EventKind`.
/// Modeled after Qt `QEvent::Type` and derived event classes.
pub enum EventKind {
    /// Timer event carries `timer_id` (`QTimerEvent`).
    Timer {
        timer_id: u64,
    },

    /// 0ms zero timer for next-tick / yield optimization (`QZeroTimerEvent`).
    ZeroTimer {
        timer_id: u64,
    },

    /// Deferred deletion event (`QDeferredDeleteEvent`). The levels are the requesting thread's
    /// loop and handler-scope levels at request time (qobject.cpp:2539-2559).
    DeferredDelete {
        loop_level: usize,
        scope_level: usize,
    },

    /// Widget layout recalculation request (`QEvent::LayoutRequest`, compressible).
    LayoutRequest,

    /// Widget repaint request (`QEvent::UpdateRequest`, compressible).
    UpdateRequest,

    /// Application quit request (`QEvent::Quit`).
    Quit {
        exit_code: i32,
    },

    /// Cross-thread queued functor execution (`QMetaCallEvent`) that needs the receiver as its
    /// target. The receiver stays borrowed while it runs.
    MetaCall(MetaCallFn),

    /// Cross-thread queued functor that does not use its receiver. The receiver is not borrowed
    /// while it runs, so the functor may reach that receiver through `with_object_mut`.
    MetaCallNoTarget(MetaCallNoTargetFn),

    /// Custom user event (`QEvent::User`).
    User(Box<dyn Any + Send + 'static>),

    /// Window close event (`QCloseEvent`).
    Close,

    /// Mouse move and click events (`QMouseEvent`).
    MouseMove {
        x: i32,
        y: i32,
    },
    MouseButtonPress {
        x: i32,
        y: i32,
        button: u32,
    },
    MouseButtonRelease {
        x: i32,
        y: i32,
        button: u32,
    },
    MouseButtonDblClick {
        x: i32,
        y: i32,
        button: u32,
    },

    /// Unified high-precision pointer event (`QPointerEvent`).
    Pointer {
        device_id: PointerDeviceId,
        points: Vec<EventPoint>,
        buttons: MouseButtons,
        modifiers: KeyboardModifiers,
        timestamp: u64,
    },

    /// Mouse enter event (`QEvent::Enter`).
    Enter {
        x: i32,
        y: i32,
    },

    /// Mouse leave event (`QEvent::Leave`).
    Leave,

    /// Mouse hover events (`QHoverEvent`).
    HoverEnter {
        pos: EventPointPos,
        old_pos: EventPointPos,
        modifiers: KeyboardModifiers,
    },
    HoverMove {
        pos: EventPointPos,
        old_pos: EventPointPos,
        modifiers: KeyboardModifiers,
    },
    HoverLeave {
        old_pos: EventPointPos,
        modifiers: KeyboardModifiers,
    },

    /// High-precision graphics tablet events (`QTabletEvent`).
    TabletPress {
        device: TabletDevice,
        pointer_type: TabletPointerType,
        pos: EventPointPos,
        global_pos: EventPointPos,
        pressure: f32,
        x_tilt: f32,
        y_tilt: f32,
        rotation: f32,
        buttons: MouseButtons,
        modifiers: KeyboardModifiers,
    },
    TabletMove {
        device: TabletDevice,
        pointer_type: TabletPointerType,
        pos: EventPointPos,
        global_pos: EventPointPos,
        pressure: f32,
        x_tilt: f32,
        y_tilt: f32,
        rotation: f32,
        buttons: MouseButtons,
        modifiers: KeyboardModifiers,
    },
    TabletRelease {
        device: TabletDevice,
        pointer_type: TabletPointerType,
        pos: EventPointPos,
        global_pos: EventPointPos,
        pressure: f32,
        x_tilt: f32,
        y_tilt: f32,
        rotation: f32,
        buttons: MouseButtons,
        modifiers: KeyboardModifiers,
    },

    /// Multi-touch contact events (`QTouchEvent`).
    TouchBegin {
        device_id: PointerDeviceId,
        points: Vec<EventPoint>,
        modifiers: KeyboardModifiers,
    },
    TouchUpdate {
        device_id: PointerDeviceId,
        points: Vec<EventPoint>,
        modifiers: KeyboardModifiers,
    },
    TouchEnd {
        device_id: PointerDeviceId,
        points: Vec<EventPoint>,
        modifiers: KeyboardModifiers,
    },
    TouchCancel {
        device_id: PointerDeviceId,
        points: Vec<EventPoint>,
        modifiers: KeyboardModifiers,
    },

    /// High-level gesture events (`QGestureEvent`).
    Gesture {
        state: GestureState,
        gesture: GestureType,
    },
    NativeGesture {
        gesture_type: NativeGestureType,
        pos: EventPointPos,
        global_pos: EventPointPos,
        value: f32,
        modifiers: KeyboardModifiers,
    },
    /// Mouse wheel scroll event (`QWheelEvent`).
    Wheel {
        x: i32,
        y: i32,
        pixel_delta_x: i32,
        pixel_delta_y: i32,
        angle_delta_x: i32,
        angle_delta_y: i32,
        modifiers: u32,
    },

    /// Keyboard key press and release events (`QKeyEvent`).
    KeyPress {
        key: u32,
        modifiers: u32,
        is_repeat: bool,
    },
    KeyRelease {
        key: u32,
        modifiers: u32,
    },

    /// Window resize event (`QResizeEvent`).
    Resize {
        width: i32,
        height: i32,
        old_width: i32,
        old_height: i32,
    },

    /// Window focus events (`QFocusEvent`).
    FocusIn {
        reason: FocusReason,
    },
    FocusOut {
        reason: FocusReason,
    },

    /// Window move and lifecycle events (`QMoveEvent`, `QShowEvent`, `QHideEvent`, `QExposeEvent`).
    Move {
        x: i32,
        y: i32,
        old_x: i32,
        old_y: i32,
    },
    Show,
    Hide,
    Expose,
    WindowActivate,
    WindowDeactivate,

    /// Keyboard shortcut events (`QShortcutEvent`).
    Shortcut {
        key: u32,
        modifiers: u32,
        shortcut_id: u32,
        ambiguous: bool,
    },
    ShortcutOverride {
        key: u32,
        modifiers: u32,
    },

    /// Context menu popup request (`QContextMenuEvent`).
    ContextMenu {
        x: i32,
        y: i32,
        global_x: i32,
        global_y: i32,
        reason: ContextMenuReason,
    },

    /// `QHelpEvent(QEvent::ToolTip)`: the position under the cursor in the receiving widget's
    /// coordinates and in global coordinates. It carries no text; the widget supplies its own
    /// `toolTip` (`QWidget::event`, qwidget.cpp:9381-9386).
    ToolTip {
        x: i32,
        y: i32,
        global_x: i32,
        global_y: i32,
    },
    StatusTip {
        text: String,
    },
    WhatsThis {
        x: i32,
        y: i32,
        text: String,
    },
    QueryWhatsThis,

    /// Action change and trigger notifications (`QActionEvent`).
    ActionChanged {
        action_id: u32,
    },
    ActionAdded {
        action_id: u32,
    },
    ActionRemoved {
        action_id: u32,
    },

    /// DPI changed event.
    DpiChanged {
        dpi_x: u32,
        dpi_y: u32,
    },

    /// Dynamic property changed event (`QDynamicPropertyChangeEvent`).
    DynamicPropertyChange {
        property_name: String,
    },

    /// Input method / IME composition event (`QInputMethodEvent`).
    InputMethod {
        commit_string: String,
        preedit_string: String,
        cursor_position: i32,
    },

    /// Drag and Drop events (`QDragEnterEvent`, `QDragMoveEvent`, `QDropEvent`).
    DragEnter {
        pos_x: i32,
        pos_y: i32,
        formats: Vec<String>,
        drop_action: u32,
    },
    DragMove {
        pos_x: i32,
        pos_y: i32,
        drop_action: u32,
    },
    DragLeave,
    Drop {
        pos_x: i32,
        pos_y: i32,
        formats: Vec<String>,
        data: Vec<(String, Vec<u8>)>,
        drop_action: u32,
    },
    /// Child object added event (`QChildEvent::added()`).
    ChildAdded {
        child_id: ObjectId,
    },

    /// Child object removed event (`QChildEvent::removed()`).
    ChildRemoved {
        child_id: ObjectId,
    },

    /// Thread migration event (`QEvent::ThreadChange`).
    ThreadChange,

    /// The widget's effective enabled state changed (`QEvent::EnabledChange`).
    EnabledChange,

    /// Paint request for a widget's dirty rectangle, in widget coordinates (`QEvent::Paint`).
    /// Sent spontaneously by the window's render pass, before `paint_event` (qwidget.cpp:9186).
    Paint {
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    },
    /// The widget's font changed (`QEvent::FontChange`, qwidget.cpp:4894-4895).
    FontChange,
    /// The widget's style or style sheet changed (`QEvent::StyleChange`, qwidget.cpp:2724-2725).
    StyleChange,
    /// The widget's tooltip changed (`QEvent::ToolTipChange`, qwidget.cpp:11798-11799).
    ToolTipChange,
    /// The widget is polished (`QEvent::Polish`, sent once by `QWidget::ensurePolished`).
    Polish,
    /// A child widget has been polished (`QChildEvent` of `QEvent::ChildPolished`).
    ChildPolished {
        child_id: ObjectId,
    },
    /// The widget's stacking order changed (`QEvent::ZOrderChange`, `QWidget::raise`/`lower`).
    ZOrderChange,
}

impl fmt::Debug for EventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EventKind::Timer { timer_id } => {
                f.debug_struct("Timer").field("timer_id", timer_id).finish()
            }
            EventKind::ZeroTimer { timer_id } => f
                .debug_struct("ZeroTimer")
                .field("timer_id", timer_id)
                .finish(),
            EventKind::DeferredDelete {
                loop_level,
                scope_level,
            } => f
                .debug_struct("DeferredDelete")
                .field("loop_level", loop_level)
                .field("scope_level", scope_level)
                .finish(),
            EventKind::LayoutRequest => write!(f, "LayoutRequest"),
            EventKind::UpdateRequest => write!(f, "UpdateRequest"),
            EventKind::Quit { exit_code } => f
                .debug_struct("Quit")
                .field("exit_code", exit_code)
                .finish(),
            EventKind::MetaCall(_) => write!(f, "MetaCall(<FnOnce>)"),
            EventKind::MetaCallNoTarget(_) => write!(f, "MetaCallNoTarget(<FnOnce>)"),
            EventKind::User(_) => write!(f, "User(<Any>)"),
            EventKind::Close => write!(f, "Close"),
            EventKind::MouseMove { x, y } => write!(f, "MouseMove({}, {})", x, y),
            EventKind::MouseButtonPress { x, y, button } => {
                write!(f, "MouseButtonPress({}, {}, button={})", x, y, button)
            }
            EventKind::MouseButtonRelease { x, y, button } => {
                write!(f, "MouseButtonRelease({}, {}, button={})", x, y, button)
            }
            EventKind::MouseButtonDblClick { x, y, button } => {
                write!(f, "MouseButtonDblClick({}, {}, button={})", x, y, button)
            }
            EventKind::Pointer {
                device_id,
                points,
                buttons,
                modifiers,
                timestamp,
            } => {
                write!(
                    f,
                    "Pointer(dev={:?}, points={}, buttons={:#x}, mods={:#x}, ts={})",
                    device_id,
                    points.len(),
                    buttons.0,
                    modifiers.0,
                    timestamp
                )
            }
            EventKind::Enter { x, y } => write!(f, "Enter({}, {})", x, y),
            EventKind::Leave => write!(f, "Leave"),
            EventKind::HoverEnter {
                pos,
                old_pos,
                modifiers,
            } => {
                write!(
                    f,
                    "HoverEnter(pos=({},{}), old=({},{}), mods={:#x})",
                    pos.x, pos.y, old_pos.x, old_pos.y, modifiers.0
                )
            }
            EventKind::HoverMove {
                pos,
                old_pos,
                modifiers,
            } => {
                write!(
                    f,
                    "HoverMove(pos=({},{}), old=({},{}), mods={:#x})",
                    pos.x, pos.y, old_pos.x, old_pos.y, modifiers.0
                )
            }
            EventKind::HoverLeave { old_pos, modifiers } => {
                write!(
                    f,
                    "HoverLeave(old=({},{}), mods={:#x})",
                    old_pos.x, old_pos.y, modifiers.0
                )
            }
            EventKind::TabletPress {
                device,
                pointer_type,
                pos,
                pressure,
                ..
            } => {
                write!(
                    f,
                    "TabletPress(dev={:?}, type={:?}, pos=({},{}), pressure={})",
                    device, pointer_type, pos.x, pos.y, pressure
                )
            }
            EventKind::TabletMove {
                device,
                pointer_type,
                pos,
                pressure,
                ..
            } => {
                write!(
                    f,
                    "TabletMove(dev={:?}, type={:?}, pos=({},{}), pressure={})",
                    device, pointer_type, pos.x, pos.y, pressure
                )
            }
            EventKind::TabletRelease {
                device,
                pointer_type,
                pos,
                ..
            } => {
                write!(
                    f,
                    "TabletRelease(dev={:?}, type={:?}, pos=({},{}))",
                    device, pointer_type, pos.x, pos.y
                )
            }
            EventKind::TouchBegin {
                device_id, points, ..
            } => {
                write!(
                    f,
                    "TouchBegin(dev={:?}, points={})",
                    device_id,
                    points.len()
                )
            }
            EventKind::TouchUpdate {
                device_id, points, ..
            } => {
                write!(
                    f,
                    "TouchUpdate(dev={:?}, points={})",
                    device_id,
                    points.len()
                )
            }
            EventKind::TouchEnd {
                device_id, points, ..
            } => {
                write!(f, "TouchEnd(dev={:?}, points={})", device_id, points.len())
            }
            EventKind::TouchCancel {
                device_id, points, ..
            } => {
                write!(
                    f,
                    "TouchCancel(dev={:?}, points={})",
                    device_id,
                    points.len()
                )
            }
            EventKind::Gesture { state, gesture } => {
                write!(f, "Gesture(state={:?}, gesture={:?})", state, gesture)
            }
            EventKind::NativeGesture {
                gesture_type,
                pos,
                value,
                ..
            } => {
                write!(
                    f,
                    "NativeGesture(type={:?}, pos=({},{}), val={})",
                    gesture_type, pos.x, pos.y, value
                )
            }
            EventKind::Wheel {
                x,
                y,
                pixel_delta_x,
                pixel_delta_y,
                angle_delta_x,
                angle_delta_y,
                modifiers,
            } => {
                write!(
                    f,
                    "Wheel(pos=({}, {}), pixel_delta=({}, {}), angle_delta=({}, {}), modifiers={:#x})",
                    x, y, pixel_delta_x, pixel_delta_y, angle_delta_x, angle_delta_y, modifiers
                )
            }
            EventKind::KeyPress {
                key,
                modifiers,
                is_repeat,
            } => {
                write!(
                    f,
                    "KeyPress(key={:#x}, modifiers={:#x}, repeat={})",
                    key, modifiers, is_repeat
                )
            }
            EventKind::KeyRelease { key, modifiers } => {
                write!(f, "KeyRelease(key={:#x}, modifiers={:#x})", key, modifiers)
            }
            EventKind::Resize {
                width,
                height,
                old_width,
                old_height,
            } => {
                write!(
                    f,
                    "Resize(size={}x{}, old={}x{})",
                    width, height, old_width, old_height
                )
            }
            EventKind::FocusIn { reason } => write!(f, "FocusIn({:?})", reason),
            EventKind::FocusOut { reason } => write!(f, "FocusOut({:?})", reason),
            EventKind::Move { x, y, old_x, old_y } => {
                write!(f, "Move(pos=({}, {}), old=({}, {}))", x, y, old_x, old_y)
            }
            EventKind::Show => write!(f, "Show"),
            EventKind::Hide => write!(f, "Hide"),
            EventKind::Expose => write!(f, "Expose"),
            EventKind::WindowActivate => write!(f, "WindowActivate"),
            EventKind::WindowDeactivate => write!(f, "WindowDeactivate"),
            EventKind::Shortcut {
                key,
                modifiers,
                shortcut_id,
                ambiguous,
            } => {
                write!(
                    f,
                    "Shortcut(key={:#x}, mods={:#x}, id={}, amb={})",
                    key, modifiers, shortcut_id, ambiguous
                )
            }
            EventKind::ShortcutOverride { key, modifiers } => {
                write!(f, "ShortcutOverride(key={:#x}, mods={:#x})", key, modifiers)
            }
            EventKind::ContextMenu {
                x,
                y,
                global_x,
                global_y,
                reason,
            } => {
                write!(
                    f,
                    "ContextMenu(pos=({},{}), global=({},{}), reason={:?})",
                    x, y, global_x, global_y, reason
                )
            }
            EventKind::ToolTip {
                x,
                y,
                global_x,
                global_y,
            } => write!(
                f,
                "ToolTip(({},{}), global ({},{}))",
                x, y, global_x, global_y
            ),
            EventKind::StatusTip { text } => write!(f, "StatusTip(\"{}\")", text),
            EventKind::WhatsThis { x, y, text } => {
                write!(f, "WhatsThis(({},{}), \"{}\")", x, y, text)
            }
            EventKind::QueryWhatsThis => write!(f, "QueryWhatsThis"),
            EventKind::ActionChanged { action_id } => write!(f, "ActionChanged({})", action_id),
            EventKind::ActionAdded { action_id } => write!(f, "ActionAdded({})", action_id),
            EventKind::ActionRemoved { action_id } => write!(f, "ActionRemoved({})", action_id),
            EventKind::DpiChanged { dpi_x, dpi_y } => {
                write!(f, "DpiChanged({}x{})", dpi_x, dpi_y)
            }
            EventKind::DynamicPropertyChange { property_name } => {
                write!(f, "DynamicPropertyChange(\"{}\")", property_name)
            }
            EventKind::InputMethod {
                commit_string,
                preedit_string,
                cursor_position,
            } => {
                write!(
                    f,
                    "InputMethod(commit={:?}, preedit={:?}, pos={})",
                    commit_string, preedit_string, cursor_position
                )
            }
            EventKind::DragEnter {
                pos_x,
                pos_y,
                formats,
                drop_action,
            } => {
                write!(
                    f,
                    "DragEnter(({}, {}), formats={:?}, action={})",
                    pos_x, pos_y, formats, drop_action
                )
            }
            EventKind::DragMove {
                pos_x,
                pos_y,
                drop_action,
            } => {
                write!(
                    f,
                    "DragMove(({}, {}), action={})",
                    pos_x, pos_y, drop_action
                )
            }
            EventKind::DragLeave => write!(f, "DragLeave"),
            EventKind::Drop {
                pos_x,
                pos_y,
                formats,
                drop_action,
                ..
            } => {
                write!(
                    f,
                    "Drop(({}, {}), formats={:?}, action={})",
                    pos_x, pos_y, formats, drop_action
                )
            }
            EventKind::ChildAdded { child_id } => {
                write!(f, "ChildAdded(child={:?})", child_id)
            }
            EventKind::ChildRemoved { child_id } => {
                write!(f, "ChildRemoved(child={:?})", child_id)
            }
            EventKind::ThreadChange => write!(f, "ThreadChange"),
            EventKind::EnabledChange => write!(f, "EnabledChange"),
            EventKind::Paint {
                x,
                y,
                width,
                height,
            } => write!(f, "Paint(({}, {}) {}x{})", x, y, width, height),
            EventKind::FontChange => write!(f, "FontChange"),
            EventKind::StyleChange => write!(f, "StyleChange"),
            EventKind::ToolTipChange => write!(f, "ToolTipChange"),
            EventKind::Polish => write!(f, "Polish"),
            EventKind::ChildPolished { child_id } => {
                write!(f, "ChildPolished(child={:?})", child_id)
            }
            EventKind::ZOrderChange => write!(f, "ZOrderChange"),
        }
    }
}

impl EventKind {
    /// Returns true if this event kind can be coalesced / compressed.
    pub fn is_compressible(&self) -> bool {
        matches!(
            self,
            EventKind::Timer { .. }
                | EventKind::ZeroTimer { .. }
                | EventKind::UpdateRequest
                | EventKind::LayoutRequest
        )
    }

    /// Returns the corresponding strongly-typed Qt `QEvent::Type` (`event->type()`).
    pub fn event_type(&self) -> EventType {
        match self {
            EventKind::Timer { .. } => EventType::Timer,
            EventKind::ZeroTimer { .. } => EventType::ZeroTimerEvent,
            EventKind::DeferredDelete { .. } => EventType::DeferredDelete,
            EventKind::LayoutRequest => EventType::LayoutRequest,
            EventKind::UpdateRequest => EventType::UpdateRequest,
            EventKind::Quit { .. } => EventType::Quit,
            EventKind::MetaCall(_) | EventKind::MetaCallNoTarget(_) => EventType::MetaCall,
            EventKind::User(_) => EventType::User,
            EventKind::Close => EventType::Close,
            EventKind::MouseMove { .. } => EventType::MouseMove,
            EventKind::MouseButtonPress { .. } => EventType::MouseButtonPress,
            EventKind::MouseButtonRelease { .. } => EventType::MouseButtonRelease,
            EventKind::MouseButtonDblClick { .. } => EventType::MouseButtonDblClick,
            EventKind::Pointer { .. } => EventType::Pointer,
            EventKind::Enter { .. } => EventType::Enter,
            EventKind::Leave => EventType::Leave,
            EventKind::HoverEnter { .. } => EventType::HoverEnter,
            EventKind::HoverMove { .. } => EventType::HoverMove,
            EventKind::HoverLeave { .. } => EventType::HoverLeave,
            EventKind::Wheel { .. } => EventType::Wheel,
            EventKind::KeyPress { .. } => EventType::KeyPress,
            EventKind::KeyRelease { .. } => EventType::KeyRelease,
            EventKind::Resize { .. } => EventType::Resize,
            EventKind::FocusIn { .. } => EventType::FocusIn,
            EventKind::FocusOut { .. } => EventType::FocusOut,
            EventKind::DpiChanged { .. } => EventType::DpiChanged,
            EventKind::DynamicPropertyChange { .. } => EventType::DynamicPropertyChange,
            EventKind::InputMethod { .. } => EventType::InputMethod,
            EventKind::DragEnter { .. } => EventType::DragEnter,
            EventKind::DragMove { .. } => EventType::DragMove,
            EventKind::DragLeave => EventType::DragLeave,
            EventKind::Drop { .. } => EventType::Drop,
            EventKind::ChildAdded { .. } => EventType::ChildAdded,
            EventKind::ChildRemoved { .. } => EventType::ChildRemoved,
            EventKind::ThreadChange => EventType::ThreadChange,
            EventKind::EnabledChange => EventType::EnabledChange,
            EventKind::Paint { .. } => EventType::Paint,
            EventKind::FontChange => EventType::FontChange,
            EventKind::StyleChange => EventType::StyleChange,
            EventKind::ToolTipChange => EventType::ToolTipChange,
            EventKind::Polish => EventType::Polish,
            EventKind::ChildPolished { .. } => EventType::ChildPolished,
            EventKind::ZOrderChange => EventType::ZOrderChange,
            EventKind::TabletPress { .. } => EventType::TabletPress,
            EventKind::TabletMove { .. } => EventType::TabletMove,
            EventKind::TabletRelease { .. } => EventType::TabletRelease,
            EventKind::TouchBegin { .. } => EventType::TouchBegin,
            EventKind::TouchUpdate { .. } => EventType::TouchUpdate,
            EventKind::TouchEnd { .. } => EventType::TouchEnd,
            EventKind::TouchCancel { .. } => EventType::TouchCancel,
            EventKind::Gesture { .. } => EventType::Gesture,
            EventKind::NativeGesture { .. } => EventType::NativeGesture,
            EventKind::Move { .. } => EventType::Move,
            EventKind::Show => EventType::Show,
            EventKind::Hide => EventType::Hide,
            EventKind::Expose => EventType::Expose,
            EventKind::WindowActivate => EventType::WindowActivate,
            EventKind::WindowDeactivate => EventType::WindowDeactivate,
            EventKind::Shortcut { .. } => EventType::Shortcut,
            EventKind::ShortcutOverride { .. } => EventType::ShortcutOverride,
            EventKind::ContextMenu { .. } => EventType::ContextMenu,
            EventKind::ToolTip { .. } => EventType::ToolTip,
            EventKind::StatusTip { .. } => EventType::StatusTip,
            EventKind::WhatsThis { .. } => EventType::WhatsThis,
            EventKind::QueryWhatsThis => EventType::QueryWhatsThis,
            EventKind::ActionChanged { .. } => EventType::ActionChanged,
            EventKind::ActionAdded { .. } => EventType::ActionAdded,
            EventKind::ActionRemoved { .. } => EventType::ActionRemoved,
        }
    }
}

/// Generic event container (`QEvent` equivalent).
#[derive(Debug)]
pub struct Event {
    /// Event type and payload.
    pub kind: EventKind,
    accepted: bool,
    spontaneous: bool,
}

impl Event {
    /// Creates a non-spontaneous event (accepted = true, spontaneous = false).
    pub fn new(kind: EventKind) -> Self {
        Self {
            kind,
            accepted: true,
            spontaneous: false,
        }
    }

    /// Creates an OS-generated spontaneous event (accepted = true, spontaneous = true).
    pub fn new_spontaneous(kind: EventKind) -> Self {
        Self {
            kind,
            accepted: true,
            spontaneous: true,
        }
    }
    /// Returns the Qt-equivalent event type (`QEvent::type()`).
    #[inline]
    pub fn event_type(&self) -> EventType {
        self.kind.event_type()
    }

    /// Marks the event as accepted (`QEvent::accept`).
    pub fn accept(&mut self) {
        self.accepted = true;
    }

    /// Marks the event as ignored / unhandled (`QEvent::ignore`).
    pub fn ignore(&mut self) {
        self.accepted = false;
    }

    /// Returns true if the event has been accepted (`QEvent::isAccepted`).
    pub fn is_accepted(&self) -> bool {
        self.accepted
    }

    /// Sets the accepted state of the event.
    pub fn set_accepted(&mut self, accepted: bool) {
        self.accepted = accepted;
    }

    /// Returns true if the event was generated by the operating system (`QEvent::spontaneous`).
    pub fn is_spontaneous(&self) -> bool {
        self.spontaneous
    }

    /// Returns true if the event can be compressed.
    pub fn is_compressible(&self) -> bool {
        self.kind.is_compressible()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_accept_ignore() {
        let mut event = Event::new(EventKind::LayoutRequest);
        assert!(event.is_accepted());

        event.ignore();
        assert!(!event.is_accepted());

        event.accept();
        assert!(event.is_accepted());
    }

    #[test]
    fn test_is_compressible() {
        let timer_event = Event::new(EventKind::Timer { timer_id: 1 });
        let update_event = Event::new(EventKind::UpdateRequest);
        let layout_event = Event::new(EventKind::LayoutRequest);

        assert!(timer_event.is_compressible());
        assert!(update_event.is_compressible());
        assert!(layout_event.is_compressible());

        let quit_event = Event::new(EventKind::Quit { exit_code: 0 });
        let metacall_event = Event::new(EventKind::MetaCall(Box::new(|_| {})));
        let user_event = Event::new(EventKind::User(Box::new(42u32)));

        assert!(!quit_event.is_compressible());
        assert!(!metacall_event.is_compressible());
        assert!(!user_event.is_compressible());
    }

    #[test]
    fn test_spontaneous() {
        let normal_event = Event::new(EventKind::UpdateRequest);
        assert!(!normal_event.is_spontaneous());

        let spont_event = Event::new_spontaneous(EventKind::UpdateRequest);
        assert!(spont_event.is_spontaneous());
    }

    #[test]
    fn test_metacall_dispatch() {
        let mut x: i32 = 0;
        let event = Event::new(EventKind::MetaCall(Box::new(|target| {
            if let Some(val) = target.downcast_mut::<i32>() {
                *val += 10;
            }
        })));

        if let EventKind::MetaCall(task) = event.kind {
            task(&mut x);
        } else {
            panic!("Expected EventKind::MetaCall");
        }

        assert_eq!(x, 10);
    }

    #[test]
    fn test_user_downcast() {
        #[derive(Debug, PartialEq, Eq)]
        struct MyMetrics {
            tokens: u32,
        }

        let original = MyMetrics { tokens: 42 };
        let event = Event::new(EventKind::User(Box::new(original)));

        if let EventKind::User(any_data) = event.kind {
            let restored = any_data.downcast_ref::<MyMetrics>();
            assert!(restored.is_some());
            assert_eq!(restored.unwrap().tokens, 42);
        } else {
            panic!("Expected EventKind::User");
        }
    }

    #[test]
    fn test_send_assertion() {
        fn assert_send<T: Send>() {}
        assert_send::<Event>();
        assert_send::<EventKind>();
    }

    #[test]
    fn test_window_system_events_creation_and_debug() {
        let wheel = Event::new_spontaneous(EventKind::Wheel {
            x: 100,
            y: 200,
            pixel_delta_x: 0,
            pixel_delta_y: 0,
            angle_delta_x: 0,
            angle_delta_y: 120,
            modifiers: 0x0200_0000,
        });
        assert!(wheel.is_spontaneous());
        let wheel_dbg = format!("{:?}", wheel.kind);
        assert!(wheel_dbg.contains("Wheel"));
        assert!(wheel_dbg.contains("angle_delta=(0, 120)"));

        let key_press = Event::new_spontaneous(EventKind::KeyPress {
            key: 0x41,
            modifiers: 0x0400_0000,
            is_repeat: true,
        });
        let key_dbg = format!("{:?}", key_press.kind);
        assert!(key_dbg.contains("KeyPress"));
        assert!(key_dbg.contains("repeat=true"));

        let key_release = Event::new(EventKind::KeyRelease {
            key: 0x41,
            modifiers: 0,
        });
        let rel_dbg = format!("{:?}", key_release.kind);
        assert!(rel_dbg.contains("KeyRelease"));

        let resize = Event::new(EventKind::Resize {
            width: 800,
            height: 600,
            old_width: 640,
            old_height: 480,
        });
        let res_dbg = format!("{:?}", resize.kind);
        assert!(res_dbg.contains("Resize(size=800x600, old=640x480)"));

        let focus_in = Event::new_spontaneous(EventKind::FocusIn {
            reason: FocusReason::ActiveWindow,
        });
        let focus_out = Event::new_spontaneous(EventKind::FocusOut {
            reason: FocusReason::Other,
        });
        assert_eq!(format!("{:?}", focus_in.kind), "FocusIn(ActiveWindow)");
        assert_eq!(format!("{:?}", focus_out.kind), "FocusOut(Other)");
    }
}
