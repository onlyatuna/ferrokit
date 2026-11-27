// SPDX-License-Identifier: LGPL-3.0-only
use super::*;

#[derive(Clone)]
pub struct EventLoopHandle {
    pub dispatcher: Arc<dyn EventDispatcherHandle>,
    pub queue: Arc<Mutex<EventQueue>>,
    pub(super) running: Arc<parking_lot::Mutex<RunningLoops>>,
}

impl EventLoopHandle {
    pub fn post_event(&self, receiver: ObjectId, event: Event) {
        self.post_event_with_priority(receiver, event, 0);
    }
    pub fn post_event_with_priority(&self, receiver: ObjectId, event: Event, priority: i32) {
        // Declared before the lock, so that a merged event, or one that a panicking compressor was
        // looking at, is dropped after the queue lock is released: a payload's `Drop` may post
        // events or unregister objects, which take that lock.
        let mut event = Some(event);
        self.post_from_slot(receiver, &mut event, priority);
    }

    /// See [`EventQueue::post_from_slot`]: the caller drops what is left in the slot, after it has
    /// released every lock it holds.
    pub(crate) fn post_from_slot(
        &self,
        receiver: ObjectId,
        event: &mut Option<Event>,
        priority: i32,
    ) {
        // `EventCompressor::try_compress` is user code and runs under the queue lock, because it
        // merges into queued events in place and the merge-or-insert decision must be atomic
        // against other posters and the pump (Qt compresses under the `postEventList` mutex).
        // A panicking compressor cannot leave the queue structurally inconsistent: it runs before
        // the queue and its index are changed (it is given one queued event at a time, never the
        // collection) and the incoming event stays in the slot; an event it had already edited in
        // place stays as it was left. The lock is therefore taken with `lock_unpoisoned`, so a
        // later destruction or loop teardown still completes.
        let mut queue = lock_unpoisoned(&self.queue);
        queue.post_from_slot(receiver, event, priority);
        if event.is_none() {
            self.dispatcher.wake_up();
        }
    }

    pub fn post_quit(&self, receiver: ObjectId, exit_code: i32) {
        self.post_event(receiver, Event::new(EventKind::Quit { exit_code }));
    }

    pub fn wake_up(&self) {
        self.dispatcher.wake_up();
    }

    /// Ends every loop running on the thread with `return_code`, and a loop the thread enters
    /// before they have all returned (it returns -1 at once). With no loop running it does
    /// nothing.
    pub fn exit(&self, return_code: i32) {
        // (v6.11.2): QThread::exit sets `quit_now` and ends every loop running on the
        // thread, under the lock an event loop run checks `quit_now` with.
        {
            let mut running = self.running.lock();
            if !running.loops.is_empty() {
                running.quit_now = true;
            }
            for loop_exit in running.loops.iter() {
                loop_exit.request(return_code);
            }
        }
        self.dispatcher.wake_up();
    }

    pub fn quit(&self) {
        self.exit(0);
    }
}

/// Per-thread posted-event routing: the live loop handle (if any) and events posted before one
/// exists. Qt keeps `postEventList` in the per-thread event state, independent of the event dispatcher
/// (posted event queueing), so an event posted to a thread
/// that has no loop yet is queued and delivered once the thread runs events.
pub(super) struct ThreadEventRegistry {
    handles: HashMap<ThreadId, EventLoopHandle>,
    /// Same ordering and compression rules as a live loop's queue (`EventQueue`), so a thread
    /// that never starts a loop does not accumulate duplicate `UpdateRequest`s.
    pending: HashMap<ThreadId, EventQueue>,
    /// Threads whose locals have been destroyed (see [`mark_thread_ended`]). A thread id is never
    /// reused, so an entry stays valid; it costs a few bytes for each thread that used the library.
    ended: std::collections::HashSet<ThreadId>,
}

pub(super) static THREAD_EVENT_HANDLES: Mutex<Option<ThreadEventRegistry>> = Mutex::new(None);

impl ThreadEventRegistry {
    fn new() -> Self {
        Self {
            handles: HashMap::new(),
            pending: HashMap::new(),
            ended: std::collections::HashSet::new(),
        }
    }
}

/// Records that `thread_id` has ended and drops what was held for it. Events posted to it from
/// now on are dropped (see [`post_event_to_thread`]). Both happen under the registry lock, so
/// an event is either held and dropped here, or posted after and dropped by the poster: none is
/// left in a queue that nobody will turn. The dropped events are released after the lock is
/// released, since a payload's `Drop` may post events or unregister objects.
pub(crate) fn mark_thread_ended(thread_id: ThreadId) {
    let held = {
        let mut reg = lock_unpoisoned(&THREAD_EVENT_HANDLES);
        let reg = reg.get_or_insert_with(ThreadEventRegistry::new);
        reg.ended.insert(thread_id);
        reg.pending.remove(&thread_id)
    };
    drop(held);
}

/// Whether `thread_id` has been reported by [`mark_thread_ended`] and no loop has registered on it
/// since. A thread id is never reused, so a `true` answer stays true unless that same thread
/// starts a loop while its locals are being destroyed.
pub(crate) fn is_thread_ended(thread_id: ThreadId) -> bool {
    let reg = lock_unpoisoned(&THREAD_EVENT_HANDLES);
    reg.as_ref()
        .is_some_and(|reg| reg.ended.contains(&thread_id))
}

/// Registers the live loop for `thread_id` and delivers, in posting order, every event that was
/// posted to that thread before the loop existed. The drain happens under the registry
/// lock so a concurrent poster cannot overtake the buffered events.
pub fn register_thread_event_loop(thread_id: ThreadId, handle: EventLoopHandle) {
    // What is left over (a replayed event that was merged, the handle this one replaces) is kept
    // in values declared before the lock, so that it is dropped after the lock is released: a
    // `Drop` that posts an event or unregisters an object takes that lock.
    let mut merged = Vec::new();
    let mut replaced = None;
    {
        let mut reg = lock_unpoisoned(&THREAD_EVENT_HANDLES);
        let reg = reg.get_or_insert_with(ThreadEventRegistry::new);
        // A loop that starts on a thread already reported as ended (during the destruction of
        // its locals) can turn its queue again: posts must reach it.
        reg.ended.remove(&thread_id);
        if let Some(buffered) = reg.pending.remove(&thread_id) {
            for posted in buffered.events {
                let mut event = Some(posted.event);
                handle.post_from_slot(posted.receiver, &mut event, posted.priority);
                merged.push(event);
            }
        }
        replaced = reg.handles.insert(thread_id, handle).or(replaced);
    }
    drop(merged);
    drop(replaced);
}

pub fn unregister_thread_event_loop(thread_id: ThreadId) {
    let removed = {
        let mut reg = lock_unpoisoned(&THREAD_EVENT_HANDLES);
        reg.as_mut().and_then(|reg| reg.handles.remove(&thread_id))
    };
    drop(removed);
}

/// Posts `event` to the thread's event queue. If the thread has no event loop yet the event is
/// held until one registers (see [`register_thread_event_loop`]), or until the thread ends, when
/// it is dropped with the rest of what was held for it. An event posted to a thread that has
/// already ended is dropped at once, after the registry lock is released: no loop can turn that
/// queue any more. A thread counts as ended once its locals are destroyed, and only if it asked
/// for its [`ThreadId::current`](crate::object::ThreadId::current) while it lived.
pub fn post_event_to_thread(thread_id: ThreadId, receiver: ObjectId, event: Event) {
    post_event_to_thread_with_priority(thread_id, receiver, event, 0);
}

pub fn post_event_to_thread_with_priority(
    thread_id: ThreadId,
    receiver: ObjectId,
    event: Event,
    priority: i32,
) {
    // One lock covers "who owns the receiver now?", "is there a loop?" and "buffer it". A
    // `move_to_thread` flips registration and moves queued events under the same lock, so a post
    // cannot land on the old owner's queue after the move. Qt posts to the receiver's thread data
    // A user compressor can panic while this lock is held (see
    // `EventLoopHandle::post_event_with_priority`). That happens before the registry or the
    // queue is changed beyond the empty entries created by `get_or_insert_with` and
    // `or_default`, so the registry stays valid and every lock of it recovers from poison.
    //
    // The event is held in a slot declared before the lock: an event that is merged into a queued
    // one, or that a panicking compressor was looking at, is dropped after the registry lock is
    // released. A payload's `Drop` may post events or unregister objects, which take that lock.
    let mut event = Some(event);
    let mut reg = lock_unpoisoned(&THREAD_EVENT_HANDLES);
    let reg = reg.get_or_insert_with(ThreadEventRegistry::new);
    let thread_id = routed_thread(receiver, thread_id);
    if reg.ended.contains(&thread_id) {
        return;
    }
    match reg.handles.get(&thread_id) {
        Some(handle) => handle.post_from_slot(receiver, &mut event, priority),
        None => reg
            .pending
            .entry(thread_id)
            .or_default()
            .post_from_slot(receiver, &mut event, priority),
    }
}

/// The thread a post to `receiver` must reach: the receiver's registration owner when it is
/// registered, else `default`. `ObjectId(0)` is the fire-and-forget sentinel and is never routed.
pub(super) fn routed_thread(receiver: ObjectId, default: ThreadId) -> ThreadId {
    if receiver == ObjectId(0) {
        return default;
    }
    crate::object::qobject::registration_thread_of(receiver).unwrap_or(default)
}

pub fn get_thread_event_sender(thread_id: ThreadId) -> Option<EventLoopHandle> {
    let reg = lock_unpoisoned(&THREAD_EVENT_HANDLES);
    reg.as_ref()
        .and_then(|r| r.handles.get(&thread_id).cloned())
}

/// Moves the posted events addressed to `receivers` onto the queue of `to`, and runs `flip` (which
/// sets their physical registration) in the same critical section. A concurrent
/// `post_event_to_thread` either queued before this point, so its event moves here, or runs after
/// and is routed to `to`. The moved events keep their order and priority and are not compressed
/// (Qt `setThreadData_helper`, under the locks of).
pub(crate) fn move_receivers_to_thread(receivers: &[ObjectId], to: ThreadId, flip: impl FnOnce()) {
    let receiver_set: std::collections::HashSet<ObjectId> = receivers.iter().copied().collect();
    let mut woken = None;
    // Events moved to a thread that has ended; dropped after the registry lock is released, as a
    // payload's `Drop` may post events or unregister objects.
    let mut discarded = Vec::new();
    {
        let mut guard = lock_unpoisoned(&THREAD_EVENT_HANDLES);
        let reg = guard.get_or_insert_with(ThreadEventRegistry::new);
        flip();
        let mut moved = Vec::new();
        let mut seen: Vec<&Arc<Mutex<EventQueue>>> = Vec::new();
        for (thread, handle) in reg.handles.iter() {
            if *thread == to || seen.iter().any(|queue| Arc::ptr_eq(queue, &handle.queue)) {
                continue;
            }
            seen.push(&handle.queue);
            moved.extend(
                lock_unpoisoned(&handle.queue)
                    .remove_posted_events_matching(|id| receiver_set.contains(&id)),
            );
        }
        for (thread, queue) in reg.pending.iter_mut() {
            if *thread != to {
                moved.extend(queue.remove_posted_events_matching(|id| receiver_set.contains(&id)));
            }
        }
        // A queued `ZeroTimer` of a moved receiver is not carried: `timer::adopt_timers` re-arms
        // the zero timer on the target. The compressor would merge the two anyway; dropping it
        // keeps the move independent of that merge.
        moved.retain(|posted| !matches!(posted.event.kind, EventKind::ZeroTimer { .. }));
        if !moved.is_empty() {
            match reg.handles.get(&to) {
                Some(handle) => {
                    let mut queue = lock_unpoisoned(&handle.queue);
                    for posted in moved {
                        queue.insert_posted(posted);
                    }
                    woken = Some(handle.clone());
                }
                None if reg.ended.contains(&to) => {
                    // No loop can turn that queue: the events go with the thread, and their
                    // payloads are dropped after the lock is released.
                    discarded.extend(moved);
                }
                None => {
                    let queue = reg.pending.entry(to).or_default();
                    for posted in moved {
                        queue.insert_posted(posted);
                    }
                }
            }
        }
    }
    drop(discarded);
    if let Some(handle) = woken {
        handle.wake_up();
    }
}

/// Removes every posted event addressed to `receiver` from all threads: live loops and the pending
/// queues of threads without a loop yet (Qt `~QObject` → `removePostedEvents`).
/// `ObjectId(0)` is the fire-and-forget sentinel, not an object, so it is never purged. Lock order is
/// the registry, then each queue. Removed events are dropped only after every lock is released,
/// because a payload's `Drop` may unregister objects and purge again.
pub(crate) fn purge_posted_events_for_receiver(receiver: ObjectId) {
    if receiver == ObjectId(0) {
        return;
    }
    let mut removed = Vec::new();
    {
        let mut guard = lock_unpoisoned(&THREAD_EVENT_HANDLES);
        if let Some(reg) = guard.as_mut() {
            let mut seen: Vec<&Arc<Mutex<EventQueue>>> = Vec::new();
            for handle in reg.handles.values() {
                if seen.iter().any(|queue| Arc::ptr_eq(queue, &handle.queue)) {
                    continue;
                }
                seen.push(&handle.queue);
                removed.extend(lock_unpoisoned(&handle.queue).remove_posted_events(receiver));
            }
            for queue in reg.pending.values_mut() {
                removed.extend(queue.remove_posted_events(receiver));
            }
        }
    }
    drop(removed);
}
