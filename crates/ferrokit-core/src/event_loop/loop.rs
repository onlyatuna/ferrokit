// SPDX-License-Identifier: LGPL-3.0-only
use crate::event::EventFilterChain;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use crate::object::{ScopeLevelGuard, ThreadContext, ThreadId};

use crate::event::{CoreCompressor, Event, EventCompressor, EventKind, MergeKey};
use crate::object::{EventSender, ObjectData, ObjectId, QObject, send_event};
use crate::poison::lock_unpoisoned;
use crate::timer::TimerRegistry;

#[allow(unused_imports)]
use crate::event_loop::dispatcher::EventDispatcher;
thread_local! {
    static APPLICATION_EVENT_FILTERS: RefCell<EventFilterChain> = RefCell::new(EventFilterChain::new());
}

pub fn install_application_event_filter(filter: ObjectId) {
    APPLICATION_EVENT_FILTERS.with(|filters| {
        filters.borrow_mut().install(filter);
    });
}

pub fn remove_application_event_filter(filter: ObjectId) {
    APPLICATION_EVENT_FILTERS.with(|filters| {
        filters.borrow_mut().remove(filter);
    });
}

pub fn clear_application_event_filters() {
    APPLICATION_EVENT_FILTERS.with(|filters| {
        *filters.borrow_mut() = EventFilterChain::new();
    });
}

/// Qt walks the filter list by index and skips entries that have become null
/// (`sendThroughApplicationEventFilters` qcoreapplication.cpp:1237-1247,
/// `sendThroughObjectEventFilters` :1255-1265). `slot_at` is called once per step, so the chain is
/// read live: a filter tombstoned by an earlier callback is skipped, and one installed during dispatch
/// shifts the walk exactly as in Qt. No chain borrow is held while a filter runs.
/// Returns true if a filter consumed the event.
fn consumed_by_filter_chain(
    slot_at: impl Fn(usize) -> Option<Option<ObjectId>>,
    receiver: ObjectId,
    event: &mut Event,
) -> bool {
    let mut index = 0;
    while let Some(slot) = slot_at(index) {
        index += 1;
        let Some(filter_id) = slot else { continue };
        let consumed = crate::object::with_object_mut(filter_id, |filter_obj| {
            filter_obj.event_filter(receiver, event)
        });
        if consumed == Some(true) {
            return true;
        }
    }
    false
}

/// Application event filters (`sendThroughApplicationEventFilters`, qcoreapplication.cpp:1237-1247).
fn app_filters_consumed(receiver: ObjectId, event: &mut Event) -> bool {
    let app_slot =
        |index: usize| APPLICATION_EVENT_FILTERS.with(|filters| filters.borrow().slot_at(index));
    consumed_by_filter_chain(app_slot, receiver, event)
}

/// The receiver's object event filters (`sendThroughObjectEventFilters`, qcoreapplication.cpp:1252-1268).
fn object_filters_consumed(receiver: ObjectId, event: &mut Event) -> bool {
    let object_slot = |index: usize| {
        crate::object::with_object(receiver, |obj| {
            obj.object_data().event_filters.slot_at(index)
        })
        .flatten()
    };
    consumed_by_filter_chain(object_slot, receiver, event)
}

/// `QCoreApplicationPrivate::sendThroughApplicationEventFilters` alone (qcoreapplication.cpp:1237-1247):
/// the application's event filters see `event` for `receiver`, in installation order, until one
/// consumes it. No object filter and no `event()` follows. `QApplication::notify` uses it for a
/// buttonless mouse move over a widget without mouse tracking, which it otherwise throws away
/// (qapplication.cpp:2751-2756). Returns true if a filter consumed the event.
pub fn send_through_application_event_filters(receiver: ObjectId, event: &mut Event) -> bool {
    app_filters_consumed(receiver, event)
}

/// Filter-only delivery for a receiver the caller already holds (`&ObjectData`): application
/// filters, then the receiver's object filters (`sendThroughApplicationEventFilters` and
/// `sendThroughObjectEventFilters`, qcoreapplication.cpp:1237-1268). Works for objects that are
/// not in the QObject registry (widgets), and for callers that cannot take `&mut` on the receiver.
/// Returns true if a filter consumed the event.
pub fn filters_consume(receiver: &ObjectData, event: &mut Event) -> bool {
    let id = receiver.id();
    if app_filters_consumed(id, event) {
        return true;
    }
    let object_slot = |index: usize| receiver.event_filters.slot_at(index);
    consumed_by_filter_chain(object_slot, id, event)
}

/// Delivers `event` to a receiver the caller already holds mutably: filters (see
/// `filters_consume`) then `receiver.event(event)`, the order of `QCoreApplication::notify`
/// (qcoreapplication.cpp:1118 scope counter, :1237-1268 filters). Returns true when a filter
/// consumed the event, otherwise the result of `event()`.
pub fn notify_borrowed<T: QObject + ?Sized>(receiver: &mut T, event: &mut Event) -> bool {
    let _scope = ScopeLevelGuard::enter();
    if filters_consume(receiver.object_data(), event) {
        return true;
    }
    receiver.event(event)
}

/// `notify_borrowed` that runs `after_app_filters` between the application filters and the
/// receiver's object filters, where `QApplicationPrivate::notify_helper` does its own work for
/// widgets (qapplication.cpp:3236-3260: the `WA_UnderMouse` toggle). Not run when an application
/// filter consumes the event.
pub fn notify_borrowed_with<T: QObject + ?Sized>(
    receiver: &mut T,
    event: &mut Event,
    after_app_filters: impl FnOnce(&mut T, &Event),
) -> bool {
    let _scope = ScopeLevelGuard::enter();
    let id = receiver.object_data().id();
    if app_filters_consumed(id, event) {
        return true;
    }
    after_app_filters(receiver, event);
    let object_slot = |index: usize| receiver.object_data().event_filters.slot_at(index);
    if consumed_by_filter_chain(object_slot, id, event) {
        return true;
    }
    receiver.event(event)
}

/// Outcome of delivering a timer tick to its receiver.
pub enum TimerDelivery {
    /// An application or object filter consumed the `Timer` event; `event()` did not run.
    Filtered,
    /// `event()` ran on the receiver.
    Delivered,
    /// The receiver is not registered on this thread, or is currently borrowed.
    NotDeliverable,
}

/// Delivers a `QTimerEvent` for `timer_id` to `receiver` through application filters → object
/// filters → `event()` (qcoreapplication.cpp:1275-1300; qeventdispatcher_win.cpp:414-415).
pub fn deliver_timer_event(receiver: ObjectId, timer_id: u64) -> TimerDelivery {
    let mut event = Event::new(EventKind::Timer { timer_id });
    if app_filters_consumed(receiver, &mut event) || object_filters_consumed(receiver, &mut event) {
        return TimerDelivery::Filtered;
    }
    match crate::object::with_object_mut(receiver, |obj| obj.event(&mut event)) {
        Some(_) => TimerDelivery::Delivered,
        None => TimerDelivery::NotDeliverable,
    }
}

static BORROWED_SEND_REJECTIONS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Synchronous sends rejected because the receiver's borrow was held by a running callback.
#[cfg(test)]
pub(crate) fn borrowed_send_rejections() -> usize {
    BORROWED_SEND_REJECTIONS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Whether a queued call may run for `receiver`. A non-zero receiver that is not registered on this
/// thread was destroyed or moved away; Qt never delivers posted events to a destroyed object
/// (qobject.cpp:206-207). `ObjectId(0)` names no object and is always allowed.
fn queued_call_receiver_alive(receiver: ObjectId) -> bool {
    if receiver == ObjectId(0) || crate::object::qobject::registered_on_current_thread(receiver) {
        return true;
    }
    eprintln!("ferrokit: dropped posted call to unregistered object {receiver:?}");
    false
}

/// Runs a `MetaCallNoTarget`. No borrow of `receiver` is held, so the functor can reach its
/// receiver through `with_object_mut`, as a direct slot can.
fn run_no_target_call(receiver: ObjectId, task: crate::event::MetaCallNoTargetFn) -> bool {
    if !queued_call_receiver_alive(receiver) {
        return false;
    }
    task();
    true
}

/// Runs a `MetaCall` with its receiver as the target. The receiver is borrowed while the functor
/// runs. Only `ObjectId(0)`, which names no object, gets the unit target, so a downcast to a
/// receiver type fails. A registered receiver never gets it: `notify_helper` rejects or defers a
/// receiver that a running callback has borrowed before this call.
fn run_target_call(receiver: ObjectId, task: crate::event::MetaCallFn) -> bool {
    if !queued_call_receiver_alive(receiver) {
        return false;
    }
    let mut task_opt = Some(task);
    let handled = crate::object::with_object_mut(receiver, |obj| {
        if let Some(task) = task_opt.take() {
            task(obj);
        }
    });
    if handled.is_none()
        && let Some(task) = task_opt.take()
    {
        task(&mut ());
    }
    true
}

thread_local! {
    /// Receivers whose `DeferredDelete` an event filter consumed on this thread. Keyed by
    /// receiver so a nested send to another object cannot change an outer delivery's outcome.
    static DEFERRED_DELETE_FILTERED: std::cell::RefCell<Vec<ObjectId>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn note_deferred_delete_filtered(receiver: ObjectId, event: &Event) {
    if matches!(event.kind, EventKind::DeferredDelete { .. }) {
        DEFERRED_DELETE_FILTERED.with(|list| list.borrow_mut().push(receiver));
    }
}

/// Sends a posted `DeferredDelete` and destroys the receiver unless an event filter consumed the
/// event: Qt deletes the object in `QObject::event`, which a consuming filter never reaches
/// (qcoreapplication.cpp:1118-1131 notify; qobject.cpp `QObject::event` DeferredDelete).
fn deliver_deferred_delete(receiver: ObjectId, event: &mut Event) {
    DEFERRED_DELETE_FILTERED.with(|list| list.borrow_mut().retain(|id| *id != receiver));
    let _ = catch_unwind(AssertUnwindSafe(|| send_event(receiver, event)));
    let filtered = DEFERRED_DELETE_FILTERED.with(|list| {
        let mut list = list.borrow_mut();
        let before = list.len();
        list.retain(|id| *id != receiver);
        list.len() != before
    });
    if !filtered {
        let _ = catch_unwind(AssertUnwindSafe(|| finish_deferred_delete(receiver)));
    }
}

pub fn notify_helper(receiver: ObjectId, event: &mut Event) -> bool {
    // QScopedScopeLevelCounter in QCoreApplication::notify (qcoreapplication.cpp:1118).
    let _scope = ScopeLevelGuard::enter();
    // Qt checks the receiver's thread in `sendEvent` (qcoreapplication.cpp:539-548, the check itself
    // debug-only at qcoreapplication.cpp:1223-1225). A foreign thread never runs the owner's
    // callback here: the call is rejected in every build.
    if let Some(owner) = crate::object::qobject::registration_thread_of(receiver) {
        let current = ThreadId::current();
        if owner != current {
            eprintln!(
                "ferrokit: cannot send events to objects owned by a different thread. Current thread {current:?}. Receiver '{receiver:?}' was created in thread {owner:?}"
            );
            return false;
        }
    }
    if app_filters_consumed(receiver, event) {
        note_deferred_delete_filtered(receiver, event);
        return true;
    }

    // Qt posts QZeroTimerEvent to the dispatcher, not to the timer object, so object filters never
    // see it. The dispatcher sends a QTimerEvent through sendEvent, then reposts the zero timer
    // whatever the filter result (qeventdispatcher_win.cpp:879-899). Top-level delivery
    // (loop.rs process_events) holds no object borrow here.
    if let EventKind::ZeroTimer { timer_id } = &event.kind {
        let timer_id = *timer_id;
        // A timer killed since the event was queued is gone: no tick, no repost.
        if crate::timer::zero_timer_armed(receiver, timer_id) {
            let _ = deliver_timer_event(receiver, timer_id);
            crate::timer::repost_zero_timer(receiver, timer_id);
        }
        return true;
    }

    // Qt has no borrow check in `sendEvent` (qcoreapplication.cpp:1563-1572): it calls the receiver
    // directly, re-entrantly. Here the receiver's `&mut` is already held by a running callback, so
    // the event cannot be delivered now. It is rejected visibly and the caller gets `false`. Posted
    // events are deferred instead (`next_deliverable_index`).
    if crate::object::qobject::registered_and_borrowed(receiver) {
        BORROWED_SEND_REJECTIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        eprintln!(
            "ferrokit: sendEvent to borrowed receiver '{receiver:?}' rejected: its callback is still running"
        );
        return false;
    }

    if object_filters_consumed(receiver, event) {
        note_deferred_delete_filtered(receiver, event);
        return true;
    }

    // The application object's Quit runs after the filters, then the window veto and exit
    // (qcoreapplication.cpp:1285-1290, 2052-2058).
    if matches!(&event.kind, EventKind::Quit { .. })
        && crate::application::is_application_object(receiver)
    {
        crate::application::deliver_application_quit(event);
        return true;
    }

    if matches!(&event.kind, EventKind::MetaCallNoTarget(_))
        && let EventKind::MetaCallNoTarget(task) =
            std::mem::replace(&mut event.kind, EventKind::LayoutRequest)
    {
        return run_no_target_call(receiver, task);
    }

    if matches!(&event.kind, EventKind::MetaCall(_))
        && let EventKind::MetaCall(task) =
            std::mem::replace(&mut event.kind, EventKind::LayoutRequest)
    {
        return run_target_call(receiver, task);
    }

    crate::object::dispatch_to_object(receiver, event)
}
use super::dispatcher::{
    DefaultEventDispatcher, DispatchResult, DispatcherKind, EventDispatcherHandle,
    create_dispatcher,
};

/// One thread's loop state, shared by every `EventLoop` on that thread. Qt keeps one event
/// dispatcher, one posted-event queue and one timer registry per thread (`QThreadData`). A nested
/// `QEventLoop` only pushes itself onto the thread's stack of running loops, so an inner loop
/// ending never tears down state the outer loop still uses (qeventloop.cpp:157-192).
struct ThreadLoopState {
    thread_id: ThreadId,
    dispatcher: DefaultEventDispatcher,
    queue: Arc<Mutex<EventQueue>>,
    timer_registry: Arc<Mutex<TimerRegistry>>,
    /// The loops running `exec` on this thread, and whether they are being ended.
    running: Arc<parking_lot::Mutex<RunningLoops>>,
}

/// Exit flags of one running loop. `EventLoop::exit` affects only its own loop
/// (qeventloop.cpp:260-273), while `EventLoopHandle::exit` ends every loop on the thread
/// (qthread.cpp:754-767).
#[derive(Clone)]
struct LoopExit {
    exit_requested: Arc<AtomicBool>,
    return_code: Arc<AtomicI32>,
}

impl LoopExit {
    fn new() -> Self {
        Self {
            exit_requested: Arc::new(AtomicBool::new(false)),
            return_code: Arc::new(AtomicI32::new(0)),
        }
    }

    fn request(&self, return_code: i32) {
        self.return_code.store(return_code, Ordering::SeqCst);
        self.exit_requested.store(true, Ordering::SeqCst);
    }
}

/// The loops running `exec` on one thread (`QThreadData::eventLoops`) and `QThreadData::quitNow`.
#[derive(Default)]
struct RunningLoops {
    /// Exit flags of every running loop, innermost last.
    loops: Vec<LoopExit>,
    /// Set by [`EventLoopHandle::exit`] and by a consumed `WM_QUIT` while loops run: a loop that
    /// starts before they have all returned (a nested loop entered by an event the exiting loop
    /// is still delivering) returns -1 at once instead of running on, as `QEventLoop::exec` does
    /// while `quitNow` is set (qtbase v6.11.2 qeventloop.cpp:142; set by `QThread::exit`,
    /// qthread.cpp:760, and `QCoreApplication::exit`, qcoreapplication.cpp:1537).
    ///
    /// Deviation from Qt: Qt sets `quitNow` even when no loop runs, and clears it only at the
    /// start of `QThread::exec` (qthread.cpp:664) and around `QCoreApplication::exec`
    /// (qcoreapplication.cpp:1465, 1470, 1486), so after an exit every later `QEventLoop::exec`
    /// on the thread returns -1 until one of those runs. Here the flag is set only while a loop
    /// runs and is cleared when the last running loop returns, so an exit with no loop running is
    /// a no-op and a later `exec` on the thread runs normally.
    quit_now: bool,
}

/// Pops a loop's entry from its thread's running-loop stack when `exec` returns.
struct RunningLoop(Arc<parking_lot::Mutex<RunningLoops>>);

impl Drop for RunningLoop {
    fn drop(&mut self) {
        let mut running = self.0.lock();
        running.loops.pop();
        if running.loops.is_empty() {
            running.quit_now = false;
        }
    }
}

thread_local! {
    /// The calling thread's live loop state. Weak, so the state (and its dispatcher) lives exactly
    /// as long as some `EventLoop` on the thread.
    static THREAD_LOOP_STATE: RefCell<Weak<ThreadLoopState>> = const { RefCell::new(Weak::new()) };
}

impl ThreadLoopState {
    /// Returns the calling thread's live state, creating it (with its timer context and loop
    /// handle) when the thread has none. As Qt creates a thread's dispatcher once, the first
    /// creator's `kind` wins.
    fn current_or_create(kind: DispatcherKind) -> Arc<Self> {
        let existing = THREAD_LOOP_STATE.with(|slot| slot.borrow().upgrade());
        if let Some(state) = existing {
            return state;
        }
        let dispatcher = create_dispatcher(kind);
        let timer_registry = Arc::new(Mutex::new(TimerRegistry::new()));
        #[cfg(windows)]
        crate::timer::register_thread_timer_context(
            Arc::clone(&timer_registry),
            dispatcher.internal_hwnd,
        );
        #[cfg(not(windows))]
        crate::timer::register_thread_timer_context(Arc::clone(&timer_registry));

        let thread_id = ThreadId::current();
        // The loop state is deliberately thread-local data shared only through `Arc` handles.
        #[allow(clippy::arc_with_non_send_sync)]
        let state = Arc::new(Self {
            thread_id,
            dispatcher,
            queue: Arc::new(Mutex::new(EventQueue::new())),
            timer_registry,
            running: Arc::default(),
        });
        register_thread_event_loop(thread_id, state.handle());
        THREAD_LOOP_STATE.with(|slot| *slot.borrow_mut() = Arc::downgrade(&state));
        state
    }

    fn handle(&self) -> EventLoopHandle {
        EventLoopHandle {
            dispatcher: Arc::new(self.dispatcher.clone_handle()),
            queue: Arc::clone(&self.queue),
            running: Arc::clone(&self.running),
        }
    }
}

impl Drop for ThreadLoopState {
    fn drop(&mut self) {
        crate::timer::unregister_thread_timer_context();
        unregister_thread_event_loop(self.thread_id);
        // The loop is gone, so nothing turns this queue any more, yet a handle on it can outlive
        // the loop (an `EventSender` clone, such as the one an `EventLoopThreadHandle` keeps), and
        // what is still queued would stay queued as long as that handle lives. Qt deletes the
        // undelivered posted events of a thread together with its `QThreadData`, and the
        // destructor of a posted `QMetaCallEvent` releases a blocked
        // `BlockingQueuedConnection` caller. Here the caller waits on a channel whose sender is
        // inside the call, so the call must be dropped, not kept. The queue is closed, so a post
        // through such a handle after this point is dropped by the poster instead of queued. The
        // events are dropped after the queue lock is released: a payload's `Drop` may post events
        // or unregister objects, which take that lock.
        let undelivered = lock_unpoisoned(&self.queue).close();
        for posted in undelivered {
            // A payload whose `Drop` panics is contained and the other events are still dropped:
            // this runs from a destructor, possibly while the thread unwinds, and a panic that
            // escaped from here then would abort the process.
            let _ = catch_unwind(AssertUnwindSafe(move || drop(posted)));
        }
    }
}

/// A running loop belongs to the thread that made it: its drop and `exec` use that thread's
/// timer context, loop level and thread id, so it is `!Send` on every platform. Other threads
/// reach the loop through an [`EventLoopHandle`].
///
/// ```compile_fail
/// fn needs_send<T: Send>(_: T) {}
/// needs_send(ferrokit_core::event_loop::EventLoop::new());
/// ```
pub struct EventLoop {
    state: Arc<ThreadLoopState>,
    exit: LoopExit,
    custom_timeout: Option<Duration>,
    _not_send: std::marker::PhantomData<*const ()>,
}

impl Drop for EventLoop {
    fn drop(&mut self) {
        // The thread's last loop: nothing delivers a queued `DeferredDelete` after this, and its
        // object would stay registered. Qt delivers them when a thread finishes
        // (`QThreadPrivate::finish` calls `sendPostedEvents(nullptr, DeferredDelete)`). The
        // thread's state is still alive here, so a handler runs with a working loop. What else is
        // queued is dropped with the state (`ThreadLoopState::drop`). The flush is contained as a
        // whole too: this may run while the thread's locals are destroyed or while it unwinds, and
        // a panic that escaped from a destructor then would abort the process.
        if Arc::strong_count(&self.state) == 1 {
            let _ = catch_unwind(AssertUnwindSafe(|| self.flush_deferred_deletes()));
        }
    }
}

impl EventLoop {
    /// A core event loop (worker threads and `CoreApplication`), whichever thread creates it.
    pub fn new() -> Self {
        Self::with_dispatcher_kind(DispatcherKind::Core)
    }

    /// An event loop whose dispatcher comes from the `kind` creation path; `GuiApplication`
    /// creates its loop with `DispatcherKind::Gui`. Loops on one thread share one dispatcher, so
    /// the first loop created on the thread decides its kind.
    pub fn with_dispatcher_kind(kind: DispatcherKind) -> Self {
        Self {
            state: ThreadLoopState::current_or_create(kind),
            exit: LoopExit::new(),
            custom_timeout: None,
            _not_send: std::marker::PhantomData,
        }
    }

    pub(crate) fn dispatcher(&self) -> &DefaultEventDispatcher {
        &self.state.dispatcher
    }

    pub fn queue(&self) -> &Arc<Mutex<EventQueue>> {
        &self.state.queue
    }

    pub fn timer_registry(&self) -> &Arc<Mutex<TimerRegistry>> {
        &self.state.timer_registry
    }

    /// Loop recursion depth of the current thread (`QThreadData::loopLevel`).
    pub fn loop_level(&self) -> usize {
        ThreadContext::current_loop_level()
    }

    pub fn is_exit_requested(&self) -> bool {
        self.exit.exit_requested.load(Ordering::SeqCst)
    }

    pub fn return_code(&self) -> i32 {
        self.exit.return_code.load(Ordering::SeqCst)
    }

    pub fn next_timeout(&self) -> Option<Duration> {
        if let Some(custom) = self.custom_timeout {
            return Some(custom);
        }
        let deadline = lock_unpoisoned(&self.state.timer_registry).next_deadline()?;
        let now = crate::timer::current_time_ms();
        if deadline > now {
            Some(Duration::from_millis(deadline - now))
        } else {
            Some(Duration::ZERO)
        }
    }

    pub fn set_next_timeout(&mut self, timeout: Option<Duration>) {
        self.custom_timeout = timeout;
    }

    pub fn post_event(&self, receiver: ObjectId, event: Event) {
        self.post_event_with_priority(receiver, event, 0);
    }

    pub fn post_event_with_priority(&self, receiver: ObjectId, event: Event, priority: i32) {
        // A merged event is dropped after the queue lock is released (the slot is declared first):
        // a payload's `Drop` may post events or unregister objects.
        let mut event = Some(event);
        // The compressor runs under the queue lock: it merges into queued events in place, and
        // the merge-or-insert decision must be atomic against other posters and the pump. See
        // `EventLoopHandle::post_event_with_priority` for why a panicking compressor is harmless.
        let mut queue = lock_unpoisoned(&self.state.queue);
        let q = &mut *queue;
        q.post_from_slot(receiver, &mut event, priority);
        if event.is_none() {
            self.state.dispatcher.wake_up();
        }
    }

    pub fn set_compressor(&mut self, compressor: Arc<dyn EventCompressor>) {
        // The compressor that is replaced is dropped after the queue lock is released: its `Drop`
        // may post events, which takes that lock.
        let replaced = lock_unpoisoned(&self.state.queue).replace_compressor(compressor);
        drop(replaced);
    }

    pub fn install_native_event_filter(&self, filter: Box<dyn crate::event::NativeEventFilter>) {
        self.state.dispatcher.install_native_event_filter(filter);
    }

    pub fn send_posted_events(&self) -> usize {
        let (delivered, quit_code) = send_posted_events_for_queue(&self.state.queue);
        if let Some(code) = quit_code {
            self.exit.request(code);
        }
        delivered
    }

    /// Delivers the queued `DeferredDelete` events that the loop-exit flush allows, matching
    /// `sendPostedEvents(nullptr, QEvent::DeferredDelete)` (qcoreapplication.cpp:1484-1489). Other
    /// posted events stay queued, and so does a `DeferredDelete` whose receiver a running callback
    /// has borrowed. A handler or destructor that panics is contained and the flush goes on, as in
    /// a loop turn; the flush also runs when the last loop of a thread is dropped, where a panic
    /// that escaped during unwinding would abort the process. Returns the number delivered.
    pub fn flush_deferred_deletes(&self) -> usize {
        {
            let mut q = lock_unpoisoned(&self.state.queue);
            q.insertion_offset = q.events.len();
        }
        let mut delivered = 0;
        loop {
            let posted = {
                let mut q = lock_unpoisoned(&self.state.queue);
                let end = q.insertion_offset;
                let levels = current_levels();
                // As in `next_deliverable_index`, a receiver that a running callback has borrowed
                // is not unregistered under that callback: its event stays queued.
                let found = q.events.iter().take(end).position(|p| match p.event.kind {
                    EventKind::DeferredDelete {
                        loop_level,
                        scope_level,
                    } => {
                        deferred_delete_deliverable((loop_level, scope_level), levels, true)
                            && !crate::object::qobject::registered_and_borrowed(p.receiver)
                    }
                    _ => false,
                });
                match found {
                    Some(index) => q.remove_at(index),
                    None => {
                        q.insertion_offset = 0;
                        break;
                    }
                }
            };
            let Some(PostedEvent {
                receiver,
                mut event,
                ..
            }) = forward_to_owner(posted)
            else {
                continue;
            };
            // As in `pump_posted_events`: the panic hook logs a raising handler, and the receiver
            // is unregistered all the same.
            deliver_deferred_delete(receiver, &mut event);
            delivered += 1;
        }
        delivered
    }

    pub fn process_events(&self, can_wait: bool) -> bool {
        let delivered = self.send_posted_events();
        let had_posted = delivered > 0;

        let next_timeout = self.next_timeout();
        let effective_wait = can_wait && !self.exit.exit_requested.load(Ordering::SeqCst);

        let res = self
            .state
            .dispatcher
            .process_events(effective_wait, next_timeout);
        self.state
            .dispatcher
            .send_timer_events(&self.state.timer_registry);
        if let DispatchResult::Quit(code) = res {
            // A `WM_QUIT` is `QCoreApplication::quit`: it ends every loop running on the thread,
            // not only the innermost one, since the message is consumed here and the outer
            // loops never see it (`QEventDispatcherWin32::processEvents`). As `quit` leads to
            // `QCoreApplication::exit`, it also sets `quit_now`, so a loop entered before the
            // running ones return does not run on.
            {
                let mut running = self.state.running.lock();
                if !running.loops.is_empty() {
                    running.quit_now = true;
                }
                for loop_exit in running.loops.iter() {
                    loop_exit.request(code);
                }
            }
            self.exit.request(code);
        }

        let had_system_events = matches!(
            res,
            DispatchResult::Normal | DispatchResult::Awoken | DispatchResult::Quit(_)
        );
        had_posted || had_system_events
    }

    pub fn exec(&self) -> i32 {
        self.exec_after(|| None)
    }

    /// `exec`, for a thread whose own exit request (`QThread::exit`) may arrive before this loop
    /// runs. `pending_exit` is asked once the loop is registered as running, so an exit issued
    /// at any earlier point is seen here and one issued later reaches the loop through
    /// [`EventLoopHandle::exit`]. When it returns `Some(code)` the loop returns `code` at once,
    /// without delivering events, as `QThread::exec` returns the code of an `exit` that came
    /// before it (qthread.cpp:685-689).
    ///
    /// A loop entered while the thread's running loops are being ended by
    /// [`EventLoopHandle::exit`] returns at once too: `pending_exit`'s code, else -1
    /// (qeventloop.cpp:140-143). Checked under the same lock that `EventLoopHandle::exit` holds
    /// while it tells the running loops, so a loop is either told or sees the flag.
    pub(crate) fn exec_after(&self, pending_exit: impl FnOnce() -> Option<i32>) -> i32 {
        {
            let mut running = self.state.running.lock();
            if running.quit_now {
                drop(running);
                return pending_exit().unwrap_or(-1);
            }
            // qeventloop.cpp:157-158: entering a loop resets the exit flag, so exit() issued
            // before exec() does not end this loop.
            self.exit.exit_requested.store(false, Ordering::SeqCst);
            running.loops.push(self.exit.clone());
        }
        let _running = RunningLoop(Arc::clone(&self.state.running));
        // qeventloop.cpp:192-195: entering the application's loop drops its stale posted Quit.
        if let Some(app_id) = crate::application::app_object_on_current_thread() {
            lock_unpoisoned(&self.state.queue).remove_posted_quits_for(app_id);
        }
        let _level = LoopLevelGuard::enter();
        if let Some(code) = pending_exit() {
            // No events are delivered: the exit came first (qthread.cpp:686-689).
            return code;
        }
        while !self.exit.exit_requested.load(Ordering::SeqCst) {
            self.process_events(true);
        }
        // qeventloop.cpp:161, 187: the loop itself moves `loopLevel`; `scopeLevel` is per handler.
        self.exit.return_code.load(Ordering::SeqCst)
    }

    /// Ends this loop only (`QEventLoop::exit`, qeventloop.cpp:260-273).
    pub fn exit(&self, return_code: i32) {
        self.exit.request(return_code);
        self.state.dispatcher.wake_up();
    }

    pub fn quit(&self) {
        self.exit(0);
    }

    pub fn handle(&self) -> EventLoopHandle {
        self.state.handle()
    }

    pub fn sender(&self) -> EventSender {
        let dispatcher_handle = self.state.dispatcher.clone_handle();
        EventSender::new(
            ThreadId::current(),
            Arc::clone(&self.state.queue),
            Arc::new(move || {
                dispatcher_handle.wake_up();
            }),
        )
    }
}

thread_local! {
    /// True while this thread is inside the posted-event pump (normal or modal).
    static POSTED_PUMP_ACTIVE: Cell<bool> = const { Cell::new(false) };
    /// Set when a modal pump left events queued (kept deferred deletes), so the outermost normal
    /// pump can re-arm the wake-up for them.
    static MODAL_PUMP_LEFT_QUEUED: Cell<bool> = const { Cell::new(false) };
}

/// RAII marker for "a posted-event pump is running on this thread".
struct PostedPumpGuard {
    was_active: bool,
}

impl PostedPumpGuard {
    fn enter() -> Self {
        Self {
            was_active: POSTED_PUMP_ACTIVE.with(|f| f.replace(true)),
        }
    }
}

impl Drop for PostedPumpGuard {
    fn drop(&mut self) {
        POSTED_PUMP_ACTIVE.with(|f| f.set(self.was_active));
    }
}

/// Normal event-loop pump. A callback may re-enter it (nested event loops) and each call runs its
/// own turn; `outermost` only decides which call re-arms the wake-up left by a modal pump.
pub fn send_posted_events_for_queue(queue: &Arc<Mutex<EventQueue>>) -> (usize, Option<i32>) {
    let guard = PostedPumpGuard::enter();
    let outermost = !guard.was_active;
    let result = pump_posted_events(queue, false);
    drop(guard);

    if outermost && MODAL_PUMP_LEFT_QUEUED.with(|f| f.replace(false)) {
        // A native modal loop left events queued and consumed its wake-up. Re-arm it if a normal
        // pump at the current levels can deliver them, so `process_events` returns instead of
        // blocking on them.
        let deliverable = lock_unpoisoned(queue)
            .events
            .iter()
            .any(|e| deliverable_now(&e.event));
        if deliverable && let Some(handle) = get_thread_event_sender(ThreadId::current()) {
            handle.wake_up();
        }
    }
    result
}

/// Pumps the current thread's posted-event queue from a native modal loop
/// (e.g. `WM_FERROKIT_WAKEUP` reaching `internal_wnd_proc` during a Win32 sizing loop).
///
/// Returns `None` if no event loop is registered for this thread; otherwise the number of
/// delivered events. Nested calls are allowed: a callback of an outer pump may open a native
/// modal loop, and this pump then runs its own turn, so its posted events are delivered before
/// the callback returns (Qt re-enters `sendPostedEvents`). `DeferredDelete` events are always
/// left queued, since a modal loop runs inside an arbitrary native callback stack.
pub fn pump_posted_events_modal() -> Option<usize> {
    let handle = get_thread_event_sender(ThreadId::current())?;
    let _guard = PostedPumpGuard::enter();
    let (delivered, quit_code) = pump_posted_events(&handle.queue, true);
    if let Some(code) = quit_code {
        handle.exit(code);
    }
    if !lock_unpoisoned(&handle.queue).is_empty() {
        MODAL_PUMP_LEFT_QUEUED.with(|f| f.set(true));
    }
    Some(delivered)
}

/// Whether an exit (`QCoreApplication::exit` or `quit`, [`EventLoopHandle::exit`], a consumed
/// `WM_QUIT`) is ending the loops running on the calling thread and they have not all returned
/// yet: `QThreadData::quitNow` (qcoreapplication.cpp:1564, qeventloop.cpp:142). False when the
/// thread has no loop or none is running.
///
/// A native modal loop that stands in for a `QEventLoop` (the popup menu's, as `QMenu::exec` runs
/// one) checks it after each message and returns, as Qt's `exit` ends every loop of the thread.
pub fn thread_quit_now() -> bool {
    THREAD_LOOP_STATE
        .try_with(|slot| {
            slot.borrow()
                .upgrade()
                .is_some_and(|state| state.running.lock().quit_now)
        })
        .unwrap_or(false)
}

/// Current (loop level, scope level) of this thread, as `QThreadData::loopLevel` and `scopeLevel`.
fn current_levels() -> (usize, usize) {
    (
        ThreadContext::current_loop_level(),
        ThreadContext::current_scope_level(),
    )
}

/// Event-level gate for a `DeferredDelete` (qcoreapplication.cpp:1870-1885). `stamp` and `current`
/// are (loop level, scope level) pairs, compared by their sums as Qt's `loopLevel + scopeLevel`.
/// Delivered when the stamp outranks the current levels, when it was posted at level zero and a
/// level is active now, or, for the loop-exit flush, when the levels are equal.
fn deferred_delete_deliverable(
    stamp: (usize, usize),
    current: (usize, usize),
    explicit_flush: bool,
) -> bool {
    let stamp_sum = stamp.0 + stamp.1;
    let current_sum = current.0 + current.1;
    stamp_sum > current_sum
        || (stamp_sum == 0 && current_sum > 0)
        || (explicit_flush && stamp_sum == current_sum)
}

/// Whether a normal pump running now delivers `event`. Other events are always delivered.
fn deliverable_now(event: &Event) -> bool {
    match event.kind {
        EventKind::DeferredDelete {
            loop_level,
            scope_level,
        } => deferred_delete_deliverable((loop_level, scope_level), current_levels(), false),
        _ => true,
    }
}

/// Index of the next posted event this pump may deliver, among the first `insertion_offset`.
///
/// An event whose receiver is borrowed by a running callback stays queued in place. Qt has no such
/// state (`QCoreApplication::sendEvent` has no borrow check, qcoreapplication.cpp:1563-1572), and
/// skipping in place keeps per-receiver order and lets `purge` still see the event. A gated
/// `DeferredDelete` leaves the turn, as `remove_at` plus `insert_posted` did before. When nothing is
/// deliverable, the turn ends with `insertion_offset = 0` and the events stay queued.
fn next_deliverable_index(q: &mut EventQueue, modal: bool) -> Option<usize> {
    let mut index = 0;
    while index < q.insertion_offset {
        let posted = &q.events[index];
        if crate::object::qobject::registered_and_borrowed(posted.receiver) {
            index += 1;
            continue;
        }
        if let EventKind::DeferredDelete {
            loop_level,
            scope_level,
        } = posted.event.kind
            && (modal
                || !deferred_delete_deliverable((loop_level, scope_level), current_levels(), false))
        {
            let posted = q.remove_at(index);
            q.insert_posted(posted);
            continue;
        }
        return Some(index);
    }
    q.insertion_offset = 0;
    None
}

static DEFERRED_DELETE_HOOK: std::sync::OnceLock<fn(ObjectId)> = std::sync::OnceLock::new();

/// Installs the callback run after each delivered `DeferredDelete`. Widgets are not registered in
/// the object registry, so they drop their parent link here instead of in `event()`.
pub fn set_deferred_delete_hook(hook: fn(ObjectId)) {
    let _ = DEFERRED_DELETE_HOOK.set(hook);
}

/// Finishes one delivered `DeferredDelete`: destroys the receiver with its subtree and runs the hook.
fn finish_deferred_delete(receiver: ObjectId) {
    crate::object::qobject::destroy_deferred(receiver);
    if let Some(hook) = DEFERRED_DELETE_HOOK.get() {
        hook(receiver);
    }
}

/// Gives a posted event to its receiver's current thread when the receiver moved after the event
/// was queued here. `move_receivers_to_thread` moves queued events itself, so this only catches
/// events added through a direct `EventLoopHandle::post_event`. Returns the event when it is
/// delivered on this thread.
fn forward_to_owner(posted: PostedEvent) -> Option<PostedEvent> {
    if posted.receiver == ObjectId(0) {
        return Some(posted);
    }
    match crate::object::qobject::registration_thread_of(posted.receiver) {
        Some(owner) if owner != ThreadId::current() => {
            post_event_to_thread_with_priority(
                owner,
                posted.receiver,
                posted.event,
                posted.priority,
            );
            None
        }
        _ => Some(posted),
    }
}

/// Restores the thread's loop level on every exit path, including unwinding.
struct LoopLevelGuard;

impl LoopLevelGuard {
    fn enter() -> Self {
        ThreadContext::set_current_loop_level(ThreadContext::current_loop_level() + 1);
        Self
    }
}

impl Drop for LoopLevelGuard {
    fn drop(&mut self) {
        ThreadContext::set_current_loop_level(ThreadContext::current_loop_level() - 1);
    }
}

fn pump_posted_events(queue: &Arc<Mutex<EventQueue>>, modal: bool) -> (usize, Option<i32>) {
    {
        let mut q = lock_unpoisoned(queue);
        q.insertion_offset = q.events.len();
    }

    let mut delivered_count = 0;
    let mut quit_code = None;

    // `insertion_offset` counts this turn's undelivered events at the front of the queue. It
    // shrinks as they leave, so a nested pump that drains them also ends the outer turn (Qt
    // `startOffset`, qcoreapplication.cpp:1815-1858).
    loop {
        let posted = {
            let mut q = lock_unpoisoned(queue);
            let Some(index) = next_deliverable_index(&mut q, modal) else {
                break;
            };
            q.remove_at(index)
        };
        let Some(PostedEvent {
            receiver,
            mut event,
            ..
        }) = forward_to_owner(posted)
        else {
            continue;
        };

        if let EventKind::Quit { exit_code } = event.kind {
            // The application object's Quit is an event: it records nothing before delivery and
            // exits only when accepted (qcoreapplication.cpp:2052-2058).
            if !crate::application::is_application_object(receiver) {
                quit_code = Some(exit_code);
                // A posted Quit to another object ends the loop like `QEventLoop::quit` or
                // `QThread::exit`, which never emit `aboutToQuit`: only `QCoreApplication::exit`
                // does (qcoreapplication.cpp:1532-1535), and the application's own Quit reaches
                // it through `deliver_application_quit`. A nested loop's quit must not use up
                // the one emission of the real application quit.
            }
        }

        // Python/PySide6 reports a raising handler and keeps delivering later events; the panic
        // hook logs it. The in-flight event is dropped, as Python drops it.
        if let EventKind::DeferredDelete { .. } = event.kind {
            deliver_deferred_delete(receiver, &mut event);
        } else {
            let _ = catch_unwind(AssertUnwindSafe(|| send_event(receiver, &mut event)));
        }

        delivered_count += 1;
    }

    (delivered_count, quit_code)
}

impl Default for EventLoop {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod merge_index_tests;
mod queue;
mod registry;
#[cfg(test)]
mod tests;

pub use queue::*;
pub use registry::*;
#[allow(unused_imports)]
use registry::{THREAD_EVENT_HANDLES, ThreadEventRegistry, routed_thread};
