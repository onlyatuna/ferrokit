// SPDX-License-Identifier: LGPL-3.0-only
//! Controls for the Miri runs: each test uses the registry in a way that breaks Rust's aliasing
//! rules, and Miri must report undefined behavior for it under both Stacked Borrows and Tree
//! Borrows (a control whose own documentation names one model is reported by that model only). A
//! control that passes under Miri means the check has stopped seeing the problem, not that the
//! use is sound. A control failing under Miri is the expected result, not a failure of the code
//! under test.
//!
//! They show the limits of a pointer made from a reference, and that a registered child freed
//! before it is unregistered is reported. They are not run on a normal build, where the undefined
//! behavior would go unnoticed.
//!
//! Every control uses a concrete type. Miri does not check the uniqueness of `&mut` or `Box` to a
//! `!Unpin` type, and `dyn QObject` is not `Unpin`, so the same misuse through a `Box<dyn QObject>`
//! goes unreported and cannot serve as a control.

use std::mem::ManuallyDrop;
use std::ptr::NonNull;

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::object::{
    register_qobject_ptr, set_parent_by_id, unregister_qobject, with_object_mut, ObjectData,
    ObjectId, QObject,
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
    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
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
// Deliberate contract violation (rules 1 and 7, a pointer made from a `&mut` then a write through
// the variable): no truthful `SAFETY` comment exists; Miri reports it.
#[allow(clippy::undocumented_unsafe_blocks)]
#[cfg_attr(
    not(miri),
    ignore = "Miri control: expected to be reported as undefined behavior under Miri"
)]
fn a_pointer_made_from_a_reference_then_use_of_the_variable() {
    let mut node = Node::new();
    let id = node.data.id();
    let ptr = NonNull::from(&mut node as &mut dyn QObject);
    // Deliberate violation of rules 1 and 7 of the `object` module: the pointer comes from a
    // `&mut` reference, and `use_directly(&mut node)` writes through the variable, which
    // invalidates it. Miri reports the second `touch` (see the `allow`).
    unsafe { register_qobject_ptr(ptr) };
    touch(id);
    use_directly(&mut node);
    touch(id);
    use_directly(&mut node);
    // (Miri has already reported the second `touch`: the pointer was invalidated by
    // `use_directly`.)
    unsafe { unregister_qobject(id) };
}

/// A read of the variable between callbacks is not allowed either when the registered pointer was
/// made from a reference. (A pointer taken by `&raw mut x` survives such a read, see
/// `test_pointer_registration_between_callbacks`.) Expected: undefined behavior under Tree
/// Borrows only. The reference is a unique one there: the read through the variable freezes it,
/// and the next write through the registry is forbidden. Stacked Borrows only discards the
/// registered pointer on a write through the variable, so this passes under it, as does a read
/// that goes through a shared reference.
#[test]
// Deliberate contract violation (rule 1, a pointer made from a `&mut` then a read of the
// variable): no truthful `SAFETY` comment exists; Tree Borrows reports it.
#[allow(clippy::undocumented_unsafe_blocks)]
#[cfg_attr(
    not(miri),
    ignore = "Miri control: expected to be reported as undefined behavior under Miri"
)]
fn a_pointer_made_from_a_reference_then_a_read_of_the_variable() {
    let mut node = Node::new();
    let id = node.data.id();
    let ptr = NonNull::from(&mut node as &mut dyn QObject);
    // Deliberate violation of rule 1 of the `object` module: the registered pointer comes from a
    // `&mut` reference, not `&raw mut`, so the reads of `node.n` below invalidate it under Tree
    // Borrows, which reports the second `touch`. `node` stays unmoved on this thread (see the `allow`).
    unsafe { register_qobject_ptr(ptr) };
    touch(id);
    assert_eq!(node.n, 1);
    touch(id);
    assert_eq!(node.n, 2);
    // (Under Tree Borrows Miri has already reported the second `touch`: the registered pointer
    // came from a `&mut` reference and the read of `node.n` invalidated it.)
    unsafe { unregister_qobject(id) };
}

/// The memory of a registered child is freed without its drop running, so it is never
/// unregistered. Destroying the parent then reaches the freed child through the registry (the
/// parent's `delete_children` borrows each child it lists), a use after free that Miri reports.
#[test]
// Deliberate contract violation (rule 5, a registered child freed without unregistering): no
// truthful `SAFETY` comment exists for the registration; Miri reports the use after free.
#[allow(clippy::undocumented_unsafe_blocks)]
#[cfg_attr(
    not(miri),
    ignore = "Miri control: expected to be reported as undefined behavior under Miri"
)]
fn a_registered_child_freed_before_it_is_unregistered_then_the_parent_is_destroyed() {
    let parent = Node::new();
    let parent_id = parent.data.id();
    let parent_ptr = Box::into_raw(Box::new(parent));
    // SAFETY: the parent is a `Box::into_raw` allocation that is freed only at the end of this test,
    // after which nothing uses its id, and it is reached only through `parent_ptr` on this thread.
    unsafe { register_qobject_ptr(NonNull::new_unchecked(parent_ptr)) };

    let child = Node::new();
    let child_id = child.data.id();
    let child_ptr = Box::into_raw(Box::new(child));
    // Deliberate violation of rule 5 of the `object` module (the address must stay until
    // `unregister_qobject`): `child_ptr` is valid at registration, but the child is freed below
    // without being unregistered. Miri reports the use after free (see the `allow`).
    unsafe { register_qobject_ptr(NonNull::new_unchecked(child_ptr)) };
    set_parent_by_id(child_id, Some(parent_id)).expect("both are registered");

    // SAFETY: `child_ptr` came from `Box::into_raw(Box::new(Node))` above and is not freed
    // elsewhere; `ManuallyDrop<Node>` has the layout of `Node`, so the allocation is released with
    // the layout it was made with. Skipping `Node`'s drop means the child is never unregistered:
    // the registry keeps a pointer to freed memory (the contract broken on purpose).
    drop(unsafe { Box::from_raw(child_ptr.cast::<ManuallyDrop<Node>>()) });

    // The parent's drop borrows the child through the registry to release it.
    // SAFETY: `parent_ptr` came from `Box::into_raw` above, so it is a live, uniquely owned
    // allocation, and this is its only `Box::from_raw`. Dropping it runs `delete_children`, which
    // reaches the freed child through the registry: that is the use after free this control expects.
    drop(unsafe { Box::from_raw(parent_ptr) });
}
