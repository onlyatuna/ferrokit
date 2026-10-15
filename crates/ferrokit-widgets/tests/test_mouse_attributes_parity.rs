// SPDX-License-Identifier: LGPL-3.0-only
//! Parity tests for `WA_TransparentForMouseEvents` and `WA_NoMousePropagation`.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::object::ObjectId;
use ferrokit_gui::geometry::primitives::{Point, Rect};
use ferrokit_widgets::hit_test::EventTreeDispatcher;
use ferrokit_widgets::layout::Layout;
use ferrokit_widgets::widget::{EmptyWidget, Widget, WidgetRef, WidgetWeak};

fn widget(p: impl Widget + 'static) -> WidgetRef {
    Rc::new(RefCell::new(Box::new(p)))
}

fn adopt_tree(root: &WidgetRef) {
    let weak = WidgetRef::downgrade(root);
    for child in root.borrow().children() {
        child.borrow_mut().set_parent_widget(Some(weak.clone()));
        adopt_tree(&child);
    }
}

#[test]
fn transparent_for_mouse_events_bypasses_child_in_hit_test() {
    let parent = widget(EmptyWidget::new());
    parent.borrow_mut().set_geometry(Rect::new(0, 0, 200, 200));

    let child = widget(EmptyWidget::new());
    child.borrow_mut().set_geometry(Rect::new(10, 10, 100, 100));
    parent.borrow_mut().add_child(child.clone());
    adopt_tree(&parent);

    // Hit child without attribute
    let path = ferrokit_widgets::hit_test::hit_path(&parent, Point::new(20, 20));
    assert_eq!(
        path.last().unwrap().widget.borrow().id(),
        child.borrow().id()
    );

    // Enable WA_TransparentForMouseEvents on child
    child
        .borrow()
        .widget_base()
        .set_transparent_for_mouse_events(true);
    let path2 = ferrokit_widgets::hit_test::hit_path(&parent, Point::new(20, 20));
    // Must fall through to parent
    assert_eq!(
        path2.last().unwrap().widget.borrow().id(),
        parent.borrow().id()
    );
}

#[test]
fn no_mouse_propagation_stops_event_from_propagating_to_parent() {
    struct TestWidget {
        base: ferrokit_widgets::widget::WidgetBase,
        press_received: Arc<AtomicBool>,
    }
    impl ferrokit_core::object::QObject for TestWidget {
        fn object_data(&self) -> &ferrokit_core::object::ObjectData {
            &self.base.object_data
        }
        fn object_data_mut(&mut self) -> &mut ferrokit_core::object::ObjectData {
            &mut self.base.object_data
        }
        fn event(&mut self, ev: &mut Event) -> bool {
            if matches!(ev.kind, EventKind::MouseButtonPress { .. }) {
                self.press_received.store(true, Ordering::SeqCst);
                // intentionally do NOT accept, testing propagation
                ev.ignore();
                return false;
            }
            false
        }
    }
    impl Widget for TestWidget {
        fn widget_base(&self) -> &ferrokit_widgets::widget::WidgetBase {
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
        fn add_child(&mut self, child: WidgetRef) {
            self.base.children.borrow_mut().push(child);
        }
        fn remove_child(&mut self, child_id: ObjectId) {
            self.base
                .children
                .borrow_mut()
                .retain(|c| c.borrow().id() != child_id);
        }
        fn children(&self) -> Vec<WidgetRef> {
            self.base.children()
        }
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
        fn clear_dirty(&self) {
            self.base.clear_dirty();
        }
        fn dirty_rect(&self) -> Option<Rect> {
            self.base.dirty_rect()
        }
        fn layout(&self) -> Option<&dyn Layout> {
            None
        }
        fn layout_mut(&mut self) -> Option<&mut Box<dyn Layout>> {
            None
        }
        fn set_layout(&mut self, _layout: Box<dyn Layout>) {}
        fn paint_event(&mut self, _painter: &mut ferrokit_gui::paint::Painter) {}
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
    }

    let parent_flag = Arc::new(AtomicBool::new(false));
    let parent = widget(TestWidget {
        base: ferrokit_widgets::widget::WidgetBase::with_geometry(Rect::new(0, 0, 200, 200)),
        press_received: Arc::clone(&parent_flag),
    });

    let child_flag = Arc::new(AtomicBool::new(false));
    let child = widget(TestWidget {
        base: ferrokit_widgets::widget::WidgetBase::with_geometry(Rect::new(10, 10, 100, 100)),
        press_received: Arc::clone(&child_flag),
    });
    parent.borrow_mut().add_child(child.clone());
    adopt_tree(&parent);

    let mut dispatcher = EventTreeDispatcher::new();
    let mut ev = Event::new_spontaneous(EventKind::MouseButtonPress {
        x: 20,
        y: 20,
        button: 1,
    });

    // Case 1: Without no_mouse_propagation, parent receives unaccepted event
    dispatcher.dispatch_event(&parent, &mut ev);
    assert!(child_flag.load(Ordering::SeqCst));
    assert!(
        parent_flag.load(Ordering::SeqCst),
        "without attribute, parent receives unaccepted event"
    );

    // Reset flags
    child_flag.store(false, Ordering::SeqCst);
    parent_flag.store(false, Ordering::SeqCst);

    // Case 2: With WA_NoMousePropagation, child stops propagation
    child.borrow().widget_base().set_no_mouse_propagation(true);
    let mut ev2 = Event::new_spontaneous(EventKind::MouseButtonPress {
        x: 20,
        y: 20,
        button: 1,
    });
    dispatcher.dispatch_event(&parent, &mut ev2);
    assert!(child_flag.load(Ordering::SeqCst));
    assert!(
        !parent_flag.load(Ordering::SeqCst),
        "WA_NoMousePropagation stops event from reaching parent"
    );
}
