// SPDX-License-Identifier: LGPL-3.0-only
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::event_loop::EventLoopHandle;
use ferrokit_gui::geometry::primitives::Rect;
use ferrokit_widgets::button::Button;
use ferrokit_widgets::command::{WidgetCommand, WidgetCommandQueue, delete_widget_later};
use ferrokit_widgets::frame::Frame;
use ferrokit_widgets::hit_test::EventTreeDispatcher;
use ferrokit_widgets::layout::{BoxLayout, Direction, Layout};
use ferrokit_widgets::widget::{EmptyWidget, WidgetRef, set_widget_enabled};

thread_local! {
    static TEST1_BTN: RefCell<Option<WidgetRef>> = const { RefCell::new(None) };
    static TEST1_PARENT: RefCell<Option<WidgetRef>> = const { RefCell::new(None) };
    static TEST1_LOOP: RefCell<Option<EventLoopHandle>> = const { RefCell::new(None) };
    static TEST1_CHILDREN_AFTER_DISPATCH: Cell<usize> = const { Cell::new(usize::MAX) };
    static TEST2_SIBLING: RefCell<Option<WidgetRef>> = const { RefCell::new(None) };
    static TEST2_PARENT: RefCell<Option<WidgetRef>> = const { RefCell::new(None) };
    static TEST3_SIBLING: RefCell<Option<WidgetRef>> = const { RefCell::new(None) };
}

#[test]
fn test_self_deletion_during_event_dispatch() {
    // QObject::deleteLater posts a DeferredDelete; the widget leaves its parent when the event
    // loop delivers it, not when the dispatch that requested it returns (qobject.cpp:2563-2564).
    // A click arrives inside the loop, so the dispatch runs from a posted callback inside
    // `EventLoop::exec` (loop level 1, qeventloop.cpp:161). Outside a loop Qt does not deliver a
    // DeferredDelete (qcoreapplication.cpp:1881-1885).
    let event_loop = ferrokit_core::event_loop::EventLoop::new();

    let parent: WidgetRef = Rc::new(RefCell::new(Box::new(Frame::new())));
    parent.borrow().set_geometry(Rect::new(0, 0, 200, 200));

    let button: WidgetRef = Rc::new(RefCell::new(Box::new(Button::new("Delete Me"))));
    button.borrow().set_geometry(Rect::new(10, 10, 80, 30));
    parent.borrow_mut().add_child(button.clone());
    button
        .borrow()
        .set_parent_widget(Some(Rc::downgrade(&parent)));
    assert_eq!(parent.borrow().children().len(), 1);

    TEST1_BTN.with(|b| *b.borrow_mut() = Some(button.clone()));
    TEST1_PARENT.with(|p| *p.borrow_mut() = Some(parent.clone()));
    TEST1_LOOP.with(|l| *l.borrow_mut() = Some(event_loop.handle()));

    let clicked_count = Arc::new(AtomicUsize::new(0));
    let clicked_count_clone = clicked_count.clone();

    // Hook clicked signal on the button
    {
        let btn_borrow = button.borrow();
        let btn_downcast = btn_borrow
            .as_any()
            .downcast_ref::<Button>()
            .expect("should be button");
        btn_downcast.clicked.connect(move |_| {
            clicked_count_clone.fetch_add(1, Ordering::SeqCst);
            TEST1_BTN.with(|b| {
                if let Some(btn) = b.borrow().as_ref() {
                    delete_widget_later(btn);
                }
            });
        });
    }

    ferrokit_core::timer::Timer::single_shot(0, move || {
        let parent = TEST1_PARENT
            .with(|p| p.borrow().clone())
            .expect("parent is set");
        let mut dispatcher = EventTreeDispatcher::new();

        // Simulate mouse press and release on the button
        let mut press_ev = Event::new_spontaneous(EventKind::MouseButtonPress {
            x: 20,
            y: 20,
            button: 1,
        });
        dispatcher.dispatch_event(&parent, &mut press_ev);

        let mut release_ev = Event::new_spontaneous(EventKind::MouseButtonRelease {
            x: 20,
            y: 20,
            button: 1,
        });
        // This MUST NOT panic with BorrowMutError
        let consumed = dispatcher.dispatch_event(&parent, &mut release_ev);
        assert!(consumed);
        assert_eq!(clicked_count.load(Ordering::SeqCst), 1);

        // Dispatch has returned; the DeferredDelete is still queued, so the child is attached.
        TEST1_CHILDREN_AFTER_DISPATCH.with(|c| c.set(parent.borrow().children().len()));

        // Posted after the DeferredDelete, so the loop delivers that first and then quits.
        ferrokit_core::timer::Timer::single_shot(0, || {
            TEST1_LOOP.with(|l| l.borrow().as_ref().expect("loop handle is set").quit());
        });
    });

    assert_eq!(event_loop.exec(), 0);

    assert_eq!(
        TEST1_CHILDREN_AFTER_DISPATCH.with(|c| c.get()),
        1,
        "button removed before the event loop delivered DeferredDelete"
    );
    assert_eq!(parent.borrow().children().len(), 0);

    TEST1_BTN.with(|b| *b.borrow_mut() = None);
    TEST1_PARENT.with(|p| *p.borrow_mut() = None);
    TEST1_LOOP.with(|l| *l.borrow_mut() = None);
}

#[test]
fn test_reentrant_layout_request_during_callback() {
    let parent: WidgetRef = Rc::new(RefCell::new(Box::new(Frame::new())));
    parent.borrow().set_geometry(Rect::new(0, 0, 300, 200));

    let mut layout = Box::new(BoxLayout::new(Direction::TopToBottom));
    layout.set_spacing(5);
    layout.set_margins(ferrokit_gui::geometry::primitives::Margins::new(0, 0, 0, 0));

    let btn1: WidgetRef = Rc::new(RefCell::new(Box::new(Button::new("Button 1"))));
    btn1.borrow().set_geometry(Rect::new(0, 0, 100, 30));
    layout.add_widget(btn1.clone());

    let btn2: WidgetRef = Rc::new(RefCell::new(Box::new(Button::new("Button 2"))));
    btn2.borrow().set_geometry(Rect::new(0, 0, 100, 30));
    layout.add_widget(btn2.clone());

    parent.borrow_mut().set_layout(layout);
    parent.borrow_mut().add_child(btn1.clone());
    parent.borrow_mut().add_child(btn2.clone());
    ferrokit_widgets::widget::adopt_tree(&parent);

    // Initially layout positions them
    parent.borrow().update_layout();
    // Two buttons that cannot grow vertically in a 200px tall `QVBoxLayout`: `qGeomCalc` splits
    // the spare 133px into three equal gaps (above, between and below), so the first sits at 44.
    // The button height comes from Segoe UI's metrics, installed only on Windows.
    let first_y = btn1.borrow().geometry().y;
    #[cfg(windows)]
    assert_eq!(first_y, 44);

    TEST2_SIBLING.with(|s| *s.borrow_mut() = Some(btn2.clone()));
    TEST2_PARENT.with(|p| *p.borrow_mut() = Some(parent.clone()));

    let callback_executed = Arc::new(AtomicUsize::new(0));
    let callback_executed_clone = callback_executed.clone();

    {
        let b = btn1.borrow();
        let btn_downcast = b
            .as_any()
            .downcast_ref::<Button>()
            .expect("should be button");
        btn_downcast.clicked.connect(move |_| {
            callback_executed_clone.fetch_add(1, Ordering::SeqCst);
            // Mutate sibling visibility using &self (Cell)
            TEST2_SIBLING.with(|s| {
                if let Some(b2) = s.borrow().as_ref() {
                    b2.borrow().set_visible(false);
                }
            });
            // Request layout on parent
            TEST2_PARENT.with(|p| {
                if let Some(par) = p.borrow().as_ref() {
                    par.borrow().request_layout();
                }
            });
        });
    }

    let mut dispatcher = EventTreeDispatcher::new();
    let mut press_ev = Event::new_spontaneous(EventKind::MouseButtonPress {
        x: 10,
        y: first_y + 10,
        button: 1,
    });
    dispatcher.dispatch_event(&parent, &mut press_ev);

    let mut release_ev = Event::new_spontaneous(EventKind::MouseButtonRelease {
        x: 10,
        y: first_y + 10,
        button: 1,
    });
    // This MUST NOT panic with BorrowMutError
    dispatcher.dispatch_event(&parent, &mut release_ev);

    assert_eq!(callback_executed.load(Ordering::SeqCst), 1);
    assert!(!btn2.borrow().is_visible());
    // Layout queue flushed cleanly
    assert_eq!(WidgetCommandQueue::pending_count(), 0);

    TEST2_SIBLING.with(|s| *s.borrow_mut() = None);
    TEST2_PARENT.with(|p| *p.borrow_mut() = None);
}

#[test]
fn test_sibling_mutation_without_borrow_mut() {
    let parent: WidgetRef = Rc::new(RefCell::new(Box::new(Frame::new())));
    parent.borrow().set_geometry(Rect::new(0, 0, 300, 200));

    let btn1: WidgetRef = Rc::new(RefCell::new(Box::new(Button::new("Btn 1"))));
    btn1.borrow().set_geometry(Rect::new(0, 0, 50, 30));

    let btn2: WidgetRef = Rc::new(RefCell::new(Box::new(Button::new("Btn 2"))));
    btn2.borrow().set_geometry(Rect::new(60, 0, 50, 30));

    parent.borrow_mut().add_child(btn1.clone());
    parent.borrow_mut().add_child(btn2.clone());

    TEST3_SIBLING.with(|s| *s.borrow_mut() = Some(btn2.clone()));

    {
        let b = btn1.borrow();
        let btn_downcast = b
            .as_any()
            .downcast_ref::<Button>()
            .expect("should be button");
        btn_downcast.clicked.connect(move |_| {
            // Sibling mutation via immutable borrow &self!
            TEST3_SIBLING.with(|s| {
                if let Some(b2) = s.borrow().as_ref() {
                    b2.borrow().set_geometry(Rect::new(100, 100, 80, 40));
                    set_widget_enabled(b2, false);
                    b2.borrow().update();
                }
            });
        });
    }

    let mut dispatcher = EventTreeDispatcher::new();
    let mut press_ev = Event::new_spontaneous(EventKind::MouseButtonPress {
        x: 10,
        y: 10,
        button: 1,
    });
    dispatcher.dispatch_event(&parent, &mut press_ev);

    let mut release_ev = Event::new_spontaneous(EventKind::MouseButtonRelease {
        x: 10,
        y: 10,
        button: 1,
    });
    dispatcher.dispatch_event(&parent, &mut release_ev);

    assert_eq!(btn2.borrow().geometry(), Rect::new(100, 100, 80, 40));
    assert!(!btn2.borrow().is_enabled());

    TEST3_SIBLING.with(|s| *s.borrow_mut() = None);
}

#[test]
fn test_multilevel_layout_traversal_with_cell() {
    // 3 levels of nested containers: Root -> Container -> Leaf
    let root: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::new())));
    root.borrow().set_geometry(Rect::new(0, 0, 400, 400));

    let container: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::new())));
    container.borrow().set_geometry(Rect::new(10, 10, 300, 300));

    let leaf: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::new())));
    leaf.borrow().set_geometry(Rect::new(5, 5, 100, 100));

    container.borrow_mut().add_child(leaf.clone());
    root.borrow_mut().add_child(container.clone());

    // Calling update_layout on root traverses all children with &self
    root.borrow().update_layout();

    // Verify leaf can trigger layout recalculation from within without panic
    leaf.borrow().update_layout();
    assert_eq!(leaf.borrow().geometry(), Rect::new(5, 5, 100, 100));
}

#[test]
fn test_command_queue_deduplication() {
    WidgetCommandQueue::clear();

    let widget: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::new())));
    let weak = Rc::downgrade(&widget);

    // Post multiple layout requests on the same widget
    WidgetCommandQueue::post(WidgetCommand::RequestLayout(weak.clone()));
    WidgetCommandQueue::post(WidgetCommand::RequestLayout(weak.clone()));
    WidgetCommandQueue::post(WidgetCommand::RequestLayout(weak.clone()));

    assert_eq!(WidgetCommandQueue::pending_count(), 3);

    // Flush should deduplicate and only invoke layout once
    WidgetCommandQueue::flush();
    assert_eq!(WidgetCommandQueue::pending_count(), 0);
}
