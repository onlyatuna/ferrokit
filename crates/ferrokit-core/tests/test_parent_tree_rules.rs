// SPDX-License-Identifier: LGPL-3.0-only
//! Parent changes never make an object its own ancestor, and a parent that goes away leaves its
//! non-owned children without a parent.
//!
//! Qt's release build lets `setParent` build a cycle and documents that as undefined behavior;
//! here the change is refused. Qt deletes every child with its parent; here a child whose `Box`
//! is not held by the parent (it was attached with `set_parent`) stays alive and loses the parent.

use std::cell::RefCell;
use std::rc::Rc;

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::object::{
    dispatch_to_object, move_to_thread_by_id, register_owned_qobject, set_parent, set_parent_by_id,
    unregister_qobject, MoveByIdError, ObjectData, ObjectId, OwnedObject, QObject, ReparentError,
    ThreadId,
};

/// An `ObjectData` that lets `move_to_thread_by_id` move its object.
fn movable_data(id: ObjectId) -> ObjectData {
    // SAFETY: `Node` holds an `ObjectData` and a `Vec<String>`, both `Send`; no `Rc`, no `Cell`
    // shared with a handle left behind, no thread-local state.
    unsafe { ObjectData::new(id).with_thread_moves_allowed() }
}

struct Node {
    data: ObjectData,
    seen: Vec<String>,
}

impl Node {
    fn registered() -> OwnedObject<Self> {
        // SAFETY: the returned owner is a test local on this thread, used only through
        // `with`/`with_mut`/`object_id` and dropped by the test; where a callback drops another
        // owner (`Reactor`), `OwnedObject::drop` parks the allocation until the callback returns
        // (see `destroy_owned`), and the test touches no owner during a callback.
        unsafe {
            register_owned_qobject(Self {
                data: movable_data(ObjectId::next()),
                seen: Vec::new(),
            })
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
    fn event(&mut self, event: &mut Event) -> bool {
        match &event.kind {
            EventKind::ChildAdded { child_id } => self.seen.push(format!("added {}", child_id.0)),
            EventKind::ChildRemoved { child_id } => {
                self.seen.push(format!("removed {}", child_id.0))
            }
            _ => {}
        }
        false
    }
}

fn id_of(o: &OwnedObject<Node>) -> ObjectId {
    OwnedObject::object_id(o)
}

fn parent_of(o: &OwnedObject<Node>) -> Option<ObjectId> {
    o.with(|n| n.data.parent()).unwrap()
}

fn reparent(o: &mut OwnedObject<Node>, parent: Option<ObjectId>) -> Result<(), ReparentError> {
    set_parent_by_id(OwnedObject::object_id(o), parent)
}

fn seen_len(o: &OwnedObject<Node>) -> usize {
    o.with(|n| n.seen.len()).unwrap()
}

#[test]
fn an_object_cannot_be_its_own_parent() {
    let mut a = Node::registered();
    let id = id_of(&a);

    assert_eq!(
        reparent(&mut a, Some(id)),
        Err(ReparentError::InvalidParent)
    );

    assert_eq!(parent_of(&a), None);
    assert!(a.with(|n| n.data.children().is_empty()).unwrap());
    let seen = a.with(|n| n.seen.clone()).unwrap();
    assert!(
        seen.is_empty(),
        "a refused change sends no event: {:?}",
        seen
    );
}

#[test]
fn a_descendant_cannot_become_the_parent() {
    let mut a = Node::registered();
    let mut b = Node::registered();
    let mut c = Node::registered();
    let (aid, bid, cid) = (id_of(&a), id_of(&b), id_of(&c));
    reparent(&mut b, Some(aid)).unwrap();
    reparent(&mut c, Some(bid)).unwrap();
    let events_before = (seen_len(&a), seen_len(&b), seen_len(&c));

    // a -> b -> c: neither the child nor the grandchild may become a's parent.
    assert_eq!(
        reparent(&mut a, Some(bid)),
        Err(ReparentError::InvalidParent)
    );
    assert_eq!(
        reparent(&mut a, Some(cid)),
        Err(ReparentError::InvalidParent)
    );

    assert_eq!(parent_of(&a), None);
    assert_eq!(parent_of(&b), Some(aid));
    assert_eq!(parent_of(&c), Some(bid));
    assert_eq!(a.with(|n| n.data.children().to_vec()).unwrap(), vec![bid]);
    assert_eq!(b.with(|n| n.data.children().to_vec()).unwrap(), vec![cid]);
    assert_eq!((seen_len(&a), seen_len(&b), seen_len(&c)), events_before);
}

#[test]
fn the_cycle_check_follows_the_tree_as_it_is_now() {
    let mut a = Node::registered();
    let mut b = Node::registered();
    let (aid, bid) = (id_of(&a), id_of(&b));
    reparent(&mut b, Some(aid)).unwrap();
    assert_eq!(
        reparent(&mut a, Some(bid)),
        Err(ReparentError::InvalidParent)
    );

    // Once b is detached it is not a's descendant any more, so the same call is allowed.
    reparent(&mut b, None).unwrap();
    reparent(&mut a, Some(bid)).unwrap();

    assert_eq!(parent_of(&a), Some(bid));
    assert_eq!(parent_of(&b), None);
}

#[test]
fn dropping_a_parent_leaves_a_non_owned_child_without_a_parent() {
    let parent = Node::registered();
    let parent_id = id_of(&parent);
    let mut child = Node::registered();
    reparent(&mut child, Some(parent_id)).unwrap();
    assert_eq!(parent_of(&child), Some(parent_id));

    drop(parent);

    assert_eq!(
        parent_of(&child),
        None,
        "the child's own parent still names the dead parent"
    );
}

#[test]
fn dropping_a_parent_does_not_touch_the_parent_of_an_unrelated_child() {
    let doomed = Node::registered();
    let kept = Node::registered();
    let kept_id = id_of(&kept);
    let mut child = Node::registered();
    reparent(&mut child, Some(kept_id)).unwrap();

    drop(doomed);

    assert_eq!(parent_of(&child), Some(kept_id));
}

/// Drops the parent it is given from inside its own `event`, so the parent goes away while this
/// object is borrowed by a running callback.
struct Reactor {
    data: ObjectData,
    victim: Rc<RefCell<Option<OwnedObject<Node>>>>,
    parent_seen: Vec<Option<ObjectId>>,
}

impl Reactor {
    fn registered(victim: Rc<RefCell<Option<OwnedObject<Node>>>>) -> OwnedObject<Self> {
        // SAFETY: the returned owner is a test local on this thread, handed to the caller; its
        // callback (`dispatch_to_object`) runs while the caller holds no reference to it, and
        // dropping its victim inside that callback is the case `OwnedObject::drop` parks (see
        // `destroy_owned`).
        unsafe {
            register_owned_qobject(Self {
                data: ObjectData::new(ObjectId::next()),
                victim,
                parent_seen: Vec::new(),
            })
        }
    }
}

impl QObject for Reactor {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
    }
    fn event(&mut self, event: &mut Event) -> bool {
        if matches!(event.kind, EventKind::UpdateRequest) {
            self.parent_seen.push(self.data.parent());
            drop(self.victim.borrow_mut().take());
            self.parent_seen.push(self.data.parent());
        }
        false
    }
}

/// A registered child whose callback drops its own parent, and that callback's observations.
fn child_that_drops_its_parent() -> (OwnedObject<Reactor>, ObjectId) {
    let parent = Node::registered();
    let parent_id = OwnedObject::object_id(&parent);
    let victim = Rc::new(RefCell::new(Some(parent)));
    let child = Reactor::registered(Rc::clone(&victim));
    set_parent_by_id(OwnedObject::object_id(&child), Some(parent_id)).unwrap();

    let mut event = Event::new(EventKind::UpdateRequest);
    dispatch_to_object(OwnedObject::object_id(&child), &mut event);

    assert!(victim.borrow().is_none(), "the callback dropped the parent");
    (child, parent_id)
}

#[test]
fn a_child_borrowed_while_its_parent_is_dropped_is_left_without_a_parent() {
    let (child, parent_id) = child_that_drops_its_parent();

    assert_eq!(
        child.with(|c| c.parent_seen.clone()).unwrap(),
        vec![Some(parent_id), None],
        "inside the callback: the parent before it is dropped, none right after"
    );
    assert_eq!(
        child.with(|c| c.data.parent()).unwrap(),
        None,
        "once the callback has returned"
    );
}

#[test]
fn a_child_that_was_never_registered_is_left_without_a_parent() {
    let parent = Node::registered();
    let parent_id = OwnedObject::object_id(&parent);
    let mut child = ObjectData::new(ObjectId::next());
    set_parent(&mut child, Some(parent_id)).unwrap();
    assert_eq!(child.parent(), Some(parent_id));

    drop(parent);

    assert_eq!(child.parent(), None);
}

#[test]
fn a_former_child_is_not_refused_a_thread_move_for_still_having_a_parent() {
    let (child, _) = child_that_drops_its_parent();
    let elsewhere = std::thread::spawn(ThreadId::current).join().unwrap();

    let moved = move_to_thread_by_id(
        OwnedObject::object_id(&child),
        elsewhere,
        ThreadId::current(),
    );

    assert_ne!(moved, Err(MoveByIdError::HasParent));
}

/// Adopts the child it is given from inside its own `event`, so it is borrowed while `set_parent`
/// runs and cannot record the child in its own list.
struct Adopter {
    data: ObjectData,
    child: Rc<RefCell<OwnedObject<Node>>>,
}

impl QObject for Adopter {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
    }
    fn event(&mut self, event: &mut Event) -> bool {
        if matches!(event.kind, EventKind::UpdateRequest) {
            let id = self.data.id();
            set_parent_by_id(OwnedObject::object_id(&self.child.borrow()), Some(id)).unwrap();
        }
        false
    }
}

#[test]
fn a_child_attached_while_its_parent_was_borrowed_goes_with_the_parent() {
    let adopted = Rc::new(RefCell::new(Node::registered()));
    // SAFETY: the returned owner is a test local on this thread; the `Adopter`'s callback runs
    // from `dispatch_to_object` and uses only ids and the `adopted` handle, while the test does
    // not touch the parent. The owner is dropped by the test after the dispatch returned.
    let parent = unsafe {
        register_owned_qobject(Adopter {
            data: ObjectData::new(ObjectId::next()),
            child: Rc::clone(&adopted),
        })
    };
    let parent_id = OwnedObject::object_id(&parent);
    let mut event = Event::new(EventKind::UpdateRequest);
    dispatch_to_object(parent_id, &mut event);
    assert_eq!(parent_of(&adopted.borrow()), Some(parent_id));

    // Attached from outside any callback, for comparison.
    let mut plain = Node::registered();
    reparent(&mut plain, Some(parent_id)).unwrap();

    drop(parent);

    assert_eq!(parent_of(&plain), None);
    assert!(
        !plain.with(|n| n.data.is_alive()).unwrap(),
        "a child in the parent's list is unregistered"
    );
    let adopted = adopted.borrow();
    assert_eq!(parent_of(&adopted), None);
    assert!(
        !adopted.with(|n| n.data.is_alive()).unwrap(),
        "a child attached during a callback is unregistered too"
    );
}

#[test]
fn unregistering_a_parent_ends_its_children_links_like_dropping_it() {
    let parent = Node::registered();
    let parent_id = id_of(&parent);
    let mut child = Node::registered();
    reparent(&mut child, Some(parent_id)).unwrap();
    assert_eq!(parent_of(&child), Some(parent_id));

    // SAFETY: `parent` (the owner) is alive and unmoved until the end of the test, so the memory
    // stays valid; no callback of it is active (`dispatch_to_object` returned long before and no
    // loop runs), and this is its registration thread.
    unsafe { unregister_qobject(parent_id) };

    assert_eq!(parent_of(&child), None);
}
