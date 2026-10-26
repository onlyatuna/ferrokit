// SPDX-License-Identifier: LGPL-3.0-only
#![allow(clippy::unnecessary_map_or, clippy::bool_assert_comparison)]

use parking_lot::Mutex;
use std::cell::RefCell;
use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use crate::object::query_object_thread;
use crate::object::{ObjectData, ObjectId, QObject, ThreadId};
use crate::poison::write_unpoisoned;

use crate::event::{Event, EventKind};
use crate::event_loop::post_event_to_thread;

thread_local! {
    static CURRENT_SENDER_STACK: RefCell<Vec<Option<ObjectId>>> = const { RefCell::new(Vec::new()) };
}

/// Returns the ObjectId of the object that sent the signal currently being handled.
/// Returns None if no signal is actively being emitted.
/// Modeled after Qt's QObject::sender().
pub fn sender() -> Option<ObjectId> {
    // A slot that runs while the thread's locals are destroyed (a `destroyed` receiver of an object
    // held by a thread local) finds the stack gone: there is no sender then.
    CURRENT_SENDER_STACK
        .try_with(|stack| stack.borrow().last().copied().flatten())
        .unwrap_or(None)
}

/// RAII guard that pushes the emitter of the signal being delivered onto the sender stack.
/// Every emission pushes an entry, `None` for an unbound signal, so a slot reached through an
/// unbound signal sees `sender() == None` rather than an outer emitter. Qt `QObject::sender()` is
/// `nullptr` when no QObject emitted the signal.
pub struct SenderGuard;

impl SenderGuard {
    pub fn new(sender_id: Option<ObjectId>) -> Self {
        // Once the thread's locals are destroyed there is no stack to push to, and `drop` finds
        // none to pop from; an emission from a destructor must not panic there.
        let _ = CURRENT_SENDER_STACK.try_with(|stack| stack.borrow_mut().push(sender_id));
        SenderGuard
    }
}

impl Drop for SenderGuard {
    fn drop(&mut self) {
        let _ = CURRENT_SENDER_STACK.try_with(|stack| {
            if let Ok(mut borrow) = stack.try_borrow_mut() {
                borrow.pop();
            }
        });
    }
}

/// Connection type matching Qt::ConnectionType.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ConnectionType {
    /// Direct connection: invoked synchronously in emitter's thread.
    #[default]
    Auto,
    /// Direct connection: invoked synchronously in caller's thread.
    Direct,
    /// Queued connection: posted into receiver's thread event loop.
    Queued,
    /// Blocking queued connection: caller thread blocks until receiver thread finishes execution.
    ///
    /// The wait ends when the receiver's loop has run the call, or when the call is dropped
    /// without running: the receiver was destroyed, its slot panicked, its loop ended or its
    /// thread ended. A receiver thread that is alive but does not run its event loop (it is
    /// stalled, or inside a long callback, or its loop waits on a lock the caller holds) keeps the
    /// caller waiting for as long as that lasts; there is no timeout or cancellation, as in Qt's
    /// `Qt::BlockingQueuedConnection`.
    BlockingQueued,
}

impl ConnectionType {
    pub fn is_auto(&self) -> bool {
        matches!(self, ConnectionType::Auto)
    }

    pub fn is_direct(&self) -> bool {
        matches!(self, ConnectionType::Direct)
    }

    pub fn is_queued(&self) -> bool {
        matches!(self, ConnectionType::Queued)
    }

    pub fn is_blocking_queued(&self) -> bool {
        matches!(self, ConnectionType::BlockingQueued)
    }
}
/// Unique connection identifier: ConnectionId.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConnectionId(pub u64);

impl ConnectionId {
    /// Returned when a connection is rejected; never allocated by [`ConnectionId::next`].
    pub const INVALID: ConnectionId = ConnectionId(0);

    /// `false` for [`ConnectionId::INVALID`].
    pub fn is_valid(&self) -> bool {
        self.0 != 0
    }

    /// Unique connection identifier: ConnectionId.
    pub fn next() -> Self {
        static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        ConnectionId(NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
    }
}

/// Unique slot identifier for [`SlotKey::Id`], allocated by an atomic counter rather than a
/// function pointer address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SlotId(pub u64);

impl SlotId {
    /// Allocates a new process-unique slot identifier.
    pub fn next() -> Self {
        static NEXT_SLOT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        SlotId(NEXT_SLOT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
    }
}

/// Explicit slot identity key for `UniqueConnection` deduplication
/// ([`Signal::connect_unique_with_key`]).
///
/// Comparing function pointer addresses (`fn(&T) as usize`) is not reliable in Rust: release
/// builds with identical-code folding (such as MSVC `/OPT:ICF`) can give two distinct functions
/// the same address, and Miri or multiple codegen units can give one function distinct addresses
/// across casts. `SlotKey` compares explicit values instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SlotKey {
    /// Process-unique numeric slot token (for example from [`SlotId::next`]).
    Id(SlotId),
    /// Named slot key compared by string content (`"ClassName::slot_name"`).
    Named(&'static str),
    /// Legacy function-pointer address key used only by deprecated [`Signal::connect_unique_to`].
    #[doc(hidden)]
    FnAddr(usize),
}

impl From<SlotId> for SlotKey {
    fn from(id: SlotId) -> Self {
        SlotKey::Id(id)
    }
}

impl From<&'static str> for SlotKey {
    fn from(name: &'static str) -> Self {
        SlotKey::Named(name)
    }
}
/// RAII handle that disconnects the connection when dropped (`QMetaObject::Connection`).
pub struct ScopedConnection {
    id: ConnectionId,
    disconnect_fn: Option<Box<dyn FnOnce(ConnectionId) + Send>>,
}

impl ScopedConnection {
    pub fn new<F>(id: ConnectionId, disconnect_fn: F) -> Self
    where
        F: FnOnce(ConnectionId) + Send + 'static,
    {
        Self {
            id,
            disconnect_fn: Some(Box::new(disconnect_fn)),
        }
    }

    pub fn id(&self) -> ConnectionId {
        self.id
    }

    pub fn release(mut self) -> ConnectionId {
        self.disconnect_fn = None;
        self.id
    }

    pub fn disconnect(mut self) {
        if let Some(f) = self.disconnect_fn.take() {
            f(self.id);
        }
    }
}

impl Drop for ScopedConnection {
    fn drop(&mut self) {
        if let Some(f) = self.disconnect_fn.take() {
            f(self.id);
        }
    }
}

struct ConnectionRecord {
    #[allow(dead_code)]
    id: ConnectionId,
    sender_id: Option<ObjectId>,
    receiver_id: Option<ObjectId>,
    disconnect_fn: Arc<dyn Fn() + Send + Sync>,
}

static GLOBAL_CONNECTIONS: std::sync::LazyLock<RwLock<HashMap<ConnectionId, ConnectionRecord>>> =
    std::sync::LazyLock::new(|| RwLock::new(HashMap::new()));

pub fn register_connection(
    id: ConnectionId,
    sender_id: Option<ObjectId>,
    receiver_id: Option<ObjectId>,
    disconnect_fn: Arc<dyn Fn() + Send + Sync>,
) {
    write_unpoisoned(&GLOBAL_CONNECTIONS).insert(
        id,
        ConnectionRecord {
            id,
            sender_id,
            receiver_id,
            disconnect_fn,
        },
    );
}

pub fn unregister_connection(id: ConnectionId) {
    write_unpoisoned(&GLOBAL_CONNECTIONS).remove(&id);
}

/// Drops subscribers removed from a signal (after its lock is released: a captured value's
/// destructor may use the signal) and retires their `GLOBAL_CONNECTIONS` entries.
fn retire_registry_entries<T>(removed: Vec<Subscriber<T>>) {
    let ids: Vec<ConnectionId> = removed.iter().map(|sub| sub.id).collect();
    drop(removed);
    if ids.is_empty() {
        return;
    }
    let mut conns = write_unpoisoned(&GLOBAL_CONNECTIONS);
    for id in ids {
        conns.remove(&id);
    }
}

pub fn disconnect_all_for_object(object_id: ObjectId) {
    let to_disconnect: Vec<Arc<dyn Fn() + Send + Sync>> = {
        let mut conns = write_unpoisoned(&GLOBAL_CONNECTIONS);
        let mut matched = Vec::new();
        conns.retain(|_, record| {
            if record.sender_id == Some(object_id) || record.receiver_id == Some(object_id) {
                matched.push(Arc::clone(&record.disconnect_fn));
                false
            } else {
                true
            }
        });
        matched
    };

    for disc in to_disconnect {
        disc();
    }
}

type QueuedSlotFn<T> =
    Arc<dyn Fn(&T, Option<ObjectId>, Option<ThreadId>, Option<ObjectId>) + Send + Sync + 'static>;

/// Internal slot invocation dispatcher for Direct vs Queued connections.
enum SlotDispatcher<T> {
    /// Direct slot callable synchronously. T does not need Send, Sync, Clone, or 'static.
    Direct(Arc<dyn Fn(&T) + Send + Sync + 'static>),
    /// Queued slot capable of cross-thread posting via cloned payload, while retaining direct invocation for same thread.
    Queued {
        direct_slot: Arc<dyn Fn(&T) + Send + Sync + 'static>,
        queued_fn: QueuedSlotFn<T>,
    },
}

impl<T> Clone for SlotDispatcher<T> {
    fn clone(&self) -> Self {
        match self {
            SlotDispatcher::Direct(s) => SlotDispatcher::Direct(Arc::clone(s)),
            SlotDispatcher::Queued {
                direct_slot,
                queued_fn,
            } => SlotDispatcher::Queued {
                direct_slot: Arc::clone(direct_slot),
                queued_fn: Arc::clone(queued_fn),
            },
        }
    }
}

/// Internal subscriber representation.
struct Subscriber<T> {
    id: ConnectionId,
    receiver_id: Option<ObjectId>,
    receiver_thread: Option<ThreadId>,
    conn_type: ConnectionType,
    dispatcher: SlotDispatcher<T>,
    /// Cleared the moment the connection is severed. `emit` re-checks it before every call,
    /// like `doActivate` re-checks `c->receiver` (qobject.cpp:4269).
    connected: Arc<AtomicBool>,
    /// Qt `UniqueConnection` key: explicit [`SlotKey`] identity; `None` for other connections.
    slot_key: Option<SlotKey>,
    single_shot: bool,
}

impl<T> Clone for Subscriber<T> {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            receiver_id: self.receiver_id,
            receiver_thread: self.receiver_thread,
            conn_type: self.conn_type,
            dispatcher: self.dispatcher.clone(),
            connected: Arc::clone(&self.connected),
            slot_key: self.slot_key,
            single_shot: self.single_shot,
        }
    }
}

/// Strongly typed signal (`Signal<T>`). Modeled after Qt signals.
///
/// Unlike standard Rust channels, direct connections in Qt operate synchronously within
/// the caller/emitter's execution context and do **not** require payload types `T`
/// to implement `Send`, `Sync`, `Clone`, or have `'static` lifetime bounds.
///
/// For asynchronous cross-thread or queued connections (`ConnectionType::Queued` / `BlockingQueued`),
/// methods require `T: Clone + Send + 'static`.
pub struct Signal<T> {
    inner: Arc<Mutex<SignalInner<T>>>,
    emitter: Option<Emitter>,
}

/// The QObject that owns a bound signal. `blocked` is the owner's own `signals_blocked` flag
/// (Qt `QObjectPrivate::blockSig`), shared rather than looked up in the registry, so blocking
/// works for unregistered owners and survives unregistration. `alive` is the owner's liveness
/// token (`ObjectData::liveness`), the flag `QPointer` reads: it goes false when the owner is
/// destroyed. Holding it does not keep the owner alive, only the flag.
#[derive(Clone)]
struct Emitter {
    id: ObjectId,
    blocked: Arc<AtomicBool>,
    alive: Arc<AtomicBool>,
}

/// Type alias for signals explicitly dedicated to queued or cross-thread messaging.
pub type QueuedSignal<T> = Signal<T>;

struct SignalInner<T> {
    subscribers: Vec<Subscriber<T>>,
}

impl<T> SignalInner<T> {
    /// Removes every subscriber matching `pred` and marks it severed, so an emission that already
    /// snapshotted it skips it. Returns the removed subscribers: the caller drops them after it
    /// has released the signal's lock, because a subscriber's captured state may use the signal
    /// when it is dropped.
    #[must_use]
    fn remove_where(&mut self, mut pred: impl FnMut(&Subscriber<T>) -> bool) -> Vec<Subscriber<T>> {
        let (removed, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.subscribers)
            .into_iter()
            .partition(|sub| pred(sub));
        for sub in &removed {
            sub.connected.store(false, Ordering::Release);
        }
        self.subscribers = kept;
        removed
    }
}

impl<T> Clone for Signal<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            emitter: self.emitter.clone(),
        }
    }
}

impl<T> Default for Signal<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Signal<T> {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(SignalInner {
                subscribers: Vec::new(),
            })),
            emitter: None,
        }
    }

    /// Creates a signal owned by `owner`, matching a QObject's own signal.
    /// Emission is suppressed while `owner` has signals blocked (`QObject::blockSignals`), whether
    /// or not `owner` is registered. Slots see `owner` as `sender()`. An emission ends as soon as
    /// `owner` is destroyed (see [`Signal::emit`]).
    pub fn with_owner(owner: &ObjectData) -> Self {
        Self::with_owner_parts(
            owner.id(),
            Arc::clone(&owner.signals_blocked),
            Arc::clone(&owner.liveness),
        )
    }

    /// Creates a signal owned by an explicit `(ObjectId, signals_blocked, liveness)` triple. The
    /// last two are the owner's `ObjectData::signals_blocked` and `ObjectData::liveness`.
    pub fn with_owner_parts(
        id: ObjectId,
        blocked: Arc<AtomicBool>,
        alive: Arc<AtomicBool>,
    ) -> Self {
        Self {
            inner: Arc::new(Mutex::new(SignalInner {
                subscribers: Vec::new(),
            })),
            emitter: Some(Emitter { id, blocked, alive }),
        }
    }

    fn emitter_id(&self) -> Option<ObjectId> {
        self.emitter.as_ref().map(|e| e.id)
    }

    /// Internal subscriber representation.
    pub fn subscriber_count(&self) -> usize {
        self.inner.lock().subscribers.len()
    }

    /// Internal subscriber representation.
    pub fn is_empty(&self) -> bool {
        self.inner.lock().subscribers.is_empty()
    }

    /// Connects a direct slot executed synchronously when the signal is emitted.
    ///
    /// This connection does **not** require `T` to be `Send`, `Sync`, `Clone`, or `'static`.
    pub fn connect<F>(&self, slot: F) -> ConnectionId
    where
        F: Fn(&T) + Send + Sync + 'static,
    {
        self.connect_with_type(ConnectionType::Direct, slot)
    }

    /// Connects a slot with an explicit direct connection type.
    ///
    /// Queued and blocking-queued connections are rejected here: this entry point names no
    /// receiver thread. The call prints a diagnostic and returns [`ConnectionId::INVALID`]; use
    /// `connect_queued` / `connect_blocking_queued` instead.
    pub fn connect_with_type<F>(&self, conn_type: ConnectionType, slot: F) -> ConnectionId
    where
        F: Fn(&T) + Send + Sync + 'static,
    {
        if conn_type.is_queued() || conn_type.is_blocking_queued() {
            eprintln!(
                "connect_with_type requires a receiver thread for queued connections; use connect_queued or connect_blocking_queued"
            );
            return ConnectionId::INVALID;
        }
        let mut inner = self.inner.lock();
        let id = ConnectionId::next();

        inner.subscribers.push(Subscriber {
            id,
            receiver_id: None,
            receiver_thread: None,
            conn_type,
            connected: Arc::new(AtomicBool::new(true)),
            slot_key: None,
            single_shot: false,
            dispatcher: SlotDispatcher::Direct(Arc::new(slot)),
        });

        id
    }

    /// Connects a direct slot that runs at most once: the connection is removed before its first
    /// invocation (Qt `Qt::SingleShotConnection`), so a re-entrant emit does not call it again.
    pub fn connect_single_shot<F>(&self, slot: F) -> ConnectionId
    where
        F: Fn(&T) + Send + Sync + 'static,
    {
        let mut inner = self.inner.lock();
        let id = ConnectionId::next();

        inner.subscribers.push(Subscriber {
            id,
            receiver_id: None,
            receiver_thread: None,
            conn_type: ConnectionType::Direct,
            connected: Arc::new(AtomicBool::new(true)),
            dispatcher: SlotDispatcher::Direct(Arc::new(slot)),
            slot_key: None,
            single_shot: true,
        });

        id
    }

    /// Disconnects a connection by its ID.
    pub fn disconnect(&self, id: ConnectionId) -> bool {
        // The removed subscribers are dropped after the lock is released: a captured value's
        // destructor may use this signal.
        let removed = self.inner.lock().remove_where(|sub| sub.id == id);
        let found = !removed.is_empty();
        drop(removed);
        // Only the owner of a connection may retire its registry entry.
        if found {
            unregister_connection(id);
        }
        found
    }

    /// Disconnects all connections bound to a given receiver object ID.
    pub fn disconnect_receiver(&self, receiver_id: ObjectId) -> usize {
        let removed = self
            .inner
            .lock()
            .remove_where(|sub| sub.receiver_id == Some(receiver_id));
        let count = removed.len();
        retire_registry_entries(removed);
        count
    }

    /// Disconnects all connections from this signal.
    pub fn disconnect_all(&self) {
        let removed = self.inner.lock().remove_where(|_| true);
        retire_registry_entries(removed);
    }

    /// Emits the signal to all connected subscribers.
    /// Modeled after Qt `qobject.cpp:doActivate`.
    ///
    /// A signal that has an owner (`Signal::with_owner`) stops when its owner is destroyed, even
    /// by one of its own slots: the slots after that one are not called, however they were
    /// connected (`connect`, `connect_object`, queued or not), and an emission started on an owner
    /// that is already gone calls none. In Qt `~QObject` disconnects every receiver and zeroes the
    /// connection counter before it deletes the children, and `doActivate` leaves its loop
    /// (qobject.cpp:1086-1178, 4431). An owner counts as gone from the first step of its
    /// destruction. A nested emission ends only the emissions whose owner was destroyed; the
    /// others go on. A signal without an owner has nothing to lose and always runs to the end.
    ///
    /// Once the owner is found gone `self` is not read again: the slot that destroyed it may have
    /// freed the object this signal is a field of.
    ///
    /// # Owner already destroyed before `emit` starts
    /// When a cloned `Signal<T>` outlives its owner (after `ObjectData::drop` or
    /// `unregister_qobject` sets the shared `liveness` token to `false`), calling `emit` invokes
    /// no slot and returns `()` silently without panicking. This differs from PySide6 6.11.2,
    /// which raises `RuntimeError: Signal source has been deleted` when `.emit()` is called on a
    /// `SignalInstance` whose C++ `QObject` has been deleted (while C++ Qt has no safe way to call
    /// a member on a deleted `QObject*`). Returning `()` keeps `emit(&self, &T)` infallible across
    /// all callers and is identical on Windows, macOS, and Linux because `liveness` is an
    /// OS-independent `Arc<AtomicBool>`.
    pub fn emit(&self, value: &T) {
        self.emit_impl(value, true);
    }

    /// Emits the signal of an owner that is being destroyed: the owner's blocked flag and its
    /// liveness token are not consulted, and the emission does not end when the owner is gone.
    /// This is how `destroyed` is delivered (Qt `~QObject` clears `blockSig` before it emits
    /// `destroyed`, qobject.cpp:1086-1105); every other signal uses [`Signal::emit`].
    pub(crate) fn emit_while_owner_dying(&self, value: &T) {
        self.emit_impl(value, false);
    }

    fn emit_impl(&self, value: &T, honor_owner_state: bool) {
        if honor_owner_state {
            if let Some(emitter) = &self.emitter {
                if emitter.blocked.load(Ordering::Acquire)
                    || !emitter.alive.load(Ordering::Acquire)
                {
                    return;
                }
            }
        }
        let emitter_id = self.emitter_id();
        let owner_alive = self
            .emitter
            .as_ref()
            .filter(|_| honor_owner_state)
            .map(|e| Arc::clone(&e.alive));
        let _sender_guard = SenderGuard::new(emitter_id);

        let snapshot = self.inner.lock().subscribers.clone();
        let current_thread = ThreadId::current();

        for sub in snapshot {
            if owner_alive
                .as_ref()
                .is_some_and(|alive| !alive.load(Ordering::Acquire))
            {
                break;
            }
            if !sub.connected.load(Ordering::Acquire) {
                continue;
            }
            // Qt `doActivate` removes a single-shot connection before invoking it; a re-entrant
            // emit then cannot reach the slot again. `disconnect` also retires the registry entry.
            if sub.single_shot && !self.disconnect(sub.id) {
                continue;
            }
            // Resolve the receiver thread at emit time: a receiver moved by `move_to_thread` after
            // connect must be reached on its current thread. The connect-time value is only a fallback.
            let target_thread = sub
                .receiver_id
                .and_then(query_object_thread)
                .or(sub.receiver_thread);

            let is_same_thread = target_thread.map_or(true, |thread| thread == current_thread);

            // Python/PySide6 isolates a raising slot (sys.excepthook) and continues with the
            // remaining slots; Qt's doActivate has no such containment. The panic hook logs it.
            let _ = catch_unwind(AssertUnwindSafe(|| match sub.conn_type {
                ConnectionType::Direct => match &sub.dispatcher {
                    SlotDispatcher::Direct(slot) => slot(value),
                    SlotDispatcher::Queued { direct_slot, .. } => {
                        direct_slot(value);
                    }
                },
                ConnectionType::Auto if is_same_thread => match &sub.dispatcher {
                    SlotDispatcher::Direct(slot) => slot(value),
                    SlotDispatcher::Queued { direct_slot, .. } => {
                        direct_slot(value);
                    }
                },
                _ => match &sub.dispatcher {
                    SlotDispatcher::Direct(slot) => {
                        slot(value);
                    }
                    SlotDispatcher::Queued { queued_fn, .. } => {
                        queued_fn(value, sub.receiver_id, target_thread, emitter_id);
                    }
                },
            }));
        }
    }
}

impl<T: 'static> Signal<T> {
    /// RAII connection guard: automatically disconnects when dropped.
    pub fn connect_scoped<F>(&self, slot: F) -> ScopedConnection
    where
        F: Fn(&T) + Send + Sync + 'static,
    {
        let id = self.connect(slot);
        ScopedConnection::new(id, self.scoped_disconnector())
    }

    /// The disconnect step of a [`ScopedConnection`] of this signal. It holds the signal weakly,
    /// so a scoped connection does not keep its signal alive.
    pub(super) fn scoped_disconnector(&self) -> impl FnOnce(ConnectionId) + Send + 'static {
        let signal_weak = Arc::downgrade(&self.inner);
        move |conn_id| {
            if let Some(inner_arc) = signal_weak.upgrade() {
                let removed = inner_arc.lock().remove_where(|sub| sub.id == conn_id);
                retire_registry_entries(removed);
            }
        }
    }
    /// Connects directly to a receiver object with automatic disconnect on receiver drop,
    /// without requiring `T` to be `Send`, `Sync`, or `Clone`.
    pub fn connect_direct_object<F>(&self, receiver_id: ObjectId, slot: F) -> ConnectionId
    where
        F: Fn(&T) + Send + Sync + 'static,
    {
        let mut inner = self.inner.lock();
        let id = ConnectionId::next();

        inner.subscribers.push(Subscriber {
            id,
            receiver_id: Some(receiver_id),
            receiver_thread: None,
            conn_type: ConnectionType::Direct,
            connected: Arc::new(AtomicBool::new(true)),
            slot_key: None,
            single_shot: false,
            dispatcher: SlotDispatcher::Direct(Arc::new(slot)),
        });

        let inner_weak = Arc::downgrade(&self.inner);
        let disconnect_fn = Arc::new(move || {
            if let Some(inner_arc) = inner_weak.upgrade() {
                let removed = inner_arc.lock().remove_where(|sub| sub.id == id);
                drop(removed);
            }
        });
        register_connection(id, self.emitter_id(), Some(receiver_id), disconnect_fn);

        id
    }

    /// Connects directly to a receiver QObject with automatic disconnect on receiver drop,
    /// without requiring `T` to be `Send`, `Sync`, or `Clone`.
    pub fn connect_direct_to<R: QObject + ?Sized, F>(&self, receiver: &R, slot: F) -> ConnectionId
    where
        F: Fn(&T) + Send + Sync + 'static,
    {
        let id = receiver.object_data().id();
        self.connect_direct_object(id, slot)
    }

    /// Connects this signal to `target` (Qt `connect(sender, signal, target_signal)`): every
    /// emission re-emits `target` with the same value. The connection is direct, so `target`'s
    /// slots run inside this emit; `sender()` seen by them is the owner of `target`, as in Qt's
    /// `doActivate` for the second signal.
    pub fn connect_signal(&self, target: &Signal<T>) -> ConnectionId {
        let target = target.clone();
        self.connect(move |value: &T| target.emit(value))
    }
}

impl<T: Clone + Send + 'static> Signal<T> {
    /// Connects to a receiver object with explicit thread context and connection type.
    ///
    /// Requires `T: Clone + Send + 'static` to permit queued cross-thread dispatch.
    pub fn connect_object<F>(
        &self,
        receiver_id: ObjectId,
        receiver_thread: ThreadId,
        conn_type: ConnectionType,
        slot: F,
    ) -> ConnectionId
    where
        F: Fn(&T) + Send + Sync + 'static,
    {
        self.connect_object_impl(receiver_id, receiver_thread, conn_type, None, false, slot)
            .expect("a non-unique connection is always established")
    }

    /// Qt `UniqueConnection` (`qobject.cpp:5517-5530`) using an explicit [`SlotKey`] instead of a
    /// function-pointer address: when `receiver` already has `key` connected to this signal,
    /// nothing is connected and `None` is returned.
    pub fn connect_unique_with_key<R: QObject + ?Sized, K: Into<SlotKey>, F>(
        &self,
        receiver: &R,
        key: K,
        slot: F,
    ) -> Option<ConnectionId>
    where
        F: Fn(&T) + Send + Sync + 'static,
    {
        let receiver_id = receiver.object_data().id();
        let thread = receiver.object_data().thread_id();
        self.connect_object_impl(
            receiver_id,
            thread,
            ConnectionType::Auto,
            Some(key.into()),
            false,
            slot,
        )
    }

    /// Qt `UniqueConnection` (`qobject.cpp:5517-5530`): when `receiver` already has this `slot`
    /// connected to this signal, nothing is connected and `None` is returned. Qt only accepts
    /// unique connections with a receiver and a member slot (`qobject.cpp:5510-5513`); a function
    /// pointer plays the member-slot role here.
    #[deprecated(
        note = "comparing function pointers by address (`fn(&T) as usize`) can merge distinct \
                functions under release identical-code folding (such as MSVC `/OPT:ICF`) and can \
                produce different addresses across casts under Miri; use \
                `connect_unique_with_key` with an explicit `SlotId` or `SlotKey`"
    )]
    pub fn connect_unique_to<R: QObject + ?Sized>(
        &self,
        receiver: &R,
        slot: fn(&T),
    ) -> Option<ConnectionId> {
        let receiver_id = receiver.object_data().id();
        let thread = receiver.object_data().thread_id();
        self.connect_object_impl(
            receiver_id,
            thread,
            ConnectionType::Auto,
            Some(SlotKey::FnAddr(slot as usize)),
            false,
            slot,
        )
    }
    /// Qt `SingleShotConnection` with a receiver: removed before its slot runs, so a re-entrant
    /// emit cannot invoke it twice (`qobject.cpp:4362-4382`).
    pub fn connect_single_shot_to<R: QObject + ?Sized, F>(
        &self,
        receiver: &R,
        slot: F,
    ) -> ConnectionId
    where
        F: Fn(&T) + Send + Sync + 'static,
    {
        let receiver_id = receiver.object_data().id();
        let thread = receiver.object_data().thread_id();
        self.connect_object_impl(receiver_id, thread, ConnectionType::Auto, None, true, slot)
            .expect("a single-shot connection is never rejected as duplicate")
    }

    /// Shared body of the receiver-bound connections. `unique` = `Some(slot_key)` rejects an
    /// existing connection with the same receiver and slot key (checked under the same lock).
    fn connect_object_impl<F>(
        &self,
        receiver_id: ObjectId,
        receiver_thread: ThreadId,
        conn_type: ConnectionType,
        unique: Option<SlotKey>,
        single_shot: bool,
        slot: F,
    ) -> Option<ConnectionId>
    where
        F: Fn(&T) + Send + Sync + 'static,
    {
        let mut inner = self.inner.lock();
        if let Some(key) = unique {
            if inner
                .subscribers
                .iter()
                .any(|sub| sub.receiver_id == Some(receiver_id) && sub.slot_key == Some(key))
            {
                return None;
            }
        }
        let id = ConnectionId::next();

        let slot_arc = Arc::new(slot);
        let slot_for_queued = Arc::clone(&slot_arc);

        let direct_slot = Arc::clone(&slot_arc);
        let queued_fn = Arc::new(
            move |value: &T,
                  target_receiver: Option<ObjectId>,
                  dest_thread: Option<ThreadId>,
                  emitter: Option<ObjectId>| {
                let value_clone = value.clone();
                let slot = Arc::clone(&slot_for_queued);
                let receiver_id = target_receiver.unwrap_or(ObjectId(0));

                let meta_event = Event::new(EventKind::MetaCallNoTarget(Box::new(move || {
                    let _inner_guard = SenderGuard::new(emitter);
                    slot(&value_clone);
                })));

                if let Some(dest) = dest_thread {
                    post_event_to_thread(dest, receiver_id, meta_event);
                } else {
                    post_event_to_thread(ThreadId::current(), receiver_id, meta_event);
                }
            },
        );
        let dispatcher = SlotDispatcher::Queued {
            direct_slot,
            queued_fn,
        };

        inner.subscribers.push(Subscriber {
            id,
            receiver_id: Some(receiver_id),
            receiver_thread: Some(receiver_thread),
            conn_type,
            connected: Arc::new(AtomicBool::new(true)),
            dispatcher,
            slot_key: unique,
            single_shot,
        });

        let inner_weak = Arc::downgrade(&self.inner);
        let disconnect_fn = Arc::new(move || {
            if let Some(inner_arc) = inner_weak.upgrade() {
                let removed = inner_arc.lock().remove_where(|sub| sub.id == id);
                drop(removed);
            }
        });
        register_connection(id, self.emitter_id(), Some(receiver_id), disconnect_fn);

        Some(id)
    }

    /// Connects to a receiver QObject with automatic thread affinity tracking.
    ///
    /// When emitted from the same thread, slot is invoked directly.
    /// When emitted from a different thread, automatically queued into receiver's thread event loop.
    pub fn connect_to<R: QObject + ?Sized, F>(&self, receiver: &R, slot: F) -> ConnectionId
    where
        F: Fn(&T) + Send + Sync + 'static,
    {
        let id = receiver.object_data().id();
        let thread = receiver.object_data().thread_id();
        self.connect_object(id, thread, ConnectionType::Auto, slot)
    }

    /// Connects to a receiver with automatic dispatch: direct on same thread, queued across threads.
    pub fn connect_auto<F>(
        &self,
        receiver_id: ObjectId,
        receiver_thread: ThreadId,
        slot: F,
    ) -> ConnectionId
    where
        F: Fn(&T) + Send + Sync + 'static,
    {
        self.connect_object(receiver_id, receiver_thread, ConnectionType::Auto, slot)
    }

    /// Queued connection: posted as MetaCall event to receiver thread's event loop.
    pub fn connect_queued<F>(&self, receiver_thread: ThreadId, slot: F) -> ConnectionId
    where
        F: Fn(&T) + Send + Sync + 'static,
    {
        self.connect_object(ObjectId(0), receiver_thread, ConnectionType::Queued, slot)
    }

    /// Blocking queued connection: emitter thread will block until the receiver's event loop finishes executing slot.
    ///
    /// `emit` returns once the slot has run, or once the posted call is dropped without running:
    /// the receiver was destroyed, the slot panicked, or the receiver's loop or thread ended
    /// (the call is dropped with the loop's queue, as Qt deletes the posted events of a finished
    /// thread, which releases the blocked caller). It does not return while the receiver's
    /// thread is alive but does not run its event loop; see
    /// [`ConnectionType::BlockingQueued`]. When the receiver lives on the emitting thread the
    /// slot runs in place and nothing waits (Qt reports a dead lock for that combination).
    pub fn connect_blocking_queued<F>(
        &self,
        receiver_id: ObjectId,
        receiver_thread: ThreadId,
        slot: F,
    ) -> ConnectionId
    where
        F: Fn(&T) + Send + Sync + 'static,
    {
        let mut inner = self.inner.lock();
        let id = ConnectionId::next();

        let slot_arc = Arc::new(slot);
        let slot_for_blocking = Arc::clone(&slot_arc);

        let direct_slot = Arc::clone(&slot_arc);
        let queued_fn = Arc::new(
            move |value: &T,
                  target_receiver: Option<ObjectId>,
                  dest_thread: Option<ThreadId>,
                  emitter: Option<ObjectId>| {
                let current_thread = ThreadId::current();
                let is_same_thread = dest_thread.map_or(true, |d| d == current_thread);
                if is_same_thread {
                    let _inner_guard = SenderGuard::new(emitter);
                    slot_for_blocking(value);
                } else {
                    let value_clone = value.clone();
                    let slot = Arc::clone(&slot_for_blocking);
                    let receiver_id = target_receiver.unwrap_or(ObjectId(0));
                    let (tx, rx) = std::sync::mpsc::channel();

                    let meta_event = Event::new(EventKind::MetaCallNoTarget(Box::new(move || {
                        let _inner_guard = SenderGuard::new(emitter);
                        slot(&value_clone);
                        let _ = tx.send(());
                    })));

                    if let Some(dest) = dest_thread {
                        post_event_to_thread(dest, receiver_id, meta_event);
                    } else {
                        post_event_to_thread(ThreadId::current(), receiver_id, meta_event);
                    }

                    let _ = rx.recv();
                }
            },
        );
        let dispatcher = SlotDispatcher::Queued {
            direct_slot,
            queued_fn,
        };

        inner.subscribers.push(Subscriber {
            id,
            receiver_id: Some(receiver_id),
            receiver_thread: Some(receiver_thread),
            conn_type: ConnectionType::BlockingQueued,
            connected: Arc::new(AtomicBool::new(true)),
            slot_key: None,
            single_shot: false,
            dispatcher,
        });

        let inner_weak = Arc::downgrade(&self.inner);
        let disconnect_fn = Arc::new(move || {
            if let Some(inner_arc) = inner_weak.upgrade() {
                let removed = inner_arc.lock().remove_where(|sub| sub.id == id);
                drop(removed);
            }
        });
        register_connection(id, self.emitter_id(), Some(receiver_id), disconnect_fn);

        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr::NonNull;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn test_connection_type_defaults_and_predicates() {
        let default_conn = ConnectionType::default();
        assert_eq!(default_conn, ConnectionType::Auto);
        assert!(default_conn.is_auto());
        assert!(!default_conn.is_direct());
        assert!(!default_conn.is_queued());

        let direct = ConnectionType::Direct;
        assert!(direct.is_direct());
        assert!(!direct.is_auto());

        let queued = ConnectionType::Queued;
        assert!(queued.is_queued());
        assert!(!queued.is_auto());
    }

    #[test]
    fn test_connection_id_unique() {
        let id1 = ConnectionId::next();
        let id2 = ConnectionId::next();
        assert_ne!(id1, id2);
        assert!(id2 > id1);
    }

    #[test]
    fn test_scoped_connection_raii_drop() {
        let disconnected = Arc::new(AtomicBool::new(false));
        let disc_clone = Arc::clone(&disconnected);
        let id = ConnectionId::next();

        {
            let _conn = ScopedConnection::new(id, move |conn_id| {
                assert_eq!(conn_id, id);
                disc_clone.store(true, Ordering::Release);
            });

            assert!(!disconnected.load(Ordering::Acquire));
        }

        assert!(disconnected.load(Ordering::Acquire));
    }

    #[test]
    fn test_scoped_connection_release() {
        let disconnected = Arc::new(AtomicBool::new(false));
        let disc_clone = Arc::clone(&disconnected);
        let id = ConnectionId::next();

        {
            let conn = ScopedConnection::new(id, move |_| {
                disc_clone.store(true, Ordering::Release);
            });
            let released_id = conn.release();
            assert_eq!(released_id, id);
        }

        assert!(!disconnected.load(Ordering::Acquire));
    }

    #[test]
    fn test_scoped_connection_manual_disconnect() {
        let disconnected = Arc::new(AtomicBool::new(false));
        let disc_clone = Arc::clone(&disconnected);
        let id = ConnectionId::next();

        let conn = ScopedConnection::new(id, move |_| {
            disc_clone.store(true, Ordering::Release);
        });
        assert!(!disconnected.load(Ordering::Acquire));

        conn.disconnect();
        assert!(disconnected.load(Ordering::Acquire));
    }

    #[test]
    fn test_signal_connect_and_disconnect() {
        let signal: Signal<i32> = Signal::new();
        assert_eq!(signal.subscriber_count(), 0);
        assert!(signal.is_empty());

        let id1 = signal.connect(|val| {
            let _ = val;
        });
        assert_eq!(signal.subscriber_count(), 1);
        assert!(!signal.is_empty());

        let id2 = signal.connect_with_type(ConnectionType::Direct, |_| {});
        assert_eq!(signal.subscriber_count(), 2);

        let disc = signal.disconnect(id1);
        assert!(disc);
        assert_eq!(signal.subscriber_count(), 1);

        assert!(!signal.disconnect(id1));

        assert!(signal.disconnect(id2));
        assert_eq!(signal.subscriber_count(), 0);
    }

    #[test]
    fn test_signal_disconnect_receiver() {
        let signal: Signal<u64> = Signal::new();
        let receiver1 = ObjectId::next();
        let receiver2 = ObjectId::next();
        let thread_id = ThreadId::current();

        signal.connect_object(receiver1, thread_id, ConnectionType::Auto, |_| {});
        signal.connect_object(receiver1, thread_id, ConnectionType::Direct, |_| {});
        signal.connect_object(receiver2, thread_id, ConnectionType::Queued, |_| {});
        assert_eq!(signal.subscriber_count(), 3);

        let count = signal.disconnect_receiver(receiver1);
        assert_eq!(count, 2);
        assert_eq!(signal.subscriber_count(), 1);

        signal.disconnect_all();
        assert_eq!(signal.subscriber_count(), 0);
    }

    #[test]
    fn test_signal_emit_direct_and_auto_same_thread() {
        let signal: Signal<i32> = Signal::new();
        let received_auto = Arc::new(Mutex::new(Vec::new()));
        let received_direct = Arc::new(Mutex::new(Vec::new()));

        let a_clone = Arc::clone(&received_auto);
        signal.connect(move |val| {
            a_clone.lock().push(*val);
        });

        let d_clone = Arc::clone(&received_direct);
        signal.connect_with_type(ConnectionType::Direct, move |val| {
            d_clone.lock().push(*val);
        });

        signal.emit(&42);
        signal.emit(&100);

        assert_eq!(*received_auto.lock(), vec![42, 100]);
        assert_eq!(*received_direct.lock(), vec![42, 100]);
    }

    #[test]
    fn test_signal_emit_highest_id_protection() {
        let signal: Signal<i32> = Signal::new();
        let signal_clone = signal.clone();

        let dynamic_called = Arc::new(AtomicBool::new(false));
        let dyn_clone = Arc::clone(&dynamic_called);

        let round1_count = Arc::new(Mutex::new(0));
        let r1_clone = Arc::clone(&round1_count);

        signal.connect(move |_val| {
            *r1_clone.lock() += 1;

            let dyn_target = Arc::clone(&dyn_clone);
            signal_clone.connect(move |_| {
                dyn_target.store(true, Ordering::Release);
            });
        });

        signal.emit(&1);
        assert_eq!(*round1_count.lock(), 1);

        assert!(!dynamic_called.load(Ordering::Acquire));

        signal.emit(&2);
        assert_eq!(*round1_count.lock(), 2);
        assert!(dynamic_called.load(Ordering::Acquire));

        // Break strong reference cycle formed by subscriber closure capturing signal_clone
        signal.disconnect_all();
    }

    #[test]
    fn test_signal_emit_queued_dispatch() {
        use crate::event_loop::EventLoop;

        let signal: Signal<String> = Signal::new();
        struct Receiver {
            data: crate::object::ObjectData,
        }
        impl crate::object::QObject for Receiver {
            fn object_data(&self) -> &crate::object::ObjectData {
                &self.data
            }
            fn object_data_mut(&mut self) -> crate::object::ObjectDataMut<'_> {
        crate::object::ObjectDataMut::new(&mut self.data)
    }
        }
        let mut receiver = Receiver {
            data: crate::object::ObjectData::new(ObjectId::next()),
        };
        let receiver_id = receiver.data.id();
        // SAFETY: registered for the test; unregistered before it returns.
        unsafe { crate::object::register_qobject_ptr(NonNull::new_unchecked(&raw mut receiver)) };
        let target_thread = ThreadId::current();

        let received_msg = Arc::new(Mutex::new(String::new()));
        let msg_clone = Arc::clone(&received_msg);

        let event_loop = EventLoop::new();

        signal.connect_object(
            receiver_id,
            target_thread,
            ConnectionType::Queued,
            move |msg| {
                *msg_clone.lock() = msg.clone();
            },
        );

        signal.emit(&"hello queued".to_string());

        assert_eq!(*received_msg.lock(), "");

        let processed = event_loop.process_events(false);
        assert!(processed);

        assert_eq!(*received_msg.lock(), "hello queued");
        // SAFETY: `receiver` was registered above and is still alive.
        unsafe { crate::object::unregister_qobject(receiver_id) };
    }

    /// Bulk disconnects retire the connections' registry entries, as `disconnect` does.
    #[test]
    fn test_bulk_disconnect_retires_registry_entries() {
        let receiver_id = ObjectId::next();
        let registered = |id: ConnectionId| write_unpoisoned(&GLOBAL_CONNECTIONS).contains_key(&id);

        let signal = Signal::<i32>::new();
        let a = signal.connect_direct_object(receiver_id, |_| {});
        assert!(registered(a));
        assert_eq!(signal.disconnect_receiver(receiver_id), 1);
        assert!(!registered(a), "disconnect_receiver leaked the registry entry");

        let b = signal.connect_direct_object(receiver_id, |_| {});
        signal.disconnect_all();
        assert!(!registered(b), "disconnect_all leaked the registry entry");
    }

    #[test]
    fn test_basic_emission() {
        let signal = Signal::<i32>::new();
        let received = Arc::new(Mutex::new(Vec::new()));

        let r1 = received.clone();
        signal.connect(move |val| {
            r1.lock().push(*val * 2);
        });

        let r2 = received.clone();
        signal.connect(move |val| {
            r2.lock().push(*val + 1);
        });

        signal.emit(&10);

        assert_eq!(*received.lock(), vec![20, 11]);
    }

    #[test]
    fn test_scoped_connection_auto_disconnect() {
        let signal = Signal::<&str>::new();
        let count = Arc::new(AtomicUsize::new(0));
        assert_eq!(signal.subscriber_count(), 0);

        {
            let c = count.clone();

            let _guard = signal.connect_scoped(move |_| {
                c.fetch_add(1, Ordering::SeqCst);
            });
            assert_eq!(signal.subscriber_count(), 1);

            signal.emit(&"first");
            assert_eq!(count.load(Ordering::SeqCst), 1);
        }

        assert_eq!(signal.subscriber_count(), 0);
        signal.emit(&"second");
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_concurrent_emission() {
        let signal = Arc::new(Signal::<usize>::new());
        let sum = Arc::new(AtomicUsize::new(0));

        let s = sum.clone();
        signal.connect(move |val| {
            s.fetch_add(*val, Ordering::SeqCst);
        });

        let mut handles = vec![];

        for i in 1..=10 {
            let sig = signal.clone();
            handles.push(std::thread::spawn(move || {
                sig.emit(&i);
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        // 1 + 2 + ... + 10 = 55
        assert_eq!(sum.load(Ordering::SeqCst), 55);
    }
}
