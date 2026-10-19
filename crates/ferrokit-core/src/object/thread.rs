// SPDX-License-Identifier: LGPL-3.0-only
use crate::event::{Event, EventKind};
use crate::object::qobject::ObjectData;
use crate::object::ObjectId;
use crate::poison::{lock_unpoisoned, read_unpoisoned, write_unpoisoned};

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::thread::ThreadId as StdThreadId;

use crate::event_loop::{EventQueue, PostedEvent};

/// Cross-thread event sender handle (`EventSender`).
#[derive(Clone)]
pub struct EventSender {
    pub target_thread_id: ThreadId,
    pub queue: Arc<Mutex<EventQueue>>,
    /// Wakeup callback function.
    pub wakeup: Arc<dyn Fn() + Send + Sync>,
}

impl EventSender {
    pub fn new(
        target_thread_id: ThreadId,
        queue: Arc<Mutex<EventQueue>>,
        wakeup: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        Self {
            target_thread_id,
            queue,
            wakeup,
        }
    }

    /// Sends a posted event to the target thread queue and triggers wakeup.
    pub fn send(&self, event: PostedEvent) {
        {
            let mut q = lock_unpoisoned(&self.queue);
            q.events.push(event);
        }

        (self.wakeup)();
    }

    pub fn post_event(&self, receiver: crate::object::ObjectId, event: crate::event::Event) {
        self.send(PostedEvent::new(receiver, event, 0));
    }

    /// Unique thread identifier: ThreadId. Modeled after QThread affinity.
    pub fn target_thread_id(&self) -> ThreadId {
        self.target_thread_id
    }
}

/// Unique thread identifier: ThreadId. Modeled after QThread affinity.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ThreadId(pub StdThreadId);

impl ThreadId {
    /// Unique thread identifier: ThreadId. Modeled after QThread affinity.
    pub fn current() -> Self {
        Self(std::thread::current().id())
    }

    /// Unique thread identifier: ThreadId. Modeled after QThread affinity.
    pub fn as_std(&self) -> StdThreadId {
        self.0
    }
}

/// Thread local context: ThreadContext. Modeled after Qt QThreadData.
pub struct ThreadContext {
    pub id: ThreadId,
    /// Whether this thread is the main UI thread.
    pub is_main_thread: bool,
    /// Sends a posted event to the target thread queue and triggers wakeup.
    pub sender: Option<EventSender>,
}

thread_local! {
    static CURRENT_THREAD_CONTEXT: RefCell<Option<ThreadContext>> = const { RefCell::new(None) };
    /// Loop recursion depth of this thread, matching `QThreadData::loopLevel`.
    static LOOP_LEVEL: Cell<usize> = const { Cell::new(0) };
    /// Handler nesting depth of this thread, matching `QThreadData::scopeLevel` (qthread_p.h:400-409).
    static SCOPE_LEVEL: Cell<usize> = const { Cell::new(0) };
}

/// Marks one handler scope on the current thread (`QScopedScopeLevelCounter`, qthread_p.h:400-409).
/// The guard is `!Send` because the counter is thread-local.
pub struct ScopeLevelGuard {
    _not_send: std::marker::PhantomData<*const ()>,
}

impl ScopeLevelGuard {
    /// Enters a handler scope, incrementing the thread's `scopeLevel` until the guard drops.
    pub fn enter() -> Self {
        SCOPE_LEVEL.with(|level| level.set(level.get() + 1));
        Self {
            _not_send: std::marker::PhantomData,
        }
    }
}

impl Drop for ScopeLevelGuard {
    fn drop(&mut self) {
        SCOPE_LEVEL.with(|level| level.set(level.get() - 1));
    }
}
static GLOBAL_MAIN_THREAD_ID: RwLock<Option<ThreadId>> = RwLock::new(None);
static GLOBAL_THREAD_SENDERS: RwLock<Option<HashMap<ThreadId, EventSender>>> = RwLock::new(None);

/// Registers a thread's event sender in the global registry.
pub fn register_thread_sender(thread_id: ThreadId, sender: EventSender) {
    let mut lock = write_unpoisoned(&GLOBAL_THREAD_SENDERS);
    if lock.is_none() {
        *lock = Some(HashMap::new());
    }
    if let Some(map) = lock.as_mut() {
        map.insert(thread_id, sender);
    }
}

/// Unregisters a thread's event sender from the global registry.
pub fn unregister_thread_sender(thread_id: ThreadId) {
    if let Some(map) = write_unpoisoned(&GLOBAL_THREAD_SENDERS).as_mut() {
        map.remove(&thread_id);
    }
}

/// Queries an event sender handle for the given thread.
pub fn query_thread_sender(thread_id: ThreadId) -> Option<EventSender> {
    read_unpoisoned(&GLOBAL_THREAD_SENDERS)
        .as_ref()
        .and_then(|map| map.get(&thread_id).cloned())
}
impl ThreadContext {
    /// Initializes the thread context for the current thread.
    pub fn init_current(is_main_thread: bool, sender: Option<EventSender>) {
        if let Some(ref s) = sender {
            register_thread_sender(ThreadId::current(), s.clone());
        }
        CURRENT_THREAD_CONTEXT.with(|ctx| {
            *ctx.borrow_mut() = Some(ThreadContext {
                id: ThreadId::current(),
                is_main_thread,
                sender,
            });
        });
    }

    /// Unique thread identifier: ThreadId. Modeled after QThread affinity.
    pub fn current_id() -> ThreadId {
        CURRENT_THREAD_CONTEXT.with(|ctx| {
            ctx.borrow()
                .as_ref()
                .map(|c| c.id)
                .unwrap_or_else(ThreadId::current)
        })
    }

    /// Loop recursion depth of the current thread (`QThreadData::loopLevel`).
    pub fn current_loop_level() -> usize {
        LOOP_LEVEL.with(|level| level.get())
    }

    /// Sets the loop recursion depth of the current thread (`QThreadData::loopLevel`).
    pub fn set_current_loop_level(level: usize) {
        LOOP_LEVEL.with(|cell| cell.set(level));
    }

    /// Handler nesting depth of the current thread (`QThreadData::scopeLevel`).
    pub fn current_scope_level() -> usize {
        SCOPE_LEVEL.with(|level| level.get())
    }

    pub fn is_current_main_thread() -> bool {
        CURRENT_THREAD_CONTEXT.with(|ctx| {
            ctx.borrow()
                .as_ref()
                .map(|c| c.is_main_thread)
                .unwrap_or(false)
        })
    }

    /// Records the thread that constructed the application as the main thread, matching
    /// `QCoreApplicationPrivate::mainThread` (qcoreapplication.cpp:475, 531-537). Overwrites on
    /// each call and is not cleared on drop.
    pub(crate) fn set_main_thread_id(id: ThreadId) {
        *crate::poison::write_unpoisoned(&GLOBAL_MAIN_THREAD_ID) = Some(id);
    }

    /// The application's main thread, or `None` before any application has been constructed.
    /// Never inferred from whichever thread asks first.
    pub fn main_thread_id() -> Option<ThreadId> {
        *crate::poison::read_unpoisoned(&GLOBAL_MAIN_THREAD_ID)
    }

    pub fn is_main_thread() -> bool {
        if let Some(is_main) =
            CURRENT_THREAD_CONTEXT.with(|ctx| ctx.borrow().as_ref().map(|c| c.is_main_thread))
        {
            return is_main;
        }
        Self::main_thread_id() == Some(ThreadId::current())
    }

    /// Whether this thread is the main UI thread.
    pub fn assert_main_thread(operation: &str) {
        if !Self::is_main_thread() {
            panic!(
                "ferrokit: `{}` must be called on the main thread (Thread 0 / AppKit constraint)",
                operation
            );
        }
    }

    pub fn current_sender() -> Option<EventSender> {
        CURRENT_THREAD_CONTEXT.with(|ctx| ctx.borrow().as_ref().and_then(|c| c.sender.clone()))
    }

    pub fn clear_current() {
        unregister_thread_sender(ThreadId::current());
        CURRENT_THREAD_CONTEXT.with(|ctx| {
            *ctx.borrow_mut() = None;
        });
    }
}

/// Thread migration error matching QObject::moveToThread rules.
///
/// `NotMovable` was added after `HasParent` and `WrongThread`. The enum is not
/// `#[non_exhaustive]`, so a downstream `match` that names every variant and has no wildcard arm
/// stops compiling until it handles `NotMovable`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveError {
    HasParent,
    WrongThread,
    /// A descendant answers `false` to [`QObject::can_move_to_thread`](crate::object::QObject::can_move_to_thread).
    NotMovable,
}

impl std::fmt::Display for MoveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HasParent => write!(f, "cannot move object with parent to thread"),
            Self::WrongThread => write!(f, "cannot move object from different thread"),
            Self::NotMovable => write!(f, "object or one of its descendants cannot change thread"),
        }
    }
}

impl std::error::Error for MoveError {}

/// Thread migration error matching QObject::moveToThread rules by ObjectId.
///
/// `NotMovable` was added after `HasParent`, `WrongThread`, `UnknownObject` and `Busy`. The enum is
/// not `#[non_exhaustive]`, so a downstream `match` that names every variant and has no wildcard
/// arm stops compiling until it handles `NotMovable`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveByIdError {
    HasParent,
    WrongThread,
    UnknownObject,
    Busy,
    /// The object or a descendant answers `false` to
    /// [`QObject::can_move_to_thread`](crate::object::QObject::can_move_to_thread). Nothing was
    /// sent or changed.
    NotMovable,
}

impl std::fmt::Display for MoveByIdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HasParent => write!(f, "cannot move object with parent to thread"),
            Self::WrongThread => write!(f, "cannot move object from different thread"),
            Self::UnknownObject => write!(f, "object is not registered or no longer alive"),
            Self::Busy => write!(f, "cannot move object while it is currently borrowed"),
            Self::NotMovable => write!(f, "object or one of its descendants cannot change thread"),
        }
    }
}

impl std::error::Error for MoveByIdError {}

impl From<MoveError> for MoveByIdError {
    fn from(err: MoveError) -> Self {
        match err {
            MoveError::HasParent => Self::HasParent,
            MoveError::WrongThread => Self::WrongThread,
            MoveError::NotMovable => Self::NotMovable,
        }
    }
}

fn collect_descendants(initial_children: &[ObjectId]) -> Vec<ObjectId> {
    let mut result = Vec::new();
    let mut queue: Vec<ObjectId> = initial_children.to_vec();

    while let Some(child_id) = queue.pop() {
        if !result.contains(&child_id) {
            result.push(child_id);
            crate::object::with_object(child_id, |obj| {
                queue.extend_from_slice(obj.object_data().children());
            });
        }
    }
    result
}

/// Sets affinity on `data` and on its owned descendants (`owned_children`), recording visited ids.
fn flip_owned(data: &mut ObjectData, target_thread: ThreadId, visited: &mut Vec<ObjectId>) {
    data.set_thread_id(target_thread);
    visited.push(data.id());
    for child in data.owned_children_mut() {
        flip_owned(child.object_data_mut().raw_data_mut(), target_thread, visited);
    }
}

/// Moves a registered object and all its descendants to `target_thread` by [`ObjectId`], matching
/// Qt `QObject::moveToThread` semantics (`qobject.cpp:1706-1780`) without holding an outer
/// `&mut ObjectData` across `ThreadChange` delivery or recursive child updates.
///
/// Cascading actions, in Qt order (`qobject.cpp:1782-1792`):
/// 1. Validates that `id` is registered and alive, has no parent, belongs to
///    `current_caller_thread`, has no active callback borrow in its subtree, and that no object of
///    the subtree answers `false` to
///    [`QObject::can_move_to_thread`](crate::object::QObject::can_move_to_thread) (a
///    [`Timer`](crate::timer::Timer) is such an object); a failure returns the error with nothing
///    sent or changed. The inspection borrows are released before step 2.
/// 2. Dispatches `EventKind::ThreadChange` to the root and all descendants before any change.
/// 3. Detaches their timers from the source thread and updates each node's `ObjectData.thread_id`
///    through a separate short-lived registry borrow (`qobject.cpp:1490-1502`).
/// 4. Flips the physical registration thread and moves queued posted events to the target queue
///    (`qobject.cpp:1794-1815`), then re-arms the detached timers on the target thread.
pub fn move_to_thread_by_id(
    id: ObjectId,
    target_thread: ThreadId,
    current_caller_thread: ThreadId,
) -> Result<(), MoveByIdError> {
    let Some(all_ids) =
        crate::object::qobject::prepare_move_to_thread(id, target_thread, current_caller_thread)?
    else {
        return Ok(());
    };

    // 1. ThreadChange is sent before any change (`qobject.cpp:1749, 1785-1786`).
    for &node_id in &all_ids {
        let mut ev = Event::new(EventKind::ThreadChange);
        crate::object::send_event(node_id, &mut ev);
    }

    // 2. Detach timers from the source thread (`qobject.cpp:1490-1502`).
    let timers = crate::timer::detach_timers_for_objects(&all_ids);

    // 3. Affinity. Each node in the subtree is updated through its own short-lived registry borrow
    //    before the physical registration thread is flipped.
    for &node_id in &all_ids {
        crate::object::with_object_mut(node_id, |obj| {
            obj.object_data_mut().set_thread_id(target_thread);
        });
    }

    // 4. Physical registration flip and queued-event transfer, in one critical section.
    crate::event_loop::move_receivers_to_thread(&all_ids, target_thread, || {
        for &node_id in &all_ids {
            crate::object::qobject::set_registration_thread(node_id, target_thread);
        }
    });
    for &node_id in &all_ids {
        crate::object::register_object_thread(node_id, target_thread);
    }

    // 5. Re-arm the detached timers on the target thread (`qobject.cpp:1496-1500`).
    if !timers.is_empty() {
        crate::event_loop::post_event_to_thread(
            target_thread,
            ObjectId(0),
            Event::new(EventKind::MetaCall(Box::new(move |_| {
                crate::timer::adopt_timers(timers);
            }))),
        );
    }
    Ok(())
}

/// Moves object and all its children to target thread matching Qt moveToThread semantics
/// (`qobject.cpp:1706-1780`).
///
/// Deprecated, because holding `object_data: &mut ObjectData` while delivering `ThreadChange`
/// through the registry (`send_event`) and recursively mutating `owned_children` creates
/// overlapping mutable borrows under Stacked Borrows and Tree Borrows. Use
/// [`move_to_thread_by_id`] instead.
///
/// Refuses with [`MoveError::NotMovable`] when a descendant answers `false` to
/// [`QObject::can_move_to_thread`](crate::object::QObject::can_move_to_thread), before anything is
/// sent or changed. The root is not asked: only its `ObjectData` is available here, and reaching
/// the object through the registry would overlap that `&mut`. [`move_to_thread_by_id`] asks the
/// root too.
#[deprecated(
    note = "holding `&mut ObjectData` across `ThreadChange` delivery and recursive child updates \
            overlaps the registry's callback borrow; use `move_to_thread_by_id`"
)]
pub fn move_to_thread(
    object_data: &mut ObjectData,
    target_thread: ThreadId,
    current_caller_thread: ThreadId,
) -> Result<(), MoveError> {
    if object_data.parent().is_some() {
        return Err(MoveError::HasParent);
    }
    if object_data.thread_id() != current_caller_thread {
        return Err(MoveError::WrongThread);
    }
    if object_data.thread_id() == target_thread {
        return Ok(());
    }

    let mut all_ids = vec![object_data.id()];
    all_ids.extend(collect_descendants(object_data.children()));

    // A descendant that must stay on its thread refuses the whole move, before any event is sent.
    if all_ids[1..]
        .iter()
        .any(|&id| crate::object::with_object(id, |obj| !obj.can_move_to_thread()).unwrap_or(false))
    {
        return Err(MoveError::NotMovable);
    }

    // 1. ThreadChange is sent before any change (`qobject.cpp:1749, 1785-1786`).
    for &id in &all_ids {
        let mut ev = Event::new(EventKind::ThreadChange);
        crate::object::send_event(id, &mut ev);
    }

    // 2. Detach timers from the source thread (`qobject.cpp:1490-1502`).
    let timers = crate::timer::detach_timers_for_objects(&all_ids);

    // 3. Affinity. The root is flipped directly. Descendants are flipped through the registry,
    //    except owned children, which are reached via their parent's `owned_children`; this avoids
    //    aliasing the caller's `&mut`. Runs before the physical flip, because `with_object_mut`
    //    requires the registration to be on the current thread.
    let mut visited = Vec::with_capacity(all_ids.len());
    flip_owned(object_data, target_thread, &mut visited);
    for &id in &all_ids[1..] {
        if !visited.contains(&id) {
            crate::object::with_object_mut(id, |obj| {
                flip_owned(obj.object_data_mut().raw_data_mut(), target_thread, &mut visited);
            });
        }
    }

    // 4. Physical registration flip and queued-event transfer, in one critical section.
    crate::event_loop::move_receivers_to_thread(&all_ids, target_thread, || {
        for &id in &all_ids {
            crate::object::qobject::set_registration_thread(id, target_thread);
        }
    });
    for &id in &all_ids {
        crate::object::register_object_thread(id, target_thread);
    }

    // 5. Re-arm the detached timers on the target thread (`qobject.cpp:1496-1500`).
    if !timers.is_empty() {
        crate::event_loop::post_event_to_thread(
            target_thread,
            ObjectId(0),
            Event::new(EventKind::MetaCall(Box::new(move |_| {
                crate::timer::adopt_timers(timers);
            }))),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Event, EventKind};
    use crate::object::ObjectId;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn test_thread_id_current() {
        let t1 = ThreadId::current();
        let t2 = ThreadId::current();
        assert_eq!(t1, t2);

        let handle = std::thread::spawn(ThreadId::current);
        let other_t = handle.join().unwrap();
        assert_ne!(t1, other_t);
    }

    #[test]
    fn test_thread_context_lifecycle() {
        ThreadContext::clear_current();
        assert_eq!(ThreadContext::current_loop_level(), 0);
        assert!(!ThreadContext::is_current_main_thread());

        ThreadContext::init_current(true, None);
        assert!(ThreadContext::is_current_main_thread());
        assert_eq!(ThreadContext::current_id(), ThreadId::current());
        assert_eq!(ThreadContext::current_loop_level(), 0);

        ThreadContext::set_current_loop_level(2);
        assert_eq!(ThreadContext::current_loop_level(), 2);

        ThreadContext::clear_current();
    }

    #[test]
    fn test_event_sender_send_and_wakeup() {
        let queue = Arc::new(Mutex::new(EventQueue::new()));
        let woken = Arc::new(AtomicBool::new(false));
        let woken_clone = Arc::clone(&woken);

        let sender = EventSender::new(
            ThreadId::current(),
            Arc::clone(&queue),
            Arc::new(move || {
                woken_clone.store(true, Ordering::Release);
            }),
        );

        let event = PostedEvent::new(ObjectId(42), Event::new(EventKind::UpdateRequest), 0);

        sender.send(event);

        assert!(woken.load(Ordering::Acquire));
        assert_eq!(queue.lock().unwrap().len(), 1);
        assert_eq!(queue.lock().unwrap().events[0].receiver, ObjectId(42));
    }

    struct UnitProbe {
        data: ObjectData,
    }

    impl crate::object::QObject for UnitProbe {
        fn object_data(&self) -> &ObjectData {
            &self.data
        }
        fn object_data_mut(&mut self) -> crate::object::ObjectDataMut<'_> {
        crate::object::ObjectDataMut::new(&mut self.data)
    }
    }

    #[test]
    fn test_move_to_thread_success() {
        let thread_a = ThreadId::current();
        let thread_b = std::thread::spawn(ThreadId::current).join().unwrap();

        let obj = unsafe {
            crate::object::register_owned_qobject(UnitProbe {
                data: ObjectData::with_thread(ObjectId::next(), thread_a),
            })
        };
        let id = obj.data.id();
        assert_eq!(obj.data.thread_id(), thread_a);

        let res = move_to_thread_by_id(id, thread_b, thread_a);
        assert!(res.is_ok());
        assert_eq!(obj.data.thread_id(), thread_b);
    }

    #[test]
    fn test_move_to_thread_has_parent_error() {
        let thread_a = ThreadId::current();
        let thread_b = std::thread::spawn(ThreadId::current).join().unwrap();

        let mut data = ObjectData::with_thread(ObjectId::next(), thread_a);
        data.set_parent(Some(ObjectId(1))).unwrap();
        let obj = unsafe { crate::object::register_owned_qobject(UnitProbe { data }) };
        let id = obj.data.id();

        let res = move_to_thread_by_id(id, thread_b, thread_a);
        assert_eq!(res, Err(MoveByIdError::HasParent));
        assert_eq!(obj.data.thread_id(), thread_a);
    }

    #[test]
    fn test_move_to_thread_wrong_thread_error() {
        let thread_a = ThreadId::current();
        let thread_b = std::thread::spawn(ThreadId::current).join().unwrap();

        let obj = unsafe {
            crate::object::register_owned_qobject(UnitProbe {
                data: ObjectData::with_thread(ObjectId::next(), thread_a),
            })
        };
        let id = obj.data.id();

        let res = move_to_thread_by_id(id, thread_b, thread_b);
        assert_eq!(res, Err(MoveByIdError::WrongThread));
        assert_eq!(obj.data.thread_id(), thread_a);
    }
}
