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
//! | pinned (`!Unpin`) | [`register_pinned_qobject`] | nothing beyond rule 1 below; there is no pointer-based form for pinned objects yet |
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
//! #     fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
//! #         ferrokit_core::object::ObjectDataMut::new(&mut self.data)
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
//! #     fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
//! #         ferrokit_core::object::ObjectDataMut::new(&mut self.data)
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
//! registration thread. An owner neither unregisters, registers again nor drops an object while
//! one of its callbacks runs. The exception is an owner type that destroys the object through the
//! crate's deferred destruction (an owned child whose parent is dropped, or a `Timer` handle
//! dropped by its own slot): that unregisters the object at once and frees its memory only after
//! the running callback returns.
//!
//! An object that answers `false` to [`QObject::can_move_to_thread`] keeps its registration
//! thread: a move of it, or of an ancestor, is refused with [`MoveByIdError::NotMovable`] before
//! an event is sent or anything changes. The object of a `Timer` does, because its `!Send` handle
//! shares state with it that only the creating thread may use.
//!
//! # Synchronization and locking contract
//!
//! Cross-object hierarchy operations (`set_parent`, `reparent_owned_object`,
//! `swap_hierarchy_and_ownership`, `add_owned_child`, and registration with parent links)
//! manipulate relationships across multiple objects and global metadata concurrently. To prevent
//! deadlocks, race conditions, topological cycles, and state tearing across threads, all hierarchy
//! mutations obey a strict synchronization and lock-acquisition protocol.
//!
//! ## Global locking hierarchy
//!
//! The crate establishes a global total ordering across synchronization primitives. Every code
//! path that acquires multiple locks simultaneously MUST acquire them in this strict order:
//!
//! 1. **TR**: `ThreadTimerContext::registry` (Mutex guarding the thread's active and pending timers).
//! 2. **EH**: `THREAD_EVENT_HANDLES` (Mutex guarding the mapping of `ThreadId` to event loop handles).
//! 3. **Q**: `EventQueue` (Mutex guarding posted events and compressor state).
//! 4. **GT**: `GLOBAL_OBJECT_THREADS` (RwLock guarding global `ObjectId -> ThreadId` affinity mappings).
//! 5. **H**: `HIERARCHY_MUTATION` (Mutex serializing topology verification and atomic commits).
//! 6. **L0**: `GLOBAL_OBJECT_REGISTRY` (RwLock guarding `ObjectId -> Arc<ObjectRecord>` registration).
//! 7. **L1**: `ObjectRecord::children` (RwLock guarding direct child IDs of a parent record).
//! 8. **L2**: `ObjectRecord::parent` (RwLock guarding direct parent `Option<ObjectId>` of a child record).
//! 9. **Leaves**: Leaf synchronization points (`thread_id`, `registration_thread`,
//!    `pending_child_changes`) protecting single atomic values or statement-level queues.
//!
//! ### Same-level ordering rules
//!
//! When an operation must acquire multiple locks at the same hierarchy level:
//! - **Level 1 (Children)**: When locking the `children` lists of two parent records simultaneously
//!   (in `set_parent`, `reparent_owned_object`, or `swap_hierarchy_and_ownership`), write locks
//!   MUST be acquired in ascending numerical `ObjectId` order (`old_id < new_id`).
//! - **Level 2 (Parent)**: When locking the `parent` links of multiple child records simultaneously
//!   (in `swap_hierarchy_and_ownership`), write locks MUST be acquired in ascending numerical
//!   child `ObjectId` order.
//!
//! ### Permitted nested acquisitions and forbidden inverted edges
//!
//! - **Permitted forward nesting**:
//!   - `TR -> EH -> Q -> L0 -> Leaves` during timer event dispatch or thread transfer.
//!   - `GT -> L0 (read) -> thread_id` during thread affinity registration.
//!   - `H -> L0 -> L1 -> L2 -> Leaves` during hierarchy mutation commits.
//! - **Strictly forbidden inverted edges**:
//!   - A thread holding `H`, `L0`, `L1`, or `L2` MUST NEVER request `TR`, `EH`, `Q`, or `GT`.
//!   - A thread holding `L2` MUST NEVER request `L1`, `L0`, or `H`.
//!   - A thread holding `L1` MUST NEVER request `L0` or `H`.
//!   - A thread holding `L0` MUST NEVER request `H`.
//!
//! ### Role of HIERARCHY_MUTATION (H)
//!
//! `HIERARCHY_MUTATION` is a global mutual-exclusion token taken before `L0`. It wraps both the
//! cycle detection walk (`would_create_cycle_in`) and the subsequent `L1`/`L2` commit phase into a
//! single atomic transaction. This prevents concurrent threads from independently validating
//! non-conflicting links and subsequently committing edges that together form a cycle.
//!
//! Operations that solely remove links (`unregister_qobject`, `ObjectData::drop`,
//! `delete_children`, `release_child`) DO NOT acquire `H`. Removing an edge or clearing a parent
//! pointer can only shrink or partition an existing hierarchy; it can never introduce a cycle or
//! closed ring. Such operations proceed using `L0`, `L1`, and `L2` directly.
//!
//! ### Scope of evidence and qualification
//!
//! The deadlock-freedom and cycle-prevention properties documented here are supported by
//! extensive empirical instrumentation ([OBSERVED] over 29.6 million lock acquisitions monitored
//! via lockdep wrappers across multi-threaded randomized stress tests without a single lock
//! inversion or deadlock). This evidence constitutes rigorous empirical validation under tested
//! workloads; it does NOT constitute a formal mathematical proof of deadlock-freedom across all
//! possible unexecuted code paths.
//!
//! ## Guard lifecycle and commit boundaries
//!
//! To prevent lock inversion deadlocks, all operations modifying hierarchy relationships enforce a
//! strict five-phase release sequence on every exit path (including normal return, error return,
//! lock poison, and panic unwinding):
//!
//! 1. **Phase 1: Release hierarchy write guards (`L2`, then `L1`)**.
//! 2. **Phase 2: Release registry lock (`L0`)**.
//! 3. **Phase 3: Release hierarchy mutation lock (`H`)**.
//! 4. **Phase 4: Drop borrow guards and ownership handles (`ObjectBorrowGuard`, `OwnedObject`)**.
//! 5. **Phase 5: Deliver notifications and execute callbacks** (`notify_helper`, event handlers).
//!
//! ### Why borrow guards and owners must be dropped after H and L0
//!
//! Dropping an [`ObjectBorrowGuard`] flushes queued child list edits (`pending_child_changes`) and
//! may trigger [`free_parked_children`] when an owned child was marked for destruction during the
//! borrow. Destroying a parked child executes [`unregister_qobject`], which acquires the `L0`
//! registry lock for writing.
//!
//! Dropping an [`OwnedObject`] similarly executes [`unregister_qobject`]. If either guard were
//! dropped while holding `H` or `L0`/`L1`/`L2`, the resulting acquisition of `L0` write would cause
//! a recursive acquisition or deadlock. Therefore, borrow guards and owned handles are declared
//! before `H` and `L0` so they are dropped strictly after all locks are released.
//!
//! ### Pre-commit data preparation
//!
//! Infallible atomic commits require that no user code or overridable trait methods run while locks
//! are held:
//! - Prior to acquiring `H` or `L0`, operations acquire all necessary borrow guards
//!   (`borrow_record`) and extract [`ObjectDataMut`] handles via [`QObject::object_data_mut`].
//! - Inside the commit critical section (holding `H`, `L0`, `L1`, and `L2`), ONLY primitive struct
//!   field assignments, slice manipulations, and atomic stores occur. No user accessors or
//!   callbacks are executed.
//!
//! ### Covered topology entry points
//!
//! This transactional locking protocol protects all topology entry points:
//! - [`set_parent()`]: acquires `H` and `L0` (read), verifies cycles, acquires parents' `L1` write
//!   guards in ascending order, acquires child `L2` write guard, commits changes, releases locks
//!   and `H`, then updates local fields and dispatches events.
//! - [`reparent_owned_object()`]: obtains borrow guards and accessors outside locks, acquires `H`
//!   and `L0` (read), validates registration and cycles, acquires parents' `L1` write guards in
//!   ascending order, acquires child `L2` write guard, transfers ownership and updates registry
//!   links, releases locks, releases `H`, drops borrow guards, then dispatches notifications.
//! - [`ObjectDataMut::swap_hierarchy_and_ownership()`]: checks direct child relationships early,
//!   obtains child borrow guards and accessors outside locks, acquires `H` and `L0` (read),
//!   verifies cycles, acquires parents' `L1` write guards in ascending order, validates registry
//!   lists against local lists, acquires children's `L2` write guards in ascending order,
//!   atomically swaps registry and local metadata, releases locks, releases `H`, drops borrow guards.
//! - [`add_owned_child()`]: validates thread affinity and existing parent links, wraps the object
//!   in an owner, and calls `register_record` with `adopted_by = Some(parent_id)`. The latter
//!   acquires `H` and `L0` (write), verifies cycles, acquires the parent's `L1` write guard,
//!   inserts the record, appends the child ID to the parent's list, and releases locks and `H`.
//!   On failure, the operation unwraps the owner via [`OwnedObject::into_box`] and returns the
//!   `Box` unmodified.
//! - **Registration with parent links**: `register_record` (called by [`register_object_metadata`],
//!   [`register_qobject_ptr`], etc.) acquires `H` and `L0` (write), sanitizes parent cycles,
//!   filters listed children that are registered under another parent, preserves existing registry
//!   links on same-object re-registration, inserts the record, and releases locks and `H`.
//!
//! ## Exceptions, event semantics, and observation boundaries
//!
//! ### Thread affinity and DifferentThread detach semantics
//!
//! When [`set_parent()`] is called with a new parent belonging to a different thread affinity
//! (`ObjectData::thread_id`), the child is detached from its old parent in both Registry and local
//! data (the old parent receives `ChildRemoved`), left parentless (`parent() == None`), a
//! warning is printed to stderr, and `Err(ReparentError::DifferentThread)` is returned.
//! In contrast, [`reparent_owned_object()`] and [`add_owned_child()`] refuse cross-thread moves
//! without changing any state or sending any events.
//!
//! ### Intermediate state observation during events
//!
//! Following Qt's `setParent_helper` semantics, events delivered by [`set_parent()`] observe
//! staged states:
//! - When `ChildRemoved` is delivered to the old parent: the old parent's local `children()` is
//!   already empty, the global registry already reflects the new parent, but the child's local
//!   `parent()` still names the old parent, and the new parent's local `children()` does not yet
//!   include the child.
//! - When `ChildAdded` is delivered to the new parent: both parents' local lists and the child's
//!   local `parent()` reflect the fully completed change.
//! - If an affected parent is borrowed by an active callback when an event occurs, the event is
//!   deferred (posted to the event loop) and list edits are queued in `pending_child_changes`.
//!   Consequently, synchronous events may be observed before posted deferred events.
//! - [`set_parent()`] dispatches events directly to the target object (`dispatch_to_object`),
//!   bypassing event filters. Conversely, [`reparent_owned_object()`] and [`add_owned_child()`]
//!   use `notify_helper`, routing events through installed event filters.
//!
//! ### Handler panic recovery
//!
//! If the old parent's `ChildRemoved` handler panics during [`set_parent()`], the unwind is caught
//! via `catch_unwind`. The function forces reconciliation by updating `child.parent` and adding the
//! child to the new parent's local list (bringing local state into alignment with the already-committed
//! registry state), suppresses `ChildAdded`, releases all locks and borrow flags, and resumes the
//! panic with the original payload.
//!
//! ### Lock poisoning strategy
//!
//! The library applies two distinct policies depending on whether an operation can fail:
//! - **Operational mutation APIs** (`set_parent`, `reparent_owned_object`,
//!   `swap_hierarchy_and_ownership`, `add_owned_child`) fail-safe: encountering any poisoned lock
//!   immediately returns `Err(ReparentError::Busy)` with zero state mutation.
//! - **Cleanup and destruction routines** (`unregister_qobject`, `release_child`,
//!   `registered_children_of`, `stop_timers_for_object`, `purge_posted_events_for_receiver`,
//!   `disconnect_all_for_object`) must never fail or panic: they tolerate poisoned locks using
//!   poison-tolerant helpers (`read_unpoisoned`, `write_unpoisoned`, `lock_unpoisoned`) to complete
//!   unregistration, clear parent links, and stop timers, preventing orphaned entries or leaks.
//!
//! ### Single-record query atomicity versus multi-record snapshot atomicity
//!
//! [`query_object_parent`] and [`query_object_children`] guarantee single-record read atomicity
//! under their respective record locks. Consecutive independent invocations across different objects
//! do NOT provide cross-object snapshot consistency: concurrent hierarchy operations may interleave
//! between individual calls. For synchronized two-object parent consistency, use
//! [`query_object_parents_snapshot`].
//!
//! ### Scope of reparent()
//!
//! [`reparent()`] modifies only the local `ObjectData` fields of the passed mutable references. It
//! does NOT touch `GLOBAL_OBJECT_REGISTRY`, does NOT acquire locks, and does NOT dispatch events.
//! It is intended exclusively for pre-registration assembly of local node trees. Any inconsistencies
//! or cycles created locally are sanitized or rejected when the objects are registered.
//!
//! ## Miri baseline and process-exit leak tracking
//!
//! Process-exit allocation tracking under Miri records 6 known persistent allocations:
//! - `lib` (10 allocations across Stacked Borrows and Tree Borrows).
//! - `test_qobject_safety_and_qt6_features` (16 allocations across SB and TB).
//! - `test_signal_emit_while_mutating` (13 allocations across SB and TB).
//!
//! These 6 entries are tracked as independent open baseline items; they are NOT added to any
//! allowlist, and they are NOT claimed as fixed. Consequently, the Nightly Miri CI job remains a
//! non-blocking `failure` (e.g. CI Run `39039194218`), while all Workspace Test suites across
//! Windows, Ubuntu, and macOS pass.
//!
//! ## Review and audit record (commits 6351991..b3f37a5)
//!
//! The branch `dv15-18-reentrancy-fixes` incorporates 9 sequential commits between `6351991` and
//! candidate HEAD `b3f37a5` (correcting an earlier document section miscounting 7 commits):
//!
//! 1. `f73f41b`: `timer: saturate deadline arithmetic` (eliminates timer overflow panics).
//! 2. `dc0b250`: `cleanup: finish unregister after a poisoned registry lock` (poison-tolerant cleanup).
//! 3. `3f4ef08`: `object: serialise parent-link changes and make their commits atomic` (introduces H,
//!    eliminates B1 lock inversion, B2 lost updates, B3 cycle TOCTOU, and aligns B5 doc contract).
//! 4. `987320e`: `object: refuse a swap with a listed child before any data handle exists` (eliminates
//!    handle aliasing under Stacked Borrows).
//! 5. `61919e4`: `object: make the documented errors true, and close three smaller gaps` (aligns
//!    error precedence, refuses pre-parented box in `add_owned_child`, guards borrow flag release).
//! 6. `3f5957b`: `object: keep the registry's lists consistent when an object is registered` (preserves
//!    hierarchy on re-registration, iterative `unregister_descendants` with visited set).
//! 7. `7390f40`: `event_loop: drop a merged event after the queue and registry locks are released`
//!    (prevents event handler deadlocks on merged event drops).
//! 8. `46d589d`: `object: do not destroy a listed object that is not the dropped object's child`
//!    (prevents `release_child` from unregistering mismatched objects).
//! 9. `b3f37a5`: `event_loop: drop a replaced compressor and handle after the locks are released`
//!    (drops replaced compressors and handles outside the queue lock).

mod owned;
pub mod qobject;
pub mod thread;
pub use crate::signal::sender;
pub use owned::OwnedObject;
pub use qobject::*;
pub use thread::*;
