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
    dispatch_to_object, move_to_thread_by_id, register_owned_qobject, set_parent,
    unregister_qobject, MoveByIdError, ObjectData, ObjectId, OwnedObject, QObject, ReparentError,
    ThreadId,
};

struct Node {
    data: ObjectData,
    seen: Vec<String>,
}

impl Node {
    fn registered() -> OwnedObject<Self> {
        // SAFETY: the owner stays on this thread and is dropped, which unregisters the object,
        // when no callback of it runs.
        unsafe {
            register_owned_qobject(Self {
                data: ObjectData::new(ObjectId::next()),
                seen: Vec::new(),
            })
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

#[test]
fn an_object_cannot_be_its_own_parent() {
    let mut a = Node::registered();
    let id = a.data.id();

    assert_eq!(
        set_parent(&mut a.data, Some(id)),
        Err(ReparentError::InvalidParent)
    );

    assert_eq!(a.data.parent(), None);
    assert!(a.data.children().is_empty());
    assert!(
        a.seen.is_empty(),
        "a refused change sends no event: {:?}",
        a.seen
    );
}

#[test]
fn a_descendant_cannot_become_the_parent() {
    let mut a = Node::registered();
    let mut b = Node::registered();
    let mut c = Node::registered();
    let (aid, bid, cid) = (a.data.id(), b.data.id(), c.data.id());
    set_parent(&mut b.data, Some(aid)).unwrap();
    set_parent(&mut c.data, Some(bid)).unwrap();
    let events_before = (a.seen.len(), b.seen.len(), c.seen.len());

    // a -> b -> c: neither the child nor the grandchild may become a's parent.
    assert_eq!(
        set_parent(&mut a.data, Some(bid)),
        Err(ReparentError::InvalidParent)
    );
    assert_eq!(
        set_parent(&mut a.data, Some(cid)),
        Err(ReparentError::InvalidParent)
    );

    assert_eq!(a.data.parent(), None);
    assert_eq!(b.data.parent(), Some(aid));
    assert_eq!(c.data.parent(), Some(bid));
    assert_eq!(a.data.children(), &[bid]);
    assert_eq!(b.data.children(), &[cid]);
    assert_eq!((a.seen.len(), b.seen.len(), c.seen.len()), events_before);
}

#[test]
fn the_cycle_check_follows_the_tree_as_it_is_now() {
    let mut a = Node::registered();
    let mut b = Node::registered();
    let (aid, bid) = (a.data.id(), b.data.id());
    set_parent(&mut b.data, Some(aid)).unwrap();
    assert_eq!(
        set_parent(&mut a.data, Some(bid)),
        Err(ReparentError::InvalidParent)
    );

    // Once b is detached it is not a's descendant any more, so the same call is allowed.
    set_parent(&mut b.data, None).unwrap();
    set_parent(&mut a.data, Some(bid)).unwrap();

    assert_eq!(a.data.parent(), Some(bid));
    assert_eq!(b.data.parent(), None);
}

#[test]
fn dropping_a_parent_leaves_a_non_owned_child_without_a_parent() {
    let parent = Node::registered();
    let parent_id = parent.data.id();
    let mut child = Node::registered();
    set_parent(&mut child.data, Some(parent_id)).unwrap();
    assert_eq!(child.data.parent(), Some(parent_id));

    drop(parent);

    assert_eq!(
        child.data.parent(),
        None,
        "the child's own parent still names the dead parent"
    );
}

#[test]
fn dropping_a_parent_does_not_touch_the_parent_of_an_unrelated_child() {
    let doomed = Node::registered();
    let kept = Node::registered();
    let kept_id = kept.data.id();
    let mut child = Node::registered();
    set_parent(&mut child.data, Some(kept_id)).unwrap();

    drop(doomed);

    assert_eq!(child.data.parent(), Some(kept_id));
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
        // SAFETY: as in `Node::registered`.
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
    fn object_data_mut(&mut self) -> &mut ObjectData {
        &mut self.data
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
    let parent_id = parent.data.id();
    let victim = Rc::new(RefCell::new(Some(parent)));
    let mut child = Reactor::registered(Rc::clone(&victim));
    set_parent(&mut child.data, Some(parent_id)).unwrap();

    let mut event = Event::new(EventKind::UpdateRequest);
    dispatch_to_object(child.data.id(), &mut event);

    assert!(victim.borrow().is_none(), "the callback dropped the parent");
    (child, parent_id)
}

#[test]
fn a_child_borrowed_while_its_parent_is_dropped_is_left_without_a_parent() {
    let (child, parent_id) = child_that_drops_its_parent();

    assert_eq!(
        child.parent_seen,
        vec![Some(parent_id), None],
        "inside the callback: the parent before it is dropped, none right after"
    );
    assert_eq!(child.data.parent(), None, "once the callback has returned");
}

#[test]
fn a_child_that_was_never_registered_is_left_without_a_parent() {
    let parent = Node::registered();
    let parent_id = parent.data.id();
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

    let moved = move_to_thread_by_id(child.data.id(), elsewhere, ThreadId::current());

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
    fn object_data_mut(&mut self) -> &mut ObjectData {
        &mut self.data
    }
    fn event(&mut self, event: &mut Event) -> bool {
        if matches!(event.kind, EventKind::UpdateRequest) {
            let id = self.data.id();
            set_parent(&mut self.child.borrow_mut().data, Some(id)).unwrap();
        }
        false
    }
}

#[test]
fn a_child_attached_while_its_parent_was_borrowed_goes_with_the_parent() {
    let adopted = Rc::new(RefCell::new(Node::registered()));
    // SAFETY: as in `Node::registered`.
    let parent = unsafe {
        register_owned_qobject(Adopter {
            data: ObjectData::new(ObjectId::next()),
            child: Rc::clone(&adopted),
        })
    };
    let parent_id = parent.data.id();
    let mut event = Event::new(EventKind::UpdateRequest);
    dispatch_to_object(parent_id, &mut event);
    assert_eq!(adopted.borrow().data.parent(), Some(parent_id));

    // Attached from outside any callback, for comparison.
    let mut plain = Node::registered();
    set_parent(&mut plain.data, Some(parent_id)).unwrap();

    drop(parent);

    assert_eq!(plain.data.parent(), None);
    assert!(
        !plain.data.is_alive(),
        "a child in the parent's list is unregistered"
    );
    let adopted = adopted.borrow();
    assert_eq!(adopted.data.parent(), None);
    assert!(
        !adopted.data.is_alive(),
        "a child attached during a callback is unregistered too"
    );
}

#[test]
fn unregistering_a_parent_ends_its_children_links_like_dropping_it() {
    let parent = Node::registered();
    let parent_id = parent.data.id();
    let mut child = Node::registered();
    set_parent(&mut child.data, Some(parent_id)).unwrap();
    assert_eq!(child.data.parent(), Some(parent_id));

    // SAFETY: no callback is active on this thread.
    unsafe { unregister_qobject(parent_id) };

    assert_eq!(child.data.parent(), None);
}
