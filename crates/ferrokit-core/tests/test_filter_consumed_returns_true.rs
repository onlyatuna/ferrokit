// SPDX-License-Identifier: LGPL-3.0-only
//! When an event filter filters/consumes an event, send_event returns true.
//!
//! Qt reference:
//! - `corelib/kernel/qcoreapplication.cpp:1285-1295`:
//!   If an application or object event filter consumes an event, `filtered = true`
//!   and `notify_helper` returns `filtered` (`true`).

use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::event_loop::{
    clear_application_event_filters, install_application_event_filter,
};
use ferrokit_core::object::{
    register_qobject_ptr, send_event, unregister_qobject, ObjectData, ObjectId, QObject,
};
struct ConsumingFilter {
    data: ObjectData,
}

impl ConsumingFilter {
    fn new() -> Self {
        Self {
            data: ObjectData::new(ObjectId::next()),
        }
    }
}

impl QObject for ConsumingFilter {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
    }
    fn event_filter(&mut self, _watched: ObjectId, _event: &mut Event) -> bool {
        true // consume the event
    }
}

struct Receiver {
    data: ObjectData,
    reached: Arc<AtomicBool>,
}

impl Receiver {
    fn new(reached: Arc<AtomicBool>) -> Self {
        Self {
            data: ObjectData::new(ObjectId::next()),
            reached,
        }
    }
}

impl QObject for Receiver {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
    }
    fn event(&mut self, _event: &mut Event) -> bool {
        self.reached.store(true, Ordering::SeqCst);
        false
    }
}

#[test]
fn application_filter_consumed_event_returns_true() {
    clear_application_event_filters();
    let reached = Arc::new(AtomicBool::new(false));
    let mut filter = ConsumingFilter::new();
    let mut receiver = Receiver::new(Arc::clone(&reached));

    unsafe {
        register_qobject_ptr(NonNull::new_unchecked(&raw mut filter));
        register_qobject_ptr(NonNull::new_unchecked(&raw mut receiver));
    }
    install_application_event_filter(filter.data.id());

    let mut event = Event::new(EventKind::UpdateRequest);
    let handled = send_event(receiver.data.id(), &mut event);

    clear_application_event_filters();
    unsafe {
        unregister_qobject(filter.data.id());
        unregister_qobject(receiver.data.id());
    }

    assert!(
        !reached.load(Ordering::SeqCst),
        "receiver must not be reached"
    );
    assert!(
        handled,
        "send_event must return true when consumed by application filter (qcoreapplication.cpp:1288-1289)"
    );
}

#[test]
fn object_filter_consumed_event_returns_true() {
    clear_application_event_filters();
    let reached = Arc::new(AtomicBool::new(false));
    let mut filter = ConsumingFilter::new();
    let mut receiver = Receiver::new(Arc::clone(&reached));

    // Installed before registering: a `&mut` through the variable after that would invalidate
    // the registry's pointer.
    receiver
        .object_data_mut()
        .install_event_filter(filter.data.id());
    unsafe {
        register_qobject_ptr(NonNull::new_unchecked(&raw mut filter));
        register_qobject_ptr(NonNull::new_unchecked(&raw mut receiver));
    }

    let mut event = Event::new(EventKind::UpdateRequest);
    let handled = send_event(receiver.data.id(), &mut event);

    unsafe {
        unregister_qobject(filter.data.id());
        unregister_qobject(receiver.data.id());
    }

    assert!(
        !reached.load(Ordering::SeqCst),
        "receiver must not be reached"
    );
    assert!(
        handled,
        "send_event must return true when consumed by object filter (qcoreapplication.cpp:1293-1294)"
    );
}
