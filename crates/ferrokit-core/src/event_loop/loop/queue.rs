// SPDX-License-Identifier: LGPL-3.0-only
use super::*;

#[derive(Debug)]
pub struct PostedEvent {
    pub receiver: ObjectId,
    pub event: Event,
    pub priority: i32,
    /// Stable queue-order label used by the merge index.
    pub(super) order: u128,
    /// The merge key of `event` under the compressor of the queue that holds it, set when the
    /// queue takes the event in. It means nothing outside a queue.
    pub(super) merge_key: Option<MergeKey>,
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
    pub(super) events: VecDeque<PostedEvent>,
    /// Number of queued events of each receiver, so a purge for an object that has none costs
    /// no scan of the queue (Qt keeps a posted-event count per object).
    pub(super) receiver_counts: HashMap<ObjectId, usize>,
    pub(crate) insertion_offset: usize,
    pub(super) compressor: Arc<dyn EventCompressor>,
    /// Stable queue-order labels of keyed events, ascending, by receiver and key. Compressing an
    /// event uses these labels to find its candidates without changing the index on queue shifts.
    pub(super) merge_index: HashMap<(ObjectId, MergeKey), Vec<u128>>,
    /// `events[sorted_from..]` is in non-increasing priority order, so `insert_posted` finds its
    /// place there by binary search and walks only the events in front of it. Those are events
    /// that a turn left queued (those of a borrowed receiver), with events of a higher priority
    /// posted behind them; delivering or removing them moves `sorted_from` back toward the front.
    pub(super) sorted_from: usize,
    /// Set once the loop that turned this queue is gone (`close`). A closed queue takes no event:
    /// handles on it that outlive the loop (`EventSender` and `EventLoopHandle` clones) would
    /// otherwise queue events that nothing can run or drop.
    pub(super) closed: bool,
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
    pub(super) fn merge_into_queued(
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

    pub fn post_event_with_priority(&mut self, receiver: ObjectId, event: Event, priority: i32) {
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
    /// `insertion_offset` with it (Qt `insertionOffset -= startOffset`).
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

    pub(super) fn count_removed(&mut self, receiver: ObjectId) {
        if let Some(count) = self.receiver_counts.get_mut(&receiver) {
            *count -= 1;
            if *count == 0 {
                self.receiver_counts.remove(&receiver);
            }
        }
    }

    /// Removes every posted event addressed to `receiver` (Qt
    /// the core application). The removed events
    /// are returned, not dropped here: a payload's `Drop` may unregister objects and re-enter this
    /// queue, so the caller drops them after releasing the lock.
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

    /// Removes posted `Quit` events addressed to `receiver`.
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
    /// for the next one.
    pub(crate) fn insert_posted(&mut self, posted: PostedEvent) {
        let key = self.compressor.merge_key(&posted.event);
        self.insert_keyed(posted, key);
    }

    pub(super) fn insert_keyed(&mut self, posted: PostedEvent, key: Option<MergeKey>) {
        let pos = self.insert_position(posted.priority);
        self.insert_at(pos, posted, key);
    }

    /// The place of an event of `priority`: in front of the first event of a lower priority after
    /// the turn boundary, or the end. The events up to `sorted_from` are not in order and are
    /// looked at one by one; the rest are, so a binary search finds the place among them.
    pub(super) fn insert_position(&self, priority: i32) -> usize {
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
    pub(super) fn insert_at(&mut self, pos: usize, mut posted: PostedEvent, key: Option<MergeKey>) {
        const STRIDE: u128 = 1 << 64;
        let order_at = |events: &VecDeque<PostedEvent>| {
            let previous = pos
                .checked_sub(1)
                .map(|index| events[index].order)
                .unwrap_or(0);
            let next = events
                .get(pos)
                .map(|event| event.order)
                .unwrap_or(u128::MAX);
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

    pub(super) fn relabel_orders(&mut self) {
        let step = u128::MAX / (self.events.len() as u128 + 2);
        for (index, event) in self.events.iter_mut().enumerate() {
            event.order = step * (index as u128 + 1);
        }
        self.rebuild_merge_index();
    }

    pub(super) fn unindex(&mut self, receiver: ObjectId, key: MergeKey, order: u128) {
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

    pub(super) fn rebuild_merge_index(&mut self) {
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
