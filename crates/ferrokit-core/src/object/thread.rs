// SPDX-License-Identifier: LGPL-3.0-only
use crate::event::{Event, EventKind};
use crate::object::qobject::ObjectData;
use crate::object::ObjectId;

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
            let mut q = self.queue.lock().unwrap();
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
    let mut lock = GLOBAL_THREAD_SENDERS.write().unwrap();
    if lock.is_none() {
        *lock = Some(HashMap::new());
    }
    if let Some(map) = lock.as_mut() {
        map.insert(thread_id, sender);
    }
}

/// Unregisters a thread's event sender from the global registry.
pub fn unregister_thread_sender(thread_id: ThreadId) {
    if let Ok(mut lock) = GLOBAL_THREAD_SENDERS.write() {
        if let Some(map) = lock.as_mut() {
            map.remove(&thread_id);
        }
    }
}

/// Queries an event sender handle for the given thread.
pub fn query_thread_sender(thread_id: ThreadId) -> Option<EventSender> {
    if let Ok(lock) = GLOBAL_THREAD_SENDERS.read() {
        if let Some(map) = lock.as_ref() {
            return map.get(&thread_id).cloned();
        }
    }
    None
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
        *GLOBAL_MAIN_THREAD_ID.write().unwrap() = Some(id);
    }

    /// The application's main thread, or `None` before any application has been constructed.
    /// Never inferred from whichever thread asks first.
    pub fn main_thread_id() -> Option<ThreadId> {
        *GLOBAL_MAIN_THREAD_ID.read().unwrap()
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveError {
    HasParent,

    WrongThread,
}

fn collect_descendants(initial_children: &[ObjectId]) -> Vec<ObjectId> {
    let mut result = Vec::new();
    let mut queue: Vec<ObjectId> = initial_children.to_vec();

    while let Some(child_id) = queue.pop() {
        if !result.contains(&child_id) {
            result.push(child_id);
            crate::object::with_object(child_id, |obj| {
                queue.extend_from_slice(&obj.object_data().children);
            });
        }
    }
    result
}

/// Sets affinity on `data` and on its owned descendants (`owned_children`), recording visited ids.
fn flip_owned(data: &mut ObjectData, target_thread: ThreadId, visited: &mut Vec<ObjectId>) {
    data.thread_id = target_thread;
    visited.push(data.id);
    for child in &mut data.owned_children {
        flip_owned(child.object_data_mut(), target_thread, visited);
    }
}

/// Moves object and all its children to target thread matching Qt moveToThread semantics
/// (`qobject.cpp:1706-1780`).
///
/// Cascading actions, in Qt order (`qobject.cpp:1782-1792`):
/// 1. Dispatches `EventKind::ThreadChange` to the root and all descendants before any change.
/// 2. Detaches their timers from the source thread; the target re-arms them under the same ids
///    (`qobject.cpp:1490-1502`).
/// 3. Changes thread affinity (`ObjectData.thread_id`) and the physical registration, and moves
///    their queued posted events to the target queue (`qobject.cpp:1794-1815`).
/// 4. Re-arms the detached timers on the target thread.
pub fn move_to_thread(
    object_data: &mut ObjectData,
    target_thread: ThreadId,
    current_caller_thread: ThreadId,
) -> Result<(), MoveError> {
    if object_data.parent().is_some() {
        return Err(MoveError::HasParent);
    }
    if object_data.thread_id != current_caller_thread {
        return Err(MoveError::WrongThread);
    }
    if object_data.thread_id == target_thread {
        return Ok(());
    }

    let mut all_ids = vec![object_data.id];
    all_ids.extend(collect_descendants(&object_data.children));

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
                flip_owned(obj.object_data_mut(), target_thread, &mut visited);
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

    #[test]
    fn test_move_to_thread_success() {
        let thread_a = ThreadId::current();
        let thread_b = std::thread::spawn(ThreadId::current).join().unwrap();

        let mut obj = ObjectData::with_thread(ObjectId(10), thread_a);
        assert_eq!(obj.thread_id, thread_a);

        let res = move_to_thread(&mut obj, thread_b, thread_a);
        assert!(res.is_ok());
        assert_eq!(obj.thread_id, thread_b);
    }

    #[test]
    fn test_move_to_thread_has_parent_error() {
        let thread_a = ThreadId::current();
        let thread_b = std::thread::spawn(ThreadId::current).join().unwrap();

        let mut obj = ObjectData::with_thread(ObjectId(10), thread_a);
        obj.parent = Some(ObjectId(1));

        let res = move_to_thread(&mut obj, thread_b, thread_a);
        assert_eq!(res, Err(MoveError::HasParent));
        assert_eq!(obj.thread_id, thread_a);
    }

    #[test]
    fn test_move_to_thread_wrong_thread_error() {
        let thread_a = ThreadId::current();
        let thread_b = std::thread::spawn(ThreadId::current).join().unwrap();

        let mut obj = ObjectData::with_thread(ObjectId(10), thread_a);

        let res = move_to_thread(&mut obj, thread_b, thread_b);
        assert_eq!(res, Err(MoveError::WrongThread));
        assert_eq!(obj.thread_id, thread_a);
    }
}
