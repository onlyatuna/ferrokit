// SPDX-License-Identifier: LGPL-3.0-only
use crate::event_loop::PostedEvent;

use crate::event::{Event, EventKind};
use crate::object::ObjectId;

/// Event compression trait: determines whether a new event can be coalesced
/// with an existing queued event. Modeled after Qt's `QCoreApplication::compressEvent`.
pub trait EventCompressor: Send + Sync {
    /// Returns true if incoming event was successfully merged or deduplicated.
    fn try_compress(&self, existing: &mut Event, incoming: &Event, receiver: ObjectId) -> bool;
}

/// Default core event compressor.
///
/// Implements compression for core event types:
/// 1. Timer events: deduplicated by matching TimerId
/// 2. UpdateRequest: deduplicated
/// 3. LayoutRequest: deduplicated
/// 4. Quit events: latest exit code overrides in-place
#[derive(Debug, Default, Clone, Copy)]
pub struct CoreCompressor;

impl EventCompressor for CoreCompressor {
    fn try_compress(&self, existing: &mut Event, incoming: &Event, _receiver: ObjectId) -> bool {
        match (&mut existing.kind, &incoming.kind) {
            (EventKind::Timer { timer_id: id1 }, EventKind::Timer { timer_id: id2 }) => id1 == id2,
            (EventKind::ZeroTimer { timer_id: id1 }, EventKind::ZeroTimer { timer_id: id2 }) => {
                id1 == id2
            }
            (EventKind::UpdateRequest, EventKind::UpdateRequest) => true,
            (EventKind::LayoutRequest, EventKind::LayoutRequest) => true,
            (EventKind::Quit { exit_code: code1 }, EventKind::Quit { exit_code: code2 }) => {
                *code1 = *code2;
                true
            }
            (
                EventKind::Resize {
                    width: w1,
                    height: h1,
                    ..
                },
                EventKind::Resize {
                    width: w2,
                    height: h2,
                    ..
                },
            ) => {
                *w1 = *w2;
                *h1 = *h2;
                true
            }
            _ => false,
        }
    }
}

/// Reverse-traversal search to coalesce events for the same receiver.
/// Modeled after Qt QCoreApplication::compressEvent.
pub fn compress_event(
    events: &mut [PostedEvent],
    receiver: ObjectId,
    incoming: &Event,
    compressor: &dyn EventCompressor,
) -> bool {
    for posted in events.iter_mut().rev() {
        if posted.receiver == receiver
            && compressor.try_compress(&mut posted.event, incoming, receiver)
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Event, EventKind};
    use crate::event_loop::PostedEvent;
    use crate::object::ObjectId;

    fn make_event(kind: EventKind) -> Event {
        Event::new(kind)
    }

    #[test]
    fn test_different_receivers_not_compressed() {
        let compressor = CoreCompressor;
        let obj_a = ObjectId(1);
        let obj_b = ObjectId(2);
        let mut queue: Vec<PostedEvent> = Vec::new();

        queue.push(PostedEvent {
            receiver: obj_a,
            event: make_event(EventKind::UpdateRequest),
            priority: 0,
        });

        let new_event = make_event(EventKind::UpdateRequest);
        let compressed = compress_event(&mut queue, obj_b, &new_event, &compressor);

        assert!(!compressed);
    }

    #[test]
    fn test_timer_compression_by_id() {
        let compressor = CoreCompressor;
        let obj_a = ObjectId(1);
        let mut queue: Vec<PostedEvent> = Vec::new();

        queue.push(PostedEvent {
            receiver: obj_a,
            event: make_event(EventKind::Timer { timer_id: 10 }),
            priority: 0,
        });

        let same_timer = make_event(EventKind::Timer { timer_id: 10 });
        assert!(compress_event(&mut queue, obj_a, &same_timer, &compressor));
        assert_eq!(queue.len(), 1);

        let diff_timer = make_event(EventKind::Timer { timer_id: 20 });
        assert!(!compress_event(&mut queue, obj_a, &diff_timer, &compressor));
    }

    #[test]
    fn test_quit_code_override() {
        let compressor = CoreCompressor;
        let obj_app = ObjectId(99);
        let mut queue: Vec<PostedEvent> = Vec::new();

        queue.push(PostedEvent {
            receiver: obj_app,
            event: make_event(EventKind::Quit { exit_code: 0 }),
            priority: 0,
        });

        let new_quit = make_event(EventKind::Quit { exit_code: 1 });
        let compressed = compress_event(&mut queue, obj_app, &new_quit, &compressor);

        assert!(compressed);
        assert_eq!(queue.len(), 1);

        if let EventKind::Quit { exit_code } = queue[0].event.kind {
            assert_eq!(exit_code, 1);
        } else {
            panic!("Event kind mismatch");
        }
    }

    #[test]
    fn test_core_compressor_update_and_layout() {
        let compressor = CoreCompressor;

        let mut update1 = Event::new(EventKind::UpdateRequest);
        let update2 = Event::new(EventKind::UpdateRequest);
        assert!(compressor.try_compress(&mut update1, &update2, ObjectId(1)));

        let mut layout1 = Event::new(EventKind::LayoutRequest);
        let layout2 = Event::new(EventKind::LayoutRequest);
        assert!(compressor.try_compress(&mut layout1, &layout2, ObjectId(1)));

        assert!(!compressor.try_compress(&mut update1, &layout2, ObjectId(1)));
    }

    #[test]
    fn test_core_compressor_other_events() {
        let compressor = CoreCompressor;
        let mut user1 = Event::new(EventKind::User(Box::new(1)));
        let user2 = Event::new(EventKind::User(Box::new(2)));
        assert!(!compressor.try_compress(&mut user1, &user2, ObjectId(1)));
    }

    #[test]
    fn test_compress_event_reverse_search() {
        let compressor = CoreCompressor;
        let target1 = ObjectId(10);
        let target2 = ObjectId(20);

        let mut events = vec![
            PostedEvent::new(target1, Event::new(EventKind::UpdateRequest), 0),
            PostedEvent::new(target2, Event::new(EventKind::LayoutRequest), 0),
            PostedEvent::new(target1, Event::new(EventKind::Timer { timer_id: 1 }), 0),
        ];

        let incoming_timer = Event::new(EventKind::Timer { timer_id: 1 });
        assert!(compress_event(
            &mut events,
            target1,
            &incoming_timer,
            &compressor
        ));

        let incoming_update = Event::new(EventKind::UpdateRequest);
        assert!(!compress_event(
            &mut events,
            target2,
            &incoming_update,
            &compressor
        ));

        assert!(compress_event(
            &mut events,
            target1,
            &incoming_update,
            &compressor
        ));
    }
}
