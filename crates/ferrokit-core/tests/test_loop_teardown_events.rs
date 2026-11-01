// SPDX-License-Identifier: LGPL-3.0-only
//! What happens to the events still queued when a thread's last event loop is dropped.
//!
//! Nothing can turn the queue after that, so: a `DeferredDelete` is completed, as Qt does when a
//! thread finishes (`sendPostedEvents(nullptr, DeferredDelete)`); every other event is dropped
//! (a call that waits on it, like a `BlockingQueuedConnection` caller, wakes); and an event posted
//! afterwards through a handle that outlived the loop is dropped at once, not queued for good.
//! A payload or handler that panics is contained and does not stop the rest.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::event_loop::{EventLoop, get_thread_event_sender};
use ferrokit_core::object::{
    ObjectData, ObjectId, OwnedObject, QObject, ThreadId, query_object_thread,
    register_owned_qobject,
};

/// A call that does nothing, and holds `witness` until it is run or dropped.
fn call_holding(witness: &Arc<()>) -> Event {
    let held = Arc::clone(witness);
    Event::new(EventKind::MetaCallNoTarget(Box::new(move || drop(held))))
}

struct PanicOnDrop;

impl Drop for PanicOnDrop {
    fn drop(&mut self) {
        panic!("the payload of a queued call panics when it is dropped");
    }
}

/// A call whose payload panics when the call is dropped without having run.
fn call_with_panicking_drop() -> Event {
    let guard = PanicOnDrop;
    Event::new(EventKind::MetaCallNoTarget(Box::new(move || drop(guard))))
}

struct Victim {
    data: ObjectData,
    saw_delete: Arc<AtomicBool>,
    panic_on_delete: bool,
}

impl QObject for Victim {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }

    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
    }

    fn event(&mut self, event: &mut Event) -> bool {
        if matches!(event.kind, EventKind::DeferredDelete { .. }) {
            self.saw_delete.store(true, Ordering::SeqCst);
            if self.panic_on_delete {
                panic!("the handler of a DeferredDelete panics");
            }
            true
        } else {
            false
        }
    }
}

/// Registers a victim on this thread and asks for its deferred deletion, which queues one
/// `DeferredDelete` on the loop that exists now.
fn victim_with_delete_later(
    panic_on_delete: bool,
) -> (OwnedObject<Victim>, ObjectId, Arc<AtomicBool>) {
    let saw_delete = Arc::new(AtomicBool::new(false));
    // SAFETY: the returned owner is handed to the test, which keeps it on this thread and touches
    // it only with `with_mut` (here, before any loop runs) and `drop` (after teardown). The only
    // callback is the `DeferredDelete` delivered inside `drop(el)`, a point where the test is not
    // inside any use of the owner.
    let mut victim = unsafe {
        register_owned_qobject(Victim {
            data: ObjectData::new(ObjectId::next()),
            saw_delete: Arc::clone(&saw_delete),
            panic_on_delete,
        })
    };
    let id = OwnedObject::object_id(&victim);
    victim
        .with_mut(|v| v.delete_later())
        .expect("no callback of the victim runs");
    (victim, id, saw_delete)
}

/// A `DeferredDelete` still queued when the last loop of the thread is dropped is delivered and
/// completed: the handler sees it and the object is unregistered.
#[test]
fn deferred_delete_is_completed_when_the_last_loop_is_dropped() {
    let el = EventLoop::new();
    let (victim, id, saw_delete) = victim_with_delete_later(false);
    assert!(
        query_object_thread(id).is_some(),
        "the object is registered until then"
    );

    drop(el);

    assert!(
        saw_delete.load(Ordering::SeqCst),
        "the thread ended without delivering the DeferredDelete"
    );
    assert_eq!(
        query_object_thread(id),
        None,
        "deleteLater was dropped with the queue: the object is still registered"
    );
    drop(victim);
}

/// A handler that panics on its `DeferredDelete` does not take the teardown down with it. The
/// object is still unregistered, as when a loop turn delivers the event.
#[test]
fn panicking_deferred_delete_handler_is_contained_at_loop_teardown() {
    let el = EventLoop::new();
    let (victim, id, saw_delete) = victim_with_delete_later(true);

    let dropped = catch_unwind(AssertUnwindSafe(|| drop(el)));

    assert!(
        dropped.is_ok(),
        "the panic of the handler escaped from the drop of the loop"
    );
    assert!(saw_delete.load(Ordering::SeqCst), "the handler ran");
    assert_eq!(
        query_object_thread(id),
        None,
        "the object is unregistered all the same"
    );
    drop(victim);
}

/// A payload whose `Drop` panics must not stop the other queued events from being dropped, nor
/// escape from the drop of the loop (a panic out of a destructor that runs during unwinding
/// aborts the process).
#[test]
fn panicking_payload_drop_is_contained_and_the_rest_is_still_dropped() {
    let el = EventLoop::new();
    let sender = el.sender();
    let witness = Arc::new(());
    sender.post_event(ObjectId(0), call_with_panicking_drop());
    sender.post_event(ObjectId(0), call_holding(&witness));
    assert_eq!(Arc::strong_count(&witness), 2, "the call is queued");

    let dropped = catch_unwind(AssertUnwindSafe(|| drop(el)));

    assert!(
        dropped.is_ok(),
        "the panic of a payload escaped from the drop of the loop"
    );
    assert_eq!(
        Arc::strong_count(&witness),
        1,
        "the call behind the panicking one was not dropped"
    );
}

/// Handles on the queue that outlive the loop: an event posted through one after the loop is gone
/// has no loop to run it, so it is dropped at once and its waiter, if any, is released.
#[test]
fn post_through_a_handle_that_outlived_the_loop_is_dropped() {
    let el = EventLoop::new();
    let sender = el.sender();
    let handle = get_thread_event_sender(ThreadId::current()).expect("the loop is registered");
    drop(el);

    let through_sender = Arc::new(());
    sender.post_event(ObjectId(0), call_holding(&through_sender));
    let through_handle = Arc::new(());
    handle.post_event(ObjectId(0), call_holding(&through_handle));

    assert_eq!(
        Arc::strong_count(&through_sender),
        1,
        "an event posted through an EventSender after the loop ended stays queued for good"
    );
    assert_eq!(
        Arc::strong_count(&through_handle),
        1,
        "an event posted through an EventLoopHandle after the loop ended stays queued for good"
    );
}
