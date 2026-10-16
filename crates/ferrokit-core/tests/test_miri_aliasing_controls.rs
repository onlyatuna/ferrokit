// SPDX-License-Identifier: LGPL-3.0-only
//! Controls for the Miri runs: each test uses the registry in a way that breaks Rust's aliasing
//! rules, and Miri must report undefined behavior for it under both Stacked Borrows and Tree
//! Borrows. A control that passes under Miri means the check has stopped seeing the problem, not
//! that the use is sound. A control failing under Miri is the expected result, not a failure of
//! the code under test.
//!
//! They show the limits of the registration functions kept for existing callers
//! (`register_qobject`, `register_boxed_qobject`, a `Box` released by `reparent_owned`). They are
//! not run on a normal build, where the undefined behavior would go unnoticed.

use std::ptr::NonNull;

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::object::{
    add_owned_child, register_boxed_qobject, register_owned_qobject, register_qobject,
    register_qobject_ptr, reparent_owned, unregister_qobject, with_object_mut, ObjectData,
    ObjectId, OwnedObject, QObject,
};

struct Node {
    data: ObjectData,
    n: u32,
}

impl Node {
    fn new() -> Self {
        Self {
            data: ObjectData::new(ObjectId::next()),
            n: 0,
        }
    }

    fn bump(&mut self) -> u32 {
        self.n += 1;
        self.n
    }
}

impl QObject for Node {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> &mut ObjectData {
        &mut self.data
    }
    fn event(&mut self, _event: &mut Event) -> bool {
        self.n += 1;
        false
    }
}

fn touch(id: ObjectId) {
    with_object_mut(id, |o| o.event(&mut Event::new(EventKind::UpdateRequest)))
        .expect("registered and not borrowed");
}

#[test]
#[cfg_attr(
    not(miri),
    ignore = "Miri control: expected to be reported as undefined behavior under Miri"
)]
fn register_qobject_then_use_of_the_box_between_callbacks() {
    let mut node = Box::new(Node::new());
    let id = node.data.id;
    // SAFETY: the Box stays in place on this thread until it is unregistered below.
    unsafe { register_qobject(&mut *node) };
    touch(id);
    node.bump();
    touch(id);
    // SAFETY: no callback runs.
    unsafe { unregister_qobject(id) };
}

#[test]
#[cfg_attr(
    not(miri),
    ignore = "Miri control: expected to be reported as undefined behavior under Miri"
)]
fn register_boxed_qobject_then_use_of_the_box_between_callbacks() {
    // SAFETY: the Box stays in place on this thread until it is unregistered below.
    let (id, mut node) = unsafe { register_boxed_qobject(Box::new(Node::new())) };
    touch(id);
    node.bump();
    touch(id);
    // SAFETY: no callback runs.
    unsafe { unregister_qobject(id) };
}

#[test]
#[cfg_attr(
    not(miri),
    ignore = "Miri control: expected to be reported as undefined behavior under Miri"
)]
fn a_pointer_made_from_a_reference_then_use_of_the_variable() {
    let mut node = Node::new();
    let id = node.data.id;
    let ptr = NonNull::from(&mut node as &mut dyn QObject);
    // SAFETY: `node` stays here until it is unregistered below.
    unsafe { register_qobject_ptr(ptr) };
    node.bump();
    touch(id);
    // SAFETY: no callback runs.
    unsafe { unregister_qobject(id) };
}

#[test]
#[cfg_attr(
    not(miri),
    ignore = "Miri control: expected to be reported as undefined behavior under Miri"
)]
fn an_owner_turned_into_a_box_then_use_of_the_box_between_callbacks() {
    // SAFETY: the owner stays on this thread and is dropped when no callback runs.
    let owner = unsafe { register_owned_qobject(Node::new()) };
    let id = owner.data.id;
    touch(id);
    let mut node = OwnedObject::into_box(owner);
    node.bump();
    touch(id);
}

#[test]
#[cfg_attr(
    not(miri),
    ignore = "Miri control: expected to be reported as undefined behavior under Miri"
)]
fn a_child_released_by_reparent_owned_then_use_of_the_box_between_callbacks() {
    // SAFETY: as above.
    let parent = unsafe { register_owned_qobject(Node::new()) };
    // SAFETY: the parent owns the child until it is released below.
    let child_id = unsafe { add_owned_child(parent.data.id, Box::new(Node::new())) }
        .unwrap_or_else(|(err, _)| panic!("attach refused: {err:?}"));
    let mut released = reparent_owned(child_id, None)
        .unwrap()
        .expect("ownership comes back to the caller");
    released.event(&mut Event::new(EventKind::UpdateRequest));
    touch(child_id);
    drop(parent);
}
