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
/// Dropping it drops the object, which unregisters it, and frees the memory.
///
/// The rules in the [`crate::object`] module documentation still hold for a registered object:
/// while one of its callbacks runs, its owner must not access it at all, not even to read it;
/// between callbacks the owner may use it through `Deref` and `DerefMut`.
///
/// The functions are associated functions (`OwnedObject::object_id(&owner)`), so they do not hide
/// methods of `T` reached through `Deref`.
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

    pub(crate) fn borrow_state(this: &Self) -> &Arc<ObjectBorrowState> {
        &this.borrow_state
    }
}

impl<T: QObject + ?Sized> std::ops::Deref for OwnedObject<T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: the object lives as long as its owner. No `&mut` to it is live while the owner
        // is borrowed: the registry's callbacks and the owner's use do not overlap (see the type's
        // documentation and the module documentation of `object`).
        unsafe { self.ptr.as_ref() }
    }
}

impl<T: QObject + ?Sized> std::ops::DerefMut for OwnedObject<T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: as for `deref`, and the owner is borrowed exclusively.
        unsafe { self.ptr.as_mut() }
    }
}

impl<T: QObject + ?Sized> Drop for OwnedObject<T> {
    fn drop(&mut self) {
        // SAFETY: unregister the object's registration before deallocating memory.
        unsafe { crate::object::unregister_qobject(self.id) };
        // SAFETY: `ptr` came from `Box::into_raw` and is freed only here: the owner is never
        // turned back into a `Box`, so no second pointer that claims unique ownership exists.
        drop(unsafe { Box::from_raw(self.ptr.as_ptr()) });
    }
}

// Miri does not check the uniqueness of `&mut` to a `!Unpin` type. Every object type holds an
// `ObjectData`, so this keeps a change to it from turning that check off for all of them.
const _: () = {
    const fn assert_unpin<T: Unpin + ?Sized>() {}
    assert_unpin::<super::ObjectData>();
    assert_unpin::<OwnedObject<dyn QObject>>();
};
