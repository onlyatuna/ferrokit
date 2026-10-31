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
    reparent_owned_object, set_parent_by_id, unregister_qobject, with_object, with_object_mut,
    ObjectData, ObjectId, OwnedObject, QObject,
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
    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
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
    let id = OwnedObject::object_id(&owner);
    touch(id);
    assert_eq!(owner.with_mut(|o| o.bump()), Some(2));
    touch(id);
    assert_eq!(owner.with(|o| o.n), Some(3));
    assert_eq!(count(id), 3);
    drop(owner);
    assert!(gone(id));
}

#[test]
fn a_moved_owner_still_reaches_the_object_the_registry_calls() {
    let owner = root();
    let id = OwnedObject::object_id(&owner);
    touch(id);
    let mut moved = vec![owner];
    let mut owner = moved.pop().expect("one owner");
    assert_eq!(owner.with_mut(|o| o.bump()), Some(2));
    touch(id);
    assert_eq!(owner.with(|o| o.n), Some(3));
}

#[test]
fn three_owned_levels_are_called_and_read_by_the_owner() {
    let owner = root();
    let pid = OwnedObject::object_id(&owner);
    let cid = add(pid);
    let gid = add(cid);
    touch(gid);
    touch(cid);
    touch(gid);

    owner
        .with(|p| {
            let child = &p.data.owned_children()[0];
            assert_eq!(OwnedObject::object_id(child), cid);
            assert_eq!(
                child
                    .with(|c| c.object_data().owned_children().len())
                    .unwrap(),
                1
            );
        })
        .unwrap();
    assert_eq!(count(gid), 2);

    drop(owner);
    assert!(gone(pid) && gone(cid) && gone(gid));
}

#[test]
fn an_owned_child_moved_between_parents_stays_callable() {
    let mut a = root();
    let mut b = root();
    let (aid, bid) = (OwnedObject::object_id(&a), OwnedObject::object_id(&b));
    let cid = add(aid);
    touch(cid);

    assert!(reparent_owned_object(cid, Some(bid)).unwrap().is_none());
    touch(cid);
    assert_eq!(count(cid), 2);
    a.with_mut(|o| o.bump()).unwrap();
    drop(a);
    touch(cid);
    b.with_mut(|o| o.bump()).unwrap();
    assert_eq!(b.with(|o| o.data.owned_children().len()), Some(1));

    drop(b);
    assert!(gone(cid) && gone(aid) && gone(bid));
}

#[test]
fn a_child_released_as_an_owner_stays_registered_and_usable() {
    let a = root();
    let aid = OwnedObject::object_id(&a);
    let cid = add(aid);
    touch(cid);

    let mut released = reparent_owned_object(cid, None)
        .unwrap()
        .expect("ownership comes back to the caller");
    assert_eq!(OwnedObject::object_id(&released), cid);
    assert_eq!(released.with(|r| r.object_data().parent()), Some(None));
    touch(cid);
    // Through the concrete type: Miri does not check the uniqueness of `&mut dyn QObject`, since
    // `dyn QObject` is not `Unpin`, but it does check `&mut Node`.
    released
        .with_mut(|r| {
            let any: &mut dyn std::any::Any = r;
            any.downcast_mut::<Node>().expect("a Node").bump()
        })
        .unwrap();
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
    let id = OwnedObject::object_id(&owner);
    let alive = owner.with(|o| o.data.liveness()).unwrap();
    touch(id);
    // SAFETY: the object stays in place while `owner` lives, and no callback of it runs.
    unsafe { register_qobject_ptr(OwnedObject::<Node>::as_non_null(&owner)) };
    assert!(alive.load(std::sync::atomic::Ordering::SeqCst));
    touch(id);
    assert_eq!(owner.with_mut(|o| o.bump()), Some(3));
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
    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
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
    let pid = OwnedObject::object_id(&owner);
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

type SeenFlags = (bool, bool, bool, bool);

/// Drops its own [`OwnedObject`] (either a released `OwnedObject<dyn QObject>` from
/// [`reparent_owned_object`] or a concrete `OwnedObject<SelfDropper>` from
/// [`register_owned_qobject`]) from inside its callback, then writes to `&mut self` and records
/// what it observes before returning.
struct SelfDropper {
    data: ObjectData,
    released_self: Rc<Cell<Option<OwnedObject<dyn QObject>>>>,
    typed_self: Rc<Cell<Option<OwnedObject<SelfDropper>>>>,
    freed: Rc<Cell<bool>>,
    descendant_id: Option<ObjectId>,
    /// `(self.data.is_alive(), gone(self_id), gone(descendant_id), self.freed.get())` right after
    /// the owner handle is dropped inside `event`.
    seen: Rc<Cell<Option<SeenFlags>>>,
    post_drop_writes: u32,
}

impl Drop for SelfDropper {
    fn drop(&mut self) {
        self.freed.set(true);
    }
}

impl QObject for SelfDropper {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
    }
    fn event(&mut self, _event: &mut Event) -> bool {
        drop(self.released_self.take());
        drop(self.typed_self.take());
        self.post_drop_writes += 1;
        let desc_gone = self.descendant_id.is_none_or(gone);
        self.seen.set(Some((
            self.data.is_alive(),
            gone(self.data.id()),
            desc_gone,
            self.freed.get(),
        )));
        true
    }
}

#[test]
fn a_released_owned_child_dropped_in_its_own_callback_is_freed_afterwards() {
    let parent = root();
    let pid = OwnedObject::object_id(&parent);
    let released_slot = Rc::new(Cell::new(None));
    let freed = Rc::new(Cell::new(false));
    let seen = Rc::new(Cell::new(None));

    let grand = Box::new(Node::new());
    let gid = grand.data.id();

    let child = Box::new(SelfDropper {
        data: ObjectData::new(ObjectId::next()),
        released_self: Rc::clone(&released_slot),
        typed_self: Rc::new(Cell::new(None)),
        freed: Rc::clone(&freed),
        descendant_id: Some(gid),
        seen: Rc::clone(&seen),
        post_drop_writes: 0,
    });
    let cid = child.data.id();
    // SAFETY: `parent` owns `child` on this thread; `reparent_owned_object` below is safe.
    unsafe { add_owned_child(pid, child) }.unwrap_or_else(|(err, _)| panic!("{err:?}"));
    // SAFETY: `cid` owns `grand` on this thread.
    unsafe { add_owned_child(cid, grand) }.unwrap_or_else(|(err, _)| panic!("{err:?}"));

    // Detach `cid` from `parent` through the safe public API and hand the returned
    // `OwnedObject<dyn QObject>` to `cid`'s callback slot.
    let detached = reparent_owned_object(cid, None)
        .expect("reparent_owned_object must succeed")
        .expect("releasing to None must return the OwnedObject");
    released_slot.set(Some(detached));

    assert!(dispatch_to_object(
        cid,
        &mut Event::new(EventKind::UpdateRequest)
    ));

    assert_eq!(
        seen.get(),
        Some((false, true, true, false)),
        "inside the callback: unregistered at once, but memory is not freed until the callback returns"
    );
    assert!(
        gone(gid),
        "descendant of a parked owner must be unregistered"
    );
    assert!(freed.get(), "freed once the callback returned");
    assert!(gone(cid));
}

#[test]
fn a_top_level_owned_object_dropped_in_its_own_callback_is_freed_afterwards() {
    let typed_slot = Rc::new(Cell::new(None));
    let freed = Rc::new(Cell::new(false));
    let seen = Rc::new(Cell::new(None));

    let child = Box::new(Node::new());
    let child_id = child.data.id();

    // SAFETY: the owner stays on this thread; dropping it inside its own callback parks the
    // allocation until the callback returns.
    let owner = unsafe {
        register_owned_qobject(SelfDropper {
            data: ObjectData::new(ObjectId::next()),
            released_self: Rc::new(Cell::new(None)),
            typed_self: Rc::clone(&typed_slot),
            freed: Rc::clone(&freed),
            descendant_id: Some(child_id),
            seen: Rc::clone(&seen),
            post_drop_writes: 0,
        })
    };
    let id = OwnedObject::object_id(&owner);
    // SAFETY: `owner` owns `child` on this thread.
    unsafe { add_owned_child(id, child) }.unwrap_or_else(|(err, _)| panic!("{err:?}"));
    typed_slot.set(Some(owner));
    assert!(dispatch_to_object(
        id,
        &mut Event::new(EventKind::UpdateRequest)
    ));

    assert_eq!(
        seen.get(),
        Some((false, true, true, false)),
        "inside the callback: unregistered at once, but memory is not freed until the callback returns"
    );
    assert!(freed.get(), "freed once the callback returned");
    assert!(gone(id) && gone(child_id));
}

/// Records `self.data.parent()` when its `Drop` runs, and counts `ChildRemoved` events it receives
/// as a parent.
struct ParentObserver {
    data: ObjectData,
    removed_children: Rc<Cell<usize>>,
    parent_seen_in_drop: Rc<Cell<Option<Option<ObjectId>>>>,
    self_slot: Rc<Cell<Option<OwnedObject<ParentObserver>>>>,
    owning_parent_slot: Rc<Cell<Option<OwnedObject<ParentObserver>>>>,
}

impl Drop for ParentObserver {
    fn drop(&mut self) {
        self.parent_seen_in_drop.set(Some(self.data.parent()));
    }
}

impl QObject for ParentObserver {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
    }
    fn event(&mut self, event: &mut Event) -> bool {
        match event.kind {
            EventKind::ChildRemoved { .. } => {
                self.removed_children.set(self.removed_children.get() + 1);
                true
            }
            EventKind::UpdateRequest => {
                drop(self.self_slot.take());
                drop(self.owning_parent_slot.take());
                true
            }
            _ => false,
        }
    }
}

#[test]
fn parked_drop_clears_parent_only_when_owned_by_parent_and_notifies_non_owning_parent() {
    // 1. Non-owning parent (`owned_by_parent == false`): a top-level `OwnedObject` linked to a
    //    live parent via `set_parent_by_id` and dropped inside its own callback keeps `parent()`
    //    readable in `T::drop`, unlinks from `parent.data.children()`, and delivers `ChildRemoved`
    //    when the callback returns.
    let parent_removed = Rc::new(Cell::new(0));
    // SAFETY: `non_owning_parent` stays on this thread and no callback of it runs during setup.
    let non_owning_parent = unsafe {
        register_owned_qobject(ParentObserver {
            data: ObjectData::new(ObjectId::next()),
            removed_children: Rc::clone(&parent_removed),
            parent_seen_in_drop: Rc::new(Cell::new(None)),
            self_slot: Rc::new(Cell::new(None)),
            owning_parent_slot: Rc::new(Cell::new(None)),
        })
    };
    let pid = OwnedObject::object_id(&non_owning_parent);

    let self_slot = Rc::new(Cell::new(None));
    let non_owned_parent_in_drop = Rc::new(Cell::new(None));
    // SAFETY: `child` stays on this thread; dropping it in its callback parks deallocation.
    let child = unsafe {
        register_owned_qobject(ParentObserver {
            data: ObjectData::new(ObjectId::next()),
            removed_children: Rc::new(Cell::new(0)),
            parent_seen_in_drop: Rc::clone(&non_owned_parent_in_drop),
            self_slot: Rc::clone(&self_slot),
            owning_parent_slot: Rc::new(Cell::new(None)),
        })
    };
    let cid = OwnedObject::object_id(&child);
    set_parent_by_id(cid, Some(pid)).unwrap();
    assert_eq!(
        non_owning_parent
            .with(|p| p.data.children().to_vec())
            .unwrap(),
        [cid]
    );
    self_slot.set(Some(child));

    assert!(dispatch_to_object(
        cid,
        &mut Event::new(EventKind::UpdateRequest)
    ));
    assert_eq!(
        non_owned_parent_in_drop.get(),
        Some(Some(pid)),
        "a non-owned object dropped in its callback must still see its live parent in Drop"
    );
    assert_eq!(
        parent_removed.get(),
        1,
        "ObjectData::drop must deliver ChildRemoved to the live non-owning parent"
    );
    assert!(
        non_owning_parent
            .with(|p| p.data.children().is_empty())
            .unwrap(),
        "ObjectData::drop must remove the child id from the live non-owning parent's children list"
    );
    drop(non_owning_parent);

    // 2. Owning parent (`owned_by_parent == true`): an owned child whose parent is dropped inside
    //    the child's callback has `clear_parent()` called before `T::drop` runs, so `parent()` is
    //    `None` in `T::drop`.
    let owning_parent_slot = Rc::new(Cell::new(None));
    // SAFETY: `owning_parent` stays on this thread and is dropped inside `owned_child`'s callback.
    let owning_parent = unsafe {
        register_owned_qobject(ParentObserver {
            data: ObjectData::new(ObjectId::next()),
            removed_children: Rc::new(Cell::new(0)),
            parent_seen_in_drop: Rc::new(Cell::new(None)),
            self_slot: Rc::new(Cell::new(None)),
            owning_parent_slot: Rc::new(Cell::new(None)),
        })
    };
    let owning_pid = OwnedObject::object_id(&owning_parent);
    owning_parent_slot.set(Some(owning_parent));

    let owned_parent_in_drop = Rc::new(Cell::new(None));
    let owned_child = Box::new(ParentObserver {
        data: ObjectData::new(ObjectId::next()),
        removed_children: Rc::new(Cell::new(0)),
        parent_seen_in_drop: Rc::clone(&owned_parent_in_drop),
        self_slot: Rc::new(Cell::new(None)),
        owning_parent_slot: Rc::clone(&owning_parent_slot),
    });
    let owned_cid = owned_child.data.id();
    // SAFETY: `owning_parent` owns `owned_child` on this thread.
    unsafe { add_owned_child(owning_pid, owned_child) }
        .unwrap_or_else(|(err, _)| panic!("{err:?}"));

    assert!(dispatch_to_object(
        owned_cid,
        &mut Event::new(EventKind::UpdateRequest)
    ));
    assert_eq!(
        owned_parent_in_drop.get(),
        Some(None),
        "an owned child parked by its dying parent's delete_children clears parent before Drop"
    );
}

/// Asks its own owner, from inside its callback, whether `with` and `is_borrowed` see the object.
struct Gate {
    data: ObjectData,
    owner: Rc<Cell<*const OwnedObject<Gate>>>,
    seen: Rc<Cell<Option<(bool, bool)>>>,
}

impl QObject for Gate {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
    }
    fn event(&mut self, _event: &mut Event) -> bool {
        // SAFETY: the test keeps the owner in place, and only a shared reference to the owner
        // (not to the object) is made.
        let owner = unsafe { &*self.owner.get() };
        self.seen.set(Some((
            owner.with(|_| ()).is_none(),
            OwnedObject::is_borrowed(owner),
        )));
        true
    }
}

#[test]
fn the_owner_cannot_be_used_while_the_objects_callback_runs() {
    let slot = Rc::new(Cell::new(std::ptr::null()));
    let seen = Rc::new(Cell::new(None));
    // SAFETY: the owner stays on this thread and is dropped when no callback of it runs.
    let mut owner = unsafe {
        register_owned_qobject(Gate {
            data: ObjectData::new(ObjectId::next()),
            owner: Rc::clone(&slot),
            seen: Rc::clone(&seen),
        })
    };
    slot.set(&owner as *const OwnedObject<Gate>);
    let id = OwnedObject::object_id(&owner);

    assert!(dispatch_to_object(
        id,
        &mut Event::new(EventKind::UpdateRequest)
    ));

    assert_eq!(
        seen.get(),
        Some((true, true)),
        "no access during the callback"
    );
    assert!(!OwnedObject::is_borrowed(&owner));
    assert_eq!(owner.with(|_| 1), Some(1));
    assert_eq!(owner.with_mut(|_| 2), Some(2));
}
