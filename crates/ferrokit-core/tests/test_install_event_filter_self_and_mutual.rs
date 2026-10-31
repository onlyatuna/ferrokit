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
    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
    }
    fn event_filter(&mut self, watched: ObjectId, _event: &mut Event) -> bool {
        if watched == self.data.id() {
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
    let id = obj.data.id();
    let obj_ptr = &raw mut obj;
    // SAFETY: `obj` is a local that stays here, unmoved, until unregistered at the end of the
    // test; the pointer is taken with `&raw mut` and kept as `obj_ptr`, so no reference exists
    // that a later callback could invalidate. Single thread, no callback is active.
    unsafe { register_qobject_ptr(NonNull::new_unchecked(obj_ptr)) };

    // SAFETY: `obj_ptr` is the registered pointer and `obj` a live local; `install_event_filter`
    // only edits the filter list [inferred] and runs no callback of `obj`, so this `&mut` is the
    // only access to the object while it lives.
    let ok = install_event_filter(unsafe { &mut (*obj_ptr).data }, id);
    assert!(
        ok,
        "install_event_filter must allow installing self as filter (qobject.cpp:2407-2423)"
    );
    assert!(
        obj.data.event_filters().contains(id),
        "filter chain must contain self id"
    );

    // SAFETY: `obj` is a local, unmoved and undropped until after this call, as `unregister_qobject`
    // requires; no callback is running and no timers exist [inferred].
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
    let a_id = obj_a.data.id();
    let b_id = obj_b.data.id();
    let a_ptr = &raw mut obj_a;
    let b_ptr = &raw mut obj_b;

    // SAFETY: `obj_a` and `obj_b` are locals that stay here, unmoved, until unregistered at the
    // end of the test; the pointers are taken with `&raw mut` and kept, so no reference exists
    // that a later callback could invalidate. Single thread, no callback is active.
    unsafe {
        register_qobject_ptr(NonNull::new_unchecked(a_ptr));
        register_qobject_ptr(NonNull::new_unchecked(b_ptr));
    }

    // SAFETY: `a_ptr` is the registered pointer of `obj_a` and `b_ptr` that of `obj_b`; both
    // objects are live locals. `install_event_filter` only edits the given `ObjectData` and runs
    // no callback of either object [inferred from it being a plain filter-list edit], so each
    // `&mut` is the only access to its object while it lives.
    let ok_a = install_event_filter(unsafe { &mut (*a_ptr).data }, b_id);
    // SAFETY: as above, for `b_ptr`; the `&mut` of the previous statement has ended.
    let ok_b = install_event_filter(unsafe { &mut (*b_ptr).data }, a_id);

    assert!(ok_a, "obj_a installing obj_b must succeed");
    assert!(
        ok_b,
        "obj_b installing obj_a must succeed even when obj_a filters obj_b (qobject.cpp:2407-2423)"
    );

    // SAFETY: `obj_a` and `obj_b` are locals, unmoved and undropped until after this block, and
    // no callback is running; no timers exist [inferred].
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
    let id = obj.data.id();
    // Installed before registering: a `&mut` through the variable after that would invalidate
    // the pointer the registry holds.
    install_event_filter(&mut obj.data, id);
    let obj_ptr = &raw mut obj;
    // SAFETY: `obj` is a local that stays on this thread, unmoved and undropped, until it is
    // unregistered at the end of the test. The pointer is taken with `&raw mut`, and from here on
    // only `obj_ptr` writes the object (the earlier `install_event_filter` ran before
    // registration), so the registry's pointer stays valid across the `send_event` callbacks.
    unsafe { register_qobject_ptr(NonNull::new_unchecked(obj_ptr)) };

    let mut ev1 = Event::new(EventKind::UpdateRequest);
    let res1 = send_event(id, &mut ev1);

    assert!(res1, "event was delivered");
    assert_eq!(filter_called.load(Ordering::SeqCst), 1);
    assert_eq!(event_called.load(Ordering::SeqCst), 1);

    // 2. Consume mode: filter returns true -> event() is NOT called, send_event returns true
    // SAFETY: `obj_ptr` is the `&raw mut` pointer that was registered, so this write goes through
    // the registry's own pointer (rule 7). No callback of `obj` is running: the earlier
    // `send_event` has returned and this is the only thread.
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

    // SAFETY: `obj` is a local, unmoved and undropped until after this call, as `unregister_qobject`
    // requires; no callback is running (`send_event` has returned), and no timers exist [inferred].
    unsafe { unregister_qobject(id) };
}
