// SPDX-License-Identifier: LGPL-3.0-only
//! The owner of a registered object reaches it only through `OwnedObject::with` and `with_mut`,
//! which take the object's callback-borrow flag. A running callback and an access of the owner
//! therefore never hold references to the object at once. Run under Miri (Stacked Borrows and Tree
//! Borrows) to check that no overlap is left.
use ferrokit_core::object::{
    register_owned_qobject, with_object, with_object_mut, ObjectData, ObjectDataMut, ObjectId,
    OwnedObject, QObject,
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

fn hits_of(obj: &mut dyn QObject) -> &mut u32 {
    &mut obj
        .as_qobject_any_mut()
        .and_then(|any| any.downcast_mut::<Counter>())
        .expect("a Counter")
        .hits
}

/// `OwnedObject` has no `Deref`: this fails to compile if one is added back (the call is
/// ambiguous for a type that implements `Deref`).
#[test]
fn the_owner_has_no_deref() {
    trait AmbiguousIfDeref<A> {
        fn probe() {}
    }
    impl<T: ?Sized> AmbiguousIfDeref<()> for T {}
    impl<T: ?Sized + std::ops::Deref> AmbiguousIfDeref<u8> for T {}
    let _ = <OwnedObject<Counter> as AmbiguousIfDeref<_>>::probe;
    trait AmbiguousIfDerefMut<A> {
        fn probe() {}
    }
    impl<T: ?Sized> AmbiguousIfDerefMut<()> for T {}
    impl<T: ?Sized + std::ops::DerefMut> AmbiguousIfDerefMut<u8> for T {}
    let _ = <OwnedObject<Counter> as AmbiguousIfDerefMut<_>>::probe;
}

/// The access that used to alias: from inside a callback, the owner is refused, and the callback's
/// reference stays the only one.
#[test]
fn the_owner_is_refused_inside_a_callback_of_its_object() {
    // SAFETY: the owner is used only through `with` and `with_mut`.
    let mut owner = unsafe { register_owned_qobject(Counter::new()) };
    let id = OwnedObject::object_id(&owner);
    let ran = with_object_mut(id, |obj| {
        *hits_of(obj) += 1;
        assert!(
            OwnedObject::is_borrowed(&owner),
            "the callback holds the borrow"
        );
        assert_eq!(owner.with(|c| c.hits), None, "no read through the owner");
        assert_eq!(
            owner.with_mut(|c| c.hits += 10),
            None,
            "no write through the owner"
        );
        *hits_of(obj) += 1;
    });
    assert!(ran.is_some());
    assert_eq!(
        owner.with(|c| c.hits),
        Some(2),
        "the refused write changed nothing"
    );
}

/// Positive control: the owner between callbacks, and callbacks between uses of the owner.
#[test]
fn the_owner_between_callbacks_reads_and_writes() {
    // SAFETY: the owner is used only through `with` and `with_mut`.
    let mut owner = unsafe { register_owned_qobject(Counter::new()) };
    let id = OwnedObject::object_id(&owner);
    assert_eq!(owner.with_mut(|c| c.hits += 1), Some(()));
    assert!(with_object_mut(id, |obj| *hits_of(obj) += 10).is_some());
    assert_eq!(owner.with_mut(|c| c.hits += 100), Some(()));
    assert_eq!(owner.with(|c| c.hits), Some(111));
    assert!(!OwnedObject::is_borrowed(&owner));
}

/// A callback cannot start while the owner's closure runs, and the borrow ends with the closure.
#[test]
fn a_callback_is_refused_while_the_owner_has_the_object() {
    // SAFETY: the owner is used only through `with` and `with_mut`.
    let mut owner = unsafe { register_owned_qobject(Counter::new()) };
    let id = OwnedObject::object_id(&owner);
    let outcome = owner.with_mut(|c| {
        c.hits += 1;
        (
            with_object_mut(id, |obj| *hits_of(obj) += 10).is_none(),
            with_object(id, |_| ()).is_none(),
        )
    });
    assert_eq!(
        outcome,
        Some((true, true)),
        "a callback finds the object borrowed"
    );
    assert_eq!(owner.with(|c| c.hits), Some(1));
    assert!(with_object_mut(id, |obj| *hits_of(obj) += 10).is_some());
    assert_eq!(owner.with(|c| c.hits), Some(11));
}

/// Nested access of the same owner is refused, not aliased.
#[test]
fn nested_access_of_one_object_is_refused() {
    // SAFETY: the owner is used only through `with`.
    let owner = unsafe { register_owned_qobject(Counter::new()) };
    let outer = owner.with(|_| owner.with(|c| c.hits));
    assert_eq!(outer, Some(None));
}

/// An owner whose object was never registered takes the same flag.
#[test]
fn an_unregistered_owner_is_guarded_too() {
    let mut owner = OwnedObject::new(Counter::new());
    let outcome = owner.with_mut(|c| c.hits += 1);
    assert_eq!(outcome, Some(()));
    assert_eq!(owner.with(|c| c.hits), Some(1));
    assert!(!OwnedObject::is_borrowed(&owner));
}
