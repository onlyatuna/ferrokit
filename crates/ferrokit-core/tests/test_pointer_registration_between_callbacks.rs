// SPDX-License-Identifier: LGPL-3.0-only
//! What the owner of a variable registered with `register_qobject_ptr` may do between callbacks.
//!
//! The registry keeps the pointer it was given. Under Stacked Borrows a read through the variable
//! leaves that pointer valid, and so does a write through the pointer that was registered; a
//! write through the variable itself does not (rule 7 in the `ferrokit_core::object` module
//! documentation). Tree Borrows accepts all three. The first two tests below must pass under
//! Miri with either model. The third is a control: under Stacked Borrows Miri must report
//! undefined behavior for it, under Tree Borrows it passes, and a normal build does not run it.
//!
//! Every test uses a concrete type. Miri does not check the uniqueness of `&mut` to a `!Unpin`
//! type, and `dyn QObject` is not `Unpin`.

use std::ptr::NonNull;

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::object::{
    register_qobject_ptr, unregister_qobject, with_object_mut, ObjectData, ObjectId, QObject,
};

struct Node {
    data: ObjectData,
    hits: u32,
}

impl Node {
    fn new() -> Self {
        Self {
            data: ObjectData::new(ObjectId::next()),
            hits: 0,
        }
    }
}

impl QObject for Node {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
    }
    fn event(&mut self, _event: &mut Event) -> bool {
        self.hits += 1;
        true
    }
}

/// Runs the object's `event` through the registry, the way a loop or a queued signal would.
fn callback(id: ObjectId) {
    let mut event = Event::new(EventKind::UpdateRequest);
    with_object_mut(id, |o| o.event(&mut event)).expect("the object is registered");
}

#[test]
fn reading_the_variable_between_callbacks_leaves_the_registered_pointer_usable() {
    let mut node = Node::new();
    let id = node.data.id();
    // SAFETY: `node` is a local on this thread, not moved or dropped before the unregistration
    // below; the pointer comes from `&raw mut` (no reference), and the test only reads `node.hits`
    // through the variable between callbacks, the allowed use (rule 7).
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut node)) };

    assert_eq!(node.hits, 0);
    callback(id);
    assert_eq!(node.hits, 1);
    callback(id);
    assert_eq!(node.hits, 2);

    // SAFETY: `node` is a local that outlives this call and is unmoved; both `callback`s have
    // returned, so no callback of it runs.
    unsafe { unregister_qobject(id) };
}

#[test]
fn writing_through_the_registered_pointer_between_callbacks_leaves_it_usable() {
    let mut node = Node::new();
    let id = node.data.id();
    let node_ptr = &raw mut node;
    // SAFETY: `node` is a local on this thread, not moved or dropped before the unregistration
    // below; the pointer comes from `&raw mut`, and the variable is only read afterwards.
    unsafe { register_qobject_ptr(NonNull::new_unchecked(node_ptr)) };

    // SAFETY: `node_ptr` is the `&raw mut node` pointer that was registered and `node` is still
    // alive and in place; no callback runs (the previous `callback` returned), so the write through
    // it does not overlap one, and writing through the registered pointer keeps it valid.
    unsafe { (*node_ptr).hits += 10 };
    callback(id);
    // SAFETY: as above: `node_ptr` is still the registered pointer, no callback is running, and
    // `node` has not been written through the variable.
    unsafe { (*node_ptr).hits += 10 };
    callback(id);
    assert_eq!(node.hits, 22);

    // SAFETY: `node` is a local that outlives this call and is unmoved; both `callback`s have
    // returned, so no callback of it runs.
    unsafe { unregister_qobject(id) };
}

/// Expected: undefined behavior under Stacked Borrows (the write below invalidates the registered
/// pointer; the next callback uses it), a pass under Tree Borrows.
#[test]
#[cfg_attr(
    not(miri),
    ignore = "undefined behavior under Stacked Borrows; run under Miri"
)]
// Deliberate contract violation (Miri negative control): rule 7 of the `object` module, writing
// the variable (`node.hits += 1`) after registering it by pointer, invalidates the registered
// pointer under Stacked Borrows. The `unsafe` blocks below therefore cannot carry a truthful
// `SAFETY` comment; Miri reports the next callback.
#[allow(clippy::undocumented_unsafe_blocks)]
fn control_writing_the_variable_after_registering_it_by_pointer() {
    let mut node = Node::new();
    let id = node.data.id();
    // (No SAFETY comment: rule 7 is broken on purpose, see the `allow` on this function.)
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut node)) };

    node.hits += 1;
    callback(id);
    assert_eq!(node.hits, 2);

    // (Covered by the `allow`: the registered pointer was invalidated by `node.hits += 1`, and
    // under Stacked Borrows Miri reports at the second `callback` above, before this call.)
    unsafe { unregister_qobject(id) };
}
