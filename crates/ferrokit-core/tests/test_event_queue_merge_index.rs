// SPDX-License-Identifier: LGPL-3.0-only
//! Posting to an event queue that holds many events that cannot be merged must not look at them,
//! and the events that can be merged must still be found, whatever has been queued, delivered or
//! purged before.
//!
//! The scenario behind it: a thread that never runs its loop collects thousands of posted calls
//! (`ObjectId(0)` receiver), and each further post used to scan all of them for something to merge
//! with. The tests count how often the compressor is offered an event, so they do not depend on
//! how fast the machine is.

use ferrokit_core::event::{CoreCompressor, Event, EventCompressor, EventKind, MergeKey};
use ferrokit_core::event_loop::EventLoop;
use ferrokit_core::object::{register_owned_qobject, ObjectData, ObjectId, OwnedObject, QObject};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

const CALLS: usize = if cfg!(miri) { 100 } else { 10_000 };

/// The core compressor, counting how many queued events it is offered.
struct Counting {
    offered: Arc<AtomicUsize>,
}

impl EventCompressor for Counting {
    fn try_compress(&self, existing: &mut Event, incoming: &Event, receiver: ObjectId) -> bool {
        self.offered.fetch_add(1, Ordering::SeqCst);
        CoreCompressor.try_compress(existing, incoming, receiver)
    }

    fn merge_key(&self, event: &Event) -> Option<MergeKey> {
        CoreCompressor.merge_key(event)
    }
}
struct MutateThenPanic {
    panic_once: std::sync::atomic::AtomicBool,
}

impl EventCompressor for MutateThenPanic {
    fn try_compress(&self, existing: &mut Event, _incoming: &Event, _receiver: ObjectId) -> bool {
        if self.panic_once.swap(false, Ordering::SeqCst) {
            existing.kind = EventKind::LayoutRequest;
            panic!("compressor panic");
        }
        CoreCompressor.try_compress(existing, _incoming, _receiver)
    }

    fn merge_key(&self, event: &Event) -> Option<MergeKey> {
        CoreCompressor.merge_key(event)
    }
}

#[test]
fn a_panicking_compressor_keeps_the_merge_index_consistent() {
    let mut el = EventLoop::new();
    el.set_compressor(Arc::new(MutateThenPanic {
        panic_once: std::sync::atomic::AtomicBool::new(true),
    }));
    let receiver = ObjectId::next();
    el.post_event(receiver, update_request());

    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        el.post_event(receiver, update_request());
    }));
    assert!(panic.is_err());

    // The callback changed the queued event before panicking. Its new key must be indexed.
    el.post_event(receiver, Event::new(EventKind::LayoutRequest));
    assert_eq!(queued(&el), 1);
}

struct Node {
    data: ObjectData,
}

impl QObject for Node {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
    }
}

fn node() -> OwnedObject<Node> {
    // SAFETY: the returned owner is a test local on this thread and is used only for its id and
    // dropped by the test. Callbacks of the node (during `send_posted_events`) run on this thread
    // while the test does not touch the owner, and a drop never happens inside a callback.
    unsafe {
        register_owned_qobject(Node {
            data: ObjectData::new(ObjectId::next()),
        })
    }
}

fn update_request() -> Event {
    Event::new(EventKind::UpdateRequest)
}

fn call(ran: &Arc<AtomicUsize>) -> Event {
    let ran = Arc::clone(ran);
    Event::new(EventKind::MetaCall(Box::new(move |_| {
        ran.fetch_add(1, Ordering::SeqCst);
    })))
}

fn counting_loop() -> (EventLoop, Arc<AtomicUsize>) {
    let offered = Arc::new(AtomicUsize::new(0));
    let mut el = EventLoop::new();
    el.set_compressor(Arc::new(Counting {
        offered: Arc::clone(&offered),
    }));
    (el, offered)
}

fn queued(el: &EventLoop) -> usize {
    el.queue().lock().unwrap().len()
}

#[test]
fn calls_are_never_offered_to_the_compressor_and_do_not_hide_what_can_merge() {
    let (el, offered) = counting_loop();
    let ran = Arc::new(AtomicUsize::new(0));
    let widget = node();
    let id = OwnedObject::object_id(&widget);

    el.post_event(id, update_request());
    for _ in 0..CALLS {
        el.post_event(ObjectId(0), call(&ran));
        el.post_event(id, call(&ran));
    }
    assert_eq!(
        offered.load(Ordering::SeqCst),
        0,
        "posting calls looked at the queue"
    );
    assert_eq!(queued(&el), 1 + 2 * CALLS);

    // The update request is behind every call now, and is offered exactly one candidate: itself.
    el.post_event(id, update_request());
    el.post_event(id, update_request());
    assert_eq!(offered.load(Ordering::SeqCst), 2);
    assert_eq!(
        queued(&el),
        1 + 2 * CALLS,
        "the update requests were merged"
    );

    assert_eq!(el.send_posted_events(), 1 + 2 * CALLS);
    assert_eq!(ran.load(Ordering::SeqCst), 2 * CALLS);
    assert_eq!(queued(&el), 0);
}

#[test]
fn an_event_that_was_delivered_is_not_merged_into() {
    let (el, offered) = counting_loop();
    let ran = Arc::new(AtomicUsize::new(0));
    let (a, b) = (node(), node());
    let (a_id, b_id) = (OwnedObject::object_id(&a), OwnedObject::object_id(&b));

    el.post_event(a_id, update_request());
    for _ in 0..CALLS {
        el.post_event(ObjectId(0), call(&ran));
    }
    el.post_event(b_id, update_request());
    assert_eq!(el.send_posted_events(), CALLS + 2);
    assert_eq!(queued(&el), 0);

    // The queue is empty again; nothing of the delivered events is left to merge into.
    el.post_event(a_id, update_request());
    el.post_event(b_id, update_request());
    assert_eq!(queued(&el), 2);
    assert_eq!(offered.load(Ordering::SeqCst), 0);

    // And the new ones are merged into, as before.
    el.post_event(b_id, update_request());
    assert_eq!(queued(&el), 2);
    assert_eq!(offered.load(Ordering::SeqCst), 1);
}

#[test]
fn destroying_an_object_removes_its_events_from_the_index() {
    let (el, offered) = counting_loop();
    let ran = Arc::new(AtomicUsize::new(0));
    let (doomed, kept) = (node(), node());
    let (doomed_id, kept_id) = (
        OwnedObject::object_id(&doomed),
        OwnedObject::object_id(&kept),
    );

    el.post_event(doomed_id, update_request());
    el.post_event(kept_id, update_request());
    for _ in 0..CALLS {
        el.post_event(ObjectId(0), call(&ran));
    }
    el.post_event(doomed_id, Event::new(EventKind::LayoutRequest));
    el.post_event(kept_id, Event::new(EventKind::LayoutRequest));
    assert_eq!(queued(&el), CALLS + 4);

    // The destruction purges what was posted to the object; the events behind move forward.
    drop(doomed);
    assert_eq!(queued(&el), CALLS + 2);

    el.post_event(kept_id, update_request());
    el.post_event(kept_id, Event::new(EventKind::LayoutRequest));
    assert_eq!(
        queued(&el),
        CALLS + 2,
        "the events of the other object still merge"
    );
    assert_eq!(offered.load(Ordering::SeqCst), 2);

    // Nothing of the destroyed object is left for a later post to its id to merge into.
    el.post_event(doomed_id, update_request());
    assert_eq!(queued(&el), CALLS + 3);
    assert_eq!(offered.load(Ordering::SeqCst), 2);
}
