// SPDX-License-Identifier: LGPL-3.0-only
//! Objects, the registry that reaches them by id, and the ways to register an object.
//!
//! # Registering an object
//!
//! The registry keeps a raw pointer to every registered object and uses it to call the object by
//! id: an event, a queued signal, a timer. Rust's aliasing rules decide whether that pointer is
//! still usable when such a callback runs. A pointer made from a reference stops being valid as
//! soon as the owner uses the object in any other way, and a callback through a pointer that is no
//! longer valid is undefined behavior. The program may appear to work; Miri reports it.
//!
//! Which function to use depends on where the object lives:
//!
//! | The object lives | Register it with | Between callbacks the owner may |
//! |---|---|---|
//! | on the heap, with one owner | [`register_owned_qobject`], which returns an [`OwnedObject`] | use it through the `OwnedObject`, and move the `OwnedObject` |
//! | in a variable or field that stays in place | [`register_qobject_ptr`], with a pointer taken by `&raw mut` | read the variable or field directly; write it only through the pointer that was registered (rule 7) |
//! | in a parent's `owned_children` | [`add_owned_child`]; [`reparent_owned_object`] takes it back | use it through the parent, or through the `OwnedObject` that `reparent_owned_object` returns |
//! | pinned (`!Unpin`, like `Timer`) | [`register_pinned_qobject`] | nothing beyond rule 1 below; there is no pointer-based form for pinned objects yet |
//!
//! `register_qobject` and `register_boxed_qobject` are deprecated: they build the registry's
//! pointer from a reference or a `Box`, which is where rule 1 below is broken.
//!
//! ## Registering an object on the heap
//!
//! ```
//! # use ferrokit_core::event::Event;
//! # use ferrokit_core::object::{ObjectData, ObjectId, QObject};
//! # struct Node {
//! #     data: ObjectData,
//! #     hits: u32,
//! # }
//! # impl QObject for Node {
//! #     fn object_data(&self) -> &ObjectData {
//! #         &self.data
//! #     }
//! #     fn object_data_mut(&mut self) -> &mut ObjectData {
//! #         &mut self.data
//! #     }
//! #     fn event(&mut self, _event: &mut Event) -> bool {
//! #         self.hits += 1;
//! #         false
//! #     }
//! # }
//! use ferrokit_core::object::{register_owned_qobject, with_object, OwnedObject};
//!
//! let node = Node { data: ObjectData::new(ObjectId::next()), hits: 0 };
//! // SAFETY: while a callback of the node runs, nothing here touches it; the owner stays on
//! // this thread.
//! let mut node = unsafe { register_owned_qobject(node) };
//! let id = OwnedObject::object_id(&node);
//!
//! node.hits += 1; // a direct use between callbacks
//! assert!(with_object(id, |o| o.object_data().id() == id).unwrap()); // the registry still reaches it
//!
//! drop(node); // dropping the owner drops the object and unregisters it
//! assert!(with_object(id, |_| ()).is_none());
//! ```
//!
//! ## Registering an object in a variable
//!
//! ```
//! # use ferrokit_core::event::Event;
//! # use ferrokit_core::object::{ObjectData, ObjectId, QObject};
//! # struct Node {
//! #     data: ObjectData,
//! #     hits: u32,
//! # }
//! # impl QObject for Node {
//! #     fn object_data(&self) -> &ObjectData {
//! #         &self.data
//! #     }
//! #     fn object_data_mut(&mut self) -> &mut ObjectData {
//! #         &mut self.data
//! #     }
//! #     fn event(&mut self, _event: &mut Event) -> bool {
//! #         self.hits += 1;
//! #         false
//! #     }
//! # }
//! use std::ptr::NonNull;
//!
//! use ferrokit_core::object::{register_qobject_ptr, unregister_qobject, with_object};
//!
//! let mut node = Node { data: ObjectData::new(ObjectId::next()), hits: 0 };
//! let id = node.data.id();
//! let node_ptr = &raw mut node;
//! // SAFETY: `node` stays here, unmoved, until it is unregistered below; the pointer is taken
//! // with `&raw mut`, so no reference is made.
//! unsafe { register_qobject_ptr(NonNull::new_unchecked(node_ptr)) };
//!
//! // SAFETY: no callback of `node` runs, and `node_ptr` is the pointer that was registered.
//! unsafe { (*node_ptr).hits += 1 }; // a write between callbacks goes through that pointer
//! assert_eq!(node.hits, 1); // a read can use the variable
//! assert!(with_object(id, |o| o.object_data().id() == id).unwrap());
//!
//! // SAFETY: no callback of `node` runs.
//! unsafe { unregister_qobject(id) };
//! ```
//!
//! ## What breaks the rules
//!
//! Each of these makes a later callback undefined behavior, whichever function registered the
//! object, unless the entry says otherwise:
//!
//! 1. **Registering through a reference, then using the object directly.** `register_qobject(&mut
//!    x)`, `register_boxed_qobject(b)` followed by a use of `b`, and a pointer made with `&mut x
//!    as *mut _`, `NonNull::from(&mut x)` or from a `Box` that is used afterwards all hand the
//!    registry a pointer that the next direct use of `x` invalidates. Take the pointer with `&raw
//!    mut x` (and see rule 7), or register through an [`OwnedObject`].
//! 2. **Holding a reference across a call that may run a callback.** A `&x`, a `&mut x`, or a
//!    closure that captures one, held while events are processed, an event is sent, or a signal
//!    connected to the object is emitted, is still live when the callback writes through the
//!    registry's pointer. Read the state through [`with_object`], or keep it outside the object
//!    (`Rc<Cell<_>>`, an atomic).
//! 3. **Touching the object while one of its callbacks runs.** Not even a read is allowed,
//!    including a read of its id. An owner that needs the id of a child it owns gets it from
//!    [`OwnedObject::object_id`].
//! 4. **Using a `Box` that stands for a registered object.** A `Box` claims to be the only
//!    pointer to its object. No function of this crate turns a registered object back into a
//!    `Box`: [`reparent_owned_object`] and the owners in `owned_children` give an
//!    [`OwnedObject`], and an object leaves the registry only through `unregister_qobject`.
//! 5. **Moving or dropping the object while it is registered.** The address must stay until
//!    `unregister_qobject` returns. (An [`OwnedObject`] may move; the object it owns does not.)
//! 6. **A method that holds `&self` or `&mut self` and delivers an event to the same object by
//!    id** (`send_event`, `with_object_mut`). The callback overlaps the borrow, and no way of
//!    registering the object prevents that. The deprecated [`move_to_thread`] does this; use
//!    [`move_to_thread_by_id`] instead.
//! 7. **Writing the variable after registering it by pointer.** Under Stacked Borrows, Miri
//!    rejects the next callback after a field write through the variable (`x.n += 1`), after an
//!    explicit `&mut` taken from it (`&mut x.data`), and after an assignment that drops the old
//!    value of a field (`x.on_event = …`): each invalidates the pointer given to
//!    `register_qobject_ptr`. Reading the variable is accepted, and so was calling a `&mut self`
//!    method on it in the cases tried, but that rests on how Miri treats that borrow, so do not
//!    count on it. Tree Borrows accepts all of these, so a run under Tree Borrows alone does not
//!    show the problem. Set the object up before registering it, or keep the pointer (`let p =
//!    &raw mut x;`) and write through `p`.
//!
//! Miri does not check that a `&mut` or a `Box` to a `!Unpin` type is unique, and `dyn QObject`
//! is not `Unpin`: a program that breaks rule 1 through a `Box<dyn QObject>` passes Miri. Use a
//! concrete type to test.
//!
//! ## Thread rules
//!
//! Callbacks run only on the thread the object was registered on, its registration thread.
//! [`move_to_thread_by_id`] moves that registration to the target thread together with the thread
//! affinity. Between callbacks the owner uses the object directly only on the registration
//! thread; any other thread reaches it through the registry, which runs the callback on the
//! registration thread. An object is neither unregistered nor registered again while one of its
//! callbacks runs.

mod owned;
pub mod qobject;
pub mod thread;
pub use crate::signal::sender;
pub use owned::OwnedObject;
pub use qobject::*;
pub use thread::*;
