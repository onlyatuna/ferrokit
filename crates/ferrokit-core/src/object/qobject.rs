// SPDX-License-Identifier: LGPL-3.0-only
use crate::event::EventFilterChain;
use crate::signal::Signal;
use crate::variant::Variant;

use super::owned::OwnedObject;
use super::thread::{ThreadContext, ThreadId};
use crate::event::{Event, EventKind};
#[cfg(test)]
use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

static GLOBAL_OBJECT_THREADS: RwLock<Option<HashMap<ObjectId, ThreadId>>> = RwLock::new(None);

/// Serialises every operation that gives an object a new parent in the registry. The check that the
/// new parent is not a descendant of the child, and the commit that records the link, both happen
/// with this held, so no other operation of that kind can slip between them: `set_parent`,
/// `reparent_owned_object`, `swap_hierarchy_and_ownership` and the registration of a record that
/// carries a parent link. Operations that only remove links (`unregister_qobject`, dropping an
/// object) do not take it, since removing a link cannot close a cycle.
///
/// It is taken before the registry (L0) and is never requested while L0, L1, L2 or the
/// `pending_child_changes` mutex is held. It is released before any event is delivered, before any
/// handler or user accessor (`QObject::object_data_mut`) runs, and before an `ObjectBorrowGuard`
/// or an `OwnedObject` is dropped. It guards no data, so a poisoned state says nothing about
/// anything it protects and is taken as it is.
static HIERARCHY_MUTATION: Mutex<()> = Mutex::new(());

fn lock_hierarchy() -> MutexGuard<'static, ()> {
    HIERARCHY_MUTATION
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// Locks for a value that one store replaces, or that only plain container edits change
/// (`insert`, `remove`, `retain`, `push`, `take`): a panic cannot leave such a value half written, so
/// a poisoned lock still guards a valid value and is used as it is. They serve where the caller
/// cannot report an error (cleanup, `Drop`) and where a panic would only turn the failure of one
/// thread into one of every later caller. Unlike the helpers of the `poison` module they leave the
/// poison flag set: the commits of a hierarchy change use the plain `read`/`write` calls on these
/// locks and return `ReparentError::Busy` for a poisoned one, and cleanup must not change that.
fn read_unpoisoned<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(PoisonError::into_inner)
}

fn write_unpoisoned<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(PoisonError::into_inner)
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Physical pointer to a registered QObject. It is dereferenced only by `registered_ptr`, after
/// the caller is verified to run on the record's registration thread.
struct RegisteredPtr(*mut dyn QObject);

// SAFETY: the pointer is dereferenced only on the registration thread, which `registered_ptr`
// checks under the record's `registration_thread` lock. Registration requires the object to stay
// pinned at this address until it is unregistered.
unsafe impl Send for RegisteredPtr {}
// SAFETY: shared access to a record never dereferences the pointer off the registration thread.
unsafe impl Sync for RegisteredPtr {}

/// Shared callback-borrow and deferred-free state of an [`ObjectData`], kept by the object,
/// its [`OwnedObject`] owner (if any), and its [`ObjectRecord`] in the global registry.
///
/// Keeping this in an `Arc` owned alongside `ObjectData` lets a parent check whether a child's
/// callback is still on the stack ([`OwnedObject::is_borrowed`]) without reading the child's
/// memory and even after an ancestor's `unregister_descendants` removed the child from
/// `GLOBAL_OBJECT_REGISTRY`.
pub(crate) struct ObjectBorrowState {
    pub(crate) borrow_flag: AtomicBool,
    /// Set when an owned child was destroyed while this object's callback was running: the guard
    /// of that callback then frees the memory it kept (see `destroy_when_released`).
    pub(crate) free_when_released: AtomicBool,
    /// The allocation parked for the guard of the running callback to free. Held here, not in a
    /// thread-local list, because the callback may run on another thread than the dropping one.
    parked: Mutex<Option<SendFree>>,
    /// Set when the underlying object memory is deallocated or its destructor has begun.
    pub(crate) memory_destroyed: AtomicBool,
    /// `QObject::destroyed`. It lives here, with the state the registry record and the owner
    /// share, so that the destruction can emit it without reading the object: a callback of the
    /// object may still be running when its owner is dropped (see `destroy_here`).
    destroyed: Signal<ObjectId>,
    /// The object's liveness token (shared with its record): false makes the object unreachable
    /// through the registry (`registered_ptr`), which `emit_destroyed_once` does first.
    liveness: Arc<AtomicBool>,
    /// Set by the one emission of `destroyed`, whichever teardown step comes first.
    destroyed_emitted: AtomicBool,
}

impl ObjectBorrowState {
    fn new(destroyed: Signal<ObjectId>, liveness: Arc<AtomicBool>) -> Self {
        Self {
            borrow_flag: AtomicBool::new(false),
            free_when_released: AtomicBool::new(false),
            parked: Mutex::new(None),
            memory_destroyed: AtomicBool::new(false),
            destroyed,
            liveness,
            destroyed_emitted: AtomicBool::new(false),
        }
    }

    /// Marks the object dead, then emits `destroyed(id)` once, the first time it is called for
    /// this object. Qt `~QObject` emits it first, before it disconnects anything and before
    /// `deleteChildren` (qobject.cpp:1086-1105). The object is unreachable through the registry
    /// from the first step (liveness false), so a slot cannot get at it, and the receivers get the
    /// id and nothing else. Callers run it before any descendant is unregistered, so a parent's
    /// `destroyed` always goes out before its descendants'.
    fn emit_destroyed_once(&self, id: ObjectId) {
        self.liveness.store(false, Ordering::Release);
        if !self.destroyed_emitted.swap(true, Ordering::AcqRel) {
            self.destroyed.emit_while_owner_dying(&id);
        }
    }
}

/// Parent sync data: `(old parent, new (parent, flag))`.
type PendingParentSync = (Option<ObjectId>, Option<(ObjectId, Arc<AtomicBool>)>);
/// Registry record for a QObject. The stored pointer is dereferenced only on the registration
/// thread, which `registered_ptr` enforces together with the borrow flag.
pub struct ObjectRecord {
    pub id: ObjectId,
    pub thread_id: RwLock<ThreadId>,
    registration_thread: RwLock<ThreadId>,
    ptr: RegisteredPtr,
    pub parent: RwLock<Option<ObjectId>>,
    pub children: RwLock<Vec<ObjectId>>,
    pub liveness: Arc<AtomicBool>,
    pub generation: AtomicU32,
    borrow_state: Arc<ObjectBorrowState>,
    reparenting: AtomicBool,
    /// Edits to the `children` list in the object's own `ObjectData` that arrived while a callback
    /// held its borrow; the guard of that callback applies them when it ends.
    pending_child_changes: Mutex<Vec<ChildListChange>>,
    has_pending_child_changes: AtomicBool,
    /// Pending post-commit parent sync that arrived while a callback held the child's borrow;
    /// the guard of that callback applies it when it ends.
    pending_parent_sync: Mutex<Option<PendingParentSync>>,
    has_pending_parent_sync: AtomicBool,
}
/// One edit to the `children` list in an object's own `ObjectData`.
enum ChildListChange {
    Add(ObjectId),
    Remove(ObjectId),
    /// Takes the child out of `owned_children` as well, and hands its owner back to the caller to
    /// drop once the parent's borrow is released (a `DeferredDelete` of the child, delivered while a
    /// callback of the parent runs).
    DeleteOwned(ObjectId),
}

impl ChildListChange {
    fn apply(&self, mut data: ObjectDataMut<'_>) -> Option<OwnedObject<dyn QObject>> {
        match *self {
            ChildListChange::Add(id) => data.add_child(id),
            ChildListChange::Remove(id) => data.remove_child(id),
            ChildListChange::DeleteOwned(id) => {
                let owner = data.take_owned_child(id);
                if owner.is_none() {
                    data.remove_child(id);
                }
                return owner;
            }
        }
        None
    }
}

pub struct ObjectBorrowGuard {
    ptr: *mut dyn QObject,
    borrow_state: Arc<ObjectBorrowState>,
    _entry: Arc<ObjectRecord>,
}

impl std::ops::Deref for ObjectBorrowGuard {
    type Target = dyn QObject;
    fn deref(&self) -> &Self::Target {
        // SAFETY: registration requires pointer stability and exclusive registry access;
        // registered_ptr checked physical thread and acquired this entry's borrow flag.
        unsafe { &*self.ptr }
    }
}

impl std::ops::DerefMut for ObjectBorrowGuard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY: the guard owns the entry's exclusive borrow flag.
        unsafe { &mut *self.ptr }
    }
}

/// Releases a borrow flag when it goes out of scope, a panic included.
struct ReleaseBorrowFlag<'a>(&'a ObjectBorrowState);

impl Drop for ReleaseBorrowFlag<'_> {
    fn drop(&mut self) {
        self.0.borrow_flag.store(false, Ordering::Release);
    }
}

impl Drop for ObjectBorrowGuard {
    fn drop(&mut self) {
        // Owners taken out of `owned_children` by queued `DeleteOwned` edits; dropped after the
        // borrow flag is released, since destroying them reads this object (their parent).
        let mut released: Vec<OwnedObject<dyn QObject>> = Vec::new();
        {
            // The borrow flag is released when this block ends (via RAII `ReleaseBorrowFlag`), even if
            // a custom `QObject::object_data_mut()` accessor panics while queued child edits or pending
            // parent syncs are applied: the object would stay borrowed forever otherwise.
            //
            // CONTRACT / EXCLUSION:
            // If a custom `QObject::object_data_mut()` accessor panics during `ObjectBorrowGuard::drop`,
            // `ReleaseBorrowFlag` guarantees that the borrow flag is safely released to prevent borrow leaks
            // and deadlocks. However, any `pending_child_changes` or `pending_parent_sync` extracted after
            // swapping their pending flags are consumed and will not be restored or reapplied on unwind.
            // This custom accessor panic scenario is strictly excluded from the transactional recovery
            // guarantees provided for `ChildRemoved` user callback panics.
            let _release = ReleaseBorrowFlag(&self.borrow_state);
            if self
                ._entry
                .has_pending_child_changes
                .swap(false, Ordering::AcqRel)
            {
                let changes =
                    std::mem::take(&mut *lock_unpoisoned(&self._entry.pending_child_changes));
                // A destroyed object is not written to: its memory may already be freed. Every
                // drop of an object marks its memory destroyed before the memory goes. An object
                // that was only unregistered (its liveness token is false) still owns its
                // children, so queued `DeleteOwned` edits are applied to it.
                if !self.borrow_state.memory_destroyed.load(Ordering::Acquire) {
                    // SAFETY: the callback that held this borrow has returned, so no reference into
                    // the object is live, and the object's memory is not destroyed.
                    let mut data = unsafe { (*self.ptr).object_data_mut() };
                    for change in &changes {
                        released.extend(change.apply(ObjectDataMut::new(data.raw_data_mut())));
                    }
                }
            }
            if self
                ._entry
                .has_pending_parent_sync
                .swap(false, Ordering::AcqRel)
            {
                let sync = lock_unpoisoned(&self._entry.pending_parent_sync).take();
                if let Some((new_parent, target_liveness)) = sync
                    && !self.borrow_state.memory_destroyed.load(Ordering::Acquire)
                {
                    // SAFETY: as above: this guard still holds the borrow flag (`_release`),
                    // so no callback of the object is running, and the object's memory is not
                    // freed before the guard has released the flag and returned from `drop`
                    // (`destroy_here` frees only after it wins the flag, or parks the
                    // allocation for `run_parked` below). `self.ptr` is the registered pointer.
                    let mut data = unsafe { (*self.ptr).object_data_mut() };
                    let raw = data.raw_data_mut();
                    raw.parent = new_parent;
                    raw.parent_liveness = target_liveness;
                }
            }
        }
        drop(released);
        if self
            .borrow_state
            .free_when_released
            .swap(false, Ordering::AcqRel)
        {
            run_parked(&self.borrow_state);
        }
    }
}

/// A destruction closure that may run on another thread than the one that built it. The closure
/// frees an object that only the registration thread (or, once that thread is gone, nobody) can be
/// running a callback of, and the borrow flag orders it after those callbacks.
pub(crate) struct SendFree(Box<dyn FnOnce(bool)>);
// SAFETY: the closure captures only what `OwnedObject::drop` gives it: a raw pointer to the heap
// allocation that it alone frees, and ids. `destroy_here` and `run_parked` decide which thread runs
// it, and the borrow flag's acquire/release exchange orders it after every callback of the object.
// An object type that is not `Send` reaches another thread only after its creator opted in with
// `ObjectData::with_thread_moves_allowed`, which promises that dropping it there is sound.
unsafe impl Send for SendFree {}

impl SendFree {
    /// `free(parked)` drops the object's memory; `parked` is true when a callback of the object
    /// had to return first.
    pub(crate) fn new(free: impl FnOnce(bool) + 'static) -> Self {
        SendFree(Box::new(free))
    }
}

/// Destroys the object of an [`OwnedObject`] that is being dropped.
///
/// * On the object's registration thread (or when the object is not registered), the owner takes
///   the borrow flag with a compare-and-swap. If it gets the flag, no callback runs and none can
///   start (`registered_ptr` callers fail the same compare-and-swap), so the object is
///   unregistered and freed at once. If a callback holds the flag, the object and everything the
///   registry lists below it are unregistered at once and the allocation is parked in the
///   object's [`ObjectBorrowState`]; the guard of that callback frees it when it ends.
/// * On another thread, the object is not touched. The destruction is posted to the registration
///   thread as a queued call (as `deleteLater` posts a `DeferredDelete`), where it takes the
///   branch above: the timers are stopped in that thread's registry and no callback overlaps. The
///   object stays registered and usable by its callbacks until that call runs. If the call is
///   discarded unrun, the object is destroyed where it was dropped (see [`ForwardedDestroy`]):
///   * the registration thread has already ended, so the post itself is dropped (see
///     [`post_event_to_thread`](crate::event_loop::post_event_to_thread)), and the object is
///     destroyed on the owner's thread, since no other thread can;
///   * the thread's event queue went away (its loop ended) and the call was dropped with it;
///   * the thread ends while the call is still held for a loop it never started: the object is
///     then destroyed on that thread, as its locals are destroyed.
///
///   A thread that is alive but never runs its event loop (or whose loop ended while the thread
///   goes on without one) keeps the object until it ends or starts a loop.
pub(crate) fn destroy_owned(id: ObjectId, state: Arc<ObjectBorrowState>, free: SendFree) {
    match registration_thread_of(id) {
        Some(thread) if thread != ThreadId::current() => {
            let mut job = ForwardedDestroy {
                id,
                state: Some(state),
                free: Some(free),
            };
            crate::event_loop::post_event_to_thread(
                thread,
                ObjectId(0),
                Event::new(EventKind::MetaCall(Box::new(move |_| job.deliver()))),
            );
        }
        _ => destroy_here(id, state, free),
    }
}

/// A destruction forwarded to the registration thread. Dropped without having been delivered, the
/// event queue that held it is gone (or the registration thread has ended), and the object is
/// destroyed on the dropping thread.
///
/// Timers: they live in the thread-local timer registry of the registration thread (on Windows
/// with a native source that only that thread can kill), so another thread cannot stop them and
/// `unregister_qobject` stops none from here. Nothing is lost when the registration thread has
/// ended or its loop, and with it its timer context, is gone: the registry went away. Only a
/// queue dropped by a different thread while the registration thread's timer context is alive
/// leaves the object's timer entries there, until that thread's registry is dropped.
struct ForwardedDestroy {
    id: ObjectId,
    state: Option<Arc<ObjectBorrowState>>,
    free: Option<SendFree>,
}

// SAFETY: the payload is a `SendFree` (see its `Send` impl) and an `Arc<ObjectBorrowState>`, whose
// fields are atomics and a mutex; the ids are plain integers. The job is delivered once or dropped
// once, and either path ends in `destroy_here`.
unsafe impl Send for ForwardedDestroy {}

impl ForwardedDestroy {
    fn deliver(&mut self) {
        if let (Some(state), Some(free)) = (self.state.take(), self.free.take()) {
            // The object may have moved again, or been unregistered, since the post.
            destroy_owned(self.id, state, free);
        }
    }
}

impl Drop for ForwardedDestroy {
    fn drop(&mut self) {
        if let (Some(state), Some(free)) = (self.state.take(), self.free.take()) {
            destroy_here(self.id, state, free);
        }
    }
}

fn destroy_here(id: ObjectId, state: Arc<ObjectBorrowState>, free: SendFree) {
    let SendFree(free) = free;
    if state
        .borrow_flag
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        state.memory_destroyed.store(true, Ordering::Release);
        // SAFETY: the flag is ours, so no callback runs or can start; the object's memory is
        // freed after the unregistration.
        unsafe { unregister_qobject(id) };
        free(false);
        return;
    }
    // A callback holds the flag: park the allocation in the shared slot for its guard.
    state.memory_destroyed.store(true, Ordering::Release);
    // The object is dead from here on, and `destroyed` goes out before any descendant is
    // unregistered (and so before theirs).
    state.emit_destroyed_once(id);
    *lock_unpoisoned(&state.parked) = Some(SendFree(free));
    state.free_when_released.store(true, Ordering::Release);
    unregister_descendants(id);
    // SAFETY: the running callback holds the borrow flag and its own `Arc` to the record, and the
    // memory is not freed until it returns. No other callback can start on an unregistered object.
    unsafe { unregister_qobject(id) };
    // The callback may have returned before the allocation was parked and so never seen it.
    if state
        .borrow_flag
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        state.free_when_released.store(false, Ordering::Release);
        run_parked(&state);
    }
}

/// Destroys an owner whose object may have a callback still running: an owned child whose parent
/// is being dropped from inside that callback, or an owner (such as a `Timer` handle) dropped by
/// one of its own slots. [`OwnedObject::drop`] unregisters the object (and its registry-listed
/// descendants) at once and parks the allocation when `borrow_flag` is held, freeing it when the
/// running callback's guard returns (`run_parked`).
pub(crate) fn destroy_when_released(child: OwnedObject<dyn QObject>) {
    drop(child);
}

/// Unregisters everything the registry lists below `id`, the deepest first, and touches no memory:
/// a callback may be running in any of them, and whoever owns an object frees it.
///
/// The walk keeps the objects it has met, so lists that name each other in a ring end it, and it
/// does not recurse, so a deep hierarchy does not use the stack.
fn unregister_descendants(id: ObjectId) {
    let mut below = Vec::new();
    let mut met = std::collections::HashSet::from([id]);
    let mut pending = vec![id];
    while let Some(parent_id) = pending.pop() {
        for child_id in registered_children_of(parent_id) {
            if met.insert(child_id) {
                below.push(child_id);
                pending.push(child_id);
            }
        }
    }
    // A child comes after its parent in `below`, so the reverse order takes the deepest first.
    for child_id in below.into_iter().rev() {
        // SAFETY: a running callback keeps its own `Arc` to the record, and the memory is not
        // freed before that callback returns (see `destroy_when_released`).
        unsafe { unregister_qobject(child_id) };
    }
}

/// Runs the destruction parked in `state`, if it is still there. The parking thread (when it finds
/// the borrow flag free after parking) and the callback's guard both call this: the slot is
/// emptied under its lock, so one of them runs it.
fn run_parked(state: &ObjectBorrowState) {
    let free = lock_unpoisoned(&state.parked).take();
    if let Some(SendFree(free)) = free {
        free(true);
    }
}

static GLOBAL_OBJECT_REGISTRY: std::sync::LazyLock<RwLock<HashMap<ObjectId, Arc<ObjectRecord>>>> =
    std::sync::LazyLock::new(|| RwLock::new(HashMap::new()));

/// Reverse of the `parent` links of the records in [`GLOBAL_OBJECT_REGISTRY`]: for every registered
/// object whose record links to a parent, which parent. It answers "which registered objects name
/// `id` as their parent" without a walk over the registry, so what a thread move or a drop inspects
/// depends on the subtree and not on the number of registered objects. The parent need not be
/// registered: a child can name an id that has no record.
///
/// The index is a leaf lock (see "Global locking hierarchy" in the [`crate::object`] module
/// documentation): it is taken last and nothing is requested while it is held. It changes together
/// with what it mirrors. A `parent` link (L2) is written through [`store_link`], which updates the
/// index while the link's write guard and the registry (L0) are held; `register_record` and
/// `unregister_qobject` add and remove a record with the registry held for writing, and update the
/// index before they let it go. A reader that holds L0 therefore finds no entry of an object that
/// is not registered, and each entry is the link of the current record.
#[derive(Default)]
struct ParentLinkIndex {
    /// Parent id to the registered objects that link to it, in the order they were linked.
    children: HashMap<ObjectId, Vec<ObjectId>>,
    /// Registered object to the parent its record links to; absent when it has none.
    parent_of: HashMap<ObjectId, ObjectId>,
}

impl ParentLinkIndex {
    /// Records that `child`'s link is now `parent`, whatever it was.
    fn relink(&mut self, child: ObjectId, parent: Option<ObjectId>) {
        if self.parent_of.get(&child).copied() == parent {
            return;
        }
        if let Some(old) = self.parent_of.remove(&child)
            && let Some(list) = self.children.get_mut(&old)
        {
            if let Some(at) = list.iter().position(|&listed| listed == child) {
                list.remove(at);
            }
            if list.is_empty() {
                self.children.remove(&old);
            }
        }
        if let Some(parent) = parent {
            self.parent_of.insert(child, parent);
            self.children.entry(parent).or_default().push(child);
        }
    }
}

static PARENT_LINK_INDEX: std::sync::LazyLock<Mutex<ParentLinkIndex>> =
    std::sync::LazyLock::new(|| Mutex::new(ParentLinkIndex::default()));

/// Writes `new` into the `parent` link of `record`, held through `link` (its L2 write guard), and
/// into [`PARENT_LINK_INDEX`]. The caller holds the registry (L0) and passes it: a record that the
/// registry no longer holds, or holds as another record under its id, is not the one the index
/// mirrors, and its link is written without touching the index.
fn store_link(
    registry: &HashMap<ObjectId, Arc<ObjectRecord>>,
    record: &Arc<ObjectRecord>,
    link: &mut Option<ObjectId>,
    new: Option<ObjectId>,
) {
    if registry
        .get(&record.id)
        .is_some_and(|current| Arc::ptr_eq(current, record))
    {
        lock_unpoisoned(&PARENT_LINK_INDEX).relink(record.id, new);
    }
    *link = new;
}

/// Checks that [`PARENT_LINK_INDEX`] is exactly the relation a walk over the registry derives: the
/// `parent` link of every record. For tests: it walks the whole registry, and it is exact only
/// while no other thread changes a link.
#[doc(hidden)]
pub fn verify_parent_link_index() -> Result<(), String> {
    let registry = read_unpoisoned(&GLOBAL_OBJECT_REGISTRY);
    // The links are read before the index is locked: a link (L2) is never requested while the
    // index, a leaf, is held.
    let mut derived: HashMap<ObjectId, ObjectId> = HashMap::new();
    for (&id, record) in registry.iter() {
        if let Some(parent) = *read_unpoisoned(&record.parent) {
            derived.insert(id, parent);
        }
    }
    let index = lock_unpoisoned(&PARENT_LINK_INDEX);
    if derived != index.parent_of {
        return Err(format!(
            "links of the registry {derived:?} differ from the index's {:?}",
            index.parent_of
        ));
    }
    let mut listed = 0;
    for (parent, list) in &index.children {
        if list.is_empty() {
            return Err(format!("{parent:?} has an empty list in the index"));
        }
        for (at, child) in list.iter().enumerate() {
            if index.parent_of.get(child) != Some(parent) || list[..at].contains(child) {
                return Err(format!("{child:?} is listed wrongly under {parent:?}"));
            }
        }
        listed += list.len();
    }
    if listed != index.parent_of.len() {
        return Err(format!(
            "the index lists {listed} children for {} links",
            index.parent_of.len()
        ));
    }
    Ok(())
}

fn registered_ptr(id: ObjectId) -> Option<(Arc<ObjectRecord>, *mut dyn QObject)> {
    let entry = GLOBAL_OBJECT_REGISTRY.read().ok()?.get(&id)?.clone();
    if *read_unpoisoned(&entry.registration_thread) != ThreadId::current()
        || !entry.liveness.load(Ordering::Acquire)
    {
        return None;
    }
    let ptr = entry.ptr.0;
    Some((entry, ptr))
}

/// Whether `id` is registered and alive on the calling thread.
pub(crate) fn registered_on_current_thread(id: ObjectId) -> bool {
    registered_ptr(id).is_some()
}

/// Whether `id` is registered and alive on the calling thread, with its borrow held by a running
/// callback. `ObjectId(0)` names no object, so it is never borrowed.
pub(crate) fn registered_and_borrowed(id: ObjectId) -> bool {
    id != ObjectId(0)
        && registered_ptr(id)
            .is_some_and(|(entry, _)| entry.borrow_state.borrow_flag.load(Ordering::Acquire))
}

/// Defers a child event to a registered parent whose borrow is held. The event is posted,
/// so the parent's handler sees it after the borrow ends, instead of it being dropped. Returns the
/// event back when the parent is not borrowed, so the caller delivers it now.
fn defer_if_borrowed(parent: ObjectId, event: Event) -> Result<(), Event> {
    if registered_and_borrowed(parent) {
        crate::event_loop::post_event_to_thread(ThreadId::current(), parent, event);
        Ok(())
    } else {
        Err(event)
    }
}

fn borrow_record(entry: &Arc<ObjectRecord>) -> Result<ObjectBorrowGuard, ReparentError> {
    let reg_thread = entry
        .registration_thread
        .read()
        .map_err(|_| ReparentError::Busy)?;
    if *reg_thread != ThreadId::current() {
        return Err(ReparentError::DifferentThread);
    }
    if !entry.liveness.load(Ordering::Acquire) {
        return Err(ReparentError::UnknownObject);
    }
    let ptr = entry.ptr.0;
    if ptr.is_null() {
        return Err(ReparentError::UnknownObject);
    }
    entry
        .borrow_state
        .borrow_flag
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .map_err(|_| ReparentError::Busy)?;
    Ok(ObjectBorrowGuard {
        ptr,
        borrow_state: Arc::clone(&entry.borrow_state),
        _entry: Arc::clone(entry),
    })
}

pub(super) fn borrow_registered(id: ObjectId) -> Option<ObjectBorrowGuard> {
    let (entry, ptr) = registered_ptr(id)?;
    entry
        .borrow_state
        .borrow_flag
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .ok()?;
    Some(ObjectBorrowGuard {
        ptr,
        borrow_state: Arc::clone(&entry.borrow_state),
        _entry: entry,
    })
}

/// Edits the `children` list kept in `parent`'s own `ObjectData`. When a running callback holds
/// the parent's borrow, the list cannot be touched now: the edit is queued and applied when that
/// callback returns. A parent that is not registered on this thread has no list to edit here.
fn edit_child_list(parent: ObjectId, change: ChildListChange) {
    let Some((entry, _)) = registered_ptr(parent) else {
        return;
    };
    match borrow_registered(parent) {
        Some(mut guard) => {
            let released = change.apply(guard.object_data_mut());
            // The owner's destruction reads the parent, so the parent is released first.
            drop(guard);
            drop(released);
        }
        None => {
            lock_unpoisoned(&entry.pending_child_changes).push(change);
            entry
                .has_pending_child_changes
                .store(true, Ordering::Release);
        }
    }
}

/// `QObject::property` as the trait default does it, for an override that handles some
/// names itself and defers the rest.
pub fn default_property<T: QObject + ?Sized>(obj: &T, name: &str) -> Option<Variant> {
    let mo = obj.meta_object();
    if let Some(idx) = mo.index_of_property(name)
        && let Some(prop) = mo.property(idx)
        && let Some(any) = obj.as_qobject_any()
        && let Some(val) = prop.read(any)
    {
        return Some(val);
    }
    obj.object_data().property(name).cloned()
}

/// `QObject::setProperty` as the trait default does it (see [`QObject::set_property`]).
pub fn default_set_property<T: QObject + ?Sized>(obj: &mut T, name: &str, value: Variant) -> bool {
    let mo = obj.meta_object();
    if let Some(idx) = mo.index_of_property(name)
        && let Some(prop) = mo.property(idx).cloned()
    {
        if !prop.is_writable() {
            return false;
        }
        if let Some(any_mut) = obj.as_qobject_any_mut() {
            return prop.write(any_mut, value).is_ok();
        }
        return false;
    }

    let changed = obj.object_data_mut().set_property(name, value);
    if changed {
        let mut ev = Event::new(EventKind::DynamicPropertyChange {
            property_name: name.to_string(),
        });
        obj.event(&mut ev);
    }
    false
}

/// Calls `f` with the registered QObject on its physical registration thread.
///
/// The callback receives exclusive mutable access; registration's unsafe contract
/// requires that no direct aliases are accessed for the duration of this call.
pub fn with_object_mut<R, F: FnOnce(&mut dyn QObject) -> R>(id: ObjectId, f: F) -> Option<R> {
    let mut guard = borrow_registered(id)?;
    Some(f(&mut *guard))
}

/// Calls `f` with the registered QObject on its physical registration thread.
///
/// Registration's unsafe contract requires that no mutable aliases are accessed
/// for the duration of this call.
pub fn with_object<R, F: FnOnce(&dyn QObject) -> R>(id: ObjectId, f: F) -> Option<R> {
    let guard = borrow_registered(id)?;
    Some(f(&*guard))
}

pub fn dispatch_to_object(receiver: ObjectId, event: &mut Event) -> bool {
    with_object_mut(receiver, |obj| obj.event(event)).unwrap_or(false)
}

/// Registers an object's thread affinity in the global registry.
///
/// Lock order: this map, then the registry (L0, read), then the record's `thread_id`. They are held
/// together so that two calls for one id leave both stores with the same last writer. No caller
/// holds L0, L1, L2 or `HIERARCHY_MUTATION`.
pub(crate) fn register_object_thread(id: ObjectId, thread_id: ThreadId) {
    let mut lock = write_unpoisoned(&GLOBAL_OBJECT_THREADS);
    if lock.is_none() {
        *lock = Some(HashMap::new());
    }
    if let Some(map) = lock.as_mut() {
        map.insert(id, thread_id);
    }
    if let Ok(reg) = GLOBAL_OBJECT_REGISTRY.read()
        && let Some(entry) = reg.get(&id)
    {
        *write_unpoisoned(&entry.thread_id) = thread_id;
    }
}

/// Unregisters an object's thread affinity from the global registry. It is part of the cleanup of
/// a destroyed object, which cannot report a failure, so a poisoned map is used as it is.
pub(crate) fn unregister_object_thread(id: ObjectId) {
    if let Some(map) = write_unpoisoned(&GLOBAL_OBJECT_THREADS).as_mut() {
        map.remove(&id);
    }
}

/// Physical registration thread of a live registered object, or `None` if it is unregistered.
pub(crate) fn registration_thread_of(id: ObjectId) -> Option<ThreadId> {
    let entry = GLOBAL_OBJECT_REGISTRY.read().ok()?.get(&id)?.clone();
    if !entry.liveness.load(Ordering::Acquire) {
        return None;
    }
    let thread_id = *read_unpoisoned(&entry.registration_thread);
    Some(thread_id)
}

/// Invokes a method on a registered object through its thread's event loop
/// (`QMetaObject::invokeMethod` with `Qt::QueuedConnection` or `Qt::BlockingQueuedConnection`).
///
/// Needs no `&mut` borrow of the object, so any thread may call it with the receiver's id and its
/// static `MetaObject`. The member is resolved and its arguments type-checked before anything is
/// posted, as Qt does (`invokeMethodImpl`, qmetaobject.cpp:1693-1708; `invokeImpl` type checks,
/// qmetaobject.cpp:2774-2809). `Queued` returns `Ok(Variant::Invalid)`
/// once posted; the return value is not delivered. `BlockingQueued` waits for the result on the
/// receiver's thread. It returns `Err(TargetBorrowFailed)` if the posted call is dropped without
/// running (the receiver was destroyed, or its loop or thread ended), and `Err(ExecutionFailed)`
/// without posting if the receiver lives on the calling thread (Qt: dead lock detected). It does
/// not return while the receiver's thread is alive but does not run its event loop, as in Qt; see
/// [`ConnectionType::BlockingQueued`](crate::signal::ConnectionType::BlockingQueued). `Auto` queues
/// when the caller is on another thread. `Direct`, and `Auto` on the receiver's own thread, need a
/// `&mut` borrow and return `Err(TargetBorrowFailed)`; use `QObject::invoke_method_with_type` for
/// those.
pub fn invoke_method_on_thread(
    receiver: ObjectId,
    meta: &'static crate::meta::MetaObject,
    member: &str,
    args: &[Variant],
    conn_type: crate::signal::ConnectionType,
) -> Result<Variant, crate::meta::InvokeError> {
    use crate::signal::ConnectionType;
    let method = meta.resolve_method(member, args)?;

    let thread =
        registration_thread_of(receiver).ok_or(crate::meta::InvokeError::TargetBorrowFailed)?;
    let blocking = match conn_type {
        ConnectionType::Queued => false,
        ConnectionType::BlockingQueued => true,
        ConnectionType::Auto if thread != ThreadId::current() => false,
        ConnectionType::Auto | ConnectionType::Direct => {
            return Err(crate::meta::InvokeError::TargetBorrowFailed);
        }
    };

    let args_owned = args.to_vec();
    if !blocking {
        let event = Event::new(EventKind::MetaCall(Box::new(
            move |target: &mut dyn std::any::Any| {
                // Qt drops the return value of a queued invoke along with any failure.
                let _ = method.invoke(target, &args_owned);
            },
        )));
        crate::event_loop::post_event_to_thread(thread, receiver, event);
        return Ok(Variant::Invalid);
    }

    if thread == ThreadId::current() {
        // Qt: "Dead lock detected" (qmetaobject.cpp:1750); QMetaMethod::invoke returns
        // DeadLockDetected (qmetaobject.cpp:2926-2929). Reported as an error instead of blocking forever.
        return Err(crate::meta::InvokeError::ExecutionFailed(
            "BlockingQueued invoke on the receiver's own thread would deadlock".to_string(),
        ));
    }
    let (tx, rx) = std::sync::mpsc::channel::<Result<Variant, crate::meta::InvokeError>>();
    let event = Event::new(EventKind::MetaCall(Box::new(
        move |target: &mut dyn std::any::Any| {
            let result = method.invoke(target, &args_owned);
            let _ = tx.send(result);
        },
    )));
    crate::event_loop::post_event_to_thread(thread, receiver, event);
    match rx.recv() {
        Ok(result) => result,
        // The loop dropped the call without running it (receiver gone or loop shut down).
        Err(_) => Err(crate::meta::InvokeError::TargetBorrowFailed),
    }
}

/// Moves an object's physical registration to `thread_id`. Only `move_to_thread` calls this.
pub(crate) fn set_registration_thread(id: ObjectId, thread_id: ThreadId) {
    if let Ok(reg) = GLOBAL_OBJECT_REGISTRY.read()
        && let Some(entry) = reg.get(&id)
    {
        *write_unpoisoned(&entry.registration_thread) = thread_id;
    }
}

/// Registered children of `id` according to the global registry: every registered object whose own
/// record names `id` as its parent (this covers an intermediate `id` that has no record in the
/// registry, and a child linked while unregistered and registered afterwards). It is read from
/// [`PARENT_LINK_INDEX`] under the registry's read lock, so it holds for the registered objects at
/// that moment and costs what `id` has below it, not what is registered.
fn registered_children_below(id: ObjectId) -> Vec<ObjectId> {
    let _registry = read_unpoisoned(&GLOBAL_OBJECT_REGISTRY);
    let index = lock_unpoisoned(&PARENT_LINK_INDEX);
    let children = index.children.get(&id).cloned().unwrap_or_default();
    INDEX_ENTRIES_READ.with(|read| read.set(read.get() + children.len()));
    children
}

thread_local! {
    /// Entries `registered_children_below` read from the index on this thread, in total.
    static INDEX_ENTRIES_READ: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many child entries the calling thread has read from the parent-link index so far. For tests
/// of the cost of a thread move: the difference across a call is the number of index entries the
/// call read, which is what lies below the objects it walked, whatever else is registered. It is
/// per thread, so the activity of other threads does not enter it.
#[doc(hidden)]
pub fn child_index_entries_read() -> usize {
    INDEX_ENTRIES_READ.with(std::cell::Cell::get)
}

/// The thread of an owned child: the registry's record when it has one (readable while a callback
/// of the child runs), else the object's own data.
fn owned_thread(child: &OwnedObject<dyn QObject>) -> Option<ThreadId> {
    query_object_thread(OwnedObject::object_id(child))
        .or_else(|| child.with(|c| c.object_data().thread()))
}

/// The ids of a subtree being collected for a thread move: the objects found (`all_ids`, root
/// first) and the ids still to inspect. Membership is a hash lookup, so a subtree of `n` objects
/// costs `O(n)` and not `O(n²)`.
#[derive(Default)]
struct SubtreeScan {
    all_ids: Vec<ObjectId>,
    found: std::collections::HashSet<ObjectId>,
    queue: Vec<ObjectId>,
    queued: std::collections::HashSet<ObjectId>,
}

impl SubtreeScan {
    fn add(&mut self, id: ObjectId) {
        if self.found.insert(id) {
            self.all_ids.push(id);
        }
    }

    fn enqueue(&mut self, id: ObjectId) {
        if !self.found.contains(&id) && self.queued.insert(id) {
            self.queue.push(id);
        }
    }

    fn pop(&mut self) -> Option<ObjectId> {
        let id = self.queue.pop()?;
        self.queued.remove(&id);
        Some(id)
    }
}

/// Walks `data` and its unregistered `owned_children`, checking that no owned child is borrowed by
/// a running callback and that every unregistered owned descendant allows moving to another
/// thread, while queuing registered children for registry-backed inspection.
fn inspect_owned_tree(
    data: &ObjectData,
    scan: &mut SubtreeScan,
) -> Result<(), super::thread::MoveError> {
    use super::thread::MoveError;
    scan.add(data.id());
    for owned in data.owned_children() {
        if OwnedObject::is_borrowed(owned) {
            return Err(MoveError::Busy);
        }
        let owned_id = OwnedObject::object_id(owned);
        let live_entry = {
            let Ok(reg) = GLOBAL_OBJECT_REGISTRY.read() else {
                return Err(MoveError::Busy);
            };
            reg.get(&owned_id)
                .filter(|e| e.liveness.load(Ordering::Acquire))
                .cloned()
        };
        if let Some(entry) = live_entry {
            if entry.borrow_state.borrow_flag.load(Ordering::Acquire) {
                return Err(MoveError::Busy);
            }
            if *read_unpoisoned(&entry.registration_thread) != ThreadId::current() {
                return Err(MoveError::WrongThread);
            }
            scan.enqueue(owned_id);
        } else {
            // SAFETY: `owned_id` has no live registry entry (the branch above handles a live
            // one), so no callback of it can start; `is_borrowed` above found no running one, and
            // the owner (`owned`) is alive and borrowed here, so nothing frees the object.
            let owned = unsafe { owned.as_ref_unchecked() };
            if !owned.object_data().can_move_to_thread() {
                return Err(MoveError::NotMovable);
            }
            inspect_owned_tree(owned.object_data(), scan)?;
        }
    }
    for &child_id in data.children() {
        scan.enqueue(child_id);
    }
    for child_id in registered_children_below(data.id()) {
        scan.enqueue(child_id);
    }
    Ok(())
}

/// Drains `queue`, requiring every live registered descendant to be unborrowed on the current
/// thread and to be opted in (`ObjectData::can_move_to_thread`). An id that has no live record in
/// the registry (such as an unregistered non-owned child, or a record whose liveness is already
/// false) is kept in `all_ids` and its registered children are still queued for inspection.
fn collect_queued_descendants(scan: &mut SubtreeScan) -> Result<(), super::thread::MoveError> {
    use super::thread::MoveError;
    while let Some(child_id) = scan.pop() {
        if scan.found.contains(&child_id) {
            continue;
        }
        let live_entry = {
            let Ok(reg) = GLOBAL_OBJECT_REGISTRY.read() else {
                return Err(MoveError::Busy);
            };
            reg.get(&child_id)
                .filter(|e| e.liveness.load(Ordering::Acquire))
                .cloned()
        };
        if let Some(entry) = live_entry {
            if entry.borrow_state.borrow_flag.load(Ordering::Acquire) {
                return Err(MoveError::Busy);
            }
            if *read_unpoisoned(&entry.registration_thread) != ThreadId::current() {
                return Err(MoveError::WrongThread);
            }
            with_object(child_id, |obj| {
                if !obj.object_data().can_move_to_thread() {
                    return Err(MoveError::NotMovable);
                }
                inspect_owned_tree(obj.object_data(), scan)
            })
            .ok_or(MoveError::Busy)??;
        } else {
            scan.add(child_id);
            for descendant_id in registered_children_below(child_id) {
                scan.enqueue(descendant_id);
            }
        }
    }
    Ok(())
}

/// Stage A of [`crate::object::move_to_thread_by_id`]: validates `id` and collects the full
/// registered subtree (`all_ids`, root first), releasing every borrow before returning.
///
/// Returns `Ok(None)` when `id` is already on `target_thread`. Refuses with
/// `MoveByIdError::NotMovable` when `id` or any descendant answers `false` to
/// `ObjectData::can_move_to_thread`. This stage sends nothing and changes nothing, so every refusal
/// happens before the first `ThreadChange`.
pub(crate) fn prepare_move_to_thread(
    id: ObjectId,
    target_thread: ThreadId,
    current_caller_thread: ThreadId,
) -> Result<Option<Vec<ObjectId>>, super::thread::MoveByIdError> {
    use super::thread::MoveByIdError;
    let entry = {
        let Ok(reg) = GLOBAL_OBJECT_REGISTRY.read() else {
            return Err(MoveByIdError::UnknownObject);
        };
        let Some(entry) = reg.get(&id).cloned() else {
            return Err(MoveByIdError::UnknownObject);
        };
        entry
    };
    if !entry.liveness.load(Ordering::Acquire) {
        return Err(MoveByIdError::UnknownObject);
    }
    if entry.borrow_state.borrow_flag.load(Ordering::Acquire) {
        return Err(MoveByIdError::Busy);
    }
    let on_registration_thread =
        *read_unpoisoned(&entry.registration_thread) == ThreadId::current();
    if !on_registration_thread {
        if read_unpoisoned(&entry.parent).is_some() {
            return Err(MoveByIdError::HasParent);
        }
        return Err(MoveByIdError::WrongThread);
    }

    let initial = with_object(id, |obj| {
        let data = obj.object_data();
        if data.parent().is_some() {
            return Err(MoveByIdError::HasParent);
        }
        if data.thread_id() != current_caller_thread {
            return Err(MoveByIdError::WrongThread);
        }
        if data.thread_id() == target_thread {
            return Ok(None);
        }
        if !data.can_move_to_thread() {
            return Err(MoveByIdError::NotMovable);
        }
        let mut scan = SubtreeScan::default();
        inspect_owned_tree(data, &mut scan)?;
        Ok(Some(scan))
    })
    .ok_or(MoveByIdError::Busy)??;

    let Some(mut scan) = initial else {
        return Ok(None);
    };
    collect_queued_descendants(&mut scan)?;
    Ok(Some(scan.all_ids))
}

/// Pre-validation and subtree collection for the deprecated [`crate::object::move_to_thread`].
///
/// Checks the root `object_data` and every owned or registered descendant before any
/// `ThreadChange` is sent or any state is modified. A registered descendant that cannot be
/// borrowed on the current thread is refused instead of being assumed movable, while unregistered
/// owned descendants are inspected through `owned_children`.
pub(crate) fn prepare_move_to_thread_from_data(
    object_data: &ObjectData,
    target_thread: ThreadId,
    current_caller_thread: ThreadId,
) -> Result<Option<Vec<ObjectId>>, super::thread::MoveError> {
    use super::thread::MoveError;
    if object_data.parent().is_some() {
        return Err(MoveError::HasParent);
    }
    if object_data.thread_id() != current_caller_thread {
        return Err(MoveError::WrongThread);
    }
    if object_data.borrow_state.borrow_flag.load(Ordering::Acquire) {
        return Err(MoveError::Busy);
    }
    {
        let Ok(reg) = GLOBAL_OBJECT_REGISTRY.read() else {
            return Err(MoveError::Busy);
        };
        if let Some(entry) = reg
            .get(&object_data.id())
            .filter(|e| e.liveness.load(Ordering::Acquire))
        {
            if entry.borrow_state.borrow_flag.load(Ordering::Acquire) {
                return Err(MoveError::Busy);
            }
            if *read_unpoisoned(&entry.registration_thread) != ThreadId::current() {
                return Err(MoveError::WrongThread);
            }
        }
    }
    if object_data.thread_id() == target_thread {
        return Ok(None);
    }
    if !object_data.can_move_to_thread() {
        return Err(MoveError::NotMovable);
    }

    let mut scan = SubtreeScan::default();
    inspect_owned_tree(object_data, &mut scan)?;
    collect_queued_descendants(&mut scan)?;
    Ok(Some(scan.all_ids))
}

/// Queries the target object's thread affinity across threads.
/// Queries the parent ObjectId recorded in the global object registry for `id`, or `None` if
/// `id` is not registered. Returns `Some(None)` if registered but parent is `None`.
///
/// # Atomicity Boundary
/// This query guarantees single-record read atomicity protected by the target object's `parent` lock.
/// Calling this function independently multiple times across different objects does NOT constitute
/// a cross-object consistent snapshot: concurrent hierarchy operations may interleave between individual
/// calls. For multi-record cross-object parent consistency, use [`query_object_parents_snapshot`].
pub fn query_object_parent(id: ObjectId) -> Option<Option<ObjectId>> {
    let registry = GLOBAL_OBJECT_REGISTRY.read().ok()?;
    let record = registry.get(&id)?;
    let parent = *record.parent.read().ok()?;
    Some(parent)
}

/// Queries the list of children recorded in the global object registry for `id`, or `None` if
/// `id` is not registered.
///
/// # Atomicity Boundary
/// This query guarantees single-record read atomicity protected by the target object's `children` lock.
/// Querying multiple parent objects independently does NOT provide cross-object snapshot consistency;
/// concurrent hierarchy operations may interleave between calls to different objects.
pub fn query_object_children(id: ObjectId) -> Option<Vec<ObjectId>> {
    let registry = GLOBAL_OBJECT_REGISTRY.read().ok()?;
    let record = registry.get(&id)?;
    let children = record.children.read().ok()?.clone();
    Some(children)
}

/// Queries the parent ObjectIds of two objects simultaneously in a consistent multi-record snapshot,
/// acquiring their parent read locks in a fixed global order (ascending ObjectId).
/// Returns `None` if either object is not registered or a lock is poisoned.
///
/// # Atomicity Boundary
/// Unlike separate invocations of [`query_object_parent`], this function provides cross-object snapshot
/// consistency across both objects by holding their read locks concurrently.
pub fn query_object_parents_snapshot(
    id1: ObjectId,
    id2: ObjectId,
) -> Option<(Option<ObjectId>, Option<ObjectId>)> {
    if id1 == id2 {
        let p = query_object_parent(id1)?;
        return Some((p, p));
    }
    let registry = GLOBAL_OBJECT_REGISTRY.read().ok()?;
    let r1 = registry.get(&id1)?;
    let r2 = registry.get(&id2)?;
    let (p1, p2) = if id1 < id2 {
        let g1 = r1.parent.read().ok()?;
        let g2 = r2.parent.read().ok()?;
        (*g1, *g2)
    } else {
        let g2 = r2.parent.read().ok()?;
        let g1 = r1.parent.read().ok()?;
        (*g1, *g2)
    };
    Some((p1, p2))
}

pub fn query_object_thread(id: ObjectId) -> Option<ThreadId> {
    if let Ok(reg) = GLOBAL_OBJECT_REGISTRY.read()
        && let Some(entry) = reg.get(&id)
    {
        return Some(*read_unpoisoned(&entry.thread_id));
    }
    if let Some(map) = read_unpoisoned(&GLOBAL_OBJECT_THREADS).as_ref() {
        return map.get(&id).copied();
    }
    None
}

/// Registers `ptr` under the id of `data`. See [`register_record`]. This form cannot fail: a parent
/// link that would close a cycle is left out of the record.
fn register_object_metadata(data: &ObjectData, ptr: *mut dyn QObject) {
    // The record gets copies; nothing keeps a reference to `data` past the call.
    let _ = register_record(data, ptr, None);
}

/// Builds the record of `data` and inserts it into the registry, replacing the record of an
/// object registered earlier under the same id.
///
/// The record carries the parent link `data` has, which is a new parent edge in the registry, so
/// it is made like every other: with `HIERARCHY_MUTATION` held, and with the check that the parent
/// is not a descendant done against the registry the record goes into. `set_parent` accepts an
/// unregistered parent for an unregistered child, so objects can have been linked into a cycle
/// that no registry walk could see. A link that would close one is not recorded, and neither is
/// a child that is the object itself or one of its ancestors: the registry never holds a cycle.
/// A link that cannot be checked, because a lock on the way is poisoned, is not recorded either.
///
/// The children the record lists are the object's own, without those that are registered under
/// another parent (or none): a listed child that the registry says belongs elsewhere would make
/// the lists disagree with the links, and lists that name each other in a ring would never end a
/// walk down them. A child that is not registered yet is listed as the object has it.
///
/// When the object is registered again while it is registered (the record being replaced shares
/// its liveness token), the replaced record is the truth for its parent and children: it has
/// children that only the registry knows (a child of another registration thread, listed by
/// `set_parent`), and the object's own copy of its links may be older. They are kept.
///
/// With `adopted_by`, the object is also listed among the children of that parent, and a link
/// that would close a cycle, or cannot be checked, is refused: nothing is inserted and the caller,
/// who still owns the object, gets the error.
fn register_record(
    data: &ObjectData,
    ptr: *mut dyn QObject,
    adopted_by: Option<ObjectId>,
) -> Result<(), ReparentError> {
    let id = data.id();
    let thread_id = data.thread_id();
    {
        let _hierarchy = lock_hierarchy();
        let mut registry = write_unpoisoned(&GLOBAL_OBJECT_REGISTRY);
        let same_object = registry
            .get(&id)
            .filter(|previous| Arc::ptr_eq(&previous.liveness, &data.liveness))
            .cloned();
        let mut parent = match (&same_object, adopted_by) {
            (_, Some(parent_id)) => Some(parent_id),
            (Some(previous), None) => *read_unpoisoned(&previous.parent),
            (None, None) => data.parent(),
        };
        let mut children = match &same_object {
            Some(previous) => read_unpoisoned(&previous.children).clone(),
            None => data.children.clone(),
        };
        let verdict = match parent {
            Some(parent_id) => would_create_cycle_in(&registry, id, parent_id),
            None => Ok(false),
        };
        match (verdict, adopted_by) {
            (Ok(false), _) => {}
            (Ok(true), Some(_)) => return Err(ReparentError::InvalidParent),
            (Err(error), Some(_)) => return Err(error),
            (Ok(true) | Err(_), None) => parent = None,
        }
        let ancestors = ancestors_in(&registry, parent);
        children.retain(|child_id| {
            *child_id != id
                && !ancestors.contains(child_id)
                && registry
                    .get(child_id)
                    .is_none_or(|record| *read_unpoisoned(&record.parent) == Some(id))
        });
        // The parent's list is taken before anything is inserted, so that a poisoned list refuses
        // the adoption with nothing changed, as every other commit does. The parent is borrowed by
        // the caller, so it is registered. Its registry list is what still reaches the child while
        // a running callback of the parent hides it from the parent's own `children`. L0 (write)
        // before L1, as everywhere.
        let parent_record = adopted_by.and_then(|parent_id| registry.get(&parent_id).cloned());
        let mut parent_list = match &parent_record {
            Some(parent_record) => Some(
                parent_record
                    .children
                    .write()
                    .map_err(|_| ReparentError::Busy)?,
            ),
            None => None,
        };
        let record = Arc::new(ObjectRecord {
            id,
            thread_id: RwLock::new(thread_id),
            registration_thread: RwLock::new(ThreadId::current()),
            ptr: RegisteredPtr(ptr),
            parent: RwLock::new(parent),
            children: RwLock::new(children),
            liveness: Arc::clone(&data.liveness),
            generation: AtomicU32::new(data.generation),
            borrow_state: Arc::clone(&data.borrow_state),
            reparenting: AtomicBool::new(false),
            pending_child_changes: Mutex::new(Vec::new()),
            has_pending_child_changes: AtomicBool::new(false),
            pending_parent_sync: Mutex::new(None),
            has_pending_parent_sync: AtomicBool::new(false),
        });
        if let Some(previous) = registry.insert(id, record) {
            // Re-registering the same object (a second registration of a live object, possibly
            // at a new address) shares the liveness token with the replaced record. Killing it
            // would mark the live object dead and null every `QPointer` to it. Only a different
            // object that reused the id is retired.
            if !Arc::ptr_eq(&previous.liveness, &data.liveness) {
                previous.liveness.store(false, Ordering::Release);
            }
        }
        lock_unpoisoned(&PARENT_LINK_INDEX).relink(id, parent);
        if let Some(list) = parent_list.as_mut()
            && !list.contains(&id)
        {
            list.push(id);
        }
    }
    register_object_thread(id, thread_id);
    Ok(())
}

/// Registers a pinned object.
///
/// The registry's pointer is made from the reference inside `obj`, so the next direct use of the
/// object through another path invalidates it (rule 1 in the [`crate::object`] module
/// documentation). Miri does not check that for `!Unpin` types, so a `!Unpin` object passing
/// under Miri is no evidence that the use is sound. There is no pointer-based form for pinned
/// objects yet. No type in this crate is registered through this function.
///
/// # Safety
/// The pinned object must remain alive and at its pinned address until unregistered. The thread
/// rules of [`crate::object`] apply. The caller must ensure that no overlapping mutable or
/// mutable/shared aliases exist while a callback runs, and that the owner makes no direct use of
/// the object between callbacks.
pub unsafe fn register_pinned_qobject(mut obj: std::pin::Pin<&mut dyn QObject>) {
    // SAFETY: only the pointer is taken, nothing is moved out of the pin; the caller promises (see
    // `# Safety`) that the object stays at its pinned address while it is registered.
    let ptr = unsafe { obj.as_mut().get_unchecked_mut() as *mut dyn QObject };
    // SAFETY: `ptr` comes from the live pinned object; the shared reference ends with
    // `object_data`'s use in the call below, which copies what it keeps.
    let data = unsafe { (*ptr).object_data() };
    register_object_metadata(data, ptr);
}

/// Registers the object at `ptr`. The registry keeps this exact pointer.
///
/// Every access the owner makes to the object between callbacks must go through `ptr` or a
/// pointer derived from it; then the registry's pointer stays valid. That holds for:
/// - an object on the heap held by an [`OwnedObject`], registered with
///   [`OwnedObject::as_non_null`] (what [`register_owned_qobject`] does);
/// - an object in a variable or field, registered with a pointer taken by `&raw mut x`. The owner
///   may read `x` directly between callbacks, but writes it only through `ptr` (rule 7 in the
///   [`crate::object`] module documentation): under Stacked Borrows a write through `x` itself
///   invalidates the registered pointer.
///
/// A pointer made from a reference (`&mut x as *mut _`, `NonNull::from(&mut x)`) or from a `Box`
/// that is used afterwards does not qualify (rule 1 in the [`crate::object`] module
/// documentation).
///
/// # Safety
/// - `ptr` points to a live object that stays at this address until `unregister_qobject`
///   completes.
/// - Between callbacks, the owner reaches the object only through `ptr` or pointers derived from
///   it, as described above.
/// - While a callback of the object runs, nothing else accesses the object, not even to read it.
/// - Registering the object again, while it is registered, gives the registry the new pointer, and
///   nothing uses the old one afterwards. That does not happen while a callback of the object
///   runs.
/// - Unregistering the object while a callback of it runs is allowed only if its memory stays
///   valid and unmoved until that callback returns (see [`unregister_qobject`]). An owner type
///   whose drop calls `destroy_when_released` does this; any other owner does not unregister or
///   drop the object during a callback.
/// - The thread rules of [`crate::object`] apply.
pub unsafe fn register_qobject_ptr(ptr: NonNull<dyn QObject>) {
    let raw = ptr.as_ptr();
    // SAFETY: the caller guarantees (`# Safety`) that `ptr` points to a live object and that no
    // callback of it runs, so forming a shared reference for the duration of `object_data` is sound.
    let data = unsafe { (*raw).object_data() };
    register_object_metadata(data, raw);
}

/// Moves `obj` to the heap, registers it, and returns its owner. Dropping the owner drops the
/// object, which unregisters it.
///
/// The owner may use the object through [`OwnedObject::with`] and [`OwnedObject::with_mut`]
/// between callbacks, and may be moved; the registry's pointer stays valid (see [`OwnedObject`]).
///
/// # Safety
/// While a callback of the object runs, nothing outside it accesses the object, not even to read
/// it (the owner's `with` and `with_mut` refuse then). Dropping the owner while a callback of the object runs is
/// allowed: [`OwnedObject::drop`] unregisters the object at once and frees its memory only after
/// the running callback returns. The owner is used on the registration thread; it may be dropped on
/// another thread, which forwards the destruction to the registration thread (see
/// `destroy_owned`). The thread rules of [`crate::object`] apply.
pub unsafe fn register_owned_qobject<T: QObject + 'static>(obj: T) -> OwnedObject<T> {
    let owned = OwnedObject::new(obj);
    let ptr: NonNull<dyn QObject> = OwnedObject::<T>::as_non_null(&owned);
    // SAFETY: `ptr` is `OwnedObject::as_non_null(&owned)`, the owner keeps the object at this heap
    // address until it is dropped, and `owned` is returned (moved, which does not move the object).
    // The caller upholds the rest through this function's `# Safety` section.
    unsafe { register_qobject_ptr(ptr) };
    owned
}

/// Unregisters an object from all registries and cleans up active timers and connections.
///
/// This is memory-neutral: it works from the registry record and ids and never reads, writes or
/// forms a reference to the object. It may be called while a callback of the object runs, which
/// is how `destroy_when_released` uses it. Freeing the object is a separate step.
///
/// # Safety
/// The object's memory must stay valid, unmoved and undropped until every callback of the object
/// that was running when this is called has returned (until its `ObjectBorrowGuard` is dropped).
/// No concurrent registry callback can begin: callbacks run only on the registration thread and
/// `registered_ptr` rejects an unregistered id. Invoke on the object's physical registration
/// thread so its timers are stopped from that thread's timer registry.
pub unsafe fn unregister_qobject(id: ObjectId) {
    // This cleanup cannot report a failure, and a step that is skipped leaves a record that
    // points to freed memory, an id in a parent's list, or a child with a dead parent. The locks
    // below guard single stores and plain container edits, which a panic cannot leave half done,
    // so a poisoned lock is used as it is (see `read_unpoisoned`).
    let removed = {
        let mut registry = write_unpoisoned(&GLOBAL_OBJECT_REGISTRY);
        let removed = registry.remove(&id);
        if removed.is_some() {
            lock_unpoisoned(&PARENT_LINK_INDEX).relink(id, None);
        }
        removed
    };
    let state = removed
        .as_ref()
        .map(|entry| Arc::clone(&entry.borrow_state));
    if let Some(entry) = removed {
        entry.liveness.store(false, Ordering::Release);
        // Lock order: the registry (L0), then the `children` of a parent (L1), then the `parent`
        // of a child (L2). This function does not take `HIERARCHY_MUTATION`: it only removes links.
        let registry = read_unpoisoned(&GLOBAL_OBJECT_REGISTRY);
        // Unlist from the parent. The parent is named by this object's own `parent` (L2),
        // which must not be held while the parent's list (L1) is locked: it is read and let
        // go, the parent's list is locked, and the link is read again under that lock.
        // Changing the link of a listed object needs that list, so the link is stable under
        // it; if it changed in between, the unlisting is done for the new parent. The loop is
        // bounded, and ends at once when the parent is gone.
        for _ in 0..=registry.len() {
            let Some(parent_id) = *read_unpoisoned(&entry.parent) else {
                break;
            };
            #[cfg(test)]
            test_hooks::run(test_hooks::Point::AfterLinkRead);
            let Some(parent) = registry.get(&parent_id) else {
                break;
            };
            let mut children = write_unpoisoned(&parent.children);
            if *read_unpoisoned(&entry.parent) == Some(parent_id) {
                // A parent deleting its children in list order finds each one at the front.
                if let Some(at) = children.iter().position(|&child_id| child_id == id) {
                    children.remove(at);
                }
                break;
            }
        }
        for child_id in read_unpoisoned(&entry.children).iter() {
            if let Some(child) = registry.get(child_id) {
                // A listed child whose own link names another parent (a stale entry) is not
                // this object's child and keeps its parent.
                let mut link = write_unpoisoned(&child.parent);
                if *link == Some(id) {
                    store_link(&registry, child, &mut link, None);
                }
            }
        }
    }
    // Unregistering is the object's destruction (this is what delivering a `DeferredDelete`
    // does, and what the destruction of an owner does first). The object is dead and out of the
    // registry, its links are cut, and no lock of ours is held. `destroyed` goes out now, before
    // the connections of the object are cut, so its receivers are still connected (Qt emits it
    // before it disconnects anything, qobject.cpp:1086-1105). An object that was never
    // registered emits it from its drop instead.
    if let Some(state) = state {
        state.emit_destroyed_once(id);
    }
    unregister_object_thread(id);
    // Posted events addressed to a destroyed object are removed, never delivered (Qt
    // `~QObjectPrivate`, qobject.cpp:206-207).
    crate::event_loop::purge_posted_events_for_receiver(id);
    crate::signal::disconnect_all_for_object(id);
    crate::timer::stop_timers_for_object(id);
}

/// Destroys the object whose `DeferredDelete` was just delivered, with everything below it (Qt
/// `QObject::event` runs `qDeleteInEventHandler`, which deletes the object and so its children).
///
/// * An object its parent owns is taken out of the parent's `owned_children` and dropped through
///   its owner: the usual destruction, which frees the memory, tells the parent
///   (`ChildRemoved`), and deletes the object's own children. When a callback of the parent runs,
///   the owner can't be taken out now: the object is unregistered (with its subtree) at once and
///   the owner is taken out, and dropped, when that callback returns.
/// * Any other object has an owner this function can't reach (a standalone [`OwnedObject`], or the
///   caller of a registration through a pointer). It is unregistered with everything the
///   registry lists below it, and forgotten by its parent's list. Its owner frees the memory.
///
/// Runs on the object's registration thread, after the delivery returned. An id that is no longer
/// registered is left alone.
pub(crate) fn destroy_deferred(id: ObjectId) {
    let Some(record) = GLOBAL_OBJECT_REGISTRY
        .read()
        .ok()
        .and_then(|r| r.get(&id).cloned())
    else {
        return;
    };
    let parent = *read_unpoisoned(&record.parent);
    // Ownership is read from the parent's list: the object itself may be borrowed by a callback
    // further up the stack, which its own flag would hide.
    if let Some(parent_id) = parent {
        match with_object_mut(parent_id, |p| p.object_data_mut().take_owned_child(id)) {
            Some(Some(owner)) => {
                drop(owner);
                return;
            }
            None if registered_and_borrowed(parent_id) => {
                // Whether the parent owns it is settled when its callback returns.
                unregister_deferred_subtree(id, &record.borrow_state);
                edit_child_list(parent_id, ChildListChange::DeleteOwned(id));
                return;
            }
            _ => {}
        }
    }
    unregister_deferred_subtree(id, &record.borrow_state);
    if let Some(parent_id) = parent {
        edit_child_list(parent_id, ChildListChange::Remove(id));
    }
}

/// The ids `data` lists as children that it does not own (attached with `set_parent_by_id`).
fn unowned_child_ids(data: &ObjectData) -> Vec<ObjectId> {
    if data.owned_children.is_empty() {
        return data.children.clone();
    }
    let owned: std::collections::HashSet<ObjectId> = data
        .owned_children
        .iter()
        .map(OwnedObject::object_id)
        .collect();
    data.children
        .iter()
        .copied()
        .filter(|id| !owned.contains(id))
        .collect()
}
/// Unregisters `id` and everything the registry lists below it, the deepest first; `destroyed` of
/// `id` goes out before any descendant is unregistered (and so before theirs).
fn unregister_deferred_subtree(id: ObjectId, state: &ObjectBorrowState) {
    state.emit_destroyed_once(id);
    unregister_descendants(id);
    // SAFETY: the delivery that ran this has returned, so no callback of the object runs (a
    // callback further up the stack keeps its own `Arc` to the record, and the memory stays until
    // its owner frees it), and this runs on the registration thread.
    unsafe { unregister_qobject(id) };
}

/// Test-only fixed points inside the hierarchy operations. A test installs a hook on its own
/// thread to hold an operation at a point where another thread can then act.
#[cfg(test)]
pub(crate) mod test_hooks {
    use std::cell::RefCell;

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub(crate) enum Point {
        /// After the check that the new parent is not a descendant, before any list is locked
        /// (`set_parent`, `reparent_owned_object`, `swap_hierarchy_and_ownership`). Everything
        /// the operation serialises on is held.
        AfterCycleCheck,
        /// `unregister_qobject`: after the child's parent link has been read and let go, before the
        /// parent's list is locked.
        AfterLinkRead,
    }

    type Hook = Box<dyn FnOnce()>;

    thread_local! {
        static HOOKS: RefCell<Vec<(Point, Hook)>> = const { RefCell::new(Vec::new()) };
    }

    /// Runs `hook` once, the next time the calling thread reaches `point`.
    pub(crate) fn on_next(point: Point, hook: impl FnOnce() + 'static) {
        HOOKS.with(|hooks| hooks.borrow_mut().push((point, Box::new(hook))));
    }

    pub(crate) fn run(point: Point) {
        let hook = HOOKS.with(|hooks| {
            let mut hooks = hooks.borrow_mut();
            let index = hooks.iter().position(|(at, _)| *at == point)?;
            Some(hooks.remove(index).1)
        });
        if let Some(hook) = hook {
            hook();
        }
    }
}

mod data;
mod reparent;
#[cfg(test)]
mod tests;
mod traits;

pub use data::*;
pub use reparent::*;
#[allow(unused_imports)]
use reparent::{
    ReparentGuard, ShortTermBorrowGuard, ancestors_in, in_different_thread, liveness_of, record_of,
    registered_children_of, release_child, would_create_cycle_in,
};
pub use traits::*;
