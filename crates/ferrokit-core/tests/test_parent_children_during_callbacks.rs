// SPDX-License-Identifier: LGPL-3.0-only
//! A parent's `children()` follows every change to its children, including the ones made while a
//! callback of that parent is running.
//!
//! Qt edits the list at once, whenever and wherever `setParent` runs. Here the parent's own list
//! cannot be edited while a callback holds the parent's borrow, so the change waits and is applied
//! when the callback returns. Inside that callback the list still shows the old state.

use ferrokit_core::object::{
    register_owned_qobject, set_parent, with_object_mut, ObjectData, ObjectId, OwnedObject,
    QObject,
};

struct Node {
    data: ObjectData,
}

impl Node {
    fn registered() -> OwnedObject<Self> {
        // SAFETY: the owner stays on this thread and is dropped, which unregisters the object,
        // when no callback of it runs.
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
    fn object_data_mut(&mut self) -> &mut ObjectData {
        &mut self.data
    }
}

#[test]
fn a_child_attached_while_the_parent_is_borrowed_is_listed_when_the_callback_returns() {
    let parent = Node::registered();
    let parent_id = parent.data.id;
    let mut child = Node::registered();
    let child_id = child.data.id;

    with_object_mut(parent_id, |_| {
        set_parent(&mut child.data, Some(parent_id)).unwrap();
    })
    .expect("the parent is registered and not borrowed");

    assert_eq!(parent.data.children(), &[child_id]);
    assert_eq!(child.data.parent(), Some(parent_id));
}

#[test]
fn a_child_detached_while_its_parent_is_borrowed_leaves_the_list_when_the_callback_returns() {
    let parent = Node::registered();
    let parent_id = parent.data.id;
    let mut child = Node::registered();
    set_parent(&mut child.data, Some(parent_id)).unwrap();
    assert_eq!(parent.data.children(), &[child.data.id]);

    with_object_mut(parent_id, |_| {
        set_parent(&mut child.data, None).unwrap();
    })
    .expect("the parent is registered and not borrowed");

    assert!(
        parent.data.children().is_empty(),
        "the detached child is still listed: {:?}",
        parent.data.children()
    );
    assert_eq!(child.data.parent(), None);
}

#[test]
fn a_child_moved_away_from_a_borrowed_parent_is_listed_only_under_the_new_parent() {
    let old = Node::registered();
    let new = Node::registered();
    let (old_id, new_id) = (old.data.id, new.data.id);
    let mut child = Node::registered();
    let child_id = child.data.id;
    set_parent(&mut child.data, Some(old_id)).unwrap();

    with_object_mut(old_id, |_| {
        set_parent(&mut child.data, Some(new_id)).unwrap();
    })
    .expect("the old parent is registered and not borrowed");

    assert!(old.data.children().is_empty());
    assert_eq!(new.data.children(), &[child_id]);
}

#[test]
fn a_child_dropped_while_its_parent_is_borrowed_leaves_the_list_when_the_callback_returns() {
    let parent = Node::registered();
    let parent_id = parent.data.id;
    let mut kept = Node::registered();
    let kept_id = kept.data.id;
    let mut dropped = Node::registered();
    set_parent(&mut kept.data, Some(parent_id)).unwrap();
    set_parent(&mut dropped.data, Some(parent_id)).unwrap();

    with_object_mut(parent_id, move |_| drop(dropped))
        .expect("the parent is registered and not borrowed");

    assert_eq!(
        parent.data.children(),
        &[kept_id],
        "the dropped child's id stays in the parent's list"
    );
}

#[test]
fn changes_made_during_one_borrow_are_applied_in_the_order_they_were_made() {
    let parent = Node::registered();
    let parent_id = parent.data.id;
    let mut a = Node::registered();
    let mut b = Node::registered();
    let mut c = Node::registered();
    let (aid, bid, cid) = (a.data.id, b.data.id, c.data.id);
    set_parent(&mut a.data, Some(parent_id)).unwrap();

    with_object_mut(parent_id, |_| {
        set_parent(&mut b.data, Some(parent_id)).unwrap();
        set_parent(&mut a.data, None).unwrap();
        set_parent(&mut c.data, Some(parent_id)).unwrap();
        set_parent(&mut a.data, Some(parent_id)).unwrap();
    })
    .expect("the parent is registered and not borrowed");

    assert_eq!(parent.data.children(), &[bid, cid, aid]);
}

#[test]
fn a_change_is_applied_once_and_later_borrows_leave_the_list_alone() {
    let parent = Node::registered();
    let parent_id = parent.data.id;
    let mut child = Node::registered();
    let child_id = child.data.id;
    with_object_mut(parent_id, |_| {
        set_parent(&mut child.data, Some(parent_id)).unwrap();
    })
    .unwrap();

    // The list is edited directly now; a stale copy of the earlier change must not undo it.
    set_parent(&mut child.data, None).unwrap();
    with_object_mut(parent_id, |_| {}).unwrap();
    assert!(parent.data.children().is_empty());

    set_parent(&mut child.data, Some(parent_id)).unwrap();
    with_object_mut(parent_id, |_| {}).unwrap();
    assert_eq!(parent.data.children(), &[child_id]);
}

#[test]
fn a_parent_that_is_not_borrowed_is_edited_at_once() {
    let parent = Node::registered();
    let parent_id = parent.data.id;
    let mut child = Node::registered();

    set_parent(&mut child.data, Some(parent_id)).unwrap();

    assert_eq!(parent.data.children(), &[child.data.id]);
}

#[test]
fn a_child_moved_between_two_borrowed_parents_ends_up_under_the_new_one_only() {
    let old = Node::registered();
    let new = Node::registered();
    let (old_id, new_id) = (old.data.id, new.data.id);
    let mut child = Node::registered();
    let child_id = child.data.id;
    set_parent(&mut child.data, Some(old_id)).unwrap();

    // The callback of one parent runs the callback of the other, so both borrows are held and
    // both lists wait; the inner borrow ends first.
    with_object_mut(old_id, |_| {
        with_object_mut(new_id, |_| {
            set_parent(&mut child.data, Some(new_id)).unwrap();
        })
        .expect("the new parent is registered and not borrowed");
    })
    .expect("the old parent is registered and not borrowed");

    assert!(old.data.children().is_empty());
    assert_eq!(new.data.children(), &[child_id]);
    assert_eq!(child.data.parent(), Some(new_id));
}

#[test]
fn a_child_attached_and_dropped_in_the_same_borrow_is_never_listed() {
    let parent = Node::registered();
    let parent_id = parent.data.id;
    let mut temporary = Node::registered();

    with_object_mut(parent_id, move |_| {
        set_parent(&mut temporary.data, Some(parent_id)).unwrap();
        drop(temporary);
    })
    .expect("the parent is registered and not borrowed");

    assert!(
        parent.data.children().is_empty(),
        "the dropped child is still listed: {:?}",
        parent.data.children()
    );
}
