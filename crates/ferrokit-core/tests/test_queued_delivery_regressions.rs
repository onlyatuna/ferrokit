// SPDX-License-Identifier: LGPL-3.0-only
//! Regression reproductions for queued delivery.
//!
//! 1. Queued connections without a receiver (`connect_queued`, receiver `ObjectId(0)`) must keep
//!    delivering across event-loop turns. Delivering a fire-and-forget sentinel call must not cut
//!    them.
//! 2. A queued slot whose connection names a real registered receiver must be able to reach that
//!    receiver through `with_object_mut`, the same as a direct slot can.

use ferrokit_core::event_loop::EventLoop;
use ferrokit_core::object::{
    register_owned_qobject, with_object_mut, ObjectData, OwnedObject, QObject, ThreadId,
};
use ferrokit_core::signal::{ConnectionType, Signal};
use ferrokit_core::timer::Timer;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

struct Receiver {
    data: ObjectData,
}

impl QObject for Receiver {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
    }
}

fn counting_slot() -> (Arc<AtomicUsize>, impl Fn(&i32) + Send + Sync + 'static) {
    let n = Arc::new(AtomicUsize::new(0));
    let slot = {
        let n = Arc::clone(&n);
        move |_: &i32| {
            n.fetch_add(1, Ordering::SeqCst);
        }
    };
    (n, slot)
}

/// Two `connect_queued` slots, one emit per event-loop turn: each slot must run once per emit.
#[test]
fn connect_queued_survives_sentinel_delivery_between_emits() {
    let signal: Signal<i32> = Signal::new();
    let thread = ThreadId::current();
    let (a, slot_a) = counting_slot();
    let (b, slot_b) = counting_slot();
    let _ca = signal.connect_queued(thread, slot_a);
    let _cb = signal.connect_queued(thread, slot_b);

    let event_loop = EventLoop::new();
    signal.emit(&1);
    event_loop.process_events(false);
    signal.emit(&2);
    event_loop.process_events(false);

    assert_eq!(
        (a.load(Ordering::SeqCst), b.load(Ordering::SeqCst)),
        (2, 2),
        "each queued slot must run once per emit; delivering the ObjectId(0) sentinel must not \
         disconnect connect_queued connections"
    );
}

/// Same as above, but the sentinel comes from a `Timer::single_shot(0, ..)` posted between the
/// two emits, as an application would do.
#[test]
fn connect_queued_survives_single_shot_zero_between_emits() {
    let signal: Signal<i32> = Signal::new();
    let thread = ThreadId::current();
    let (a, slot_a) = counting_slot();
    let _ca = signal.connect_queued(thread, slot_a);

    let fired = Arc::new(AtomicUsize::new(0));
    let event_loop = EventLoop::new();

    signal.emit(&1);
    {
        let fired = Arc::clone(&fired);
        Timer::single_shot(0, move || {
            fired.fetch_add(1, Ordering::SeqCst);
        });
    }
    event_loop.process_events(false);
    signal.emit(&2);
    event_loop.process_events(false);

    assert_eq!(fired.load(Ordering::SeqCst), 1, "single_shot(0) must fire");
    assert_eq!(
        a.load(Ordering::SeqCst),
        2,
        "queued slot must run for the emit made after a single_shot(0) was delivered"
    );
}

/// A queued slot bound to a registered receiver must see that receiver via `with_object_mut`,
/// as a direct slot bound to the same receiver does.
#[test]
fn queued_slot_can_access_its_registered_receiver() {
    let thread = ThreadId::current();
    // SAFETY: the `OwnedObject` stays on this thread; no callback of the receiver runs.
    let rcv = unsafe {
        register_owned_qobject(Receiver {
            data: ObjectData::with_auto_id(),
        })
    };
    let rid = OwnedObject::object_id(&rcv);

    let queued_signal: Signal<i32> = Signal::new();
    let queued_seen = Arc::new(AtomicUsize::new(0));
    {
        let queued_seen = Arc::clone(&queued_seen);
        queued_signal.connect_object(rid, thread, ConnectionType::Queued, move |_| {
            if with_object_mut(rid, |_| ()).is_some() {
                queued_seen.fetch_add(1, Ordering::SeqCst);
            }
        });
    }

    let direct_signal: Signal<i32> = Signal::new();
    let direct_seen = Arc::new(AtomicUsize::new(0));
    {
        let direct_seen = Arc::clone(&direct_seen);
        direct_signal.connect_object(rid, thread, ConnectionType::Direct, move |_| {
            if with_object_mut(rid, |_| ()).is_some() {
                direct_seen.fetch_add(1, Ordering::SeqCst);
            }
        });
    }

    let event_loop = EventLoop::new();
    queued_signal.emit(&1);
    event_loop.process_events(false);
    direct_signal.emit(&1);

    assert_eq!(
        direct_seen.load(Ordering::SeqCst),
        1,
        "control: direct slot must reach the receiver"
    );
    assert_eq!(
        queued_seen.load(Ordering::SeqCst),
        1,
        "queued slot must reach its registered receiver via with_object_mut, as the direct slot does"
    );
    drop(rcv);
}
