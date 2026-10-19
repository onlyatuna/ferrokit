// SPDX-License-Identifier: LGPL-3.0-only
//! Connection identity across signals.
//!
//! Qt keeps connections on the sender and disconnects every one that has a destroyed object as
//! receiver (`~QObject`, qobject.cpp:1046-1180). A connection of one signal must never be
//! confused with the connection of another signal.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use ferrokit_core::event::Event;
use ferrokit_core::object::{
    register_owned_qobject, unregister_qobject, ObjectData, ObjectId, OwnedObject, QObject,
};
use ferrokit_core::signal::Signal;

struct Receiver {
    data: ObjectData,
}

impl Receiver {
    fn owned() -> OwnedObject<Self> {
        // SAFETY: the owner stays on this thread; nothing touches the object while a callback of
        // it runs, and `destroy` unregisters it before the owner is dropped.
        unsafe {
            register_owned_qobject(Self {
                data: ObjectData::new(ObjectId::next()),
            })
        }
    }

    fn id(&self) -> ObjectId {
        self.data.id()
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
    let id = receiver.id();
    // SAFETY: no callback is running; the object is still alive for this call.
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
    a.connect_to(&*r1, slot_r1);
    b.connect_to(&*r2, slot_r2);

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
    let id_a = a.connect_to(&*r, slot);

    assert!(!b.disconnect(id_a), "b never owned this connection");
    a.emit(&1);
    assert_eq!(calls.load(Ordering::SeqCst), 1, "a's connection was removed");

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
    let id_a = a.connect_to(&*ra, slot_a);
    b.connect_to(&*rb, slot_b);

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
