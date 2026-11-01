// SPDX-License-Identifier: LGPL-3.0-only
//! Connection identity across signals.
//!
//! Qt keeps connections on the sender and disconnects every one that has a destroyed object as
//! receiver (`~QObject`, qobject.cpp:1046-1180). A connection of one signal must never be
//! confused with the connection of another signal.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ferrokit_core::event::Event;
use ferrokit_core::object::{
    ObjectData, ObjectId, OwnedObject, QObject, register_owned_qobject, unregister_qobject,
};
use ferrokit_core::signal::Signal;

struct Receiver {
    data: ObjectData,
}

impl Receiver {
    fn owned() -> OwnedObject<Self> {
        // SAFETY: the returned owner is a test local on this thread, used only for its id and
        // dropped by the test (after `destroy` unregistered it, or directly). The object's event
        // handler is empty and no event is sent to it, so no callback runs during those uses.
        unsafe {
            register_owned_qobject(Self {
                data: ObjectData::new(ObjectId::next()),
            })
        }
    }
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

/// Destroys a receiver the way its owner does: unregister (which severs its connections), drop.
fn destroy(receiver: OwnedObject<Receiver>) {
    let id = OwnedObject::object_id(&receiver);
    // SAFETY: `receiver` is a local owner still alive (it is dropped on the next line), so the
    // memory stays valid, and no callback of it runs: the test only emits signals, never events
    // to it. `unregister_qobject` is memory-neutral (it works from the registry record).
    unsafe { unregister_qobject(id) };
    drop(receiver);
}

fn counter() -> (Arc<AtomicUsize>, impl Fn(&i32) + Send + Sync + 'static) {
    let n = Arc::new(AtomicUsize::new(0));
    let n2 = Arc::clone(&n);
    (n, move |_| {
        n2.fetch_add(1, Ordering::SeqCst);
    })
}

#[test]
fn receiver_destroyed_disconnects_from_every_signal() {
    let a: Signal<i32> = Signal::new();
    let b: Signal<i32> = Signal::new();
    let r1 = Receiver::owned();
    let r2 = Receiver::owned();
    let (calls_r1, slot_r1) = counter();
    let (calls_r2, slot_r2) = counter();
    r1.with(|r| a.connect_to(r, slot_r1)).unwrap();
    r2.with(|r| b.connect_to(r, slot_r2)).unwrap();

    a.emit(&1);
    b.emit(&1);
    assert_eq!(calls_r1.load(Ordering::SeqCst), 1);
    assert_eq!(calls_r2.load(Ordering::SeqCst), 1);

    destroy(r1);
    a.emit(&2);
    assert_eq!(
        calls_r1.load(Ordering::SeqCst),
        1,
        "slot of a destroyed receiver ran"
    );

    // The other signal's receiver is untouched by the destruction of r1.
    b.emit(&2);
    assert_eq!(calls_r2.load(Ordering::SeqCst), 2);

    destroy(r2);
    b.emit(&3);
    assert_eq!(calls_r2.load(Ordering::SeqCst), 2);
}

#[test]
fn disconnect_with_another_signals_id_leaves_that_connection_intact() {
    let a: Signal<i32> = Signal::new();
    let b: Signal<i32> = Signal::new();
    let r = Receiver::owned();
    let (calls, slot) = counter();
    let id_a = r.with(|r| a.connect_to(r, slot)).unwrap();

    assert!(!b.disconnect(id_a), "b never owned this connection");
    a.emit(&1);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "a's connection was removed"
    );

    // The foreign disconnect must not have cost the connection its automatic disconnect.
    destroy(r);
    a.emit(&2);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "slot of a destroyed receiver ran"
    );
}

#[test]
fn disconnect_removes_only_its_own_connection() {
    let a: Signal<i32> = Signal::new();
    let b: Signal<i32> = Signal::new();
    let ra = Receiver::owned();
    let rb = Receiver::owned();
    let (calls_a, slot_a) = counter();
    let (calls_b, slot_b) = counter();
    let id_a = ra.with(|r| a.connect_to(r, slot_a)).unwrap();
    rb.with(|r| b.connect_to(r, slot_b)).unwrap();

    assert!(a.disconnect(id_a));
    a.emit(&1);
    b.emit(&1);
    assert_eq!(calls_a.load(Ordering::SeqCst), 0);
    assert_eq!(calls_b.load(Ordering::SeqCst), 1);

    // b's connection still has its automatic disconnect.
    destroy(rb);
    b.emit(&2);
    assert_eq!(calls_b.load(Ordering::SeqCst), 1);
    destroy(ra);
}
