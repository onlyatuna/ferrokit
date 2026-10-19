// SPDX-License-Identifier: LGPL-3.0-only
//! Does the owner of a registered object reach the object while one of its callbacks runs?
//!
//! `with_object_mut` takes the object's borrow flag and hands the callback a `&mut`. `OwnedObject`'s
//! `Deref` and `DerefMut` do not look at that flag. The first test touches the object through the
//! owner from inside such a callback; the second only uses the owner between callbacks. Run both
//! under Miri (Stacked Borrows and Tree Borrows) to see which one the borrow models reject.

use ferrokit_core::object::{
    register_owned_qobject, with_object_mut, ObjectData, ObjectDataMut, ObjectId, OwnedObject,
    QObject,
};

struct Counter {
    data: ObjectData,
    hits: u32,
}

impl Counter {
    fn new() -> Self {
        Counter {
            data: ObjectData::new(ObjectId::next()),
            hits: 0,
        }
    }
}

impl QObject for Counter {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ObjectDataMut<'_> {
        ObjectDataMut::new(&mut self.data)
    }
    fn as_qobject_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn as_qobject_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

const _: () = {
    const fn unpin<T: Unpin>() {}
    unpin::<Counter>();
};

/// Negative control of the contract: a second `&mut` to the object, made through the owner while
/// the callback holds the first.
#[test]
#[ignore = "violates the documented contract on purpose; run under Miri"]
fn the_owner_deref_mut_inside_a_callback_aliases_the_callbacks_mut() {
    // SAFETY: this is the call whose contract the test breaks below.
    let mut owner = unsafe { register_owned_qobject(Counter::new()) };
    let id = OwnedObject::object_id(&owner);
    let ran = with_object_mut(id, |obj| {
        let inside = obj
            .as_qobject_any_mut()
            .and_then(|any| any.downcast_mut::<Counter>())
            .expect("a Counter");
        inside.hits += 1;
        assert!(
            OwnedObject::is_borrowed(&owner),
            "the borrow flag is set while the callback runs"
        );
        owner.hits += 10;
        inside.hits += 1;
    });
    assert!(ran.is_some());
    assert_eq!(owner.hits, 12);
}

/// Positive control: the owner used between callbacks, and callbacks between uses of the owner.
#[test]
fn the_owner_between_callbacks_does_not_alias() {
    // SAFETY: the owner is not used while a callback runs.
    let mut owner = unsafe { register_owned_qobject(Counter::new()) };
    let id = OwnedObject::object_id(&owner);
    owner.hits += 1;
    let ran = with_object_mut(id, |obj| {
        let inside = obj
            .as_qobject_any_mut()
            .and_then(|any| any.downcast_mut::<Counter>())
            .expect("a Counter");
        inside.hits += 10;
    });
    assert!(ran.is_some());
    owner.hits += 100;
    assert_eq!(owner.hits, 111);
    assert!(!OwnedObject::is_borrowed(&owner));
}
