// SPDX-License-Identifier: LGPL-3.0-only
//! Regression tests for `UniqueConnection` deduplication via explicit [`SlotId`] and [`SlotKey`].
//!
//! Comparing function pointer addresses (`fn(&T) as usize`) is not reliable in Rust:
//! - Release builds with identical-code folding (such as MSVC `/OPT:ICF`) can fold two distinct
//!   functions with identical bodies (`slot_a` and `slot_b`) to the same address.
//! - Miri can assign a different address at each function-item cast.
//!
//! [`Signal::connect_unique_with_key`] compares explicit [`SlotId`] or [`SlotKey::Named`] values
//! instead of function pointer addresses.

use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};

use ferrokit_core::object::{
    ObjectData, ObjectId, OwnedObject, QObject, register_owned_qobject, unregister_qobject,
};
use ferrokit_core::signal::{Signal, SlotId, SlotKey};

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

fn receiver() -> OwnedObject<Receiver> {
    // SAFETY: the returned owner is a test local on this thread, used only through `with`/
    // `object_id` and dropped by the test; `Receiver` has no event handler and no event is sent
    // to it, so no callback of it runs.
    unsafe {
        register_owned_qobject(Receiver {
            data: ObjectData::new(ObjectId::next()),
        })
    }
}

// Two different slots with identical machine code, so that a linker with `/OPT:ICF` folds them
// to the same function address in release builds.
fn slot_a(_: &i32) {}
fn slot_b(_: &i32) {}

#[test]
fn two_slots_with_equal_code_are_both_connected() {
    let rcv = receiver();
    let id = OwnedObject::object_id(&rcv);
    let signal: Signal<i32> = Signal::new();

    let key_a = SlotId::next();
    let key_b = SlotId::next();
    let first = rcv
        .with(|r| signal.connect_unique_with_key(r, key_a, slot_a))
        .unwrap();
    let second = rcv
        .with(|r| signal.connect_unique_with_key(r, key_b, slot_b))
        .unwrap();

    assert!(first.is_some(), "the first connection is made");
    assert!(
        second.is_some(),
        "slot_b has a distinct SlotId from slot_a and must be connected even under /OPT:ICF"
    );

    let named_first = rcv
        .with(|r| signal.connect_unique_with_key(r, SlotKey::Named("Receiver::a"), slot_a))
        .unwrap();
    let named_second = rcv
        .with(|r| signal.connect_unique_with_key(r, SlotKey::Named("Receiver::b"), slot_b))
        .unwrap();
    assert!(named_first.is_some());
    assert!(named_second.is_some());

    // SAFETY: `rcv` (the owner) is alive until the end of the test, so the memory stays valid; no
    // callback of the receiver runs (the test only connects slots, none is emitted).
    unsafe { unregister_qobject(id) };
}

#[test]
fn the_same_slot_is_refused_a_second_time() {
    let rcv = receiver();
    let id = OwnedObject::object_id(&rcv);
    let signal: Signal<i32> = Signal::new();
    let sum = Arc::new(AtomicI32::new(0));

    let slot_id = SlotId::next();
    let sum_first = Arc::clone(&sum);
    let first = rcv
        .with(|r| {
            signal.connect_unique_with_key(r, slot_id, move |v: &i32| {
                sum_first.fetch_add(*v, Ordering::SeqCst);
            })
        })
        .unwrap();
    let sum_second = Arc::clone(&sum);
    let second = rcv
        .with(|r| {
            signal.connect_unique_with_key(r, slot_id, move |v: &i32| {
                sum_second.fetch_add(*v * 10, Ordering::SeqCst);
            })
        })
        .unwrap();

    assert!(first.is_some(), "the first connection is made");
    assert!(
        second.is_none(),
        "the same receiver and SlotId is rejected as a duplicate"
    );

    let named_first = rcv
        .with(|r| signal.connect_unique_with_key(r, "Receiver::slot_a", slot_a))
        .unwrap();
    let named_dup = rcv
        .with(|r| signal.connect_unique_with_key(r, "Receiver::slot_a", slot_a))
        .unwrap();
    assert!(named_first.is_some());
    assert!(named_dup.is_none());

    signal.emit(&4);
    assert_eq!(sum.load(Ordering::SeqCst), 4);

    // Disconnecting the first connection allows re-connecting the same SlotId.
    assert!(signal.disconnect(first.unwrap()));
    let reconnected = rcv
        .with(|r| signal.connect_unique_with_key(r, slot_id, slot_a))
        .unwrap();
    assert!(
        reconnected.is_some(),
        "after disconnect, the same SlotId can be connected again"
    );

    // SAFETY: `rcv` (the owner) is alive until the end of the test, so the memory stays valid; no
    // callback of the receiver runs (the test only emits signals whose slots are plain functions).
    unsafe { unregister_qobject(id) };
}
