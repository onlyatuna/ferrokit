// SPDX-License-Identifier: LGPL-3.0-only
//! An owner and the registry's callbacks take turns using one object.
//!
//! [`OwnedObject`] and [`register_qobject_ptr`] give the registry the same pointer the owner uses,
//! so the owner may use the object between callbacks and move its owner around. Each test here is
//! a scenario that Miri also checks under Stacked Borrows and Tree Borrows; on a normal build they
//! check that the owner and the callbacks see each other's writes.

use std::cell::Cell;
use std::ptr::NonNull;
use std::rc::Rc;

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::object::{
    add_owned_child, dispatch_to_object, register_owned_qobject, register_qobject_ptr,
    reparent_owned_object, unregister_qobject, with_object, with_object_mut, ObjectData, ObjectId,
    OwnedObject, QObject,
};

// Miri does not check the uniqueness of `&mut` to a `!Unpin` type; the scenarios rely on it
// checking `Node`.
const _: () = {
    const fn assert_unpin<T: Unpin>() {}
    assert_unpin::<Node>();
    assert_unpin::<OwnedObject<Node>>();
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

/// Writes through the registry: the callback bumps `n`.
fn touch(id: ObjectId) {
    with_object_mut(id, |o| o.event(&mut Event::new(EventKind::UpdateRequest)))
        .expect("registered and not borrowed");
}

fn count(id: ObjectId) -> u32 {
    with_object(id, |o| {
        let any: &dyn std::any::Any = o;
        any.downcast_ref::<Node>().expect("a Node").n
    })
    .expect("registered and not borrowed")
}

fn gone(id: ObjectId) -> bool {
    with_object(id, |_| ()).is_none()
}

fn root() -> OwnedObject<Node> {
    // SAFETY: the owner stays on this thread and is dropped, which unregisters the object, when
    // no callback of it runs.
    unsafe { register_owned_qobject(Node::new()) }
}

fn add(parent: ObjectId) -> ObjectId {
    // SAFETY: the parent owns the child from here on and is dropped on this thread.
    unsafe { add_owned_child(parent, Box::new(Node::new())) }
        .unwrap_or_else(|(err, _)| panic!("attach refused: {err:?}"))
}

#[test]
fn the_owner_and_the_callbacks_see_each_others_writes() {
    let mut owner = root();
    let id = owner.data.id();
    touch(id);
    assert_eq!(owner.bump(), 2);
    touch(id);
    assert_eq!(owner.n, 3);
    assert_eq!(count(id), 3);
    drop(owner);
    assert!(gone(id));
}

#[test]
fn a_moved_owner_still_reaches_the_object_the_registry_calls() {
    let owner = root();
    let id = owner.data.id();
    touch(id);
    let mut moved = vec![owner];
    let mut owner = moved.pop().expect("one owner");
    assert_eq!(owner.bump(), 2);
    touch(id);
    assert_eq!(owner.n, 3);
}

#[test]
fn three_owned_levels_are_called_and_read_by_the_owner() {
    let owner = root();
    let (pid, cid) = (owner.data.id(), add(owner.data.id()));
    let gid = add(cid);
    touch(gid);
    touch(cid);
    touch(gid);

    let child = &owner.data.owned_children()[0];
    assert_eq!(OwnedObject::object_id(child), cid);
    assert_eq!(child.object_data().owned_children().len(), 1);
    assert_eq!(count(gid), 2);

    drop(owner);
    assert!(gone(pid) && gone(cid) && gone(gid));
}

#[test]
fn an_owned_child_moved_between_parents_stays_callable() {
    let mut a = root();
    let mut b = root();
    let (aid, bid) = (a.data.id(), b.data.id());
    let cid = add(aid);
    touch(cid);

    assert!(reparent_owned_object(cid, Some(bid)).unwrap().is_none());
    touch(cid);
    assert_eq!(count(cid), 2);
    a.bump();
    drop(a);
    touch(cid);
    b.bump();
    assert_eq!(b.data.owned_children().len(), 1);

    drop(b);
    assert!(gone(cid) && gone(aid) && gone(bid));
}

#[test]
fn a_child_released_as_an_owner_stays_registered_and_usable() {
    let a = root();
    let aid = a.data.id();
    let cid = add(aid);
    touch(cid);

    let mut released = reparent_owned_object(cid, None)
        .unwrap()
        .expect("ownership comes back to the caller");
    assert_eq!(OwnedObject::object_id(&released), cid);
    assert_eq!(released.object_data().parent(), None);
    touch(cid);
    // Through the concrete type: Miri does not check the uniqueness of `&mut dyn QObject`, since
    // `dyn QObject` is not `Unpin`, but it does check `&mut Node`.
    let any: &mut dyn std::any::Any = &mut *released;
    any.downcast_mut::<Node>().expect("a Node").bump();
    touch(cid);
    drop(a);
    assert!(!gone(cid), "the old parent no longer owns it");
    touch(cid);
    assert_eq!(count(cid), 5);

    drop(released);
    assert!(gone(cid));
}

#[test]
fn an_object_in_a_variable_registered_by_a_raw_pointer_can_be_read_between_callbacks() {
    let mut x = Node::new();
    let id = x.data.id();
    // SAFETY: `x` stays here, unmoved, until it is unregistered below, and the pointer is taken
    // without making a reference.
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut x as *mut dyn QObject)) };
    touch(id);
    assert_eq!(x.n, 1);
    touch(id);
    assert_eq!(x.n, 2);
    // SAFETY: no callback of `x` runs.
    unsafe { unregister_qobject(id) };
    assert!(gone(id));
}

#[test]
fn registering_again_with_the_owners_pointer_keeps_the_object_alive() {
    let mut owner = root();
    let id = owner.data.id();
    let alive = owner.data.liveness();
    touch(id);
    // SAFETY: the object stays in place while `owner` lives, and no callback of it runs.
    unsafe { register_qobject_ptr(OwnedObject::<Node>::as_non_null(&owner)) };
    assert!(alive.load(std::sync::atomic::Ordering::SeqCst));
    touch(id);
    assert_eq!(owner.bump(), 3);
    drop(owner);
    assert!(gone(id));
}

/// Drops its parent from inside its own callback, then keeps using `&mut self`.
struct Reactor {
    data: ObjectData,
    parent: Rc<Cell<Option<OwnedObject<Node>>>>,
    freed: Rc<Cell<bool>>,
    /// Right after the drop: whether the object is still alive, and whether it was freed.
    seen: Rc<Cell<Option<(bool, bool)>>>,
}

impl Drop for Reactor {
    fn drop(&mut self) {
        self.freed.set(true);
    }
}

impl QObject for Reactor {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> &mut ObjectData {
        &mut self.data
    }
    fn event(&mut self, _event: &mut Event) -> bool {
        drop(self.parent.take());
        self.seen
            .set(Some((self.data.is_alive(), self.freed.get())));
        true
    }
}

#[test]
fn a_registered_parent_dropped_in_its_childs_callback_frees_the_child_afterwards() {
    let parent = Rc::new(Cell::new(None));
    let freed = Rc::new(Cell::new(false));
    let seen = Rc::new(Cell::new(None));
    let owner = root();
    let pid = owner.data.id();
    parent.set(Some(owner));
    let reactor = Box::new(Reactor {
        data: ObjectData::new(ObjectId::next()),
        parent: Rc::clone(&parent),
        freed: Rc::clone(&freed),
        seen: Rc::clone(&seen),
    });
    let rid = reactor.data.id();
    // SAFETY: the parent owns the child from here on and is dropped on this thread.
    unsafe { add_owned_child(pid, reactor) }.unwrap_or_else(|(err, _)| panic!("{err:?}"));

    dispatch_to_object(rid, &mut Event::new(EventKind::UpdateRequest));

    assert_eq!(
        seen.get(),
        Some((false, false)),
        "inside the callback: destroyed for everything else, memory still there"
    );
    assert!(freed.get(), "freed once the callback returned");
    assert!(gone(rid) && gone(pid));
}
