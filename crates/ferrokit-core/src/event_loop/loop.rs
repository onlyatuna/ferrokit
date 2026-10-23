// SPDX-License-Identifier: LGPL-3.0-only
use crate::event::EventFilterChain;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use crate::object::{ScopeLevelGuard, ThreadContext, ThreadId};

use crate::event::{CoreCompressor, Event, EventCompressor, EventKind, MergeKey};
use crate::object::{send_event, EventSender, ObjectData, ObjectId, QObject};
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
    if handled.is_none() {
        if let Some(task) = task_opt.take() {
            task(&mut ());
        }
    }
    true
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

    if matches!(&event.kind, EventKind::MetaCallNoTarget(_)) {
        if let EventKind::MetaCallNoTarget(task) =
            std::mem::replace(&mut event.kind, EventKind::LayoutRequest)
        {
            return run_no_target_call(receiver, task);
        }
    }

    if matches!(&event.kind, EventKind::MetaCall(_)) {
        if let EventKind::MetaCall(task) =
            std::mem::replace(&mut event.kind, EventKind::LayoutRequest)
        {
            return run_target_call(receiver, task);
        }
    }

    crate::object::dispatch_to_object(receiver, event)
}
use super::dispatcher::{
    create_dispatcher, DefaultEventDispatcher, DispatchResult, DispatcherKind,
    EventDispatcherHandle,
};

#[derive(Debug)]
pub struct PostedEvent {
    pub receiver: ObjectId,
    pub event: Event,
    pub priority: i32,
    /// Stable queue-order label used by the merge index.
    order: u128,
    /// The merge key of `event` under the compressor of the queue that holds it, set when the
    /// queue takes the event in. It means nothing outside a queue.
    merge_key: Option<MergeKey>,
}

impl PostedEvent {
    pub fn new(receiver: ObjectId, event: Event, priority: i32) -> Self {
        Self {
            receiver,
            event,
            priority,
            order: 0,
            merge_key: None,
        }
    }
}

pub struct EventQueue {
    /// Only `insert_at`, `remove_at` and `remove_posted_events_matching` change the events, so
    /// that `merge_index` and `sorted_from` follow every change.
    events: VecDeque<PostedEvent>,
    /// Number of queued events of each receiver, so a purge for an object that has none costs
    /// no scan of the queue (Qt keeps a posted-event count per object).
    receiver_counts: HashMap<ObjectId, usize>,
    pub(crate) insertion_offset: usize,
    compressor: Arc<dyn EventCompressor>,
    /// Stable queue-order labels of keyed events, ascending, by receiver and key. Compressing an
    /// event uses these labels to find its candidates without changing the index on queue shifts.
    merge_index: HashMap<(ObjectId, MergeKey), Vec<u128>>,
    /// `events[sorted_from..]` is in non-increasing priority order, so `insert_posted` finds its
    /// place there by binary search and walks only the events in front of it. Those are events
    /// that a turn left queued (those of a borrowed receiver), with events of a higher priority
    /// posted behind them; delivering or removing them moves `sorted_from` back toward the front.
    sorted_from: usize,
    /// Set once the loop that turned this queue is gone (`close`). A closed queue takes no event:
    /// handles on it that outlive the loop (`EventSender` and `EventLoopHandle` clones) would
    /// otherwise queue events that nothing can run or drop.
    closed: bool,
}

impl Default for EventQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl EventQueue {
    pub fn new() -> Self {
        Self::with_compressor(Arc::new(CoreCompressor))
    }

    pub fn with_compressor(compressor: Arc<dyn EventCompressor>) -> Self {
        Self {
            events: VecDeque::new(),
            receiver_counts: HashMap::new(),
            insertion_offset: 0,
            compressor,
            merge_index: HashMap::new(),
            sorted_from: 0,
            closed: false,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Closes the queue and returns every event it still holds. Nothing is posted to it from now
    /// on (`post_from_slot` and `try_push_back` leave the event with the caller), so what is
    /// returned is the last of it. The caller drops the events after releasing the queue lock.
    pub(crate) fn close(&mut self) -> Vec<PostedEvent> {
        self.closed = true;
        self.remove_posted_events_matching(|_| true)
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn events(&self) -> &VecDeque<PostedEvent> {
        &self.events
    }

    /// Installs `compressor` and returns the one it replaces. The queued events get their merge
    /// keys from the new compressor. The keys are all computed before anything changes, so a
    /// compressor that panics leaves the queue as it was.
    pub(crate) fn replace_compressor(
        &mut self,
        compressor: Arc<dyn EventCompressor>,
    ) -> Arc<dyn EventCompressor> {
        let keys: Vec<Option<MergeKey>> = self
            .events
            .iter()
            .map(|posted| compressor.merge_key(&posted.event))
            .collect();
        for (posted, key) in self.events.iter_mut().zip(keys) {
            posted.merge_key = key;
        }
        self.rebuild_merge_index();
        std::mem::replace(&mut self.compressor, compressor)
    }

    /// Merges `event` into a queued event of `receiver`, if the compressor finds one.
    pub fn compress_event(&mut self, receiver: ObjectId, event: &mut Event) -> bool {
        let key = self.compressor.merge_key(event);
        self.merge_into_queued(receiver, key, event)
    }

    /// Offers `incoming` to the queued events of `receiver` that have its merge key, newest first,
    /// until one takes it. An event without a key, and a key no queued event has, end it at once.
    fn merge_into_queued(
        &mut self,
        receiver: ObjectId,
        key: Option<MergeKey>,
        incoming: &Event,
    ) -> bool {
        let Some(key) = key else {
            return false;
        };
        let Some(bucket_len) = self.merge_index.get(&(receiver, key)).map(Vec::len) else {
            return false;
        };
        for bucket_index in (0..bucket_len).rev() {
            let order = self.merge_index[&(receiver, key)][bucket_index];
            let pos = self.events.partition_point(|posted| posted.order < order);
            let outcome = catch_unwind(AssertUnwindSafe(|| {
                self.compressor
                    .try_compress(&mut self.events[pos].event, incoming, receiver)
            }));
            match outcome {
                Ok(true) => return true,
                Ok(false) => {}
                Err(payload) => {
                    let old_key = self.events[pos].merge_key;
                    let new_key = catch_unwind(AssertUnwindSafe(|| {
                        self.compressor.merge_key(&self.events[pos].event)
                    }))
                    .unwrap_or(None);
                    if let Some(old_key) = old_key {
                        self.unindex(receiver, old_key, order);
                    }
                    self.events[pos].merge_key = new_key;
                    if let Some(new_key) = new_key {
                        let positions = self.merge_index.entry((receiver, new_key)).or_default();
                        let at = positions.partition_point(|&candidate| candidate < order);
                        positions.insert(at, order);
                    }
                    std::panic::resume_unwind(payload);
                }
            }
        }
        false
    }

    pub fn post_event(&mut self, receiver: ObjectId, event: Event) {
        self.post_event_with_priority(receiver, event, 0);
    }

    pub fn post_event_with_priority(
        &mut self,
        receiver: ObjectId,
        event: Event,
        priority: i32,
    ) {
        // A merged event is dropped here, under whatever lock the caller holds. Callers that hold
        // a lock use `post_from_slot`, which leaves it to them.
        let mut event = Some(event);
        self.post_from_slot(receiver, &mut event, priority);
    }

    /// Posts the event in `event` and leaves the slot empty, or, when the event is merged into one
    /// that is queued, or the queue is closed (see `close`), leaves it in the slot for the caller
    /// to drop. A payload's `Drop` may unregister objects or post events, which takes locks that
    /// the caller of this function may hold, so the caller declares the slot before taking them
    /// and drops it after releasing them.
    pub(crate) fn post_from_slot(
        &mut self,
        receiver: ObjectId,
        event: &mut Option<Event>,
        priority: i32,
    ) {
        if self.closed {
            return;
        }
        let Some(incoming) = event.as_mut() else {
            return;
        };
        let key = self.compressor.merge_key(incoming);
        if self.merge_into_queued(receiver, key, incoming) {
            return;
        }
        if let Some(event) = event.take() {
            self.insert_keyed(PostedEvent::new(receiver, event, priority), key);
        }
    }

    /// Removes the event at `index`. Removing one from the current turn shrinks
    /// `insertion_offset` with it (Qt `insertionOffset -= startOffset`, qcoreapplication.cpp:1847).
    pub(crate) fn remove_at(&mut self, index: usize) -> PostedEvent {
        let posted = self
            .events
            .remove(index)
            .expect("remove_at: index is within the queue");
        if index < self.insertion_offset {
            self.insertion_offset -= 1;
        }
        if index < self.sorted_from {
            self.sorted_from -= 1;
        }
        if let Some(key) = posted.merge_key {
            self.unindex(posted.receiver, key, posted.order);
        }
        self.count_removed(posted.receiver);
        posted
    }

    fn count_removed(&mut self, receiver: ObjectId) {
        if let Some(count) = self.receiver_counts.get_mut(&receiver) {
            *count -= 1;
            if *count == 0 {
                self.receiver_counts.remove(&receiver);
            }
        }
    }

    /// Removes every posted event addressed to `receiver` (Qt
    /// `QCoreApplication::removePostedEvents`, qcoreapplication.cpp:1955-2001). The removed events
    /// are returned, not dropped here: a payload's `Drop` may unregister objects and re-enter this
    /// queue, so the caller drops them after releasing the lock (qcoreapplication.cpp:2000-2001).
    /// `insertion_offset` shrinks by the removed entries inside the current turn, as in `remove_at`.
    pub(crate) fn remove_posted_events(&mut self, receiver: ObjectId) -> Vec<PostedEvent> {
        if !self.receiver_counts.contains_key(&receiver) {
            return Vec::new();
        }
        self.remove_posted_events_matching(|id| id == receiver)
    }

    /// Removes every posted event whose receiver satisfies `affected`. The removed events are
    /// returned for the same reason as `remove_posted_events`.
    pub(crate) fn remove_posted_events_matching(
        &mut self,
        affected: impl Fn(ObjectId) -> bool,
    ) -> Vec<PostedEvent> {
        if !self.receiver_counts.keys().any(|&id| affected(id)) {
            return Vec::new();
        }
        let turn_end = self.insertion_offset;
        let sorted_from = self.sorted_from;
        let mut removed = Vec::new();
        let mut removed_in_turn = 0;
        let mut removed_before_sorted = 0;
        let mut kept = VecDeque::with_capacity(self.events.len());
        for (index, posted) in std::mem::take(&mut self.events).into_iter().enumerate() {
            if affected(posted.receiver) {
                if index < turn_end {
                    removed_in_turn += 1;
                }
                if index < sorted_from {
                    removed_before_sorted += 1;
                }
                self.count_removed(posted.receiver);
                removed.push(posted);
            } else {
                kept.push_back(posted);
            }
        }
        self.events = kept;
        self.insertion_offset -= removed_in_turn;
        self.sorted_from -= removed_before_sorted;
        self.rebuild_merge_index();
        removed
    }

    /// Removes posted `Quit` events addressed to `receiver` (qeventloop.cpp:192-195).
    pub(crate) fn remove_posted_quits_for(&mut self, receiver: ObjectId) {
        let mut index = self.events.len();
        while index > 0 {
            index -= 1;
            let posted = &self.events[index];
            if posted.receiver == receiver && matches!(posted.event.kind, EventKind::Quit { .. }) {
                self.remove_at(index);
            }
        }
    }

    /// Appends `posted` without compressing it and without looking at its priority.
    pub(crate) fn push_back(&mut self, posted: PostedEvent) {
        let key = self.compressor.merge_key(&posted.event);
        let end = self.events.len();
        self.insert_at(end, posted, key);
    }

    /// `push_back` for a queue that may be closed: returns `posted` to the caller, who drops it
    /// after releasing the queue lock, when the queue is closed.
    pub(crate) fn try_push_back(&mut self, posted: PostedEvent) -> Option<PostedEvent> {
        if self.closed {
            return Some(posted);
        }
        self.push_back(posted);
        None
    }

    /// Inserts in priority order after the turn boundary, so an event posted during a turn waits
    /// for the next one (qthread_p.h:65-86).
    pub(crate) fn insert_posted(&mut self, posted: PostedEvent) {
        let key = self.compressor.merge_key(&posted.event);
        self.insert_keyed(posted, key);
    }

    fn insert_keyed(&mut self, posted: PostedEvent, key: Option<MergeKey>) {
        let pos = self.insert_position(posted.priority);
        self.insert_at(pos, posted, key);
    }

    /// The place of an event of `priority`: in front of the first event of a lower priority after
    /// the turn boundary, or the end. The events up to `sorted_from` are not in order and are
    /// looked at one by one; the rest are, so a binary search finds the place among them.
    fn insert_position(&self, priority: i32) -> usize {
        let start = self.insertion_offset.min(self.events.len());
        let sorted_from = self.sorted_from.max(start);
        if let Some(offset) = self
            .events
            .range(start..sorted_from)
            .position(|e| e.priority < priority)
        {
            return start + offset;
        }
        // Binary search among the sorted events: the first one of a lower priority.
        let (mut low, mut high) = (sorted_from, self.events.len());
        while low < high {
            let mid = low + (high - low) / 2;
            if self.events[mid].priority >= priority {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        low
    }

    /// Puts `posted` at `pos`, with `key` as its merge key, and brings `merge_index` and
    /// `sorted_from` along.
    fn insert_at(&mut self, pos: usize, mut posted: PostedEvent, key: Option<MergeKey>) {
        const STRIDE: u128 = 1 << 64;
        let order_at = |events: &VecDeque<PostedEvent>| {
            let previous = pos.checked_sub(1).map(|index| events[index].order).unwrap_or(0);
            let next = events.get(pos).map(|event| event.order).unwrap_or(u128::MAX);
            if pos == events.len() {
                previous.checked_add(STRIDE)
            } else if pos == 0 && next >= STRIDE {
                Some(next - STRIDE)
            } else if pos > 0 && next - previous > 1 {
                Some(previous + (next - previous) / 2)
            } else {
                None
            }
        };
        let mut order = order_at(&self.events);
        if order.is_none() {
            self.relabel_orders();
            order = order_at(&self.events);
        }
        posted.order = order.expect("relabeling leaves room for an event order label");
        posted.merge_key = key;
        if let Some(key) = key {
            let positions = self.merge_index.entry((posted.receiver, key)).or_default();
            let at = positions.partition_point(|&candidate| candidate < posted.order);
            positions.insert(at, posted.order);
        }
        if pos < self.sorted_from {
            self.sorted_from += 1;
        } else if pos > self.sorted_from && self.events[pos - 1].priority < posted.priority {
            self.sorted_from = pos;
        }
        *self.receiver_counts.entry(posted.receiver).or_default() += 1;
        self.events.insert(pos, posted);
    }

    fn relabel_orders(&mut self) {
        let step = u128::MAX / (self.events.len() as u128 + 2);
        for (index, event) in self.events.iter_mut().enumerate() {
            event.order = step * (index as u128 + 1);
        }
        self.rebuild_merge_index();
    }

    fn unindex(&mut self, receiver: ObjectId, key: MergeKey, order: u128) {
        let Some(positions) = self.merge_index.get_mut(&(receiver, key)) else {
            debug_assert!(false, "an event with a merge key is not in the index");
            return;
        };
        match positions.binary_search(&order) {
            Ok(at) => {
                positions.remove(at);
            }
            Err(_) => debug_assert!(false, "an event with a merge key is not in the index"),
        }
        if positions.is_empty() {
            self.merge_index.remove(&(receiver, key));
        }
    }

    fn rebuild_merge_index(&mut self) {
        self.merge_index.clear();
        for posted in &self.events {
            if let Some(key) = posted.merge_key {
                self.merge_index
                    .entry((posted.receiver, key))
                    .or_default()
                    .push(posted.order);
            }
        }
    }

    /// Checks what the index and `sorted_from` promise, against the events.
    #[cfg(test)]
    pub(crate) fn assert_consistent(&self) {
        let mut expected: HashMap<(ObjectId, MergeKey), Vec<u128>> = HashMap::new();
        let mut previous_order = None;
        for (pos, posted) in self.events.iter().enumerate() {
            assert!(
                previous_order.is_none_or(|previous| previous < posted.order),
                "event order labels are not strictly increasing at {pos}"
            );
            previous_order = Some(posted.order);
            assert_eq!(
                posted.merge_key,
                self.compressor.merge_key(&posted.event),
                "stale merge key at {pos}"
            );
            if let Some(key) = posted.merge_key {
                expected
                    .entry((posted.receiver, key))
                    .or_default()
                    .push(posted.order);
            }
        }
        assert_eq!(self.merge_index, expected, "merge index");
        assert!(
            self.sorted_from <= self.events.len(),
            "sorted_from past the end"
        );
        assert!(
            self.events
                .range(self.sorted_from..)
                .zip(self.events.range(self.sorted_from..).skip(1))
                .all(|(first, second)| first.priority >= second.priority),
            "events from sorted_from are not in priority order"
        );
        assert_eq!(
            self.receiver_counts.values().sum::<usize>(),
            self.events.len(),
            "receiver counts"
        );
        assert!(
            self.insertion_offset <= self.events.len(),
            "offset past the end"
        );
    }
}

pub fn compress_event_with_queue(
    queue: &mut EventQueue,
    receiver: ObjectId,
    event: &mut Event,
) -> bool {
    queue.compress_event(receiver, event)
}

/// One thread's loop state, shared by every `EventLoop` on that thread. Qt keeps one event
/// dispatcher, one posted-event queue and one timer registry per thread (`QThreadData`). A nested
/// `QEventLoop` only pushes itself onto the thread's stack of running loops, so an inner loop
/// ending never tears down state the outer loop still uses (qeventloop.cpp:157-192).
struct ThreadLoopState {
    thread_id: ThreadId,
    dispatcher: DefaultEventDispatcher,
    queue: Arc<Mutex<EventQueue>>,
    timer_registry: Arc<Mutex<TimerRegistry>>,
    /// Exit flags of every loop running `exec` on this thread, innermost last.
    running: Arc<parking_lot::Mutex<Vec<LoopExit>>>,
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

/// Pops a loop's entry from its thread's running-loop stack when `exec` returns.
struct RunningLoop(Arc<parking_lot::Mutex<Vec<LoopExit>>>);

impl Drop for RunningLoop {
    fn drop(&mut self) {
        self.0.lock().pop();
    }
}

thread_local! {
    /// The calling thread's live loop state. Weak, so the state (and its dispatcher) lives exactly
    /// as long as some `EventLoop` on the thread.
    static THREAD_LOOP_STATE: RefCell<Weak<ThreadLoopState>> = RefCell::new(Weak::new());
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
        let state = Arc::new(Self {
            thread_id,
            dispatcher,
            queue: Arc::new(Mutex::new(EventQueue::new())),
            timer_registry,
            running: Arc::new(parking_lot::Mutex::new(Vec::new())),
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
            let _ = catch_unwind(AssertUnwindSafe(|| send_event(receiver, &mut event)));
            let _ = catch_unwind(AssertUnwindSafe(|| finish_deferred_delete(receiver)));
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
            // loops never see it (`QEventDispatcherWin32::processEvents`).
            for running in self.state.running.lock().iter() {
                running.request(code);
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
    /// [`EventLoopHandle::exit`]. When it returns `Some(code)` the events already queued are
    /// delivered once and the loop returns `code`, as `QThread::exec` returns the code of an
    /// `exit` that came before it (qthread.cpp:685-689).
    pub(crate) fn exec_after(&self, pending_exit: impl FnOnce() -> Option<i32>) -> i32 {
        // qeventloop.cpp:157-158: entering a loop resets the exit flag, so exit() issued before
        // exec() does not end this loop.
        self.exit.exit_requested.store(false, Ordering::SeqCst);
        // qeventloop.cpp:192-195: entering the application's loop drops its stale posted Quit.
        if let Some(app_id) = crate::application::app_object_on_current_thread() {
            lock_unpoisoned(&self.state.queue).remove_posted_quits_for(app_id);
        }
        self.state.running.lock().push(self.exit.clone());
        let _running = RunningLoop(Arc::clone(&self.state.running));
        let _level = LoopLevelGuard::enter();
        if let Some(code) = pending_exit() {
            self.process_events(false);
            self.exit.request(code);
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
        if deliverable {
            if let Some(handle) = get_thread_event_sender(ThreadId::current()) {
                handle.wake_up();
            }
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
        {
            if modal
                || !deferred_delete_deliverable((loop_level, scope_level), current_levels(), false)
            {
                let posted = q.remove_at(index);
                q.insert_posted(posted);
                continue;
            }
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
        let _ = catch_unwind(AssertUnwindSafe(|| send_event(receiver, &mut event)));

        if let EventKind::DeferredDelete { .. } = event.kind {
            finish_deferred_delete(receiver);
        }

        delivered_count += 1;
    }

    (delivered_count, quit_code)
}

#[derive(Clone)]
pub struct EventLoopHandle {
    pub dispatcher: Arc<dyn EventDispatcherHandle>,
    pub queue: Arc<Mutex<EventQueue>>,
    running: Arc<parking_lot::Mutex<Vec<LoopExit>>>,
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
    pub(crate) fn post_from_slot(&self, receiver: ObjectId, event: &mut Option<Event>, priority: i32) {
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

    pub fn exit(&self, return_code: i32) {
        // qthread.cpp:754-767: QThread::exit ends every loop running on the thread.
        for running in self.running.lock().iter() {
            running.request(return_code);
        }
        self.dispatcher.wake_up();
    }

    pub fn quit(&self) {
        self.exit(0);
    }
}

/// Per-thread posted-event routing: the live loop handle (if any) and events posted before one
/// exists. Qt keeps `postEventList` in `QThreadData`, independent of the event dispatcher
/// (`QCoreApplication::postEvent`, qcoreapplication.cpp:1694), so an event posted to a thread
/// that has no loop yet is queued and delivered once the thread runs events.
struct ThreadEventRegistry {
    handles: HashMap<ThreadId, EventLoopHandle>,
    /// Same ordering and compression rules as a live loop's queue (`EventQueue`), so a thread
    /// that never starts a loop does not accumulate duplicate `UpdateRequest`s.
    pending: HashMap<ThreadId, EventQueue>,
    /// Threads whose locals have been destroyed (see [`mark_thread_ended`]). A thread id is never
    /// reused, so an entry stays valid; it costs a few bytes for each thread that used the library.
    ended: std::collections::HashSet<ThreadId>,
}

static THREAD_EVENT_HANDLES: Mutex<Option<ThreadEventRegistry>> = Mutex::new(None);

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
    // (qcoreapplication.cpp:1694).
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
fn routed_thread(receiver: ObjectId, default: ThreadId) -> ThreadId {
    if receiver == ObjectId(0) {
        return default;
    }
    crate::object::qobject::registration_thread_of(receiver).unwrap_or(default)
}

pub fn get_thread_event_sender(thread_id: ThreadId) -> Option<EventLoopHandle> {
    let reg = lock_unpoisoned(&THREAD_EVENT_HANDLES);
    reg.as_ref().and_then(|r| r.handles.get(&thread_id).cloned())
}

/// Moves the posted events addressed to `receivers` onto the queue of `to`, and runs `flip` (which
/// sets their physical registration) in the same critical section. A concurrent
/// `post_event_to_thread` either queued before this point, so its event moves here, or runs after
/// and is routed to `to`. The moved events keep their order and priority and are not compressed
/// (Qt `setThreadData_helper`, qobject.cpp:1805-1815, under the locks of qobject.cpp:1757-1758).
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
/// queues of threads without a loop yet (Qt `~QObject` → `removePostedEvents`, qobject.cpp:206-207).
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

impl Default for EventLoop {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::{
        register_owned_qobject, register_qobject_ptr, unregister_qobject, ObjectData, QObject,
        ThreadContext,
    };
    use std::ptr::NonNull;

    #[test]
    fn test_livelock_prevention() {
        struct LivelockWidget {
            data: ObjectData,
            queue: Arc<Mutex<EventQueue>>,
            call_count: usize,
        }

        impl LivelockWidget {
            fn new(id: ObjectId, queue: Arc<Mutex<EventQueue>>) -> Self {
                Self {
                    data: ObjectData::new(id),
                    queue,
                    call_count: 0,
                }
            }
        }

        impl QObject for LivelockWidget {
            fn object_data(&self) -> &ObjectData {
                &self.data
            }
            fn object_data_mut(&mut self) -> crate::object::ObjectDataMut<'_> {
        crate::object::ObjectDataMut::new(&mut self.data)
    }
            fn event(&mut self, _event: &mut Event) -> bool {
                self.call_count += 1;
                let mut q = self.queue.lock().unwrap();
                q.push_back(PostedEvent::new(
                    self.data.id(),
                    Event::new(EventKind::UpdateRequest),
                    0,
                ));
                true
            }
        }

        let event_loop = EventLoop::new();
        let q_clone = Arc::clone(event_loop.queue());

        let mut widget = LivelockWidget::new(ObjectId::next(), q_clone);
        // SAFETY: this test keeps the widget on this thread and alive through unregister.
        unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut widget)) };

        {
            let mut q = event_loop.queue().lock().unwrap();
            q.push_back(PostedEvent::new(
                widget.object_data().id(),
                Event::new(EventKind::UpdateRequest),
                0,
            ));
        }

        event_loop.send_posted_events();

        assert_eq!(widget.call_count, 1);
        assert_eq!(event_loop.queue().lock().unwrap().len(), 1);

        event_loop.send_posted_events();
        assert_eq!(widget.call_count, 2);
        assert_eq!(event_loop.queue().lock().unwrap().len(), 1);

        // SAFETY: the event loop has finished all callbacks for this test object.
        unsafe { unregister_qobject(widget.object_data().id()) };
    }

    #[test]
    fn test_lock_release_reentrancy() {
        struct ReentrantWidget {
            data: ObjectData,
            queue: Arc<Mutex<EventQueue>>,
            received: bool,
        }

        impl ReentrantWidget {
            fn new(id: ObjectId, queue: Arc<Mutex<EventQueue>>) -> Self {
                Self {
                    data: ObjectData::new(id),
                    queue,
                    received: false,
                }
            }
        }

        impl QObject for ReentrantWidget {
            fn object_data(&self) -> &ObjectData {
                &self.data
            }
            fn object_data_mut(&mut self) -> crate::object::ObjectDataMut<'_> {
        crate::object::ObjectDataMut::new(&mut self.data)
    }
            fn event(&mut self, _event: &mut Event) -> bool {
                self.received = true;
                let mut q = self.queue.lock().unwrap();
                q.post_event(self.data.id(), Event::new(EventKind::Quit { exit_code: 0 }));
                true
            }
        }

        let event_loop = EventLoop::new();
        let q_clone = Arc::clone(event_loop.queue());

        let mut widget = ReentrantWidget::new(ObjectId::next(), q_clone);
        // SAFETY: this test keeps the widget on this thread and alive through unregister.
        unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut widget)) };

        event_loop.post_event(
            widget.object_data().id(),
            Event::new(EventKind::UpdateRequest),
        );

        event_loop.send_posted_events();

        assert!(widget.received);
        assert_eq!(event_loop.queue().lock().unwrap().len(), 1);

        // SAFETY: the event loop has finished all callbacks for this test object.
        unsafe { unregister_qobject(widget.object_data().id()) };
    }

    #[test]
    fn test_deferred_delete_loop_level() {
        struct DeleteWidget {
            data: ObjectData,
            deleted: bool,
        }

        impl DeleteWidget {
            fn new(id: ObjectId) -> Self {
                Self {
                    data: ObjectData::new(id),
                    deleted: false,
                }
            }
        }

        impl QObject for DeleteWidget {
            fn object_data(&self) -> &ObjectData {
                &self.data
            }
            fn object_data_mut(&mut self) -> crate::object::ObjectDataMut<'_> {
        crate::object::ObjectDataMut::new(&mut self.data)
    }
            fn event(&mut self, event: &mut Event) -> bool {
                if matches!(event.kind, EventKind::DeferredDelete { .. }) {
                    self.deleted = true;
                    true
                } else {
                    false
                }
            }
        }

        let event_loop = EventLoop::new();
        let mut widget = DeleteWidget::new(ObjectId::next());
        // SAFETY: this test keeps the widget on this thread and alive through unregister.
        unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut widget)) };

        // delete_later stamps (loop 1, scope 0), promoted to scope 1 (qobject.cpp:2558-2559).
        ThreadContext::set_current_loop_level(1);
        widget.delete_later();

        // Gate: stamp sum 2 is not greater than the current sum 2 (qcoreapplication.cpp:1880-1885).
        ThreadContext::set_current_loop_level(2);
        event_loop.process_events(false);
        assert!(!widget.deleted);
        assert_eq!(event_loop.queue().lock().unwrap().len(), 1);

        ThreadContext::set_current_loop_level(1);
        event_loop.process_events(false);
        assert!(widget.deleted);
        assert_eq!(event_loop.queue().lock().unwrap().len(), 0);
        ThreadContext::set_current_loop_level(0);

        // SAFETY: the deferred-delete callback has returned.
        unsafe { unregister_qobject(widget.object_data().id()) };
    }

    #[test]
    fn test_process_events_pumping() {
        let event_loop = EventLoop::new();

        let handled = event_loop.process_events(false);
        assert!(!handled);

        let obj_id = ObjectId::next();
        event_loop.post_event(obj_id, Event::new(EventKind::LayoutRequest));
        let handled = event_loop.process_events(false);
        assert!(handled);
    }

    #[test]
    fn test_exec_and_exit() {
        let event_loop = EventLoop::new();
        let handle = event_loop.handle();
        let receiver_id = ObjectId::next();

        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            handle.post_quit(receiver_id, 42);
        });

        assert_eq!(event_loop.loop_level(), 0);

        let ret = event_loop.exec();

        assert_eq!(ret, 42);
        assert_eq!(event_loop.loop_level(), 0);
    }

    #[test]
    fn test_exec_exit_method() {
        struct ExitWidget {
            data: ObjectData,
            call_count: usize,
            received_quit: bool,
        }

        impl QObject for ExitWidget {
            fn object_data(&self) -> &ObjectData {
                &self.data
            }
            fn object_data_mut(&mut self) -> crate::object::ObjectDataMut<'_> {
        crate::object::ObjectDataMut::new(&mut self.data)
    }
            fn event(&mut self, event: &mut Event) -> bool {
                if matches!(event.kind, EventKind::Quit { .. }) {
                    self.received_quit = true;
                }
                self.call_count += 1;
                true
            }
        }

        let event_loop = EventLoop::new();
        let mut widget = ExitWidget {
            data: ObjectData::new(ObjectId::next()),
            call_count: 0,
            received_quit: false,
        };
        // SAFETY: this test keeps the widget on this thread and alive through unregister.
        unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut widget)) };

        event_loop.post_event(
            widget.object_data().id(),
            Event::new(EventKind::UpdateRequest),
        );
        event_loop.post_event(
            widget.object_data().id(),
            Event::new(EventKind::Quit { exit_code: 99 }),
        );

        let ret = event_loop.exec();
        assert_eq!(ret, 99);
        assert_eq!(widget.call_count, 2);
        assert!(widget.received_quit);
        assert_eq!(event_loop.loop_level(), 0);

        // SAFETY: the event loop has finished all callbacks for this test object.
        unsafe { unregister_qobject(widget.object_data().id()) };
    }

    #[test]
    fn exit_before_exec_is_noop() {
        let el = EventLoop::new();
        el.handle().exit(7);
        let h = el.handle();
        crate::timer::Timer::single_shot(0, move || h.exit(3));
        assert_eq!(el.exec(), 3);
    }

    #[test]
    fn second_exec_is_fresh() {
        let el = EventLoop::new();
        let h1 = el.handle();
        crate::timer::Timer::single_shot(0, move || h1.exit(7));
        assert_eq!(el.exec(), 7);

        let h2 = el.handle();
        crate::timer::Timer::single_shot(0, move || h2.exit(0));
        assert_eq!(el.exec(), 0);
    }

    #[test]
    fn test_basic_delivery() {
        struct MockObject {
            data: ObjectData,
            counter: usize,
        }

        impl MockObject {
            fn new(id: ObjectId) -> Self {
                Self {
                    data: ObjectData::new(id),
                    counter: 0,
                }
            }
        }

        impl QObject for MockObject {
            fn object_data(&self) -> &ObjectData {
                &self.data
            }
            fn object_data_mut(&mut self) -> crate::object::ObjectDataMut<'_> {
        crate::object::ObjectDataMut::new(&mut self.data)
    }
            fn event(&mut self, event: &mut Event) -> bool {
                if matches!(event.kind, EventKind::User(..)) {
                    self.counter += 1;
                    true
                } else {
                    false
                }
            }
        }

        let event_loop = EventLoop::new();
        let mut mock = MockObject::new(ObjectId::next());
        // SAFETY: this test keeps the object on this thread and alive through unregister.
        unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut mock)) };

        event_loop.post_event(
            mock.object_data().id(),
            Event::new(EventKind::User(Box::new(42u32))),
        );

        let processed = event_loop.process_events(false);

        assert!(processed);
        assert_eq!(mock.counter, 1);
        assert_eq!(event_loop.queue().lock().unwrap().len(), 0);

        // SAFETY: the event loop has finished all callbacks for this test object.
        unsafe { unregister_qobject(mock.object_data().id()) };
    }

    #[test]
    fn test_live_lock_prevention() {
        struct RelayObject {
            data: ObjectData,
            handle: EventLoopHandle,
            call_count: usize,
        }

        impl RelayObject {
            fn new(id: ObjectId, handle: EventLoopHandle) -> Self {
                Self {
                    data: ObjectData::new(id),
                    handle,
                    call_count: 0,
                }
            }
        }

        impl QObject for RelayObject {
            fn object_data(&self) -> &ObjectData {
                &self.data
            }
            fn object_data_mut(&mut self) -> crate::object::ObjectDataMut<'_> {
        crate::object::ObjectDataMut::new(&mut self.data)
    }
            fn event(&mut self, _event: &mut Event) -> bool {
                self.call_count += 1;
                self.handle
                    .post_event(self.data.id(), Event::new(EventKind::UpdateRequest));
                true
            }
        }

        let event_loop = EventLoop::new();
        let handle = event_loop.handle();

        let mut relay = RelayObject::new(ObjectId::next(), handle);
        // SAFETY: this test keeps the relay on this thread and alive through unregister.
        unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut relay)) };

        event_loop.post_event(relay.object_data().id(), Event::new(EventKind::UpdateRequest));

        let processed = event_loop.process_events(false);

        assert!(processed);
        assert_eq!(relay.call_count, 1);
        assert_eq!(event_loop.queue().lock().unwrap().len(), 1);

        // SAFETY: the event loop has finished all callbacks for this test object.
        unsafe { unregister_qobject(relay.object_data().id()) };
    }

    #[test]
    fn test_event_compression() {
        struct PaintWidget {
            data: ObjectData,
            paint_count: usize,
        }

        impl PaintWidget {
            fn new(id: ObjectId) -> Self {
                Self {
                    data: ObjectData::new(id),
                    paint_count: 0,
                }
            }
        }

        impl QObject for PaintWidget {
            fn object_data(&self) -> &ObjectData {
                &self.data
            }
            fn object_data_mut(&mut self) -> crate::object::ObjectDataMut<'_> {
        crate::object::ObjectDataMut::new(&mut self.data)
    }
            fn event(&mut self, event: &mut Event) -> bool {
                if matches!(event.kind, EventKind::UpdateRequest) {
                    self.paint_count += 1;
                    true
                } else {
                    false
                }
            }
        }

        let event_loop = EventLoop::new();
        let mut widget = PaintWidget::new(ObjectId::next());
        // SAFETY: this test keeps the widget on this thread and alive through unregister.
        unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut widget)) };

        let id = widget.object_data().id();

        for _ in 0..10 {
            event_loop.post_event(id, Event::new(EventKind::UpdateRequest));
        }

        assert_eq!(event_loop.queue().lock().unwrap().len(), 1);

        let processed = event_loop.process_events(false);
        assert!(processed);
        assert_eq!(widget.paint_count, 1);
        assert_eq!(event_loop.queue().lock().unwrap().len(), 0);

        // SAFETY: the event loop has finished all callbacks for this test object.
        unsafe { unregister_qobject(id) };
    }

    #[test]
    fn test_cross_thread_wakeup() {
        struct WakeupWidget {
            data: ObjectData,
            received_event: bool,
        }

        impl WakeupWidget {
            fn new(id: ObjectId) -> Self {
                Self {
                    data: ObjectData::new(id),
                    received_event: false,
                }
            }
        }

        impl QObject for WakeupWidget {
            fn object_data(&self) -> &ObjectData {
                &self.data
            }
            fn object_data_mut(&mut self) -> crate::object::ObjectDataMut<'_> {
        crate::object::ObjectDataMut::new(&mut self.data)
    }
            fn event(&mut self, event: &mut Event) -> bool {
                if matches!(event.kind, EventKind::User(..)) {
                    self.received_event = true;
                    true
                } else {
                    false
                }
            }
        }

        let event_loop = EventLoop::new();
        let handle = event_loop.handle();
        let mut widget = WakeupWidget::new(ObjectId::next());
        // SAFETY: this test keeps the widget alive but event is posted cross-thread.
        // QObject callback dispatch remains on this registration thread.
        unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut widget)) };

        let id = widget.object_data().id();

        let start = std::time::Instant::now();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            handle.post_event(id, Event::new(EventKind::User(Box::new("wakeup"))));
            handle.post_quit(id, 42);
        });

        let ret = event_loop.exec();
        let elapsed = start.elapsed();

        assert_eq!(ret, 42);
        assert!(widget.received_event);
        assert!(elapsed >= Duration::from_millis(15));
        assert_eq!(event_loop.loop_level(), 0);

        // SAFETY: the event loop has finished all callbacks for this test object.
        unsafe { unregister_qobject(id) };
    }

    #[test]
    fn test_notify_helper_pipeline_and_safe_removal() {
        use crate::object::{register_qobject_ptr, unregister_qobject, ObjectData};

        struct TraceFilter {
            data: ObjectData,
            trace: Arc<Mutex<Vec<&'static str>>>,
            label: &'static str,
            intercept: bool,
            remove_target: Option<ObjectId>,
        }

        impl TraceFilter {
            fn new(
                label: &'static str,
                intercept: bool,
                remove_target: Option<ObjectId>,
                trace: Arc<Mutex<Vec<&'static str>>>,
            ) -> Self {
                Self {
                    data: ObjectData::new(ObjectId::next()),
                    trace,
                    label,
                    intercept,
                    remove_target,
                }
            }
        }

        impl QObject for TraceFilter {
            fn object_data(&self) -> &ObjectData {
                &self.data
            }
            fn object_data_mut(&mut self) -> crate::object::ObjectDataMut<'_> {
        crate::object::ObjectDataMut::new(&mut self.data)
    }
            fn event_filter(&mut self, _watched: ObjectId, _event: &mut Event) -> bool {
                self.trace.lock().unwrap().push(self.label);
                if let Some(target) = self.remove_target {
                    crate::object::with_object_mut(target, |obj| {
                        obj.object_data_mut().remove_event_filter(self.data.id());
                    });
                    self.remove_target = None;
                }
                self.intercept
            }
        }

        struct TargetWidget {
            data: ObjectData,
            trace: Arc<Mutex<Vec<&'static str>>>,
        }

        impl TargetWidget {
            fn new(trace: Arc<Mutex<Vec<&'static str>>>) -> Self {
                Self {
                    data: ObjectData::new(ObjectId::next()),
                    trace,
                }
            }
        }

        impl QObject for TargetWidget {
            fn object_data(&self) -> &ObjectData {
                &self.data
            }
            fn object_data_mut(&mut self) -> crate::object::ObjectDataMut<'_> {
        crate::object::ObjectDataMut::new(&mut self.data)
    }
            fn event(&mut self, _event: &mut Event) -> bool {
                self.trace.lock().unwrap().push("target_event");
                true
            }
        }

        clear_application_event_filters();
        let trace = Arc::new(Mutex::new(Vec::new()));

        let mut app_filter = TraceFilter::new("app_filter", false, None, Arc::clone(&trace));
        let app_filter_id = app_filter.data.id();
        let mut target = TargetWidget::new(Arc::clone(&trace));
        let target_id = target.data.id();
        let mut obj_filter =
            TraceFilter::new("obj_filter", false, Some(target_id), Arc::clone(&trace));
        let obj_filter_id = obj_filter.data.id();
        // Set up before registering: the registry holds a pointer made here, and a use of the
        // variable after that would invalidate it.
        target.object_data_mut().install_event_filter(obj_filter_id);
        let app_filter_ptr = &raw mut app_filter;
        // SAFETY: the three objects stay on this thread, unmoved and alive, until unregistered
        // below; the variables are not written to after this, only `app_filter_ptr` is.
        unsafe {
            register_qobject_ptr(NonNull::new_unchecked(app_filter_ptr));
            register_qobject_ptr(NonNull::new_unchecked(&raw mut target));
            register_qobject_ptr(NonNull::new_unchecked(&raw mut obj_filter));
        }
        install_application_event_filter(app_filter_id);

        let mut event1 = Event::new(EventKind::UpdateRequest);
        let handled = send_event(target_id, &mut event1);
        assert!(handled);
        assert_eq!(
            *trace.lock().unwrap(),
            vec!["app_filter", "obj_filter", "target_event"]
        );

        trace.lock().unwrap().clear();
        let mut event2 = Event::new(EventKind::UpdateRequest);
        let handled2 = send_event(target_id, &mut event2);
        assert!(handled2);
        assert_eq!(*trace.lock().unwrap(), vec!["app_filter", "target_event"]);

        // SAFETY: no callback is active, and `app_filter_ptr` is the pointer that was registered.
        unsafe { (*app_filter_ptr).intercept = true };
        trace.lock().unwrap().clear();
        let mut event3 = Event::new(EventKind::UpdateRequest);
        let handled3 = send_event(target_id, &mut event3);
        assert!(handled3);
        assert_eq!(*trace.lock().unwrap(), vec!["app_filter"]);

        clear_application_event_filters();
        // SAFETY: the event-loop callback has returned and these registrations are owner-thread.
        unsafe {
            unregister_qobject(app_filter_id);
            unregister_qobject(obj_filter_id);
            unregister_qobject(target_id);
        }
    }

    type Trace = Arc<Mutex<Vec<&'static str>>>;
    /// Runs inside `ScriptedFilter::event_filter`; receives (watched object, this filter).
    type FilterAction = Box<dyn FnMut(ObjectId, ObjectId)>;

    struct ScriptedFilter {
        data: ObjectData,
        label: &'static str,
        trace: Trace,
        on_event: Option<FilterAction>,
    }

    impl ScriptedFilter {
        fn new(label: &'static str, trace: Trace) -> Self {
            Self {
                data: ObjectData::new(ObjectId::next()),
                label,
                trace,
                on_event: None,
            }
        }
    }

    impl QObject for ScriptedFilter {
        // Holds `!Send` state shared with the registering thread, so the object must stay there.
        fn can_move_to_thread(&self) -> bool {
            false
        }
        fn object_data(&self) -> &ObjectData {
            &self.data
        }
        fn object_data_mut(&mut self) -> crate::object::ObjectDataMut<'_> {
        crate::object::ObjectDataMut::new(&mut self.data)
    }
        fn event_filter(&mut self, watched: ObjectId, _event: &mut Event) -> bool {
            self.trace.lock().unwrap().push(self.label);
            let me = self.data.id();
            if let Some(action) = &mut self.on_event {
                action(watched, me);
            }
            false
        }
    }

    struct TraceTarget {
        data: ObjectData,
        trace: Trace,
    }

    impl TraceTarget {
        fn new(trace: Trace) -> Self {
            Self {
                data: ObjectData::new(ObjectId::next()),
                trace,
            }
        }
    }

    impl QObject for TraceTarget {
        fn object_data(&self) -> &ObjectData {
            &self.data
        }
        fn object_data_mut(&mut self) -> crate::object::ObjectDataMut<'_> {
        crate::object::ObjectDataMut::new(&mut self.data)
    }
        fn event(&mut self, _event: &mut Event) -> bool {
            self.trace.lock().unwrap().push("target");
            true
        }
    }

    #[test]
    fn filter_removed_by_earlier_filter_is_not_called() {
        // Qt: object filters are walked by live index; a removed slot is null and skipped
        // (qcoreapplication.cpp:1255-1265; qobject.cpp:2441-2446).
        let trace: Trace = Arc::new(Mutex::new(Vec::new()));
        let mut target = TraceTarget::new(Arc::clone(&trace));
        let mut earlier = ScriptedFilter::new("earlier", Arc::clone(&trace));
        let mut later = ScriptedFilter::new("later", Arc::clone(&trace));
        let target_id = target.data.id();
        let earlier_id = earlier.data.id();
        let later_id = later.data.id();
        // Set up before registering: a write through the variable after that would invalidate
        // the pointer the registry holds.
        target.object_data_mut().install_event_filter(later_id);
        target.object_data_mut().install_event_filter(earlier_id);
        earlier.on_event = Some(Box::new(move |watched: ObjectId, _: ObjectId| {
            crate::object::with_object_mut(watched, |obj| {
                obj.object_data_mut().remove_event_filter(later_id);
            });
        }));
        // SAFETY: all three stay on this thread and unmoved until unregistered below, and the
        // variables are not used again.
        unsafe {
            register_qobject_ptr(NonNull::new_unchecked(&raw mut target));
            register_qobject_ptr(NonNull::new_unchecked(&raw mut earlier));
            register_qobject_ptr(NonNull::new_unchecked(&raw mut later));
        }

        let mut event = Event::new(EventKind::UpdateRequest);
        assert!(send_event(target_id, &mut event));
        assert_eq!(*trace.lock().unwrap(), vec!["earlier", "target"]);

        // SAFETY: the dispatch above has returned; no callback is active.
        unsafe {
            unregister_qobject(target_id);
            unregister_qobject(earlier_id);
            unregister_qobject(later_id);
        }
    }

    #[test]
    fn app_filter_removed_by_earlier_filter_is_not_called() {
        // Qt: application filters use the same live index walk (qcoreapplication.cpp:1237-1247).
        clear_application_event_filters();
        let trace: Trace = Arc::new(Mutex::new(Vec::new()));
        let mut target = TraceTarget::new(Arc::clone(&trace));
        let mut earlier = ScriptedFilter::new("earlier", Arc::clone(&trace));
        let mut later = ScriptedFilter::new("later", Arc::clone(&trace));
        let target_id = target.data.id();
        let earlier_id = earlier.data.id();
        let later_id = later.data.id();
        // Set up before registering: a write through the variable after that would invalidate
        // the pointer the registry holds.
        earlier.on_event = Some(Box::new(move |_: ObjectId, _: ObjectId| {
            remove_application_event_filter(later_id);
        }));
        // SAFETY: all three stay on this thread and unmoved until unregistered below, and the
        // variables are not used again.
        unsafe {
            register_qobject_ptr(NonNull::new_unchecked(&raw mut target));
            register_qobject_ptr(NonNull::new_unchecked(&raw mut earlier));
            register_qobject_ptr(NonNull::new_unchecked(&raw mut later));
        }
        install_application_event_filter(later_id);
        install_application_event_filter(earlier_id);

        let mut event = Event::new(EventKind::UpdateRequest);
        assert!(send_event(target_id, &mut event));
        assert_eq!(*trace.lock().unwrap(), vec!["earlier", "target"]);

        clear_application_event_filters();
        // SAFETY: the dispatch above has returned; no callback is active.
        unsafe {
            unregister_qobject(target_id);
            unregister_qobject(earlier_id);
            unregister_qobject(later_id);
        }
    }

    #[test]
    fn filter_installed_during_dispatch_follows_qt_index_walk() {
        // Qt: installEventFilter prepends (qobject.cpp:2420-2422). The walk is already past index 0
        // when the callback runs, so the new filter is NOT called for this event. The prepend shifts
        // the running filter to index 1, and the `++i` step calls it once more for the same event
        // (qcoreapplication.cpp:1255-1265). The next event sees the new filter first.
        let trace: Trace = Arc::new(Mutex::new(Vec::new()));
        let mut target = TraceTarget::new(Arc::clone(&trace));
        let mut first = ScriptedFilter::new("first", Arc::clone(&trace));
        let mut added = ScriptedFilter::new("added", Arc::clone(&trace));
        let target_id = target.data.id();
        let first_id = first.data.id();
        let added_id = added.data.id();
        // Set up before registering: a write through the variable after that would invalidate
        // the pointer the registry holds.
        target.object_data_mut().install_event_filter(first_id);
        let mut installed = false;
        first.on_event = Some(Box::new(move |watched: ObjectId, _: ObjectId| {
            if !installed {
                installed = true;
                crate::object::with_object_mut(watched, |obj| {
                    obj.object_data_mut().install_event_filter(added_id);
                });
            }
        }));
        // SAFETY: all three stay on this thread and unmoved until unregistered below, and the
        // variables are not used again.
        unsafe {
            register_qobject_ptr(NonNull::new_unchecked(&raw mut target));
            register_qobject_ptr(NonNull::new_unchecked(&raw mut first));
            register_qobject_ptr(NonNull::new_unchecked(&raw mut added));
        }

        let mut event = Event::new(EventKind::UpdateRequest);
        assert!(send_event(target_id, &mut event));
        assert_eq!(*trace.lock().unwrap(), vec!["first", "first", "target"]);

        trace.lock().unwrap().clear();
        let mut next_event = Event::new(EventKind::UpdateRequest);
        assert!(send_event(target_id, &mut next_event));
        assert_eq!(*trace.lock().unwrap(), vec!["added", "first", "target"]);

        // SAFETY: the dispatches above have returned; no callback is active.
        unsafe {
            unregister_qobject(target_id);
            unregister_qobject(first_id);
            unregister_qobject(added_id);
        }
    }

    #[test]
    fn app_filter_installed_during_dispatch_follows_qt_index_walk() {
        // Qt: same prepend + index walk as the object chain, on the application list
        // (qcoreapplication.cpp:1237-1247; qobject.cpp:2420-2422).
        clear_application_event_filters();
        let trace: Trace = Arc::new(Mutex::new(Vec::new()));
        let mut target = TraceTarget::new(Arc::clone(&trace));
        let mut first = ScriptedFilter::new("first", Arc::clone(&trace));
        let mut added = ScriptedFilter::new("added", Arc::clone(&trace));
        let target_id = target.data.id();
        let first_id = first.data.id();
        let added_id = added.data.id();
        // Set up before registering: a write through the variable after that would invalidate
        // the pointer the registry holds.
        let mut installed = false;
        first.on_event = Some(Box::new(move |_: ObjectId, _: ObjectId| {
            if !installed {
                installed = true;
                install_application_event_filter(added_id);
            }
        }));
        // SAFETY: all three stay on this thread and unmoved until unregistered below, and the
        // variables are not used again.
        unsafe {
            register_qobject_ptr(NonNull::new_unchecked(&raw mut target));
            register_qobject_ptr(NonNull::new_unchecked(&raw mut first));
            register_qobject_ptr(NonNull::new_unchecked(&raw mut added));
        }
        install_application_event_filter(first_id);

        let mut event = Event::new(EventKind::UpdateRequest);
        assert!(send_event(target_id, &mut event));
        assert_eq!(*trace.lock().unwrap(), vec!["first", "first", "target"]);

        trace.lock().unwrap().clear();
        let mut next_event = Event::new(EventKind::UpdateRequest);
        assert!(send_event(target_id, &mut next_event));
        assert_eq!(*trace.lock().unwrap(), vec!["added", "first", "target"]);

        clear_application_event_filters();
        // SAFETY: the dispatches above have returned; no callback is active.
        unsafe {
            unregister_qobject(target_id);
            unregister_qobject(first_id);
            unregister_qobject(added_id);
        }
    }

    #[test]
    fn test_post_event_short_circuit_and_compression() {
        struct MockTarget {
            data: ObjectData,
        }
        impl MockTarget {
            fn new(id: ObjectId) -> Self {
                Self {
                    data: ObjectData::new(id),
                }
            }
        }
        impl QObject for MockTarget {
            fn object_data(&self) -> &ObjectData {
                &self.data
            }
            fn object_data_mut(&mut self) -> crate::object::ObjectDataMut<'_> {
        crate::object::ObjectDataMut::new(&mut self.data)
    }
            fn event(&mut self, _event: &mut Event) -> bool {
                true
            }
        }

        let event_loop = EventLoop::new();
        let mut target = MockTarget::new(ObjectId::next());
        // SAFETY: target stays on this thread and alive until the test ends.
        unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut target)) };

        let id = target.object_data().id();

        event_loop.post_event(id, Event::new(EventKind::UpdateRequest));
        assert_eq!(event_loop.queue().lock().unwrap().len(), 1);

        event_loop.post_event(id, Event::new(EventKind::UpdateRequest));
        assert_eq!(event_loop.queue().lock().unwrap().len(), 1);

        event_loop.post_event(id, Event::new(EventKind::Timer { timer_id: 10 }));
        assert_eq!(event_loop.queue().lock().unwrap().len(), 2);

        event_loop.post_event(id, Event::new(EventKind::Timer { timer_id: 10 }));

        assert_eq!(event_loop.queue().lock().unwrap().len(), 2);

        // SAFETY: no callback is active and this runs on the registration thread.
        unsafe { unregister_qobject(id) };
    }

    #[test]
    fn test_insertion_offset_priority_ordering() {
        let execution_order = Arc::new(Mutex::new(Vec::new()));

        struct PriorityRelay {
            data: ObjectData,
            order: Arc<Mutex<Vec<&'static str>>>,
            loop_handle: EventLoopHandle,
        }
        impl QObject for PriorityRelay {
            fn object_data(&self) -> &ObjectData {
                &self.data
            }
            fn object_data_mut(&mut self) -> crate::object::ObjectDataMut<'_> {
        crate::object::ObjectDataMut::new(&mut self.data)
    }
            fn event(&mut self, event: &mut Event) -> bool {
                if let EventKind::User(any_data) = &event.kind {
                    let label = *any_data.downcast_ref::<&'static str>().unwrap();
                    self.order.lock().unwrap().push(label);

                    if label == "initial_event_1" {
                        self.loop_handle.post_event_with_priority(
                            self.data.id(),
                            Event::new(EventKind::User(Box::new("high_priority_event"))),
                            100,
                        );
                    }
                    true
                } else {
                    false
                }
            }
        }

        let event_loop = EventLoop::new();
        let mut relay = PriorityRelay {
            data: ObjectData::new(ObjectId::next()),
            order: Arc::clone(&execution_order),
            loop_handle: event_loop.handle(),
        };
        // SAFETY: relay stays on this thread and alive until the test ends.
        unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut relay)) };
        let id = relay.data.id();

        event_loop.post_event_with_priority(
            id,
            Event::new(EventKind::User(Box::new("initial_event_1"))),
            0,
        );
        event_loop.post_event_with_priority(
            id,
            Event::new(EventKind::User(Box::new("initial_event_2"))),
            0,
        );

        let delivered = event_loop.send_posted_events();
        assert_eq!(delivered, 2);
        assert_eq!(
            *execution_order.lock().unwrap(),
            vec!["initial_event_1", "initial_event_2"],
        );

        let delivered2 = event_loop.send_posted_events();
        assert_eq!(delivered2, 1);
        assert_eq!(
            *execution_order.lock().unwrap(),
            vec!["initial_event_1", "initial_event_2", "high_priority_event"]
        );

        // SAFETY: no callback is active and this runs on the registration thread.
        unsafe { unregister_qobject(id) };
    }

    /// `QCoreApplication::sendPostedEvents` snapshots `insertionOffset` at the start
    /// of a pass (qcoreapplication.cpp:1815-1817), so events posted from a handler wait for the next
    /// pass, and events posted between passes are ordered by priority among the remaining entries.
    /// `CleanUp` shifts `insertionOffset` down by the number of erased entries (1844-1849).
    #[test]
    fn test_posted_during_pump_waits_for_next_pass_and_orders_by_priority() {
        let execution_order = Arc::new(Mutex::new(Vec::new()));

        struct NestedRelay {
            data: ObjectData,
            order: Arc<Mutex<Vec<&'static str>>>,
            loop_handle: EventLoopHandle,
        }
        impl QObject for NestedRelay {
            fn object_data(&self) -> &ObjectData {
                &self.data
            }
            fn object_data_mut(&mut self) -> crate::object::ObjectDataMut<'_> {
        crate::object::ObjectDataMut::new(&mut self.data)
    }
            fn event(&mut self, event: &mut Event) -> bool {
                if let EventKind::User(any_data) = &event.kind {
                    let label = *any_data.downcast_ref::<&'static str>().unwrap();
                    self.order.lock().unwrap().push(label);
                    if label == "B" {
                        self.loop_handle.post_event_with_priority(
                            self.data.id(),
                            Event::new(EventKind::User(Box::new("X"))),
                            0,
                        );
                        self.loop_handle.post_event_with_priority(
                            self.data.id(),
                            Event::new(EventKind::User(Box::new("Y"))),
                            100,
                        );
                    }
                    true
                } else {
                    false
                }
            }
        }

        let event_loop = EventLoop::new();
        let mut relay = NestedRelay {
            data: ObjectData::new(ObjectId::next()),
            order: Arc::clone(&execution_order),
            loop_handle: event_loop.handle(),
        };
        // SAFETY: relay stays on this thread and alive until the test ends.
        unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut relay)) };
        let id = relay.data.id();

        event_loop.post_event_with_priority(id, Event::new(EventKind::User(Box::new("A"))), 0);
        event_loop.post_event_with_priority(id, Event::new(EventKind::User(Box::new("B"))), 0);

        // Pass 1 delivers A and B; X and Y were posted during it and must not run yet.
        assert_eq!(event_loop.send_posted_events(), 2);
        assert_eq!(*execution_order.lock().unwrap(), vec!["A", "B"]);

        // Pass 2 delivers the nested posts, higher priority first.
        assert_eq!(event_loop.send_posted_events(), 2);
        assert_eq!(*execution_order.lock().unwrap(), vec!["A", "B", "Y", "X"]);

        // Posts made between passes: priority decides among them, insertion order breaks ties.
        event_loop.post_event_with_priority(id, Event::new(EventKind::User(Box::new("L"))), 0);
        event_loop.post_event_with_priority(id, Event::new(EventKind::User(Box::new("M"))), 0);
        event_loop.post_event_with_priority(id, Event::new(EventKind::User(Box::new("H"))), 100);
        assert_eq!(event_loop.send_posted_events(), 3);
        assert_eq!(
            *execution_order.lock().unwrap(),
            vec!["A", "B", "Y", "X", "H", "L", "M"]
        );

        // SAFETY: no callback is active and this runs on the registration thread.
        unsafe { unregister_qobject(id) };
    }
    #[test]
    fn test_generic_event_dispatcher_and_event_loop_abstraction() {
        use super::super::dispatcher::{EventDispatcher, GenericEventDispatcher};

        let generic_dispatcher = GenericEventDispatcher::new();
        let handle = generic_dispatcher.clone_handle();

        let handle_clone = Arc::clone(&handle);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            handle_clone.wake_up();
        });

        let res = generic_dispatcher.process_events(true, Some(Duration::from_millis(500)));
        assert_eq!(res, DispatchResult::Awoken);

        let el = EventLoop::new();
        let el_handle = el.handle();
        el_handle.wake_up();
    }

    fn meta_call<F: FnOnce() + Send + 'static>(f: F) -> Event {
        Event::new(EventKind::MetaCall(Box::new(move |_| f())))
    }

    fn post_current(event: Event) {
        post_event_to_thread(ThreadId::current(), ObjectId(0), event);
    }

    #[test]
    fn test_normal_pump_delivers_posted_events() {
        let el = EventLoop::new();
        let log = Arc::new(Mutex::new(Vec::new()));
        for name in ["a", "b"] {
            let log = Arc::clone(&log);
            post_current(meta_call(move || log.lock().unwrap().push(name)));
        }
        assert_eq!(el.send_posted_events(), 2);
        assert_eq!(*log.lock().unwrap(), vec!["a", "b"]);
        assert!(el.queue().lock().unwrap().events.is_empty());
    }

    #[test]
    fn test_modal_pump_keeps_turn_boundary() {
        let el = EventLoop::new();
        let log = Arc::new(Mutex::new(Vec::new()));
        {
            let log_a = Arc::clone(&log);
            let log_c = Arc::clone(&log);
            post_current(meta_call(move || {
                log_a.lock().unwrap().push("a");
                post_current(meta_call(move || log_c.lock().unwrap().push("c")));
            }));
        }
        {
            let log_b = Arc::clone(&log);
            post_current(meta_call(move || log_b.lock().unwrap().push("b")));
        }

        assert_eq!(pump_posted_events_modal(), Some(2));
        assert_eq!(*log.lock().unwrap(), vec!["a", "b"]);
        assert_eq!(
            el.queue().lock().unwrap().events.len(),
            1,
            "C waits for next turn"
        );

        assert_eq!(pump_posted_events_modal(), Some(1));
        assert_eq!(*log.lock().unwrap(), vec!["a", "b", "c"]);
    }

    #[test]
    fn test_modal_pump_without_event_loop_is_noop() {
        assert_eq!(pump_posted_events_modal(), None);
    }

    /// A native modal loop opened from a posted callback (`Menu::exec_popup` from
    /// `Timer::single_shot(0)`) is a nested event loop: Qt delivers posted events from it
    /// before the callback returns (`QMenuPrivate::exec` runs a `QEventLoop`, qmenu.cpp:2691-2697;
    /// `QCoreApplication::sendPostedEvents` re-enters, qcoreapplication.cpp:1796-1817).
    #[test]
    fn test_modal_pump_inside_a_posted_callback_delivers_events_before_it_returns() {
        let el = EventLoop::new();
        let nested = Arc::new(Mutex::new(None));
        let log = Arc::new(Mutex::new(Vec::new()));
        {
            let nested = Arc::clone(&nested);
            let log = Arc::clone(&log);
            post_current(meta_call(move || {
                let log_late = Arc::clone(&log);
                post_current(meta_call(move || log_late.lock().unwrap().push("late")));
                *nested.lock().unwrap() = Some(pump_posted_events_modal());
                log.lock().unwrap().push("outer returns");
            }));
        }

        assert_eq!(el.send_posted_events(), 1);
        assert_eq!(*nested.lock().unwrap(), Some(Some(1)));
        assert_eq!(*log.lock().unwrap(), vec!["late", "outer returns"]);
        assert!(el.queue().lock().unwrap().events.is_empty());
    }

    #[test]
    fn test_modal_pump_inside_a_modal_pump_delivers_events_before_it_returns() {
        let el = EventLoop::new();
        let nested = Arc::new(Mutex::new(None));
        let log = Arc::new(Mutex::new(Vec::new()));
        {
            let nested = Arc::clone(&nested);
            let log = Arc::clone(&log);
            post_current(meta_call(move || {
                let log_late = Arc::clone(&log);
                post_current(meta_call(move || log_late.lock().unwrap().push("late")));
                *nested.lock().unwrap() = Some(pump_posted_events_modal());
                log.lock().unwrap().push("outer returns");
            }));
        }

        assert_eq!(pump_posted_events_modal(), Some(1));
        assert_eq!(*nested.lock().unwrap(), Some(Some(1)));
        assert_eq!(*log.lock().unwrap(), vec!["late", "outer returns"]);
        assert!(el.queue().lock().unwrap().events.is_empty());
        assert!(!POSTED_PUMP_ACTIVE.with(|f| f.get()));
    }

    /// The nested pump takes every event queued when it starts (Qt resets `insertionOffset` to
    /// the list size, qcoreapplication.cpp:1817) and delivers each once; the outer pump resumes
    /// from the shared `startOffset` (1815-1816), so it neither repeats them nor runs events
    /// posted during the nested pump, which wait for the next turn (1856-1858).
    #[test]
    fn test_a_nested_pump_delivers_each_event_once_and_leaves_its_own_posts_for_the_next_turn() {
        let el = EventLoop::new();
        let log = Arc::new(Mutex::new(Vec::new()));
        {
            let log = Arc::clone(&log);
            post_current(meta_call(move || {
                let _ = pump_posted_events_modal();
                log.lock().unwrap().push("outer returns");
            }));
        }
        {
            let log_b = Arc::clone(&log);
            let log_d = Arc::clone(&log);
            post_current(meta_call(move || {
                log_b.lock().unwrap().push("b");
                post_current(meta_call(move || log_d.lock().unwrap().push("d")));
            }));
        }
        {
            let log = Arc::clone(&log);
            post_current(meta_call(move || log.lock().unwrap().push("c")));
        }

        el.send_posted_events();
        assert_eq!(*log.lock().unwrap(), vec!["b", "c", "outer returns"]);
        assert_eq!(
            el.queue().lock().unwrap().events.len(),
            1,
            "d waits for the next turn"
        );

        el.send_posted_events();
        assert_eq!(*log.lock().unwrap(), vec!["b", "c", "outer returns", "d"]);
        assert!(el.queue().lock().unwrap().events.is_empty());
    }

    /// An object deleted later from the outer loop survives a nested loop and is deleted once
    /// control is back in the loop that requested it (qcoreapplication.cpp:1877-1890).
    #[test]
    fn test_a_deferred_delete_from_the_outer_loop_waits_out_a_nested_pump() {
        struct Deletable {
            data: ObjectData,
            deleted: Arc<AtomicBool>,
        }
        impl QObject for Deletable {
            fn object_data(&self) -> &ObjectData {
                &self.data
            }
            fn object_data_mut(&mut self) -> crate::object::ObjectDataMut<'_> {
        crate::object::ObjectDataMut::new(&mut self.data)
    }
            fn event(&mut self, event: &mut Event) -> bool {
                if matches!(event.kind, EventKind::DeferredDelete { .. }) {
                    self.deleted.store(true, Ordering::SeqCst);
                    true
                } else {
                    false
                }
            }
        }

        let el = EventLoop::new();
        ThreadContext::set_current_loop_level(1);
        let deleted = Arc::new(AtomicBool::new(false));
        let mut object = Deletable {
            data: ObjectData::new(ObjectId::next()),
            deleted: Arc::clone(&deleted),
        };
        // SAFETY: the object stays on this thread and alive until the pump unregisters it after
        // delivering its deferred delete.
        unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut object)) };
        let object_addr = &object as *const Deletable as usize;

        let deleted_during_nested = Arc::new(Mutex::new(None));
        {
            let deleted = Arc::clone(&deleted);
            let deleted_during_nested = Arc::clone(&deleted_during_nested);
            post_current(meta_call(move || {
                // SAFETY: `object` is alive and not moved while this test's event pumps run.
                let object = unsafe { &*(object_addr as *const Deletable) };
                // Requested inside a handler: stamped at loop level 1, scope level 1 (qobject.cpp:2558-2559).
                object.delete_later();
                let _ = pump_posted_events_modal();
                *deleted_during_nested.lock().unwrap() = Some(deleted.load(Ordering::SeqCst));
            }));
        }

        el.send_posted_events();
        assert_eq!(*deleted_during_nested.lock().unwrap(), Some(false));
        el.send_posted_events();
        assert!(deleted.load(Ordering::SeqCst));
        assert!(el.queue().lock().unwrap().events.is_empty());
        ThreadContext::set_current_loop_level(0);
    }

    /// A handler that calls `delete_later` and then runs a nested non-modal pump keeps the target
    /// alive for that pump. `notify_helper` holds scope level 1 for the whole handler
    /// (qcoreapplication.cpp:1118), so the DeferredDelete stamped (1, 1) is not deliverable at
    /// (1, 0) (qcoreapplication.cpp:1881-1885). The outer loop delivers it once the handler returns.
    #[test]
    fn test_nested_pump_inside_a_handler_keeps_its_delete_later_target_alive() {
        struct Victim {
            data: ObjectData,
        }
        impl QObject for Victim {
            fn object_data(&self) -> &ObjectData {
                &self.data
            }
            fn object_data_mut(&mut self) -> crate::object::ObjectDataMut<'_> {
        crate::object::ObjectDataMut::new(&mut self.data)
    }
            fn event(&mut self, _event: &mut Event) -> bool {
                false
            }
        }

        struct Driver {
            data: ObjectData,
            victim_data: usize,
            victim_id: ObjectId,
            survived_nested: Arc<Mutex<Option<bool>>>,
            deleted_by_outer: Arc<Mutex<Option<bool>>>,
        }
        impl QObject for Driver {
            fn object_data(&self) -> &ObjectData {
                &self.data
            }
            fn object_data_mut(&mut self) -> crate::object::ObjectDataMut<'_> {
        crate::object::ObjectDataMut::new(&mut self.data)
    }
            fn event(&mut self, event: &mut Event) -> bool {
                match event.kind {
                    EventKind::MouseButtonPress { .. } => {
                        // SAFETY: the victim is boxed in the test and alive until the test ends;
                        // only the outer delivery unregisters it.
                        let victim = unsafe { &*(self.victim_data as *const ObjectData) };
                        victim.delete_later();
                        let handle =
                            get_thread_event_sender(ThreadId::current()).expect("loop registered");
                        let _ = pump_posted_events(&handle.queue, false);
                        *self.survived_nested.lock().unwrap() =
                            Some(crate::object::query_object_thread(self.victim_id).is_some());
                        post_event_to_thread(
                            ThreadId::current(),
                            self.data.id(),
                            Event::new_spontaneous(EventKind::MouseButtonRelease {
                                x: 0,
                                y: 0,
                                button: 1,
                            }),
                        );
                        true
                    }
                    EventKind::MouseButtonRelease { .. } => {
                        *self.deleted_by_outer.lock().unwrap() =
                            Some(crate::object::query_object_thread(self.victim_id).is_none());
                        if let Some(handle) = get_thread_event_sender(ThreadId::current()) {
                            handle.quit();
                        }
                        true
                    }
                    _ => false,
                }
            }
        }

        let survived_nested = Arc::new(Mutex::new(None));
        let deleted_by_outer = Arc::new(Mutex::new(None));
        let el = EventLoop::new();
        // SAFETY: the owner stays on this thread and alive for the whole test.
        let victim = unsafe {
            register_owned_qobject(Victim {
                data: ObjectData::new(ObjectId::next()),
            })
        };
        let victim_id = crate::object::OwnedObject::object_id(&victim);
        // SAFETY: as above.
        let driver = unsafe {
            register_owned_qobject(Driver {
                data: ObjectData::new(ObjectId::next()),
                victim_data: victim.with(|v| &v.data as *const ObjectData as usize).unwrap(),
                victim_id,
                survived_nested: Arc::clone(&survived_nested),
                deleted_by_outer: Arc::clone(&deleted_by_outer),
            })
        };
        let driver_id = crate::object::OwnedObject::object_id(&driver);
        post_event_to_thread(
            ThreadId::current(),
            driver_id,
            Event::new_spontaneous(EventKind::MouseButtonPress {
                x: 0,
                y: 0,
                button: 1,
            }),
        );

        assert_eq!(el.exec(), 0);
        assert_eq!(
            *survived_nested.lock().unwrap(),
            Some(true),
            "delete_later target was deleted by a nested pump inside its requesting handler"
        );
        assert_eq!(
            *deleted_by_outer.lock().unwrap(),
            Some(true),
            "delete_later target was not delivered by the outer loop after the handler returned"
        );
        // SAFETY: the driver is no longer delivered to once exec has returned.
        unsafe { unregister_qobject(driver_id) };
    }

    #[test]
    fn test_modal_pump_runs_sentinel_metacall() {
        let _el = EventLoop::new();
        let ran = Arc::new(AtomicBool::new(false));
        let ran_clone = Arc::clone(&ran);
        post_current(meta_call(move || ran_clone.store(true, Ordering::SeqCst)));
        assert_eq!(pump_posted_events_modal(), Some(1));
        assert!(ran.load(Ordering::SeqCst));
    }

    #[test]
    fn test_posts_made_during_a_nested_pump_wait_for_the_next_turn_in_priority_order() {
        let el = EventLoop::new();
        let log = Arc::new(Mutex::new(Vec::<&'static str>::new()));

        let log_outer = Arc::clone(&log);
        post_current(meta_call(move || {
            let _ = pump_posted_events_modal();
            log_outer.lock().unwrap().push("outer returns");
        }));
        let log_x = Arc::clone(&log);
        post_current(meta_call(move || {
            log_x.lock().unwrap().push("x");
            for (priority, name) in [(0, "p0"), (5, "p5")] {
                let log_p = Arc::clone(&log_x);
                post_event_to_thread_with_priority(
                    ThreadId::current(),
                    ObjectId(0),
                    meta_call(move || log_p.lock().unwrap().push(name)),
                    priority,
                );
            }
        }));
        let log_y = Arc::clone(&log);
        post_current(meta_call(move || log_y.lock().unwrap().push("y")));

        // `x` and `y` run inside the nested pump, before the outer callback returns.
        assert_eq!(el.send_posted_events(), 1);
        assert_eq!(*log.lock().unwrap(), vec!["x", "y", "outer returns"]);

        // Posts made during the nested turn wait for the next turn, highest priority first.
        assert_eq!(el.send_posted_events(), 2);
        assert_eq!(
            *log.lock().unwrap(),
            vec!["x", "y", "outer returns", "p5", "p0"]
        );
    }

    #[test]
    fn test_a_priority_post_between_turns_overtakes_a_lower_priority_leftover() {
        let el = EventLoop::new();
        let log = Arc::new(Mutex::new(Vec::<&'static str>::new()));

        let log_outer = Arc::clone(&log);
        post_current(meta_call(move || {
            // Posted during the turn, so it stays queued for the next turn at priority 0.
            let log_x = Arc::clone(&log_outer);
            post_event_to_thread_with_priority(
                ThreadId::current(),
                ObjectId(0),
                meta_call(move || log_x.lock().unwrap().push("x")),
                0,
            );
        }));
        assert_eq!(el.send_posted_events(), 1);

        let log_y = Arc::clone(&log);
        post_event_to_thread_with_priority(
            ThreadId::current(),
            ObjectId(0),
            meta_call(move || log_y.lock().unwrap().push("y")),
            5,
        );
        assert_eq!(el.send_posted_events(), 2);
        assert_eq!(*log.lock().unwrap(), vec!["y", "x"]);
    }

    struct UnregisteredProbe {
        data: ObjectData,
    }

    impl QObject for UnregisteredProbe {
        fn object_data(&self) -> &ObjectData {
            &self.data
        }

        fn object_data_mut(&mut self) -> crate::object::ObjectDataMut<'_> {
        crate::object::ObjectDataMut::new(&mut self.data)
    }
    }

    fn queued_for_current_thread() -> usize {
        get_thread_event_sender(ThreadId::current())
            .map(|handle| handle.queue.lock().unwrap().events.len())
            .unwrap_or(0)
    }

    #[test]
    fn test_purge_keeps_the_current_turn_boundary() {
        let el = EventLoop::new();
        let a = ObjectId::next();
        let b = ObjectId::next();
        for receiver in [a, b, a, b] {
            post_event_to_thread(ThreadId::current(), receiver, meta_call(|| {}));
        }
        {
            let mut q = el.queue().lock().unwrap();
            q.insertion_offset = q.events.len();
        }
        // SAFETY: `a` was never registered; the call only purges its posted events.
        unsafe { unregister_qobject(a) };
        let q = el.queue().lock().unwrap();
        assert_eq!(q.events.len(), 2);
        assert_eq!(
            q.insertion_offset, 2,
            "offset must count only surviving current-turn events"
        );
    }

    #[test]
    fn test_metacall_to_receiver_unregistered_before_delivery_is_dropped() {
        let el = EventLoop::new();
        let mut probe = UnregisteredProbe {
            data: ObjectData::new(ObjectId::next()),
        };
        let id = probe.data.id();
        unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut probe)) };
        let ran = Arc::new(AtomicBool::new(false));
        let ran_clone = Arc::clone(&ran);
        post_event_to_thread(
            ThreadId::current(),
            id,
            meta_call(move || ran_clone.store(true, Ordering::SeqCst)),
        );
        unsafe { unregister_qobject(id) };
        assert_eq!(el.send_posted_events(), 0);
        assert!(
            !ran.load(Ordering::SeqCst),
            "call to a destroyed receiver must not run"
        );
    }

    #[test]
    fn test_event_posted_to_unregistered_receiver_is_removed_from_queue() {
        let _el = EventLoop::new();
        let mut probe = UnregisteredProbe {
            data: ObjectData::new(ObjectId::next()),
        };
        let id = probe.data.id();
        unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut probe)) };
        post_event_to_thread(ThreadId::current(), id, meta_call(|| {}));
        assert_eq!(queued_for_current_thread(), 1);
        unsafe { unregister_qobject(id) };
        assert_eq!(queued_for_current_thread(), 0);
    }

    #[test]
    fn test_object_id_zero_metacall_still_runs() {
        let el = EventLoop::new();
        let ran = Arc::new(AtomicBool::new(false));
        let ran_clone = Arc::clone(&ran);
        post_event_to_thread(
            ThreadId::current(),
            ObjectId(0),
            meta_call(move || ran_clone.store(true, Ordering::SeqCst)),
        );
        assert_eq!(el.send_posted_events(), 1);
        assert!(ran.load(Ordering::SeqCst));
    }

    #[test]
    fn metacall_to_registered_borrowed_receiver_is_deferred_not_run_on_stub() {
        let _watchdog = Watchdog::arm(Duration::from_secs(10));
        let el = EventLoop::new();
        let mut probe = UnregisteredProbe {
            data: ObjectData::new(ObjectId::next()),
        };
        let id = probe.data.id();
        unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut probe)) };
        let ran = Arc::new(AtomicBool::new(false));
        let ran_clone = Arc::clone(&ran);
        post_event_to_thread(
            ThreadId::current(),
            id,
            meta_call(move || ran_clone.store(true, Ordering::SeqCst)),
        );
        // A callback on the receiver holds its borrow while the pump runs (re-entrant case).
        let during = crate::object::with_object_mut(id, |_| el.send_posted_events());
        assert_eq!(
            during,
            Some(0),
            "posted call must not be delivered while its receiver is borrowed"
        );
        assert!(
            !ran.load(Ordering::SeqCst),
            "call must not run on the NullObj stub while its receiver is borrowed"
        );
        assert_eq!(
            el.send_posted_events(),
            1,
            "call must be delivered once the borrow ends"
        );
        assert!(ran.load(Ordering::SeqCst));
        unsafe { unregister_qobject(id) };
    }

    /// Aborts the test process when a body hangs, so a regression fails instead of hanging CI.
    /// Disarmed on drop, so a passing test leaves no live thread behind.
    struct Watchdog {
        done: Arc<AtomicBool>,
    }

    impl Watchdog {
        fn arm(limit: Duration) -> Self {
            let done = Arc::new(AtomicBool::new(false));
            let flag = Arc::clone(&done);
            std::thread::spawn(move || {
                let deadline = std::time::Instant::now() + limit;
                while std::time::Instant::now() < deadline {
                    if flag.load(Ordering::SeqCst) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                // Written to the raw stderr so libtest output capture cannot swallow it on exit.
                let _ = std::io::Write::write_all(
                    &mut std::io::stderr(),
                    format!("watchdog: test body exceeded {limit:?}\n").as_bytes(),
                );
                std::process::exit(101);
            });
            Self { done }
        }
    }

    impl Drop for Watchdog {
        fn drop(&mut self) {
            self.done.store(true, Ordering::SeqCst);
        }
    }

    /// An inner loop that returns must not disturb the outer loop's timers or exit.
    #[test]
    fn inner_loop_exit_returns_only_the_inner_exec_and_the_outer_loop_keeps_running() {
        let _watchdog = Watchdog::arm(Duration::from_secs(10));
        let outer = EventLoop::new();
        let outer_handle = outer.handle();
        let log = Arc::new(Mutex::new(Vec::<&'static str>::new()));

        let log_outer = Arc::clone(&log);
        crate::timer::Timer::single_shot(0, move || {
            // A nested loop entered from a posted callback of the outer loop.
            let inner = EventLoop::new();
            let inner_exit = inner.exit.clone();
            crate::timer::Timer::single_shot(0, move || inner_exit.request(5));
            let inner_code = inner.exec();
            log_outer.lock().unwrap().push(if inner_code == 5 {
                "inner-returned"
            } else {
                "inner-wrong-code"
            });

            drop(inner);

            // Once the inner loop is gone, the outer loop must still run timers and accept exit.
            let log_timer = Arc::clone(&log_outer);
            crate::timer::Timer::single_shot(0, move || {
                log_timer.lock().unwrap().push("outer-timer");
                outer_handle.exit(0);
            });
        });

        let outer_code = outer.exec();
        assert_eq!(outer_code, 0);
        assert_eq!(*log.lock().unwrap(), vec!["inner-returned", "outer-timer"]);
    }

    /// A cross-thread exit ends every loop running on the target thread
    /// (qthread.cpp:754-767).
    #[test]
    fn cross_thread_handle_exit_ends_every_loop_running_on_that_thread() {
        let _watchdog = Watchdog::arm(Duration::from_secs(10));
        let outer = EventLoop::new();
        let handle = outer.handle();
        let (inner_running_tx, inner_running_rx) = std::sync::mpsc::channel::<()>();
        let log = Arc::new(Mutex::new(Vec::<String>::new()));

        // Another thread ends the loops only after the nested loop has started running.
        let exiter = std::thread::spawn(move || {
            inner_running_rx.recv().unwrap();
            handle.exit(3);
        });

        let log_outer = Arc::clone(&log);
        crate::timer::Timer::single_shot(0, move || {
            let inner = EventLoop::new();
            crate::timer::Timer::single_shot(0, move || {
                let _ = inner_running_tx.send(());
            });
            let inner_code = inner.exec();
            log_outer
                .lock()
                .unwrap()
                .push(format!("inner-{inner_code}"));
        });

        let outer_code = outer.exec();
        exiter.join().unwrap();
        assert_eq!(
            outer_code, 3,
            "the outer loop must end with the cross-thread code"
        );
        assert_eq!(*log.lock().unwrap(), vec!["inner-3".to_string()]);
    }

    /// A loop that registers on a thread already reported as ended (one that starts while the
    /// thread's locals are destroyed) gets the posts made after it registered; the ones made
    /// while the thread counted as ended were dropped.
    #[test]
    fn registering_a_loop_on_an_ended_thread_makes_it_receive_posts_again() {
        let delivered = Arc::new(parking_lot::Mutex::new(Vec::<&'static str>::new()));
        let event = |name: &'static str| {
            let log = Arc::clone(&delivered);
            Event::new(EventKind::MetaCall(Box::new(move |_| log.lock().push(name))))
        };
        let el = EventLoop::new();
        let thread = ThreadId::current();
        mark_thread_ended(thread);
        post_event_to_thread(thread, ObjectId(0), event("while-ended"));
        register_thread_event_loop(thread, el.handle());
        post_event_to_thread(thread, ObjectId(0), event("after-register"));
        el.process_events(false);
        assert_eq!(*delivered.lock(), vec!["after-register"]);
    }
}

#[cfg(test)]
mod merge_index_tests {
    use super::*;

    fn update_request() -> Event {
        Event::new(EventKind::UpdateRequest)
    }

    fn meta_call() -> Event {
        Event::new(EventKind::MetaCall(Box::new(|_| {})))
    }

    /// The queue as it was before it had an index and `sorted_from`: a plain vector that every
    /// post scans from the back, and in which an insertion scans from the turn boundary.
    #[derive(Default)]
    struct ScanQueue {
        events: Vec<PostedEvent>,
        insertion_offset: usize,
    }

    impl ScanQueue {
        fn post(&mut self, receiver: ObjectId, event: Event, priority: i32) {
            for posted in self.events.iter_mut().rev() {
                if posted.receiver == receiver
                    && CoreCompressor.try_compress(&mut posted.event, &event, receiver)
                {
                    return;
                }
            }
            self.insert(PostedEvent::new(receiver, event, priority));
        }

        fn insert(&mut self, posted: PostedEvent) {
            let start_search = self.insertion_offset.min(self.events.len());
            let relative_idx = self.events[start_search..]
                .iter()
                .position(|e| e.priority < posted.priority)
                .unwrap_or(self.events.len() - start_search);
            self.events.insert(start_search + relative_idx, posted);
        }

        fn remove_at(&mut self, index: usize) -> PostedEvent {
            if index < self.insertion_offset {
                self.insertion_offset -= 1;
            }
            self.events.remove(index)
        }

        fn remove_receiver(&mut self, receiver: ObjectId) {
            let turn_end = self.insertion_offset;
            let mut removed_in_turn = 0;
            let mut index = 0;
            self.events.retain(|posted| {
                let remove = posted.receiver == receiver;
                if remove && index < turn_end {
                    removed_in_turn += 1;
                }
                index += 1;
                !remove
            });
            self.insertion_offset -= removed_in_turn;
        }
    }

    /// What tells two queued events apart: the receiver, the priority and the kind with its data.
    fn summary<'a>(events: impl IntoIterator<Item = &'a PostedEvent>) -> Vec<String> {
        events
            .into_iter()
            .map(|posted| {
                let kind = match &posted.event.kind {
                    EventKind::User(data) => {
                        format!("User({})", data.downcast_ref::<u32>().unwrap())
                    }
                    kind => format!("{kind:?}"),
                };
                format!("{:?} p{} {kind}", posted.receiver, posted.priority)
            })
            .collect()
    }

    struct Rng(u64);

    impl Rng {
        fn next(&mut self, below: u64) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0 % below
        }
    }

    /// A kind of event that the compressor merges, one that it does not, and a payload that tells
    /// events of the second kind apart.
    fn random_event(rng: &mut Rng, serial: &mut u32) -> Event {
        let kind = match rng.next(9) {
            0 | 1 => EventKind::UpdateRequest,
            2 => EventKind::LayoutRequest,
            3 => EventKind::Timer {
                timer_id: rng.next(3),
            },
            4 => EventKind::ZeroTimer {
                timer_id: rng.next(2),
            },
            5 => EventKind::Quit {
                exit_code: rng.next(100) as i32,
            },
            6 => EventKind::Resize {
                width: rng.next(1000) as i32,
                height: rng.next(1000) as i32,
                old_width: 0,
                old_height: 0,
            },
            7 => return meta_call(),
            _ => {
                *serial += 1;
                EventKind::User(Box::new(*serial))
            }
        };
        Event::new(kind)
    }

    /// Runs the same random operations on an `EventQueue` and on the scanning queue it replaced,
    /// and requires the same events in the same order after each of them, and an index that is
    /// what the events say it should be.
    #[test]
    fn the_queue_matches_a_scanning_queue_under_random_operations() {
        let (seeds, steps) = if cfg!(miri) { (3, 150) } else { (40, 3000) };
        for seed in 1..=seeds {
            let mut rng = Rng(0x9E37_79B9_7F4A_7C15 ^ (seed * 0x1234_5678_9ABC));
            let mut serial = 0u32;
            let mut queue = EventQueue::new();
            let mut model = ScanQueue::default();

            for step in 0..steps {
                let receiver = ObjectId(rng.next(4));
                let priority = [-1, 0, 0, 0, 1, 3][rng.next(6) as usize];
                match rng.next(100) {
                    0..=64 => {
                        // Both queues get an event that has the same payload.
                        let before = serial;
                        let event = random_event(&mut rng, &mut serial);
                        let twin = match &event.kind {
                            EventKind::User(_) => Event::new(EventKind::User(Box::new(before + 1))),
                            EventKind::MetaCall(_) => meta_call(),
                            kind => Event::new(clone_plain_kind(kind)),
                        };
                        queue.post_event_with_priority(receiver, event, priority);
                        model.post(receiver, twin, priority);
                    }
                    65..=69 => {
                        queue.insertion_offset = queue.events.len();
                        model.insertion_offset = model.events.len();
                    }
                    70..=79 if !queue.events.is_empty() => {
                        let index = rng.next(queue.events.len() as u64) as usize;
                        queue.remove_at(index);
                        model.remove_at(index);
                    }
                    80..=82 => {
                        queue.insertion_offset = 0;
                        model.insertion_offset = 0;
                    }
                    83..=89 => {
                        queue.remove_posted_events(receiver);
                        model.remove_receiver(receiver);
                    }
                    90..=93 => {
                        let twin = Event::new(EventKind::UpdateRequest);
                        queue.push_back(PostedEvent::new(receiver, update_request(), priority));
                        model
                            .events
                            .push(PostedEvent::new(receiver, twin, priority));
                    }
                    94..=97 if !queue.events.is_empty() => {
                        // A gated `DeferredDelete` leaves the turn and is queued again.
                        let index = rng.next(queue.events.len() as u64) as usize;
                        let posted = queue.remove_at(index);
                        queue.insert_posted(posted);
                        let posted = model.remove_at(index);
                        model.insert(posted);
                    }
                    _ => {}
                }
                queue.assert_consistent();
                assert_eq!(
                    summary(&queue.events),
                    summary(&model.events),
                    "seed {seed} step {step}"
                );
                assert_eq!(
                    queue.insertion_offset, model.insertion_offset,
                    "seed {seed} step {step}"
                );
            }
        }
    }

    #[test]
    fn stable_order_labels_handle_repeated_front_middle_and_tail_edits() {
        let mut queue = EventQueue::new();
        let receiver = ObjectId(17);
        let key = MergeKey::new(3, 0);

        for _ in 0..250 {
            queue.insert_at(0, PostedEvent::new(receiver, update_request(), 0), Some(key));
        }
        for _ in 0..250 {
            queue.push_back(PostedEvent::new(receiver, update_request(), 0));
        }
        for _ in 0..250 {
            let middle = queue.len() / 2;
            queue.insert_at(middle, PostedEvent::new(receiver, update_request(), 0), Some(key));
        }
        queue.assert_consistent();

        let len = queue.len();
        queue.post_event(receiver, update_request());
        assert_eq!(queue.len(), len, "the indexed candidate must be merged");
        for _ in 0..200 {
            queue.remove_at(queue.len() / 3);
        }
        queue.assert_consistent();
        queue.post_event(receiver, update_request());
        queue.assert_consistent();
    }

    /// The kinds `random_event` makes that carry no payload that has to stay unique.
    fn clone_plain_kind(kind: &EventKind) -> EventKind {
        match kind {
            EventKind::UpdateRequest => EventKind::UpdateRequest,
            EventKind::LayoutRequest => EventKind::LayoutRequest,
            EventKind::Timer { timer_id } => EventKind::Timer {
                timer_id: *timer_id,
            },
            EventKind::ZeroTimer { timer_id } => EventKind::ZeroTimer {
                timer_id: *timer_id,
            },
            EventKind::Quit { exit_code } => EventKind::Quit {
                exit_code: *exit_code,
            },
            EventKind::Resize {
                width,
                height,
                old_width,
                old_height,
            } => EventKind::Resize {
                width: *width,
                height: *height,
                old_width: *old_width,
                old_height: *old_height,
            },
            other => panic!("not a plain kind: {other:?}"),
        }
    }

    #[test]
    fn events_that_cannot_merge_are_not_indexed_and_do_not_hide_the_ones_that_can() {
        let mut queue = EventQueue::new();
        let widget = ObjectId(7);
        queue.post_event(widget, update_request());
        queue.post_event(widget, Event::new(EventKind::Timer { timer_id: 1 }));
        let others = if cfg!(miri) { 50 } else { 5000 };
        for _ in 0..others {
            queue.post_event(ObjectId(0), meta_call());
            queue.post_event(widget, meta_call());
        }
        queue.assert_consistent();
        assert_eq!(queue.len(), 2 + 2 * others);
        assert_eq!(
            queue.merge_index.len(),
            2,
            "only the update request and the timer are indexed"
        );

        // Both find their candidate behind all the calls.
        queue.post_event(widget, update_request());
        queue.post_event(widget, Event::new(EventKind::Timer { timer_id: 1 }));
        assert_eq!(queue.len(), 2 + 2 * others, "both were merged");
        // A timer with another id and an update request for another receiver are new events.
        queue.post_event(widget, Event::new(EventKind::Timer { timer_id: 2 }));
        queue.post_event(ObjectId(8), update_request());
        assert_eq!(queue.len(), 4 + 2 * others);
        queue.assert_consistent();
    }

    #[test]
    fn removing_posted_events_keeps_the_index_in_step() {
        let mut queue = EventQueue::new();
        let (a, b) = (ObjectId(1), ObjectId(2));
        queue.post_event(a, update_request());
        queue.post_event(b, update_request());
        queue.post_event(ObjectId(0), meta_call());
        queue.post_event(a, Event::new(EventKind::Timer { timer_id: 5 }));
        queue.post_event(b, Event::new(EventKind::LayoutRequest));
        queue.assert_consistent();

        // Everything of `a` goes; what was behind it moves forward in the index with it.
        let removed = queue.remove_posted_events(a);
        assert_eq!(removed.len(), 2);
        queue.assert_consistent();
        assert_eq!(queue.len(), 3);

        queue.post_event(b, update_request());
        queue.post_event(b, Event::new(EventKind::LayoutRequest));
        assert_eq!(queue.len(), 3, "b's events merge into the ones that moved");
        queue.post_event(a, update_request());
        assert_eq!(
            queue.len(),
            4,
            "a's update request was removed, not merged into"
        );

        // Delivering from the front, as a pump does.
        queue.insertion_offset = queue.len();
        let first = queue.remove_at(0);
        assert_eq!(first.receiver, b);
        queue.assert_consistent();
        queue.post_event(b, update_request());
        assert_eq!(
            queue.len(),
            4,
            "the removed update request is not merged into"
        );
        queue.post_event(b, Event::new(EventKind::LayoutRequest));
        assert_eq!(queue.len(), 4, "the layout request behind it still is");
        queue.assert_consistent();

        let moved = queue.remove_posted_events_matching(|id| id == a || id == b);
        assert_eq!(moved.len(), 3);
        queue.assert_consistent();
        assert!(queue.merge_index.is_empty());
    }

    #[test]
    fn an_insertion_goes_in_front_of_the_first_lower_priority_after_the_turn() {
        let mut queue = EventQueue::new();
        let r = ObjectId(0);
        for (label, priority) in [(1u32, 0), (2, 0)] {
            queue.post_event_with_priority(
                r,
                Event::new(EventKind::User(Box::new(label))),
                priority,
            );
        }
        // A turn that leaves its events queued, with higher priorities posted behind it.
        queue.insertion_offset = queue.len();
        queue.post_event_with_priority(r, Event::new(EventKind::User(Box::new(3u32))), 5);
        queue.post_event_with_priority(r, Event::new(EventKind::User(Box::new(4u32))), 5);
        queue.insertion_offset = 0;
        queue.assert_consistent();
        // `[1 p0, 2 p0, 3 p5, 4 p5]`: a priority 3 event goes in front of the first lower one.
        queue.post_event_with_priority(r, Event::new(EventKind::User(Box::new(5u32))), 3);
        queue.assert_consistent();
        assert_eq!(
            summary(queue.events()),
            [
                "ObjectId(0) p3 User(5)",
                "ObjectId(0) p0 User(1)",
                "ObjectId(0) p0 User(2)",
                "ObjectId(0) p5 User(3)",
                "ObjectId(0) p5 User(4)",
            ]
        );
    }

    /// A compressor that merges everything of a receiver, and leaves the default merge key.
    struct MergeAll;

    impl EventCompressor for MergeAll {
        fn try_compress(
            &self,
            _existing: &mut Event,
            _incoming: &Event,
            _receiver: ObjectId,
        ) -> bool {
            true
        }
    }

    #[test]
    fn installing_a_compressor_gives_the_queued_events_its_keys() {
        let mut queue = EventQueue::new();
        let widget = ObjectId(3);
        queue.post_event(widget, update_request());
        queue.post_event(widget, meta_call());
        assert_eq!(queue.len(), 2, "the core compressor does not merge a call");
        assert_eq!(queue.merge_index.len(), 1);

        let replaced = queue.replace_compressor(Arc::new(MergeAll));
        queue.assert_consistent();
        assert_eq!(queue.merge_index.len(), 1, "both are in one class now");
        assert!(queue.merge_index[&(widget, MergeKey::new(0, 0))][0]
            < queue.merge_index[&(widget, MergeKey::new(0, 0))][1]);

        queue.post_event(widget, Event::new(EventKind::LayoutRequest));
        assert_eq!(queue.len(), 2, "any event of the receiver is merged now");
        queue.post_event(ObjectId(4), update_request());
        assert_eq!(queue.len(), 3, "but only into those of its own receiver");

        let merge_all = queue.replace_compressor(replaced);
        queue.assert_consistent();
        assert_eq!(
            queue.merge_index.len(),
            2,
            "the update requests of both receivers"
        );
        drop(merge_all);
    }
}
