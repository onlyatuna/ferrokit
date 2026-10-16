// SPDX-License-Identifier: LGPL-3.0-only
//! install_event_filter allows self and mutual cycles (Qt parity).
//!
//! Qt reference:
//! - `corelib/kernel/qobject.cpp:2407-2423`:
//!   `QObject::installEventFilter` does not reject self (`obj == this`) or direct cycles.
//!   It only validates thread affinity, removes existing entry, and prepends to the filter list.

use std::ptr::NonNull;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::object::{
    install_event_filter, register_qobject_ptr, send_event, unregister_qobject, ObjectData,
    ObjectId, QObject,
};

struct SelfFilterWidget {
    data: ObjectData,
    filter_called: Arc<AtomicU32>,
    event_called: Arc<AtomicU32>,
    consume: bool,
}

impl SelfFilterWidget {
    fn new(filter_called: Arc<AtomicU32>, event_called: Arc<AtomicU32>, consume: bool) -> Self {
        Self {
            data: ObjectData::new(ObjectId::next()),
            filter_called,
            event_called,
            consume,
        }
    }
}

impl QObject for SelfFilterWidget {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> &mut ObjectData {
        &mut self.data
    }
    fn event_filter(&mut self, watched: ObjectId, _event: &mut Event) -> bool {
        if watched == self.data.id {
            self.filter_called.fetch_add(1, Ordering::SeqCst);
        }
        self.consume
    }
    fn event(&mut self, _event: &mut Event) -> bool {
        self.event_called.fetch_add(1, Ordering::SeqCst);
        true
    }
}

#[test]
fn install_event_filter_allows_self() {
    let mut obj = SelfFilterWidget::new(
        Arc::new(AtomicU32::new(0)),
        Arc::new(AtomicU32::new(0)),
        false,
    );
    let id = obj.data.id;
    let obj_ptr = &raw mut obj;
    unsafe { register_qobject_ptr(NonNull::new_unchecked(obj_ptr)) };

    // SAFETY: no callback of `obj` runs; `obj_ptr` is the registered pointer.
    let ok = install_event_filter(unsafe { &mut (*obj_ptr).data }, id);
    assert!(
        ok,
        "install_event_filter must allow installing self as filter (qobject.cpp:2407-2423)"
    );
    assert!(
        obj.data.event_filters.contains(id),
        "filter chain must contain self id"
    );

    unsafe { unregister_qobject(id) };
}

#[test]
fn install_event_filter_allows_mutual_cycle() {
    let mut obj_a = SelfFilterWidget::new(
        Arc::new(AtomicU32::new(0)),
        Arc::new(AtomicU32::new(0)),
        false,
    );
    let mut obj_b = SelfFilterWidget::new(
        Arc::new(AtomicU32::new(0)),
        Arc::new(AtomicU32::new(0)),
        false,
    );
    let a_id = obj_a.data.id;
    let b_id = obj_b.data.id;
    let a_ptr = &raw mut obj_a;
    let b_ptr = &raw mut obj_b;

    unsafe {
        register_qobject_ptr(NonNull::new_unchecked(a_ptr));
        register_qobject_ptr(NonNull::new_unchecked(b_ptr));
    }

    // SAFETY: no callback of either object runs; the pointers are the registered ones.
    let ok_a = install_event_filter(unsafe { &mut (*a_ptr).data }, b_id);
    let ok_b = install_event_filter(unsafe { &mut (*b_ptr).data }, a_id);

    assert!(ok_a, "obj_a installing obj_b must succeed");
    assert!(
        ok_b,
        "obj_b installing obj_a must succeed even when obj_a filters obj_b (qobject.cpp:2407-2423)"
    );

    unsafe {
        unregister_qobject(b_id);
        unregister_qobject(a_id);
    }
}

#[test]
fn self_event_filter_delivers_and_can_pass_or_consume() {
    let filter_called = Arc::new(AtomicU32::new(0));
    let event_called = Arc::new(AtomicU32::new(0));

    // 1. Pass-through mode: filter returns false -> event() is called
    let mut obj =
        SelfFilterWidget::new(Arc::clone(&filter_called), Arc::clone(&event_called), false);
    let id = obj.data.id;
    // Installed before registering: a `&mut` through the variable after that would invalidate
    // the pointer the registry holds.
    install_event_filter(&mut obj.data, id);
    let obj_ptr = &raw mut obj;
    // SAFETY: `obj` stays on this thread, unmoved and alive, until unregistered below, and only
    // `obj_ptr` is used to reach it from here on.
    unsafe { register_qobject_ptr(NonNull::new_unchecked(obj_ptr)) };

    let mut ev1 = Event::new(EventKind::UpdateRequest);
    let res1 = send_event(id, &mut ev1);

    assert!(res1, "event was delivered");
    assert_eq!(filter_called.load(Ordering::SeqCst), 1);
    assert_eq!(event_called.load(Ordering::SeqCst), 1);

    // 2. Consume mode: filter returns true -> event() is NOT called, send_event returns true
    // SAFETY: no callback is active, and `obj_ptr` is the pointer that was registered.
    unsafe { (*obj_ptr).consume = true };
    let mut ev2 = Event::new(EventKind::UpdateRequest);
    let res2 = send_event(id, &mut ev2);

    assert!(res2, "send_event returns true when consumed");
    assert_eq!(filter_called.load(Ordering::SeqCst), 2);
    assert_eq!(
        event_called.load(Ordering::SeqCst),
        1,
        "event() must not be called when consumed"
    );

    unsafe { unregister_qobject(id) };
}
