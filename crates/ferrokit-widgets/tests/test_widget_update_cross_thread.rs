// SPDX-License-Identifier: LGPL-3.0-only
//! A repaint request reaches the widget's own thread.
//!
//! Qt reference:
//! - `widgets/kernel/qwidget.cpp:11367-11420`: `QWidget::update` posts `UpdateRequest` via
//!   `QCoreApplication::postEvent`, which posts to the receiver's thread (`d->threadData`).
//! - A `QWidget` is not reentrant: another thread does not call `update()` on it but posts the
//!   request (`QCoreApplication::postEvent`). In ferrokit a widget is `!Sync`, so another thread
//!   cannot call `Widget::update` at all; it posts an `UpdateRequest` with `post_event_to_thread`.

use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::event_loop::{EventLoop, post_event_to_thread};
use ferrokit_core::object::{
    ObjectData, ObjectId, QObject, query_object_thread, register_qobject_ptr, unregister_qobject,
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
fn an_update_request_posted_from_a_worker_thread_reaches_the_widget_on_its_thread() {
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
    // SAFETY: both pointers are taken with `&raw mut` from locals that stay on this thread,
    // unmoved and not used directly, until they are unregistered below.
    unsafe {
        register_qobject_ptr(NonNull::new_unchecked(&raw mut widget));
        register_qobject_ptr(NonNull::new_unchecked(&raw mut filter));
    }

    // The worker only has the id: it looks up the widget's thread and posts the request there.
    std::thread::spawn(move || {
        let thread = query_object_thread(widget_id).expect("the widget is registered");
        post_event_to_thread(thread, widget_id, Event::new(EventKind::UpdateRequest));
    })
    .join()
    .unwrap();

    el.process_events(false);
    let updated_on_widget_thread = seen.load(Ordering::SeqCst);

    // SAFETY: both objects are registered above, still alive and unmoved here, and no callback of
    // either runs (the event queue is drained); the filter is unregistered first, then the
    // widget, before either is dropped.
    unsafe {
        unregister_qobject(filter_id);
        unregister_qobject(widget_id);
    }

    assert!(
        updated_on_widget_thread,
        "an UpdateRequest posted from another thread must be delivered on the widget's thread"
    );
}

#[test]
fn update_of_an_unregistered_widget_without_a_window_posts_to_its_affinity_thread() {
    let el = EventLoop::new();

    // Not registered and in no window: `update` cannot use a window's render state and falls back
    // to posting `UpdateRequest` to the widget's thread affinity (this thread).
    let widget = EmptyWidget::new();
    widget.set_visible(true);
    let widget_id = widget.id();

    widget.update();

    let has_event =
        el.queue().lock().unwrap().events().iter().any(|pe| {
            pe.receiver == widget_id && matches!(pe.event.kind, EventKind::UpdateRequest)
        });
    assert!(
        has_event,
        "update() of an unregistered widget must post UpdateRequest to its thread affinity"
    );
}
