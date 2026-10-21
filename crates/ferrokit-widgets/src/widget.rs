// SPDX-License-Identifier: LGPL-3.0-only
use crate::accessibility::{AccessibleAction, AccessibleRole};
use crate::style::stylesheet::QStyleSheetStyle;
use crate::focus::FocusPolicy;
use crate::layout::Layout;
use crate::layout_engine::{owner_layout_margin, WIDGET_LAYOUT_SPACING, WIDGET_SIZE_MAX};
pub use crate::size_policy::{Policy, QSizePolicy};
use ferrokit_core::event::{Event, EventKind, FocusReason};
use ferrokit_core::event_loop::{filters_consume, notify_borrowed, post_event_to_thread};
use ferrokit_core::object::{ObjectData, ObjectId, QObject};
use ferrokit_gui::geometry::primitives::{Point, Rect, RectF, Size};
use ferrokit_gui::paint::Painter;
use std::cell::{Cell, RefCell, RefMut};
use std::rc::{Rc, Weak};

pub type WidgetRef = Rc<RefCell<Box<dyn Widget>>>;
pub type WidgetWeak = Weak<RefCell<Box<dyn Widget>>>;

/// `QWidget::setEnabled`, the only way to enable or disable a widget: it records the explicit
/// state, passes the effective one down the tree (qwidget.cpp:3405-3476) and sends the events
/// before it returns, in Qt's order: each widget whose state changed gets `EnabledChange` after
/// those below it, and a disabled focus widget gets `FocusOut` (and the next one `FocusIn`)
/// before its own `EnabledChange`. A widget is not enabled under a disabled parent, and a child
/// disabled explicitly stays disabled when its parent is enabled again. The focus move waits for
/// the dispatcher's next event while a widget of the window is still borrowed, and a widget
/// still borrowed by the caller gets no `EnabledChange`.
pub fn set_widget_enabled(widget: &WidgetRef, enabled: bool) {
    let mut notices = Vec::new();
    let changed = {
        let w = widget.borrow();
        let base = w.widget_base();
        base.force_disabled.set(!enabled);
        base.set_enabled_helper(enabled, &mut notices)
    };
    if changed {
        notices.push(EnabledNotice::Changed(widget.clone()));
    }
    for notice in notices {
        match notice {
            EnabledNotice::FocusLost(state) => crate::focus::move_lost_focus_now(&state),
            EnabledNotice::Changed(w) => {
                if let Ok(mut w) = w.try_borrow_mut() {
                    notify_borrowed(&mut **w, &mut Event::new(EventKind::EnabledChange));
                }
            }
        }
    }
}

/// What `set_enabled_helper` did, in Qt's order, for `set_widget_enabled` to send the events for.
pub(crate) enum EnabledNotice {
    /// The focus widget was disabled; its window's focus is to move on.
    FocusLost(Rc<crate::focus::FocusState>),
    /// The widget's effective state changed (`QEvent::EnabledChange`).
    Changed(WidgetRef),
}

/// `QWidget::setVisible` for a widget its owner holds: records the flag, then sends the events the
/// change of shown state implies (qwidget.cpp:8401-8443). Showing sends `Show` to the children
/// first, then to the widget (`show_helper`, qwidget.cpp:8074-8130); hiding sends `Hide` to the
/// widget first, then to its children (`hide_helper`, qwidget.cpp:8238-8289). A change that leaves
/// the widget's shown state as it was sends nothing, so a child of a hidden window gets no event.
/// As in `set_widget_enabled`, a widget still borrowed by the caller gets no event.
pub fn set_widget_visible(widget: &WidgetRef, visible: bool) {
    let was_shown = is_shown(widget);
    widget.borrow().set_visible(visible);
    let now_shown = is_shown(widget);
    if now_shown && !was_shown {
        show_children_first(widget);
        if let Ok(mut w) = widget.try_borrow_mut() {
            notify_borrowed(&mut **w, &mut Event::new(EventKind::Show));
        }
    } else if was_shown && !now_shown {
        if let Ok(mut w) = widget.try_borrow_mut() {
            notify_borrowed(&mut **w, &mut Event::new(EventKind::Hide));
        }
        hide_children(widget);
    }
}

/// `QWidget::isVisible`: the widget and its ancestors are shown, and so is the window it belongs
/// to. A widget that belongs to no window counts as shown when it is visible.
fn is_shown(widget: &WidgetRef) -> bool {
    let w = widget.borrow();
    let base = w.widget_base();
    if !base.is_visible_in_tree() {
        return false;
    }
    match base.window_id() {
        Some(id) => crate::application::Application::is_window_visible(id),
        None => true,
    }
}

/// `show_helper`'s child loop (qwidget.cpp:8074-8130): every child that is not explicitly hidden
/// gets its `Show` after its own children, before the parent's.
pub(crate) fn show_children_first(widget: &WidgetRef) {
    let children = widget.borrow().children();
    for child in children {
        if child.borrow().is_visible() {
            show_children_first(&child);
            let mut w = child.borrow_mut();
            notify_borrowed(&mut **w, &mut Event::new(EventKind::Show));
        }
    }
}

/// `hide_helper`'s child loop (qwidget.cpp:8238-8289): every child that is not explicitly hidden
/// gets its `Hide` before its own children do.
pub(crate) fn hide_children(widget: &WidgetRef) {
    let children = widget.borrow().children();
    for child in children {
        if child.borrow().is_visible() {
            {
                let mut w = child.borrow_mut();
                notify_borrowed(&mut **w, &mut Event::new(EventKind::Hide));
            }
            hide_children(&child);
        }
    }
}

/// Points each of `children` at `parent`, so ancestor-dependent lookups (the style sheet
/// cascade) can walk upwards.
///
/// A widget cannot name its own `Rc`, and `add_child` / `Layout::add_widget` only receive the
/// child, so the link is made by whoever holds the owner's `WidgetRef`: layout activation
/// (`adopt_tree`) and painting.
pub fn adopt_children(parent: &WidgetRef, children: &[WidgetRef]) {
    for child in children {
        let Ok(c) = child.try_borrow() else { continue };
        let linked = c
            .parent_widget()
            .and_then(|w| w.upgrade())
            .is_some_and(|owner| Rc::ptr_eq(&owner, parent));
        if !linked {
            c.set_parent_widget(Some(Rc::downgrade(parent)));
        }
    }
}

/// `adopt_children` for every widget below `root`.
pub fn adopt_tree(root: &WidgetRef) {
    let children = match root.try_borrow() {
        Ok(w) => w.children(),
        Err(_) => return,
    };
    adopt_children(root, &children);
    for child in &children {
        adopt_tree(child);
    }
}

/// The minimum and maximum `QLayout::activate` gives the owner of top-level layout `layout`
/// (qlayout.cpp:1006-1095); `None` leaves that bound alone. `window` is `mw->isWindow()`;
/// `explicit_min` and `current_min` are the owner's explicit minimum axes and current minimum,
/// which `SetDefaultConstraint` keeps. The layout's total sizes are its own sizes: ferrokit widgets
/// have no contents margins or menu bar to add (qlayout.cpp:615-678).
pub(crate) fn size_constraint_bounds(
    layout: &dyn Layout,
    window: bool,
    explicit_min: (bool, bool),
    current_min: Size,
) -> (Option<Size>, Option<Size>) {
    use crate::layout::SizeConstraint::*;
    // `QLayout::totalSizeHint`: a height-for-width layout's hint height is its height at the hint
    // width (qlayout.cpp:648-650).
    let total_size_hint = || {
        let hint = layout.size_hint();
        if layout.has_height_for_width() {
            Size::new(hint.width, layout.height_for_width(hint.width))
        } else {
            hint
        }
    };
    let total_maximum_size = || {
        let max = layout.maximum_size();
        Size::new(
            max.width.min(crate::layout_engine::LAYOUT_SIZE_MAX),
            max.height.min(crate::layout_engine::LAYOUT_SIZE_MAX),
        )
    };
    match layout.size_constraint() {
        SetFixedSize => {
            let hint = total_size_hint();
            (Some(hint), Some(hint))
        }
        SetMinimumSize => (Some(layout.minimum_size()), None),
        SetMaximumSize => (None, Some(total_maximum_size())),
        SetMinAndMaxSize => (Some(layout.minimum_size()), Some(total_maximum_size())),
        SetDefaultConstraint => {
            let derived = if window {
                layout.minimum_size()
            } else {
                Size::new(0, 0)
            };
            let width = if explicit_min.0 {
                current_min.width
            } else {
                derived.width
            };
            let height = if explicit_min.1 {
                current_min.height
            } else {
                derived.height
            };
            (Some(Size::new(width, height)), None)
        }
        SetNoConstraint => (None, None),
    }
}

/// The bounding step of `QLayout::activate` (qlayout.cpp:996-1129) for `owner`'s top-level layout:
/// the size constraint's minimum and maximum go through `set_minimum_size` / `set_maximum_size`
/// (`set_fixed_size` when they are equal), which grow or shrink `owner`, and the explicit flags
/// are put back afterwards, so the bounds stay the layout's. A widget without a parent is a window
/// (`isWindow`). The root of a native [`Window`](crate::window::Window) is bounded by the window
/// instead (`Window::sync_top_level_minimum`), since its size is the native window's. The holder of
/// a nested layout is not an owner (`!topLevel`, qlayout.cpp:985-986).
pub fn apply_size_constraint<W: Widget + ?Sized>(owner: &W) {
    let window = owner.parent_widget().is_none();
    if window && owner.window_id().is_some() {
        return;
    }
    let base = owner.widget_base();
    let (explicit_min, explicit_max) = (base.explicit_min.get(), base.explicit_max.get());
    let (min, max) = {
        let Some(layout) = owner.layout_ref_mut() else {
            return;
        };
        if layout.is_nested() {
            return;
        }
        size_constraint_bounds(&**layout, window, explicit_min, base.min_size.get())
    };
    match (min, max) {
        (Some(min), Some(max)) if min == max => owner.set_fixed_size(min),
        _ => {
            if let Some(min) = min {
                owner.set_minimum_size(min);
            }
            if let Some(max) = max {
                owner.set_maximum_size(max);
            }
        }
    }
    base.explicit_min.set(explicit_min);
    base.explicit_max.set(explicit_max);
}

/// Sends `StyleChange` (`WidgetBase::style_changed`) to every widget below `children`, depth
/// first: `QStyleSheetStyle::updateObjects` (qstylesheetstyle.cpp:2780).
///
/// A widget that is mutably borrowed is the one currently being driven; it is skipped, as
/// `resolve_style` also stops at it. Its caller is the one changing it. A widget that is only
/// shared-borrowed still gets its layout invalidated, but no `StyleChange` event: `event()` needs
/// `&mut`, so the event is not delivered to it (a known limitation).
pub fn style_changed_below(children: Vec<WidgetRef>) {
    for child in children {
        if let Ok(mut widget) = child.try_borrow_mut() {
            notify_borrowed(&mut **widget, &mut Event::new(EventKind::StyleChange));
            widget.widget_base().style_changed();
            style_changed_below(widget.children());
        } else if let Ok(widget) = child.try_borrow() {
            widget.widget_base().style_changed();
            style_changed_below(widget.children());
        }
    }
}

/// Strongly typed `Qt::WidgetAttribute` matching Qt `qnamespace.h:300-439`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum WidgetAttribute {
    Disabled = 0,
    UnderMouse = 1,
    MouseTracking = 2,
    OpaquePaintEvent = 4,
    StaticContents = 5,
    LaidOut = 7,
    PaintOnScreen = 8,
    NoSystemBackground = 9,
    UpdatesDisabled = 10,
    Mapped = 11,
    InputMethodEnabled = 14,
    ForceDisabled = 32,
    CustomWhatsThis = 47,
    LayoutOnEntireRect = 48,
    TransparentForMouseEvents = 51,
    PaintUnclipped = 52,
    NoMousePropagation = 73,
    Hover = 74,
    QuitOnClose = 76,
    KeyboardFocusChange = 77,
    AcceptDrops = 78,
    AlwaysShowToolTips = 84,
    StyledBackground = 93,
    ShowWithoutActivating = 98,
    NativeWindow = 100,
    TranslucentBackground = 120,
    AcceptTouchEvents = 121,
    AlwaysStackOnTop = 128,
    TabletTracking = 129,
}

pub trait Widget: QObject + 'static {
    /// The `WidgetBase` holding this widget's shared state.
    ///
    /// Size policy, style sheet and style properties live there, and changing them has to enter
    /// the repaint/relayout protocol (`WidgetBase::update_geometry`, `style_changed`). Having one
    /// accessor instead of per-type forwarding makes it impossible for a widget type to accept
    /// such a change and silently drop it.
    fn widget_base(&self) -> &WidgetBase;

    fn id(&self) -> ObjectId;

    fn geometry(&self) -> Rect;

    fn set_geometry(&self, rect: Rect);
    fn size_hint(&self) -> Size {
        Size::new(-1, -1)
    }

    fn minimum_size(&self) -> Size {
        self.widget_base().min_size.get()
    }

    fn minimum_size_hint(&self) -> Size {
        Size::new(-1, -1)
    }
    fn maximum_size(&self) -> Size {
        self.widget_base().max_size.get()
    }

    /// `QWidget::setMinimumSize` (qwidget.cpp:4005-4028): stores the bound, marks each non-zero axis
    /// explicit (qwidget.cpp:3971), runs `updateGeometry`, and grows the widget to the bound if it
    /// is currently smaller. An unchanged bound is a no-op (qwidget.cpp:3967-3968).
    fn set_minimum_size(&self, size: Size) {
        let min = Size::new(
            size.width.clamp(0, WIDGET_SIZE_MAX),
            size.height.clamp(0, WIDGET_SIZE_MAX),
        );
        let base = self.widget_base();
        if base.min_size.get() == min {
            return;
        }
        base.min_size.set(min);
        base.explicit_min.set((min.width != 0, min.height != 0));
        self.update_geometry();
        let g = self.geometry();
        if min.width > g.width || min.height > g.height {
            self.set_geometry(Rect::new(
                g.x,
                g.y,
                g.width.max(min.width),
                g.height.max(min.height),
            ));
        }
    }

    /// `QWidget::setMaximumSize` (qwidget.cpp:4065-4087): stores the bound (negatives clamp to 0),
    /// marks each bounded axis explicit (qwidget.cpp:4053-4054), runs `updateGeometry`, and shrinks
    /// the widget to the bound if it is currently larger. An unchanged bound is a no-op.
    fn set_maximum_size(&self, size: Size) {
        let max = Size::new(
            size.width.clamp(0, WIDGET_SIZE_MAX),
            size.height.clamp(0, WIDGET_SIZE_MAX),
        );
        let base = self.widget_base();
        if base.max_size.get() == max {
            return;
        }
        base.max_size.set(max);
        base.explicit_max
            .set((max.width != WIDGET_SIZE_MAX, max.height != WIDGET_SIZE_MAX));
        self.update_geometry();
        let g = self.geometry();
        if max.width < g.width || max.height < g.height {
            self.set_geometry(Rect::new(
                g.x,
                g.y,
                g.width.min(max.width),
                g.height.min(max.height),
            ));
        }
    }

    /// `QWidget::setFixedSize` (qwidget.cpp:4155-4170): sets the minimum and maximum to `size` and
    /// resizes to it. The size policy is not changed.
    fn set_fixed_size(&self, size: Size) {
        self.set_minimum_size(size);
        self.set_maximum_size(size);
    }

    /// `QWidget::hasHeightForWidth` (qwidget.cpp:10495-10499): the layout's answer when the widget
    /// has a layout, else the size policy's `heightForWidth` flag.
    fn has_height_for_width(&self) -> bool {
        match self.widget_base().layout.borrow().as_ref() {
            Some(layout) => layout.has_height_for_width(),
            None => self.size_policy().height_for_width,
        }
    }

    /// `QWidget::heightForWidth` (qwidget.cpp:10482-10487): the layout's answer when the widget has
    /// a layout, else -1.
    fn height_for_width(&self, width: i32) -> i32 {
        match self.widget_base().layout.borrow().as_ref() {
            Some(layout) => layout.height_for_width(width),
            None => -1,
        }
    }

    fn size_policy(&self) -> QSizePolicy {
        self.widget_base().size_policy()
    }

    /// `QWidget::setSizePolicy`: stores the policy and, if it changed, runs `updateGeometry`.
    fn set_size_policy(&self, policy: QSizePolicy) {
        self.widget_base().set_size_policy(policy);
    }

    /// `QWidget::updateGeometry`: this widget's size hint or policy changed, so the parent's
    /// layout has to run again.
    fn update_geometry(&self) {
        self.widget_base().update_geometry();
    }
    fn is_visible(&self) -> bool;

    fn set_visible(&self, visible: bool);
    fn is_enabled(&self) -> bool;

    fn update(&self);
    /// `QWidget::update(const QRect &)` (`qwidget.cpp:11340`).
    fn update_rect(&self, rect: Rect) {
        self.widget_base().update_rect(rect);
    }
    fn dirty_rect(&self) -> Option<Rect>;

    fn clear_dirty(&self);

    fn layout(&self) -> Option<&dyn Layout> {
        None
    }

    fn layout_mut(&mut self) -> Option<&mut Box<dyn Layout>> {
        None
    }

    fn layout_ref_mut(&self) -> Option<RefMut<'_, Box<dyn Layout>>> {
        None
    }

    fn set_layout(&mut self, _layout: Box<dyn Layout>) {}

    /// `QWidget::isActiveWindow` (qwidget.cpp:6967): the window this widget belongs to is
    /// `QApplication::activeWindow()`. A widget tree that belongs to no `Window` is not.
    fn is_active_window(&self) -> bool {
        let mut id = self.window_id();
        let mut parent = self.parent_widget();
        while id.is_none() {
            let Some(p) = parent.and_then(|p| p.upgrade()) else {
                return false;
            };
            let Ok(p) = p.try_borrow() else {
                return false;
            };
            id = p.window_id();
            parent = p.parent_widget();
        }
        id.is_some() && id == crate::application::Application::active_window()
    }

    /// `QLayout::activate` on this widget's top-level layout (qlayout.cpp:980-1133): the layout's
    /// size constraint bounds this widget first ([`apply_size_constraint`]), then the layout runs on
    /// the resulting rectangle. Child widgets with layouts follow.
    fn update_layout(&self) {
        apply_size_constraint(self);
        let g = self.geometry();
        if let Some(mut layout) = self.layout_ref_mut() {
            layout.set_default_margin(owner_layout_margin(self.parent_widget().is_some()));
            layout.set_geometry(Rect::new(0, 0, g.width, g.height));
            layout.activate();
        }
        for child in self.children() {
            if child.borrow().layout_ref_mut().is_some() {
                child.borrow().update_layout();
            }
        }
        crate::layout_scheduler::LayoutScheduler::activate_pending();
    }
    fn parent_widget(&self) -> Option<WidgetWeak>;

    fn set_parent_widget(&self, parent: Option<WidgetWeak>);
    fn window_id(&self) -> Option<ObjectId>;

    fn set_window_id(&self, window_id: Option<ObjectId>);
    fn children(&self) -> Vec<WidgetRef>;

    fn add_child(&mut self, child: WidgetRef);

    fn remove_child(&mut self, child_id: ObjectId);

    /// Requests a deferred layout update for this widget or its parent container.
    fn request_layout(&self) {
        if let Some(parent) = self.parent_widget() {
            crate::command::WidgetCommandQueue::post_layout_weak(&parent);
        }
    }

    /// Virtual paint event handler (`QWidget::paintEvent` equivalent).
    ///
    /// Called when the widget needs to repaint its content.
    /// The painter is pre-translated to the widget's local coordinates `(0, 0)`.
    fn paint_event(&mut self, _painter: &mut Painter) {}

    /// The part of this widget (its own coordinates) inside which its children are drawn; `None`
    /// is the whole widget. A scroll area returns its viewport: `QScrollArea::setWidget`
    /// reparents the content to the viewport (qscrollarea.cpp:242-243), so it paints clipped to
    /// it (`QWidgetPrivate::clipRect`, qwidget.cpp:1904-1922).
    fn children_clip_rect(&self) -> Option<Rect> {
        None
    }

    /// Paints after the children, on top of them. A scroll area draws its scroll bars here: they
    /// are siblings of the viewport created after it (qabstractscrollarea.cpp:254, 258, 266), so
    /// they stack above it.
    fn paint_over_children(&mut self, _painter: &mut Painter) {}

    fn mouse_press_event(&mut self, _pos: Point, _button: u32, _modifiers: u32) {}

    fn mouse_release_event(&mut self, _pos: Point, _button: u32, _modifiers: u32) {}

    fn mouse_move_event(&mut self, _pos: Point) {}

    fn enter_event(&mut self, _pos: Point) {}

    fn leave_event(&mut self) {}

    fn wheel_event(&mut self, _pos: Point, _delta_y: i32, _modifiers: u32) {}

    fn resize_event(&self, _new_size: Size, _old_size: Size) {}

    fn focus_policy(&self) -> FocusPolicy {
        FocusPolicy::NoFocus
    }

    fn set_focus_policy(&self, _policy: FocusPolicy) {}
    fn has_focus(&self) -> bool {
        false
    }

    fn set_has_focus(&self, _focus: bool) {}
    fn accessible_role(&self) -> AccessibleRole {
        AccessibleRole::Custom
    }

    fn accessible_name(&self) -> String {
        QObject::object_name(self).unwrap_or_default().to_owned()
    }

    fn accessible_value(&self) -> Option<String> {
        None
    }

    fn accessible_actions(&self) -> Vec<AccessibleAction> {
        Vec::new()
    }
    fn perform_accessible_action(&mut self, _action: AccessibleAction) -> bool {
        false
    }

    fn focus_in_event(&mut self, _reason: FocusReason) {}

    fn focus_out_event(&mut self, _reason: FocusReason) {}

    fn key_press_event(&mut self, _key: u32, _modifiers: u32, _is_repeat: bool) {}

    fn key_release_event(&mut self, _key: u32, _modifiers: u32) {}

    /// Called when the screen DPI changes during Per-Monitor V2 dynamic dragging or display reconfiguration.
    /// Widgets should invalidate cached metrics, size hints, and layouts.
    fn dpi_changed_event(&mut self, _old_dpr: f32, _new_dpr: f32) {
        self.update();
    }
    fn as_any(&self) -> &dyn std::any::Any;

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;

    /// `QWidget::setStyleSheet`: the sheet applies to this widget and its subtree, so a
    /// `StyleChange` (repaint, `updateGeometry`, own layout invalid) goes to this widget and to
    /// every descendant, as `QStyleSheetStyle::repolish(w)` does (`updateObjects`,
    /// qstylesheetstyle.cpp:2780).
    fn set_style_sheet(&self, qss: &str) {
        self.widget_base().set_style_sheet(qss);
        // This widget gets its StyleChange through `&self`, so only its filters see it here (the
        // widget's own `event()` needs `&mut`; a known limitation).
        filters_consume(self.object_data(), &mut Event::new(EventKind::StyleChange));
        style_changed_below(self.children());
    }

    /// This widget's own style sheet (not its ancestors').
    fn style_sheet(&self) -> Option<QStyleSheetStyle> {
        self.widget_base().style_sheet.borrow().clone()
    }

    /// Sets a style property (`QObject::setProperty` on a dynamic property).
    ///
    /// As in Qt, this alone does not restyle the widget: a `[state="x"]` selector is only
    /// re-evaluated when the widget is polished again, so call [`repolish`](Self::repolish)
    /// afterwards (Python does `style().unpolish(w); style().polish(w)`).
    fn set_property(&self, name: &str, value: &str) {
        self.widget_base().set_property(name, value);
    }

    /// Unpolish then polish through the widget's style: re-evaluates the style sheet, which Qt
    /// reports as a `StyleChange` (repaint, geometry update, layout invalidation).
    fn repolish(&self) {
        self.widget_base().style_changed();
        filters_consume(self.object_data(), &mut Event::new(EventKind::StyleChange));
    }

    fn property(&self, name: &str) -> Option<String> {
        self.widget_base().property(name)
    }

    /// `QWidget::toolTip`: the text shown when the cursor rests on the widget. Empty means none.
    fn tool_tip(&self) -> String {
        self.widget_base().tool_tip.borrow().clone()
    }

    /// `QWidget::setToolTip` (qwidget.cpp:11793-11800): stores the text, then always sends
    /// `ToolTipChange`, even when the text is unchanged. The event goes through the object
    /// filters and then [`event`](QObject::event) like `FontChange` does in `Button::set_font`,
    /// and needs `&mut self` for that, so no shared borrow of the widget is alive while it runs.
    ///
    /// Calling it from a `ToolTipChange` handler with a different text sends the event again and
    /// recurses, as in Qt; the handler has to stop on its own.
    fn set_tool_tip(&mut self, text: &str) {
        *self.widget_base().tool_tip.borrow_mut() = text.to_string();
        notify_borrowed(self, &mut Event::new(EventKind::ToolTipChange));
    }

    /// `QWidget::toolTipDuration`: how long the tip stays, in ms; -1 (the default) derives it from
    /// the text length.
    fn tool_tip_duration(&self) -> i32 {
        self.widget_base().tool_tip_duration.get()
    }

    fn set_tool_tip_duration(&self, msec: i32) {
        self.widget_base().tool_tip_duration.set(msec);
    }

    /// `Qt::WA_AlwaysShowToolTips` on a window widget: its widgets show tool tips even when the
    /// window is not the active one (`QApplication::event`, qapplication.cpp:1710-1735).
    fn always_show_tool_tips(&self) -> bool {
        self.widget_base().always_show_tool_tips.get()
    }
    fn set_always_show_tool_tips(&self, on: bool) {
        self.widget_base().always_show_tool_tips.set(on);
    }

    /// `QWidget::testAttribute` (`qwidget.cpp:11431-11445`).
    fn test_attribute(&self, attr: WidgetAttribute) -> bool {
        self.widget_base().test_attribute(attr)
    }

    /// `QWidget::setAttribute` (`qwidget.cpp:11432-11480`).
    fn set_attribute(&self, attr: WidgetAttribute, on: bool) {
        self.widget_base().set_attribute(attr, on);
    }
}

pub struct WidgetBase {
    pub object_data: ObjectData,
    pub geometry: Cell<Rect>,
    pub visible: Cell<bool>,
    /// Effective state (`!WA_Disabled`): false when this widget or an ancestor is disabled.
    pub enabled: Cell<bool>,
    /// Disabled explicitly with `set_enabled(false)` (`WA_ForceDisabled`).
    pub force_disabled: Cell<bool>,
    pub dirty: Cell<Option<Rect>>,
    pub parent: RefCell<Option<WidgetWeak>>,
    pub window_id: Cell<Option<ObjectId>>,
    pub children: RefCell<Vec<WidgetRef>>,
    pub layout: RefCell<Option<Box<dyn Layout>>>,
    pub focus_policy: Cell<FocusPolicy>,
    pub has_focus: Cell<bool>,
    /// The focus slot of the window that last gave this widget focus (`FocusManager`).
    pub(crate) focus_state: RefCell<Weak<crate::focus::FocusState>>,
    pub size_policy: Cell<QSizePolicy>,
    pub style_sheet: RefCell<Option<QStyleSheetStyle>>,
    pub properties: RefCell<Vec<(String, String)>>,
    pub tool_tip: RefCell<String>,
    pub tool_tip_duration: Cell<i32>,
    pub always_show_tool_tips: Cell<bool>,
    /// `WA_MouseTracking`: buttonless mouse moves are delivered to this widget.
    pub mouse_tracking: Cell<bool>,
    /// `WA_TransparentForMouseEvents` (`QWidget::testAttribute(Qt::WA_TransparentForMouseEvents)`).
    pub transparent_for_mouse_events: Cell<bool>,
    /// `WA_NoMousePropagation` (`QWidget::testAttribute(Qt::WA_NoMousePropagation)`).
    pub no_mouse_propagation: Cell<bool>,
    /// `QWidget::minimumSize` / `maximumSize` (`setMinimumSize`, `setMaximumSize`, and the owner's
    /// layout through its size constraint).
    pub min_size: Cell<Size>,
    pub max_size: Cell<Size>,
    /// `QWidgetPrivate::extra->explicitMinSize` / `explicitMaxSize` (qwidget_p.h:156-157) as
    /// (horizontal, vertical): the axes the user bounded. A bound the owner's layout sets is not
    /// explicit (qlayout.cpp:1126-1129).
    pub explicit_min: Cell<(bool, bool)>,
    pub explicit_max: Cell<(bool, bool)>,
    /// The deferral epoch of `LayoutScheduler` this tree root last scanned for layouts left dirty
    /// (`LayoutScheduler::flush_deferred`).
    pub(crate) layout_epoch: Cell<u64>,
}

impl WidgetBase {
    pub fn new() -> Self {
        let id = ObjectId::next();
        Self {
            object_data: ObjectData::new(id),
            geometry: Cell::new(Rect::new(0, 0, 100, 30)),
            visible: Cell::new(true),
            enabled: Cell::new(true),
            force_disabled: Cell::new(false),
            dirty: Cell::new(Some(Rect::new(0, 0, 100, 30))),
            parent: RefCell::new(None),
            window_id: Cell::new(None),
            children: RefCell::new(Vec::new()),
            layout: RefCell::new(None),
            focus_policy: Cell::new(FocusPolicy::NoFocus),
            has_focus: Cell::new(false),
            focus_state: RefCell::new(Weak::new()),
            size_policy: Cell::new(QSizePolicy::default()),
            style_sheet: RefCell::new(None),
            properties: RefCell::new(Vec::new()),
            tool_tip: RefCell::new(String::new()),
            tool_tip_duration: Cell::new(-1),
            always_show_tool_tips: Cell::new(false),
            mouse_tracking: Cell::new(false),
            transparent_for_mouse_events: Cell::new(false),
            no_mouse_propagation: Cell::new(false),
            min_size: Cell::new(Size::new(0, 0)),
            max_size: Cell::new(Size::new(WIDGET_SIZE_MAX, WIDGET_SIZE_MAX)),
            explicit_min: Cell::new((false, false)),
            explicit_max: Cell::new((false, false)),
            layout_epoch: Cell::new(0),
        }
    }
    /// Unique object identifier.
    #[inline]
    pub fn id(&self) -> ObjectId {
        self.object_data.id()
    }


    pub fn with_geometry(geometry: Rect) -> Self {
        let base = Self::new();
        base.geometry.set(geometry);
        base.dirty.set(Some(Rect::new(0, 0, geometry.width, geometry.height)));
        base
    }

    pub fn geometry(&self) -> Rect {
        self.geometry.get()
    }

    pub fn set_geometry(&self, rect: Rect) {
        let min = self.min_size.get();
        let max = self.max_size.get();
        // `QWidgetPrivate::setGeometry_sys` (qwidget.cpp:7301-7304):
        // Width and height are first capped at the maximum, then raised to the minimum.
        // Minimum wins if min > max.
        let width = rect.width.min(max.width).max(min.width);
        let height = rect.height.min(max.height).max(min.height);
        let clamped = Rect::new(rect.x, rect.y, width, height);
        let old = self.geometry.get();
        if old != clamped {
            self.geometry.set(clamped);
            self.invalidate_vacated_rect(old);
            self.update();
        }
    }

    /// Repaints the vacated footprint `old` (parent coordinates) of a visible widget that moved or
    /// resized: the parent's dirty region gets it. `QWidgetPrivate::setGeometry_sys`
    /// (qwidget.cpp:7297-7410) calls `moveRect` (:7380) or `invalidateBackingStore_resizeHelper`
    /// (:7382) on the old position; `moveRect` subtracts the new rect from the parent's region
    /// (qwidgetrepaintmanager.cpp:411-419). Only the parent is invalidated. A parent that is
    /// mutably borrowed on the current stack is skipped; its own layout or update pass repaints it.
    pub fn invalidate_vacated_rect(&self, old: Rect) {
        if old.is_empty() || !self.is_visible_in_tree() {
            return;
        }
        let Some(parent) = self.parent_widget().and_then(|w| w.upgrade()) else {
            return;
        };
        let parent_ref = parent.try_borrow();
        if let Ok(p) = parent_ref {
            p.widget_base().update_rect(old);
        }
    }

    pub fn is_visible(&self) -> bool {
        self.visible.get()
    }

    pub fn set_visible(&self, visible: bool) {
        if self.visible.get() == visible {
            return;
        }
        if !visible {
            // `hide_sys` repaints the area the widget leaves while it still counts as visible
            // (qwidget.cpp:8312-8319); once hidden, `update` does nothing.
            self.update();
        }
        self.visible.set(visible);
        // `QWidget::setVisible` invalidates the parent layout (qwidget.cpp:8465-8468); showing
        // repaints the widget (`show_sys`).
        self.update_geometry();
    }

    /// `QWidget::isVisible`: this widget and every ancestor are shown. An ancestor that is
    /// mutably borrowed right now (it is driving this call) counts as shown.
    fn is_visible_in_tree(&self) -> bool {
        if !self.visible.get() {
            return false;
        }
        let mut parent = self.parent_widget().and_then(|p| p.upgrade());
        while let Some(p) = parent {
            let Ok(p) = p.try_borrow() else {
                return true;
            };
            if !p.is_visible() {
                return false;
            }
            parent = p.widget_base().parent_widget().and_then(|w| w.upgrade());
        }
        true
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.get()
    }

    /// `QWidget::setMouseTracking` (`WA_MouseTracking`).
    pub fn set_mouse_tracking(&self, on: bool) {
        self.mouse_tracking.set(on);
    }

    pub fn has_mouse_tracking(&self) -> bool {
        self.mouse_tracking.get()
    }

    /// `QWidget::setAttribute(Qt::WA_TransparentForMouseEvents)`.
    pub fn set_transparent_for_mouse_events(&self, on: bool) {
        self.transparent_for_mouse_events.set(on);
    }

    pub fn is_transparent_for_mouse_events(&self) -> bool {
        self.transparent_for_mouse_events.get()
    }

    /// `QWidget::setAttribute(Qt::WA_NoMousePropagation)`.
    pub fn set_no_mouse_propagation(&self, on: bool) {
        self.no_mouse_propagation.set(on);
    }

    pub fn has_no_mouse_propagation(&self) -> bool {
        self.no_mouse_propagation.get()
    }

    /// `QWidget::testAttribute`.
    pub fn test_attribute(&self, attr: WidgetAttribute) -> bool {
        match attr {
            WidgetAttribute::Disabled => !self.enabled.get(),
            WidgetAttribute::MouseTracking => self.mouse_tracking.get(),
            WidgetAttribute::TransparentForMouseEvents => self.transparent_for_mouse_events.get(),
            WidgetAttribute::NoMousePropagation => self.no_mouse_propagation.get(),
            WidgetAttribute::AlwaysShowToolTips => self.always_show_tool_tips.get(),
            _ => false,
        }
    }

    /// `QWidget::setAttribute`.
    pub fn set_attribute(&self, attr: WidgetAttribute, on: bool) {
        match attr {
            WidgetAttribute::Disabled => {
                let mut notices = Vec::new();
                self.force_disabled.set(on);
                self.set_enabled_helper(!on, &mut notices);
            }
            WidgetAttribute::MouseTracking => {
                self.mouse_tracking.set(on);
            }
            WidgetAttribute::TransparentForMouseEvents => {
                self.transparent_for_mouse_events.set(on);
            }
            WidgetAttribute::NoMousePropagation => {
                self.no_mouse_propagation.set(on);
            }
            WidgetAttribute::AlwaysShowToolTips => {
                self.always_show_tool_tips.set(on);
            }
            _ => {}
        }
    }

    /// QSS pseudo-states of the widget's state, as `pseudoClass(QStyle::State)` derives them
    /// (qstylesheetstyle.cpp:1756-1773): `:enabled` and `:hover` only while enabled, `:disabled`
    /// otherwise, `:pressed` for a sunken widget and `:focus` while it has keyboard focus.
    pub fn style_pseudo_states<'a>(
        &self,
        hover: bool,
        pressed: bool,
        buf: &'a mut [&'static str; 4],
    ) -> &'a [&'static str] {
        let enabled = self.is_enabled();
        let mut n = 0;
        for (on, name) in [
            (enabled, "enabled"),
            (enabled && hover, "hover"),
            (!enabled, "disabled"),
            (pressed, "pressed"),
            (self.has_focus(), "focus"),
        ] {
            if on {
                buf[n] = name;
                n += 1;
            }
        }
        &buf[..n]
    }

    /// `QWidgetPrivate::setEnabled_helper`. Returns whether the state changed; `notices` collects
    /// the events to send once the widgets are released.
    fn set_enabled_helper(&self, enable: bool, notices: &mut Vec<EnabledNotice>) -> bool {
        let parent_disabled = self
            .parent_widget()
            .and_then(|p| p.upgrade())
            .is_some_and(|p| p.try_borrow().is_ok_and(|p| !p.is_enabled()));
        if (enable && parent_disabled) || enable == self.enabled.get() {
            return false;
        }
        self.enabled.set(enable);
        // Disabling the focus widget takes the focus away (qwidget.cpp:3442-3446). The flag and the
        // window's focus id change now; the events follow in `set_widget_enabled` or, when it
        // could not send them, `FocusManager::process_pending`.
        if !enable {
            if let Some(focus) = self.focus_state.borrow().upgrade() {
                if focus.lose(self.object_data.id(), !parent_disabled) {
                    self.has_focus.set(false);
                    notices.push(EnabledNotice::FocusLost(focus));
                }
            }
        }
        // The children are this widget's own and those of its layout, as `EmptyWidget::children`.
        let mut children = self.children.borrow().clone();
        if let Some(layout) = self.layout.borrow().as_ref() {
            children.extend(layout.widgets());
        }
        for child_ref in children {
            let Ok(child) = child_ref.try_borrow() else { continue };
            let base = child.widget_base();
            // Enabling skips explicitly disabled children; disabling skips disabled ones.
            let skip = if enable { base.force_disabled.get() } else { !base.enabled.get() };
            if !skip && base.set_enabled_helper(enable, notices) {
                notices.push(EnabledNotice::Changed(child_ref.clone()));
            }
        }
        // `QWidget::changeEvent(EnabledChange)` repaints (qwidget.cpp:9491-9492).
        self.update();
        true
    }

    pub fn update(&self) {
        let g = self.geometry.get();
        self.update_rect(Rect::new(0, 0, g.width, g.height));
    }

    /// `QWidget::update(const QRect &)`: adds `rect` (widget coordinates) to the pending dirty
    /// region. Pending dirt accumulates; a smaller rect never replaces a larger pending one.
    pub fn update_rect(&self, rect: Rect) {
        // `QWidgetPrivate::update` ignores a widget that is not visible (qwidget.cpp:11367-11368).
        if !self.is_visible_in_tree() {
            return;
        }
        let pending = match self.dirty.get() {
            Some(d) => d.united(&rect),
            None => rect,
        };
        self.dirty.set(Some(pending));
        let target_receiver = self.window_id.get().unwrap_or(self.object_data.id());
        // Direct path: the window's render state queues its deferred render without a QObject
        // dispatch. Falls back to `UpdateRequest` when this thread has no window render state.
        if crate::window::schedule_widget_update(target_receiver) {
            return;
        }
        let target_thread = ferrokit_core::object::query_object_thread(target_receiver)
            .unwrap_or(self.object_data.thread_id());
        post_event_to_thread(
            target_thread,
            target_receiver,
            Event::new(EventKind::UpdateRequest),
        );
    }

    pub fn dirty_rect(&self) -> Option<Rect> {
        self.dirty.get()
    }

    pub fn clear_dirty(&self) {
        self.dirty.set(None);
    }

    pub fn parent_widget(&self) -> Option<WidgetWeak> {
        self.parent.borrow().clone()
    }

    pub fn set_parent_widget(&self, parent: Option<WidgetWeak>) {
        *self.parent.borrow_mut() = parent;
    }

    pub fn window_id(&self) -> Option<ObjectId> {
        self.window_id.get()
    }

    pub fn set_window_id(&self, window_id: Option<ObjectId>) {
        self.window_id.set(window_id);
    }

    pub fn focus_policy(&self) -> FocusPolicy {
        self.focus_policy.get()
    }

    pub fn set_focus_policy(&self, policy: FocusPolicy) {
        self.focus_policy.set(policy);
    }

    pub fn has_focus(&self) -> bool {
        self.has_focus.get()
    }

    pub fn set_has_focus(&self, focus: bool) {
        self.has_focus.set(focus);
    }

    pub fn size_policy(&self) -> QSizePolicy {
        self.size_policy.get()
    }

    /// `QWidget::setSizePolicy`: nothing happens when the policy is unchanged.
    pub fn set_size_policy(&self, policy: QSizePolicy) {
        if self.size_policy.get() == policy {
            return;
        }
        self.size_policy.set(policy);
        self.update_geometry();
    }

    /// `QWidget::updateGeometry` (`QWidgetPrivate::updateGeometry_helper`, qwidget.cpp:10571):
    /// asks the parent's layout to run again, since this widget's size hint or policy changed.
    /// A widget without a parent has no layout above it, so there is no request to make.
    ///
    /// ferrokit has no `LayoutRequest` event to wake the event loop: queued requests are delivered by
    /// the next render (`WidgetCommandQueue::flush_layouts`). So the request is paired with an
    /// `update()`, which is what schedules that render; without it a size-hint change on an
    /// otherwise idle window would sit in the queue until something else repainted.
    pub fn update_geometry(&self) {
        self.request_layout();
        self.update();
    }

    /// What `QWidget::event` does for `StyleChange` / `FontChange` (qwidget.cpp:9502-9510):
    /// Repaint, geometry update and layout invalidation.
    pub fn style_changed(&self) {
        self.update_geometry();
        if let Ok(mut layout) = self.layout.try_borrow_mut() {
            if let Some(layout) = layout.as_mut() {
                layout.invalidate();
            }
        }
    }

    pub fn request_layout(&self) {
        if let Some(parent) = self.parent.borrow().as_ref() {
            crate::command::WidgetCommandQueue::post_layout_weak(parent);
        }
    }

    /// Sets this widget's own style sheet. As in Qt, a string without a rule block
    /// (`"color: red;"`) is a declaration list for the widget itself, i.e. `* { … }`.
    pub fn set_style_sheet(&self, qss: &str) {
        if qss.trim().is_empty() {
            *self.style_sheet.borrow_mut() = None;
        } else if qss.contains('{') {
            *self.style_sheet.borrow_mut() = Some(QStyleSheetStyle::parse(qss));
        } else {
            *self.style_sheet.borrow_mut() = Some(QStyleSheetStyle::parse(&format!("* {{ {qss} }}")));
        }
        self.style_changed();
    }

    pub fn style_sheet(&self) -> Option<std::cell::Ref<'_, QStyleSheetStyle>> {
        let borrow = self.style_sheet.borrow();
        if borrow.is_some() {
            Some(std::cell::Ref::map(borrow, |s| s.as_ref().unwrap()))
        } else {
            None
        }
    }

    /// Resolves this widget's style the way `QStyleSheetStyle` does: the application sheet,
    /// then the sheets of its ancestors from the outermost inwards, then its own sheet.
    ///
    /// An ancestor that is mutably borrowed (it is the widget currently being driven) ends the
    /// walk; painting and layout of children happen after the parent's borrow is released.
    pub fn resolve_style(
        &self,
        ctx: &crate::style::stylesheet::WidgetStyleContext,
    ) -> crate::style::stylesheet::ResolvedStyle {
        let mut ancestors: Vec<QStyleSheetStyle> = Vec::new();
        let mut next = self.parent.borrow().clone();
        while let Some(weak) = next {
            let Some(rc) = weak.upgrade() else { break };
            let Ok(widget) = rc.try_borrow() else { break };
            if let Some(sheet) = widget.style_sheet() {
                ancestors.push(sheet);
            }
            next = widget.parent_widget();
        }

        let app = crate::application::Application::style_sheet();
        let own = self.style_sheet.borrow();
        let sheets: Vec<&QStyleSheetStyle> = app
            .as_deref()
            .into_iter()
            .chain(ancestors.iter().rev())
            .chain(own.as_ref())
            .collect();
        QStyleSheetStyle::resolve_chain(&sheets, ctx)
    }

    /// Stores a style property. Like `QObject::setProperty`, this does not restyle the widget;
    /// `style_changed` (the repolish) does.
    pub fn set_property(&self, name: &str, value: &str) {
        let mut props = self.properties.borrow_mut();
        if let Some(pos) = props.iter().position(|(k, _)| k == name) {
            props[pos].1 = value.to_string();
        } else {
            props.push((name.to_string(), value.to_string()));
        }
        self.dirty.set(Some(self.geometry.get()));
    }

    pub fn property(&self, name: &str) -> Option<String> {
        self.properties
            .borrow()
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    }

    pub fn children(&self) -> Vec<WidgetRef> {
        self.children.borrow().clone()
    }

    pub fn add_child(&mut self, child: WidgetRef) {
        self.children.get_mut().retain(|c| !Rc::ptr_eq(c, &child));
        if let Some(win_id) = self.window_id.get() {
            child.borrow().set_window_id(Some(win_id));
        }
        self.children.get_mut().push(child);
    }

    pub fn remove_child(&mut self, child_id: ObjectId) {
        self.children.get_mut().retain(|c| c.try_borrow().map_or(true, |w| w.id() != child_id));
    }
}

impl Default for WidgetBase {
    fn default() -> Self {
        Self::new()
    }
}

pub type PaintHandler = Box<dyn FnMut(&mut Painter) + 'static>;

pub type ResizeHandler = Box<dyn FnMut(Size, Size) + 'static>;

pub struct EmptyWidget {
    pub base: WidgetBase,
    pub background_color: Option<ferrokit_gui::tiny_skia::Color>,
    pub paint_handler: Option<PaintHandler>,
    pub resize_handler: RefCell<Option<ResizeHandler>>,
}

pub type CustomWidget = EmptyWidget;

impl EmptyWidget {
    pub fn new() -> Self {
        Self {
            base: WidgetBase::new(),
            background_color: None,
            paint_handler: None,
            resize_handler: RefCell::new(None),
        }
    }

    pub fn with_geometry(geometry: Rect) -> Self {
        Self {
            base: WidgetBase::with_geometry(geometry),
            background_color: None,
            paint_handler: None,
            resize_handler: RefCell::new(None),
        }
    }

    pub fn set_background_color(&mut self, color: Option<ferrokit_gui::tiny_skia::Color>) {
        self.background_color = color;
        self.update();
    }

    pub fn set_paint_handler<F>(&mut self, handler: F)
    where
        F: FnMut(&mut Painter) + 'static,
    {
        self.paint_handler = Some(Box::new(handler));
        self.update();
    }

    pub fn with_paint_handler<F>(mut self, handler: F) -> Self
    where
        F: FnMut(&mut Painter) + 'static,
    {
        self.paint_handler = Some(Box::new(handler));
        self
    }

    pub fn set_resize_handler<F: FnMut(Size, Size) + 'static>(&self, handler: F) {
        *self.resize_handler.borrow_mut() = Some(Box::new(handler));
    }
}

impl Default for EmptyWidget {
    fn default() -> Self {
        Self::new()
    }
}

impl QObject for EmptyWidget {
    fn object_data(&self) -> &ObjectData {
        &self.base.object_data
    }

    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.base.object_data)
    }

    fn as_qobject_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_qobject_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn event(&mut self, event: &mut Event) -> bool {
        match &event.kind {
            EventKind::MouseMove { x, y } => {
                self.mouse_move_event(Point::new(*x, *y));
                false
            }
            EventKind::Enter { x, y } => {
                self.enter_event(Point::new(*x, *y));
                false
            }
            EventKind::Leave => {
                self.leave_event();
                false
            }
            EventKind::MouseButtonPress { x, y, button } => {
                self.mouse_press_event(Point::new(*x, *y), *button, 0);
                false
            }
            EventKind::MouseButtonRelease { x, y, button } => {
                self.mouse_release_event(Point::new(*x, *y), *button, 0);
                false
            }
            EventKind::Wheel {
                x,
                y,
                angle_delta_y,
                modifiers,
                ..
            } => {
                self.wheel_event(Point::new(*x, *y), *angle_delta_y, *modifiers);
                // The default `wheel_event` does nothing, like `QWidget::wheelEvent`, which
                // ignores the event so that it reaches the parent (a scroll area).
                false
            }
            EventKind::Resize {
                width,
                height,
                old_width,
                old_height,
            } => {
                self.resize_event(
                    Size::new(*width, *height),
                    Size::new(*old_width, *old_height),
                );
                true
            }
            EventKind::FocusIn { reason } => {
                self.focus_in_event(*reason);
                true
            }
            EventKind::FocusOut { reason } => {
                self.focus_out_event(*reason);
                true
            }
            EventKind::KeyPress {
                key,
                modifiers,
                is_repeat,
            } => {
                self.key_press_event(*key, *modifiers, *is_repeat);
                true
            }
            EventKind::KeyRelease { key, modifiers } => {
                self.key_release_event(*key, *modifiers);
                true
            }
            _ => false,
        }
    }
}

impl Widget for EmptyWidget {
    fn widget_base(&self) -> &crate::widget::WidgetBase {
        &self.base
    }

    fn id(&self) -> ObjectId {
        self.base.object_data.id()
    }
    fn geometry(&self) -> Rect {
        self.base.geometry.get()
    }

    fn set_geometry(&self, rect: Rect) {
        let min = self.base.min_size.get();
        let max = self.base.max_size.get();
        let width = rect.width.min(max.width).max(min.width);
        let height = rect.height.min(max.height).max(min.height);
        let rect = Rect::new(rect.x, rect.y, width, height);
        let old_rect = self.base.geometry.get();
        let size_changed = old_rect.width != rect.width || old_rect.height != rect.height;
        let old_size = Size::new(old_rect.width, old_rect.height);
        let new_size = Size::new(rect.width, rect.height);
        self.base.geometry.set(rect);
        if old_rect != rect {
            self.base.invalidate_vacated_rect(old_rect);
        }
        if size_changed {
            let has_parent = self.base.parent.borrow().is_some();
            // A window root gets its `Resize` event from `Window` (one `QResizeEvent` per resize);
            // sending it here as well ran `resize_event` twice.
            if has_parent || self.base.window_id.get().is_none() {
                self.resize_event(new_size, old_size);
            }
            // A layout-owning child re-lays out its children when its size changes, as Qt's
            // QLayout::widgetEvent does on Resize, when the layout is already activated (qlayout.cpp:528-530).
            // A parentless widget is a window root, which Window lays out itself (window.rs:859-860);
            // fitting it here as well ran that pass twice (test_layout_metric_caching aligned container).
            if let Some(mut layout) = self.layout_ref_mut() {
                if has_parent && !layout.is_dirty() {
                    layout.set_geometry(Rect::new(0, 0, rect.width, rect.height));
                }
            }
        }

        self.update();
    }

    fn size_hint(&self) -> Size {
        if let Some(layout) = self.base.layout.borrow().as_ref() {
            layout.size_hint()
        } else {
            Size::new(-1, -1)
        }
    }

    fn minimum_size(&self) -> Size {
        self.base.min_size.get()
    }

    /// `QWidget::minimumSizeHint` of a widget with a layout: the layout's minimum size.
    fn minimum_size_hint(&self) -> Size {
        if let Some(layout) = self.base.layout.borrow().as_ref() {
            layout.minimum_size()
        } else {
            Size::new(-1, -1)
        }
    }

    fn maximum_size(&self) -> Size {
        self.base.max_size.get()
    }

    fn is_visible(&self) -> bool {
        self.base.visible.get()
    }

    fn set_visible(&self, visible: bool) {
        self.base.set_visible(visible);
    }

    fn is_enabled(&self) -> bool {
        self.base.enabled.get()
    }

    fn update(&self) {
        self.base.update();
    }

    fn dirty_rect(&self) -> Option<Rect> {
        self.base.dirty.get()
    }

    fn clear_dirty(&self) {
        self.base.dirty.set(None);
    }

    fn layout_mut(&mut self) -> Option<&mut Box<dyn Layout>> {
        self.base.layout.get_mut().as_mut()
    }

    fn layout_ref_mut(&self) -> Option<RefMut<'_, Box<dyn Layout>>> {
        if let Ok(borrow) = self.base.layout.try_borrow_mut() {
            if borrow.is_some() {
                Some(RefMut::map(borrow, |opt| opt.as_mut().unwrap()))
            } else {
                None
            }
        } else {
            None
        }
    }
    fn set_layout(&mut self, mut layout: Box<dyn Layout>) {
        let g = self.base.geometry.get();
        layout.set_default_margin(owner_layout_margin(self.base.parent.borrow().is_some()));
        layout.set_geometry(Rect::new(0, 0, g.width, g.height));
        layout.set_smart_spacing(WIDGET_LAYOUT_SPACING);
        if let Some(win_id) = self.base.window_id.get() {
            for w in layout.widgets() {
                w.borrow().set_window_id(Some(win_id));
            }
        }
        *self.base.layout.get_mut() = Some(layout);
        self.update();
    }

    fn parent_widget(&self) -> Option<WidgetWeak> {
        self.base.parent.borrow().clone()
    }

    fn set_parent_widget(&self, parent: Option<WidgetWeak>) {
        let has_parent = parent.is_some();
        *self.base.parent.borrow_mut() = parent;
        // The owner margin depends on whether this widget is a window (`QLayoutPrivate::getMargin`).
        if let Some(mut layout) = self.layout_ref_mut() {
            layout.set_default_margin(owner_layout_margin(has_parent));
        }
    }

    fn window_id(&self) -> Option<ObjectId> {
        self.base.window_id.get()
    }

    fn set_window_id(&self, window_id: Option<ObjectId>) {
        self.base.window_id.set(window_id);
        for child in self.children() {
            child.borrow().set_window_id(window_id);
        }
    }

    fn children(&self) -> Vec<WidgetRef> {
        let mut list = self.base.children.borrow().clone();
        if let Some(layout) = self.base.layout.borrow().as_ref() {
            // Identity, not `id()`: no `RefCell` is borrowed, so a child on the stack is fine.
            let mut seen: std::collections::HashSet<*const RefCell<Box<dyn Widget>>> =
                list.iter().map(Rc::as_ptr).collect();
            for w in layout.widgets() {
                if seen.insert(Rc::as_ptr(&w)) {
                    list.push(w);
                }
            }
        }
        list
    }

    fn add_child(&mut self, child: WidgetRef) {
        self.base.children.get_mut().retain(|c| !Rc::ptr_eq(c, &child));
        if let Some(win_id) = self.base.window_id.get() {
            child.borrow().set_window_id(Some(win_id));
        }
        self.base.children.get_mut().push(child);
        self.update();
    }

    fn remove_child(&mut self, child_id: ObjectId) {
        // A child that is borrowed (its own handler removes it) cannot be identified: it stays.
        self.base
            .children
            .get_mut()
            .retain(|c| c.try_borrow().map_or(true, |w| w.id() != child_id));
        self.update();
    }

    fn focus_policy(&self) -> FocusPolicy {
        self.base.focus_policy.get()
    }

    fn set_focus_policy(&self, policy: FocusPolicy) {
        self.base.focus_policy.set(policy);
    }

    fn has_focus(&self) -> bool {
        self.base.has_focus.get()
    }

    fn set_has_focus(&self, focus: bool) {
        self.base.has_focus.set(focus);
    }

    fn paint_event(&mut self, painter: &mut Painter) {
        if let Some(color) = self.background_color {
            let brush = ferrokit_gui::paint::Brush::Color(color);
            painter.set_brush(brush);
            painter.set_pen(None);
            let g = self.base.geometry.get();
            let rect_f = RectF::new(0.0, 0.0, g.width as f32, g.height as f32);
            painter.draw_rect(rect_f);
        }
        if let Some(handler) = &mut self.paint_handler {
            handler(painter);
        }
    }
    fn resize_event(&self, new_size: Size, old_size: Size) {
        if let Ok(mut h) = self.resize_handler.try_borrow_mut() {
            if let Some(cb) = h.as_mut() {
                cb(new_size, old_size);
            }
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
