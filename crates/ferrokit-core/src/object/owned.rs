// SPDX-License-Identifier: LGPL-3.0-only
//! [`OwnedObject`]: the owner of a heap object that the registry also points to.

use std::marker::PhantomData;
use std::mem::ManuallyDrop;
use std::ptr::NonNull;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use super::qobject::{ObjectBorrowState, ObjectId, QObject};

/// Owner of a heap object whose pointer the registry shares.
///
/// It holds the raw pointer made by `Box::into_raw`. An object registered through
/// [`OwnedObject::as_non_null`], as [`register_owned_qobject`](super::register_owned_qobject) and
/// [`add_owned_child`](super::add_owned_child) do, gives the registry a copy of that same pointer:
/// the owner's accesses and the registry's callbacks then derive from one pointer, and moving the
/// `OwnedObject` moves no `Box` and leaves the registry's pointer valid. A `Box<T>` cannot do
/// that. Rust treats a `Box` as the only pointer to its object, so using or moving it invalidates
/// a pointer the registry keeps, even if the program appears to work.
///
/// Dropping it drops the object, which unregisters it, and frees the memory. When a callback of
/// the object is currently running, dropping the owner unregisters the object (and its
/// registry-listed descendants) at once and parks the allocation until that callback returns.
///
/// The rules in the [`crate::object`] module documentation still hold for a registered object:
/// while one of its callbacks runs, its owner must not access it at all, not even to read it.
/// The owner reaches the object only through [`OwnedObject::with`] and [`OwnedObject::with_mut`],
/// which take the object's borrow flag and return `None` instead of calling the closure while a
/// callback of the object runs. There is no `Deref`: a reference that outlived a call could
/// overlap the `&mut` of a callback that starts later.
///
/// The other functions are associated functions (`OwnedObject::object_id(&owner)`).
pub struct OwnedObject<T: QObject + ?Sized> {
    ptr: NonNull<T>,
    /// Read once, when the owner is made. A parent decides what to do with a child whose callback
    /// is running without reading the child: a `&` to it would alias the callback's `&mut self`.
    id: ObjectId,
    /// Shared callback-borrow state, read without touching the object or looking up the registry.
    borrow_state: Arc<ObjectBorrowState>,
    // `Box<T>` states ownership for the drop check and is `Unpin` for every `T`. A bare
    // `PhantomData<T>` would make every type that holds an `ObjectData` `!Unpin`, and Miri does
    // not check the uniqueness of `&mut` to a `!Unpin` type.
    _owns: PhantomData<Box<T>>,
}

impl<T: QObject> OwnedObject<T> {
    /// Moves `obj` to the heap. The object is not registered.
    pub fn new(obj: T) -> Self {
        Self::from_box(Box::new(obj))
    }

    /// The same owner, of the object behind `dyn QObject`, as a parent keeps it.
    pub fn into_dyn(this: Self) -> OwnedObject<dyn QObject> {
        let this = ManuallyDrop::new(this);
        // SAFETY: `this` is in `ManuallyDrop`, so `this.borrow_state` is moved out once here.
        let borrow_state = unsafe { std::ptr::read(&this.borrow_state) };
        OwnedObject {
            ptr: this.ptr,
            id: this.id,
            borrow_state,
            _owns: PhantomData,
        }
    }

    /// Gives the object back as a `Box`: for an owner whose object the registry never received a
    /// pointer of, because the registration was refused before anything was inserted.
    ///
    /// # Safety
    /// The registry must hold no pointer made from this owner (its [`OwnedObject::as_non_null`]),
    /// and no other pointer made from it may be used afterwards.
    pub(crate) unsafe fn into_box(this: Self) -> Box<T> {
        let this = ManuallyDrop::new(this);
        // SAFETY: `this` is in `ManuallyDrop`, so `borrow_state` is moved out once here.
        drop(unsafe { std::ptr::read(&this.borrow_state) });
        // SAFETY: `ptr` came from `Box::into_raw` in `from_box` and is turned back into its `Box`
        // once: the owner is consumed and its `Drop` does not run.
        unsafe { Box::from_raw(this.ptr.as_ptr()) }
    }
}

impl<T: QObject + ?Sized> OwnedObject<T> {
    /// Takes over the object of `boxed`, keeping it at its address.
    ///
    /// The object's id is read here and kept; it must not change afterwards. If the object was
    /// registered through the `Box` before, the registry's pointer is not this owner's: register
    /// it again through [`OwnedObject::as_non_null`] before a callback can run.
    pub fn from_box(boxed: Box<T>) -> Self {
        let data = boxed.object_data();
        let id = data.id();
        let borrow_state = Arc::clone(&data.borrow_state);
        // SAFETY: `Box::into_raw` never returns null.
        let ptr = unsafe { NonNull::new_unchecked(Box::into_raw(boxed)) };
        Self {
            ptr,
            id,
            borrow_state,
            _owns: PhantomData,
        }
    }

    /// The pointer that the owner and the registry share. Pass it to
    /// [`register_qobject_ptr`](super::register_qobject_ptr) to register the object.
    pub fn as_non_null(this: &Self) -> NonNull<T> {
        this.ptr
    }

    /// The object's id, read without touching the object.
    pub fn object_id(this: &Self) -> ObjectId {
        this.id
    }

    /// Whether a callback of this object is currently running and holds its exclusive borrow,
    /// checked without reading the object or looking it up in the global registry.
    pub fn is_borrowed(this: &Self) -> bool {
        this.borrow_state.borrow_flag.load(Ordering::Acquire)
    }
    #[allow(dead_code)]
    pub(crate) fn borrow_state(this: &Self) -> &Arc<ObjectBorrowState> {
        &this.borrow_state
    }
}

/// Keeps the callback-borrow flag of an owner's object set while the owner's closure runs. For a
/// registered object it is the registry's own guard, which also applies the child-list edits and
/// the parent sync that queued up meanwhile when it ends.
enum Held {
    Registered(#[allow(dead_code)] super::qobject::ObjectBorrowGuard),
    Unregistered(Arc<ObjectBorrowState>),
}

impl Drop for Held {
    fn drop(&mut self) {
        if let Held::Unregistered(state) = self {
            state.borrow_flag.store(false, Ordering::Release);
        }
    }
}

impl<T: QObject + ?Sized> OwnedObject<T> {
    fn hold(&self) -> Option<Held> {
        if let Some(guard) = super::qobject::borrow_registered(self.id) {
            return Some(Held::Registered(guard));
        }
        // Registered, but not borrowable here: a callback of it runs, or its registration thread
        // is another one (the owner stays where it is when the object is moved to a thread). The
        // object is not reachable from this thread then.
        if super::qobject::registration_thread_of(self.id).is_some() {
            return None;
        }
        // Not registered: the flag is still the one a callback of this object would take, so
        // take it here the same way.
        self.borrow_state
            .borrow_flag
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()?;
        Some(Held::Unregistered(Arc::clone(&self.borrow_state)))
    }

    /// Calls `f` with the object, unless a callback of it is running.
    ///
    /// Returns `None` without calling `f` when the object cannot be reached from this thread now:
    /// - its borrow is held (a callback of it is running, or an access through another call of
    ///   this kind is still open);
    /// - it is registered on another thread. The owner stays on the thread it was made on when
    ///   the object is moved with `move_to_thread_by_id`, and does not reach the object there; use
    ///   [`with_object`](super::with_object) on the thread the object lives on.
    ///
    /// The reference given to `f` is never alive together with another reference to the object.
    /// A callback that starts while `f` runs finds the object borrowed, as it would during another
    /// callback. `f` holds the object's borrow, so a call that needs the object's borrow itself
    /// fails with `Busy` inside `f`: that includes [`set_parent`](super::set_parent) and the
    /// `set_parent` methods of the object's data (use
    /// [`set_parent_by_id`](super::set_parent_by_id) with [`OwnedObject::object_id`] from outside
    /// the closure).
    pub fn with<R>(&self, f: impl FnOnce(&T) -> R) -> Option<R> {
        let _held = self.hold()?;
        // SAFETY: the object lives as long as its owner, and the borrow flag is held until `f`
        // returns, so no callback (which needs the flag) holds a `&mut` to it meanwhile.
        Some(f(unsafe { self.ptr.as_ref() }))
    }

    /// Calls `f` with the object mutably, unless a callback of it is running or the object is
    /// registered on another thread. See [`OwnedObject::with`], including what fails with `Busy`
    /// inside `f` (`set_parent` and the other calls that take the object's own borrow).
    pub fn with_mut<R>(&mut self, f: impl FnOnce(&mut T) -> R) -> Option<R> {
        let _held = self.hold()?;
        // SAFETY: as for `with`, and the owner is borrowed exclusively.
        Some(f(unsafe { self.ptr.as_mut() }))
    }

    /// The object, without taking its borrow.
    ///
    /// # Safety
    /// No callback of the object runs or can start while the reference lives, and no other
    /// reference to it is live: the caller holds the object's borrow flag, or knows the object has
    /// no live registry entry.
    pub(crate) unsafe fn as_ref_unchecked(&self) -> &T {
        // SAFETY: the object lives as long as its owner; the rest is the caller's contract.
        unsafe { self.ptr.as_ref() }
    }

    /// The object mutably, without taking its borrow. Same contract as
    /// [`OwnedObject::as_ref_unchecked`].
    ///
    /// # Safety
    /// See [`OwnedObject::as_ref_unchecked`].
    pub(crate) unsafe fn as_mut_unchecked(&mut self) -> &mut T {
        // SAFETY: as above, and the owner is borrowed exclusively.
        unsafe { self.ptr.as_mut() }
    }
}

impl<T: QObject + ?Sized> Drop for OwnedObject<T> {
    fn drop(&mut self) {
        self.borrow_state.memory_destroyed.store(true, std::sync::atomic::Ordering::Release);
        let ptr = self.ptr;
        if self.borrow_state.borrow_flag.load(Ordering::Acquire) {
            let borrow_state = Arc::clone(&self.borrow_state);
            super::qobject::park_owned_drop(
                self.id,
                borrow_state,
                Box::new(move || {
                    // SAFETY: `ObjectBorrowGuard::drop` runs this closure only after releasing
                    // `borrow_flag`, so the callback's `&mut` borrow of `*ptr` has ended. `ptr`
                    // came from `Box::into_raw` in `from_box` and is freed only here.
                    let mut boxed = unsafe { Box::from_raw(ptr.as_ptr()) };
                    // Clear `parent` only when the object was parked from its parent's
                    // `owned_children` (`delete_children` on a dying owning parent). A standalone
                    // `OwnedObject` attached to a live non-owning parent (`set_parent_by_id`)
                    // keeps `parent` so `T::drop` can read `object_data().parent()` and
                    // `ObjectData::drop` unlinks from the parent and sends `ChildRemoved`.
                    if boxed.object_data().owned_by_parent {
                        boxed.object_data_mut().clear_parent();
                    }
                    drop(boxed);
                }),
            );
            return;
        }
        // SAFETY: unregister the object's registration before deallocating memory.
        unsafe { crate::object::unregister_qobject(self.id) };
        // SAFETY: `ptr` came from `Box::into_raw` and is freed only here: the owner is turned back
        // into a `Box` only by `into_box`, for an object that was never registered through it, so
        // no second pointer that claims unique ownership exists.
        drop(unsafe { Box::from_raw(ptr.as_ptr()) });
    }
}

// Miri does not check the uniqueness of `&mut` to a `!Unpin` type. Every object type holds an
// `ObjectData`, so this keeps a change to it from turning that check off for all of them.
const _: () = {
    const fn assert_unpin<T: Unpin + ?Sized>() {}
    assert_unpin::<super::ObjectData>();
    assert_unpin::<OwnedObject<dyn QObject>>();
};
