// SPDX-License-Identifier: LGPL-3.0-only
//! Reproductions, not regression tests. `connect_unique_to` decides that a connection is a
//! duplicate by comparing the address of the slot (`slot as usize`). Rust does not guarantee that
//! one function has one address, nor that two functions have two.
//!
//! - `cargo test --release -p ferrokit-core --test test_unique_connection_slot_identity --
//!   --ignored two_slots` fails where the linker folds functions with identical code into one
//!   (MSVC `/OPT:ICF`): the second slot is refused as a duplicate of the first.
//! - Under Miri, `the_same_slot_is_refused_a_second_time` fails: each cast of one function
//!   gives a new address, so the duplicate is accepted.

use ferrokit_core::object::{
    register_owned_qobject, unregister_qobject, ObjectData, ObjectId, OwnedObject, QObject,
};
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

fn receiver() -> OwnedObject<Receiver> {
    // SAFETY: the owner stays on this thread; no callback of the receiver runs.
    unsafe {
        register_owned_qobject(Receiver {
            data: ObjectData::new(ObjectId::next()),
        })
    }
}

// Two different slots with the same code, so that a linker may fold them into one.
fn slot_a(_: &i32) {}
fn slot_b(_: &i32) {}

#[test]
#[ignore = "reproduction of an open defect: slots are compared by address"]
fn two_slots_with_equal_code_are_both_connected() {
    let rcv = receiver();
    let id = rcv.data.id;
    let signal: Signal<i32> = Signal::new();

    let first = signal.connect_unique_to(&*rcv, slot_a);
    let second = signal.connect_unique_to(&*rcv, slot_b);

    assert!(first.is_some(), "the first connection is made");
    assert!(
        second.is_some(),
        "slot_b is a different slot from slot_a and must be connected"
    );
    // SAFETY: no callback of the receiver runs.
    unsafe { unregister_qobject(id) };
}

#[test]
#[cfg_attr(
    not(miri),
    ignore = "Miri reproduction: a function has a new address at each cast under Miri"
)]
fn the_same_slot_is_refused_a_second_time() {
    let rcv = receiver();
    let id = rcv.data.id;
    let signal: Signal<i32> = Signal::new();

    let first = signal.connect_unique_to(&*rcv, slot_a);
    let second = signal.connect_unique_to(&*rcv, slot_a);

    assert!(first.is_some(), "the first connection is made");
    assert!(second.is_none(), "the same receiver and slot is a duplicate");
    // SAFETY: no callback of the receiver runs.
    unsafe { unregister_qobject(id) };
}
