// SPDX-License-Identifier: LGPL-3.0-only
//! Parity tests for `ToolTipChange` event.

use std::cell::RefCell;
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::Arc;

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::object::{
    register_owned_qobject, register_qobject_ptr, unregister_qobject, ObjectData, ObjectDataMut,
    ObjectId, OwnedObject, QObject,
};
use ferrokit_gui::geometry::primitives::Rect;
use ferrokit_widgets::widget::{Widget, WidgetRef};
use ferrokit_widgets::Button;
use parking_lot::Mutex;

struct ToolTipProbe {
    base: ferrokit_widgets::widget::WidgetBase,
    log: Arc<Mutex<Vec<String>>>,
}

impl QObject for ToolTipProbe {
    fn object_data(&self) -> &ObjectData {
        &self.base.object_data
    }
    fn object_data_mut(&mut self) -> ObjectDataMut<'_> {
        ObjectDataMut::new(&mut self.base.object_data)
    }
    fn event(&mut self, ev: &mut Event) -> bool {
        if matches!(ev.kind, EventKind::ToolTipChange) {
            let text = self.tool_tip();
            self.log.lock().push(format!("event:{text}"));
            return true;
        }
        false
    }
}

impl Widget for ToolTipProbe {
    fn widget_base(&self) -> &ferrokit_widgets::widget::WidgetBase {
        &self.base
    }
    fn id(&self) -> ObjectId {
        self.base.object_data.id()
    }
    fn geometry(&self) -> Rect {
        self.base.geometry()
    }
    fn set_geometry(&self, rect: Rect) {
        self.base.set_geometry(rect);
    }
    fn is_visible(&self) -> bool {
        self.base.is_visible()
    }
    fn set_visible(&self, visible: bool) {
        self.base.set_visible(visible);
    }
    fn is_enabled(&self) -> bool {
        self.base.is_enabled()
    }
    fn update(&self) {
        self.base.update();
    }
    fn clear_dirty(&self) {
        self.base.clear_dirty();
    }
    fn dirty_rect(&self) -> Option<Rect> {
        self.base.dirty_rect()
    }
    fn layout(&self) -> Option<&dyn ferrokit_widgets::layout::Layout> {
        None
    }
    fn layout_mut(&mut self) -> Option<&mut Box<dyn ferrokit_widgets::layout::Layout>> {
        None
    }
    fn set_layout(&mut self, _layout: Box<dyn ferrokit_widgets::layout::Layout>) {}
    fn parent_widget(&self) -> Option<ferrokit_widgets::widget::WidgetWeak> {
        self.base.parent_widget()
    }
    fn set_parent_widget(&self, parent: Option<ferrokit_widgets::widget::WidgetWeak>) {
        self.base.set_parent_widget(parent);
    }
    fn window_id(&self) -> Option<ObjectId> {
        self.base.window_id()
    }
    fn set_window_id(&self, window_id: Option<ObjectId>) {
        self.base.set_window_id(window_id);
    }
    fn children(&self) -> Vec<ferrokit_widgets::widget::WidgetRef> {
        self.base.children()
    }
    fn add_child(&mut self, child: ferrokit_widgets::widget::WidgetRef) {
        self.base.children.borrow_mut().push(child);
    }
    fn remove_child(&mut self, child_id: ObjectId) {
        self.base
            .children
            .borrow_mut()
            .retain(|c| c.borrow().id() != child_id);
    }
    fn paint_event(&mut self, _painter: &mut ferrokit_gui::paint::Painter) {}
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

type Log = Arc<Mutex<Vec<String>>>;

fn new_probe(log: &Log) -> ToolTipProbe {
    ToolTipProbe {
        base: ferrokit_widgets::widget::WidgetBase::new(),
        log: Arc::clone(log),
    }
}

fn entries(log: &Log) -> Vec<String> {
    log.lock().clone()
}

/// Object filter that logs `filter` for `ToolTipChange` and consumes it when `consume` is set.
struct ChangeFilter {
    data: ObjectData,
    log: Log,
    consume: bool,
}

impl QObject for ChangeFilter {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ObjectDataMut<'_> {
        ObjectDataMut::new(&mut self.data)
    }
    fn event(&mut self, _event: &mut Event) -> bool {
        false
    }
    fn event_filter(&mut self, _watched: ObjectId, event: &mut Event) -> bool {
        if matches!(event.kind, EventKind::ToolTipChange) {
            self.log.lock().push("filter".to_string());
            return self.consume;
        }
        false
    }
}

fn register_filter(log: &Log, consume: bool) -> (ObjectId, OwnedObject<ChangeFilter>) {
    let filter = ChangeFilter {
        data: ObjectData::with_auto_id(),
        log: Arc::clone(log),
        consume,
    };
    // SAFETY: test-only; the owner stays on this thread for the whole test and nothing touches the
    // filter while a callback of it runs.
    let owner = unsafe { register_owned_qobject(filter) };
    (OwnedObject::object_id(&owner), owner)
}

/// `QWidget::setToolTip` (qwidget.cpp:11793-11800) stores the text and then always sends
/// `ToolTipChange`, so an unregistered widget gets it too, once per call even for the same text.
#[test]
fn set_tool_tip_on_an_unregistered_widget_sends_tool_tip_change() {
    let log: Log = Arc::default();
    let mut w = new_probe(&log);
    w.set_tool_tip("one");
    assert_eq!(w.tool_tip(), "one");
    assert_eq!(entries(&log), ["event:one"]);
    w.set_tool_tip("one");
    assert_eq!(entries(&log), ["event:one", "event:one"], "sent even if the text is unchanged");
}

/// The event goes through the object filters first and `event()` second, and the new text is
/// already stored when either runs, for a widget registered by pointer.
#[test]
fn set_tool_tip_on_a_pointer_registered_widget_goes_through_filters_then_event() {
    let log: Log = Arc::default();
    let (filter_id, _filter) = register_filter(&log, false);
    let mut probe = new_probe(&log);
    probe.object_data_mut().install_event_filter(filter_id);
    let id = probe.id();
    let probe_ptr = &raw mut probe;
    unsafe {
        register_qobject_ptr(NonNull::new_unchecked(probe_ptr));
    }
    // SAFETY: the pointer is valid for the whole test and no other reference is used while this one
    // is; the registry reaches the widget only through the pointer, not through `probe`.
    let w = unsafe { &mut *probe_ptr };
    w.set_tool_tip("Hello Tooltip");
    assert_eq!(w.tool_tip(), "Hello Tooltip");
    assert_eq!(entries(&log), ["filter", "event:Hello Tooltip"]);
    unsafe {
        unregister_qobject(id);
    }
}

/// A filter that consumes `ToolTipChange` keeps `event()` from seeing it; the text is stored all
/// the same (`d->toolTip` is assigned before the event is sent).
#[test]
fn a_consuming_filter_hides_tool_tip_change_from_the_widget_but_not_the_text() {
    let log: Log = Arc::default();
    let (filter_id, _filter) = register_filter(&log, true);
    // SAFETY: test-only; the owner stays on this thread for the whole test.
    let mut owned = unsafe { register_owned_qobject(new_probe(&log)) };
    owned
        .with_mut(|w| w.object_data_mut().install_event_filter(filter_id))
        .unwrap();
    owned.with_mut(|w| w.set_tool_tip("kept")).unwrap();
    assert_eq!(entries(&log), ["filter"]);
    assert_eq!(owned.with(|w| w.tool_tip()).as_deref(), Some("kept"));
}

/// A widget held by its owner (`WidgetRef`) gets the event through the borrow the caller holds; it
/// is not skipped because the widget is borrowed.
#[test]
fn set_tool_tip_through_a_widget_ref_borrow_sends_tool_tip_change() {
    let log: Log = Arc::default();
    let widget_ref: WidgetRef = Rc::new(RefCell::new(Box::new(new_probe(&log))));
    widget_ref.borrow_mut().set_tool_tip("Ref Tooltip");
    assert_eq!(entries(&log), ["event:Ref Tooltip"]);
    assert_eq!(widget_ref.borrow().tool_tip(), "Ref Tooltip");
}

/// Every widget type has it, not only a custom one: a stock `Button` reaches an object filter.
#[test]
fn a_stock_button_sends_tool_tip_change_to_its_filter() {
    let log: Log = Arc::default();
    let (filter_id, _filter) = register_filter(&log, false);
    let mut button = Button::new("b");
    button.object_data_mut().install_event_filter(filter_id);
    button.set_tool_tip("tip");
    assert_eq!(entries(&log), ["filter"]);
    assert_eq!(button.tool_tip(), "tip");
}
