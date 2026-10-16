// SPDX-License-Identifier: LGPL-3.0-only
//! Controls for the Miri runs: each test uses the registry in a way that breaks Rust's aliasing
//! rules, and Miri must report undefined behavior for it under both Stacked Borrows and Tree
//! Borrows. A control that passes under Miri means the check has stopped seeing the problem, not
//! that the use is sound. A control failing under Miri is the expected result, not a failure of
//! the code under test.
//!
//! They show the limits of the registration functions kept for existing callers
//! (`register_qobject`, `register_boxed_qobject`) and of giving an owner up for a `Box`. They are
//! not run on a normal build, where the undefined behavior would go unnoticed.
//!
//! Every control uses a concrete type. Miri does not check the uniqueness of `&mut` or `Box` to a
//! `!Unpin` type, and `dyn QObject` is not `Unpin`, so the same misuse through a `Box<dyn QObject>`
//! (what `reparent_owned` returns) goes unreported and cannot serve as a control.

// The controls call the deprecated registration functions on purpose.
#![allow(deprecated)]

use std::ptr::NonNull;

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::object::{
    register_boxed_qobject, register_owned_qobject, register_qobject, register_qobject_ptr,
    unregister_qobject, with_object_mut, ObjectData, ObjectId, OwnedObject, QObject,
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

/// A write through the registry.
fn touch(id: ObjectId) {
    with_object_mut(id, |o| o.event(&mut Event::new(EventKind::UpdateRequest)))
        .expect("registered and not borrowed");
}

/// A write by the owner, through an explicit `&mut`. A method call such as `node.event(..)` would
/// make a two-phase borrow, which Stacked Borrows treats more leniently.
fn use_directly(o: &mut dyn QObject) {
    o.event(&mut Event::new(EventKind::UpdateRequest));
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
    use_directly(&mut *node);
    touch(id);
    use_directly(&mut *node);
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
    use_directly(&mut *node);
    touch(id);
    use_directly(&mut *node);
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
    touch(id);
    use_directly(&mut node);
    touch(id);
    use_directly(&mut node);
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
    use_directly(&mut *node);
    touch(id);
    use_directly(&mut *node);
}
