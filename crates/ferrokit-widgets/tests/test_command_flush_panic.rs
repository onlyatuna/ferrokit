// SPDX-License-Identifier: LGPL-3.0-only
//! A task that panics unwinds through `WidgetCommandQueue::flush` (the panic is the caller's to
//! contain), but the tasks and layout requests queued behind it must not be lost with it.

use ferrokit_core::event_loop::EventLoop;
use ferrokit_gui::geometry::primitives::Rect;
use ferrokit_widgets::command::WidgetCommandQueue;
use ferrokit_widgets::widget::{EmptyWidget, WidgetRef};
use std::cell::{Cell, RefCell};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;

fn widget() -> WidgetRef {
    Rc::new(RefCell::new(Box::new(EmptyWidget::with_geometry(
        Rect::new(0, 0, 10, 10),
    ))))
}

#[test]
fn the_commands_behind_a_panicking_task_run_at_the_next_flush() {
    let el = EventLoop::new();
    WidgetCommandQueue::clear();
    let order = Rc::new(RefCell::new(Vec::new()));
    let target = widget();
    let borrowed = widget();

    let log = Rc::clone(&order);
    WidgetCommandQueue::post_task(move || log.borrow_mut().push("first"));
    // A layout request that is already collected when the panic comes, and one kept because its
    // target is borrowed.
    WidgetCommandQueue::request_layout(&target);
    let held = borrowed.borrow_mut();
    WidgetCommandQueue::request_layout(&borrowed);
    WidgetCommandQueue::post_task(|| panic!("task failed"));
    let log = Rc::clone(&order);
    WidgetCommandQueue::post_task(move || log.borrow_mut().push("second"));
    let log = Rc::clone(&order);
    WidgetCommandQueue::post_task(move || log.borrow_mut().push("third"));

    assert!(catch_unwind(AssertUnwindSafe(WidgetCommandQueue::flush)).is_err());
    assert_eq!(*order.borrow(), ["first"], "stopped at the panic");
    assert_eq!(
        WidgetCommandQueue::pending_count(),
        4,
        "the layout request, the borrowed one, second and third are all still queued"
    );

    // Nothing else is needed: the loop's next turn flushes them, in their order.
    el.process_events(false);
    assert_eq!(*order.borrow(), ["first", "second", "third"]);
    assert_eq!(
        WidgetCommandQueue::pending_count(),
        1,
        "only the request of the borrowed widget waits"
    );
    drop(held);
    WidgetCommandQueue::flush();
    assert_eq!(WidgetCommandQueue::pending_count(), 0);
}

#[test]
fn a_task_posted_after_the_panic_still_runs_after_the_requeued_ones() {
    let el = EventLoop::new();
    WidgetCommandQueue::clear();
    let order = Rc::new(Cell::new(0u32));
    WidgetCommandQueue::post_task(|| panic!("task failed"));
    let seen = Rc::clone(&order);
    WidgetCommandQueue::post_task(move || seen.set(seen.get() * 10 + 1));
    assert!(catch_unwind(AssertUnwindSafe(WidgetCommandQueue::flush)).is_err());
    let seen = Rc::clone(&order);
    WidgetCommandQueue::post_task(move || seen.set(seen.get() * 10 + 2));
    el.process_events(false);
    assert_eq!(order.get(), 12);
}
