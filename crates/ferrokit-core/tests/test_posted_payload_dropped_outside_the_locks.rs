// SPDX-License-Identifier: LGPL-3.0-only
//! An event that is merged into a queued one is dropped after the queue and registry locks are
//! released: the `Drop` of its payload may post events (or unregister objects), which takes those
//! locks again on the same thread.

use ferrokit_core::event::{Event, EventCompressor, EventKind};
use ferrokit_core::event_loop::{post_event_to_thread, EventLoop};
use ferrokit_core::object::{ObjectId, ThreadId};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

/// Merges every event into the one queued for the same receiver.
struct MergeAll;

impl EventCompressor for MergeAll {
    fn try_compress(&self, _existing: &mut Event, _incoming: &Event, _receiver: ObjectId) -> bool {
        true
    }
}

/// A payload that posts an event when it is dropped.
struct PostWhenDropped {
    thread: ThreadId,
    drops: Arc<AtomicUsize>,
}

impl Drop for PostWhenDropped {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
        post_event_to_thread(self.thread, ObjectId(7), Event::new(EventKind::UpdateRequest));
    }
}

fn payload_event(payload: PostWhenDropped) -> Event {
    Event::new(EventKind::MetaCall(Box::new(move |_| {
        let _keep = &payload;
    })))
}

/// Runs `body` on a thread of its own and fails, instead of hanging, when it does not end.
fn finishes_within(limit: Duration, body: impl FnOnce() + Send + 'static) {
    let (done, wait) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        body();
        let _ = done.send(());
    });
    if wait.recv_timeout(limit).is_err() {
        panic!("did not finish within {limit:?}: an event is dropped while a lock of the post is held");
    }
    thread.join().unwrap();
}

#[test]
fn a_merged_event_is_dropped_after_the_registry_lock_is_released() {
    finishes_within(Duration::from_secs(30), || {
        let mut el = EventLoop::new();
        el.set_compressor(Arc::new(MergeAll));
        let thread = ThreadId::current();
        let drops = Arc::new(AtomicUsize::new(0));

        // The first event is queued; the second is merged into it and dropped by the post.
        post_event_to_thread(thread, ObjectId(7), Event::new(EventKind::UpdateRequest));
        post_event_to_thread(
            thread,
            ObjectId(7),
            payload_event(PostWhenDropped { thread, drops: Arc::clone(&drops) }),
        );

        assert_eq!(drops.load(Ordering::SeqCst), 1);
        drop(el);
    });
}

#[test]
fn a_merged_event_is_dropped_after_the_queue_lock_of_the_loop_is_released() {
    finishes_within(Duration::from_secs(30), || {
        let mut el = EventLoop::new();
        el.set_compressor(Arc::new(MergeAll));
        let thread = ThreadId::current();
        let drops = Arc::new(AtomicUsize::new(0));

        el.post_event(ObjectId(7), Event::new(EventKind::UpdateRequest));
        el.post_event(ObjectId(7), payload_event(PostWhenDropped { thread, drops: Arc::clone(&drops) }));

        assert_eq!(drops.load(Ordering::SeqCst), 1);
        drop(el);
    });
}
