// SPDX-License-Identifier: LGPL-3.0-only
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
        queue.insert_at(
            0,
            PostedEvent::new(receiver, update_request(), 0),
            Some(key),
        );
    }
    for _ in 0..250 {
        queue.push_back(PostedEvent::new(receiver, update_request(), 0));
    }
    for _ in 0..250 {
        let middle = queue.len() / 2;
        queue.insert_at(
            middle,
            PostedEvent::new(receiver, update_request(), 0),
            Some(key),
        );
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
        queue.post_event_with_priority(r, Event::new(EventKind::User(Box::new(label))), priority);
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
    fn try_compress(&self, _existing: &mut Event, _incoming: &Event, _receiver: ObjectId) -> bool {
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
    assert!(
        queue.merge_index[&(widget, MergeKey::new(0, 0))][0]
            < queue.merge_index[&(widget, MergeKey::new(0, 0))][1]
    );

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
