// SPDX-License-Identifier: LGPL-3.0-only
//! `QObject::killTimer` kills only a timer the object started (Qt `QObject::killTimer`,
//! qobject.cpp: "Timers cannot be stopped from another thread" and an id that is not the
//! object's is refused with a warning).

use ferrokit_core::event_loop::EventLoop;
use ferrokit_core::object::{
    ObjectData, ObjectDataMut, ObjectId, OwnedObject, QObject, register_owned_qobject,
};
use ferrokit_core::timer::TimerType;

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

fn node() -> OwnedObject<Node> {
    // SAFETY: the returned owner is a test local on this thread, used only through `with_mut`
    // (which refuses during a callback) and dropped by the test. No event loop runs, so no
    // callback of the node is ever invoked.
    unsafe {
        register_owned_qobject(Node {
            data: ObjectData::new(ObjectId::next()),
        })
    }
}

#[test]
fn kill_timer_refuses_a_timer_of_another_object() {
    let _el = EventLoop::new();
    let mut owner = node();
    let mut other = node();
    let id = owner
        .with_mut(|o| o.start_timer(1000, TimerType::Coarse))
        .unwrap();
    assert!(id.is_valid());

    assert_eq!(
        other.with_mut(|o| o.kill_timer(id)),
        Some(false),
        "not its timer"
    );
    assert_eq!(
        owner.with_mut(|o| o.kill_timer(id)),
        Some(true),
        "the timer was still there"
    );
    assert_eq!(
        owner.with_mut(|o| o.kill_timer(id)),
        Some(false),
        "and is gone now"
    );
}
