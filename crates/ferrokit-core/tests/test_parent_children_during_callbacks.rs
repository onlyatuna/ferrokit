// SPDX-License-Identifier: LGPL-3.0-only
//! A parent's `children()` follows every change to its children, including the ones made while a
//! callback of that parent is running.
//!
//! Qt edits the list at once, whenever and wherever `setParent` runs. Here the parent's own list
//! cannot be edited while a callback holds the parent's borrow, so the change waits and is applied
//! when the callback returns. Inside that callback the list still shows the old state.

use ferrokit_core::object::{
    register_owned_qobject, set_parent_by_id, with_object_mut, ObjectData, ObjectId, OwnedObject,
    QObject, ReparentError,
};

struct Node {
    data: ObjectData,
}

impl Node {
    fn registered() -> OwnedObject<Self> {
        // SAFETY: the returned owner is a test local on this thread, used only through `with` and
        // `object_id` and dropped by the test. The `with_object_mut` callbacks in these tests
        // are empty closures that call `set_parent_by_id` on other objects; none touches an
        // owner.
        unsafe {
            register_owned_qobject(Self {
                data: ObjectData::new(ObjectId::next()),
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
}

fn id_of(o: &OwnedObject<Node>) -> ObjectId {
    OwnedObject::object_id(o)
}

fn parent_of(o: &OwnedObject<Node>) -> Option<ObjectId> {
    o.with(|n| n.data.parent()).unwrap()
}

fn children_of(o: &OwnedObject<Node>) -> Vec<ObjectId> {
    o.with(|n| n.data.children().to_vec()).unwrap()
}

/// `set_parent` on a child that is not borrowed; the parent may be inside a callback.
fn reparent(child: &OwnedObject<Node>, parent: Option<ObjectId>) -> Result<(), ReparentError> {
    set_parent_by_id(id_of(child), parent)
}

#[test]
fn a_child_attached_while_the_parent_is_borrowed_is_listed_when_the_callback_returns() {
    let parent = Node::registered();
    let parent_id = id_of(&parent);
    let child = Node::registered();
    let child_id = id_of(&child);

    with_object_mut(parent_id, |_| {
        reparent(&child, Some(parent_id)).unwrap();
    })
    .expect("the parent is registered and not borrowed");

    assert_eq!(children_of(&parent), &[child_id]);
    assert_eq!(parent_of(&child), Some(parent_id));
}

#[test]
fn a_child_detached_while_its_parent_is_borrowed_leaves_the_list_when_the_callback_returns() {
    let parent = Node::registered();
    let parent_id = id_of(&parent);
    let child = Node::registered();
    reparent(&child, Some(parent_id)).unwrap();
    assert_eq!(children_of(&parent), &[id_of(&child)]);

    with_object_mut(parent_id, |_| {
        reparent(&child, None).unwrap();
    })
    .expect("the parent is registered and not borrowed");

    assert!(
        children_of(&parent).is_empty(),
        "the detached child is still listed: {:?}",
        children_of(&parent)
    );
    assert_eq!(parent_of(&child), None);
}

#[test]
fn a_child_moved_away_from_a_borrowed_parent_is_listed_only_under_the_new_parent() {
    let old = Node::registered();
    let new = Node::registered();
    let (old_id, new_id) = (id_of(&old), id_of(&new));
    let child = Node::registered();
    let child_id = id_of(&child);
    reparent(&child, Some(old_id)).unwrap();

    with_object_mut(old_id, |_| {
        reparent(&child, Some(new_id)).unwrap();
    })
    .expect("the old parent is registered and not borrowed");

    assert!(children_of(&old).is_empty());
    assert_eq!(children_of(&new), &[child_id]);
}

#[test]
fn a_child_dropped_while_its_parent_is_borrowed_leaves_the_list_when_the_callback_returns() {
    let parent = Node::registered();
    let parent_id = id_of(&parent);
    let kept = Node::registered();
    let kept_id = id_of(&kept);
    let dropped = Node::registered();
    reparent(&kept, Some(parent_id)).unwrap();
    reparent(&dropped, Some(parent_id)).unwrap();

    with_object_mut(parent_id, move |_| drop(dropped))
        .expect("the parent is registered and not borrowed");

    assert_eq!(
        children_of(&parent),
        &[kept_id],
        "the dropped child's id stays in the parent's list"
    );
}

#[test]
fn changes_made_during_one_borrow_are_applied_in_the_order_they_were_made() {
    let parent = Node::registered();
    let parent_id = id_of(&parent);
    let a = Node::registered();
    let b = Node::registered();
    let c = Node::registered();
    let (aid, bid, cid) = (id_of(&a), id_of(&b), id_of(&c));
    reparent(&a, Some(parent_id)).unwrap();

    with_object_mut(parent_id, |_| {
        reparent(&b, Some(parent_id)).unwrap();
        reparent(&a, None).unwrap();
        reparent(&c, Some(parent_id)).unwrap();
        reparent(&a, Some(parent_id)).unwrap();
    })
    .expect("the parent is registered and not borrowed");

    assert_eq!(children_of(&parent), &[bid, cid, aid]);
}

#[test]
fn a_change_is_applied_once_and_later_borrows_leave_the_list_alone() {
    let parent = Node::registered();
    let parent_id = id_of(&parent);
    let child = Node::registered();
    let child_id = id_of(&child);
    with_object_mut(parent_id, |_| {
        reparent(&child, Some(parent_id)).unwrap();
    })
    .unwrap();

    // The list is edited directly now; a stale copy of the earlier change must not undo it.
    reparent(&child, None).unwrap();
    with_object_mut(parent_id, |_| {}).unwrap();
    assert!(children_of(&parent).is_empty());

    reparent(&child, Some(parent_id)).unwrap();
    with_object_mut(parent_id, |_| {}).unwrap();
    assert_eq!(children_of(&parent), &[child_id]);
}

#[test]
fn a_parent_that_is_not_borrowed_is_edited_at_once() {
    let parent = Node::registered();
    let parent_id = id_of(&parent);
    let child = Node::registered();

    reparent(&child, Some(parent_id)).unwrap();

    assert_eq!(children_of(&parent), &[id_of(&child)]);
}

#[test]
fn a_child_moved_between_two_borrowed_parents_ends_up_under_the_new_one_only() {
    let old = Node::registered();
    let new = Node::registered();
    let (old_id, new_id) = (id_of(&old), id_of(&new));
    let child = Node::registered();
    let child_id = id_of(&child);
    reparent(&child, Some(old_id)).unwrap();

    // The callback of one parent runs the callback of the other, so both borrows are held and
    // both lists wait; the inner borrow ends first.
    with_object_mut(old_id, |_| {
        with_object_mut(new_id, |_| {
            reparent(&child, Some(new_id)).unwrap();
        })
        .expect("the new parent is registered and not borrowed");
    })
    .expect("the old parent is registered and not borrowed");

    assert!(children_of(&old).is_empty());
    assert_eq!(children_of(&new), &[child_id]);
    assert_eq!(parent_of(&child), Some(new_id));
}

#[test]
fn a_child_attached_and_dropped_in_the_same_borrow_is_never_listed() {
    let parent = Node::registered();
    let parent_id = id_of(&parent);
    let temporary = Node::registered();

    with_object_mut(parent_id, move |_| {
        reparent(&temporary, Some(parent_id)).unwrap();
        drop(temporary);
    })
    .expect("the parent is registered and not borrowed");

    assert!(
        children_of(&parent).is_empty(),
        "the dropped child is still listed: {:?}",
        children_of(&parent)
    );
}
