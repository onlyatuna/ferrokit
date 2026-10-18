// SPDX-License-Identifier: LGPL-3.0-only
//! Connection changes made while a signal is being emitted.
//!
//! Qt's `doActivate` (qobject.cpp:4269) walks the sender's connection list and re-checks each
//! connection's `receiver` immediately before calling it, so a connection that is severed by an
//! earlier slot of the same emission is never called. A connection made during an emission does
//! not take part in that emission.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use ferrokit_core::event::Event;
use ferrokit_core::object::{
    register_owned_qobject, unregister_qobject, ObjectData, ObjectId, QObject,
};
use ferrokit_core::signal::{ConnectionId, Signal};

fn counter() -> (Arc<AtomicUsize>, impl Fn(&i32) + Send + Sync + 'static) {
    let count = Arc::new(AtomicUsize::new(0));
    let slot_count = Arc::clone(&count);
    (count, move |_: &i32| {
        slot_count.fetch_add(1, Ordering::SeqCst);
    })
}

/// `Mutex<Option<..>>` cell so a slot can refer to ids that only exist after it was connected.
fn cell<T>() -> Arc<Mutex<Option<T>>> {
    Arc::new(Mutex::new(None))
}

#[test]
fn slot_disconnected_during_emit_is_not_called() {
    let signal = Signal::<i32>::new();
    let victim_id = cell::<ConnectionId>();

    let (calls, victim) = counter();
    let killer = {
        let signal = signal.clone();
        let victim_id = Arc::clone(&victim_id);
        move |_: &i32| {
            let id = victim_id.lock().unwrap().expect("victim connected");
            // Second emission: already gone, so this reports false; that is fine.
            signal.disconnect(id);
        }
    };
    signal.connect(killer);
    *victim_id.lock().unwrap() = Some(signal.connect(victim));

    signal.emit(&1);

    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "a slot disconnected earlier in the same emission must not run"
    );
    signal.emit(&2);
    assert_eq!(calls.load(Ordering::SeqCst), 0, "and stays disconnected");

    // Break strong reference cycle between killer closure capturing signal and SignalInner
    signal.disconnect_all();
}

#[test]
fn disconnect_all_during_emit_stops_the_remaining_slots() {
    let signal = Signal::<i32>::new();
    let (calls, later) = counter();
    {
        let signal = signal.clone();
        signal.clone().connect(move |_| signal.disconnect_all());
    }
    signal.connect(later);

    signal.emit(&1);

    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn disconnect_receiver_during_emit_stops_that_receivers_slots() {
    let signal = Signal::<i32>::new();
    let receiver = ObjectId::next();
    let (calls, slot) = counter();
    {
        let signal = signal.clone();
        signal.clone().connect(move |_| {
            assert_eq!(signal.disconnect_receiver(receiver), 1);
        });
    }
    signal.connect_direct_object(receiver, slot);

    signal.emit(&1);

    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn scoped_connection_dropped_during_emit_stops_its_slot() {
    let signal = Signal::<i32>::new();
    let (calls, slot) = counter();
    let scoped = Arc::new(Mutex::new(None));
    {
        let scoped = Arc::clone(&scoped);
        signal.connect(move |_| {
            scoped.lock().unwrap().take();
        });
    }
    *scoped.lock().unwrap() = Some(signal.connect_scoped(slot));

    signal.emit(&1);

    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

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
    fn as_qobject_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn as_qobject_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
    fn event(&mut self, _event: &mut Event) -> bool {
        false
    }
}

#[test]
fn receiver_destroyed_during_emit_is_not_called() {
    let signal = Signal::<i32>::new();
    // SAFETY: the owner stays on this thread; nothing touches the object while a callback of it
    // runs, and the slot below unregisters it before the owner is dropped.
    let receiver = unsafe {
        register_owned_qobject(Receiver {
            data: ObjectData::with_auto_id(),
        })
    };
    let receiver_id = receiver.data.id();

    let (calls, slot) = counter();
    signal.connect(move |_| {
        // SAFETY: no callback is active for the receiver; same registration thread.
        unsafe { unregister_qobject(receiver_id) };
    });
    signal.connect_direct_object(receiver_id, slot);

    signal.emit(&1);

    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "a receiver destroyed by an earlier slot must not be called"
    );
    drop(receiver);
}

#[test]
fn slot_connected_during_emit_does_not_run_in_that_emit() {
    let signal = Signal::<i32>::new();
    let (calls, late) = counter();
    let late = Arc::new(Mutex::new(Some(late)));
    {
        let signal = signal.clone();
        let late = Arc::clone(&late);
        signal.clone().connect(move |_| {
            if let Some(slot) = late.lock().unwrap().take() {
                signal.connect(slot);
            }
        });
    }

    signal.emit(&1);
    assert_eq!(calls.load(Ordering::SeqCst), 0, "not part of the running emission");

    signal.emit(&2);
    assert_eq!(calls.load(Ordering::SeqCst), 1, "part of the next emission");

    // Break strong reference cycle between dynamic connector closure and SignalInner
    signal.disconnect_all();
}

#[test]
fn slot_disconnecting_itself_still_finishes_and_later_slots_run() {
    let signal = Signal::<i32>::new();
    let own_id = cell::<ConnectionId>();
    let self_calls = Arc::new(AtomicUsize::new(0));
    let id = {
        let signal = signal.clone();
        let own_id = Arc::clone(&own_id);
        let self_calls = Arc::clone(&self_calls);
        signal.clone().connect(move |_| {
            self_calls.fetch_add(1, Ordering::SeqCst);
            let id = own_id.lock().unwrap().expect("own id set");
            assert!(signal.disconnect(id));
        })
    };
    *own_id.lock().unwrap() = Some(id);
    let (later_calls, later) = counter();
    signal.connect(later);

    signal.emit(&1);
    signal.emit(&2);

    assert_eq!(self_calls.load(Ordering::SeqCst), 1, "disconnected after its first run");
    assert_eq!(later_calls.load(Ordering::SeqCst), 2, "unrelated slot unaffected");
}
