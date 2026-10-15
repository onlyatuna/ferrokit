// SPDX-License-Identifier: LGPL-3.0-only
//! Widget events are delivered through the object filters (`QObject::eventFilter`) before
//! the receiver's `event()`, as `QCoreApplicationPrivate::sendThroughObjectEventFilters` does.
//! Covered here: `FontChange` (Button::set_font), `StyleChange` (a parent's `setStyleSheet` reaching
//! a child, qwidget.cpp:2724-2725), `EnabledChange` (qwidget.cpp:3474-3475), and `Paint`
//! (`QWidget::event(Paint)` runs `paintEvent`, qwidget.cpp:9186-9190; a consuming filter skips it).

use parking_lot::Mutex;
use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::object::{register_boxed_qobject, ObjectData, ObjectId, QObject};
use ferrokit_gui::geometry::primitives::Rect;
use ferrokit_gui::paint::pixmap::Pixmap;
use ferrokit_gui::paint::Painter;
use ferrokit_gui::text::Font;
use ferrokit_widgets::window::deliver_paint_event;
use ferrokit_widgets::*;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

type Seen = Arc<Mutex<Vec<(ObjectId, &'static str)>>>;

fn kind_name(kind: &EventKind) -> &'static str {
    match kind {
        EventKind::FontChange => "FontChange",
        EventKind::StyleChange => "StyleChange",
        EventKind::EnabledChange => "EnabledChange",
        EventKind::Paint { .. } => "Paint",
        _ => "other",
    }
}

/// Object filter: records every event it sees with the watched object's id, and consumes `Paint`
/// when `consume_paint` is set (a consumed event is not delivered to the receiver).
struct Spy {
    data: ObjectData,
    seen: Seen,
    consume_paint: bool,
}

impl QObject for Spy {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> &mut ObjectData {
        &mut self.data
    }
    fn event(&mut self, _event: &mut Event) -> bool {
        false
    }
    fn event_filter(&mut self, watched: ObjectId, event: &mut Event) -> bool {
        self.seen.lock().push((watched, kind_name(&event.kind)));
        self.consume_paint && matches!(event.kind, EventKind::Paint { .. })
    }
}

/// Registers a spy filter. The returned box keeps the filter alive for the test body.
fn register_spy(consume_paint: bool) -> (ObjectId, Seen, Box<Spy>) {
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let spy = Box::new(Spy {
        data: ObjectData::with_auto_id(),
        seen: Arc::clone(&seen),
        consume_paint,
    });
    // SAFETY: test-only; the returned box is kept alive by the caller for the whole test.
    let (id, pin) = unsafe { register_boxed_qobject(spy) };
    (id, seen, pin)
}

fn saw(seen: &Seen, watched: ObjectId, kind: &str) -> bool {
    seen.lock().iter().any(|(w, k)| *w == watched && *k == kind)
}

type Log = Arc<Mutex<Vec<String>>>;

/// Widget that logs `StyleChange` as `<name>:SC` and counts `paint_event` calls.
struct Probe {
    base: WidgetBase,
    name: &'static str,
    log: Log,
    paints: Arc<AtomicUsize>,
}

impl QObject for Probe {
    fn event(&mut self, event: &mut Event) -> bool {
        if matches!(event.kind, EventKind::StyleChange) {
            self.log.lock().push(format!("{}:SC", self.name));
        }
        false
    }
    fn object_data(&self) -> &ObjectData {
        &self.base.object_data
    }
    fn object_data_mut(&mut self) -> &mut ObjectData {
        &mut self.base.object_data
    }
}

impl Widget for Probe {
    fn widget_base(&self) -> &WidgetBase {
        &self.base
    }
    fn id(&self) -> ObjectId {
        self.base.object_data.id
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
    fn dirty_rect(&self) -> Option<Rect> {
        self.base.dirty_rect()
    }
    fn clear_dirty(&self) {
        self.base.clear_dirty();
    }
    fn layout(&self) -> Option<&dyn Layout> {
        None
    }
    fn layout_mut(&mut self) -> Option<&mut Box<dyn Layout>> {
        None
    }
    fn set_layout(&mut self, _layout: Box<dyn Layout>) {}
    fn parent_widget(&self) -> Option<WidgetWeak> {
        self.base.parent_widget()
    }
    fn set_parent_widget(&self, parent: Option<WidgetWeak>) {
        self.base.set_parent_widget(parent);
    }
    fn window_id(&self) -> Option<ObjectId> {
        self.base.window_id()
    }
    fn set_window_id(&self, window_id: Option<ObjectId>) {
        self.base.set_window_id(window_id);
    }
    fn children(&self) -> Vec<WidgetRef> {
        self.base.children()
    }
    fn add_child(&mut self, child: WidgetRef) {
        self.base.children.borrow_mut().push(child);
    }
    fn remove_child(&mut self, child_id: ObjectId) {
        self.base
            .children
            .borrow_mut()
            .retain(|c| c.borrow().id() != child_id);
    }
    fn paint_event(&mut self, _painter: &mut Painter) {
        self.paints.fetch_add(1, Ordering::SeqCst);
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
    fn focus_policy(&self) -> FocusPolicy {
        self.base.focus_policy()
    }
    fn has_focus(&self) -> bool {
        self.base.has_focus()
    }
    fn set_has_focus(&self, focus: bool) {
        self.base.set_has_focus(focus);
    }
}

fn probe(name: &'static str, log: &Log, paints: &Arc<AtomicUsize>) -> WidgetRef {
    Rc::new(RefCell::new(Box::new(Probe {
        base: WidgetBase::with_geometry(Rect::new(0, 0, 10, 10)),
        name,
        log: Arc::clone(log),
        paints: Arc::clone(paints),
    })))
}

#[test]
fn font_change_reaches_object_filter_on_button_set_font() {
    let (filter_id, seen, _pin) = register_spy(false);
    let mut button = Button::new("b");
    button.object_data_mut().install_event_filter(filter_id);
    let id = button.object_data().id;

    button.set_font(Font::new("Consolas", 9.0));

    assert!(
        saw(&seen, id, "FontChange"),
        "filter did not see FontChange"
    );
}

#[test]
fn style_change_reaches_child_filter_and_event() {
    let (filter_id, seen, _pin) = register_spy(false);
    let log: Log = Arc::default();
    let paints = Arc::new(AtomicUsize::new(0));
    let parent = probe("parent", &log, &paints);
    let child = probe("child", &log, &paints);
    parent.borrow_mut().add_child(child.clone());
    let child_id = child.borrow().id();
    child
        .borrow_mut()
        .object_data_mut()
        .install_event_filter(filter_id);

    parent
        .borrow()
        .set_style_sheet("QWidget { color: #ff0000; }");

    assert!(
        saw(&seen, child_id, "StyleChange"),
        "filter did not see StyleChange"
    );
    assert_eq!(*log.lock(), vec!["child:SC".to_string()]);
}

#[test]
fn enabled_change_reaches_object_filter() {
    let (filter_id, seen, _pin) = register_spy(false);
    let button: WidgetRef = Rc::new(RefCell::new(Box::new(Button::new("b"))));
    button
        .borrow_mut()
        .object_data_mut()
        .install_event_filter(filter_id);
    let id = button.borrow().id();

    set_widget_enabled(&button, false);

    assert!(
        saw(&seen, id, "EnabledChange"),
        "filter did not see EnabledChange"
    );
}

#[test]
fn paint_goes_through_filters_and_a_consuming_filter_skips_paint_event() {
    for (consume_paint, expected_paints) in [(false, 1), (true, 0)] {
        let (filter_id, seen, _pin) = register_spy(consume_paint);
        let log: Log = Arc::default();
        let paints = Arc::new(AtomicUsize::new(0));
        let mut widget = Probe {
            base: WidgetBase::with_geometry(Rect::new(0, 0, 10, 10)),
            name: "paint",
            log: Arc::clone(&log),
            paints: Arc::clone(&paints),
        };
        widget.object_data_mut().install_event_filter(filter_id);
        let id = widget.object_data().id;

        let mut pixmap = Pixmap::with_dpr(10, 10, 1.0).expect("pixmap");
        let mut painter = Painter::begin(&mut pixmap);
        deliver_paint_event(&mut widget, &mut painter, Rect::new(0, 0, 10, 10));

        assert!(
            saw(&seen, id, "Paint"),
            "consume_paint={consume_paint}: filter did not see Paint"
        );
        assert_eq!(
            paints.load(Ordering::SeqCst),
            expected_paints,
            "consume_paint={consume_paint}: paint_event call count"
        );
    }
}
