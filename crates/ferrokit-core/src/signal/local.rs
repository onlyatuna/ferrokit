// SPDX-License-Identifier: LGPL-3.0-only
#![forbid(unsafe_code)]
//! Slots that are not `Send`: [`Signal::connect_local`] and its variants.
//!
//! [`Signal`] is a `Send + Sync + Clone` handle: any thread that holds a clone may emit it,
//! disconnect from it, or drop the last reference to it. [`Signal::connect`] therefore asks for a
//! `Send + Sync` slot. A closure that captures an `Rc`, a `RefCell` or any other state that
//! belongs to one thread (a widget tree, for example) cannot be given to it.
//!
//! `connect_local` keeps such a closure out of the signal. The closure is stored in a table that
//! belongs to the thread that connected it (a `thread_local!`), and the signal gets a
//! [`LocalHandle`] that holds only a key into that table and the connecting thread's id. The
//! handle is `Send + Sync` by construction, and no `unsafe` is involved (this module is
//! `#![forbid(unsafe_code)]`): another thread that follows the handle looks the key up in its own
//! table and finds nothing.
//!
//! # Differences from Qt
//!
//! * **Emitting from another thread skips the slot (Qt would run it).** A `DirectConnection`
//!   runs the slot in the emitting thread, whatever thread that is. A closure that is not `Send`
//!   cannot run anywhere but its own thread, so an emission from any other thread does not call
//!   it. The first skipped emission of each connection prints a diagnostic to stderr; later ones
//!   are silent. The emitter gets no error: [`Signal::emit`] returns `()`. Emit signals with local
//!   slots from the thread that connected them, or hop to that thread first. A single-shot local
//!   slot is not consumed by such an emission: the connection stays and the slot runs, once, on
//!   the first emission from the connecting thread.
//! * **The closure is dropped on the connecting thread, not on the thread that disconnects.**
//!   After `disconnect`, `disconnect_all`, a [`ScopedConnection`] drop, or the drop of the last
//!   clone of the signal on another thread, the slot never runs again at once, but its captures
//!   are dropped when the connecting thread next runs its event loop (the drop is posted there as
//!   a queued call), not inside the call that disconnected. Qt destroys the slot object in the
//!   disconnecting thread. The thread must run an event loop for that to happen; one that never
//!   does keeps the closure until it ends.
//! * **Thread exit.** When the connecting thread ends, its table is destroyed with the thread's
//!   other thread-locals, and the closures still in it are dropped then, on that thread, in a
//!   thread-local destructor. A capture whose destructor uses another thread-local (a widget
//!   that talks to a thread-local command queue, for example) may find it already destroyed; the
//!   order in which thread-local destructors run is unspecified. Disconnect local slots, and drop
//!   their signals, before the thread ends if their captures need that.
//! * `T: 'static`: the closure is stored type-erased. A payload that borrows (see
//!   `Signal::connect` with a `'a` payload) stays on [`Signal::connect`].
//! * **Context-bound connects** ([`Signal::connect_local_to`] and the variants that take a
//!   context object) put the slot on Qt's `connect(sender, signal, context, functor, type)`
//!   footing: the context is the receiver, and its thread decides `AutoConnection`
//!   (queued when the emitter is elsewhere, which needs `T: Clone + Send`). The context must live
//!   on the calling thread and must not be movable (a local slot cannot follow `moveToThread`);
//!   for `Auto`, `Queued` and `BlockingQueued` it must also be registered with the object
//!   registry, because a call posted to an unregistered receiver is dropped on delivery. A
//!   `Direct` connection still skips an emission from another thread, as above. Destroying the
//!   context disconnects the slot and drops a call that was posted but not delivered, and the
//!   closure is dropped on the context's thread. That holds for an unregistered context too (a
//!   `Direct` connection accepts one): its destruction cuts its connections, it just has no
//!   queued calls to drop.
//! * **`BlockingQueued` on the context's own thread runs the slot in place.** Qt prints "Dead
//!   lock detected" and then waits for itself forever. ferrokit runs the slot at once, as
//!   [`Signal::connect_blocking_queued`] does.
//!
//! # Invariants of this module
//!
//! * I1: the table is reached only through `LocalKey::try_with`; a destroyed table means "no
//!   slot" and never panics.
//! * I2: the table's `RefCell` is never borrowed while user code (a slot, or a closure's
//!   destructor) runs. A slot is cloned out as an `Rc` before it is called; a removed slot is
//!   dropped after the borrow ends.
//! * I3: what is posted to another thread is the key (`u64`) and nothing else.
//! * I4: keys come from one process-wide atomic counter and are never reused.

use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use crate::event::{Event, EventKind};
use crate::event_loop::post_event_to_thread;
use crate::object::qobject::registration_thread_of;
use crate::object::{ObjectId, QObject, ThreadId};

use super::signal::{ConnectionId, ConnectionType, ScopedConnection, Signal, SlotKey};

thread_local! {
    /// The slots connected on this thread with `connect_local*`, by key (I4).
    static LOCAL_SLOTS: RefCell<HashMap<u64, Rc<dyn Any>>> = RefCell::new(HashMap::new());
}

static NEXT_KEY: AtomicU64 = AtomicU64::new(1);

/// A slot of payload type `T`, as stored in the table.
struct LocalFn<T>(Box<dyn Fn(&T)>);

/// A one-shot closure, as stored in the table by [`LocalHandle::install_once`].
struct LocalOnce(RefCell<Option<Box<dyn FnOnce()>>>);

/// Names a slot in the table of the thread that connected it. It holds no closure, so it is
/// `Send + Sync` without `unsafe`: the signal's subscriber captures an `Arc` of it.
///
/// Dropping the last `Arc` retires the slot: on the connecting thread at once, anywhere else by
/// posting the key to that thread (I3).
pub(crate) struct LocalHandle {
    owner: ThreadId,
    key: u64,
    /// Set by the first emission from another thread, so the diagnostic is printed once.
    warned: AtomicBool,
}

impl LocalHandle {
    /// Stores `slot` in this thread's table and returns its handle.
    fn install<T: 'static>(slot: impl Fn(&T) + 'static) -> Arc<Self> {
        let key = NEXT_KEY.fetch_add(1, Ordering::Relaxed);
        let entry: Rc<dyn Any> = Rc::new(LocalFn::<T>(Box::new(slot)));
        // If the table is already destroyed (a thread-local destructor connects a slot), the entry
        // is dropped here, on the connecting thread, and the slot never runs.
        let _ = LOCAL_SLOTS.try_with(|table| table.borrow_mut().insert(key, entry));
        Arc::new(Self {
            owner: ThreadId::current(),
            key,
            warned: AtomicBool::new(false),
        })
    }

    /// Prints the foreign-emission diagnostic, the first time only.
    fn warn_foreign_emission(&self) {
        if !self.warned.swap(true, Ordering::Relaxed) {
            eprintln!(
                "ferrokit: a signal with a local slot (connect_local) was emitted from a \
                 thread other than the one that connected it; the slot is skipped"
            );
        }
    }

    /// Calls the slot if this is the connecting thread. Returns whether it ran.
    fn call<T: 'static>(&self, value: &T) -> bool {
        if ThreadId::current() != self.owner {
            self.warn_foreign_emission();
            return false;
        }
        // I1, I2: the `Rc` is cloned out, so the borrow ends before the slot runs and the slot
        // may connect, disconnect or emit. The clone also keeps a slot that disconnects itself
        // alive until it returns.
        let entry = LOCAL_SLOTS
            .try_with(|table| table.borrow().get(&self.key).cloned())
            .ok()
            .flatten();
        match entry
            .as_deref()
            .and_then(|e| e.downcast_ref::<LocalFn<T>>())
        {
            Some(slot) => {
                (slot.0)(value);
                true
            }
            None => false,
        }
    }

    /// Stores a one-shot `FnOnce` in this thread's table (the same table, the same keys, the same
    /// retirement as a slot) and returns its handle. Used by `Timer::single_shot_local`.
    pub(crate) fn install_once(f: impl FnOnce() + 'static) -> Self {
        let key = NEXT_KEY.fetch_add(1, Ordering::Relaxed);
        let entry: Rc<dyn Any> = Rc::new(LocalOnce(RefCell::new(Some(Box::new(f)))));
        let _ = LOCAL_SLOTS.try_with(|table| table.borrow_mut().insert(key, entry));
        Self {
            owner: ThreadId::current(),
            key,
            warned: AtomicBool::new(false),
        }
    }

    /// Runs the one-shot closure if this is the owning thread and it is still stored; the entry is
    /// removed first, so it runs at most once. Returns whether it ran.
    pub(crate) fn run_once(&self) -> bool {
        if ThreadId::current() != self.owner {
            return false;
        }
        // I1, I2: removed under a short borrow; the closure runs, and is dropped, outside it.
        let entry = LOCAL_SLOTS
            .try_with(|table| table.borrow_mut().remove(&self.key))
            .ok()
            .flatten();
        let Some(entry) = entry else { return false };
        let f = entry
            .downcast_ref::<LocalOnce>()
            .and_then(|once| once.0.borrow_mut().take());
        match f {
            Some(f) => {
                f();
                true
            }
            None => false,
        }
    }
}

/// Removes `key` from this thread's table and drops its closure outside the borrow (I2).
fn retire_here(key: u64) {
    let removed = LOCAL_SLOTS
        .try_with(|table| table.borrow_mut().remove(&key))
        .ok()
        .flatten();
    drop(removed);
}

impl Drop for LocalHandle {
    fn drop(&mut self) {
        if ThreadId::current() == self.owner {
            retire_here(self.key);
        } else {
            let key = self.key;
            // `ObjectId(0)` is the fire-and-forget receiver: no object, always delivered. An ended
            // owner thread drops the post, and its table was destroyed with it.
            post_event_to_thread(
                self.owner,
                ObjectId(0),
                Event::new(EventKind::MetaCallNoTarget(Box::new(move || {
                    retire_here(key)
                }))),
            );
        }
    }
}

impl<T: 'static> Signal<T> {
    /// Connects a slot that may capture state that is not `Send` (an `Rc`, a `RefCell`, a widget
    /// tree), as Qt's `connect(sender, signal, functor)` does with a functor of the current
    /// thread. The connection is direct: the slot runs inside `emit`.
    ///
    /// # Qt hangs (and UB, in Rust) here: emitting from another thread
    ///
    /// The slot runs only when the signal is emitted from the thread that called
    /// `connect_local`. An emission from any other thread **skips it** (Qt would run it in the
    /// emitting thread, which for a closure that is not `Send` would be undefined behavior in
    /// Rust). The first skipped emission of the connection prints a diagnostic to stderr. If a
    /// worker thread must reach such a slot, have it post to the connecting thread and emit
    /// there.
    ///
    /// # Dropping
    ///
    /// The closure is dropped on the connecting thread: when the connection is removed and the
    /// last reference to it goes (at once if that happens on this thread, otherwise at the next
    /// turn of this thread's event loop), or, if it was never removed, when this thread ends, in
    /// a thread-local destructor. A capture whose destructor needs another thread-local may find
    /// it destroyed there; disconnect before the thread ends. See the [module
    /// documentation](self) for the details.
    ///
    /// A panic in the slot is contained by `emit`, as for [`Signal::connect`].
    ///
    /// ```
    /// use std::cell::Cell;
    /// use std::rc::Rc;
    /// use ferrokit_core::signal::Signal;
    ///
    /// let signal: Signal<i32> = Signal::new();
    /// let total = Rc::new(Cell::new(0)); // `Rc<Cell<_>>` is not `Send`
    /// let sink = Rc::clone(&total);
    /// signal.connect_local(move |value| sink.set(sink.get() + *value));
    /// signal.emit(&4);
    /// assert_eq!(total.get(), 4);
    /// ```
    ///
    /// [`Signal::connect`] keeps its `Send + Sync` bound:
    ///
    /// ```compile_fail
    /// use std::rc::Rc;
    /// use ferrokit_core::signal::Signal;
    ///
    /// let signal: Signal<i32> = Signal::new();
    /// let state = Rc::new(0);
    /// signal.connect(move |_| {
    ///     let _ = &state;
    /// });
    /// ```
    pub fn connect_local<F>(&self, slot: F) -> ConnectionId
    where
        F: Fn(&T) + 'static,
    {
        let handle = LocalHandle::install::<T>(slot);
        self.connect_with_type(ConnectionType::Direct, move |value| {
            handle.call(value);
        })
    }

    /// [`Signal::connect_local`] with a guard: the connection is removed when the returned
    /// [`ScopedConnection`] is dropped (or disconnected). The guard is `Send`; dropping it on
    /// another thread removes the connection at once, and the closure is dropped on the
    /// connecting thread as described for `connect_local`.
    pub fn connect_local_scoped<F>(&self, slot: F) -> ScopedConnection
    where
        F: Fn(&T) + 'static,
    {
        let id = self.connect_local(slot);
        ScopedConnection::new(id, self.scoped_disconnector())
    }

    /// [`Signal::connect_local`] that runs at most once: the connection is removed before the
    /// slot is called (Qt `SingleShotConnection`), so a re-entrant emit does not call it again.
    ///
    /// An emission from another thread skips the connection and **does not consume it** (the
    /// signal leaves a thread-bound single-shot connection alone, with the diagnostic described
    /// at [`Signal::connect_local`]); the slot runs, exactly once, on the first emission from the
    /// connecting thread, and not again.
    pub fn connect_local_single_shot<F>(&self, slot: F) -> ConnectionId
    where
        F: Fn(&T) + 'static,
    {
        let handle = LocalHandle::install::<T>(slot);
        let owner = handle.owner;
        let skipped = Arc::clone(&handle);
        self.connect_single_shot_bound(
            owner,
            move || skipped.warn_foreign_emission(),
            move |value| {
                handle.call(value);
            },
        )
    }
}

/// Why a context-bound local connection was refused. Nothing was connected, and the closure was
/// not stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LocalConnectError {
    /// The context object belongs to another thread than the caller. A local slot runs on the
    /// thread that connected it, so that thread has to be the context's thread.
    WrongThread,
    /// The context opted in to `move_to_thread_by_id`
    /// ([`ObjectData::can_move_to_thread`](crate::object::ObjectData::can_move_to_thread)); it
    /// could leave the thread the slot is bound to.
    ContextMovable,
    /// An `Auto`, `Queued` or `BlockingQueued` connection needs a context that is registered on
    /// the calling thread; a call posted to any other receiver would be dropped on delivery.
    ContextNotRegistered,
}

impl std::fmt::Display for LocalConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongThread => write!(f, "the context object belongs to another thread"),
            Self::ContextMovable => write!(f, "the context object may be moved to another thread"),
            Self::ContextNotRegistered => {
                write!(
                    f,
                    "the context object is not registered on the calling thread"
                )
            }
        }
    }
}

impl std::error::Error for LocalConnectError {}

/// The checks every context-bound local connect makes before it stores a closure. Returns the
/// context's id and thread (the calling thread).
fn check_context<R: QObject + ?Sized>(
    context: &R,
    conn_type: ConnectionType,
) -> Result<(ObjectId, ThreadId), LocalConnectError> {
    let data = context.object_data();
    let current = ThreadId::current();
    if data.thread_id() != current {
        return Err(LocalConnectError::WrongThread);
    }
    if data.can_move_to_thread() {
        return Err(LocalConnectError::ContextMovable);
    }
    let id = data.id();
    if conn_type != ConnectionType::Direct && registration_thread_of(id) != Some(current) {
        return Err(LocalConnectError::ContextNotRegistered);
    }
    Ok((id, current))
}

impl<T: Clone + Send + 'static> Signal<T> {
    /// Qt `connect(sender, signal, context, functor, type)` for a closure that is not `Send`.
    /// `context` is the receiver: it names the thread that decides `Auto`, and the connection is
    /// cut when it is destroyed. The slot always runs on the calling thread, which has to be the
    /// context's thread.
    ///
    /// * `Auto`: direct when the emitting thread is the context's thread, else queued there.
    /// * `Queued`: always posted to the context's thread, also when the emitter is that thread.
    /// * `BlockingQueued`: as [`Signal::connect_local_blocking_queued`].
    /// * `Direct`: runs in the emitting thread, so an emission from another thread **skips the
    ///   slot** (Qt would run it there); see [`Signal::connect_local`].
    ///
    /// A queued call sees `sender()` of the emitter. `Auto` (when the emitter is on another
    /// thread) and `Queued` clone the payload once per queued slot per emission.
    ///
    /// Every method of this impl needs `T: Clone + Send`, even for `Direct`. A `Direct`-only
    /// user whose payload is `!Send` or not `Clone` should use [`Signal::connect_local`], which
    /// has no such bound (and no context).
    ///
    /// # Errors
    /// [`LocalConnectError`]; the context must be on this thread, not movable, and (unless
    /// `Direct`) registered.
    pub fn connect_local_to<R, F>(
        &self,
        context: &R,
        conn_type: ConnectionType,
        slot: F,
    ) -> Result<ConnectionId, LocalConnectError>
    where
        R: QObject + ?Sized,
        F: Fn(&T) + 'static,
    {
        if conn_type == ConnectionType::BlockingQueued {
            return self.connect_local_blocking_queued(context, slot);
        }
        let (id, thread) = check_context(context, conn_type)?;
        let handle = LocalHandle::install::<T>(slot);
        Ok(self
            .connect_object_impl(id, thread, conn_type, None, false, move |value| {
                handle.call(value);
            })
            .expect("a non-unique connection is always established"))
    }

    /// [`Signal::connect_local_to`] with `BlockingQueuedConnection`: an emission from another
    /// thread posts the call to the context's thread and **waits until the slot has run** (or
    /// the call was dropped unrun: context destroyed, loop ended). The context's thread must run
    /// its event loop, or the emitter waits for as long as it does not.
    ///
    /// **Deviation from Qt:** an emission on the context's own thread runs the slot in place;
    /// Qt warns "Dead lock detected" and then blocks forever
    /// (`qobject.cpp`, `BlockingQueuedConnection` in `doActivate`).
    ///
    /// # Errors
    /// As [`Signal::connect_local_to`].
    pub fn connect_local_blocking_queued<R, F>(
        &self,
        context: &R,
        slot: F,
    ) -> Result<ConnectionId, LocalConnectError>
    where
        R: QObject + ?Sized,
        F: Fn(&T) + 'static,
    {
        let (id, thread) = check_context(context, ConnectionType::BlockingQueued)?;
        let handle = LocalHandle::install::<T>(slot);
        Ok(self.connect_blocking_queued(id, thread, move |value| {
            handle.call(value);
        }))
    }

    /// Qt `UniqueConnection` for a local slot: when `context` already has a connection with this
    /// `key` on this signal, nothing is connected, the closure is dropped at once and `Ok(None)`
    /// is returned. The key stands in for Qt's member-function pointer, as for
    /// [`Signal::connect_unique_with_key`]. The type is `Auto`.
    ///
    /// # Errors
    /// As [`Signal::connect_local_to`] for `Auto`.
    pub fn connect_local_unique<R, K, F>(
        &self,
        context: &R,
        key: K,
        slot: F,
    ) -> Result<Option<ConnectionId>, LocalConnectError>
    where
        R: QObject + ?Sized,
        K: Into<SlotKey>,
        F: Fn(&T) + 'static,
    {
        let (id, thread) = check_context(context, ConnectionType::Auto)?;
        let handle = LocalHandle::install::<T>(slot);
        // On a duplicate the closure (and so the handle) is dropped after the signal's lock was
        // released, so its destructor may use this signal.
        Ok(self.connect_object_impl(
            id,
            thread,
            ConnectionType::Auto,
            Some(key.into()),
            false,
            move |value| {
                handle.call(value);
            },
        ))
    }

    /// [`Signal::connect_local_to`] that runs at most once (Qt `SingleShotConnection`): the
    /// connection is removed before the call, as the signal does for every single-shot. The
    /// type is `Auto`, so an emission from another thread is queued to the context's thread and
    /// consumes the connection at that moment; the slot then runs there, once.
    ///
    /// # Errors
    /// As [`Signal::connect_local_to`] for `Auto`.
    pub fn connect_local_single_shot_to<R, F>(
        &self,
        context: &R,
        slot: F,
    ) -> Result<ConnectionId, LocalConnectError>
    where
        R: QObject + ?Sized,
        F: Fn(&T) + 'static,
    {
        let (id, thread) = check_context(context, ConnectionType::Auto)?;
        let handle = LocalHandle::install::<T>(slot);
        Ok(self
            .connect_object_impl(id, thread, ConnectionType::Auto, None, true, move |value| {
                handle.call(value);
            })
            .expect("a single-shot connection is never rejected as duplicate"))
    }
}
