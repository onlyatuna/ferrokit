// SPDX-License-Identifier: LGPL-3.0-only
//! A task posted to `WidgetCommandQueue` while no widget event is dispatched (a system setting
//! change, a timer slot) is run by the event loop, as a queued `invokeMethod` is
//! (`QCoreApplication::postEvent` wakes the dispatcher).

use ferrokit_core::event_loop::EventLoop;
use ferrokit_gui::geometry::primitives::Rect;
use ferrokit_widgets::command::{WidgetCommandQueue, delete_widget_later};
use ferrokit_widgets::widget::{EmptyWidget, WidgetRef};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

fn widget() -> WidgetRef {
    Rc::new(RefCell::new(Box::new(EmptyWidget::with_geometry(
        Rect::new(0, 0, 10, 10),
    ))))
}

#[test]
fn a_posted_task_runs_on_an_idle_loop_without_any_widget_event() {
    let el = EventLoop::new();
    WidgetCommandQueue::clear();
    let ran = Rc::new(Cell::new(false));
    let flag = Rc::clone(&ran);
    WidgetCommandQueue::post_task(move || flag.set(true));
    assert!(!ran.get(), "never inline");

    el.process_events(false);

    assert!(ran.get());
}

#[test]
fn many_posted_tasks_cost_one_wake_up_event() {
    let el = EventLoop::new();
    WidgetCommandQueue::clear();
    let count = Rc::new(Cell::new(0));
    for _ in 0..50 {
        let count = Rc::clone(&count);
        WidgetCommandQueue::post_task(move || count.set(count.get() + 1));
    }
    assert_eq!(el.queue().lock().unwrap().len(), 1);

    el.process_events(false);
    assert_eq!(count.get(), 50);

    // The event that ran cleared the claim: the next task wakes the loop again.
    let again = Rc::clone(&count);
    WidgetCommandQueue::post_task(move || again.set(again.get() + 1));
    assert_eq!(el.queue().lock().unwrap().len(), 1);
    el.process_events(false);
    assert_eq!(count.get(), 51);
}

#[test]
fn a_wake_up_discarded_with_its_loop_does_not_stop_later_ones() {
    let el = EventLoop::new();
    WidgetCommandQueue::clear();
    WidgetCommandQueue::post_task(|| {});
    drop(el);

    let el = EventLoop::new();
    let ran = Rc::new(Cell::new(false));
    let flag = Rc::clone(&ran);
    WidgetCommandQueue::post_task(move || flag.set(true));
    el.process_events(false);

    assert!(ran.get());
}

#[test]
fn deleting_a_borrowed_widget_later_does_not_keep_flush_running() {
    let _el = EventLoop::new();
    WidgetCommandQueue::clear();
    let w = widget();
    {
        let _in_its_handler = w.borrow_mut();
        delete_widget_later(&w);
        // Used to spin here for as long as the widget stayed borrowed.
        WidgetCommandQueue::flush();
        assert_eq!(WidgetCommandQueue::pending_count(), 1, "kept for later");
    }
    WidgetCommandQueue::flush();
    assert_eq!(WidgetCommandQueue::pending_count(), 0);
}

#[test]
fn a_task_waiting_for_a_borrow_is_tried_again_without_spinning_the_loop() {
    let el = EventLoop::new();
    WidgetCommandQueue::clear();
    let w = widget();
    let held = w.borrow_mut();
    delete_widget_later(&w);

    // The widget stays borrowed for 300 ms: the retry must not run the loop at every turn.
    let start = Instant::now();
    let mut busy_turns = 0;
    while start.elapsed() < Duration::from_millis(300) {
        if el.process_events(false) {
            busy_turns += 1;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(busy_turns < 60, "{busy_turns} turns with work");
    assert_eq!(WidgetCommandQueue::pending_count(), 1, "still waiting");

    // Once the borrow ends the loop gets to it by itself, with no widget event.
    drop(held);
    let start = Instant::now();
    while WidgetCommandQueue::pending_count() > 0 && start.elapsed() < Duration::from_secs(5) {
        el.process_events(false);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(WidgetCommandQueue::pending_count(), 0);
}
