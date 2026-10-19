// SPDX-License-Identifier: LGPL-3.0-only
use crate::event::EventFilterChain;
use crate::signal::Signal;
use crate::variant::Variant;

use super::owned::OwnedObject;
use super::thread::{ThreadContext, ThreadId};
use crate::event::{Event, EventKind};
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

#[cfg(not(test))]
type HierarchyGuard = MutexGuard<'static, ()>;
#[cfg(test)]
type HierarchyGuard = Option<MutexGuard<'static, ()>>;

#[cfg(not(test))]
fn lock_hierarchy() -> HierarchyGuard {
    HIERARCHY_MUTATION.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
fn lock_hierarchy() -> HierarchyGuard {
    if test_hooks::mutant("skip_hierarchy_lock") {
        return None;
    }
    Some(HIERARCHY_MUTATION.lock().unwrap_or_else(PoisonError::into_inner))
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
#[derive(Default)]
pub(crate) struct ObjectBorrowState {
    pub(crate) borrow_flag: AtomicBool,
    /// Set when an owned child was destroyed while this object's callback was running: the guard
    /// of that callback then frees the memory it kept (see `destroy_when_released`).
    pub(crate) free_when_released: AtomicBool,
    /// Set when the underlying object memory is deallocated or its destructor has begun.
    pub(crate) memory_destroyed: AtomicBool,
}

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
    pending_parent_sync: Mutex<Option<(Option<ObjectId>, Option<(ObjectId, Arc<AtomicBool>)>)>>,
    has_pending_parent_sync: AtomicBool,
}
/// One edit to the `children` list in an object's own `ObjectData`.
enum ChildListChange {
    Add(ObjectId),
    Remove(ObjectId),
}

impl ChildListChange {
    fn apply(&self, mut data: ObjectDataMut<'_>) {
        match *self {
            ChildListChange::Add(id) => data.add_child(id),
            ChildListChange::Remove(id) => data.remove_child(id),
        }
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
                let changes = std::mem::take(&mut *lock_unpoisoned(&self._entry.pending_child_changes));
                // A dead object is not written to: its memory may already be freed. Every drop of a
                // registered object marks the shared liveness token dead before the memory goes.
                if self._entry.liveness.load(Ordering::Acquire) {
                    // SAFETY: the callback that held this borrow has returned, so no reference into
                    // the object is live, and the object is alive.
                    let mut data = unsafe { (*self.ptr).object_data_mut() };
                    for change in &changes {
                        change.apply(ObjectDataMut::new(data.raw_data_mut()));
                    }
                }
            }
            if self
                ._entry
                .has_pending_parent_sync
                .swap(false, Ordering::AcqRel)
            {
                let sync = lock_unpoisoned(&self._entry.pending_parent_sync).take();
                if let Some((new_parent, target_liveness)) = sync {
                    if !self.borrow_state.memory_destroyed.load(Ordering::Acquire) {
                        let mut data = unsafe { (*self.ptr).object_data_mut() };
                        let raw = data.raw_data_mut();
                        raw.parent = new_parent;
                        raw.parent_liveness = target_liveness;
                    }
                }
            }
        }
        if self
            .borrow_state
            .free_when_released
            .swap(false, Ordering::AcqRel)
        {
            free_parked_children(&self.borrow_state);
        }
    }
}

/// An owned child that waits to be freed, with the borrow state of the callback it waits for.
type ParkedChild = (Arc<ObjectBorrowState>, OwnedObject<dyn QObject>);
thread_local! {
    /// Owned children whose parent was destroyed while a callback of theirs was running. The guard
    /// of that callback frees them when it ends.
    static PARKED_CHILDREN: RefCell<Vec<ParkedChild>> = const { RefCell::new(Vec::new()) };
}

/// Destroys an owner whose object has a callback still running: an owned child whose parent is
/// being dropped from inside that callback, or an owner (such as a `Timer` handle) dropped by
/// one of its own slots. An owner whose object runs no callback is simply dropped. Otherwise the
/// object is destroyed at once, and so is everything the
/// registry lists below it: it is unregistered, so it is no longer alive, gets no events or
/// timers, and its connections to other objects are gone. Its memory is kept until the callback
/// returns, since that callback still runs on `&mut self`; the guard of the callback frees it
/// (`free_parked_children`), and with it the memory of the objects below it. The object is not
/// read here: its id comes from its owner.
///
/// The steps, in order: the borrow flag is read (the only read of the borrow state), the
/// allocation is parked and flagged `free_when_released`, the registry-listed descendants are
/// unregistered, then the object itself. Only the guard of the running callback frees the
/// parked allocation, after it has released the borrow flag.
pub(crate) fn destroy_when_released(child: OwnedObject<dyn QObject>) {
    let child_id = OwnedObject::object_id(&child);
    let borrow_state = Arc::clone(OwnedObject::borrow_state(&child));
    if !borrow_state.borrow_flag.load(Ordering::Acquire) {
        drop(child);
        return;
    }
    borrow_state
        .free_when_released
        .store(true, Ordering::Release);
    PARKED_CHILDREN.with(|parked| {
        parked.borrow_mut().push((borrow_state, child));
    });
    unregister_descendants(child_id);
    // SAFETY: the running callback holds the borrow flag and its own `Arc` to the record, and the
    // memory is not freed until it returns. No other callback can start on an unregistered object.
    unsafe { unregister_qobject(child_id) };
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

/// Frees the children parked while the callback that held `flag` was running.
fn free_parked_children(state: &Arc<ObjectBorrowState>) {
    let ready: Vec<OwnedObject<dyn QObject>> = PARKED_CHILDREN.with(|parked| {
        let mut parked = parked.borrow_mut();
        let (ready, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut *parked)
            .into_iter()
            .partition(|(held, _)| Arc::ptr_eq(held, state));
        *parked = waiting;
        ready.into_iter().map(|(_, child)| child).collect()
    });
    for mut child in ready {
        child.object_data_mut().clear_parent();
        drop(child);
    }
}

static GLOBAL_OBJECT_REGISTRY: std::sync::LazyLock<RwLock<HashMap<ObjectId, Arc<ObjectRecord>>>> =
    std::sync::LazyLock::new(|| RwLock::new(HashMap::new()));

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
    let reg_thread = entry.registration_thread.read().map_err(|_| ReparentError::Busy)?;
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

fn borrow_registered(id: ObjectId) -> Option<ObjectBorrowGuard> {
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
        Some(mut guard) => change.apply(guard.object_data_mut()),
        None => {
            lock_unpoisoned(&entry.pending_child_changes).push(change);
            entry
                .has_pending_child_changes
                .store(true, Ordering::Release);
        }
    }
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
pub fn register_object_thread(id: ObjectId, thread_id: ThreadId) {
    let mut lock = write_unpoisoned(&GLOBAL_OBJECT_THREADS);
    if lock.is_none() {
        *lock = Some(HashMap::new());
    }
    if let Some(map) = lock.as_mut() {
        map.insert(id, thread_id);
    }
    if let Ok(reg) = GLOBAL_OBJECT_REGISTRY.read() {
        if let Some(entry) = reg.get(&id) {
            *write_unpoisoned(&entry.thread_id) = thread_id;
        }
    }
}

/// Unregisters an object's thread affinity from the global registry. It is part of the cleanup of
/// a destroyed object, which cannot report a failure, so a poisoned map is used as it is.
pub fn unregister_object_thread(id: ObjectId) {
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
/// receiver's thread. `Auto` queues when the caller is on another thread. `Direct`, and `Auto` on the
/// receiver's own thread, need a `&mut` borrow and return `Err(TargetBorrowFailed)`; use
/// `QObject::invoke_method_with_type` for those.
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
            return Err(crate::meta::InvokeError::TargetBorrowFailed)
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
    if let Ok(reg) = GLOBAL_OBJECT_REGISTRY.read() {
        if let Some(entry) = reg.get(&id) {
            *write_unpoisoned(&entry.registration_thread) = thread_id;
        }
    }
}

/// Walks `data` and its unregistered `owned_children`, checking that no owned child is borrowed by
/// a running callback and that every unregistered owned descendant allows moving to another
/// thread, while queuing registered children for registry-backed inspection.
fn inspect_owned_tree(
    data: &ObjectData,
    all_ids: &mut Vec<ObjectId>,
    queue: &mut Vec<ObjectId>,
) -> Result<(), super::thread::MoveError> {
    use super::thread::MoveError;
    if !all_ids.contains(&data.id()) {
        all_ids.push(data.id());
    }
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
            if !all_ids.contains(&owned_id) && !queue.contains(&owned_id) {
                queue.push(owned_id);
            }
        } else {
            if !owned.can_move_to_thread() {
                return Err(MoveError::NotMovable);
            }
            inspect_owned_tree(owned.object_data(), all_ids, queue)?;
        }
    }
    for &child_id in data.children() {
        if !all_ids.contains(&child_id) && !queue.contains(&child_id) {
            queue.push(child_id);
        }
    }
    for child_id in registered_children_of(data.id()) {
        if !all_ids.contains(&child_id) && !queue.contains(&child_id) {
            queue.push(child_id);
        }
    }
    Ok(())
}

/// Drains `queue`, requiring every live registered descendant to be unborrowed on the current
/// thread and to answer `true` to `QObject::can_move_to_thread`. An id that has no live record in
/// the registry (such as an unregistered non-owned child) is kept in `all_ids` without failing.
fn collect_queued_descendants(
    all_ids: &mut Vec<ObjectId>,
    queue: &mut Vec<ObjectId>,
) -> Result<(), super::thread::MoveError> {
    use super::thread::MoveError;
    while let Some(child_id) = queue.pop() {
        if all_ids.contains(&child_id) {
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
                if !obj.can_move_to_thread() {
                    return Err(MoveError::NotMovable);
                }
                inspect_owned_tree(obj.object_data(), all_ids, queue)
            })
            .ok_or(MoveError::Busy)??;
        } else {
            all_ids.push(child_id);
        }
    }
    Ok(())
}

/// Stage A of [`crate::object::move_to_thread_by_id`]: validates `id` and collects the full
/// registered subtree (`all_ids`, root first), releasing every borrow before returning.
///
/// Returns `Ok(None)` when `id` is already on `target_thread`. Refuses with
/// `MoveByIdError::NotMovable` when `id` or any descendant answers `false` to
/// `QObject::can_move_to_thread`. This stage sends nothing and changes nothing, so every refusal
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
        if !obj.can_move_to_thread() {
            return Err(MoveByIdError::NotMovable);
        }
        let mut all_ids = Vec::new();
        let mut queue = Vec::new();
        inspect_owned_tree(data, &mut all_ids, &mut queue)?;
        Ok(Some((all_ids, queue)))
    })
    .ok_or(MoveByIdError::Busy)??;

    let Some((mut all_ids, mut queue)) = initial else {
        return Ok(None);
    };
    collect_queued_descendants(&mut all_ids, &mut queue)?;
    Ok(Some(all_ids))
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

    let mut all_ids = Vec::new();
    let mut queue = Vec::new();
    inspect_owned_tree(object_data, &mut all_ids, &mut queue)?;
    collect_queued_descendants(&mut all_ids, &mut queue)?;
    Ok(Some(all_ids))
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
pub fn query_object_parents_snapshot(id1: ObjectId, id2: ObjectId) -> Option<(Option<ObjectId>, Option<ObjectId>)> {
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
    if let Ok(reg) = GLOBAL_OBJECT_REGISTRY.read() {
        if let Some(entry) = reg.get(&id) {
            return Some(*read_unpoisoned(&entry.thread_id));
        }
    }
    if let Some(map) = read_unpoisoned(&GLOBAL_OBJECT_THREADS).as_ref() {
        return map.get(&id).copied();
    }
    None
}

/// Registers `obj` so that the registry can call it by id.
///
/// Deprecated, because the registry keeps a pointer made from the reference `obj`. Under Rust's
/// aliasing rules, the next direct use of the object through the variable, field or `Box` that
/// `obj` borrowed from invalidates that pointer, so a callback after such a use is undefined
/// behavior, even if the program appears to work; Miri reports it. Nothing in this function
/// prevents that use, and nothing in the function can tell whether the caller makes it.
///
/// # Migrating
/// - An object in a variable: `register_qobject_ptr(NonNull::new_unchecked(&raw mut x))`. The
///   variable can be read directly between callbacks; write it before registering, or through the
///   pointer that was registered.
/// - An object in a `Box`: create it with [`register_owned_qobject`] and keep the returned
///   [`OwnedObject`] where the `Box` was. It dereferences the same way and may be moved.
///
/// The [`crate::object`] module documentation says which function fits where the object lives
/// and lists the uses that break the aliasing rules.
///
/// # Safety
/// The object must remain alive at the same address until `unregister_qobject` completes.
/// Do not move or drop it while registered. The thread rules of [`crate::object`] apply. The
/// caller must ensure that no overlapping mutable or mutable/shared aliases exist while a
/// callback runs, and that the owner makes no direct use of the object between callbacks (see
/// above).
#[deprecated(
    note = "the registry's pointer comes from a reference that the next direct use of the object \
            invalidates; use `register_qobject_ptr` with a pointer taken by `&raw mut`, or \
            `register_owned_qobject`; see the documentation of the `object` module"
)]
pub unsafe fn register_qobject(obj: &mut dyn QObject) {
    let ptr = obj as *mut dyn QObject;
    let data = unsafe { &*ptr }.object_data();
    register_object_metadata(data, ptr);
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
                    .map_or(true, |record| *read_unpoisoned(&record.parent) == Some(id))
        });
        // The parent's list is taken before anything is inserted, so that a poisoned list refuses
        // the adoption with nothing changed, as every other commit does. The parent is borrowed by
        // the caller, so it is registered. Its registry list is what still reaches the child while
        // a running callback of the parent hides it from the parent's own `children`. L0 (write)
        // before L1, as everywhere.
        let parent_record = adopted_by.and_then(|parent_id| registry.get(&parent_id).cloned());
        let mut parent_list = match &parent_record {
            Some(parent_record) => Some(parent_record.children.write().map_err(|_| ReparentError::Busy)?),
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
        if let Some(list) = parent_list.as_mut() {
            if !list.contains(&id) {
                list.push(id);
            }
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
    let ptr = unsafe { obj.as_mut().get_unchecked_mut() as *mut dyn QObject };
    let data = unsafe { (*ptr).object_data() };
    register_object_metadata(data, ptr);
}

/// Registers a boxed object and hands the `Box` back.
///
/// Deprecated, because Rust treats a `Box` as the only pointer to its object. Returning `(id,
/// boxed)` moves the `Box<T>`, which under Stacked Borrows (for `T: Sized + Unpin`) already
/// invalidates the pointer the registry took before the first callback even runs; under both
/// Stacked Borrows and Tree Borrows, any direct use of the `Box<T>` invalidates the registry's
/// pointer, and any callback through the registry invalidates the `Box<T>` so that even dropping
/// it afterwards is undefined behavior. No wrapper around this function can make that sound.
///
/// # Migrating
/// [`register_owned_qobject`] takes the value instead of a `Box` and returns an [`OwnedObject`],
/// whose pointer is the one the registry keeps. It dereferences like the `Box` did.
///
/// ```text
/// // before
/// let (id, mut node) = unsafe { register_boxed_qobject(Box::new(Node::new())) };
/// // after
/// let mut node = unsafe { register_owned_qobject(Node::new()) };
/// let id = OwnedObject::object_id(&node);
/// ```
///
/// # Safety
/// The returned Box must remain alive and unmoved until unregistered. The thread rules of
/// [`crate::object`] apply, and the owner must make no use of the object between callbacks, which
/// is what the `Box` is for.
#[deprecated(
    note = "using or moving the returned `Box` invalidates the registry's pointer; use \
            `register_owned_qobject`, which takes the value and returns an `OwnedObject` that \
            shares its pointer with the registry: `let (id, b) = register_boxed_qobject(Box::new(v))` \
            becomes `let b = register_owned_qobject(v); let id = OwnedObject::object_id(&b);`"
)]
pub unsafe fn register_boxed_qobject<T: QObject + 'static>(
    mut boxed: Box<T>,
) -> (ObjectId, Box<T>) {
    let id = boxed.object_data().id();
    let ptr = &mut *boxed as *mut dyn QObject;
    register_object_metadata(boxed.object_data(), ptr);
    (id, boxed)
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
    // SAFETY: the caller guarantees the object is alive and that no callback of it runs.
    let data = unsafe { (*raw).object_data() };
    register_object_metadata(data, raw);
}

/// Moves `obj` to the heap, registers it, and returns its owner. Dropping the owner drops the
/// object, which unregisters it.
///
/// The owner may use the object through `Deref` and `DerefMut` between callbacks, and may be
/// moved; the registry's pointer stays valid (see [`OwnedObject`]).
///
/// # Safety
/// While a callback of the object runs, the owner does not access the object, not even to read
/// it, and does not drop it. The one exception is an owner type whose drop calls
/// `destroy_when_released`: that unregisters the object at once and frees its memory only after
/// the running callback returns, so such an owner may be dropped from inside the callback. The
/// owner is used and dropped on the registration thread. The thread rules of [`crate::object`]
/// apply.
pub unsafe fn register_owned_qobject<T: QObject + 'static>(obj: T) -> OwnedObject<T> {
    let owned = OwnedObject::new(obj);
    let ptr: NonNull<dyn QObject> = OwnedObject::<T>::as_non_null(&owned);
    // SAFETY: the object stays at this address until the owner is dropped; the rest is the
    // caller's contract.
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
    let removed = write_unpoisoned(&GLOBAL_OBJECT_REGISTRY).remove(&id);
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
            #[cfg(test)]
            let _held_by_mutant = test_hooks::mutant("unregister_holds_link")
                .then(|| read_unpoisoned(&entry.parent));
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
                children.retain(|&child_id| child_id != id);
                break;
            }
        }
        for child_id in read_unpoisoned(&entry.children).iter() {
            if let Some(child) = registry.get(child_id) {
                *write_unpoisoned(&child.parent) = None;
            }
        }
    }
    unregister_object_thread(id);
    // Posted events addressed to a destroyed object are removed, never delivered (Qt
    // `~QObjectPrivate`, qobject.cpp:206-207).
    crate::event_loop::purge_posted_events_for_receiver(id);
    crate::signal::disconnect_all_for_object(id);
    crate::timer::stop_timers_for_object(id);
}
/// Unique object identifier: ObjectId (`QObject*` equivalent).

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjectId(pub u64);

impl ObjectId {
    /// Unique object identifier: ObjectId (`QObject*` equivalent).
    pub fn next() -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        ObjectId(NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
    }
}

/// Core object metadata: ObjectData (`QObjectData` equivalent).
/// Core object metadata: ObjectData (`QObjectData` equivalent).
/// Unique generational object identifier for stale-reference detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GenerationalId {
    pub id: ObjectId,
    pub generation: u32,
}

impl GenerationalId {
    pub fn new(id: ObjectId, generation: u32) -> Self {
        Self { id, generation }
    }
}

/// Core object metadata: ObjectData (`QObjectData` equivalent).
pub struct ObjectData {
    id: ObjectId,
    parent: Option<ObjectId>,
    children: Vec<ObjectId>,

    /// Thread affinity.
    thread_id: ThreadId,

    pub(crate) delete_later_called: AtomicBool,

    pub(crate) signals_blocked: Arc<AtomicBool>,

    /// Installed event filters chain.
    pub(crate) event_filters: EventFilterChain,

    /// Dynamic properties map (`QObject::dynamicPropertyNames`).
    pub(crate) dynamic_properties: HashMap<String, Variant>,

    /// Object name for hierarchy queries (`QObject::objectName`).
    pub(crate) object_name: Option<String>,

    /// Emitted when object name changes (`QObject::objectNameChanged`).
    pub(crate) object_name_changed: Signal<String>,

    /// Shared liveness token for guarded weak references (`QPointer`).
    pub(crate) liveness: Arc<AtomicBool>,

    /// Object lifecycle generation counter.
    pub(crate) generation: u32,

    /// Directly owned child objects for cascading destruction. Each owner shares its pointer with
    /// the registry (see [`OwnedObject`]), and knows the child's id without reading the child.
    owned_children: Vec<OwnedObject<dyn QObject>>,
    pub(crate) timers: Vec<crate::timer::TimerId>,

    /// True while this object is held in its parent's `owned_children`. Such an object's
    /// lifetime belongs to the parent, so `set_parent` (which only sees `&mut ObjectData`, a
    /// borrow *into* the owned object) must not detach it; [`reparent_owned_object`] moves its owner.
    owned_by_parent: bool,

    /// The parent's id and liveness token, captured when a registered parent was set. It lets
    /// [`ObjectData::parent`] see the parent die when the parent's drop cannot write to this
    /// object: a running callback borrows it, it was never registered, or it is registered on
    /// another thread. A token whose id differs from `parent` is stale and ignored, because
    /// `parent` is a private field managed through `set_parent`.
    parent_liveness: Option<(ObjectId, Arc<AtomicBool>)>,
    pub(crate) borrow_state: Arc<ObjectBorrowState>,
}

impl fmt::Debug for ObjectData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ObjectData")
            .field("id", &self.id)
            .field("object_name", &self.object_name)
            .field("parent", &self.parent)
            .field("children", &self.children)
            .field("thread_id", &self.thread_id)
            .field("delete_later_called", &self.delete_later_called)
            .field(
                "signals_blocked",
                &self.signals_blocked.load(Ordering::Relaxed),
            )
            .field("generation", &self.generation)
            .field("is_alive", &self.liveness.load(Ordering::Relaxed))
            .field("owned_children_count", &self.owned_children.len())
            .finish()
    }
}
impl Default for ObjectData {
    fn default() -> Self {
        Self::with_auto_id()
    }
}

impl ObjectData {
    /// Core object metadata: ObjectData (`QObjectData` equivalent).
    pub fn new(id: ObjectId) -> Self {
        let thread_id = ThreadId::current();
        register_object_thread(id, thread_id);
        let signals_blocked = Arc::new(AtomicBool::new(false));
        let liveness = Arc::new(AtomicBool::new(true));
        let object_name_changed =
            Signal::with_owner_parts(id, Arc::clone(&signals_blocked), Arc::clone(&liveness));
        Self {
            id,
            parent: None,
            children: Vec::new(),
            thread_id,
            delete_later_called: AtomicBool::new(false),
            signals_blocked,
            object_name_changed,
            event_filters: EventFilterChain::new(),
            dynamic_properties: HashMap::new(),
            object_name: None,
            liveness,
            generation: 1,
            owned_children: Vec::new(),
            timers: Vec::new(),
            owned_by_parent: false,
            parent_liveness: None,
            borrow_state: Arc::new(ObjectBorrowState::default()),
        }
    }
    /// Creates a new ObjectData with a unique auto-generated ObjectId.
    #[allow(clippy::new_without_default)]
    pub fn with_auto_id() -> Self {
        Self::new(ObjectId::next())
    }
    /// Returns the unique object identifier (`QObject*` equivalent).
    #[inline]
    pub fn id(&self) -> ObjectId {
        self.id
    }

    /// Returns the thread affinity of this object (`QObject::thread`).
    #[inline]
    pub fn thread_id(&self) -> ThreadId {
        self.thread_id
    }

    /// Returns a slice of directly owned child objects.
    #[inline]
    pub fn owned_children(&self) -> &[OwnedObject<dyn QObject>] {
        &self.owned_children
    }
    /// Emitted when object name changes (`QObject::objectNameChanged`).
    #[inline]
    pub fn object_name_changed(&self) -> &Signal<String> {
        &self.object_name_changed
    }

    /// Installed event filters chain.
    #[inline]
    pub fn event_filters(&self) -> &EventFilterChain {
        &self.event_filters
    }

    /// Active timer identifiers registered on this object.
    #[inline]
    pub fn timers(&self) -> &[crate::timer::TimerId] {
        &self.timers
    }
    /// True while signals of this object are blocked (`QObject::signalsBlocked`).
    #[inline]
    pub fn signals_blocked(&self) -> bool {
        self.signals_blocked.load(Ordering::Acquire)
    }

    /// Shared signal-blocker token.
    #[inline]
    pub fn signals_blocked_token(&self) -> &Arc<AtomicBool> {
        &self.signals_blocked
    }



    pub(crate) fn set_thread_id(&mut self, thread_id: ThreadId) {
        self.thread_id = thread_id;
    }

    pub(crate) fn owned_children_mut(&mut self) -> &mut Vec<OwnedObject<dyn QObject>> {
        &mut self.owned_children
    }


    /// Core object metadata: ObjectData (`QObjectData` equivalent).
    pub fn with_thread(id: ObjectId, thread_id: ThreadId) -> Self {
        register_object_thread(id, thread_id);
        let signals_blocked = Arc::new(AtomicBool::new(false));
        let liveness = Arc::new(AtomicBool::new(true));
        let object_name_changed =
            Signal::with_owner_parts(id, Arc::clone(&signals_blocked), Arc::clone(&liveness));
        Self {
            id,
            parent: None,
            children: Vec::new(),
            thread_id,
            delete_later_called: AtomicBool::new(false),
            signals_blocked,
            object_name_changed,
            event_filters: EventFilterChain::new(),
            dynamic_properties: HashMap::new(),
            object_name: None,
            liveness,
            generation: 1,
            owned_children: Vec::new(),
            timers: Vec::new(),
            owned_by_parent: false,
            parent_liveness: None,
            borrow_state: Arc::new(ObjectBorrowState::default()),
        }
    }

    /// Schedules deferred deletion of this object (`QObject::deleteLater`).
    pub fn delete_later(&self) {
        delete_later(self)
    }

    pub fn install_event_filter(&mut self, filter: ObjectId) {
        install_event_filter(self, filter);
    }

    pub fn remove_event_filter(&mut self, filter: ObjectId) {
        remove_event_filter(self, filter);
    }

    /// See [`set_parent`].
    pub fn set_parent(&mut self, new_parent: Option<ObjectId>) -> Result<(), ReparentError> {
        set_parent(self, new_parent)
    }

    /// True while a parent owns this object (see [`reparent_owned_object`]).
    pub fn is_owned_by_parent(&self) -> bool {
        self.owned_by_parent
    }

    pub(crate) fn add_child(&mut self, child_id: ObjectId) {
        if !self.children.contains(&child_id) {
            self.children.push(child_id);
        }
    }

    pub(crate) fn remove_child(&mut self, child_id: ObjectId) {
        self.children.retain(|&id| id != child_id);
    }

    /// Returns the parent object ID if any (`QObject::parent`).
    ///
    /// `None` once the parent has been dropped, as long as it was registered when it was set,
    /// even if this object could not be updated at that moment: the link this object keeps can
    /// still name the dead parent then, and this method does not report it.
    pub fn parent(&self) -> Option<ObjectId> {
        let id = self.parent?;
        match &self.parent_liveness {
            Some((token_id, alive)) if *token_id == id && !alive.load(Ordering::Acquire) => None,
            _ => Some(id),
        }
    }

    /// Forgets the parent, and the token that tracked it.
    fn clear_parent(&mut self) {
        self.parent = None;
        self.parent_liveness = None;
    }

    /// Returns a slice of direct children object IDs (`QObject::children`).
    ///
    /// A change to the children made while a callback of this object is running (for example
    /// `set_parent` called from its `event`) shows here once that callback has returned.
    pub fn children(&self) -> &[ObjectId] {
        &self.children
    }

    /// Returns the thread affinity of this object (`QObject::thread`).
    pub fn thread(&self) -> ThreadId {
        self.thread_id
    }
    /// Sets object name for reflection and hierarchy queries (`QObject::setObjectName`).
    pub fn set_object_name(&mut self, name: impl Into<String>) {
        let name = name.into();
        let prev = self.object_name.as_deref().unwrap_or("");
        if prev != name {
            self.object_name = Some(name.clone());
            self.object_name_changed.emit(&name);
        }
    }

    /// Returns object name (`QObject::objectName`).
    pub fn object_name(&self) -> Option<&str> {
        self.object_name.as_deref()
    }

    /// Shared liveness token.
    pub fn liveness(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.liveness)
    }

    /// Returns true if the object has not been dropped.
    pub fn is_alive(&self) -> bool {
        self.liveness.load(Ordering::Acquire)
    }

    /// Current generation index.
    pub fn generation(&self) -> u32 {
        self.generation
    }

    /// Generational identifier for guarded validation.
    pub fn generational_id(&self) -> GenerationalId {
        GenerationalId::new(self.id, self.generation)
    }

    /// Recursively cascades deletion to all children matching Qt `QObjectPrivate::deleteChildren`.
    ///
    /// An owned child whose callback is running is destroyed at once as far as anything else can
    /// tell (unregistered, no longer alive, no events, timers or connections to other objects),
    /// and so is everything below it; but its memory and its `Drop` wait until that callback
    /// returns: the callback still runs on `&mut self`. Such a child is not read, not even for its
    /// id, which comes from its owner.
    pub fn delete_children(&mut self) {
        let mut idle = Vec::with_capacity(self.owned_children.len());
        for mut child in std::mem::take(&mut self.owned_children) {
            if OwnedObject::is_borrowed(&child)
                || registered_and_borrowed(OwnedObject::object_id(&child))
            {
                destroy_when_released(child);
            } else {
                child.object_data_mut().clear_parent();
                idle.push(child);
            }
        }
        for child_id in std::mem::take(&mut self.children) {
            release_child(child_id, self.id);
        }
        // The registry's list is complete at once, while this object's own list lags until a
        // running callback of this object returns. Whatever is still registered here goes too.
        for child_id in registered_children_of(self.id) {
            release_child(child_id, self.id);
        }
        drop(idle);
    }

    /// Unregisters this object from dispatch and global metadata.
    ///
    /// # Safety
    /// Follows [`unregister_qobject`] requirements; no callback may be active for this object.
    pub unsafe fn unregister(&mut self) {
        unsafe { unregister_qobject(self.id) };
    }

    /// Sets a dynamic property value (`QObject::setProperty` dynamic fallback).
    ///
    /// If `value` is `Variant::Invalid`:
    /// - If the property exists, it is removed and `true` (changed) is returned.
    /// - If the property does not exist, `false` (unchanged) is returned.
    ///
    /// If `value` is valid:
    /// - If the property already exists with the same value, `false` (unchanged) is returned.
    /// - Otherwise (new property or changed value), it is stored and `true` (changed) is returned.
    pub fn set_property(&mut self, name: &str, value: Variant) -> bool {
        if !value.is_valid() {
            return self.dynamic_properties.remove(name).is_some();
        }
        match self.dynamic_properties.get(name) {
            Some(existing) if existing == &value => false,
            _ => {
                self.dynamic_properties.insert(name.to_string(), value);
                true
            }
        }
    }

    /// Retrieves a dynamic property by name.
    pub fn property(&self, name: &str) -> Option<&Variant> {
        self.dynamic_properties.get(name)
    }

    /// Returns all registered dynamic property names.
    pub fn dynamic_property_names(&self) -> Vec<String> {
        self.dynamic_properties.keys().cloned().collect()
    }

    /// Swaps only property and name contents with another ObjectData instance.
    /// Strictly does NOT swap hierarchy (parent/children), ownership (owned_children/owned_by_parent),
    /// identity (id), liveness, thread_id, borrow_state, event filters, signals_blocked, timers,
    /// or delete_later_called.
    pub fn swap_properties(&mut self, other: &mut Self) {
        std::mem::swap(&mut self.object_name, &mut other.object_name);
        std::mem::swap(&mut self.dynamic_properties, &mut other.dynamic_properties);
    }

    /// Pure property swap alias. Hierarchy and ownership must be transferred via
    /// transactional operations that update child parent pointers and registry state.
    #[inline]
    pub fn swap_content(&mut self, other: &mut Self) {
        self.swap_properties(other);
    }
}

/// Scoped mutable proxy for ObjectData.
///
/// Encapsulates mutable access to an object's internal metadata without giving out a raw
/// `&mut ObjectData`, preventing arbitrary `std::mem::swap` from breaking identity,
/// registry and liveness invariants.
pub struct ObjectDataMut<'a> {
    pub(crate) data: &'a mut ObjectData,
}

impl<'a> ObjectDataMut<'a> {
    #[inline]
    pub fn new(data: &'a mut ObjectData) -> Self {
        Self { data }
    }

    /// Swaps only property and name contents with another ObjectDataMut proxy.
    #[inline]
    pub fn swap_properties(&mut self, other: &mut ObjectDataMut<'_>) {
        self.data.swap_properties(other.data);
    }

    /// Pure property swap alias.
    #[inline]
    pub fn swap_content(&mut self, other: &mut ObjectDataMut<'_>) {
        self.data.swap_content(other.data);
    }

    /// Transactionally swaps children hierarchy and ownership between two objects,
    /// synchronizing both owned and non-owned transferred children's local parent ID,
    /// parent liveness token, and the global object registry.
    ///
    /// # Locking
    /// - Before any lock: the registry records of both objects and of every child to move are
    ///   looked up, an [`ObjectBorrowGuard`] is taken for every child (a failed borrow returns
    ///   [`ReparentError::Busy`] at once, nothing waits for one), and the handles to the data the
    ///   commit writes are obtained from the objects' accessors (their own code, which must not run
    ///   under a lock).
    /// - `HIERARCHY_MUTATION`, then Level 0 (the registry, read). The cycle checks run against that
    ///   registry, under both, so nothing that adds a parent link can slip in before the commit.
    ///   The records looked up earlier must still be the registered ones.
    /// - Level 1: both objects' `children` write locks in ascending `ObjectId` order. The registry's
    ///   lists are compared with the objects' own lists only now, under these write locks, so a list
    ///   that changed since the lookup is refused instead of overwritten.
    /// - Level 2: the `parent` write locks of the children in ascending `ObjectId` order, each
    ///   checked to name the object it is being moved away from.
    /// - On every exit, the locks go first (Level 2, Level 1, Level 0), then `HIERARCHY_MUTATION`,
    ///   and the borrow guards last: dropping one takes the `pending_child_changes` mutex and can
    ///   free parked children, which takes the registry for writing.
    ///
    /// # Atomicity Boundary
    /// During the commit phase, all write locks and borrow guards are held simultaneously, guaranteeing
    /// that intermediate states cannot be observed via synchronized cross-object snapshot queries
    /// (such as [`query_object_parents_snapshot`]). Note that independent single-record queries
    /// (such as [`query_object_parent`]) are protected individually by single locks, but do not provide
    /// cross-record snapshot atomicity.
    ///
    /// An `Err` leaves everything as it was, and no event is sent. `InvalidParent`: one object lists
    /// the other (that is answered first), an owned child is missing from its parent's list, a
    /// listed child is not registered, a ring would form, or the lists and links the objects keep
    /// disagree with the registry's. `DifferentThread`: the thread affinities of the two objects, or
    /// of a child and the object it moves to, do not fit, or a child is registered on another
    /// thread. `UnknownObject`: either object is not registered or is dead, or a child is dead.
    /// `Busy`: a child is borrowed by a running callback, or a lock is poisoned.
    pub fn swap_hierarchy_and_ownership(&mut self, other: &mut ObjectDataMut<'_>) -> Result<(), ReparentError> {
        let self_id = self.data.id;
        let other_id = other.data.id;
        if self_id == other_id {
            return Ok(());
        }

        // A parent that lists the other object, or is listed by it, cannot be swapped: the other
        // one would end up its own child. It is answered here, also because the data handles taken
        // below must not reach the object of the other argument, which the caller borrows.
        if self.data.children.contains(&other_id) || other.data.children.contains(&self_id) {
            return Err(ReparentError::InvalidParent);
        }

        // 1. Thread affinity validation: all children must match destination parent's thread.
        let self_thread = self.data.thread_id;
        let other_thread = other.data.thread_id;
        if self_thread != other_thread {
            for child in &self.data.owned_children {
                if child.object_data().thread() != other_thread {
                    return Err(ReparentError::DifferentThread);
                }
            }
            for &child_id in &self.data.children {
                if in_different_thread(other_thread, child_id) {
                    return Err(ReparentError::DifferentThread);
                }
            }
            for child in &other.data.owned_children {
                if child.object_data().thread() != self_thread {
                    return Err(ReparentError::DifferentThread);
                }
            }
            for &child_id in &other.data.children {
                if in_different_thread(self_thread, child_id) {
                    return Err(ReparentError::DifferentThread);
                }
            }
        }

        // 2. Owned children, by the ids their owners kept: each must be in its parent's list.
        let self_owned_set: std::collections::HashSet<ObjectId> =
            self.data.owned_children.iter().map(OwnedObject::object_id).collect();
        let other_owned_set: std::collections::HashSet<ObjectId> =
            other.data.owned_children.iter().map(OwnedObject::object_id).collect();
        for &owned_id in &self_owned_set {
            if !self.data.children.contains(&owned_id) {
                return Err(ReparentError::InvalidParent);
            }
        }
        for &owned_id in &other_owned_set {
            if !other.data.children.contains(&owned_id) {
                return Err(ReparentError::InvalidParent);
            }
        }

        #[derive(Clone, Copy, PartialEq, Eq)]
        enum TransferTarget {
            ToOther,
            ToSelf,
        }

        struct ChildTransfer {
            child_id: ObjectId,
            target: TransferTarget,
            record: Arc<ObjectRecord>,
            is_owned: bool,
        }

        // 3. The records involved. The registry lock is let go again; what is found here is
        // checked to be still registered once the locks below are held.
        let (self_record, other_record, mut transfers) = {
            let registry = GLOBAL_OBJECT_REGISTRY.read().map_err(|_| ReparentError::Busy)?;
            let self_record = Arc::clone(registry.get(&self_id).ok_or(ReparentError::UnknownObject)?);
            let other_record = Arc::clone(registry.get(&other_id).ok_or(ReparentError::UnknownObject)?);
            if !self_record.liveness.load(Ordering::Acquire)
                || !other_record.liveness.load(Ordering::Acquire)
            {
                return Err(ReparentError::UnknownObject);
            }
            let mut transfers: Vec<ChildTransfer> =
                Vec::with_capacity(self.data.children.len() + other.data.children.len());
            for (children, target, owned_set) in [
                (&self.data.children, TransferTarget::ToOther, &self_owned_set),
                (&other.data.children, TransferTarget::ToSelf, &other_owned_set),
            ] {
                for &child_id in children {
                    // A child that the object lists but the registry does not hold: the two disagree.
                    let record = registry.get(&child_id).ok_or(ReparentError::InvalidParent)?;
                    if !record.liveness.load(Ordering::Acquire) {
                        return Err(ReparentError::UnknownObject);
                    }
                    transfers.push(ChildTransfer {
                        child_id,
                        target,
                        record: Arc::clone(record),
                        is_owned: owned_set.contains(&child_id),
                    });
                }
            }
            (self_record, other_record, transfers)
        };

        // Sort all child transfers by child_id for a lock acquisition order that is the same for everyone.
        transfers.sort_by_key(|t| t.child_id);
        if transfers.windows(2).any(|w| w[0].child_id == w[1].child_id) {
            return Err(ReparentError::InvalidParent);
        }

        // 4. Borrows and data handles, before any lock. The borrow guards are declared before the
        // registry guard and the locks below, so they are dropped after all of them, on every exit.
        let mut borrow_guards: Vec<ObjectBorrowGuard> = Vec::with_capacity(transfers.len());
        for t in &transfers {
            borrow_guards.push(borrow_record(&t.record)?);
        }
        // A non-owned child is written through its borrow guard; an owned child through its owner,
        // as the parent holds it. Each object gets one handle, so no two `&mut` reach it.
        let mut child_datas: Vec<Option<ObjectDataMut<'_>>> = borrow_guards
            .iter_mut()
            .zip(&transfers)
            .map(|(guard, t)| (!t.is_owned).then(|| guard.object_data_mut()))
            .collect();
        let mut self_owned_datas: Vec<ObjectDataMut<'_>> = self
            .data
            .owned_children
            .iter_mut()
            .map(|child| child.object_data_mut())
            .collect();
        let mut other_owned_datas: Vec<ObjectDataMut<'_>> = other
            .data
            .owned_children
            .iter_mut()
            .map(|child| child.object_data_mut())
            .collect();

        // 5. From here on no accessor and no handler runs.
        let _hierarchy = lock_hierarchy();
        let registry = GLOBAL_OBJECT_REGISTRY.read().map_err(|_| ReparentError::Busy)?;
        let registered = |id: ObjectId, record: &Arc<ObjectRecord>| {
            registry.get(&id).is_some_and(|current| Arc::ptr_eq(current, record))
        };
        if !registered(self_id, &self_record) || !registered(other_id, &other_record) {
            return Err(ReparentError::UnknownObject);
        }
        if transfers.iter().any(|t| !registered(t.child_id, &t.record)) {
            return Err(ReparentError::InvalidParent);
        }

        // Cycle check: neither object can be a descendant of the other, and no child being
        // transferred can create a cycle with its target parent.
        if would_create_cycle_in(&registry, self_id, other_id)?
            || would_create_cycle_in(&registry, other_id, self_id)?
        {
            return Err(ReparentError::InvalidParent);
        }
        for &child_id in &self.data.children {
            if would_create_cycle_in(&registry, child_id, other_id)? {
                return Err(ReparentError::InvalidParent);
            }
        }
        for &child_id in &other.data.children {
            if would_create_cycle_in(&registry, child_id, self_id)? {
                return Err(ReparentError::InvalidParent);
            }
        }

        #[cfg(test)]
        let validate_under_the_locks = !test_hooks::mutant("swap_prevalidates_children");
        #[cfg(not(test))]
        let validate_under_the_locks = true;
        #[cfg(test)]
        if !validate_under_the_locks {
            // The defect this function once had: the lists are compared under read locks taken in
            // argument order and let go before the write locks are taken.
            let first = self_record.children.read().map_err(|_| ReparentError::Busy)?;
            test_hooks::run(test_hooks::Point::BetweenChildrenReads);
            let second = other_record.children.read().map_err(|_| ReparentError::Busy)?;
            if *first != self.data.children || *second != other.data.children {
                return Err(ReparentError::InvalidParent);
            }
        }
        #[cfg(test)]
        test_hooks::run(test_hooks::Point::AfterCycleCheck);

        // Tier 1: the parents' `children` write locks in ascending ObjectId order.
        let (mut self_children_guard, mut other_children_guard) = if self_id < other_id {
            let s = self_record.children.write().map_err(|_| ReparentError::Busy)?;
            let o = other_record.children.write().map_err(|_| ReparentError::Busy)?;
            (s, o)
        } else {
            let o = other_record.children.write().map_err(|_| ReparentError::Busy)?;
            let s = self_record.children.write().map_err(|_| ReparentError::Busy)?;
            (s, o)
        };
        // Only a writer of these lists could change them, and none can now.
        if validate_under_the_locks
            && (*self_children_guard != self.data.children
                || *other_children_guard != other.data.children)
        {
            return Err(ReparentError::InvalidParent);
        }

        // Tier 2: the children's `parent` write locks in ascending child_id order.
        let mut child_parent_guards: Vec<std::sync::RwLockWriteGuard<'_, Option<ObjectId>>> =
            Vec::with_capacity(transfers.len());
        for t in &transfers {
            let guard = t.record.parent.write().map_err(|_| ReparentError::Busy)?;
            let named = match t.target {
                TransferTarget::ToOther => self_id,
                TransferTarget::ToSelf => other_id,
            };
            if *guard != Some(named) {
                return Err(ReparentError::InvalidParent);
            }
            child_parent_guards.push(guard);
        }

        let self_liveness = Arc::clone(&self_record.liveness);
        let other_liveness = Arc::clone(&other_record.liveness);

        // 6. Infallible Atomic Commit:
        // All checks succeeded, all borrow guards and write locks are held.
        // No step from here can fail, and none runs code of an object.

        // Step A: Update non-owned children's local ObjectData.
        for (t, data) in transfers.iter().zip(child_datas.iter_mut()) {
            if let Some(data) = data {
                let raw = data.raw_data_mut();
                match t.target {
                    TransferTarget::ToOther => {
                        raw.parent = Some(other_id);
                        raw.parent_liveness = Some((other_id, Arc::clone(&other_liveness)));
                    }
                    TransferTarget::ToSelf => {
                        raw.parent = Some(self_id);
                        raw.parent_liveness = Some((self_id, Arc::clone(&self_liveness)));
                    }
                }
            }
        }

        // Step B: Update owned children's local ObjectData via their respective parents' owned_children.
        for data in &mut self_owned_datas {
            let raw = data.raw_data_mut();
            raw.parent = Some(other_id);
            raw.parent_liveness = Some((other_id, Arc::clone(&other_liveness)));
        }
        for data in &mut other_owned_datas {
            let raw = data.raw_data_mut();
            raw.parent = Some(self_id);
            raw.parent_liveness = Some((self_id, Arc::clone(&self_liveness)));
        }

        // Step C: Update Registry `record.parent` for all children under the held write locks.
        for (t, guard) in transfers.iter().zip(child_parent_guards.iter_mut()) {
            **guard = Some(match t.target {
                TransferTarget::ToOther => other_id,
                TransferTarget::ToSelf => self_id,
            });
        }

        // Step D: Update Registry `record.children` for both parents under the held write locks.
        *self_children_guard = other.data.children.clone();
        *other_children_guard = self.data.children.clone();

        // Step E: Atomic local vector swap.
        std::mem::swap(&mut self.data.children, &mut other.data.children);
        std::mem::swap(&mut self.data.owned_children, &mut other.data.owned_children);

        // Commit complete. When returning Ok(()), all guards drop cleanly and publish the changes atomically.
        Ok(())
    }

    #[inline]
    pub fn install_event_filter(&mut self, filter: ObjectId) {
        self.data.install_event_filter(filter);
    }

    #[inline]
    pub fn remove_event_filter(&mut self, filter: ObjectId) {
        self.data.remove_event_filter(filter);
    }

    #[inline]
    pub fn set_object_name(&mut self, name: &str) {
        self.data.set_object_name(name);
    }

    #[inline]
    pub fn set_property(&mut self, name: &str, value: Variant) -> bool {
        self.data.set_property(name, value)
    }

    #[inline]
    pub(crate) fn set_thread_id(&mut self, thread_id: ThreadId) {
        self.data.set_thread_id(thread_id);
    }

    #[inline]
    pub(crate) fn clear_parent(&mut self) {
        self.data.clear_parent();
    }

    #[inline]
    pub(crate) fn add_child(&mut self, child: ObjectId) {
        self.data.add_child(child);
    }

    #[inline]
    pub(crate) fn remove_child(&mut self, child: ObjectId) {
        self.data.remove_child(child);
    }

    #[inline]
    pub fn set_parent(&mut self, parent: Option<ObjectId>) -> Result<(), ReparentError> {
        self.data.set_parent(parent)
    }

    #[inline]
    pub(crate) fn add_timer(&mut self, id: crate::timer::TimerId) {
        self.data.timers.push(id);
    }

    #[inline]
    pub(crate) fn remove_timer(&mut self, id: crate::timer::TimerId) {
        self.data.timers.retain(|&t| t != id);
    }

    #[inline]
    pub(crate) fn raw_data_mut(&mut self) -> &mut ObjectData {
        self.data
    }

    #[inline]
    pub(crate) fn owned_children_mut(&mut self) -> &mut Vec<OwnedObject<dyn QObject>> {
        &mut self.data.owned_children
    }

    #[inline]
    pub(crate) fn into_owned_children_mut(self) -> &'a mut Vec<OwnedObject<dyn QObject>> {
        &mut self.data.owned_children
    }

    #[inline]
    #[allow(dead_code)]
    pub(crate) fn timers_mut(&mut self) -> &mut Vec<crate::timer::TimerId> {
        &mut self.data.timers
    }

    #[inline]
    pub(crate) fn children_mut(&mut self) -> &mut Vec<ObjectId> {
        &mut self.data.children
    }
}

impl<'a> std::ops::Deref for ObjectDataMut<'a> {
    type Target = ObjectData;
    #[inline]
    fn deref(&self) -> &Self::Target {
        self.data
    }
}

impl Drop for ObjectData {
    fn drop(&mut self) {
        // The object is gone from this first step on: the token goes false before the parent is
        // told or a child is deleted, so a signal of this object that is emitted by then reaches
        // no slot. Qt `~QObject` disconnects every receiver before `deleteChildren`
        // (qobject.cpp:1086-1178), and `QPointer` is cleared before anything else is destroyed.
        self.liveness.store(false, Ordering::Release);
        self.borrow_state.memory_destroyed.store(true, Ordering::Release);
        // 1. If we have a parent, unlink from parent and notify it
        if let Some(parent_id) = self.parent() {
            if registered_and_borrowed(parent_id) {
                // The parent's borrow is held, so its child lists cannot be edited now. The
                // removal is queued and the event is posted, not dropped.
                edit_child_list(parent_id, ChildListChange::Remove(self.id));
                crate::event_loop::post_event_to_thread(
                    ThreadId::current(),
                    parent_id,
                    Event::new(EventKind::ChildRemoved { child_id: self.id }),
                );
            } else {
                with_object_mut(parent_id, |parent_obj| {
                    let mut pdata = parent_obj.object_data_mut();
                    pdata.children_mut().retain(|&id| id != self.id);
                    pdata.owned_children_mut().retain(|c| OwnedObject::object_id(c) != self.id);
                    let mut ev = Event::new(EventKind::ChildRemoved { child_id: self.id });
                    parent_obj.event(&mut ev);
                });
            }
        }

        // 2. Cascade deletion to all children (deleteChildren)
        self.delete_children();

        // 3. Advance the generation for QPointer / GenerationalId safety
        self.generation = self.generation.wrapping_add(1);

        // SAFETY: an object registered through the unsafe registration API must be
        // unregistered only after dispatch callbacks have completed; Drop follows that contract.
        unsafe { unregister_qobject(self.id) };
    }
}

/// Marks an object for deferred deletion (`QObject::deleteLater`, qobject.cpp:2511-2565).
///
/// Debounced per object: the first request posts one `DeferredDelete` and later requests are
/// ignored (qobject.cpp:2531-2534). A request from the owning thread is stamped with that
/// thread's loop and handler-scope levels; scope 0 is promoted to 1 while a loop runs
/// (qobject.cpp:2539-2559). A request from another thread is stamped (0, 0).
pub fn delete_later(obj: &ObjectData) {
    let Some(owner) = query_object_thread(obj.id) else {
        return;
    };
    if obj.delete_later_called.swap(true, Ordering::SeqCst) {
        return;
    }
    let (loop_level, scope_level) = if owner == ThreadContext::current_id() {
        let loop_level = ThreadContext::current_loop_level();
        let scope_level = ThreadContext::current_scope_level();
        if scope_level == 0 && loop_level != 0 {
            (loop_level, 1)
        } else {
            (loop_level, scope_level)
        }
    } else {
        (0, 0)
    };
    crate::event_loop::post_event_to_thread(
        owner,
        obj.id,
        Event::new(EventKind::DeferredDelete {
            loop_level,
            scope_level,
        }),
    );
}

/// Event filter callback (`QObject::eventFilter`, `qobject.cpp:2407-2423`).
pub fn install_event_filter(target: &mut ObjectData, filter: ObjectId) -> bool {
    // Cross-thread check: filter and target must be in the same thread (qobject.cpp:2412-2415)
    if let Some(filter_thread) = query_object_thread(filter) {
        if filter_thread != target.thread_id {
            return false;
        }
    }

    target.event_filters.install(filter);
    true
}

pub fn remove_event_filter(target: &mut ObjectData, filter: ObjectId) {
    target.event_filters.remove(filter);
}

/// Why a parent change was refused. Nothing is changed when an error is returned, except for
/// [`ReparentError::DifferentThread`] from [`set_parent`] and [`set_parent_by_id`], where
/// the child is detached from its old parent (matching Qt `setParent(nullptr)` detach semantics).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReparentError {
    /// [`set_parent`]: the object is held by its parent's `owned_children`. Detaching it there
    /// would destroy it (or leave the caller's `&mut` dangling); use [`reparent_owned_object`].
    OwnedByParent,
    /// [`reparent_owned_object`]: the object has no parent that owns it, or that parent is not
    /// registered on the calling thread.
    NotOwned,
    /// The object is not registered, is dead, or is registered on another thread. For
    /// [`add_owned_child`] the object is the new parent; for
    /// [`ObjectDataMut::swap_hierarchy_and_ownership`] it is either of the two objects, or one of
    /// the children that move (dead).
    UnknownObject,
    /// [`set_parent`]: the new parent is the object itself or one of its descendants, or the
    /// child is registered and the new parent is not (two unregistered objects can be linked: only
    /// the child's own link is recorded). A new parent of another thread affinity gets
    /// `DifferentThread` instead of the second case. [`reparent_owned_object`]: the new parent is the child itself, or is
    /// not registered on the calling thread, or (checked last) is a descendant of the child.
    /// [`add_owned_child`]: the child has a parent already, or the parent is the child itself or
    /// one of its descendants. [`ObjectDataMut::swap_hierarchy_and_ownership`]: one object is
    /// below the other, the lists the objects keep disagree with the registry's, or a listed child
    /// is not registered.
    InvalidParent,
    /// The object or a parent is borrowed by an active callback, or a lock that guards a link or
    /// a list of the hierarchy is poisoned. Nothing was changed.
    Busy,
    /// The new parent belongs to a different thread than the child: their thread affinities
    /// (`ObjectData::thread_id`) differ, and a parent and its children share one thread. What is
    /// left behind depends on the call:
    /// - [`set_parent`] and [`set_parent_by_id`] do what Qt's `setParent` does. The child is removed
    ///   from its old parent in both the registry and local data, the old parent receives `ChildRemoved`,
    ///   the child is left without a parent (`parent() == None`), and Qt's warning goes to stderr.
    ///   This detach behavior is an explicit, documented exception to the general "zero state mutation
    ///   on failure" rule of other reparent errors.
    /// - [`reparent_owned_object`] changes nothing and prints nothing: the child stays with its old
    ///   parent, which still owns it. Qt would detach the child.
    /// - [`add_owned_child`] attaches nothing and prints nothing, and hands the `Box` back: the
    ///   child has no parent and the parent's children are as they were. Qt's constructor leaves
    ///   the new object without a parent in the same way, but warns.
    DifferentThread,
}

/// Lets go of a child that its parent does not own: it survives the parent, loses its
/// parent link, and is unregistered. A listed object whose own link names another parent, or none,
/// is not a child of `parent_id` (the safe `reparent` edits the objects' own links and lists
/// separately, so a list can name an object that was never linked to this parent) and is left
/// alone.
fn release_child(child_id: ObjectId, parent_id: ObjectId) {
    // The link is cleared before the unregister below, after which the child can no longer be
    // reached. A child that cannot be borrowed here (a running callback holds it, or it was never
    // registered) is not written to; its `parent()` sees the parent's liveness token instead, and
    // it is let go.
    let belongs_elsewhere = with_object_mut(child_id, |child| {
        let mut data = child.object_data_mut();
        if data.raw_data_mut().parent == Some(parent_id) {
            data.clear_parent();
            false
        } else {
            true
        }
    });
    if belongs_elsewhere == Some(true) {
        return;
    }
    if belongs_elsewhere.is_none() {
        if let Some((child_record, _)) = registered_ptr(child_id) {
            *lock_unpoisoned(&child_record.pending_parent_sync) = Some((None, None));
            child_record.has_pending_parent_sync.store(true, Ordering::Release);
        }
    }
    // whoever owns it frees it, and the callback's guard keeps its own `Arc` to the record.
    unsafe { unregister_qobject(child_id) };
}

/// The children the registry lists for `id` that are still registered and that the registry says
/// are `id`'s: a listed child whose own record names another parent (or none) is a stale entry
/// (the safe `reparent` edits the objects' own links only, so lists can name objects that were
/// never linked in the registry) and is not a child to destroy or move.
fn registered_children_of(id: ObjectId) -> Vec<ObjectId> {
    // Reached from the cleanup of a destroyed object, which cannot report a failure and must not
    // skip its children: a poisoned lock is read as it is (see `read_unpoisoned`).
    let registry = read_unpoisoned(&GLOBAL_OBJECT_REGISTRY);
    let Some(record) = registry.get(&id) else {
        return Vec::new();
    };
    let children = read_unpoisoned(&record.children);
    children
        .iter()
        .copied()
        .filter(|child_id| {
            registry
                .get(child_id)
                .is_some_and(|child| *read_unpoisoned(&child.parent) == Some(id))
        })
        .collect()
}

/// Whether `parent` belongs to a different thread than `child_thread`. An object whose thread is
/// unknown (not built through `ObjectData`, or already unregistered) is not judged to differ.
fn in_different_thread(child_thread: ThreadId, parent: ObjectId) -> bool {
    query_object_thread(parent).is_some_and(|thread| thread != child_thread)
}

/// The liveness token of a registered object, or `None` if `id` is not registered.
fn liveness_of(id: ObjectId) -> Option<Arc<AtomicBool>> {
    let registry = GLOBAL_OBJECT_REGISTRY.read().ok()?;
    registry.get(&id).map(|record| Arc::clone(&record.liveness))
}

/// The record the registry holds for `id`. The registry read lock is taken and let go inside, so
/// the caller must not hold it.
fn record_of(id: ObjectId) -> Result<Option<Arc<ObjectRecord>>, ReparentError> {
    let registry = GLOBAL_OBJECT_REGISTRY.read().map_err(|_| ReparentError::Busy)?;
    Ok(registry.get(&id).cloned())
}

/// Whether making `new_parent` the parent of `child_id` would make an object its own ancestor:
/// `new_parent` is the child itself or one of its descendants, as the registry records them.
///
/// The caller holds `HIERARCHY_MUTATION` and a lock on the registry, and passes the registry it
/// holds: a second read lock taken by the same thread waits for ever when a writer has queued
/// between the two. While `HIERARCHY_MUTATION` is held no other operation adds a parent link, so
/// the chain walked here does not grow before the caller commits; links removed meanwhile only
/// shorten it.
///
/// A walk that does not end within one step per registered object means the registry already
/// holds a cycle; the answer is then `true`, so that nothing is linked into it.
fn would_create_cycle_in(
    registry: &HashMap<ObjectId, Arc<ObjectRecord>>,
    child_id: ObjectId,
    new_parent: ObjectId,
) -> Result<bool, ReparentError> {
    let mut cursor = Some(new_parent);
    for _ in 0..=registry.len() {
        let Some(id) = cursor else { return Ok(false) };
        if id == child_id {
            return Ok(true);
        }
        let Some(record) = registry.get(&id) else {
            return Ok(false);
        };
        let parent_guard = record.parent.read().map_err(|_| ReparentError::Busy)?;
        cursor = *parent_guard;
    }
    Ok(true)
}

/// `start` and the ids above it, as the registry records them. The chain ends at an id that is
/// not registered or that was already seen. A poisoned link is read as it is.
fn ancestors_in(
    registry: &HashMap<ObjectId, Arc<ObjectRecord>>,
    start: Option<ObjectId>,
) -> Vec<ObjectId> {
    let mut chain = Vec::new();
    let mut cursor = start;
    while let Some(id) = cursor {
        if chain.contains(&id) {
            break;
        }
        chain.push(id);
        cursor = registry
            .get(&id)
            .and_then(|record| *read_unpoisoned(&record.parent));
    }
    chain
}

/// Sets a non-owned child's parent and updates registered relationship metadata, sending
/// `ChildRemoved`/`ChildAdded` to the registered parents.
///
/// Refuses with [`ReparentError::OwnedByParent`] when a parent owns the child; use
/// [`reparent_owned_object`] for those. Qt's `setParent_helper` (qobject.cpp:2287-2345) never destroys
/// the child; here the borrow `child` points into the object the parent owns, so ownership has to
/// move through an id-based call.
///
/// Refuses with [`ReparentError::InvalidParent`], changing nothing, when `new_parent` is the
/// child itself or one of its descendants. Qt's release build accepts that and builds a cycle,
/// which Qt documents as undefined behavior.
///
/// Thread affinity: the child's thread and the new parent's thread (`ObjectData::thread_id`, as
/// [`move_to_thread`](crate::object::move_to_thread) changes it) must be the same, because a parent and its children share one
/// thread. Otherwise, as in Qt (qobject.cpp:2309-2345; line numbers differ between Qt versions,
/// these were checked against Qt 6.11.2's `qobject.cpp`), the child is removed from its old parent,
/// which receives `ChildRemoved`, and is left without a parent; a warning goes to stderr, and this
/// returns [`ReparentError::DifferentThread`]. Only the two affinities are compared. The calling
/// thread is not checked, but a registered child must still be used on the thread it is registered
/// on (see the thread rules in [`crate::object`]). [`reparent_owned_object`] and [`add_owned_child`] refuse the same
/// pairs without detaching anything and without the warning; see [`ReparentError::DifferentThread`].
///
/// The parent does not own the child: whoever does keeps the child alive and frees it.
/// Dropping the parent does not destroy such a child (Qt deletes it). The parent unregisters its
/// children (see [`unregister_qobject`] for what that ends), including a child attached while a
/// running callback borrowed the parent. [`ObjectData::parent`] of the child returns `None` once
/// the parent has been dropped or unregistered, provided the parent was registered when this was
/// called. That holds for a child that a running callback borrows, or that was never registered,
/// too.
///
/// Limits of that guarantee:
/// - In the cases just named, where the parent's drop cannot write to the child, the child's own
///   link can keep the old id. Read the parent through `parent()`, which ignores a link whose
///   parent is dead.
/// - While a running callback holds the parent's borrow, the parent's own `children()` still
///   shows the list as it was; the change is applied when that callback returns. The registry
///   has it at once, so dropping the parent reaches the child either way.
///
/// `new_parent` need not be registered when the child is not either: the id is accepted and only
/// the child's own link is recorded, and the parent's child list, `ChildAdded` and the death
/// tracking above, which need a registered parent, do not happen. A registered child is refused
/// with [`ReparentError::InvalidParent`], changing nothing: `set_parent` does not link a
/// registered object to an id the registry does not hold. (An object that was linked to an
/// unregistered parent while it was unregistered keeps that link when it is registered; the
/// registry then has it on the object's record only, and not in the parent's list.) A new parent
/// of another thread affinity gets [`ReparentError::DifferentThread`] instead, registered or not.
/// Qt has no such case (a `QObject*` is always a live object), so nothing more is specified for
/// it. [`reparent_owned_object`] and [`add_owned_child`] refuse an unregistered parent.
///
/// # Events and what they see
/// The registry is changed completely before the first event. From `ChildRemoved` on,
/// [`query_object_parent`], [`query_object_children`] and the registry's lists show the finished
/// change. What the objects keep themselves follows Qt's order (`setParent_helper` removes the
/// child from the old parent's list, sends `ChildRemoved`, and only then changes the child's
/// parent and the new parent's list): the old parent's own `children()` has lost the child when it
/// gets `ChildRemoved`; the child's own `parent()` and the new parent's own `children()` change
/// after that handler has returned, and `ChildAdded` sees all of it.
///
/// An object whose borrow is held by a running callback has its list change applied when that
/// callback returns, and its event is posted to the thread's event queue: the next event
/// processing delivers it, not this call. An event that is delivered at once (to the other
/// parent, when its borrow is not held) can therefore come before a posted one. The events go to
/// the receiver directly, so its object event filters do not see them, unlike those of
/// [`reparent_owned_object`] and [`add_owned_child`].
///
/// If the `ChildRemoved` handler panics, the registry hierarchy lists and child parent link in
/// the registry are repaired prior to `resume_unwind`, and `ChildAdded` is suppressed. If the
/// child's borrow was not retained across the panic, its local `ObjectData` parent is also
/// synchronized prior to `resume_unwind`; however, if an `ObjectBorrowGuard` on the child remains
/// alive across the panic, local parent data synchronization is deferred until that guard drops,
/// rather than being applied prior to `resume_unwind`.
struct ReparentGuard<'a>(&'a AtomicBool);
impl<'a> Drop for ReparentGuard<'a> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

struct ShortTermBorrowGuard<'a>(&'a AtomicBool);
impl<'a> Drop for ShortTermBorrowGuard<'a> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
/// Status of post-commit synchronization of local parent data with the registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostCommitSyncStatus {
    /// Synchronized directly to local ObjectData via with_object_mut.
    AppliedDirectly,
    /// Direct borrow could not be acquired (held by active callback); queued in ObjectRecord
    /// for deterministic application upon release of that borrow.
    DeferredToPending,
    /// Object is unregistered or dead; no local synchronization needed.
    DeadOrUnregistered,
}

pub(crate) fn sync_child_local_parent(
    _child_id: ObjectId,
    child_record: &Arc<ObjectRecord>,
    current_reg_parent: Option<ObjectId>,
    target_liveness: Option<(ObjectId, Arc<AtomicBool>)>,
) -> PostCommitSyncStatus {
    if child_record.borrow_state.memory_destroyed.load(Ordering::Acquire) {
        return PostCommitSyncStatus::DeadOrUnregistered;
    }
    if child_record
        .borrow_state
        .borrow_flag
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        let _release = ReleaseBorrowFlag(&child_record.borrow_state);
        let mut data = unsafe { (*child_record.ptr.0).object_data_mut() };
        let raw = data.raw_data_mut();
        raw.parent = current_reg_parent;
        raw.parent_liveness = target_liveness;
        *lock_unpoisoned(&child_record.pending_parent_sync) = None;
        child_record.has_pending_parent_sync.store(false, Ordering::Release);
        PostCommitSyncStatus::AppliedDirectly
    } else {
        *lock_unpoisoned(&child_record.pending_parent_sync) = Some((current_reg_parent, target_liveness));
        child_record.has_pending_parent_sync.store(true, Ordering::Release);
        PostCommitSyncStatus::DeferredToPending
    }
}

/// Returns whether a registered object has a pending post-commit parent sync queued.
pub fn query_pending_parent_sync(child_id: ObjectId) -> Option<bool> {
    registered_ptr(child_id).map(|(rec, _)| rec.has_pending_parent_sync.load(Ordering::Acquire))
}

/// Sets a registered object's parent by its [`ObjectId`].
///
/// This id-based interface eliminates `&mut ObjectData` aliasing across callbacks,
/// and rejects conflicting nested hierarchy mutations on the same object with [`ReparentError::Busy`].
///
/// # Thread affinity and DifferentThread detach exception
///
/// When `new_parent` belongs to a different thread affinity than `child_id`, `set_parent_by_id`
/// follows Qt `setParent` semantics: the child is detached from its old parent in both registry
/// and local data (the old parent receives `ChildRemoved`), left parentless (`parent() == None`),
/// a warning is printed to stderr, and `Err(ReparentError::DifferentThread)` is returned.
/// This detach behavior is an explicit, documented exception to the general "zero state mutation
/// on failure" guarantee of other reparent errors.
///
/// # Post-commit synchronization guarantee
///
/// Once registry and hierarchy write locks are committed, `set_parent_by_id` synchronizes local
/// child and parent data without post-commit `Busy` aborts. If the child is actively borrowed by
/// an ongoing callback, the parent update is queued via a pending sync record and applied
/// deterministically upon release of that borrow.
///
/// If a `ChildRemoved` handler panics while retaining an `ObjectBorrowGuard` on the child,
/// the global registry and hierarchy lists are repaired before `resume_unwind`, but local
/// parent data synchronization remains pending and is deferred until that `ObjectBorrowGuard`
/// drops.
pub fn set_parent_by_id(child_id: ObjectId, new_parent: Option<ObjectId>) -> Result<(), ReparentError> {
    let (child_record, _) = registered_ptr(child_id).ok_or(ReparentError::UnknownObject)?;
    if child_record
        .reparenting
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Err(ReparentError::Busy);
    }
    let _reparent_guard = ReparentGuard(&child_record.reparenting);

    if child_record.borrow_state.borrow_flag.load(Ordering::Acquire) {
        return Err(ReparentError::Busy);
    }

    let old_parent = *child_record.parent.read().map_err(|_| ReparentError::Busy)?;
    if old_parent == new_parent {
        return Ok(());
    }

    let child_owned = with_object(child_id, |c| c.object_data().is_owned_by_parent())
        .ok_or(ReparentError::Busy)?;
    if child_owned {
        return Err(ReparentError::OwnedByParent);
    }

    if let Some(parent_id) = old_parent {
        let parent_owns = with_object(parent_id, |p| {
            p.object_data()
                .owned_children
                .iter()
                .any(|c| OwnedObject::object_id(c) == child_id)
        })
        .ok_or(ReparentError::Busy)?;
        if parent_owns {
            return Err(ReparentError::OwnedByParent);
        }
    }

    child_record
        .borrow_state
        .borrow_flag
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .map_err(|_| ReparentError::Busy)?;
    let short_term_borrow = ShortTermBorrowGuard(&child_record.borrow_state.borrow_flag);

    let child_thread_id = *child_record.thread_id.read().map_err(|_| ReparentError::Busy)?;
    let requested_parent = new_parent;
    let refused_thread = requested_parent.is_some_and(|np| in_different_thread(child_thread_id, np));
    let new_parent = if refused_thread { None } else { requested_parent };

    let hierarchy = lock_hierarchy();
    let registry = GLOBAL_OBJECT_REGISTRY.read().map_err(|_| ReparentError::Busy)?;
    if let Some(np) = requested_parent {
        if would_create_cycle_in(&registry, child_id, np)? {
            return Err(ReparentError::InvalidParent);
        }
    }
    #[cfg(test)]
    test_hooks::run(test_hooks::Point::AfterCycleCheck);

    let old_rec = old_parent.and_then(|id| registry.get(&id));
    let new_rec = new_parent.and_then(|id| registry.get(&id));

    if let Some(_) = new_parent {
        if new_rec.is_none() {
            return Err(ReparentError::InvalidParent);
        }
    }

    enum ParentLockPair<'a> {
        None,
        SingleOld(std::sync::RwLockWriteGuard<'a, Vec<ObjectId>>),
        SingleNew(std::sync::RwLockWriteGuard<'a, Vec<ObjectId>>),
        Both {
            old_guard: std::sync::RwLockWriteGuard<'a, Vec<ObjectId>>,
            new_guard: std::sync::RwLockWriteGuard<'a, Vec<ObjectId>>,
        },
    }

    let parent_locks = match (old_parent, new_parent) {
        (Some(old_id), Some(new_id)) if old_id != new_id => {
            let old_r = old_rec.ok_or(ReparentError::InvalidParent);
            let new_r = new_rec.ok_or(ReparentError::InvalidParent);
            if let (Ok(o_rec), Ok(n_rec)) = (old_r, new_r) {
                if old_id < new_id {
                    let o_g = o_rec.children.write().map_err(|_| ReparentError::Busy)?;
                    let n_g = n_rec.children.write().map_err(|_| ReparentError::Busy)?;
                    ParentLockPair::Both { old_guard: o_g, new_guard: n_g }
                } else {
                    let n_g = n_rec.children.write().map_err(|_| ReparentError::Busy)?;
                    let o_g = o_rec.children.write().map_err(|_| ReparentError::Busy)?;
                    ParentLockPair::Both { old_guard: o_g, new_guard: n_g }
                }
            } else if let Ok(o_rec) = old_r {
                let o_g = o_rec.children.write().map_err(|_| ReparentError::Busy)?;
                ParentLockPair::SingleOld(o_g)
            } else if let Ok(n_rec) = new_r {
                let n_g = n_rec.children.write().map_err(|_| ReparentError::Busy)?;
                ParentLockPair::SingleNew(n_g)
            } else {
                ParentLockPair::None
            }
        }
        (Some(_), _) => {
            if let Some(o_rec) = old_rec {
                let o_g = o_rec.children.write().map_err(|_| ReparentError::Busy)?;
                ParentLockPair::SingleOld(o_g)
            } else {
                ParentLockPair::None
            }
        }
        (_, Some(_)) => {
            if let Some(n_rec) = new_rec {
                let n_g = n_rec.children.write().map_err(|_| ReparentError::Busy)?;
                ParentLockPair::SingleNew(n_g)
            } else {
                ParentLockPair::None
            }
        }
        (None, None) => ParentLockPair::None,
    };

    let child_parent_guard = child_record.parent.write().map_err(|_| ReparentError::Busy)?;
    {
        let parent_locks = parent_locks;
        let mut child_parent_guard = child_parent_guard;
        match parent_locks {
            ParentLockPair::Both { mut old_guard, mut new_guard } => {
                old_guard.retain(|&id| id != child_id);
                if !new_guard.contains(&child_id) {
                    new_guard.push(child_id);
                }
            }
            ParentLockPair::SingleOld(mut old_guard) => {
                old_guard.retain(|&id| id != child_id);
            }
            ParentLockPair::SingleNew(mut new_guard) => {
                if !new_guard.contains(&child_id) {
                    new_guard.push(child_id);
                }
            }
            ParentLockPair::None => {}
        }

        *child_parent_guard = new_parent;
    }

    let new_liveness = new_parent.and_then(|id| registry.get(&id).map(|rec| (id, Arc::clone(&rec.liveness))));
    drop(registry);
    drop(hierarchy);
    drop(short_term_borrow);
    #[cfg(test)]
    let _held_by_mutant =
        test_hooks::mutant("set_parent_holds_registry").then(|| GLOBAL_OBJECT_REGISTRY.read());
    let removed_panic = old_parent.and_then(|old_parent_id| {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            edit_child_list(old_parent_id, ChildListChange::Remove(child_id));
            let removed = Event::new(EventKind::ChildRemoved { child_id });
            if let Err(mut removed) = defer_if_borrowed(old_parent_id, removed) {
                dispatch_to_object(old_parent_id, &mut removed);
            }
        }))
        .err()
    });

    let current_reg_parent = query_object_parent(child_id).flatten();
    let target_liveness = if current_reg_parent == new_parent {
        new_liveness
    } else {
        current_reg_parent.and_then(|id| liveness_of(id).map(|alive| (id, alive)))
    };
    let _sync_status = sync_child_local_parent(
        child_id,
        &child_record,
        current_reg_parent,
        target_liveness,
    );
    if current_reg_parent != new_parent {
        if let Some(np) = new_parent {
            if let Ok(reg) = GLOBAL_OBJECT_REGISTRY.read() {
                if let Some(rec) = reg.get(&np) {
                    if let Ok(mut ch) = rec.children.write() {
                        ch.retain(|&id| id != child_id);
                    }
                }
            }
        }
    }

    if let Some(target_parent_id) = current_reg_parent {
        edit_child_list(target_parent_id, ChildListChange::Add(child_id));
        if removed_panic.is_none() {
            let added = Event::new(EventKind::ChildAdded { child_id });
            if let Err(mut added) = defer_if_borrowed(target_parent_id, added) {
                dispatch_to_object(target_parent_id, &mut added);
            }
        }
    }
    if let Some(payload) = removed_panic {
        std::panic::resume_unwind(payload);
    }
    if refused_thread {
        eprintln!("QObject::setParent: Cannot set parent, new parent is in a different thread");
        return Err(ReparentError::DifferentThread);
    }
    Ok(())
}

pub fn set_parent(child: &mut ObjectData, new_parent: Option<ObjectId>) -> Result<(), ReparentError> {
    let old_parent = child.parent();
    if old_parent == new_parent {
        return Ok(());
    }
    if child.owned_by_parent {
        return Err(ReparentError::OwnedByParent);
    }
    let child_id = child.id;
    let requested_parent = new_parent;
    let refused_thread = requested_parent.is_some_and(|np| in_different_thread(child.thread_id, np));
    let new_parent = if refused_thread { None } else { requested_parent };

    let child_rec = registered_ptr(child_id).map(|(rec, _)| rec);
    let _reparent_guard = if let Some(ref rec) = child_rec {
        if rec
            .reparenting
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(ReparentError::Busy);
        }
        Some(ReparentGuard(&rec.reparenting))
    } else {
        None
    };

    let short_term_borrow = if let Some(ref rec) = child_rec {
        rec.borrow_state
            .borrow_flag
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| ReparentError::Busy)?;
        Some(ShortTermBorrowGuard(&rec.borrow_state.borrow_flag))
    } else {
        None
    };

    // `HIERARCHY_MUTATION`, then the registry (L0). The cycle check and the commit below both
    // happen under the two, so no other operation can add a parent link between them.
    let hierarchy = lock_hierarchy();
    let registry = GLOBAL_OBJECT_REGISTRY.read().map_err(|_| ReparentError::Busy)?;
    if let Some(np) = requested_parent {
        if would_create_cycle_in(&registry, child_id, np)? {
            return Err(ReparentError::InvalidParent);
        }
    }
    #[cfg(test)]
    test_hooks::run(test_hooks::Point::AfterCycleCheck);

    let child_is_registered = registry.contains_key(&child_id);
    let old_rec = old_parent.and_then(|id| registry.get(&id));
    let new_rec = new_parent.and_then(|id| registry.get(&id));

    if child_is_registered {
        if let Some(_) = new_parent {
            if new_rec.is_none() {
                return Err(ReparentError::InvalidParent);
            }
        }
    }

    // Tier 1: Acquire parents' `children` write lock(s) in ascending ObjectId order.
    enum ParentLockPair<'a> {
        None,
        SingleOld(std::sync::RwLockWriteGuard<'a, Vec<ObjectId>>),
        SingleNew(std::sync::RwLockWriteGuard<'a, Vec<ObjectId>>),
        Both {
            old_guard: std::sync::RwLockWriteGuard<'a, Vec<ObjectId>>,
            new_guard: std::sync::RwLockWriteGuard<'a, Vec<ObjectId>>,
        },
    }

    let parent_locks = match (old_parent, new_parent) {
        (Some(old_id), Some(new_id)) if old_id != new_id => {
            let old_r = old_rec.ok_or(ReparentError::InvalidParent);
            let new_r = new_rec.ok_or(ReparentError::InvalidParent);
            if let (Ok(o_rec), Ok(n_rec)) = (old_r, new_r) {
                if old_id < new_id {
                    let o_g = o_rec.children.write().map_err(|_| ReparentError::Busy)?;
                    let n_g = n_rec.children.write().map_err(|_| ReparentError::Busy)?;
                    ParentLockPair::Both { old_guard: o_g, new_guard: n_g }
                } else {
                    let n_g = n_rec.children.write().map_err(|_| ReparentError::Busy)?;
                    let o_g = o_rec.children.write().map_err(|_| ReparentError::Busy)?;
                    ParentLockPair::Both { old_guard: o_g, new_guard: n_g }
                }
            } else if let Ok(o_rec) = old_r {
                let o_g = o_rec.children.write().map_err(|_| ReparentError::Busy)?;
                ParentLockPair::SingleOld(o_g)
            } else if let Ok(n_rec) = new_r {
                let n_g = n_rec.children.write().map_err(|_| ReparentError::Busy)?;
                ParentLockPair::SingleNew(n_g)
            } else {
                ParentLockPair::None
            }
        }
        (Some(_), _) => {
            if let Some(o_rec) = old_rec {
                let o_g = o_rec.children.write().map_err(|_| ReparentError::Busy)?;
                ParentLockPair::SingleOld(o_g)
            } else {
                ParentLockPair::None
            }
        }
        (_, Some(_)) => {
            if let Some(n_rec) = new_rec {
                let n_g = n_rec.children.write().map_err(|_| ReparentError::Busy)?;
                ParentLockPair::SingleNew(n_g)
            } else {
                ParentLockPair::None
            }
        }
        (None, None) => ParentLockPair::None,
    };

    // Tier 2: Acquire child's `record.parent` write lock if child is registered.
    let child_parent_guard = if child_is_registered {
        Some(registry.get(&child_id).unwrap().parent.write().map_err(|_| ReparentError::Busy)?)
    } else {
        None
    };

    // Infallible Commit: all locks held, no failures possible from here. The guards are moved
    // into a block of their own, so that L1 and L2 are gone before L0 is let go below.
    {
        let parent_locks = parent_locks;
        let child_parent_guard = child_parent_guard;
        match parent_locks {
            ParentLockPair::Both { mut old_guard, mut new_guard } => {
                old_guard.retain(|&id| id != child_id);
                if !new_guard.contains(&child_id) {
                    new_guard.push(child_id);
                }
            }
            ParentLockPair::SingleOld(mut old_guard) => {
                old_guard.retain(|&id| id != child_id);
            }
            ParentLockPair::SingleNew(mut new_guard) => {
                if !new_guard.contains(&child_id) {
                    new_guard.push(child_id);
                }
            }
            ParentLockPair::None => {}
        }

        if let Some(mut p_guard) = child_parent_guard {
            *p_guard = new_parent;
        }
    }

    // The registry lock (L0) and `HIERARCHY_MUTATION` end here. Below, `edit_child_list`,
    // `defer_if_borrowed` and `dispatch_to_object` take the registry again for themselves, deliver
    // events to handlers that may register or drop objects, and take the `pending_child_changes`
    // mutex, which is only taken with none of L0, L1 and L2 held. What the child keeps of its new
    // parent is copied out first.
    let new_liveness = new_parent.and_then(|id| registry.get(&id).map(|rec| (id, Arc::clone(&rec.liveness))));
    drop(registry);
    drop(hierarchy);
    #[cfg(test)]
    let _held_by_mutant =
        test_hooks::mutant("set_parent_holds_registry").then(|| GLOBAL_OBJECT_REGISTRY.read());
    // Update local children lists and send ChildRemoved/ChildAdded via registered callbacks. A
    // panic of the old parent's handler is held back until the child and the new parent's list
    // agree with the registry, and then continues.
    let removed_panic = old_parent.and_then(|old_parent_id| {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            edit_child_list(old_parent_id, ChildListChange::Remove(child_id));
            let removed = Event::new(EventKind::ChildRemoved { child_id });
            if let Err(mut removed) = defer_if_borrowed(old_parent_id, removed) {
                dispatch_to_object(old_parent_id, &mut removed);
            }
        }))
        .err()
    });

    // Following Qt's setParent_helper ordering: child.parent is updated after ChildRemoved is
    // dispatched, and reconciled on panic before resuming unwind.
    let pending_sync = if let Some(ref rec) = child_rec {
        if rec.has_pending_parent_sync.swap(false, Ordering::AcqRel) {
            lock_unpoisoned(&rec.pending_parent_sync).take()
        } else {
            None
        }
    } else {
        None
    };

    let (effective_parent, effective_liveness) = if let Some((synced_parent, synced_liveness)) = pending_sync {
        (synced_parent, synced_liveness)
    } else if child_rec.is_some() && query_object_parent(child_id).flatten() != new_parent {
        let current_reg = query_object_parent(child_id).flatten();
        let target_liveness = current_reg.and_then(|id| liveness_of(id).map(|alive| (id, alive)));
        (current_reg, target_liveness)
    } else {
        (new_parent, new_liveness.clone())
    };

    child.parent = effective_parent;
    child.parent_liveness = effective_liveness;

    let current_reg_parent = effective_parent;
    if let Some(target_parent_id) = current_reg_parent {
        edit_child_list(target_parent_id, ChildListChange::Add(child_id));
        if removed_panic.is_none() {
            let added = Event::new(EventKind::ChildAdded { child_id });
            if let Err(mut added) = defer_if_borrowed(target_parent_id, added) {
                dispatch_to_object(target_parent_id, &mut added);
            }
        }
    }
    drop(short_term_borrow);
    if let Some(payload) = removed_panic {
        std::panic::resume_unwind(payload);
    }
    if refused_thread {
        eprintln!("QObject::setParent: Cannot set parent, new parent is in a different thread");
        return Err(ReparentError::DifferentThread);
    }
    Ok(())
}

/// Moves an owned child to `new_parent`, or releases it to the caller when `new_parent` is
/// `None` (`Ok(Some(owner))`; the child stays alive and registered, with no parent).
///
/// Unlike Qt's `setParent`, ownership is explicit: the parent owns the child, so detaching hands
/// the child to the caller instead of dropping it. `ChildRemoved`/`ChildAdded` go through
/// `notify_helper`, so object and application event filters see them. Validation and borrows are
/// taken before anything is changed, so an `Err` leaves the hierarchy exactly as it was.
///
/// Refuses with [`ReparentError::DifferentThread`] when the new parent's thread differs from the
/// child's. Unlike [`set_parent`] it changes nothing: the child keeps its old parent and that
/// parent keeps owning it, no event is sent, and no warning is printed. Qt's `setParent` would
/// detach the child and leave it without a parent; here that would leave the child without an
/// owner, and the return type cannot hand it back together with an error.
///
/// The errors: `UnknownObject` when the child is not registered on the calling thread;
/// `InvalidParent` when the new parent is the child itself (answered before anything is
/// borrowed, also from inside the child's own callback), is not registered on the calling thread,
/// or is a descendant of the child; `NotOwned` when the child has no parent, its parent does not
/// own it, or that parent is not registered on the calling thread; `DifferentThread`; and `Busy`
/// when the child or a parent is borrowed by a running callback, or a lock is poisoned. A
/// parentless child, or a new parent, that is borrowed is `Busy` before the checks that need the
/// borrows: the descendant check is made last, once the borrows are held, so a descendant that is
/// borrowed answers `Busy`. Which of several applicable errors is returned is not otherwise
/// specified. Moving the child to the parent that already owns it changes nothing and returns
/// `Ok(None)`, as a real move does.
///
/// A released child comes back as an [`OwnedObject`], which shares its pointer with the
/// registry, so the caller may use the child between its callbacks while it stays registered. It
/// is never given up as a `Box`: a `Box` claims to be the only pointer to its object, and the
/// registry's pointer contradicts that.
pub fn reparent_owned_object(
    child_id: ObjectId,
    new_parent: Option<ObjectId>,
) -> Result<Option<OwnedObject<dyn QObject>>, ReparentError> {
    // The records and the borrows come first, with no lock held: the records by one registry
    // lookup each (which takes L0 and lets it go), the borrows by `borrow_record`, one
    // compare-and-swap that fails at once, so nothing ever waits for it. The handles to the data
    // the commit writes come from the objects' accessors (their own code) before any lock, too.
    //
    // The commit takes `HIERARCHY_MUTATION` and the registry (L0) once, checks that the records
    // are still the registered ones and that the new parent is not a descendant, takes the
    // `children` of the parents (L1, lower id first) and the `parent` of the child (L2), and
    // writes through the handles. The borrow guards are declared before all of that, so they are
    // dropped after L2, L1, L0 and `HIERARCHY_MUTATION`: dropping one takes the
    // `pending_child_changes` mutex and can free parked children, which unregisters them and so
    // takes L0 for writing.
    let (child_record, _) = registered_ptr(child_id).ok_or(ReparentError::UnknownObject)?;
    // An object is not its own parent. This is answered before the child is borrowed: the borrow
    // of the new parent, which would be the child, would answer `Busy` otherwise.
    if new_parent == Some(child_id) {
        return Err(ReparentError::InvalidParent);
    }
    let mut child_guard = borrow_record(&child_record).map_err(|_| ReparentError::Busy)?;

    let old_parent_id = (*child_record.parent.read().map_err(|_| ReparentError::Busy)?)
        .ok_or(ReparentError::NotOwned)?;
    let old_record = record_of(old_parent_id)?.ok_or(ReparentError::NotOwned)?;

    if new_parent == Some(old_parent_id) {
        let owned = with_object(old_parent_id, |p| {
            p.object_data()
                .owned_children
                .iter()
                .any(|c| OwnedObject::object_id(c) == child_id)
        })
        .ok_or(ReparentError::Busy)?;
        return if owned { Ok(None) } else { Err(ReparentError::NotOwned) };
    }

    let new_record = match new_parent {
        Some(np) => {
            let child_thread = *child_record.thread_id.read().map_err(|_| ReparentError::Busy)?;
            if in_different_thread(child_thread, np) {
                return Err(ReparentError::DifferentThread);
            }
            Some(record_of(np)?.ok_or(ReparentError::InvalidParent)?)
        }
        None => None,
    };

    // A parent that is not registered on the calling thread, or is dead, is not `Busy`: the
    // calling thread cannot do what is asked of it.
    let mut old_guard = borrow_record(&old_record).map_err(|error| match error {
        ReparentError::Busy => ReparentError::Busy,
        _ => ReparentError::NotOwned,
    })?;
    let mut new_guard = match &new_record {
        Some(record) => Some(borrow_record(record).map_err(|error| match error {
            ReparentError::Busy => ReparentError::Busy,
            _ => ReparentError::InvalidParent,
        })?),
        None => None,
    };

    let mut child_data = child_guard.object_data_mut();
    let mut old_data = old_guard.object_data_mut();
    let mut new_data = new_guard.as_mut().map(|guard| guard.object_data_mut());

    let pos = old_data
        .owned_children
        .iter()
        .position(|c| OwnedObject::object_id(c) == child_id)
        .ok_or(ReparentError::NotOwned)?;

    enum ParentLockPair<'a> {
        SingleOld(std::sync::RwLockWriteGuard<'a, Vec<ObjectId>>),
        Both {
            old_guard: std::sync::RwLockWriteGuard<'a, Vec<ObjectId>>,
            new_guard: std::sync::RwLockWriteGuard<'a, Vec<ObjectId>>,
        },
    }

    let released = {
        let hierarchy = lock_hierarchy();
        let registry = GLOBAL_OBJECT_REGISTRY.read().map_err(|_| ReparentError::Busy)?;
        // The records taken above must still be the registered ones. Nothing is unregistered or
        // registered again while a callback of the object runs, and the borrows above stand for
        // one; this keeps a broken promise from becoming a commit on a record nobody can reach.
        let registered = |id: ObjectId, record: &Arc<ObjectRecord>| {
            registry.get(&id).is_some_and(|current| Arc::ptr_eq(current, record))
        };
        if !registered(child_id, &child_record) {
            return Err(ReparentError::UnknownObject);
        }
        if !registered(old_parent_id, &old_record) {
            return Err(ReparentError::NotOwned);
        }
        if let (Some(np), Some(record)) = (new_parent, &new_record) {
            if !registered(np, record) {
                return Err(ReparentError::InvalidParent);
            }
            if would_create_cycle_in(&registry, child_id, np)? {
                return Err(ReparentError::InvalidParent);
            }
        }
        #[cfg(test)]
        test_hooks::run(test_hooks::Point::AfterCycleCheck);

        let parent_locks = match (new_parent, &new_record) {
            (Some(new_id), Some(new_rec)) => {
                if old_parent_id < new_id {
                    let o = old_record.children.write().map_err(|_| ReparentError::Busy)?;
                    let n = new_rec.children.write().map_err(|_| ReparentError::Busy)?;
                    ParentLockPair::Both { old_guard: o, new_guard: n }
                } else {
                    let n = new_rec.children.write().map_err(|_| ReparentError::Busy)?;
                    let o = old_record.children.write().map_err(|_| ReparentError::Busy)?;
                    ParentLockPair::Both { old_guard: o, new_guard: n }
                }
            }
            _ => {
                let o = old_record.children.write().map_err(|_| ReparentError::Busy)?;
                ParentLockPair::SingleOld(o)
            }
        };

        let mut child_parent_guard = child_record.parent.write().map_err(|_| ReparentError::Busy)?;
        if *child_parent_guard != Some(old_parent_id) {
            return Err(ReparentError::NotOwned);
        }

        // Infallible Commit: all locks and borrows held, and no code of an object runs.
        let owner = {
            old_data.children_mut().retain(|&id| id != child_id);
            old_data.owned_children_mut().remove(pos)
        };

        let new_liveness = new_parent
            .zip(new_record.as_ref())
            .map(|(id, record)| (id, Arc::clone(&record.liveness)));
        {
            let raw = child_data.raw_data_mut();
            raw.parent = new_parent;
            raw.parent_liveness = new_liveness;
            raw.owned_by_parent = new_parent.is_some();
        }

        match parent_locks {
            ParentLockPair::Both { mut old_guard, mut new_guard } => {
                old_guard.retain(|&id| id != child_id);
                if !new_guard.contains(&child_id) {
                    new_guard.push(child_id);
                }
            }
            ParentLockPair::SingleOld(mut old_guard) => {
                old_guard.retain(|&id| id != child_id);
            }
        }
        *child_parent_guard = new_parent;

        let released = match new_data.as_mut() {
            Some(data) => {
                if !data.children.contains(&child_id) {
                    data.children_mut().push(child_id);
                }
                data.owned_children_mut().push(owner);
                None
            }
            None => Some(owner),
        };

        drop(child_parent_guard);
        drop(registry);
        drop(hierarchy);
        released
    };

    drop(new_guard);
    drop(old_guard);
    drop(child_guard);

    let mut removed = Event::new(EventKind::ChildRemoved { child_id });
    crate::event_loop::notify_helper(old_parent_id, &mut removed);
    if let Some(np) = new_parent {
        let mut added = Event::new(EventKind::ChildAdded { child_id });
        crate::event_loop::notify_helper(np, &mut added);
    }
    Ok(released)
}

/// Attaches `child` as an owned child of `parent_id` and sends `ChildAdded` to the parent
/// (Qt `setParent_helper`, qobject.cpp:2346-2352).
///
/// Returns the child id. If the parent's thread differs from the child's, returns
/// `DifferentThread`; if the parent is not registered, returns `UnknownObject`; if the child has
/// a parent already (take it from that parent first: that parent would go on listing it, and
/// dropping that parent would unregister the child the new parent owns), or the parent is the
/// child itself or one of its descendants (a child that is registered already can have
/// descendants), returns `InvalidParent`; if the parent is borrowed (for example from inside its
/// own callback), returns `Busy`, which is also what a borrowed descendant answers. In every case
/// the `Box` is
/// handed back and nothing is attached: the child gets no parent, the parent's children stay as
/// they were, and no event is sent. For `DifferentThread` Qt's constructor ends the same way
/// (qobject.cpp:941-959, 994-997) but prints a warning; this prints nothing.
///
/// On success the parent holds the child in an [`OwnedObject`] and the child is registered with
/// that owner's pointer, so moving the child between parents leaves the registry's pointer valid.
///
/// Dropping the parent from inside a callback of this child is allowed. The child, and everything
/// the registry lists below it, is then destroyed at once for everything but that callback: it is
/// unregistered, no longer alive, gets no events or timers, and its connections to other objects
/// are gone. Its memory, and so its `Drop`, waits until the callback returns, because the
/// callback still runs on `&mut self`. Qt frees the child there and then. A signal emission that
/// is under way ends as in Qt: no slot is called after the one that dropped the parent (see
/// [`Signal::emit`](crate::signal::Signal::emit)).
///
/// # Safety
/// The child must remain at its allocated address while owned and registered, and must be used on
/// its registration thread; the thread rules of [`register_qobject_ptr`] apply. While a callback
/// of the child runs, nothing outside it accesses the child. A reference to the child taken
/// before this call, from the `Box` or from a pointer made from it, is not valid after it.
pub unsafe fn add_owned_child<T: QObject + 'static>(
    parent_id: ObjectId,
    child: Box<T>,
) -> Result<ObjectId, (ReparentError, Box<T>)> {
    if in_different_thread(child.object_data().thread_id(), parent_id) {
        return Err((ReparentError::DifferentThread, child));
    }
    if registered_ptr(parent_id).is_none() {
        return Err((ReparentError::UnknownObject, child));
    }
    if child.object_data().parent().is_some() {
        return Err((ReparentError::InvalidParent, child));
    }
    let child_id = child.object_data().id();
    let mut pending = Some(child);
    let attached = with_object_mut(parent_id, |parent| {
        let mut child = pending.take().expect("closure runs at most once");
        let parent_liveness = liveness_of(parent_id).map(|alive| (parent_id, alive));
        {
            let mut data = child.object_data_mut();
            let raw = data.raw_data_mut();
            raw.parent = Some(parent_id);
            raw.parent_liveness = parent_liveness;
            raw.owned_by_parent = true;
        }
        // The parent holds the child through an owner made from the `Box`; the registry gets a
        // copy of the owner's pointer, so moving the owner later invalidates nothing.
        let owner = OwnedObject::from_box(child);
        let ptr: NonNull<dyn QObject> = OwnedObject::<T>::as_non_null(&owner);
        // The accessors of both objects (their own code) run before the registry is touched.
        let mut parent_data = parent.object_data_mut();
        // SAFETY: the owner keeps the object alive at this address, and nothing else reaches it.
        let child_data = unsafe { (*ptr.as_ptr()).object_data() };
        // The registration checks that the parent is not a descendant of the child, records the
        // child and lists it in the registry's list of the parent, all under
        // `HIERARCHY_MUTATION`. That list is what still reaches the child while a running
        // callback of the parent hides it from the parent's own `children`.
        match register_record(child_data, ptr.as_ptr(), Some(parent_id)) {
            Ok(()) => {
                parent_data.add_child(child_id);
                parent_data.owned_children_mut().push(OwnedObject::into_dyn(owner));
                Ok(())
            }
            Err(error) => {
                // Nothing was inserted, so the registry holds no pointer made from this owner.
                // SAFETY: see above; the object goes back to the caller as it came.
                let mut child = unsafe { OwnedObject::into_box(owner) };
                {
                    let mut data = child.object_data_mut();
                    let raw = data.raw_data_mut();
                    raw.clear_parent();
                    raw.owned_by_parent = false;
                }
                pending = Some(child);
                Err(error)
            }
        }
    });
    match attached {
        Some(Ok(())) => {
            let mut added = Event::new(EventKind::ChildAdded { child_id });
            crate::event_loop::notify_helper(parent_id, &mut added);
            Ok(child_id)
        }
        Some(Err(error)) => Err((
            error,
            pending
                .take()
                .expect("the closure gave the Box back with the error"),
        )),
        None => Err((
            ReparentError::Busy,
            pending
                .take()
                .expect("closure did not run, so the Box is still here"),
        )),
    }
}

/// Sets the links of the three objects it is given and nothing else: it takes no registry lock,
/// sends no event and does not look at the registry. A registered object's registry record and
/// its parent's registry list stay as they were; use [`set_parent`] or [`reparent_owned_object`]
/// for those. Since it only edits the objects' own links it cannot make the registry cyclic: a
/// cycle built out of such links is not recorded when the objects are registered.
pub fn reparent(
    child: &mut ObjectData,
    old_parent: Option<&mut ObjectData>,
    new_parent: Option<&mut ObjectData>,
) {
    if let Some(old) = old_parent {
        old.children.retain(|&id| id != child.id);
    }
    if let Some(new_p) = new_parent {
        child.parent = Some(new_p.id);
        child.parent_liveness = Some((new_p.id, Arc::clone(&new_p.liveness)));
        if !new_p.children.contains(&child.id) {
            new_p.children.push(child.id);
        }
    } else {
        child.clear_parent();
    }
}

/// Core polymorphic object trait: `QObject`.
pub trait QObject: std::any::Any {
    /// Core object metadata: ObjectData (`QObjectData` equivalent).
    fn object_data(&self) -> &ObjectData;
    fn object_data_mut(&mut self) -> ObjectDataMut<'_>;
    /// Returns the static MetaObject reflection structure for this class (`QObject::metaObject`).
    fn meta_object(&self) -> &'static crate::meta::MetaObject {
        &crate::meta::QOBJECT_META_OBJECT
    }

    /// Returns true if this object inherits the given class name (`QObject::inherits`).
    fn inherits(&self, class_name: &str) -> bool {
        self.meta_object().inherits_name(class_name)
    }
    /// Optional downcast to Any for hierarchy reflection.
    fn as_qobject_any(&self) -> Option<&dyn std::any::Any> {
        None
    }

    /// Optional mutable downcast to Any for hierarchy reflection.
    fn as_qobject_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        None
    }

    /// Returns object name (`QObject::objectName`).
    fn object_name(&self) -> Option<&str> {
        self.object_data().object_name()
    }

    /// Sets object name (`QObject::setObjectName`).
    fn set_object_name(&mut self, name: &str) {
        self.object_data_mut().set_object_name(name);
    }

    /// Signal emitted when object name changes (`QObject::objectNameChanged`).
    fn object_name_changed(&self) -> &Signal<String> {
        &self.object_data().object_name_changed
    }

    /// Returns parent object ID (`QObject::parent`).
    fn parent(&self) -> Option<ObjectId> {
        self.object_data().parent()
    }

    /// Returns direct children object IDs (`QObject::children`).
    ///
    /// Named `children_ids` to avoid name collision with GUI/Widget tree `Widget::children()`.
    fn children_ids(&self) -> Vec<ObjectId> {
        self.object_data().children().to_vec()
    }
    /// Returns the thread affinity of this object (`QObject::thread`).
    fn thread(&self) -> ThreadId {
        self.object_data().thread()
    }

    /// Whether [`move_to_thread_by_id`](crate::object::move_to_thread_by_id) may change this
    /// object's thread (`QObject::moveToThread`). The default is `true`.
    ///
    /// Return `false` when state of the object is also reached through handles that never leave
    /// the creating thread, such as an `Rc` shared with a `!Send` owner. After a move the
    /// registry would run the object's callbacks on the target thread while the owner keeps using
    /// that state on the old one. The move is refused with
    /// [`MoveByIdError::NotMovable`](crate::object::MoveByIdError::NotMovable) before an event is
    /// sent or any state changes. A move takes the whole subtree, so the same answer from a
    /// descendant refuses the move of every ancestor.
    ///
    /// Called on the registration thread while the registry holds the object's borrow: it must not
    /// call back into the registry for this object, and its answer must not change while the
    /// object is registered.
    fn can_move_to_thread(&self) -> bool {
        true
    }

    /// Blocks or unblocks signals emitted by this object (`QObject::blockSignals`).
    /// Returns previous blocked state.
    fn block_signals(&mut self, block: bool) -> bool {
        self.object_data()
            .signals_blocked
            .swap(block, Ordering::AcqRel)
    }

    fn signals_blocked(&self) -> bool {
        self.object_data().signals_blocked.load(Ordering::Acquire)
    }

    /// Queries a child object by type ID and optional name (`QObject::findChild`).
    fn find_child_any(&self, name: &str, type_id: std::any::TypeId) -> Option<&dyn std::any::Any> {
        // 1. Direct children first (qt_qFindChild_helper: qobject.cpp:2242-2256)
        for child in &self.object_data().owned_children {
            if OwnedObject::is_borrowed(child) {
                continue;
            }
            let name_matches = name.is_empty() || child.object_name() == Some(name);
            if name_matches {
                if let Some(any) = child.as_qobject_any() {
                    if any.type_id() == type_id {
                        return Some(any);
                    }
                }
            }
        }
        // 2. Descendants if no direct child matched
        for child in &self.object_data().owned_children {
            if OwnedObject::is_borrowed(child) {
                continue;
            }
            if let Some(found) = child.find_child_any(name, type_id) {
                return Some(found);
            }
        }
        None
    }

    /// Queries the ObjectId of a child object matching type ID and optional name.
    fn find_child_id(&self, name: &str, type_id: std::any::TypeId) -> Option<ObjectId> {
        // 1. Direct children first (qt_qFindChild_helper: qobject.cpp:2242-2256)
        for child in &self.object_data().owned_children {
            if OwnedObject::is_borrowed(child) {
                continue;
            }
            let name_matches = name.is_empty() || child.object_name() == Some(name);
            if name_matches {
                if let Some(any) = child.as_qobject_any() {
                    if any.type_id() == type_id {
                        return Some(OwnedObject::object_id(child));
                    }
                }
            }
        }
        // 2. Descendants if no direct child matched
        for child in &self.object_data().owned_children {
            if OwnedObject::is_borrowed(child) {
                continue;
            }
            if let Some(found_id) = child.find_child_id(name, type_id) {
                return Some(found_id);
            }
        }
        None
    }

    fn find_child_any_mut(
        &mut self,
        name: &str,
        type_id: std::any::TypeId,
    ) -> Option<&mut dyn std::any::Any> {
        // 1. Direct children first (qt_qFindChild_helper: qobject.cpp:2242-2256)
        let mut match_idx = None;
        for (i, child) in self.object_data().owned_children.iter().enumerate() {
            if OwnedObject::is_borrowed(child) {
                continue;
            }
            let name_matches = name.is_empty() || child.object_name() == Some(name);
            if name_matches
                && child
                    .as_qobject_any()
                    .is_some_and(|a| a.type_id() == type_id)
            {
                match_idx = Some(i);
                break;
            }
        }
        if let Some(i) = match_idx {
            return self.object_data_mut().into_owned_children_mut()[i].as_qobject_any_mut();
        }
        // 2. Descendants if no direct child matched
        for child in self.object_data_mut().into_owned_children_mut() {
            if OwnedObject::is_borrowed(child) {
                continue;
            }
            if let Some(found) = child.find_child_any_mut(name, type_id) {
                return Some(found);
            }
        }
        None
    }

    /// Collects all descendant objects matching type ID and optional name (`QObject::findChildren`).
    fn find_children_any(
        &self,
        name: Option<&str>,
        type_id: std::any::TypeId,
    ) -> Vec<&dyn std::any::Any> {
        let mut results = Vec::new();
        let target_name = name.unwrap_or("");

        // 1. Check owned children
        for child in &self.object_data().owned_children {
            if OwnedObject::is_borrowed(child) {
                continue;
            }
            let name_matches = target_name.is_empty() || child.object_name() == Some(target_name);
            if name_matches {
                if let Some(any) = child.as_qobject_any() {
                    if any.type_id() == type_id {
                        results.push(any);
                    }
                }
            }
            results.extend(child.find_children_any(name, type_id));
        }

        results
    }

    /// Main event entry point (`QObject::event`).
    fn event(&mut self, event: &mut Event) -> bool {
        match &event.kind {
            EventKind::Timer { timer_id } => {
                self.timer_event(*timer_id);
                true
            }
            EventKind::DynamicPropertyChange { property_name } => {
                let name = property_name.clone();
                self.dynamic_property_change(&name);
                true
            }
            EventKind::ChildAdded { .. } | EventKind::ChildRemoved { .. } => {
                self.child_event(event);
                true
            }
            EventKind::User(_) => {
                self.custom_event(event);
                true
            }
            _ => false,
        }
    }

    /// Timer event handler (`QObject::timerEvent`).
    fn timer_event(&mut self, _timer_id: u64) {}

    /// Child added / removed event handler (`QObject::childEvent`).
    fn child_event(&mut self, _event: &mut Event) {}

    /// Custom event handler (`QObject::customEvent`).
    fn custom_event(&mut self, _event: &mut Event) {}

    /// Dynamic property change handler (`QObject::event` handling `QDynamicPropertyChangeEvent`).
    fn dynamic_property_change(&mut self, _property_name: &str) {}

    /// Retrieves property value, prioritizing static MetaProperty then dynamic property (`QObject::property`).
    fn property(&self, name: &str) -> Option<Variant> {
        let mo = self.meta_object();
        if let Some(idx) = mo.index_of_property(name) {
            if let Some(prop) = mo.property(idx) {
                if let Some(any) = self.as_qobject_any() {
                    if let Some(val) = prop.read(any) {
                        return Some(val);
                    }
                }
            }
        }
        self.object_data().property(name).cloned()
    }

    /// Sets property value, prioritizing static MetaProperty then fallback to dynamic property (`QObject::setProperty`).
    ///
    /// Qt semantic (`qobject.cpp:4541-4583`):
    /// - Static declared property: returns `true` if writable and write succeeds, `false` if read-only or write fails.
    /// - Dynamic property (not declared in meta-object): returns `false` (both on add/change and removal).
    ///   A `DynamicPropertyChange` event is dispatched if the dynamic property was added, modified, or removed.
    fn set_property(&mut self, name: &str, value: Variant) -> bool {
        let mo = self.meta_object();
        if let Some(idx) = mo.index_of_property(name) {
            if let Some(prop) = mo.property(idx).cloned() {
                if !prop.is_writable() {
                    return false;
                }
                if let Some(any_mut) = self.as_qobject_any_mut() {
                    return prop.write(any_mut, value).is_ok();
                }
                return false;
            }
        }

        let changed = self.object_data_mut().set_property(name, value);
        if changed {
            let mut ev = Event::new(EventKind::DynamicPropertyChange {
                property_name: name.to_string(),
            });
            self.event(&mut ev);
        }
        false
    }

    /// Dynamically invokes a method by signature or name (`QMetaObject::invokeMethod`).
    fn invoke_method(
        &mut self,
        member: &str,
        args: &[Variant],
    ) -> Result<Variant, crate::meta::InvokeError> {
        let mo = self.meta_object();
        if let Some(any_mut) = self.as_qobject_any_mut() {
            mo.invoke_method(any_mut, member, args)
        } else {
            Err(crate::meta::InvokeError::TargetBorrowFailed)
        }
    }

    /// Invokes a method with an explicit connection type (`QMetaObject::invokeMethod(obj, name,
    /// Qt::ConnectionType, ...)`, qmetaobject.cpp `invokeMethodImpl`).
    ///
    /// `Direct` invokes in the caller. `Auto` is `Direct` when the object lives on the calling thread
    /// (or is unregistered), otherwise it is queued. `Queued` and `BlockingQueued` are delegated to
    /// [`invoke_method_on_thread`], which needs no `&mut` borrow of the object.
    fn invoke_method_with_type(
        &mut self,
        member: &str,
        args: &[Variant],
        conn_type: crate::signal::ConnectionType,
    ) -> Result<Variant, crate::meta::InvokeError> {
        use crate::signal::ConnectionType;
        let id = self.object_data().id();
        let on_owner_thread =
            registration_thread_of(id).map_or(true, |thread| thread == ThreadId::current());
        let direct = match conn_type {
            ConnectionType::Direct => true,
            ConnectionType::Auto => on_owner_thread,
            ConnectionType::Queued | ConnectionType::BlockingQueued => false,
        };
        if direct {
            return self.invoke_method(member, args);
        }
        let meta = self.meta_object();
        invoke_method_on_thread(id, meta, member, args, conn_type)
    }

    /// Returns all registered dynamic property names (`QObject::dynamicPropertyNames`).
    fn dynamic_property_names(&self) -> Vec<String> {
        self.object_data().dynamic_property_names()
    }

    /// Event filter handler (`QObject::eventFilter`).
    fn event_filter(&mut self, _watched: ObjectId, _event: &mut Event) -> bool {
        false
    }

    /// Starts a timer and returns a TimerId (`QObject::startTimer`).
    fn start_timer(
        &mut self,
        interval_ms: u64,
        timer_type: crate::timer::TimerType,
    ) -> crate::timer::TimerId {
        let receiver = self.object_data().id();
        let id = crate::timer::start_object_timer(receiver, interval_ms, timer_type);
        self.object_data_mut().add_timer(id);
        id
    }

    /// Kills the timer with the specified timer ID (`QObject::killTimer`).
    fn kill_timer(&mut self, id: crate::timer::TimerId) -> bool {
        self.object_data_mut().remove_timer(id);
        crate::timer::kill_object_timer(id)
    }

    /// Schedules this object for deferred deletion (`QObject::deleteLater`).
    fn delete_later(&self) {
        self.object_data().delete_later()
    }
}

/// Generic extension trait for QObject reflection and hierarchy queries (`QObject::findChild<T>`).
pub trait QObjectExt {
    /// Recursively queries a child object by type `T` and optional `name` (`QObject::findChild<T>`).
    fn find_child<T: 'static>(&self, name: &str) -> Option<&T>;

    /// Recursively queries a mutable child object by type `T` and optional `name` (`QObject::findChild<T>`).
    fn find_child_mut<T: 'static>(&mut self, name: &str) -> Option<&mut T>;

    /// Recursively collects all matching descendant objects by type `T` (`QObject::findChildren<T>`).
    fn find_children<T: 'static>(&self, name: Option<&str>) -> Vec<&T>;
}

impl<O: QObject + ?Sized> QObjectExt for O {
    fn find_child<T: 'static>(&self, name: &str) -> Option<&T> {
        self.find_child_any(name, std::any::TypeId::of::<T>())
            .and_then(|any| any.downcast_ref::<T>())
    }

    fn find_child_mut<T: 'static>(&mut self, name: &str) -> Option<&mut T> {
        self.find_child_any_mut(name, std::any::TypeId::of::<T>())
            .and_then(|any| any.downcast_mut::<T>())
    }

    fn find_children<T: 'static>(&self, name: Option<&str>) -> Vec<&T> {
        self.find_children_any(name, std::any::TypeId::of::<T>())
            .into_iter()
            .filter_map(|any| any.downcast_ref::<T>())
            .collect()
    }
}

/// RAII guard for temporarily blocking signals emitted by a QObject (`QSignalBlocker`).
pub struct SignalBlocker {
    signals_blocked: Arc<AtomicBool>,
    previous_state: bool,
    active: bool,
}

impl SignalBlocker {
    pub fn new(obj: &mut dyn QObject) -> Self {
        Self::from_data(obj.object_data_mut().raw_data_mut())
    }

    pub fn from_data(data: &mut ObjectData) -> Self {
        let signals_blocked = Arc::clone(&data.signals_blocked);
        let previous_state = signals_blocked.swap(true, Ordering::AcqRel);
        Self {
            signals_blocked,
            previous_state,
            active: true,
        }
    }

    pub fn reblock(&mut self) {
        self.signals_blocked.store(true, Ordering::Release);
        self.active = true;
    }

    pub fn unblock(&mut self) {
        self.signals_blocked
            .store(self.previous_state, Ordering::Release);
        self.active = false;
    }
}

impl Drop for SignalBlocker {
    fn drop(&mut self) {
        if self.active {
            self.signals_blocked
                .store(self.previous_state, Ordering::Release);
        }
    }
}

/// Liveness token for a QObject identity. It does not expose references to the QObject.
/// Becomes invalid when the underlying `ObjectData` is dropped.
pub struct QPointer<T: ?Sized> {
    id: ObjectId,
    generation: u32,
    liveness: Arc<AtomicBool>,
    _marker: std::marker::PhantomData<T>,
}

impl<T: ?Sized> QPointer<T> {
    pub fn new<O: QObject>(obj: &O) -> Self {
        let data = obj.object_data();
        Self {
            id: data.id(),
            generation: data.generation,
            liveness: Arc::clone(&data.liveness),
            _marker: std::marker::PhantomData,
        }
    }

    pub fn from_data(data: &ObjectData) -> Self {
        Self {
            id: data.id(),
            generation: data.generation,
            liveness: Arc::clone(&data.liveness),
            _marker: std::marker::PhantomData,
        }
    }

    pub fn null() -> Self {
        Self {
            id: ObjectId(0),
            generation: 0,
            liveness: Arc::new(AtomicBool::new(false)),
            _marker: std::marker::PhantomData,
        }
    }

    #[inline]
    pub fn is_null(&self) -> bool {
        !self.liveness.load(Ordering::Acquire)
    }

    #[inline]
    pub fn is_valid(&self) -> bool {
        !self.is_null()
    }

    #[inline]
    pub fn id(&self) -> Option<ObjectId> {
        if self.is_null() {
            None
        } else {
            Some(self.id)
        }
    }

    pub fn clear(&mut self) {
        self.liveness = Arc::new(AtomicBool::new(false));
    }
}

impl<T: ?Sized> Clone for QPointer<T> {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            generation: self.generation,
            liveness: Arc::clone(&self.liveness),
            _marker: std::marker::PhantomData,
        }
    }
}

impl<T: ?Sized> Default for QPointer<T> {
    fn default() -> Self {
        Self::null()
    }
}

impl<T: ?Sized> fmt::Debug for QPointer<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("QPointer")
            .field("id", &self.id)
            .field("generation", &self.generation)
            .field("is_null", &self.is_null())
            .finish()
    }
}

impl<T: ?Sized> PartialEq for QPointer<T> {
    fn eq(&self, other: &Self) -> bool {
        if self.is_null() && other.is_null() {
            true
        } else if self.is_null() || other.is_null() {
            false
        } else {
            self.id == other.id && self.generation == other.generation
        }
    }
}

impl<T: ?Sized> Eq for QPointer<T> {}

/// Sends an event to a receiver object synchronously (`QCoreApplication::sendEvent`).
pub fn send_event(receiver: ObjectId, event: &mut Event) -> bool {
    crate::event_loop::notify_helper(receiver, event)
}

/// Test-only fixed points inside the hierarchy operations, and switches that bring back defects
/// these operations once had. A test installs a hook on its own thread to hold an operation at a
/// point where another thread can then act; a test child process (see `mutant_run` in the tests)
/// names a defect in its environment to check that the tests notice it.
#[cfg(test)]
pub(crate) mod test_hooks {
    use std::cell::RefCell;
    use std::sync::OnceLock;

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub(crate) enum Point {
        /// After the check that the new parent is not a descendant, before any list is locked
        /// (`set_parent`, `reparent_owned_object`, `swap_hierarchy_and_ownership`). Everything
        /// the operation serialises on is held.
        AfterCycleCheck,
        /// `unregister_qobject`: after the child's parent link has been read and let go, before the
        /// parent's list is locked.
        AfterLinkRead,
        /// `swap_hierarchy_and_ownership` with the `swap_prevalidates_children` defect: between
        /// the two read locks of the lists.
        BetweenChildrenReads,
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

    /// Whether the defect `name` is switched on by the environment of this (child) process, which
    /// lists the defects it wants, separated by commas.
    pub(crate) fn mutant(name: &str) -> bool {
        static ACTIVE: OnceLock<Option<String>> = OnceLock::new();
        ACTIVE
            .get_or_init(|| std::env::var("FERROKIT_TEST_MUTANT").ok())
            .as_deref()
            .is_some_and(|names| names.split(',').any(|listed| listed == name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr::NonNull;
    use std::sync::atomic::AtomicUsize;

    /// Timer event handler (`QObject::timerEvent`).
    #[test]
    fn test_timer_event_dispatch() {
        struct MockWidget {
            data: ObjectData,
            received_timer_id: Option<u64>,
        }

        impl MockWidget {
            fn new(id: ObjectId) -> Self {
                Self {
                    data: ObjectData::new(id),
                    received_timer_id: None,
                }
            }
        }

        impl QObject for MockWidget {
            fn object_data(&self) -> &ObjectData {
                &self.data
            }
            fn object_data_mut(&mut self) -> ObjectDataMut<'_> {
        ObjectDataMut::new(&mut self.data)
    }
            fn timer_event(&mut self, timer_id: u64) {
                self.received_timer_id = Some(timer_id);
            }
        }

        let mut mock = MockWidget::new(ObjectId::next());
        let mut ev = Event::new(EventKind::Timer { timer_id: 99 });
        let handled = mock.event(&mut ev);

        assert!(handled);
        assert_eq!(mock.received_timer_id, Some(99));
    }

    /// Event filter callback (`QObject::eventFilter`).

    #[test]
    fn test_event_filter_interception() {
        struct EventSnooper {
            data: ObjectData,
            intercept: bool,
        }

        impl EventSnooper {
            fn new(id: ObjectId, intercept: bool) -> Self {
                Self {
                    data: ObjectData::new(id),
                    intercept,
                }
            }
        }

        impl QObject for EventSnooper {
            fn object_data(&self) -> &ObjectData {
                &self.data
            }
            fn object_data_mut(&mut self) -> ObjectDataMut<'_> {
        ObjectDataMut::new(&mut self.data)
    }
            fn event_filter(&mut self, _watched: ObjectId, _event: &mut Event) -> bool {
                self.intercept
            }
        }

        struct TargetWidget {
            data: ObjectData,
            event_count: usize,
        }

        impl TargetWidget {
            fn new(id: ObjectId) -> Self {
                Self {
                    data: ObjectData::new(id),
                    event_count: 0,
                }
            }
        }

        impl QObject for TargetWidget {
            fn object_data(&self) -> &ObjectData {
                &self.data
            }
            fn object_data_mut(&mut self) -> ObjectDataMut<'_> {
        ObjectDataMut::new(&mut self.data)
    }
            fn event(&mut self, _event: &mut Event) -> bool {
                self.event_count += 1;
                true
            }
        }

        let mut snooper = EventSnooper::new(ObjectId::next(), true);
        let mut target = TargetWidget::new(ObjectId::next());

        install_event_filter(target.object_data_mut().raw_data_mut(), snooper.object_data().id());

        let mut ev = Event::new(EventKind::UpdateRequest);

        let mut filtered = false;
        for filter_id in target.object_data().event_filters.snapshot() {
            if filter_id == snooper.object_data().id()
                && snooper.event_filter(target.object_data().id(), &mut ev)
            {
                filtered = true;
                break;
            }
        }

        if !filtered {
            target.event(&mut ev);
        }

        assert_eq!(target.event_count, 0);
    }

    /// A parent that is not registered is accepted and only the child's own link is recorded:
    /// the parent's list stays empty. Qt has no such case (a `QObject*` is always a live object),
    /// so `set_parent` specifies nothing more for it; the docs of `set_parent` say so.
    #[test]
    fn test_set_parent_to_an_unregistered_parent_records_only_the_childs_link() {
        let parent = ObjectData::new(ObjectId::next());
        let mut child = ObjectData::new(ObjectId::next());

        set_parent(&mut child, Some(parent.id)).unwrap();
        assert_eq!(child.parent, Some(parent.id));
        assert!(parent.children.is_empty());

        let parent2 = ObjectData::new(ObjectId::next());
        set_parent(&mut child, Some(parent2.id)).unwrap();
        assert_eq!(child.parent, Some(parent2.id));
        assert!(parent.children.is_empty());
        assert!(parent2.children.is_empty());

        set_parent(&mut child, None).unwrap();
        assert_eq!(child.parent, None);
        assert!(parent2.children.is_empty());
    }

    /// Matches `QObject::deleteLater` (qobject.cpp:2511-2565): the first request posts one
    /// stamped `DeferredDelete` to the owning thread and later requests are debounced.
    #[test]
    fn test_delete_later() {
        struct Probe {
            data: ObjectData,
        }
        impl QObject for Probe {
            fn object_data(&self) -> &ObjectData {
                &self.data
            }
            fn object_data_mut(&mut self) -> ObjectDataMut<'_> {
        ObjectDataMut::new(&mut self.data)
    }
        }

        let el = crate::event_loop::EventLoop::new();
        let mut probe = Probe {
            data: ObjectData::new(ObjectId::next()),
        };
        let id = probe.data.id();
        // SAFETY: `probe` stays at this stack address and is unregistered below before it drops.
        unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut probe)) };

        probe.delete_later();
        probe.delete_later();

        let queue = el.queue();
        let queue = queue.lock().unwrap();
        assert_eq!(queue.events.len(), 1);
        assert!(matches!(
            queue.events[0].event.kind,
            EventKind::DeferredDelete {
                loop_level: 0,
                scope_level: 0
            }
        ));
        drop(queue);
        // SAFETY: the registry callback has completed on this thread.
        unsafe { unregister_qobject(id) };
    }

    /// Core object metadata: ObjectData (`QObjectData` equivalent).
    #[test]
    fn test_unsafe_registration_dispatches_on_owner_thread() {
        struct EphemeralWidget {
            data: ObjectData,
            events: Arc<AtomicUsize>,
        }
        impl QObject for EphemeralWidget {
            fn object_data(&self) -> &ObjectData {
                &self.data
            }
            fn object_data_mut(&mut self) -> ObjectDataMut<'_> {
        ObjectDataMut::new(&mut self.data)
    }
            fn custom_event(&mut self, _event: &mut Event) {
                self.events.fetch_add(1, Ordering::SeqCst);
            }
        }

        let id = ObjectId::next();
        let events = Arc::new(AtomicUsize::new(0));
        let mut widget = EphemeralWidget {
            data: ObjectData::new(id),
            events: Arc::clone(&events),
        };
        // SAFETY: widget remains at this stack address and alive until unregistered; this
        // thread performs all accesses and no direct alias is used during registry callbacks.
        unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut widget)) };

        assert!(with_object(id, |_| ()).is_some());
        assert!(with_object_mut(id, |_| ()).is_some());
        let mut ev = Event::new(EventKind::User(Box::new(())));
        assert!(dispatch_to_object(id, &mut ev));
        assert_eq!(events.load(Ordering::SeqCst), 1);
        // SAFETY: the last registry callback has completed on this thread.
        unsafe { unregister_qobject(id) };

        let mut ev = Event::new(EventKind::UpdateRequest);
        assert!(!send_event(id, &mut ev));
    }

    /// A ChildAdded for a borrowed registered parent is posted and delivered after the
    /// borrow ends, not dropped (Qt's setParent_helper sends it through sendEvent,
    /// qobject.cpp:2287-2352).
    #[test]
    fn test_child_added_to_borrowed_parent_is_deferred_not_lost() {
        struct Parent {
            data: ObjectData,
            child_added: Arc<AtomicUsize>,
        }
        impl QObject for Parent {
            fn object_data(&self) -> &ObjectData {
                &self.data
            }
            fn object_data_mut(&mut self) -> ObjectDataMut<'_> {
        ObjectDataMut::new(&mut self.data)
    }
            fn event(&mut self, event: &mut Event) -> bool {
                if matches!(event.kind, EventKind::ChildAdded { .. }) {
                    self.child_added.fetch_add(1, Ordering::SeqCst);
                    return true;
                }
                false
            }
        }

        let el = crate::event_loop::EventLoop::new();
        let child_added = Arc::new(AtomicUsize::new(0));
        let mut parent = Parent {
            data: ObjectData::new(ObjectId::next()),
            child_added: Arc::clone(&child_added),
        };
        let parent_id = parent.data.id();
        // SAFETY: `parent` stays at this stack address and is unregistered below before it drops.
        unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut parent)) };
        let mut child = ObjectData::new(ObjectId::next());

        // The parent's borrow is held while `set_parent` runs, as in a re-entrant callback.
        let during = with_object_mut(parent_id, |_| {
            set_parent(&mut child, Some(parent_id)).unwrap();
            el.send_posted_events()
        });
        assert_eq!(during, Some(0));
        assert_eq!(child_added.load(Ordering::SeqCst), 0);
        assert_eq!(
            el.send_posted_events(),
            1,
            "ChildAdded lost: parent was borrowed during set_parent"
        );
        assert_eq!(child_added.load(Ordering::SeqCst), 1);
        // SAFETY: the last callback has completed on this thread.
        unsafe { unregister_qobject(parent_id) };
    }

    /// A synchronous send to a borrowed receiver returns `false` and is counted, not
    /// silently dropped. Qt has no borrow check here (qcoreapplication.cpp:1563-1572); this is the
    /// documented Rust deviation.
    #[test]
    fn test_sync_send_to_borrowed_receiver_is_rejected_visibly() {
        struct Target {
            data: ObjectData,
            events: Arc<AtomicUsize>,
        }
        impl QObject for Target {
            fn object_data(&self) -> &ObjectData {
                &self.data
            }
            fn object_data_mut(&mut self) -> ObjectDataMut<'_> {
        ObjectDataMut::new(&mut self.data)
    }
            fn event(&mut self, _event: &mut Event) -> bool {
                self.events.fetch_add(1, Ordering::SeqCst);
                true
            }
        }

        let events = Arc::new(AtomicUsize::new(0));
        let mut target = Target {
            data: ObjectData::new(ObjectId::next()),
            events: Arc::clone(&events),
        };
        let id = target.data.id();
        // SAFETY: `target` stays at this stack address and is unregistered below before it drops.
        unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut target)) };

        let rejections_before = crate::event_loop::borrowed_send_rejections();
        let sent = with_object_mut(id, |_| {
            let mut ev = Event::new(EventKind::UpdateRequest);
            send_event(id, &mut ev)
        });
        assert_eq!(
            sent,
            Some(false),
            "a sync send to a borrowed receiver must report failure"
        );
        assert_eq!(
            crate::event_loop::borrowed_send_rejections() - rejections_before,
            1,
            "reentrant send was silently dropped"
        );
        assert_eq!(
            events.load(Ordering::SeqCst),
            0,
            "receiver must not run re-entrantly"
        );
        // SAFETY: the last callback has completed on this thread.
        unsafe { unregister_qobject(id) };
    }

    /// A child destroyed while its registered parent is borrowed posts `ChildRemoved`
    /// instead of dropping it. This is a documented Rust deviation: the borrow forbids the
    /// immediate call, and a deferred delivery keeps the notification.
    #[test]
    fn test_child_removed_on_drop_while_parent_borrowed_is_deferred_not_lost() {
        struct Parent {
            data: ObjectData,
            removed: Arc<AtomicUsize>,
        }
        impl QObject for Parent {
            fn object_data(&self) -> &ObjectData {
                &self.data
            }
            fn object_data_mut(&mut self) -> ObjectDataMut<'_> {
        ObjectDataMut::new(&mut self.data)
    }
            fn event(&mut self, event: &mut Event) -> bool {
                if matches!(event.kind, EventKind::ChildRemoved { .. }) {
                    self.removed.fetch_add(1, Ordering::SeqCst);
                    return true;
                }
                false
            }
        }

        let el = crate::event_loop::EventLoop::new();
        let removed = Arc::new(AtomicUsize::new(0));
        let mut parent = Parent {
            data: ObjectData::new(ObjectId::next()),
            removed: Arc::clone(&removed),
        };
        let parent_id = parent.data.id();
        // SAFETY: `parent` stays at this stack address and is unregistered below before it drops.
        unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut parent)) };
        let mut child = ObjectData::new(ObjectId::next());
        set_parent(&mut child, Some(parent_id)).unwrap();

        with_object_mut(parent_id, move |_| drop(child));
        assert_eq!(removed.load(Ordering::SeqCst), 0);
        assert_eq!(
            el.send_posted_events(),
            1,
            "ChildRemoved lost: child dropped while parent was borrowed"
        );
        assert_eq!(removed.load(Ordering::SeqCst), 1);
        // SAFETY: the last callback has completed on this thread.
        unsafe { unregister_qobject(parent_id) };
    }

    #[test]
    fn test_swap_hierarchy_handles_poisoned_registry_lock() {
        struct Node(ObjectData);
        impl QObject for Node {
            fn object_data(&self) -> &ObjectData { &self.0 }
            fn object_data_mut(&mut self) -> ObjectDataMut<'_> { ObjectDataMut::new(&mut self.0) }
        }

        let mut a = Node(ObjectData::new(ObjectId::next()));
        let mut b = Node(ObjectData::new(ObjectId::next()));
        let a_id = a.0.id();
        let b_id = b.0.id();
        let ptr_a = &raw mut a as *mut dyn QObject;
        register_object_metadata(&a.0, ptr_a);
        let ptr_b = &raw mut b as *mut dyn QObject;
        register_object_metadata(&b.0, ptr_b);

        let record_a = GLOBAL_OBJECT_REGISTRY.read().unwrap().get(&a_id).unwrap().clone();

        // Poison node_a's children lock on a background thread
        let handle = std::thread::spawn({
            let record_a = Arc::clone(&record_a);
            move || {
                let _lock = record_a.children.write().unwrap();
                panic!("intentional lock poison");
            }
        });
        let _ = handle.join();
        assert!(record_a.children.is_poisoned());

        // Attempt swap - must return Err(ReparentError::Busy) without panicking
        let before = states(&[&a.0, &b.0]);
        let res = a.object_data_mut().swap_hierarchy_and_ownership(&mut b.object_data_mut());
        assert_eq!(res, Err(ReparentError::Busy));
        assert_eq!(states(&[&a.0, &b.0]), before, "a refused swap changed something");

        unsafe {
            unregister_qobject(a_id);
            unregister_qobject(b_id);
        }
    }

    #[test]
    fn test_swap_hierarchy_handles_poisoned_ancestor_parent_lock() {
        struct Node(ObjectData);
        impl QObject for Node {
            fn object_data(&self) -> &ObjectData { &self.0 }
            fn object_data_mut(&mut self) -> ObjectDataMut<'_> { ObjectDataMut::new(&mut self.0) }
        }

        let mut ancestor = Node(ObjectData::new(ObjectId::next()));
        let mut a = Node(ObjectData::new(ObjectId::next()));
        let mut b = Node(ObjectData::new(ObjectId::next()));

        let ancestor_id = ancestor.0.id();
        let a_id = a.0.id();
        let b_id = b.0.id();

        let ptr_ancestor = &raw mut ancestor as *mut dyn QObject;
        register_object_metadata(&ancestor.0, ptr_ancestor);
        let ptr_a = &raw mut a as *mut dyn QObject;
        register_object_metadata(&a.0, ptr_a);
        let ptr_b = &raw mut b as *mut dyn QObject;
        register_object_metadata(&b.0, ptr_b);

        // Make ancestor the parent of a in the registry
        let record_a = GLOBAL_OBJECT_REGISTRY.read().unwrap().get(&a_id).unwrap().clone();
        *record_a.parent.write().unwrap() = Some(ancestor_id);
        a.0.parent = Some(ancestor_id);

        let record_ancestor = GLOBAL_OBJECT_REGISTRY.read().unwrap().get(&ancestor_id).unwrap().clone();
        record_ancestor.children.write().unwrap().push(a_id);
        ancestor.0.children.push(a_id);

        // Poison ancestor's parent lock on a background thread
        let handle = std::thread::spawn({
            let record_ancestor = Arc::clone(&record_ancestor);
            move || {
                let _lock = record_ancestor.parent.write().unwrap();
                panic!("intentional ancestor parent lock poison");
            }
        });
        let _ = handle.join();
        assert!(record_ancestor.parent.is_poisoned());

        // Attempt swap between a and b:
        // would_create_cycle(b_id, a_id) will inspect a's parent chain, encountering ancestor's poisoned parent lock.
        // It must return Err(ReparentError::Busy) without panicking!
        let before = states(&[&a.0, &b.0, &ancestor.0]);
        let res = a.object_data_mut().swap_hierarchy_and_ownership(&mut b.object_data_mut());
        assert_eq!(res, Err(ReparentError::Busy));
        assert_eq!(states(&[&a.0, &b.0, &ancestor.0]), before, "a refused swap changed something");

        unsafe {
            unregister_qobject(a_id);
            unregister_qobject(b_id);
            unregister_qobject(ancestor_id);
        }
    }

    #[test]
    fn test_swap_hierarchy_rejects_mismatched_parent_record() {
        struct Node(ObjectData);
        impl QObject for Node {
            fn object_data(&self) -> &ObjectData { &self.0 }
            fn object_data_mut(&mut self) -> ObjectDataMut<'_> { ObjectDataMut::new(&mut self.0) }
        }

        let mut a = Node(ObjectData::new(ObjectId::next()));
        let mut b = Node(ObjectData::new(ObjectId::next()));
        let mut c = Node(ObjectData::new(ObjectId::next()));
        let a_id = a.0.id();
        let b_id = b.0.id();
        let c_id = c.0.id();

        let ptr_a = &raw mut a as *mut dyn QObject;
        register_object_metadata(&a.0, ptr_a);
        let ptr_b = &raw mut b as *mut dyn QObject;
        register_object_metadata(&b.0, ptr_b);
        let ptr_c = &raw mut c as *mut dyn QObject;
        register_object_metadata(&c.0, ptr_c);

        // Manually inject c_id into a's children, but leave c's registry parent as None
        a.0.children.push(c_id);
        {
            let reg = GLOBAL_OBJECT_REGISTRY.read().unwrap();
            let a_rec = reg.get(&a_id).unwrap();
            a_rec.children.write().unwrap().push(c_id);
        }

        // Registry parent of c is None, which mismatches Some(a_id)!
        let before = states(&[&a.0, &b.0, &c.0]);
        let res = a.object_data_mut().swap_hierarchy_and_ownership(&mut b.object_data_mut());
        assert_eq!(res, Err(ReparentError::InvalidParent));
        assert_eq!(states(&[&a.0, &b.0, &c.0]), before, "a refused swap changed something");

        // Cleanup
        a.0.children.retain(|&id| id != c_id);
        unsafe {
            unregister_qobject(c_id);
            unregister_qobject(b_id);
            unregister_qobject(a_id);
        }
    }

    /// The lists the two objects keep are compared with the registry's under the write locks of the
    /// two parents: a registry list that names a child the object's own list lacks is refused, and
    /// nothing changes (no list is written over).
    #[test]
    fn test_swap_refuses_a_registry_list_that_disagrees_with_the_objects_own() {
        // SAFETY: the objects are owned here and dropped at the end, on this thread.
        let mut a = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
        let mut b = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
        let (a_id, b_id) = (OwnedObject::object_id(&a), OwnedObject::object_id(&b));
        let listed_only_in_the_registry = ObjectId::next();
        record(a_id).children.write().unwrap().push(listed_only_in_the_registry);
        let before = states(&[&a.0, &b.0]);

        let res = a.object_data_mut().swap_hierarchy_and_ownership(&mut b.object_data_mut());

        assert_eq!(res, Err(ReparentError::InvalidParent));
        assert_eq!(states(&[&a.0, &b.0]), before, "a refused swap changed something");
        assert_eq!(query_object_children(a_id), Some(vec![listed_only_in_the_registry]));
        assert_eq!(query_object_children(b_id), Some(Vec::new()));
        record(a_id).children.write().unwrap().clear();
    }

    #[test]
    fn test_set_parent_handles_poisoned_parent_lock() {
        struct Node(ObjectData);
        impl QObject for Node {
            fn object_data(&self) -> &ObjectData { &self.0 }
            fn object_data_mut(&mut self) -> ObjectDataMut<'_> { ObjectDataMut::new(&mut self.0) }
        }

        let mut parent = Node(ObjectData::new(ObjectId::next()));
        let mut child = Node(ObjectData::new(ObjectId::next()));
        let parent_id = parent.0.id();
        let child_id = child.0.id();

        let ptr_p = &raw mut parent as *mut dyn QObject;
        register_object_metadata(&parent.0, ptr_p);
        let ptr_c = &raw mut child as *mut dyn QObject;
        register_object_metadata(&child.0, ptr_c);

        let record_p = GLOBAL_OBJECT_REGISTRY.read().unwrap().get(&parent_id).unwrap().clone();
        let handle = std::thread::spawn({
            let record_p = Arc::clone(&record_p);
            move || {
                let _lock = record_p.children.write().unwrap();
                panic!("poison parent children lock");
            }
        });
        let _ = handle.join();
        assert!(record_p.children.is_poisoned());

        // Attempt set_parent - must fail with Busy, child must stay parentless
        let before = states(&[&child.0, &parent.0]);
        let res = set_parent(&mut child.0, Some(parent_id));
        assert_eq!(res, Err(ReparentError::Busy));
        assert_eq!(states(&[&child.0, &parent.0]), before, "a refused set_parent changed something");
        assert_eq!(child.0.parent(), None);

        unsafe {
            unregister_qobject(child_id);
            unregister_qobject(parent_id);
        }
    }

    #[test]
    fn test_reparent_owned_handles_poisoned_parent_lock() {
        struct Node(ObjectData);
        impl QObject for Node {
            fn object_data(&self) -> &ObjectData { &self.0 }
            fn object_data_mut(&mut self) -> ObjectDataMut<'_> { ObjectDataMut::new(&mut self.0) }
        }

        let mut p1 = Node(ObjectData::new(ObjectId::next()));
        let mut p2 = Node(ObjectData::new(ObjectId::next()));
        let p1_id = p1.0.id();
        let p2_id = p2.0.id();

        let ptr_p1 = &raw mut p1 as *mut dyn QObject;
        register_object_metadata(&p1.0, ptr_p1);
        let ptr_p2 = &raw mut p2 as *mut dyn QObject;
        register_object_metadata(&p2.0, ptr_p2);

        let child_box = Box::new(Node(ObjectData::new(ObjectId::next())));
        let child_id = unsafe { add_owned_child(p1_id, child_box).map_err(|(e, _)| e).unwrap() };
        // Poison p2's children lock
        let record_p2 = GLOBAL_OBJECT_REGISTRY.read().unwrap().get(&p2_id).unwrap().clone();
        let handle = std::thread::spawn({
            let record_p2 = Arc::clone(&record_p2);
            move || {
                let _lock = record_p2.children.write().unwrap();
                panic!("poison p2 children lock");
            }
        });
        let _ = handle.join();
        assert!(record_p2.children.is_poisoned());

        // Attempt reparent_owned_object to p2 - must fail with Busy, p1 must still own child
        let before = states(&[&p1.0, &p2.0, p1.0.owned_children[0].object_data()]);
        let res = reparent_owned_object(child_id, Some(p2_id));
        assert!(matches!(res, Err(ReparentError::Busy)));
        assert_eq!(
            states(&[&p1.0, &p2.0, p1.0.owned_children[0].object_data()]),
            before,
            "a refused reparent changed something"
        );
        assert_eq!(query_object_parent(child_id), Some(Some(p1_id)));
        assert!(p1.0.owned_children.iter().any(|c| OwnedObject::object_id(c) == child_id));

        unsafe {
            unregister_qobject(p2_id);
            unregister_qobject(p1_id);
        }
    }

    #[test]
    fn test_reparent_owned_rejects_when_child_borrowed() {
        struct Node(ObjectData);
        impl QObject for Node {
            fn object_data(&self) -> &ObjectData { &self.0 }
            fn object_data_mut(&mut self) -> ObjectDataMut<'_> { ObjectDataMut::new(&mut self.0) }
        }

        let mut p1 = Node(ObjectData::new(ObjectId::next()));
        let mut p2 = Node(ObjectData::new(ObjectId::next()));
        let p1_id = p1.0.id();
        let p2_id = p2.0.id();

        let ptr_p1 = &raw mut p1 as *mut dyn QObject;
        register_object_metadata(&p1.0, ptr_p1);
        let ptr_p2 = &raw mut p2 as *mut dyn QObject;
        register_object_metadata(&p2.0, ptr_p2);

        let child_box = Box::new(Node(ObjectData::new(ObjectId::next())));
        let child_id = unsafe { add_owned_child(p1_id, child_box).map_err(|(e, _)| e).unwrap() };
        // Borrow child exclusively via borrow_registered
        let child_guard = borrow_registered(child_id).unwrap();

        // Attempt reparent - must reject with Busy
        let before = states(&[&p1.0, &p2.0, p1.0.owned_children[0].object_data()]);
        let res = reparent_owned_object(child_id, Some(p2_id));
        assert!(matches!(res, Err(ReparentError::Busy)));
        assert_eq!(
            states(&[&p1.0, &p2.0, p1.0.owned_children[0].object_data()]),
            before,
            "a refused reparent changed something"
        );
        drop(child_guard);

        assert_eq!(query_object_parent(child_id), Some(Some(p1_id)));
        assert!(p1.0.owned_children.iter().any(|c| OwnedObject::object_id(c) == child_id));

        unsafe {
            unregister_qobject(p2_id);
            unregister_qobject(p1_id);
        }
    }

    use std::cell::Cell;
    use std::rc::Rc;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    struct Plain(ObjectData);

    impl QObject for Plain {
        fn object_data(&self) -> &ObjectData {
            &self.0
        }
        fn object_data_mut(&mut self) -> ObjectDataMut<'_> {
            ObjectDataMut::new(&mut self.0)
        }
    }

    /// A child event that a [`Handler`] got, with what the receiver's own `children()` and the
    /// registry's parent of the child were at that moment.
    #[derive(Debug, Clone, PartialEq)]
    struct Delivered {
        receiver: ObjectId,
        what: &'static str,
        child: ObjectId,
        own_children: Vec<ObjectId>,
        registry_parent: Option<Option<ObjectId>>,
    }

    type Log = Rc<RefCell<Vec<Delivered>>>;
    type OnChildEvent = Box<dyn FnMut(&Delivered)>;

    /// Object that logs the child events it gets and runs a hook on each, to see what a handler can
    /// do and see while a hierarchy change is being finished.
    struct Handler {
        data: ObjectData,
        log: Log,
        on_event: Option<OnChildEvent>,
    }

    impl Handler {
        fn new(log: &Log, on_event: Option<OnChildEvent>) -> Self {
            Self { data: ObjectData::new(ObjectId::next()), log: Rc::clone(log), on_event }
        }
    }

    impl QObject for Handler {
        fn object_data(&self) -> &ObjectData {
            &self.data
        }
        fn object_data_mut(&mut self) -> ObjectDataMut<'_> {
            ObjectDataMut::new(&mut self.data)
        }
        fn event(&mut self, event: &mut Event) -> bool {
            let (what, child) = match &event.kind {
                EventKind::ChildAdded { child_id } => ("added", *child_id),
                EventKind::ChildRemoved { child_id } => ("removed", *child_id),
                _ => return true,
            };
            let delivered = Delivered {
                receiver: self.data.id(),
                what,
                child,
                own_children: self.data.children().to_vec(),
                registry_parent: query_object_parent(child),
            };
            self.log.borrow_mut().push(delivered.clone());
            if let Some(hook) = self.on_event.as_mut() {
                hook(&delivered);
            }
            true
        }
    }

    /// Tests that hold the registry or `HIERARCHY_MUTATION` for a while, or probe them, take this
    /// one, so that they do not meet.
    fn serial() -> MutexGuard<'static, ()> {
        static SERIAL: Mutex<()> = Mutex::new(());
        lock_unpoisoned(&SERIAL)
    }

    const PROBE_WINDOW: Duration = Duration::from_secs(5);

    /// How long a test lets a thread that must be blocked try to get on. Only a defective version
    /// gets on; the outcome that is checked afterwards does not depend on this time.
    fn settle() -> Duration {
        Duration::from_millis(if cfg!(miri) { 2000 } else { 300 })
    }

    /// Whether `probe` succeeds once within [`PROBE_WINDOW`]. Another test may hold a global lock
    /// for an instant, so a failed probe is tried again; a lock that the calling thread holds fails
    /// it for the whole window.
    fn free_within(mut probe: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + PROBE_WINDOW;
        loop {
            if probe() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn record(id: ObjectId) -> Arc<ObjectRecord> {
        Arc::clone(&GLOBAL_OBJECT_REGISTRY.read().unwrap()[&id])
    }

    /// None of the locks of a hierarchy change is held by the calling thread (a handler runs on
    /// it): the lists and links of the given records, the registry, and `HIERARCHY_MUTATION`.
    fn assert_hierarchy_unlocked(records: &[Arc<ObjectRecord>]) {
        for record in records {
            assert!(record.children.try_write().is_ok(), "the `children` lock of {:?} is held", record.id);
            assert!(record.parent.try_write().is_ok(), "the `parent` lock of {:?} is held", record.id);
        }
        assert!(free_within(|| GLOBAL_OBJECT_REGISTRY.try_write().is_ok()), "the registry is held");
        assert!(free_within(|| HIERARCHY_MUTATION.try_lock().is_ok()), "`HIERARCHY_MUTATION` is held");
    }

    /// What a hierarchy change that fails must leave as it was, for each object: its own links,
    /// lists, ownership, and what the registry records for it with its borrow flag.
    #[derive(Debug, PartialEq)]
    struct State {
        id: ObjectId,
        own: (Option<ObjectId>, Option<ObjectId>, Vec<ObjectId>, Vec<ObjectId>, bool),
        registry: Option<(Option<ObjectId>, Vec<ObjectId>, bool, bool)>,
    }

    fn states(objects: &[&ObjectData]) -> Vec<State> {
        let registry = GLOBAL_OBJECT_REGISTRY.read().unwrap();
        objects
            .iter()
            .map(|data| State {
                id: data.id,
                own: (
                    data.parent,
                    data.parent_liveness.as_ref().map(|(id, _)| *id),
                    data.children.clone(),
                    data.owned_children.iter().map(OwnedObject::object_id).collect(),
                    data.owned_by_parent,
                ),
                registry: registry.get(&data.id).map(|r| {
                    // The list (L1) before the link (L2), one lock at a time, like the commits.
                    let children = read_unpoisoned(&r.children).clone();
                    let parent = *read_unpoisoned(&r.parent);
                    (
                        parent,
                        children,
                        r.borrow_state.borrow_flag.load(Ordering::Acquire),
                        r.liveness.load(Ordering::Acquire),
                    )
                }),
            })
            .collect()
    }

    /// No object among `ids` is its own ancestor in the registry.
    fn registry_has_no_cycle(ids: &[ObjectId]) -> bool {
        ids.iter().all(|&start| {
            let mut cursor = query_object_parent(start).flatten();
            for _ in 0..64 {
                match cursor {
                    None => return true,
                    Some(id) if id == start => return false,
                    Some(id) => cursor = query_object_parent(id).flatten(),
                }
            }
            false
        })
    }

    /// Poisons a lock of `record` by panicking on another thread while holding it for writing.
    fn poison(record: &Arc<ObjectRecord>, hold: fn(&ObjectRecord)) {
        let record = Arc::clone(record);
        let _ = std::thread::spawn(move || hold(&record)).join();
    }

    /// A thread that creates what it works on (`setup` runs on it, so its objects belong to it),
    /// reports that it is ready, waits for `start`, runs `act` once and sends the result, and keeps
    /// what it created alive until `finish`. Dropping a `Side` lets its thread go on and end.
    struct Side<R> {
        ready: mpsc::Receiver<()>,
        go: mpsc::Sender<()>,
        result: mpsc::Receiver<R>,
        end: mpsc::Sender<()>,
        handle: std::thread::JoinHandle<()>,
    }

    fn side<S: 'static, R: Send + 'static>(
        setup: impl FnOnce() -> S + Send + 'static,
        act: impl FnOnce(&mut S) -> R + Send + 'static,
    ) -> Side<R> {
        let (ready_tx, ready) = mpsc::channel();
        let (go, go_rx) = mpsc::channel();
        let (result_tx, result) = mpsc::channel();
        let (end, end_rx) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            let mut state = setup();
            let _ = ready_tx.send(());
            if go_rx.recv().is_ok() {
                let _ = result_tx.send(act(&mut state));
            }
            let _ = end_rx.recv();
            drop(state);
        });
        Side { ready, go, result, end, handle }
    }

    impl<R> Side<R> {
        fn wait_ready(&self) {
            self.ready.recv().expect("a thread did not get ready");
        }
        fn start(&self) {
            let _ = self.go.send(());
        }
        /// Ends the thread, whether it was started or not, and waits for it.
        fn finish(self) {
            let Side { go, end, handle, .. } = self;
            drop(go);
            let _ = end.send(());
            handle.join().expect("a thread panicked");
        }
    }

    /// Holds the next hierarchy operation of the calling thread at the first of `points` it
    /// reaches: `reached` is signalled, and the operation goes on when `resume` gets a signal or
    /// its sender is dropped.
    fn hold_next_operation(
        reached: mpsc::Sender<()>,
        resume: mpsc::Receiver<()>,
        points: &[test_hooks::Point],
    ) {
        let resume = Rc::new(resume);
        let armed = Rc::new(Cell::new(true));
        for &point in points {
            let reached = reached.clone();
            let resume = Rc::clone(&resume);
            let armed = Rc::clone(&armed);
            test_hooks::on_next(point, move || {
                if armed.replace(false) {
                    let _ = reached.send(());
                    let _ = resume.recv();
                }
            });
        }
    }

    /// The child process a test uses to check that it notices a defect (see `test_hooks::mutant`).
    #[derive(Debug, PartialEq)]
    enum ChildRun {
        Passed,
        Failed,
        /// Killed after `limit`: a deadlock ends here.
        TimedOut,
    }

    /// Runs the test `name` of this binary again in a child process with the defects `mutants`
    /// (comma separated) switched on, and reports how it ended. The child is killed when it has
    /// not ended within `limit`.
    fn run_in_child(name: &str, mutants: &str, limit: Duration) -> ChildRun {
        use std::io::Read;
        use std::process::{Command, Stdio};
        let module = module_path!().split_once("::").map_or(module_path!(), |(_, rest)| rest);
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &format!("{module}::{name}"), "--test-threads=1"])
            .env("FERROKIT_TEST_MUTANT", mutants)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let reader = std::thread::spawn(move || {
            let mut text = String::new();
            let _ = stdout.read_to_string(&mut text);
            text
        });
        let deadline = Instant::now() + limit;
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break Some(status);
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let text = reader.join().unwrap();
        match status {
            None => ChildRun::TimedOut,
            Some(status) if status.success() && text.contains("1 passed") => ChildRun::Passed,
            Some(_) if text.contains("1 failed") => ChildRun::Failed,
            Some(status) => panic!("the child did not run the test `{name}` ({status}): {text}"),
        }
    }

    /// Backstop for a child that deadlocks.
    const CHILD_LIMIT: Duration = Duration::from_secs(90);

    fn noticed(run: ChildRun) -> bool {
        run != ChildRun::Passed
    }

    /// A handler of `ChildAdded` runs with no lock of the change held, and can register and drop
    /// objects and change other links while `set_parent` delivers the event.
    #[test]
    fn test_set_parent_delivers_events_with_no_hierarchy_lock_held() {
        let _serial = serial();
        let log = Log::default();
        let records: Rc<RefCell<Vec<Arc<ObjectRecord>>>> = Rc::default();
        let ran = Rc::new(Cell::new(0usize));
        let hook: OnChildEvent = {
            let records = Rc::clone(&records);
            let ran = Rc::clone(&ran);
            Box::new(move |_| {
                assert_hierarchy_unlocked(&records.borrow());
                // SAFETY: the extra object is owned and dropped right here, on this thread.
                let extra = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
                let mut other = ObjectData::new(ObjectId::next());
                assert_eq!(set_parent(&mut other, Some(OwnedObject::object_id(&extra))), Ok(()));
                drop(other);
                drop(extra);
                ran.set(ran.get() + 1);
            })
        };
        // SAFETY: the parent is owned here and dropped at the end, on this thread.
        let parent = unsafe { register_owned_qobject(Handler::new(&log, Some(hook))) };
        let parent_id = OwnedObject::object_id(&parent);
        records.borrow_mut().push(record(parent_id));

        let mut child = ObjectData::new(ObjectId::next());
        assert_eq!(set_parent(&mut child, Some(parent_id)), Ok(()));
        assert_eq!(ran.get(), 1);
        assert_eq!(
            *log.borrow(),
            vec![Delivered {
                receiver: parent_id,
                what: "added",
                child: child.id(),
                own_children: vec![child.id()],
                registry_parent: None,
            }]
        );
        assert_eq!(child.parent(), Some(parent_id));
        assert_eq!(query_object_children(parent_id), Some(vec![child.id()]));
        drop(child);
        drop(parent);
    }

    /// A registered child moves between two parents. Both handlers run with every lock of the
    /// change free, including the lists and links of all three records, and see the registry as
    /// the finished change: `ChildRemoved` goes to the old parent, whose own list has lost the
    /// child; `ChildAdded` to the new one, whose own list has it.
    #[test]
    fn test_set_parent_of_a_registered_child_delivers_events_in_order_with_no_lock_held() {
        let _serial = serial();
        let log = Log::default();
        let records: Rc<RefCell<Vec<Arc<ObjectRecord>>>> = Rc::default();
        let probe = || -> OnChildEvent {
            let records = Rc::clone(&records);
            Box::new(move |_| assert_hierarchy_unlocked(&records.borrow()))
        };
        // SAFETY: the objects are owned here and dropped at the end, on this thread.
        let p1 = unsafe { register_owned_qobject(Handler::new(&log, Some(probe()))) };
        let p2 = unsafe { register_owned_qobject(Handler::new(&log, Some(probe()))) };
        let mut child = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
        let (p1_id, p2_id, child_id) =
            (OwnedObject::object_id(&p1), OwnedObject::object_id(&p2), OwnedObject::object_id(&child));
        records.borrow_mut().extend([record(p1_id), record(p2_id), record(child_id)]);

        assert_eq!(set_parent(&mut child.0, Some(p1_id)), Ok(()));
        assert_eq!(set_parent(&mut child.0, Some(p2_id)), Ok(()));

        let delivered = |receiver, what, own_children, parent| Delivered {
            receiver,
            what,
            child: child_id,
            own_children,
            registry_parent: Some(Some(parent)),
        };
        assert_eq!(
            *log.borrow(),
            vec![
                delivered(p1_id, "added", vec![child_id], p1_id),
                delivered(p1_id, "removed", vec![], p2_id),
                delivered(p2_id, "added", vec![child_id], p2_id),
            ]
        );
        assert_eq!(child.0.parent(), Some(p2_id));
        assert_eq!(query_object_parent(child_id), Some(Some(p2_id)));
        assert_eq!(query_object_children(p1_id), Some(Vec::new()));
        assert_eq!(query_object_children(p2_id), Some(vec![child_id]));
        drop(child);
        drop(p2);
        drop(p1);
    }

    #[test]
    fn test_set_parent_rejects_reentrant_borrow_during_callbacks() {
        let _serial = serial();
        let log = Log::default();
        let attempted_reborrow_success = Rc::new(Cell::new(false));
        let attempted_clone = Rc::clone(&attempted_reborrow_success);
        let child_id_cell = Rc::new(Cell::new(ObjectId(0)));
        let child_id_clone = Rc::clone(&child_id_cell);

        let probe: OnChildEvent = Box::new(move |delivered| {
            if delivered.what == "removed" {
                let cid = child_id_clone.get();
                // Attempting to re-acquire mutable access to child via the registry during ChildRemoved
                // must be rejected because short_term_borrow is held across the callback.
                if with_object_mut(cid, |_| ()).is_some() {
                    attempted_clone.set(true);
                }
            }
        });

        let p1 = unsafe { register_owned_qobject(Handler::new(&log, Some(probe))) };
        let p2 = unsafe { register_owned_qobject(Handler::new(&log, None)) };
        let mut child = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
        let (p1_id, p2_id, child_id) =
            (OwnedObject::object_id(&p1), OwnedObject::object_id(&p2), OwnedObject::object_id(&child));
        child_id_cell.set(child_id);

        assert_eq!(set_parent(&mut child.0, Some(p1_id)), Ok(()));
        assert_eq!(set_parent(&mut child.0, Some(p2_id)), Ok(()));

        assert!(!attempted_reborrow_success.get(), "reentrant mutable borrow via registry must be rejected during ChildRemoved callback");
        drop(child);
        drop(p2);
        drop(p1);
    }

    /// A panic in the old parent's `ChildRemoved` handler does not leave the objects behind the
    /// registry: the child's parent and the new parent's list are brought in line first, `ChildAdded`
    /// is not sent, and the panic then continues. Nothing stays locked or borrowed.
    #[test]
    fn test_set_parent_finishes_the_change_when_the_old_parents_handler_panics() {
        let _serial = serial();
        let log = Log::default();
        let panicking: OnChildEvent = Box::new(|delivered| {
            if delivered.what == "removed" {
                panic!("handler of the old parent panics");
            }
        });
        // SAFETY: the objects are owned here and dropped at the end, on this thread.
        let p1 = unsafe { register_owned_qobject(Handler::new(&log, Some(panicking))) };
        let p2 = unsafe { register_owned_qobject(Handler::new(&log, None)) };
        let mut child = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
        let (p1_id, p2_id, child_id) =
            (OwnedObject::object_id(&p1), OwnedObject::object_id(&p2), OwnedObject::object_id(&child));
        assert_eq!(set_parent(&mut child.0, Some(p1_id)), Ok(()));

        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            set_parent(&mut child.0, Some(p2_id))
        }));
        let payload = outcome.expect_err("the panic of the handler continues after the change is finished");
        assert_eq!(payload.downcast_ref::<&str>(), Some(&"handler of the old parent panics"));

        assert_eq!(child.0.parent(), Some(p2_id));
        assert_eq!(query_object_parent(child_id), Some(Some(p2_id)));
        assert_eq!(query_object_children(p1_id), Some(Vec::new()));
        assert_eq!(query_object_children(p2_id), Some(vec![child_id]));
        assert_eq!(with_object(p2_id, |p| p.object_data().children().to_vec()), Some(vec![child_id]));
        assert!(!log.borrow().iter().any(|d| d.receiver == p2_id), "`ChildAdded` was sent after a panic");

        let records = [record(p1_id), record(p2_id), record(child_id)];
        assert_hierarchy_unlocked(&records);
        for record in &records {
            assert!(!record.borrow_state.borrow_flag.load(Ordering::Acquire), "{:?} stays borrowed", record.id);
        }
        assert_eq!(set_parent(&mut child.0, None), Ok(()), "the child cannot be moved again");
        drop(child);
        drop(p2);
        drop(p1);
    }
    /// When the old parent's ChildRemoved handler panics during set_parent_by_id,
    /// the child's local parent and the new parent's children list are still brought
    /// in line with the registry before the panic resumes, and ChildAdded is suppressed.
    #[test]
    fn test_set_parent_by_id_finishes_the_change_when_the_old_parents_handler_panics() {
        let _serial = serial();
        let log = Log::default();
        let panicking: OnChildEvent = Box::new(|delivered| {
            if delivered.what == "removed" {
                panic!("handler of the old parent panics during set_parent_by_id");
            }
        });
        let p1 = unsafe { register_owned_qobject(Handler::new(&log, Some(panicking))) };
        let p2 = unsafe { register_owned_qobject(Handler::new(&log, None)) };
        let child = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
        let (p1_id, p2_id, child_id) =
            (OwnedObject::object_id(&p1), OwnedObject::object_id(&p2), OwnedObject::object_id(&child));
        assert_eq!(set_parent_by_id(child_id, Some(p1_id)), Ok(()));

        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            set_parent_by_id(child_id, Some(p2_id))
        }));
        let payload = outcome.expect_err("the panic of the handler must resume after the change is finished");
        assert_eq!(
            payload.downcast_ref::<&str>(),
            Some(&"handler of the old parent panics during set_parent_by_id")
        );

        // Strict consistency checks:
        assert_eq!(query_object_parent(child_id), Some(Some(p2_id)));
        assert_eq!(query_object_children(p1_id), Some(Vec::new()));
        assert_eq!(query_object_children(p2_id), Some(vec![child_id]));

        assert_eq!(with_object(child_id, |c| c.object_data().parent()), Some(Some(p2_id)));
        assert_eq!(with_object(p1_id, |p| p.object_data().children().to_vec()), Some(Vec::new()));
        assert_eq!(with_object(p2_id, |p| p.object_data().children().to_vec()), Some(vec![child_id]));
        assert!(!log.borrow().iter().any(|d| d.receiver == p2_id), "`ChildAdded` was sent after a panic");

        let records = [record(p1_id), record(p2_id), record(child_id)];
        assert_hierarchy_unlocked(&records);
        for record in &records {
            assert!(!record.borrow_state.borrow_flag.load(Ordering::Acquire), "{:?} stays borrowed", record.id);
            assert!(!record.reparenting.load(Ordering::Acquire), "{:?} stays reparenting", record.id);
        }

        drop(child);
        drop(p2);
        drop(p1);
    }
    /// When a registered child is reparented, the old parent's `ChildRemoved` handler can
    /// reentrantly access or mutate the child via `with_object_mut`. All locks and mutable
    /// borrows of the child must be released before callbacks execute, and the final state
    /// of the registry, child's parent link, and parents' children lists must remain consistent.
    #[test]
    fn test_set_parent_reentrant_mutation_during_child_removed() {
        let _serial = serial();
        let log = Log::default();
        let reentrant_mutated = Rc::new(Cell::new(false));
        let reentrant_mutated_clone = Rc::clone(&reentrant_mutated);
        let nested_busy_rejected = Rc::new(Cell::new(false));
        let nested_busy_rejected_clone = Rc::clone(&nested_busy_rejected);
        let p3_id_cell = Rc::new(Cell::new(None));
        let p3_id_clone = Rc::clone(&p3_id_cell);

        // The hook runs during `ChildRemoved` on the old parent.
        let hook: OnChildEvent = Box::new(move |delivered| {
            if delivered.what == "removed" {
                let cid = delivered.child;
                if let Some(target_p3) = p3_id_clone.get() {
                    let res = set_parent_by_id(cid, Some(target_p3));
                    assert_eq!(res, Err(ReparentError::Busy));
                    nested_busy_rejected_clone.set(true);
                }
                // Verify with_object_mut succeeds reentrantly without aliasing or deadlock.
                let mutated = with_object_mut(cid, |c| {
                    let mut data = c.object_data_mut();
                    data.set_object_name("reentrantly_mutated_child");
                    true
                });
                if mutated == Some(true) {
                    reentrant_mutated_clone.set(true);
                }
            }
        });

        // SAFETY: registered on current thread and owned.
        let p1 = unsafe { register_owned_qobject(Handler::new(&log, Some(hook))) };
        let p2 = unsafe { register_owned_qobject(Handler::new(&log, None)) };
        let p3 = unsafe { register_owned_qobject(Handler::new(&log, None)) };
        let child = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
        let (p1_id, p2_id, p3_id, child_id) = (
            OwnedObject::object_id(&p1),
            OwnedObject::object_id(&p2),
            OwnedObject::object_id(&p3),
            OwnedObject::object_id(&child),
        );
        p3_id_cell.set(Some(p3_id));

        assert_eq!(set_parent_by_id(child_id, Some(p1_id)), Ok(()));
        assert_eq!(set_parent_by_id(child_id, Some(p2_id)), Ok(()));
        assert!(nested_busy_rejected.get(), "nested set_parent_by_id must be rejected with Busy");
        assert!(reentrant_mutated.get(), "with_object_mut must run during ChildRemoved");

        // Assert that final registry, parent, and children states remain consistent.
        assert_eq!(query_object_parent(child_id), Some(Some(p2_id)));
        assert_eq!(query_object_children(p1_id), Some(Vec::new()));
        assert_eq!(query_object_children(p2_id), Some(vec![child_id]));
        assert_eq!(query_object_children(p3_id), Some(Vec::new()));

        // Assert child local state through with_object and through owned object
        assert_eq!(with_object(child_id, |c| c.object_data().parent()), Some(Some(p2_id)));
        assert_eq!(with_object(child_id, |c| c.object_data().object_name().map(|s| s.to_string())), Some(Some("reentrantly_mutated_child".to_string())));
        assert_eq!(with_object(p1_id, |p| p.object_data().children().to_vec()), Some(Vec::new()));
        assert_eq!(with_object(p2_id, |p| p.object_data().children().to_vec()), Some(vec![child_id]));
        assert_eq!(with_object(p3_id, |p| p.object_data().children().to_vec()), Some(Vec::new()));

        drop(child);
        drop(p3);
        drop(p2);
        drop(p1);
    }

    /// When set_parent_by_id completes its registry and hierarchy commit while the child object
    /// is actively borrowed by a callback that retained a borrow guard across the event,
    /// post-commit sync must NOT return Busy or drop the update silently. Instead, the sync
    /// is recorded as pending and deterministically applied when that borrow guard drops.
    #[test]
    fn test_set_parent_by_id_post_commit_sync_deferred_when_child_borrow_held() {
        let _serial = serial();
        let log = Log::default();
        let held_guard: Rc<RefCell<Option<ObjectBorrowGuard>>> = Rc::new(RefCell::new(None));
        let held_guard_clone = Rc::clone(&held_guard);

        let hook: OnChildEvent = Box::new(move |delivered| {
            if delivered.what == "removed" {
                // Borrow the child during ChildRemoved and retain the guard
                let guard = borrow_registered(delivered.child).expect("child should be borrowable during callback");
                *held_guard_clone.borrow_mut() = Some(guard);
            }
        });

        let p1 = unsafe { register_owned_qobject(Handler::new(&log, Some(hook))) };
        let p2 = unsafe { register_owned_qobject(Handler::new(&log, None)) };
        let child = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
        let (p1_id, p2_id, child_id) = (
            OwnedObject::object_id(&p1),
            OwnedObject::object_id(&p2),
            OwnedObject::object_id(&child),
        );

        assert_eq!(set_parent_by_id(child_id, Some(p1_id)), Ok(()));
        assert_eq!(query_object_parent(child_id), Some(Some(p1_id)));
        assert_eq!(query_pending_parent_sync(child_id), Some(false));

        // Now reparent to p2. ChildRemoved on p1 will hold child's borrow guard across return.
        assert_eq!(set_parent_by_id(child_id, Some(p2_id)), Ok(()));

        // Registry and hierarchy committed successfully
        assert_eq!(query_object_parent(child_id), Some(Some(p2_id)));
        assert_eq!(query_object_children(p1_id), Some(Vec::new()));
        assert_eq!(query_object_children(p2_id), Some(vec![child_id]));

        // Post-commit sync could not directly borrow child, so it was deferred
        assert_eq!(query_pending_parent_sync(child_id), Some(true));

        // Release the held borrow guard: ObjectBorrowGuard::drop must apply the pending sync!
        drop(held_guard.borrow_mut().take());

        // Now pending parent sync has been applied
        assert_eq!(query_pending_parent_sync(child_id), Some(false));
        assert_eq!(with_object(child_id, |c| c.object_data().parent()), Some(Some(p2_id)));

        drop(child);
        drop(p2);
        drop(p1);
    }
    /// When a ChildRemoved handler retains an ObjectBorrowGuard on the child and panics,
    /// sync_child_local_parent records PostCommitSyncStatus::DeferredToPending before resume_unwind.
    /// Prior to resume_unwind, registry and hierarchy lists are fully repaired and ChildAdded
    /// is suppressed, while local parent synchronization remains pending and completes deterministically
    /// upon release of that ObjectBorrowGuard.
    #[test]
    fn test_set_parent_by_id_panic_with_retained_child_borrow_defers_local_sync_until_guard_drop() {
        let _serial = serial();
        let log = Log::default();
        let held_guard: Rc<RefCell<Option<ObjectBorrowGuard>>> = Rc::new(RefCell::new(None));
        let held_guard_clone = Rc::clone(&held_guard);

        let hook: OnChildEvent = Box::new(move |delivered| {
            if delivered.what == "removed" {
                // Retain child borrow externally
                let guard = borrow_registered(delivered.child).expect("child should be borrowable during callback");
                *held_guard_clone.borrow_mut() = Some(guard);
                // Then panic in the handler
                panic!("handler panics after retaining child borrow");
            }
        });

        let p1 = unsafe { register_owned_qobject(Handler::new(&log, Some(hook))) };
        let p2 = unsafe { register_owned_qobject(Handler::new(&log, None)) };
        let child = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
        let (p1_id, p2_id, child_id) = (
            OwnedObject::object_id(&p1),
            OwnedObject::object_id(&p2),
            OwnedObject::object_id(&child),
        );

        assert_eq!(set_parent_by_id(child_id, Some(p1_id)), Ok(()));
        assert_eq!(query_object_parent(child_id), Some(Some(p1_id)));
        assert_eq!(query_pending_parent_sync(child_id), Some(false));

        // Invoke set_parent_by_id, which must panic and propagate resume_unwind
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            set_parent_by_id(child_id, Some(p2_id))
        }));
        let payload = outcome.expect_err("handler panic must resume unwind");
        assert_eq!(
            payload.downcast_ref::<&str>(),
            Some(&"handler panics after retaining child borrow")
        );

        // Verification PRIOR to dropping held_guard:
        // 1. Global registry and hierarchy lists are repaired and consistent:
        assert_eq!(query_object_parent(child_id), Some(Some(p2_id)));
        assert_eq!(query_object_children(p1_id), Some(Vec::new()));
        assert_eq!(query_object_children(p2_id), Some(vec![child_id]));

        // 2. ChildAdded was suppressed on panic:
        assert!(!log.borrow().iter().any(|d| d.receiver == p2_id), "`ChildAdded` must be suppressed on panic");

        // 3. Local parent sync remains pending because ObjectBorrowGuard was held across the panic:
        assert_eq!(query_pending_parent_sync(child_id), Some(true));

        // 4. Reparenting flag on child is released by ReparentGuard drop during unwind:
        let records = [record(p1_id), record(p2_id), record(child_id)];
        assert_hierarchy_unlocked(&records);
        assert!(!records[2].reparenting.load(Ordering::Acquire), "child reparenting flag must be released");

        // Now drop the retained ObjectBorrowGuard:
        drop(held_guard.borrow_mut().take());

        // Verification AFTER dropping held_guard:
        // 1. Pending sync is applied deterministically:
        assert_eq!(query_pending_parent_sync(child_id), Some(false));
        // 2. Local child parent is now in complete agreement with the registry:
        assert_eq!(with_object(child_id, |c| c.object_data().parent()), Some(Some(p2_id)));

        drop(child);
        drop(p2);
        drop(p1);
    }
    /// When set_parent_by_id is invoked while the child is actively borrowed via with_object_mut,
    /// it must be rejected with ReparentError::Busy and perform zero state mutation.
    #[test]
    fn test_set_parent_by_id_rejected_when_child_is_borrowed() {
        let _serial = serial();
        let log = Log::default();
        let p1 = unsafe { register_owned_qobject(Handler::new(&log, None)) };
        let p2 = unsafe { register_owned_qobject(Handler::new(&log, None)) };
        let child = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
        let (p1_id, p2_id, child_id) = (
            OwnedObject::object_id(&p1),
            OwnedObject::object_id(&p2),
            OwnedObject::object_id(&child),
        );
        assert_eq!(set_parent_by_id(child_id, Some(p1_id)), Ok(()));

        // Call set_parent_by_id inside with_object_mut(child_id, ...);
        // assert Err(ReparentError::Busy) and zero state mutation.
        let nested_res = with_object_mut(child_id, |_| {
            set_parent_by_id(child_id, Some(p2_id))
        });
        assert_eq!(nested_res, Some(Err(ReparentError::Busy)));

        // Verify zero state mutation:
        assert_eq!(query_object_parent(child_id), Some(Some(p1_id)));
        assert_eq!(query_object_children(p1_id), Some(vec![child_id]));
        assert_eq!(query_object_children(p2_id), Some(Vec::new()));
        assert_eq!(with_object(child_id, |c| c.object_data().parent()), Some(Some(p1_id)));
        assert_eq!(with_object(p1_id, |p| p.object_data().children().to_vec()), Some(vec![child_id]));
        assert_eq!(with_object(p2_id, |p| p.object_data().children().to_vec()), Some(Vec::new()));

        drop(child);
        drop(p2);
        drop(p1);
    }

    /// Using legacy set_parent(&mut child, ...) as the outer operation moving to p2,
    /// a nested set_parent_by_id attempted inside the old parent's ChildRemoved callback
    /// moving to p3 must be rejected with ReparentError::Busy, and must not bypass
    /// protection or desync the registry parent and parent children lists.
    #[test]
    fn test_legacy_set_parent_outer_operation_rejects_nested_set_parent_by_id() {
        let _serial = serial();
        let log = Log::default();
        let nested_rejected = Rc::new(Cell::new(false));
        let nested_rejected_clone = Rc::clone(&nested_rejected);
        let p3_id_cell = Rc::new(Cell::new(None));
        let p3_id_clone = Rc::clone(&p3_id_cell);

        let hook: OnChildEvent = Box::new(move |delivered| {
            if delivered.what == "removed" {
                let cid = delivered.child;
                let target_p3 = p3_id_clone.get().unwrap();
                let res = set_parent_by_id(cid, Some(target_p3));
                assert_eq!(res, Err(ReparentError::Busy));
                nested_rejected_clone.set(true);
            }
        });

        let p1 = unsafe { register_owned_qobject(Handler::new(&log, Some(hook))) };
        let p2 = unsafe { register_owned_qobject(Handler::new(&log, None)) };
        let p3 = unsafe { register_owned_qobject(Handler::new(&log, None)) };
        let mut child = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
        let (p1_id, p2_id, p3_id, child_id) = (
            OwnedObject::object_id(&p1),
            OwnedObject::object_id(&p2),
            OwnedObject::object_id(&p3),
            OwnedObject::object_id(&child),
        );
        p3_id_cell.set(Some(p3_id));

        // Initial parent is p1
        assert_eq!(set_parent_by_id(child_id, Some(p1_id)), Ok(()));
        assert_eq!(query_object_parent(child_id), Some(Some(p1_id)));
        assert_eq!(query_object_children(p1_id), Some(vec![child_id]));

        // Legacy set_parent(&mut child.0, ...) moving to p2 as outer operation:
        assert_eq!(set_parent(&mut child.0, Some(p2_id)), Ok(()));
        assert!(nested_rejected.get(), "nested set_parent_by_id must be rejected with Busy");

        // Assert they do not bypass protection or desync registry parent and parent children list:
        assert_eq!(query_object_parent(child_id), Some(Some(p2_id)));
        assert_eq!(query_object_children(p1_id), Some(Vec::new()));
        assert_eq!(query_object_children(p2_id), Some(vec![child_id]));
        assert_eq!(query_object_children(p3_id), Some(Vec::new()));

        assert_eq!(child.0.parent(), Some(p2_id));
        assert_eq!(with_object(child_id, |c| c.object_data().parent()), Some(Some(p2_id)));
        assert_eq!(with_object(p1_id, |p| p.object_data().children().to_vec()), Some(Vec::new()));
        assert_eq!(with_object(p2_id, |p| p.object_data().children().to_vec()), Some(vec![child_id]));
        assert_eq!(with_object(p3_id, |p| p.object_data().children().to_vec()), Some(Vec::new()));

        drop(child);
        drop(p3);
        drop(p2);
        drop(p1);
    }

    /// When a registered child's parent is modified, a nested set_parent attempted on that
    /// registered child from inside a callback is rejected with ReparentError::Busy.
    #[test]
    fn test_set_parent_nested_set_parent_registered_child_rejected_busy() {
        let _serial = serial();
        let log = Log::default();
        let nested_ran = Rc::new(Cell::new(false));
        let nested_ran_clone = Rc::clone(&nested_ran);
        let p3_id_cell = Rc::new(Cell::new(None));
        let p3_id_clone = Rc::clone(&p3_id_cell);
        let hook: OnChildEvent = Box::new(move |delivered| {
            if delivered.what == "removed" {
                let cid = delivered.child;
                let target_p3 = p3_id_clone.get().unwrap();
                // A nested set_parent on the registered child while its parent change
                // is in progress must be rejected with ReparentError::Busy.
                let res = set_parent_by_id(cid, Some(target_p3));
                assert_eq!(res, Err(ReparentError::Busy));
                nested_ran_clone.set(true);
            }
        });
        let p1 = unsafe { register_owned_qobject(Handler::new(&log, Some(hook))) };
        let p2 = unsafe { register_owned_qobject(Handler::new(&log, None)) };
        let p3 = unsafe { register_owned_qobject(Handler::new(&log, None)) };
        let child = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
        let (p1_id, p2_id, p3_id, child_id) = (
            OwnedObject::object_id(&p1),
            OwnedObject::object_id(&p2),
            OwnedObject::object_id(&p3),
            OwnedObject::object_id(&child),
        );
        p3_id_cell.set(Some(p3_id));

        assert_eq!(set_parent_by_id(child_id, Some(p1_id)), Ok(()));
        assert_eq!(set_parent_by_id(child_id, Some(p2_id)), Ok(()));
        assert!(nested_ran.get());

        // Strict consistency checks: since nested was rejected with Busy, parent remains p2!
        assert_eq!(query_object_parent(child_id), Some(Some(p2_id)));
        assert_eq!(query_object_children(p1_id), Some(Vec::new()));
        assert_eq!(query_object_children(p2_id), Some(vec![child_id]));
        assert_eq!(query_object_children(p3_id), Some(Vec::new()));
        assert_eq!(with_object(p1_id, |p| p.object_data().children().to_vec()), Some(Vec::new()));
        assert_eq!(with_object(p2_id, |p| p.object_data().children().to_vec()), Some(vec![child_id]));
        assert_eq!(with_object(p3_id, |p| p.object_data().children().to_vec()), Some(Vec::new()));
        assert_eq!(with_object(child_id, |c| c.object_data().parent()), Some(Some(p2_id)));
        assert_eq!(child.0.parent(), Some(p2_id));
        drop(child);
        drop(p3);
        drop(p2);
        drop(p1);
    }



    /// A parent of another thread: as in Qt, the child is taken from its old parent, which gets
    /// `ChildRemoved`, and is left without a parent, in the registry too; the call reports it.
    #[test]
    fn test_set_parent_to_a_parent_of_another_thread_detaches_the_child_and_reports_it() {
        let _serial = serial();
        let log = Log::default();
        let elsewhere = std::thread::spawn(ThreadId::current).join().unwrap();
        // SAFETY: the objects are owned here and dropped at the end, on this thread.
        let p1 = unsafe { register_owned_qobject(Handler::new(&log, None)) };
        let p2 = unsafe {
            register_owned_qobject(Handler {
                data: ObjectData::with_thread(ObjectId::next(), elsewhere),
                log: Rc::clone(&log),
                on_event: None,
            })
        };
        let mut child = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
        let (p1_id, p2_id, child_id) =
            (OwnedObject::object_id(&p1), OwnedObject::object_id(&p2), OwnedObject::object_id(&child));
        assert_eq!(set_parent(&mut child.0, Some(p1_id)), Ok(()));
        log.borrow_mut().clear();

        assert_eq!(set_parent(&mut child.0, Some(p2_id)), Err(ReparentError::DifferentThread));

        assert_eq!(child.0.parent(), None);
        assert_eq!(query_object_parent(child_id), Some(None));
        assert_eq!(query_object_children(p1_id), Some(Vec::new()));
        assert_eq!(query_object_children(p2_id), Some(Vec::new()));
        assert_eq!(
            *log.borrow(),
            vec![Delivered {
                receiver: p1_id,
                what: "removed",
                child: child_id,
                own_children: vec![],
                registry_parent: Some(None),
            }]
        );
        drop(child);
        drop(p2);
        drop(p1);
    }

    /// `ChildRemoved` and `ChildAdded` of `reparent_owned_object` are delivered after its locks
    /// are let go: the handlers find every lock of the change free, and see the finished change.
    #[test]
    fn test_reparent_owned_delivers_events_after_the_locks_are_released() {
        let _serial = serial();
        let log = Log::default();
        let records: Rc<RefCell<Vec<Arc<ObjectRecord>>>> = Rc::default();
        let probe = || -> OnChildEvent {
            let records = Rc::clone(&records);
            Box::new(move |_| {
                assert_hierarchy_unlocked(&records.borrow());
                // SAFETY: the extra object is owned and dropped right here, on this thread.
                drop(unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) });
            })
        };
        // SAFETY: both parents are owned here and dropped at the end, on this thread.
        let p1 = unsafe { register_owned_qobject(Handler::new(&log, Some(probe()))) };
        let p2 = unsafe { register_owned_qobject(Handler::new(&log, Some(probe()))) };
        let (p1_id, p2_id) = (OwnedObject::object_id(&p1), OwnedObject::object_id(&p2));
        // SAFETY: the child is handed over to its parent, which keeps it at its address.
        let child_id = unsafe { add_owned_child(p1_id, Box::new(Plain(ObjectData::new(ObjectId::next())))) }
            .map_err(|(e, _)| e)
            .unwrap();
        records.borrow_mut().extend([record(p1_id), record(p2_id), record(child_id)]);
        log.borrow_mut().clear();

        let res = reparent_owned_object(child_id, Some(p2_id));
        assert!(matches!(res, Ok(None)), "{:?}", res.as_ref().err());
        let delivered = |receiver, what, own_children| Delivered {
            receiver,
            what,
            child: child_id,
            own_children,
            registry_parent: Some(Some(p2_id)),
        };
        assert_eq!(
            *log.borrow(),
            vec![delivered(p1_id, "removed", vec![]), delivered(p2_id, "added", vec![child_id])]
        );
        assert_eq!(query_object_children(p2_id), Some(vec![child_id]));
        assert_eq!(query_object_children(p1_id), Some(Vec::new()));
        drop(p2);
        drop(p1);
    }

    /// Unregistering an object unlists it from its parent without holding the object's own
    /// `parent` lock while it waits for the parent's `children` lock. `set_parent` takes them in
    /// the other order (parent's list, then child's link), so holding both ways round could
    /// deadlock.
    ///
    /// The unregistering thread is held at a fixed point (the link has been read and let go, the
    /// parent's list is not locked yet), where this thread holds the parent's list, as a thread
    /// inside `set_parent` would, and takes the link. Nothing here depends on time.
    #[test]
    fn test_unregister_does_not_hold_the_childs_parent_link_while_waiting_for_the_parents_list() {
        let _serial = serial();
        let (records_tx, records_rx) = mpsc::channel();
        let (go_tx, go_rx) = mpsc::channel::<()>();
        let (reached_tx, reached_rx) = mpsc::channel::<()>();
        let (resume_tx, resume_rx) = mpsc::channel::<()>();
        let (done_tx, done_rx) = mpsc::channel::<()>();
        let unregistering = std::thread::spawn(move || {
            // SAFETY: the parent is owned here and dropped at the end, on this thread.
            let parent = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
            let parent_id = OwnedObject::object_id(&parent);
            // SAFETY: the child is handed over to its parent, which keeps it at its address.
            let child_id = unsafe { add_owned_child(parent_id, Box::new(Plain(ObjectData::new(ObjectId::next())))) }
                .map_err(|(e, _)| e)
                .unwrap();
            records_tx.send((record(parent_id), record(child_id), parent_id)).unwrap();
            go_rx.recv().unwrap();
            test_hooks::on_next(test_hooks::Point::AfterLinkRead, move || {
                let _ = reached_tx.send(());
                let _ = resume_rx.recv();
            });
            // SAFETY: no callback of the child runs; this is its registration thread.
            unsafe { unregister_qobject(child_id) };
            assert_eq!(query_object_children(parent_id), Some(Vec::new()));
            drop(parent);
            let _ = done_tx.send(());
        });

        let (parent_record, child_record, _parent_id) = records_rx.recv().unwrap();
        // (a) This thread holds the parent's list. `Release` lets the other thread go on, and then
        // frees the list, on every exit, so that a failed check ends this test and not the run.
        struct Release<'a>(Option<mpsc::Sender<()>>, Option<std::sync::RwLockWriteGuard<'a, Vec<ObjectId>>>);
        impl Drop for Release<'_> {
            fn drop(&mut self) {
                drop(self.0.take());
                drop(self.1.take());
            }
        }
        let mut release = Release(Some(resume_tx), Some(parent_record.children.write().unwrap()));
        go_tx.send(()).unwrap();
        // (b) The other thread has read the child's link, let it go, and stops.
        reached_rx.recv().expect("unregister did not get to the point after reading the link");
        // (c) It holds nothing of the child's, so the link is free for a thread that holds the
        // parent's list and needs the link.
        assert!(
            child_record.parent.try_write().is_ok(),
            "unregister held the child's `parent` lock while it waits for the parent's list"
        );
        // (d) It goes on, and now needs the parent's list, which this thread holds. (e) This thread
        // frees the list, and the other one finishes its cleanup.
        drop(release.0.take());
        drop(release.1.take());
        done_rx
            .recv_timeout(Duration::from_secs(60))
            .expect("unregister did not finish once the parent's list was free");
        unregistering.join().unwrap();
    }

    /// The tests above notice the defects they are for: each defect is switched on in a child
    /// process (`test_hooks::mutant`), where the test must not pass. A deadlock ends at the limit.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_unregister_test_notices_a_link_held_while_waiting() {
        let name = "test_unregister_does_not_hold_the_childs_parent_link_while_waiting_for_the_parents_list";
        assert_eq!(run_in_child(name, "", CHILD_LIMIT), ChildRun::Passed);
        assert!(noticed(run_in_child(name, "unregister_holds_link", CHILD_LIMIT)));
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_event_test_notices_the_registry_held_during_the_events() {
        let name = "test_set_parent_of_a_registered_child_delivers_events_in_order_with_no_lock_held";
        assert_eq!(run_in_child(name, "", CHILD_LIMIT), ChildRun::Passed);
        assert_eq!(run_in_child(name, "set_parent_holds_registry", CHILD_LIMIT), ChildRun::Failed);
    }

    /// A hierarchy change that is held between its cycle check and its commit while a second one
    /// runs. With `HIERARCHY_MUTATION` the second one waits and sees the first one's link (and is
    /// refused when it would close a ring); without it, both are accepted. The thread of the
    /// second one is not the thread of the objects the first one works on, as `ObjectData::with_thread`
    /// allows: each thread only changes its own objects.
    #[test]
    fn test_two_set_parent_calls_cannot_close_a_cycle() {
        let _serial = serial();
        let affinity = ThreadId::current();
        let (x_id, y_id) = (ObjectId::next(), ObjectId::next());
        let (reached_tx, reached) = mpsc::channel();
        let (resume, resume_rx) = mpsc::channel();
        let x = side(
            move || unsafe { register_owned_qobject(Plain(ObjectData::with_thread(x_id, affinity))) },
            move |x| {
                hold_next_operation(reached_tx, resume_rx, &[test_hooks::Point::AfterCycleCheck]);
                set_parent(&mut x.0, Some(y_id))
            },
        );
        let y = side(
            move || unsafe { register_owned_qobject(Plain(ObjectData::with_thread(y_id, affinity))) },
            move |y| set_parent(&mut y.0, Some(x_id)),
        );
        x.wait_ready();
        y.wait_ready();
        x.start();
        reached.recv().expect("the first call did not reach its check");
        let held = HIERARCHY_MUTATION.try_lock().is_err();
        y.start();
        let early = y.result.recv_timeout(settle());
        resume.send(()).unwrap();
        let from_x = x.result.recv().expect("the first call did not return");
        let from_y = early.or_else(|_| y.result.recv()).expect("the second call did not return");
        let acyclic = registry_has_no_cycle(&[x_id, y_id]);
        x.finish();
        y.finish();
        assert_eq!(from_x, Ok(()));
        assert_eq!(from_y, Err(ReparentError::InvalidParent), "the second link closes a ring and was accepted");
        assert!(acyclic);
        assert!(held, "`HIERARCHY_MUTATION` is not held between the check and the commit");
    }

    /// As above, for `reparent_owned_object` (on the objects of its own thread) against a
    /// `set_parent` of an object of another thread: x moves under y while w, an ancestor of y,
    /// is put under x.
    #[test]
    fn test_reparent_owned_object_and_set_parent_cannot_close_a_cycle() {
        let _serial = serial();
        let affinity = ThreadId::current();
        let (w_id, x_id, y_id) = (ObjectId::next(), ObjectId::next(), ObjectId::next());
        let (reached_tx, reached) = mpsc::channel();
        let (resume, resume_rx) = mpsc::channel();
        let w = side(
            move || unsafe { register_owned_qobject(Plain(ObjectData::with_thread(w_id, affinity))) },
            move |w| set_parent(&mut w.0, Some(x_id)),
        );
        w.wait_ready();
        let r = side(
            move || {
                // SAFETY: the objects are owned here and dropped at the end, on this thread.
                let p = unsafe { register_owned_qobject(Plain(ObjectData::with_thread(ObjectId::next(), affinity))) };
                let mut y = unsafe { register_owned_qobject(Plain(ObjectData::with_thread(y_id, affinity))) };
                assert_eq!(set_parent(&mut y.0, Some(w_id)), Ok(()));
                let added = unsafe {
                    add_owned_child(OwnedObject::object_id(&p), Box::new(Plain(ObjectData::with_thread(x_id, affinity))))
                }
                .map_err(|(e, _)| e)
                .unwrap();
                assert_eq!(added, x_id);
                (p, y)
            },
            move |_| {
                hold_next_operation(reached_tx, resume_rx, &[test_hooks::Point::AfterCycleCheck]);
                reparent_owned_object(x_id, Some(y_id)).map(|released| assert!(released.is_none()))
            },
        );
        r.wait_ready();
        r.start();
        reached.recv().expect("the reparenting did not reach its check");
        let held = HIERARCHY_MUTATION.try_lock().is_err();
        w.start();
        let early = w.result.recv_timeout(settle());
        resume.send(()).unwrap();
        let from_r = r.result.recv().expect("the reparenting did not return");
        let from_w = early.or_else(|_| w.result.recv()).expect("the second call did not return");
        let acyclic = registry_has_no_cycle(&[w_id, x_id, y_id]);
        r.finish();
        w.finish();
        assert_eq!(from_r, Ok(()));
        assert_eq!(from_w, Err(ReparentError::InvalidParent), "the link closes a ring and was accepted");
        assert!(acyclic);
        assert!(held, "`HIERARCHY_MUTATION` is not held between the check and the commit");
    }

    /// As above, for `swap_hierarchy_and_ownership`: it moves c from p to q while w, which q is
    /// under, is put under c.
    #[test]
    fn test_swap_hierarchy_and_set_parent_cannot_close_a_cycle() {
        let _serial = serial();
        let affinity = ThreadId::current();
        let (w_id, c_id, p_id, q_id) = (ObjectId::next(), ObjectId::next(), ObjectId::next(), ObjectId::next());
        let (reached_tx, reached) = mpsc::channel();
        let (resume, resume_rx) = mpsc::channel();
        let w = side(
            move || unsafe { register_owned_qobject(Plain(ObjectData::with_thread(w_id, affinity))) },
            move |w| set_parent(&mut w.0, Some(c_id)),
        );
        w.wait_ready();
        let s = side(
            move || {
                // SAFETY: the objects are owned here and dropped at the end, on this thread.
                let p = unsafe { register_owned_qobject(Plain(ObjectData::with_thread(p_id, affinity))) };
                let mut q = unsafe { register_owned_qobject(Plain(ObjectData::with_thread(q_id, affinity))) };
                assert_eq!(set_parent(&mut q.0, Some(w_id)), Ok(()));
                let added = unsafe { add_owned_child(p_id, Box::new(Plain(ObjectData::with_thread(c_id, affinity)))) }
                    .map_err(|(e, _)| e)
                    .unwrap();
                assert_eq!(added, c_id);
                (p, q)
            },
            move |state| {
                let (p, q) = state;
                hold_next_operation(reached_tx, resume_rx, &[test_hooks::Point::AfterCycleCheck]);
                p.object_data_mut().swap_hierarchy_and_ownership(&mut q.object_data_mut())
            },
        );
        s.wait_ready();
        s.start();
        reached.recv().expect("the swap did not reach its check");
        let held = HIERARCHY_MUTATION.try_lock().is_err();
        w.start();
        let early = w.result.recv_timeout(settle());
        resume.send(()).unwrap();
        let from_s = s.result.recv().expect("the swap did not return");
        let from_w = early.or_else(|_| w.result.recv()).expect("the second call did not return");
        let acyclic = registry_has_no_cycle(&[w_id, c_id, p_id, q_id]);
        s.finish();
        w.finish();
        assert_eq!(from_s, Ok(()));
        assert_eq!(from_w, Err(ReparentError::InvalidParent), "the link closes a ring and was accepted");
        assert!(acyclic);
        assert!(held, "`HIERARCHY_MUTATION` is not held between the check and the commit");
    }

    /// `swap_hierarchy_and_ownership` holds neither list while it waits for the other: a thread
    /// that takes the two lists in ascending order, as every list writer does, finds both free
    /// while the swap, whose first argument has the higher id, is held before it locks them.
    #[test]
    fn test_swap_does_not_hold_one_parents_list_while_it_waits_for_the_other() {
        let _serial = serial();
        let affinity = ThreadId::current();
        let (ids_tx, ids) = mpsc::channel();
        let (reached_tx, reached) = mpsc::channel();
        let (resume, resume_rx) = mpsc::channel();
        let swapper = side(
            move || {
                // SAFETY: the objects are owned here and dropped at the end, on this thread.
                let q = unsafe { register_owned_qobject(Plain(ObjectData::with_thread(ObjectId::next(), affinity))) };
                let p = unsafe { register_owned_qobject(Plain(ObjectData::with_thread(ObjectId::next(), affinity))) };
                ids_tx.send((OwnedObject::object_id(&p), OwnedObject::object_id(&q))).unwrap();
                (p, q)
            },
            move |state| {
                let (p, q) = state;
                hold_next_operation(
                    reached_tx,
                    resume_rx,
                    &[test_hooks::Point::BetweenChildrenReads, test_hooks::Point::AfterCycleCheck],
                );
                p.object_data_mut().swap_hierarchy_and_ownership(&mut q.object_data_mut())
            },
        );
        swapper.wait_ready();
        let (p_id, q_id) = ids.recv().unwrap();
        assert!(q_id < p_id);
        let (low, high) = (record(q_id), record(p_id));
        let holder = side(
            || (),
            move |_| {
                let first = low.children.write().unwrap();
                let second = high.children.write().unwrap();
                drop(second);
                drop(first);
            },
        );
        holder.wait_ready();
        swapper.start();
        reached.recv().expect("the swap did not reach its hold point");
        holder.start();
        let took_both = holder.result.recv_timeout(PROBE_WINDOW);
        let _ = resume.send(());
        assert!(took_both.is_ok(), "the swap held a list that a writer in ascending order needs");
        assert_eq!(swapper.result.recv().expect("the swap did not return"), Ok(()));
        swapper.finish();
        holder.finish();
    }

    /// A list that changed while `swap_hierarchy_and_ownership` was on its way is not overwritten
    /// by the swap: whatever order the two run in, the child that was added is in the list of its
    /// parent afterwards.
    #[test]
    fn test_swap_does_not_overwrite_a_list_that_changed_while_it_waited() {
        let _serial = serial();
        let affinity = ThreadId::current();
        let (p_id, q_id, c_id) = (ObjectId::next(), ObjectId::next(), ObjectId::next());
        let (reached_tx, reached) = mpsc::channel();
        let (resume, resume_rx) = mpsc::channel();
        let swapper = side(
            move || {
                // SAFETY: the objects are owned here and dropped at the end, on this thread.
                let p = unsafe { register_owned_qobject(Plain(ObjectData::with_thread(p_id, affinity))) };
                let q = unsafe { register_owned_qobject(Plain(ObjectData::with_thread(q_id, affinity))) };
                (p, q)
            },
            move |state| {
                let (p, q) = state;
                hold_next_operation(reached_tx, resume_rx, &[test_hooks::Point::AfterCycleCheck]);
                p.object_data_mut().swap_hierarchy_and_ownership(&mut q.object_data_mut())
            },
        );
        let adder = side(
            move || unsafe { register_owned_qobject(Plain(ObjectData::with_thread(c_id, affinity))) },
            move |c| set_parent(&mut c.0, Some(p_id)),
        );
        swapper.wait_ready();
        adder.wait_ready();
        swapper.start();
        reached.recv().expect("the swap did not reach its check");
        adder.start();
        let early = adder.result.recv_timeout(settle());
        resume.send(()).unwrap();
        let from_swap = swapper.result.recv().expect("the swap did not return");
        let from_adder = early.or_else(|_| adder.result.recv()).expect("the adder did not return");
        let parent_of_c = query_object_parent(c_id);
        let list_of_p = query_object_children(p_id);
        swapper.finish();
        adder.finish();
        assert_eq!(from_adder, Ok(()));
        assert_eq!(from_swap, Ok(()));
        assert_eq!(parent_of_c, Some(Some(p_id)));
        assert!(
            list_of_p.is_some_and(|list| list.contains(&c_id)),
            "the child was added to its parent and the swap wrote the old list over it"
        );
    }

    /// The race tests notice the missing lock and the old pre-validation: each defect is switched
    /// on in a child process (see `test_hooks::mutant`), where the test must fail.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_race_tests_notice_a_missing_hierarchy_lock() {
        for name in [
            "test_two_set_parent_calls_cannot_close_a_cycle",
            "test_reparent_owned_object_and_set_parent_cannot_close_a_cycle",
            "test_swap_hierarchy_and_set_parent_cannot_close_a_cycle",
        ] {
            assert_eq!(run_in_child(name, "", CHILD_LIMIT), ChildRun::Passed, "{name}");
            assert!(noticed(run_in_child(name, "skip_hierarchy_lock", CHILD_LIMIT)), "{name}");
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_swap_tests_notice_the_old_pre_validation() {
        let old = "skip_hierarchy_lock,swap_prevalidates_children";
        for name in [
            "test_swap_does_not_hold_one_parents_list_while_it_waits_for_the_other",
            "test_swap_does_not_overwrite_a_list_that_changed_while_it_waited",
        ] {
            assert_eq!(run_in_child(name, "", CHILD_LIMIT), ChildRun::Passed, "{name}");
            assert!(noticed(run_in_child(name, old, CHILD_LIMIT)), "{name}");
        }
    }

    /// `set_parent` does not let a registered child have a parent that is not registered, and
    /// changes nothing when it refuses. (Two unregistered objects can be linked; see the test
    /// above.)
    #[test]
    fn test_set_parent_refuses_an_unregistered_parent_for_a_registered_child() {
        let parent = ObjectData::new(ObjectId::next());
        // SAFETY: the child is owned here and dropped at the end, on this thread.
        let mut child = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
        let before = states(&[&child.0, &parent]);
        assert_eq!(set_parent(&mut child.0, Some(parent.id())), Err(ReparentError::InvalidParent));
        assert_eq!(states(&[&child.0, &parent]), before);
        assert_eq!(child.0.parent(), None);
    }

    /// Objects that were linked while unregistered can form a ring that no registry walk saw; the
    /// registration that would close it leaves its link out, so the registry never holds a cycle.
    #[test]
    fn test_registering_objects_that_were_linked_in_a_ring_keeps_the_registry_acyclic() {
        let mut a = Plain(ObjectData::new(ObjectId::next()));
        let mut b = Plain(ObjectData::new(ObjectId::next()));
        let (a_id, b_id) = (a.0.id(), b.0.id());
        assert_eq!(set_parent(&mut a.0, Some(b_id)), Ok(()));
        assert_eq!(set_parent(&mut b.0, Some(a_id)), Ok(()));

        let ptr_a = &raw mut a as *mut dyn QObject;
        register_object_metadata(&a.0, ptr_a);
        let ptr_b = &raw mut b as *mut dyn QObject;
        register_object_metadata(&b.0, ptr_b);

        assert_eq!(query_object_parent(a_id), Some(Some(b_id)));
        assert_eq!(query_object_parent(b_id), Some(None));
        assert!(registry_has_no_cycle(&[a_id, b_id]));
        unsafe {
            unregister_qobject(a_id);
            unregister_qobject(b_id);
        }
    }

    /// `add_owned_child` refuses a parent that is a descendant of the child. Only a child that is
    /// registered already can have descendants; the deprecated `Box` registration lets one reach
    /// the function. The `Box` comes back unchanged.
    #[test]
    #[allow(deprecated)]
    // Ignored under Miri without a reason on purpose: the sweep reads `ignored` at the end of the
    // line. The deprecated `Box` registration breaks the aliasing rules by design.
    #[cfg_attr(miri, ignore)]
    fn test_add_owned_child_refuses_a_parent_that_is_a_descendant_of_the_child() {
        // SAFETY: the box is kept and not used between the calls below.
        let (b_id, boxed) = unsafe { register_boxed_qobject(Box::new(Plain(ObjectData::new(ObjectId::next())))) };
        let mut d = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
        let d_id = OwnedObject::object_id(&d);
        assert_eq!(set_parent(&mut d.0, Some(b_id)), Ok(()));

        let (error, returned) = unsafe { add_owned_child(d_id, boxed) }.unwrap_err();
        assert_eq!(error, ReparentError::InvalidParent);
        assert_eq!(query_object_parent(b_id), Some(None));
        assert_eq!(query_object_parent(d_id), Some(Some(b_id)));
        assert_eq!(query_object_children(d_id), Some(Vec::new()));
        assert_eq!(returned.0.parent(), None);
        assert!(!returned.0.is_owned_by_parent());
        assert!(registry_has_no_cycle(&[b_id, d_id]));
        // SAFETY: no callback of `b` runs.
        unsafe { unregister_qobject(b_id) };
        drop(returned);
        drop(d);
    }

    /// An adoption into a parent whose registry list is poisoned is refused with `Busy` and
    /// nothing changed, like every other commit on that list; the `Box` comes back.
    #[test]
    fn test_add_owned_child_refuses_a_parent_whose_list_is_poisoned_and_changes_nothing() {
        // SAFETY: the parent is owned here and dropped at the end, on this thread.
        let parent = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
        let parent_id = OwnedObject::object_id(&parent);
        let parent_record = record(parent_id);
        poison(&parent_record, |r| {
            let _held = r.children.write().unwrap();
            panic!("poison on purpose");
        });
        let boxed = Box::new(Plain(ObjectData::new(ObjectId::next())));
        let child_id = boxed.0.id();
        let before = states(&[&parent.0]);

        let (error, returned) = unsafe { add_owned_child(parent_id, boxed) }.unwrap_err();
        assert_eq!(error, ReparentError::Busy);
        assert_eq!(states(&[&parent.0]), before);
        assert!(read_unpoisoned(&parent_record.children).is_empty());
        assert_eq!(query_object_parent(child_id), None, "the child was registered");
        assert_eq!(returned.0.parent(), None);
        assert!(!returned.0.is_owned_by_parent());
        drop(returned);
    }

    /// Objects whose own lists name each other (the safe `reparent` builds that without any
    /// parent link) are registered: a child that is registered under another parent is not listed,
    /// so the registry's lists do not form a ring, and dropping the parent of such an object from
    /// inside its own callback ends (`unregister_descendants` walks the lists).
    #[test]
    #[allow(deprecated)]
    // Ignored under Miri without a reason on purpose: the sweep reads `ignored` at the end of the
    // line. The pointer of the `Box` that is registered first is not used after the `Box` moves.
    #[cfg_attr(miri, ignore)]
    fn test_registering_objects_whose_lists_name_each_other_leaves_no_ring_in_the_registry() {
        // SAFETY: the owner is dropped at the end, on this thread.
        let q = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
        let q_id = OwnedObject::object_id(&q);
        let mut a = Box::new(Plain(ObjectData::new(ObjectId::next())));
        let mut b = OwnedObject::new(Plain(ObjectData::new(ObjectId::next())));
        let (a_id, b_id) = (a.0.id(), b.0.id());
        reparent(&mut b.0, None, Some(&mut a.0)); // b.parent = a, a.children = [b]
        reparent(&mut b.0, None, None); // b's own link is cleared; a still lists b
        reparent(&mut a.0, None, Some(&mut b.0)); // a.parent = b, b.children = [a]
        reparent(&mut a.0, None, None); // a's own link is cleared; b still lists a

        let a_ptr: *mut Plain = &raw mut *a;
        // SAFETY: the box is kept and not used again before `add_owned_child` takes it over.
        unsafe { register_qobject_ptr(NonNull::new_unchecked(a_ptr as *mut dyn QObject)) };
        // SAFETY: the owner is kept and not used again before it is dropped.
        unsafe { register_qobject_ptr(OwnedObject::as_non_null(&b) as NonNull<dyn QObject>) };
        assert_eq!(query_object_children(a_id), Some(vec![b_id]));
        assert_eq!(query_object_children(b_id), Some(Vec::new()), "the lists of a and b make a ring");

        // SAFETY: the box is not used again.
        unsafe { add_owned_child(q_id, a) }.map_err(|(e, _)| e).unwrap();
        assert_eq!(
            query_object_children(a_id),
            Some(Vec::new()),
            "b is not a's child: it is registered without a parent"
        );
        let mut q = Some(q);
        let finished = with_object_mut(a_id, |_| drop(q.take()));
        assert!(finished.is_some());
        assert_eq!(query_object_parent(a_id), None, "a is gone with its parent");
        drop(b);
    }

    /// `unregister_descendants` ends on a ring in the registry's lists and links, which the registry
    /// does not build itself but which a stale or poisoned record could hold; it unregisters what
    /// is below the object and leaves the object.
    #[test]
    fn test_unregister_descendants_ends_on_a_ring() {
        let a = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
        let b = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
        let c = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
        let (a_id, b_id, c_id) = (OwnedObject::object_id(&a), OwnedObject::object_id(&b), OwnedObject::object_id(&c));
        for (parent, child) in [(a_id, b_id), (b_id, c_id), (c_id, a_id)] {
            record(parent).children.write().unwrap().push(child);
            *record(child).parent.write().unwrap() = Some(parent);
        }

        unregister_descendants(a_id);

        assert_eq!(query_object_parent(b_id), None);
        assert_eq!(query_object_parent(c_id), None);
        assert!(query_object_parent(a_id).is_some(), "the object itself stays registered");
        record(a_id).children.write().unwrap().clear();
        *record(a_id).parent.write().unwrap() = None;
        drop(c);
        drop(b);
        drop(a);
    }

    /// A list that names an object whose own record names no parent (the safe `reparent` edits the
    /// objects' own links only, so such an entry can be registered) does not make that object a
    /// child: dropping the listing object leaves it registered.
    #[test]
    fn test_dropping_an_object_does_not_destroy_a_listed_object_that_is_not_its_child() {
        let mut a = OwnedObject::new(Plain(ObjectData::new(ObjectId::next())));
        let mut b = OwnedObject::new(Plain(ObjectData::new(ObjectId::next())));
        let b_id = OwnedObject::object_id(&b);
        reparent(&mut b.0, None, Some(&mut a.0)); // b.parent = a, a.children = [b]
        reparent(&mut b.0, None, None); // b's own link is cleared; a still lists b
        // SAFETY: the owners are kept until they are dropped below, on this thread.
        unsafe {
            register_qobject_ptr(OwnedObject::as_non_null(&a) as NonNull<dyn QObject>);
            register_qobject_ptr(OwnedObject::as_non_null(&b) as NonNull<dyn QObject>);
        }
        assert_eq!(query_object_parent(b_id), Some(None));

        drop(a);

        assert_eq!(query_object_parent(b_id), Some(None), "b was destroyed with an object that does not own it");
        assert!(with_object(b_id, |_| ()).is_some());
        drop(b);
    }

    /// An object that is registered again while it is registered keeps what only the registry
    /// knows: a child of another registration thread that `set_parent` listed in the registry
    /// (the parent's own list cannot be edited from there). It is not dropped from the list, and the
    /// child still loses its parent link when the parent is destroyed.
    #[test]
    fn test_registering_an_object_again_keeps_the_children_that_only_the_registry_lists() {
        let _serial = serial();
        let affinity = ThreadId::current();
        let x_id = ObjectId::next();
        // SAFETY: the object is owned here and dropped at the end, on this thread.
        let child = unsafe { register_owned_qobject(Plain(ObjectData::with_thread(ObjectId::next(), affinity))) };
        let child_id = OwnedObject::object_id(&child);
        let x = side(
            move || unsafe { register_owned_qobject(Plain(ObjectData::with_thread(x_id, affinity))) },
            move |x| set_parent(&mut x.0, Some(child_id)),
        );
        x.wait_ready();
        x.start();
        assert_eq!(x.result.recv().expect("set_parent did not return"), Ok(()));
        assert_eq!(query_object_children(child_id), Some(vec![x_id]));
        assert!(child.0.children().is_empty(), "the parent's own list cannot be edited from another thread");

        register_object_metadata(&child.0, OwnedObject::as_non_null(&child).as_ptr() as *mut dyn QObject);

        assert_eq!(query_object_children(child_id), Some(vec![x_id]), "re-registration dropped a listed child");
        assert_eq!(query_object_parent(x_id), Some(Some(child_id)));
        drop(child);
        assert_eq!(query_object_parent(x_id), Some(None), "x kept a dead parent");
        x.finish();
    }

    /// Cleanup of a destroyed object cannot report an error, and skipping a step leaves an id in a
    /// parent's list or a record that points to freed memory: it finishes although every lock on
    /// its way is poisoned (a poisoned lock here guards a plain list or link).
    #[test]
    fn test_unregister_finishes_its_cleanup_when_the_locks_of_its_records_are_poisoned() {
        // SAFETY: the parent is owned here and dropped at the end, on this thread.
        let parent = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
        let parent_id = OwnedObject::object_id(&parent);
        // SAFETY: the child is handed over to its parent, which keeps it at its address.
        let child_id = unsafe { add_owned_child(parent_id, Box::new(Plain(ObjectData::new(ObjectId::next())))) }
            .map_err(|(e, _)| e)
            .unwrap();
        let (parent_record, child_record) = (record(parent_id), record(child_id));
        poison(&parent_record, |r| {
            let _held = r.children.write().unwrap();
            panic!("poison on purpose");
        });
        poison(&child_record, |r| {
            let _held = r.parent.write().unwrap();
            panic!("poison on purpose");
        });
        poison(&child_record, |r| {
            let _held = r.children.write().unwrap();
            panic!("poison on purpose");
        });
        assert!(parent_record.children.is_poisoned() && child_record.parent.is_poisoned());

        // SAFETY: no callback of the child runs; this is its registration thread.
        unsafe { unregister_qobject(child_id) };
        assert_eq!(query_object_parent(child_id), None);
        assert!(!read_unpoisoned(&parent_record.children).contains(&child_id));
        assert!(!child_record.liveness.load(Ordering::Acquire));
        drop(parent);
    }

    /// A poisoned thread-affinity lock (a plain `ThreadId`, replaced by one store) does not turn
    /// the calls that read it into panics: the queries answer, `set_parent` works, and the calls
    /// that need the lock to borrow an object say `Busy`.
    #[test]
    fn test_poisoned_thread_affinity_locks_do_not_panic_the_hierarchy_calls() {
        // SAFETY: the objects are owned here and dropped at the end, on this thread.
        let parent = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
        let mut child = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
        let (parent_id, child_id) = (OwnedObject::object_id(&parent), OwnedObject::object_id(&child));
        let parent_record = record(parent_id);
        poison(&parent_record, |r| {
            let _held = r.thread_id.write().unwrap();
            panic!("poison on purpose");
        });
        poison(&parent_record, |r| {
            let _held = r.registration_thread.write().unwrap();
            panic!("poison on purpose");
        });
        assert!(parent_record.thread_id.is_poisoned() && parent_record.registration_thread.is_poisoned());

        assert!(registered_on_current_thread(parent_id));
        assert_eq!(registration_thread_of(parent_id), Some(ThreadId::current()));
        assert_eq!(query_object_thread(parent_id), Some(ThreadId::current()));
        assert_eq!(set_parent(&mut child.0, Some(parent_id)), Ok(()));
        assert_eq!(query_object_children(parent_id), Some(vec![child_id]));
        assert_eq!(set_parent(&mut child.0, None), Ok(()));
        // SAFETY: the grandchild is handed over to its parent, which keeps it at its address.
        let owned_id = unsafe { add_owned_child(child_id, Box::new(Plain(ObjectData::new(ObjectId::next())))) }
            .map_err(|(e, _)| e)
            .unwrap();
        let before = states(&[&child.0]);
        assert!(matches!(reparent_owned_object(owned_id, Some(parent_id)), Err(ReparentError::Busy)));
        assert_eq!(states(&[&child.0]), before);
        drop(child);
        drop(parent);
    }

    /// A new parent that is registered on another thread (with the affinity of the child) cannot
    /// be borrowed here: the answer is the documented `InvalidParent`, not `Busy`, and nothing
    /// changes.
    #[test]
    fn test_reparent_owned_object_answers_invalid_parent_for_a_parent_of_another_registration_thread() {
        let affinity = ThreadId::current();
        let np_id = ObjectId::next();
        let there = side(
            move || unsafe { register_owned_qobject(Plain(ObjectData::with_thread(np_id, affinity))) },
            |_| (),
        );
        there.wait_ready();
        // SAFETY: the objects are owned here and dropped at the end, on this thread.
        let p = unsafe { register_owned_qobject(Plain(ObjectData::with_thread(ObjectId::next(), affinity))) };
        let child_id = unsafe {
            add_owned_child(OwnedObject::object_id(&p), Box::new(Plain(ObjectData::with_thread(ObjectId::next(), affinity))))
        }
        .map_err(|(e, _)| e)
        .unwrap();
        let before = states(&[&p.0, p.0.owned_children[0].object_data()]);
        let res = reparent_owned_object(child_id, Some(np_id));
        assert!(matches!(res, Err(ReparentError::InvalidParent)), "{:?}", res.as_ref().err());
        assert_eq!(states(&[&p.0, p.0.owned_children[0].object_data()]), before);
        there.finish();
    }

    /// An object is not its own parent, also when the call comes from inside its own callback (its
    /// borrow is held): the answer is `InvalidParent`, not `Busy`.
    #[test]
    fn test_reparent_owned_object_answers_invalid_parent_for_its_own_id_inside_its_own_callback() {
        // SAFETY: the objects are owned here and dropped at the end, on this thread.
        let p = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
        let child_id = unsafe { add_owned_child(OwnedObject::object_id(&p), Box::new(Plain(ObjectData::new(ObjectId::next())))) }
            .map_err(|(e, _)| e)
            .unwrap();
        let inside = with_object_mut(child_id, |_| reparent_owned_object(child_id, Some(child_id)).map(|_| ()));
        assert_eq!(inside, Some(Err(ReparentError::InvalidParent)));
    }

    /// A child that has a parent already is refused: that parent would go on listing it, and
    /// dropping that parent would unregister the child that the new parent owns. The `Box` comes
    /// back, and neither parent's list changed.
    #[test]
    fn test_add_owned_child_refuses_a_child_that_has_a_parent_already() {
        // SAFETY: the parents are owned here and dropped at the end, on this thread.
        let a = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
        let b = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
        let (a_id, b_id) = (OwnedObject::object_id(&a), OwnedObject::object_id(&b));
        let mut boxed = Box::new(Plain(ObjectData::new(ObjectId::next())));
        let child_id = boxed.0.id();
        assert_eq!(set_parent(&mut boxed.0, Some(a_id)), Ok(()));
        assert_eq!(query_object_children(a_id), Some(vec![child_id]));

        let (error, returned) = unsafe { add_owned_child(b_id, boxed) }.unwrap_err();
        assert_eq!(error, ReparentError::InvalidParent);
        assert_eq!(returned.0.parent(), Some(a_id));
        assert!(!returned.0.is_owned_by_parent());
        assert_eq!(query_object_children(a_id), Some(vec![child_id]));
        assert_eq!(query_object_children(b_id), Some(Vec::new()));
        assert_eq!(query_object_parent(child_id), None, "the child was registered");
        drop(returned);
        drop(b);
        drop(a);
    }

    /// An object whose own accessor panics while the child-list edits queued during its callback
    /// are applied does not stay borrowed for ever.
    #[test]
    fn test_borrow_flag_is_released_when_the_accessor_panics_in_the_guards_drop() {
        struct Panicky {
            data: ObjectData,
            armed: Arc<AtomicBool>,
        }
        impl QObject for Panicky {
            fn object_data(&self) -> &ObjectData {
                &self.data
            }
            fn object_data_mut(&mut self) -> ObjectDataMut<'_> {
                assert!(!self.armed.load(Ordering::SeqCst), "the accessor panics on purpose");
                ObjectDataMut::new(&mut self.data)
            }
        }
        let armed = Arc::new(AtomicBool::new(false));
        // SAFETY: the parent is owned here and dropped at the end, on this thread.
        let parent = unsafe {
            register_owned_qobject(Panicky { data: ObjectData::new(ObjectId::next()), armed: Arc::clone(&armed) })
        };
        let parent_id = OwnedObject::object_id(&parent);
        let mut child = ObjectData::new(ObjectId::next());
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            with_object_mut(parent_id, |_| {
                // The parent is borrowed, so the edit of its own list is queued...
                assert_eq!(set_parent(&mut child, Some(parent_id)), Ok(()));
                // ... and applied by the guard when this callback returns.
                armed.store(true, Ordering::SeqCst);
            })
        }));
        armed.store(false, Ordering::SeqCst);
        assert!(outcome.is_err(), "the accessor did not panic: the queued edit was not applied by the guard");
        assert!(with_object(parent_id, |_| ()).is_some(), "the parent stays borrowed");
        drop(child);
        drop(parent);
    }

    struct ProbeDirectParent(ObjectData);
    impl QObject for ProbeDirectParent {
        fn object_data(&self) -> &ObjectData { &self.0 }
        fn object_data_mut(&mut self) -> ObjectDataMut<'_> { ObjectDataMut::new(&mut self.0) }
    }

    #[test]
    fn test_reentrant_new_parent_deletion_owned_object_path_b() {
        let _serial = serial();
        let log_a = Rc::new(RefCell::new(Vec::new()));
        let p_b_cell = Rc::new(RefCell::new(None));
        let p_b_clone = Rc::clone(&p_b_cell);
        let child_id_cell = Rc::new(Cell::new(ObjectId(0)));
        let child_id_clone = Rc::clone(&child_id_cell);

        let on_event: OnChildEvent = Box::new(move |delivered: &Delivered| {
            if delivered.what == "removed" {
                let cid = child_id_clone.get();
                with_object(cid, |_| {
                    let b_opt = p_b_clone.borrow_mut().take();
                    drop(b_opt);
                });
            }
        });

        let p_a = unsafe { register_owned_qobject(Handler::new(&log_a, Some(on_event))) };
        let a_id = OwnedObject::object_id(&p_a);

        let child = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
        let child_id = OwnedObject::object_id(&child);
        child_id_cell.set(child_id);
        assert_eq!(set_parent_by_id(child_id, Some(a_id)), Ok(()));
        log_a.borrow_mut().clear();

        let p_b = unsafe { register_owned_qobject(ProbeDirectParent(ObjectData::new(ObjectId::next()))) };
        let b_id = OwnedObject::object_id(&p_b);
        *p_b_cell.borrow_mut() = Some(p_b);

        assert_eq!(set_parent_by_id(child_id, Some(b_id)), Ok(()));
        assert_eq!(query_object_parent(child_id), Some(None), "registry parent should be Some(None)");
        assert!(with_object(child_id, |_| ()).is_some(), "child must remain alive and registered");
        assert_eq!(child.object_data().parent(), None, "child local parent should be None");

        // Now drop child: A must NOT receive a duplicate ChildRemoved
        drop(child);
        let removed_count = log_a.borrow().iter().filter(|d| d.what == "removed" && d.child == child_id).count();
        assert_eq!(removed_count, 1, "A must receive exactly one ChildRemoved event, no duplicate on child drop");
        drop(p_a);
    }

    #[test]
    fn test_reentrant_new_parent_deletion_direct_pointer_path_a() {
        let _serial = serial();
        let log_a = Rc::new(RefCell::new(Vec::new()));
        let p_b_ptr_cell: Rc<RefCell<Option<*mut ProbeDirectParent>>> = Rc::new(RefCell::new(None));
        let p_b_clone = Rc::clone(&p_b_ptr_cell);
        let child_id_cell = Rc::new(Cell::new(ObjectId(0)));
        let child_id_clone = Rc::clone(&child_id_cell);

        let on_event: OnChildEvent = Box::new(move |delivered: &Delivered| {
            if delivered.what == "removed" {
                let cid = child_id_clone.get();
                with_object(cid, |_| {
                    let b_ptr = p_b_clone.borrow_mut().take().unwrap();
                    unsafe {
                        std::ptr::drop_in_place(b_ptr);
                        std::alloc::dealloc(b_ptr as *mut u8, std::alloc::Layout::new::<ProbeDirectParent>());
                    }
                });
            }
        });

        let p_a = unsafe { register_owned_qobject(Handler::new(&log_a, Some(on_event))) };
        let a_id = OwnedObject::object_id(&p_a);

        let child = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
        let child_id = OwnedObject::object_id(&child);
        child_id_cell.set(child_id);
        assert_eq!(set_parent_by_id(child_id, Some(a_id)), Ok(()));
        log_a.borrow_mut().clear();

        let b_layout = std::alloc::Layout::new::<ProbeDirectParent>();
        let p_raw = unsafe { std::alloc::alloc(b_layout) as *mut ProbeDirectParent };
        unsafe { std::ptr::write(p_raw, ProbeDirectParent(ObjectData::new(ObjectId::next()))) };
        let b_id = unsafe { (*p_raw).0.id() };
        unsafe { register_object_metadata(&(*p_raw).0, p_raw as *mut dyn QObject) };
        *p_b_ptr_cell.borrow_mut() = Some(p_raw);

        assert_eq!(set_parent_by_id(child_id, Some(b_id)), Ok(()));
        assert_eq!(query_object_parent(child_id), None, "registry parent should be None");
        assert_eq!(child.object_data().parent(), None, "child local parent should be None");

        // Now drop child: A must NOT receive a duplicate ChildRemoved
        drop(child);
        let removed_count = log_a.borrow().iter().filter(|d| d.what == "removed" && d.child == child_id).count();
        assert_eq!(removed_count, 1, "A must receive exactly one ChildRemoved event, no duplicate on child drop");
        drop(p_a);
    }

    #[test]
    fn test_set_parent_reentrant_new_parent_deletion_clears_local_state() {
        let _serial = serial();
        let log_a = Rc::new(RefCell::new(Vec::new()));
        let p_b_ptr_cell: Rc<RefCell<Option<*mut ProbeDirectParent>>> = Rc::new(RefCell::new(None));
        let p_b_clone = Rc::clone(&p_b_ptr_cell);

        let on_event: OnChildEvent = Box::new(move |delivered: &Delivered| {
            if delivered.what == "removed" {
                let b_ptr = p_b_clone.borrow_mut().take().unwrap();
                unsafe {
                    std::ptr::drop_in_place(b_ptr);
                    std::alloc::dealloc(b_ptr as *mut u8, std::alloc::Layout::new::<ProbeDirectParent>());
                }
            }
        });

        let p_a = unsafe { register_owned_qobject(Handler::new(&log_a, Some(on_event))) };
        let a_id = OwnedObject::object_id(&p_a);

        let mut child = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
        let child_id = OwnedObject::object_id(&child);
        assert_eq!(set_parent(&mut child.0, Some(a_id)), Ok(()));
        log_a.borrow_mut().clear();

        let b_layout = std::alloc::Layout::new::<ProbeDirectParent>();
        let b_raw = unsafe { std::alloc::alloc(b_layout) as *mut ProbeDirectParent };
        unsafe { std::ptr::write(b_raw, ProbeDirectParent(ObjectData::new(ObjectId::next()))) };
        let b_id = unsafe { (*b_raw).0.id() };
        unsafe { register_object_metadata(&(*b_raw).0, b_raw as *mut dyn QObject) };
        *p_b_ptr_cell.borrow_mut() = Some(b_raw);
        let child_rec = record(child_id);

        // Call set_parent(&mut child.0, Some(b_id)):
        assert_eq!(set_parent(&mut child.0, Some(b_id)), Ok(()));
        // 1. Raw parent field must be None
        assert_eq!(child.0.parent, None, "raw parent field must be None");
        assert_eq!(child.0.parent(), None, "child.parent() must be None");

        // 2. Liveness token must be None
        assert!(child.0.parent_liveness.is_none(), "parent_liveness token must be None");
        assert_eq!(query_object_parent(child_id), None, "child was unregistered so query_object_parent is None");

        // 4. pending_parent_sync state must be cleared and applied
        assert!(!child_rec.has_pending_parent_sync.load(Ordering::Acquire), "has_pending_parent_sync must be false");
        assert!(lock_unpoisoned(&child_rec.pending_parent_sync).is_none(), "pending_parent_sync record must be None");
        drop(child);
        drop(p_a);
    }
}
