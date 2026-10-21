// SPDX-License-Identifier: LGPL-3.0-only
//! A subscriber's captured state may use the signal when it is dropped. Removing a subscriber must
//! not drop it while the signal's lock is held, or that use would wait for a lock its own thread
//! holds.
//!
//! Each case runs on its own thread; one that makes no progress is reported as a failure instead
//! of hanging the test run.

use std::sync::mpsc;
use std::time::Duration;

use ferrokit_core::object::{
    register_owned_qobject, unregister_qobject, ObjectData, ObjectDataMut, ObjectId, OwnedObject,
    QObject,
};
use ferrokit_core::Signal;

struct Node {
    data: ObjectData,
}

impl QObject for Node {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ObjectDataMut<'_> {
        ObjectDataMut::new(&mut self.data)
    }
}

/// Dropping it takes the signal's lock (`subscriber_count`).
struct UsesSignalOnDrop {
    signal: Signal<()>,
}

impl Drop for UsesSignalOnDrop {
    fn drop(&mut self) {
        let _ = self.signal.subscriber_count();
    }
}

fn finishes(case: impl FnOnce() + Send + 'static) -> bool {
    let (done, finished) = mpsc::channel();
    std::thread::spawn(move || {
        case();
        let _ = done.send(());
    });
    finished.recv_timeout(Duration::from_secs(10)).is_ok()
}

fn node() -> OwnedObject<Node> {
    // SAFETY: the owner stays on this thread and no callback of the node runs.
    unsafe {
        register_owned_qobject(Node {
            data: ObjectData::new(ObjectId::next()),
        })
    }
}

#[test]
fn disconnect_drops_the_subscriber_outside_the_lock() {
    assert!(finishes(|| {
        let signal: Signal<()> = Signal::new();
        let guard = UsesSignalOnDrop { signal: signal.clone() };
        let id = signal.connect(move |_| {
            let _ = &guard;
        });
        assert!(signal.disconnect(id));
    }));
}

#[test]
fn disconnect_all_drops_the_subscribers_outside_the_lock() {
    assert!(finishes(|| {
        let signal: Signal<()> = Signal::new();
        let guard = UsesSignalOnDrop { signal: signal.clone() };
        signal.connect(move |_| {
            let _ = &guard;
        });
        signal.disconnect_all();
        assert_eq!(signal.subscriber_count(), 0);
    }));
}

#[test]
fn unregistering_a_receiver_drops_its_subscribers_outside_the_lock() {
    assert!(finishes(|| {
        let receiver = node();
        let receiver_id = OwnedObject::object_id(&receiver);
        let signal: Signal<()> = Signal::new();
        let guard = UsesSignalOnDrop { signal: signal.clone() };
        receiver
            .with(|r| {
                signal.connect_direct_to(r, move |_| {
                    let _ = &guard;
                })
            })
            .unwrap();
        // SAFETY: no callback of the receiver runs.
        unsafe { unregister_qobject(receiver_id) };
        assert_eq!(signal.subscriber_count(), 0);
    }));
}

#[test]
fn unregistering_from_a_later_slot_of_the_emission_does_not_hang() {
    assert!(finishes(|| {
        let receiver = node();
        let receiver_id = OwnedObject::object_id(&receiver);
        let signal: Signal<()> = Signal::new();
        let guard = UsesSignalOnDrop { signal: signal.clone() };
        receiver
            .with(|r| {
                signal.connect_direct_to(r, move |_| {
                    let _ = &guard;
                })
            })
            .unwrap();
        // The first subscriber has run and is no longer held by the emission when this slot
        // removes it.
        // SAFETY: no callback of the receiver runs.
        signal.connect(move |_| unsafe { unregister_qobject(receiver_id) });
        signal.emit(&());
        assert_eq!(signal.subscriber_count(), 1);
    }));
}
