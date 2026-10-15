// SPDX-License-Identifier: LGPL-3.0-only
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use ferrokit_core::object::{register_qobject, unregister_qobject, ObjectData, ObjectId, QObject};
use ferrokit_core::signal::Signal;

struct Receiver {
    data: ObjectData,
}

impl QObject for Receiver {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> &mut ObjectData {
        &mut self.data
    }
}

fn slot_count(value: &i32) {
    COUNT.fetch_add(*value as usize, Ordering::SeqCst);
}

static COUNT: AtomicUsize = AtomicUsize::new(0);

#[test]
fn unique_to_rejects_duplicate_receiver_slot_pair() {
    COUNT.store(0, Ordering::SeqCst);
    let id = ObjectId::next();
    let mut rcv = Receiver {
        data: ObjectData::new(id),
    };
    // SAFETY: `rcv` stays on this thread and alive until unregistered below.
    unsafe { register_qobject(&mut rcv) };

    let signal: Signal<i32> = Signal::new();
    let first = signal.connect_unique_to(&rcv, slot_count);
    let second = signal.connect_unique_to(&rcv, slot_count);

    assert!(first.is_some(), "first unique connection must be made");
    assert!(second.is_none(), "duplicate receiver+slot must be rejected");

    signal.emit(&1);
    assert_eq!(
        COUNT.load(Ordering::SeqCst),
        1,
        "slot must run exactly once"
    );

    // SAFETY: `rcv` was registered above and is still alive.
    unsafe { unregister_qobject(id) };
}

#[test]
fn single_shot_runs_once_even_when_reentrantly_emitted() {
    let hits = Arc::new(AtomicUsize::new(0));
    let signal: Arc<Signal<i32>> = Arc::new(Signal::new());

    let hits_c = Arc::clone(&hits);
    let signal_c = Arc::clone(&signal);
    signal.connect_single_shot(move |_value: &i32| {
        let prior = hits_c.fetch_add(1, Ordering::SeqCst);
        // Re-entrant emit from inside the slot must not invoke it again. Re-emit only on the first
        // hit so a regression fails the assertion instead of recursing until the stack overflows.
        if prior == 0 {
            signal_c.emit(&0);
        }
    });

    signal.emit(&7);
    signal.emit(&7);

    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "single-shot must fire exactly once"
    );
    assert!(
        signal.is_empty(),
        "single-shot connection must be removed after firing"
    );
}

#[test]
fn single_shot_to_receiver_fires_once() {
    COUNT.store(0, Ordering::SeqCst);
    let id = ObjectId::next();
    let mut rcv = Receiver {
        data: ObjectData::new(id),
    };
    // SAFETY: `rcv` stays on this thread and alive until unregistered below.
    unsafe { register_qobject(&mut rcv) };

    let signal: Signal<i32> = Signal::new();
    signal.connect_single_shot_to(&rcv, slot_count);

    signal.emit(&3);
    signal.emit(&3);

    assert_eq!(
        COUNT.load(Ordering::SeqCst),
        3,
        "receiver slot must run once (value 3)"
    );

    // SAFETY: `rcv` was registered above and is still alive.
    unsafe { unregister_qobject(id) };
}

#[test]
fn connect_signal_forwards_emission_to_target_signal() {
    let hits = Arc::new(AtomicUsize::new(0));
    let source: Signal<i32> = Signal::new();
    let target: Signal<i32> = Signal::new();

    let hits_c = Arc::clone(&hits);
    target.connect(move |value: &i32| {
        hits_c.fetch_add(*value as usize, Ordering::SeqCst);
    });

    source.connect_signal(&target);
    source.emit(&5);

    assert_eq!(
        hits.load(Ordering::SeqCst),
        5,
        "source emission must reach target slot"
    );
}
