// SPDX-License-Identifier: LGPL-3.0-only
// Panic containment at Qt-style delivery boundaries.
//
// Python/PySide6 reference: a slot that raises does not stop the emitter or the loop; the
// exception goes to `sys.excepthook` and the remaining slots and queued events still run.
// Qt C++ `doActivate` / `QCoreApplication::sendPostedEvents` do not contain exceptions, so
// this containment is a deliberate, Python-faithful Rust decision.

use ferrokit_core::event_loop::EventLoop;
use ferrokit_core::object::ThreadId;
use ferrokit_core::signal::{QueuedSignal, Signal};
use parking_lot::Mutex;
use std::sync::Arc;

#[test]
fn signal_slot_panic_does_not_stop_later_slots() {
    let signal: Signal<i32> = Signal::new();
    let seen = Arc::new(Mutex::new(Vec::new()));

    signal.connect(|value| {
        if *value == 1 {
            panic!("slot 1 failure");
        }
    });
    let seen_clone = Arc::clone(&seen);
    signal.connect(move |value| {
        seen_clone.lock().push(*value);
    });

    signal.emit(&1);
    signal.emit(&2);

    assert_eq!(
        *seen.lock(),
        vec![1, 2],
        "later slot must still run for every emit, including after a panicking slot"
    );
}

#[test]
fn posted_event_panic_does_not_stop_later_events() {
    let signal: QueuedSignal<i32> = Signal::new();
    let receiver_thread = ThreadId::current();
    let seen = Arc::new(Mutex::new(Vec::new()));

    signal.connect_queued(receiver_thread, |value| {
        if *value == 1 {
            panic!("queued slot failure");
        }
    });
    let seen_clone = Arc::clone(&seen);
    signal.connect_queued(receiver_thread, move |value| {
        seen_clone.lock().push(*value);
    });

    let event_loop = EventLoop::new();
    signal.emit(&1);
    signal.emit(&2);
    event_loop.process_events(false);
    event_loop.process_events(false);

    assert_eq!(
        *seen.lock(),
        vec![1, 2],
        "later posted events must still be delivered after a panicking event"
    );
}
