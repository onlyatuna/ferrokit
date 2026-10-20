// SPDX-License-Identifier: LGPL-3.0-only
//! The owner of a registered object reaches it only through `OwnedObject::with` and `with_mut`,
//! which take the object's callback-borrow flag. A running callback and an access of the owner
//! therefore never hold references to the object at once. Run under Miri (Stacked Borrows and Tree
//! Borrows) to check that no overlap is left.
use ferrokit_core::event_loop::EventLoop;
use ferrokit_core::object::{
    move_to_thread_by_id, register_owned_qobject, with_object, with_object_mut, ObjectData,
    ObjectDataMut, ObjectId, OwnedObject, QObject, ThreadId,
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

/// An object registered on another thread is not reachable through its owner, which stays on the
/// thread it was made on; it is again once the object is back.
#[test]
fn the_owner_is_refused_while_its_object_is_registered_on_another_thread() {
    use std::sync::mpsc::channel;
    use std::time::Duration;

    let here = ThreadId::current();
    let _el = EventLoop::new();
    // SAFETY: the owner is used only through `with` and `with_mut`.
    let mut owner = unsafe { register_owned_qobject(Counter::new()) };
    let id = OwnedObject::object_id(&owner);
    assert_eq!(owner.with_mut(|c| c.hits += 1), Some(()));

    let (tid_tx, tid_rx) = channel();
    let (moved_tx, moved_rx) = channel::<()>();
    let (back_tx, back_rx) = channel::<()>();
    let other = std::thread::spawn(move || {
        let _el = EventLoop::new();
        tid_tx.send(ThreadId::current()).unwrap();
        moved_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        move_to_thread_by_id(id, here, ThreadId::current()).expect("the object goes back");
        back_tx.send(()).unwrap();
    });
    let there = tid_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(move_to_thread_by_id(id, there, here), Ok(()));

    assert_eq!(
        owner.with(|c| c.hits),
        None,
        "read refused: the object lives on another thread"
    );
    assert_eq!(owner.with_mut(|c| c.hits += 10), None, "write refused");
    moved_tx.send(()).unwrap();
    back_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    other.join().unwrap();

    assert_eq!(
        owner.with(|c| c.hits),
        Some(1),
        "the refused write changed nothing"
    );
    assert_eq!(owner.with_mut(|c| c.hits += 10), Some(()));
}

/// For an owner with no registry record the owner's own compare-and-swap is the only exclusion:
/// an access inside an access is refused, not admitted.
#[test]
fn nested_access_of_an_unregistered_owner_is_refused() {
    let mut owner = OwnedObject::new(Counter::new());
    assert_eq!(owner.with(|_| owner.with(|c| c.hits)), Some(None));
    assert_eq!(
        owner.with_mut(|c| c.hits += 1),
        Some(()),
        "the flag was released"
    );
    assert_eq!(owner.with(|c| c.hits), Some(1));
}

/// A panic in the closure releases the borrow, for a registered and for an unregistered owner.
#[test]
fn a_panic_in_the_closure_releases_the_borrow() {
    use std::panic::{catch_unwind, AssertUnwindSafe};

    // SAFETY: the owner is used only through `with` and `with_mut`.
    let mut registered = unsafe { register_owned_qobject(Counter::new()) };
    let id = OwnedObject::object_id(&registered);
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        registered.with_mut(|_| panic!("on purpose"))
    }));
    assert!(outcome.is_err());
    assert!(!OwnedObject::is_borrowed(&registered));
    assert!(
        with_object(id, |_| ()).is_some(),
        "a callback can start again"
    );
    assert_eq!(registered.with_mut(|c| c.hits += 1), Some(()));

    let mut plain = OwnedObject::new(Counter::new());
    let outcome = catch_unwind(AssertUnwindSafe(|| plain.with(|_| panic!("on purpose"))));
    assert!(outcome.is_err());
    assert!(!OwnedObject::is_borrowed(&plain));
    assert_eq!(plain.with_mut(|c| c.hits += 1), Some(()));
}
