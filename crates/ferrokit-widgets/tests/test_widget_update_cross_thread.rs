// SPDX-License-Identifier: LGPL-3.0-only
//! Widget::update() posts to the widget's thread affinity, not ThreadId::current().
//!
//! Qt reference:
//! - `widgets/kernel/qwidget.cpp:11367-11420`:
//!   `QWidget::update` posts `UpdateRequest` via `QCoreApplication::postEvent`,
//!   which posts to the receiver's thread (`d->threadData`), never the caller's thread.

use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::event_loop::EventLoop;
use ferrokit_core::object::{
    register_qobject_ptr, unregister_qobject, ObjectData, ObjectId, QObject,
};
use ferrokit_widgets::widget::{EmptyWidget, Widget};

struct SpyFilter {
    data: ObjectData,
    seen: Arc<AtomicBool>,
}

impl SpyFilter {
    fn new(seen: Arc<AtomicBool>) -> Self {
        Self {
            data: ObjectData::new(ObjectId::next()),
            seen,
        }
    }
}

impl QObject for SpyFilter {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
    }
    fn event_filter(&mut self, _watched: ObjectId, event: &mut Event) -> bool {
        if matches!(event.kind, EventKind::UpdateRequest) {
            self.seen.store(true, Ordering::SeqCst);
        }
        false
    }
}

#[test]
fn worker_thread_calling_widget_update_posts_to_widget_thread() {
    let el = EventLoop::new();

    let mut widget = EmptyWidget::new();
    widget.set_visible(true);
    let widget_id = widget.id();

    let seen = Arc::new(AtomicBool::new(false));
    let mut filter = SpyFilter::new(Arc::clone(&seen));
    let filter_id = filter.data.id();

    // Set up before registering, as the module documentation of `ferrokit_core::object` advises
    // (rule 7): a use of the variable after that can invalidate the pointer the registry holds.
    widget.object_data_mut().install_event_filter(filter_id);
    // SAFETY: both stay on this thread, unmoved, until unregistered below.
    unsafe {
        register_qobject_ptr(NonNull::new_unchecked(&raw mut widget));
        register_qobject_ptr(NonNull::new_unchecked(&raw mut filter));
    }

    // Call update() from a background worker thread
    let widget_ptr = &widget as *const EmptyWidget as usize;
    let handle = std::thread::spawn(move || {
        // SAFETY: `widget_ptr` is the address of `widget`, which outlives this thread: the main
        // thread joins it right below, so `widget` is neither moved nor dropped meanwhile. The
        // main thread does nothing else during the join, so this thread is the only one
        // touching `widget` (including its `Cell` fields that `update` writes), and spawn/join
        // order the accesses. Only `&self` access is made; the registry pointer taken above is
        // not used for writing meanwhile, and no callback of `widget` runs on any thread.
        let w = unsafe { &*(widget_ptr as *const EmptyWidget) };
        w.update();
    });
    handle.join().unwrap();

    // Process events on the main thread
    el.process_events(false);

    let updated_on_main = seen.load(Ordering::SeqCst);

    // SAFETY: both objects are registered above, are still alive and unmoved here, and no
    // callback of either runs (the worker thread has been joined and the event queue is
    // drained); the filter is unregistered first, then the widget, before either is dropped.
    unsafe {
        unregister_qobject(filter_id);
        unregister_qobject(widget_id);
    }

    assert!(
        updated_on_main,
        "Widget::update() from worker thread must reach the widget's thread event loop (qwidget.cpp:11367-11420)"
    );
}

#[test]
fn worker_thread_calling_unregistered_widget_update_posts_to_widget_affinity_thread() {
    let el = EventLoop::new();

    // An unregistered widget created on the main thread has object_data.thread_id() = main_thread
    let widget = EmptyWidget::new();
    widget.set_visible(true);
    let widget_id = widget.id();

    // Worker thread calls update()
    let widget_ptr = &widget as *const EmptyWidget as usize;
    let handle = std::thread::spawn(move || {
        // SAFETY: `widget_ptr` is the address of `widget`, which outlives this thread: the main
        // thread joins it right below, so `widget` is neither moved nor dropped meanwhile. The
        // main thread does nothing else during the join, so this thread is the only one
        // touching `widget` (including its `Cell` fields that `update` writes), and spawn/join
        // order the accesses. Only `&self` access is made, as `widget` is not borrowed mutably.
        let w = unsafe { &*(widget_ptr as *const EmptyWidget) };
        w.update();
    });
    handle.join().unwrap();

    // Check whether the main thread queue received the UpdateRequest
    let has_event =
        el.queue().lock().unwrap().events().iter().any(|pe| {
            pe.receiver == widget_id && matches!(pe.event.kind, EventKind::UpdateRequest)
        });

    assert!(
        has_event,
        "Unregistered widget update() from worker thread must post to widget's thread affinity"
    );
}
