// SPDX-License-Identifier: LGPL-3.0-only
//! Parent changes never make an object its own ancestor, and a parent that goes away leaves its
//! non-owned children without a parent.
//!
//! Qt's release build lets `setParent` build a cycle and documents that as undefined behavior;
//! here the change is refused. Qt deletes every child with its parent; here a child whose `Box`
//! is not held by the parent (it was attached with `set_parent`) stays alive and loses the parent.

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::object::{
    register_qobject, set_parent, ObjectData, ObjectId, QObject, ReparentError,
};

struct Node {
    data: ObjectData,
    seen: Vec<String>,
}

impl Node {
    fn registered() -> Box<Self> {
        let mut n = Box::new(Self {
            data: ObjectData::new(ObjectId::next()),
            seen: Vec::new(),
        });
        // SAFETY: the Box is never moved out of and stays on this thread until it is dropped,
        // which unregisters it.
        unsafe { register_qobject(&mut *n) };
        n
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
    let id = a.data.id;

    assert_eq!(
        set_parent(&mut a.data, Some(id)),
        Err(ReparentError::InvalidParent)
    );

    assert_eq!(a.data.parent(), None);
    assert!(a.data.children.is_empty());
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
    let (aid, bid, cid) = (a.data.id, b.data.id, c.data.id);
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
    assert_eq!(a.data.children, vec![bid]);
    assert_eq!(b.data.children, vec![cid]);
    assert_eq!((a.seen.len(), b.seen.len(), c.seen.len()), events_before);
}

#[test]
fn the_cycle_check_follows_the_tree_as_it_is_now() {
    let mut a = Node::registered();
    let mut b = Node::registered();
    let (aid, bid) = (a.data.id, b.data.id);
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
    let parent_id = parent.data.id;
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
    let kept_id = kept.data.id;
    let mut child = Node::registered();
    set_parent(&mut child.data, Some(kept_id)).unwrap();

    drop(doomed);

    assert_eq!(child.data.parent(), Some(kept_id));
}
