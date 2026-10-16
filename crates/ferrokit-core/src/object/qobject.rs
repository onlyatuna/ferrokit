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
use std::sync::{Arc, Mutex, RwLock};

static GLOBAL_OBJECT_THREADS: RwLock<Option<HashMap<ObjectId, ThreadId>>> = RwLock::new(None);

/// Physical pointer to a registered QObject. It is dereferenced only by `registered_ptr`, after
/// the caller is verified to run on the record's registration thread.
struct RegisteredPtr(*mut dyn QObject);

// SAFETY: the pointer is dereferenced only on the registration thread, which `registered_ptr`
// checks under the record's `registration_thread` lock. Registration requires the object to stay
// pinned at this address until it is unregistered.
unsafe impl Send for RegisteredPtr {}
// SAFETY: shared access to a record never dereferences the pointer off the registration thread.
unsafe impl Sync for RegisteredPtr {}

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
    borrow_flag: Arc<AtomicBool>,
    /// Edits to the `children` list in the object's own `ObjectData` that arrived while a callback
    /// held its borrow; the guard of that callback applies them when it ends.
    pending_child_changes: Mutex<Vec<ChildListChange>>,
    has_pending_child_changes: AtomicBool,
    /// Set when an owned child was destroyed while this object's callback was running: the guard
    /// of that callback then frees the memory it kept (see `destroy_when_released`).
    free_when_released: AtomicBool,
}

/// One edit to the `children` list in an object's own `ObjectData`.
enum ChildListChange {
    Add(ObjectId),
    Remove(ObjectId),
}

impl ChildListChange {
    fn apply(&self, data: &mut ObjectData) {
        match *self {
            ChildListChange::Add(id) => data.add_child(id),
            ChildListChange::Remove(id) => data.remove_child(id),
        }
    }
}

pub struct ObjectBorrowGuard {
    ptr: *mut dyn QObject,
    borrow_flag: Arc<AtomicBool>,
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

impl Drop for ObjectBorrowGuard {
    fn drop(&mut self) {
        if self
            ._entry
            .has_pending_child_changes
            .swap(false, Ordering::AcqRel)
        {
            let changes = self
                ._entry
                .pending_child_changes
                .lock()
                .map(|mut queued| std::mem::take(&mut *queued))
                .unwrap_or_default();
            // A dead object is not written to: its memory may already be freed. Every drop of a
            // registered object marks the shared liveness token dead before the memory goes.
            if self._entry.liveness.load(Ordering::Acquire) {
                // SAFETY: the callback that held this borrow has returned, so no reference into
                // the object is live, and the object is alive.
                let data = unsafe { (*self.ptr).object_data_mut() };
                for change in &changes {
                    change.apply(data);
                }
            }
        }
        self.borrow_flag.store(false, Ordering::Release);
        if self._entry.free_when_released.load(Ordering::Acquire) {
            free_parked_children(&self.borrow_flag);
        }
    }
}

/// An owned child that waits to be freed, with the borrow flag of the callback it waits for.
type ParkedChild = (Arc<AtomicBool>, OwnedObject<dyn QObject>);

thread_local! {
    /// Owned children whose parent was destroyed while a callback of theirs was running. The guard
    /// of that callback frees them when it ends.
    static PARKED_CHILDREN: RefCell<Vec<ParkedChild>> = const { RefCell::new(Vec::new()) };
}

/// Destroys an owned child whose callback is still running, because its parent is being dropped
/// from inside it. For everything else the child is destroyed at once, and so is everything the
/// registry lists below it: it is unregistered, so it is no longer alive, gets no events or
/// timers, and its connections to other objects are gone. Its memory is kept until the callback
/// returns, since that callback still runs on `&mut self`; the guard of the callback frees it
/// (`free_parked_children`), and with it the memory of the objects below it. The child is not
/// read here: its id comes from its owner.
fn destroy_when_released(child: OwnedObject<dyn QObject>) {
    let child_id = OwnedObject::object_id(&child);
    let Some((entry, _)) = registered_ptr(child_id) else {
        drop(child);
        return;
    };
    entry.free_when_released.store(true, Ordering::Release);
    PARKED_CHILDREN.with(|parked| {
        parked
            .borrow_mut()
            .push((Arc::clone(&entry.borrow_flag), child));
    });
    unregister_descendants(child_id);
    // SAFETY: the running callback holds the borrow flag and its own `Arc` to the record, and the
    // memory is not freed until it returns. No other callback can start on an unregistered object.
    unsafe { unregister_qobject(child_id) };
}

/// Unregisters everything the registry lists below `id`, the deepest first, and touches no memory:
/// a callback may be running in any of them, and whoever owns an object frees it.
fn unregister_descendants(id: ObjectId) {
    for child_id in registered_children_of(id) {
        unregister_descendants(child_id);
        // SAFETY: a running callback keeps its own `Arc` to the record, and the memory is not
        // freed before that callback returns (see `destroy_when_released`).
        unsafe { unregister_qobject(child_id) };
    }
}

/// Frees the children parked while the callback that held `flag` was running.
fn free_parked_children(flag: &Arc<AtomicBool>) {
    let ready: Vec<OwnedObject<dyn QObject>> = PARKED_CHILDREN.with(|parked| {
        let mut parked = parked.borrow_mut();
        let (ready, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut *parked)
            .into_iter()
            .partition(|(held, _)| Arc::ptr_eq(held, flag));
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
    if *entry.registration_thread.read().unwrap() != ThreadId::current()
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
        && registered_ptr(id).is_some_and(|(entry, _)| entry.borrow_flag.load(Ordering::Acquire))
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

fn borrow_registered(id: ObjectId) -> Option<ObjectBorrowGuard> {
    let (entry, ptr) = registered_ptr(id)?;
    entry
        .borrow_flag
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .ok()?;
    Some(ObjectBorrowGuard {
        ptr,
        borrow_flag: Arc::clone(&entry.borrow_flag),
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
            if let Ok(mut queued) = entry.pending_child_changes.lock() {
                queued.push(change);
                entry
                    .has_pending_child_changes
                    .store(true, Ordering::Release);
            }
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
pub fn register_object_thread(id: ObjectId, thread_id: ThreadId) {
    let mut lock = GLOBAL_OBJECT_THREADS.write().unwrap();
    if lock.is_none() {
        *lock = Some(HashMap::new());
    }
    if let Some(map) = lock.as_mut() {
        map.insert(id, thread_id);
    }
    if let Ok(reg) = GLOBAL_OBJECT_REGISTRY.read() {
        if let Some(entry) = reg.get(&id) {
            *entry.thread_id.write().unwrap() = thread_id;
        }
    }
}

/// Unregisters an object's thread affinity from the global registry.
pub fn unregister_object_thread(id: ObjectId) {
    if let Ok(mut lock) = GLOBAL_OBJECT_THREADS.write() {
        if let Some(map) = lock.as_mut() {
            map.remove(&id);
        }
    }
}

/// Physical registration thread of a live registered object, or `None` if it is unregistered.
pub(crate) fn registration_thread_of(id: ObjectId) -> Option<ThreadId> {
    let entry = GLOBAL_OBJECT_REGISTRY.read().ok()?.get(&id)?.clone();
    if !entry.liveness.load(Ordering::Acquire) {
        return None;
    }
    let thread_id = *entry.registration_thread.read().unwrap();
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
            *entry.registration_thread.write().unwrap() = thread_id;
        }
    }
}

/// Queries the target object's thread affinity across threads.
pub fn query_object_thread(id: ObjectId) -> Option<ThreadId> {
    if let Ok(reg) = GLOBAL_OBJECT_REGISTRY.read() {
        if let Some(entry) = reg.get(&id) {
            return Some(*entry.thread_id.read().unwrap());
        }
    }
    if let Ok(lock) = GLOBAL_OBJECT_THREADS.read() {
        if let Some(map) = lock.as_ref() {
            return map.get(&id).copied();
        }
    }
    None
}

/// Registers `obj` so that the registry can call it by id.
///
/// Limit: the registry keeps a pointer made from the reference `obj`. Under Rust's aliasing
/// rules, the next direct use of the object through the variable, field or `Box` that `obj`
/// borrowed from invalidates that pointer, so a callback after such a use is undefined behavior,
/// even if the program appears to work; Miri reports it. When the owner uses the object between
/// callbacks, register it with [`register_qobject_ptr`] (a pointer taken with `&raw mut`) or
/// [`register_owned_qobject`] instead. This function is kept for existing callers.
///
/// # Safety
/// The object must remain alive at the same address until `unregister_qobject` completes.
/// Do not move or drop it while registered. All access from any thread must occur through
/// registry callbacks, except sequential direct access between callbacks on the registration
/// thread. The object must not be unregistered or re-registered while a callback is active.
/// Callbacks execute only on the physical registration thread. `move_to_thread` transfers that
/// registration to the target thread together with the thread affinity. The caller must ensure
/// no overlapping mutable or mutable/shared aliases exist while a callback runs.
pub unsafe fn register_qobject(obj: &mut dyn QObject) {
    let ptr = obj as *mut dyn QObject;
    let data = unsafe { &*ptr }.object_data();
    register_object_metadata(data, ptr);
}

fn register_object_metadata(data: &ObjectData, ptr: *mut dyn QObject) {
    let id = data.id;
    let thread_id = data.thread_id;
    let record = Arc::new(ObjectRecord {
        id,
        thread_id: RwLock::new(thread_id),
        registration_thread: RwLock::new(ThreadId::current()),
        ptr: RegisteredPtr(ptr),
        parent: RwLock::new(data.parent()),
        children: RwLock::new(data.children.clone()),
        liveness: Arc::clone(&data.liveness),
        generation: AtomicU32::new(data.generation),
        borrow_flag: Arc::new(AtomicBool::new(false)),
        pending_child_changes: Mutex::new(Vec::new()),
        has_pending_child_changes: AtomicBool::new(false),
        free_when_released: AtomicBool::new(false),
    });
    if let Ok(mut reg) = GLOBAL_OBJECT_REGISTRY.write() {
        if let Some(previous) = reg.insert(id, record) {
            // Re-registering the same object (e.g. `Timer::start` after `stop`, possibly at a new
            // address) shares the liveness token with the replaced record. Killing it would
            // mark the live object dead and null every `QPointer` to it. Only a different
            // object that reused the id is retired.
            if !Arc::ptr_eq(&previous.liveness, &data.liveness) {
                previous.liveness.store(false, Ordering::Release);
            }
        }
    }
    register_object_thread(id, thread_id);
}

/// Registers a pinned object.
///
/// The registry's pointer is made from the reference inside `obj`, with the limit described at
/// [`register_qobject`]. Miri does not check that limit for `!Unpin` types, so a `!Unpin` object
/// passing under Miri is no evidence that the use is sound.
///
/// # Safety
/// The pinned object must remain alive and at its pinned address until unregistered.
/// All access and aliasing requirements are identical to [`register_qobject`].
pub unsafe fn register_pinned_qobject(mut obj: std::pin::Pin<&mut dyn QObject>) {
    let ptr = unsafe { obj.as_mut().get_unchecked_mut() as *mut dyn QObject };
    let data = unsafe { (*ptr).object_data() };
    register_object_metadata(data, ptr);
}

/// Registers a boxed object and hands the `Box` back.
///
/// Limit: Rust treats a `Box` as the only pointer to its object, so using or moving the returned
/// `Box` invalidates the pointer the registry keeps, with the consequences described at
/// [`register_qobject`]. Use [`register_owned_qobject`], whose [`OwnedObject`] shares its pointer
/// with the registry. This function is kept for existing callers.
///
/// # Safety
/// The returned Box must remain alive and unmoved until unregistered. All access and aliasing
/// requirements are identical to [`register_qobject`].
pub unsafe fn register_boxed_qobject<T: QObject + 'static>(
    mut boxed: Box<T>,
) -> (ObjectId, Box<T>) {
    let id = boxed.object_data().id;
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
///   may then use `x` directly between callbacks.
///
/// A pointer made from a reference (`&mut x as *mut _`, `NonNull::from(&mut x)`) or from a `Box`
/// that is used afterwards does not qualify; it has the limit described at [`register_qobject`].
///
/// # Safety
/// - `ptr` points to a live object that stays at this address until `unregister_qobject`
///   completes.
/// - Between callbacks, the owner reaches the object only through `ptr` or pointers derived from
///   it, as described above.
/// - While a callback of the object runs, nothing else accesses the object, not even to read it.
/// - Registering the object again, while it is registered, uses the same pointer. It does not
///   happen, and neither does unregistering it, while a callback of the object runs.
/// - The thread rules of [`register_qobject`] apply.
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
/// it, and is not dropped. The owner is used and dropped on the registration thread. The thread
/// rules of [`register_qobject`] apply.
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
/// # Safety
/// The caller must ensure that no callback currently holds a borrow of this object, and that
/// no concurrent registry callback can begin until this call completes. The object must remain
/// alive through this call. Invoke on the object's physical registration thread so its timers
/// are stopped from that thread's timer registry.
pub unsafe fn unregister_qobject(id: ObjectId) {
    let removed = GLOBAL_OBJECT_REGISTRY
        .write()
        .ok()
        .and_then(|mut reg| reg.remove(&id));
    if let Some(entry) = removed {
        entry.liveness.store(false, Ordering::Release);
        if let Ok(registry) = GLOBAL_OBJECT_REGISTRY.read() {
            if let Some(parent_id) = *entry.parent.read().unwrap() {
                if let Some(parent) = registry.get(&parent_id) {
                    parent
                        .children
                        .write()
                        .unwrap()
                        .retain(|&child_id| child_id != id);
                }
            }
            for child_id in entry.children.read().unwrap().iter() {
                if let Some(child) = registry.get(child_id) {
                    *child.parent.write().unwrap() = None;
                }
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
    pub id: ObjectId,
    pub parent: Option<ObjectId>,
    pub children: Vec<ObjectId>,

    /// Thread affinity.
    pub thread_id: ThreadId,

    pub delete_later_called: AtomicBool,

    pub signals_blocked: Arc<AtomicBool>,

    /// Installed event filters chain.
    pub event_filters: EventFilterChain,

    /// Dynamic properties map (`QObject::dynamicPropertyNames`).
    pub dynamic_properties: HashMap<String, Variant>,

    /// Object name for hierarchy queries (`QObject::objectName`).
    pub object_name: Option<String>,

    /// Emitted when object name changes (`QObject::objectNameChanged`).
    pub object_name_changed: Signal<String>,

    /// Shared liveness token for guarded weak references (`QPointer`).
    pub liveness: Arc<AtomicBool>,

    /// Object lifecycle generation counter.
    pub generation: u32,

    /// Directly owned child objects for cascading destruction. Each owner shares its pointer with
    /// the registry (see [`OwnedObject`]), and knows the child's id without reading the child.
    pub owned_children: Vec<OwnedObject<dyn QObject>>,
    pub timers: Vec<crate::timer::TimerId>,

    /// True while this object is held in its parent's `owned_children`. Such an object's
    /// lifetime belongs to the parent, so `set_parent` (which only sees `&mut ObjectData`, a
    /// borrow *into* the owned object) must not detach it; [`reparent_owned`] moves its owner.
    owned_by_parent: bool,

    /// The parent's id and liveness token, captured when a registered parent was set. It lets
    /// [`ObjectData::parent`] see the parent die when the parent's drop cannot write to this
    /// object: a running callback borrows it, it was never registered, or it is registered on
    /// another thread. A token whose id differs from `parent` is stale and ignored, because
    /// `parent` is a public field that can be assigned directly.
    parent_liveness: Option<(ObjectId, Arc<AtomicBool>)>,
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
        }
    }
    /// Creates a new ObjectData with a unique auto-generated ObjectId.
    #[allow(clippy::new_without_default)]
    pub fn with_auto_id() -> Self {
        Self::new(ObjectId::next())
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

    /// True while a parent owns this object (see [`reparent_owned`]).
    pub fn is_owned_by_parent(&self) -> bool {
        self.owned_by_parent
    }

    pub fn add_child(&mut self, child_id: ObjectId) {
        if !self.children.contains(&child_id) {
            self.children.push(child_id);
        }
    }

    pub fn remove_child(&mut self, child_id: ObjectId) {
        self.children.retain(|&id| id != child_id);
    }

    /// Returns the parent object ID if any (`QObject::parent`).
    ///
    /// `None` once the parent has been dropped, as long as it was registered when it was set,
    /// even if this object could not be updated at that moment. The public `parent` field can
    /// still name the dead parent then; read it through this method.
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
            if registered_and_borrowed(OwnedObject::object_id(&child)) {
                destroy_when_released(child);
            } else {
                child.object_data_mut().clear_parent();
                idle.push(child);
            }
        }
        for child_id in std::mem::take(&mut self.children) {
            release_child(child_id);
        }
        // The registry's list is complete at once, while this object's own list lags until a
        // running callback of this object returns. Whatever is still registered here goes too.
        for child_id in registered_children_of(self.id) {
            release_child(child_id);
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
}

impl Drop for ObjectData {
    fn drop(&mut self) {
        // The object is gone from this first step on: the token goes false before the parent is
        // told or a child is deleted, so a signal of this object that is emitted by then reaches
        // no slot. Qt `~QObject` disconnects every receiver before `deleteChildren`
        // (qobject.cpp:1086-1178), and `QPointer` is cleared before anything else is destroyed.
        self.liveness.store(false, Ordering::Release);

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
                    parent_obj
                        .object_data_mut()
                        .children
                        .retain(|&id| id != self.id);
                    parent_obj
                        .object_data_mut()
                        .owned_children
                        .retain(|c| OwnedObject::object_id(c) != self.id);
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
/// [`ReparentError::DifferentThread`] from [`set_parent`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReparentError {
    /// [`set_parent`]: the object is held by its parent's `owned_children`. Detaching it there
    /// would destroy it (or leave the caller's `&mut` dangling); use [`reparent_owned`].
    OwnedByParent,
    /// [`reparent_owned`]: the object has no parent that owns it.
    NotOwned,
    /// The object is not registered, is dead, or is registered on another thread. For
    /// [`add_owned_child`] the object is the new parent.
    UnknownObject,
    /// [`set_parent`]: the new parent is the object itself or one of its descendants. Not for an
    /// unregistered parent, which `set_parent` accepts. [`reparent_owned`]: those two, and also a
    /// new parent that is not registered on the calling thread.
    InvalidParent,
    /// The object or a parent is borrowed by an active callback.
    Busy,
    /// The new parent belongs to a different thread than the child: their thread affinities
    /// (`ObjectData::thread_id`) differ, and a parent and its children share one thread. What is
    /// left behind depends on the call:
    /// - [`set_parent`] does what Qt's `setParent` does. The child has already been removed from
    ///   its old parent, which received `ChildRemoved`, so it is left without a parent, and Qt's
    ///   warning goes to stderr.
    /// - [`reparent_owned`] changes nothing and prints nothing: the child stays with its old
    ///   parent, which still owns it. Qt would detach the child.
    /// - [`add_owned_child`] attaches nothing and prints nothing, and hands the `Box` back: the
    ///   child has no parent and the parent's children are as they were. Qt's constructor leaves
    ///   the new object without a parent in the same way, but warns.
    DifferentThread,
}

/// Lets go of a child that its parent does not own: it survives the parent, loses its
/// parent link, and is unregistered.
fn release_child(child_id: ObjectId) {
    // The link is cleared before the unregister below, after which the child can no longer be
    // reached. A child that cannot be borrowed here (a running callback holds it, or it was never
    // registered) is not written to; its `parent()` sees the parent's liveness token instead.
    let _ = with_object_mut(child_id, |child| child.object_data_mut().clear_parent());
    // SAFETY: the child may be borrowed by a running callback. It is not written to or freed here:
    // whoever owns it frees it, and the callback's guard keeps its own `Arc` to the record.
    unsafe { unregister_qobject(child_id) };
}

/// Lists `child_id` among the children the registry keeps for `parent_id`.
fn list_in_registry(parent_id: ObjectId, child_id: ObjectId) {
    let Ok(registry) = GLOBAL_OBJECT_REGISTRY.read() else {
        return;
    };
    if let Some(record) = registry.get(&parent_id) {
        let mut children = record.children.write().unwrap();
        if !children.contains(&child_id) {
            children.push(child_id);
        }
    }
}

/// The children the registry lists for `id` that are still registered.
fn registered_children_of(id: ObjectId) -> Vec<ObjectId> {
    let Ok(registry) = GLOBAL_OBJECT_REGISTRY.read() else {
        return Vec::new();
    };
    let Some(record) = registry.get(&id) else {
        return Vec::new();
    };
    let children = record.children.read().unwrap();
    children
        .iter()
        .copied()
        .filter(|child_id| registry.contains_key(child_id))
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

/// Whether making `new_parent` the parent of `child_id` would make an object its own ancestor:
/// `new_parent` is the child itself or one of its descendants, as the registry records them.
fn would_create_cycle(child_id: ObjectId, new_parent: ObjectId) -> bool {
    let Ok(registry) = GLOBAL_OBJECT_REGISTRY.read() else {
        return false;
    };
    let mut cursor = Some(new_parent);
    // The bound ends the walk if the registry already holds a cycle.
    for _ in 0..=registry.len() {
        let Some(id) = cursor else { break };
        if id == child_id {
            return true;
        }
        cursor = registry.get(&id).and_then(|r| *r.parent.read().unwrap());
    }
    false
}

/// Sets a non-owned child's parent and updates registered relationship metadata, sending
/// `ChildRemoved`/`ChildAdded` to the registered parents.
///
/// Refuses with [`ReparentError::OwnedByParent`] when a parent owns the child; use
/// [`reparent_owned`] for those. Qt's `setParent_helper` (qobject.cpp:2287-2345) never destroys
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
/// on (see [`register_qobject`]). [`reparent_owned`] and [`add_owned_child`] refuse the same
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
/// - In the cases just named, where the parent's drop cannot write to the child, the public
///   `parent` field can keep the old id. Read the parent through `parent()`.
/// - While a running callback holds the parent's borrow, the parent's own `children()` still
///   shows the list as it was; the change is applied when that callback returns. The registry
///   has it at once, so dropping the parent reaches the child either way.
///
/// `new_parent` need not be registered: the id is accepted and only the child's own link is
/// recorded. The parent's child list, `ChildAdded` and the death tracking above all need a
/// registered parent and do not happen. Qt has no such case (a `QObject*` is always a live
/// object), so nothing more is specified for it. [`reparent_owned`] and [`add_owned_child`]
/// refuse an unregistered parent.
///
/// Use [`add_owned_child`] to make the parent own the child.
pub fn set_parent(child: &mut ObjectData, new_parent: Option<ObjectId>) -> Result<(), ReparentError> {
    let old_parent = child.parent();
    if old_parent == new_parent {
        return Ok(());
    }
    if child.owned_by_parent {
        return Err(ReparentError::OwnedByParent);
    }
    let child_id = child.id;
    if let Some(np) = new_parent {
        if would_create_cycle(child_id, np) {
            return Err(ReparentError::InvalidParent);
        }
    }
    // Qt finds the mismatch only after it has removed the child from its old parent, so the
    // change below goes ahead as a detach.
    let refused_thread = new_parent.is_some_and(|np| in_different_thread(child.thread_id, np));
    let new_parent = if refused_thread { None } else { new_parent };

    // Update global registry parent/children records.
    if let Ok(registry) = GLOBAL_OBJECT_REGISTRY.read() {
        if let Some(old_record) = old_parent.and_then(|id| registry.get(&id)) {
            old_record
                .children
                .write()
                .unwrap()
                .retain(|&id| id != child_id);
        }
        if let Some(child_record) = registry.get(&child_id) {
            *child_record.parent.write().unwrap() = new_parent;
        }
        if let Some(new_record) = new_parent.and_then(|id| registry.get(&id)) {
            let mut children = new_record.children.write().unwrap();
            if !children.contains(&child_id) {
                children.push(child_id);
            }
        }
    }

    // Update local children lists and send ChildRemoved/ChildAdded via registered callbacks.
    if let Some(old_parent_id) = old_parent {
        edit_child_list(old_parent_id, ChildListChange::Remove(child_id));
        let removed = Event::new(EventKind::ChildRemoved { child_id });
        if let Err(mut removed) = defer_if_borrowed(old_parent_id, removed) {
            dispatch_to_object(old_parent_id, &mut removed);
        }
    }
    child.parent = new_parent;
    child.parent_liveness = new_parent.and_then(|id| liveness_of(id).map(|alive| (id, alive)));
    if let Some(new_parent_id) = new_parent {
        edit_child_list(new_parent_id, ChildListChange::Add(child_id));
        let added = Event::new(EventKind::ChildAdded { child_id });
        if let Err(mut added) = defer_if_borrowed(new_parent_id, added) {
            dispatch_to_object(new_parent_id, &mut added);
        }
    }
    if refused_thread {
        eprintln!("QObject::setParent: Cannot set parent, new parent is in a different thread");
        return Err(ReparentError::DifferentThread);
    }
    Ok(())
}

/// Moves an owned child to `new_parent`, or releases it to the caller when `new_parent` is
/// `None` (`Ok(Some(box))`; the child stays alive and registered, with no parent).
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
/// A released child comes back as a `Box`. A `Box` claims to be the only pointer to its object,
/// which the registry's pointer contradicts while the child stays registered. Use
/// [`reparent_owned_object`] to get an [`OwnedObject`], which shares its pointer with the
/// registry.
pub fn reparent_owned(
    child_id: ObjectId,
    new_parent: Option<ObjectId>,
) -> Result<Option<Box<dyn QObject>>, ReparentError> {
    reparent_owned_object(child_id, new_parent).map(|released| released.map(OwnedObject::into_box))
}

/// [`reparent_owned`] that releases the child as an [`OwnedObject`] instead of a `Box`. The owner
/// keeps the pointer the registry has, so the caller may use the child between its callbacks
/// while it stays registered.
pub fn reparent_owned_object(
    child_id: ObjectId,
    new_parent: Option<ObjectId>,
) -> Result<Option<OwnedObject<dyn QObject>>, ReparentError> {
    let (child_record, _) = registered_ptr(child_id).ok_or(ReparentError::UnknownObject)?;
    // Only the borrow flag is used: the guard is never dereferenced, so no `&mut` aliases the
    // child, which is written through its owner below. It rejects a call made from inside the
    // child's own callback.
    let child_guard = borrow_registered(child_id).ok_or(ReparentError::Busy)?;
    let old_parent_id = (*child_record.parent.read().unwrap()).ok_or(ReparentError::NotOwned)?;
    registered_ptr(old_parent_id).ok_or(ReparentError::NotOwned)?;

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

    if let Some(np) = new_parent {
        if in_different_thread(*child_record.thread_id.read().unwrap(), np) {
            return Err(ReparentError::DifferentThread);
        }
        registered_ptr(np).ok_or(ReparentError::InvalidParent)?;
        if would_create_cycle(child_id, np) {
            return Err(ReparentError::InvalidParent);
        }
    }

    let mut old_guard = borrow_registered(old_parent_id).ok_or(ReparentError::Busy)?;
    let mut new_guard = match new_parent {
        Some(np) => Some(borrow_registered(np).ok_or(ReparentError::Busy)?),
        None => None,
    };

    let mut owner = {
        let data = old_guard.object_data_mut();
        let pos = data
            .owned_children
            .iter()
            .position(|c| OwnedObject::object_id(c) == child_id)
            .ok_or(ReparentError::NotOwned)?;
        data.children.retain(|&id| id != child_id);
        data.owned_children.remove(pos)
    };
    {
        let data = owner.object_data_mut();
        data.parent = new_parent;
        data.parent_liveness = new_parent.and_then(|id| liveness_of(id).map(|alive| (id, alive)));
        data.owned_by_parent = new_parent.is_some();
    }

    if let Ok(registry) = GLOBAL_OBJECT_REGISTRY.read() {
        if let Some(old_record) = registry.get(&old_parent_id) {
            old_record
                .children
                .write()
                .unwrap()
                .retain(|&id| id != child_id);
        }
        *child_record.parent.write().unwrap() = new_parent;
        if let Some(new_record) = new_parent.and_then(|id| registry.get(&id)) {
            let mut children = new_record.children.write().unwrap();
            if !children.contains(&child_id) {
                children.push(child_id);
            }
        }
    }

    let released = match new_guard.as_mut() {
        Some(guard) => {
            let data = guard.object_data_mut();
            if !data.children.contains(&child_id) {
                data.children.push(child_id);
            }
            data.owned_children.push(owner);
            None
        }
        None => Some(owner),
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
/// `DifferentThread`; if the parent is not registered, returns `UnknownObject`; if the parent is
/// borrowed (for example from inside its own callback), returns `Busy`. In every case the `Box` is
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
    if in_different_thread(child.object_data().thread_id, parent_id) {
        return Err((ReparentError::DifferentThread, child));
    }
    if registered_ptr(parent_id).is_none() {
        return Err((ReparentError::UnknownObject, child));
    }
    let child_id = child.object_data().id;
    let mut pending = Some(child);
    let attached = with_object_mut(parent_id, |parent| {
        let mut child = pending.take().expect("closure runs at most once");
        {
            let data = child.object_data_mut();
            data.parent = Some(parent_id);
            data.parent_liveness = liveness_of(parent_id).map(|alive| (parent_id, alive));
            data.owned_by_parent = true;
        }
        parent.object_data_mut().add_child(child_id);
        // The parent holds the child through an owner made from the `Box`; the registry gets a
        // copy of the owner's pointer, so moving the owner later invalidates nothing.
        let owner = OwnedObject::into_dyn(OwnedObject::from_box(child));
        // SAFETY: the object stays at this address while the owner lives; the rest is delegated
        // to this function's caller contract.
        unsafe { register_qobject_ptr(OwnedObject::as_non_null(&owner)) };
        // The registry's list is what still reaches the child while a running callback of the
        // parent hides it from the parent's own `children`.
        list_in_registry(parent_id, child_id);
        parent.object_data_mut().owned_children.push(owner);
    });
    match attached {
        Some(()) => {
            let mut added = Event::new(EventKind::ChildAdded { child_id });
            crate::event_loop::notify_helper(parent_id, &mut added);
            Ok(child_id)
        }
        None => Err((
            ReparentError::Busy,
            pending
                .take()
                .expect("closure did not run, so the Box is still here"),
        )),
    }
}

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
    fn object_data_mut(&mut self) -> &mut ObjectData;
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
            let name_matches = name.is_empty() || child.object_name() == Some(name);
            if name_matches {
                if let Some(any) = child.as_qobject_any() {
                    if any.type_id() == type_id {
                        return Some(child.object_data().id);
                    }
                }
            }
        }
        // 2. Descendants if no direct child matched
        for child in &self.object_data().owned_children {
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
            return self.object_data_mut().owned_children[i].as_qobject_any_mut();
        }
        // 2. Descendants if no direct child matched
        for child in &mut self.object_data_mut().owned_children {
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
        let id = self.object_data().id;
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
        let receiver = self.object_data().id;
        let id = crate::timer::start_object_timer(receiver, interval_ms, timer_type);
        self.object_data_mut().timers.push(id);
        id
    }

    /// Kills the timer with the specified timer ID (`QObject::killTimer`).
    fn kill_timer(&mut self, id: crate::timer::TimerId) -> bool {
        self.object_data_mut().timers.retain(|&t| t != id);
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
        Self::from_data(obj.object_data_mut())
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
            id: data.id,
            generation: data.generation,
            liveness: Arc::clone(&data.liveness),
            _marker: std::marker::PhantomData,
        }
    }

    pub fn from_data(data: &ObjectData) -> Self {
        Self {
            id: data.id,
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

#[cfg(test)]
mod tests {
    use super::*;
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
            fn object_data_mut(&mut self) -> &mut ObjectData {
                &mut self.data
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
            fn object_data_mut(&mut self) -> &mut ObjectData {
                &mut self.data
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
            fn object_data_mut(&mut self) -> &mut ObjectData {
                &mut self.data
            }
            fn event(&mut self, _event: &mut Event) -> bool {
                self.event_count += 1;
                true
            }
        }

        let mut snooper = EventSnooper::new(ObjectId::next(), true);
        let mut target = TargetWidget::new(ObjectId::next());

        install_event_filter(target.object_data_mut(), snooper.object_data().id);

        let mut ev = Event::new(EventKind::UpdateRequest);

        let mut filtered = false;
        for filter_id in target.object_data().event_filters.snapshot() {
            if filter_id == snooper.object_data().id
                && snooper.event_filter(target.object_data().id, &mut ev)
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
            fn object_data_mut(&mut self) -> &mut ObjectData {
                &mut self.data
            }
        }

        let el = crate::event_loop::EventLoop::new();
        let mut probe = Probe {
            data: ObjectData::new(ObjectId::next()),
        };
        let id = probe.data.id;
        // SAFETY: `probe` stays at this stack address and is unregistered below before it drops.
        unsafe { register_qobject(&mut probe) };

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
            fn object_data_mut(&mut self) -> &mut ObjectData {
                &mut self.data
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
        unsafe { register_qobject(&mut widget) };

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
            fn object_data_mut(&mut self) -> &mut ObjectData {
                &mut self.data
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
        let parent_id = parent.data.id;
        // SAFETY: `parent` stays at this stack address and is unregistered below before it drops.
        unsafe { register_qobject(&mut parent) };
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
            fn object_data_mut(&mut self) -> &mut ObjectData {
                &mut self.data
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
        let id = target.data.id;
        // SAFETY: `target` stays at this stack address and is unregistered below before it drops.
        unsafe { register_qobject(&mut target) };

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
            fn object_data_mut(&mut self) -> &mut ObjectData {
                &mut self.data
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
        let parent_id = parent.data.id;
        // SAFETY: `parent` stays at this stack address and is unregistered below before it drops.
        unsafe { register_qobject(&mut parent) };
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
}
