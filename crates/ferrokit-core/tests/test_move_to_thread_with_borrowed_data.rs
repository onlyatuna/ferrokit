// SPDX-License-Identifier: LGPL-3.0-only
//! Miri reproductions, not regression tests. `move_to_thread` takes `&mut ObjectData` of the
//! object it moves and, while that reference is live, sends the object a `ThreadChange` event by
//! id, which runs the object's callback through the registry's pointer. The callback's write and
//! the live `&mut ObjectData` overlap.
//!
//! The registration function does not matter: the object below is registered in each way that is
//! sound for every other use.
//!
//! `cargo +nightly miri test -p ferrokit-core --test test_move_to_thread_with_borrowed_data` reports
//! undefined behavior for both tests, under Stacked Borrows and under Tree Borrows. On a normal
//! build they check only that the event arrives; run them with `--ignored`.

use std::ptr::NonNull;

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::object::{
    move_to_thread, register_owned_qobject, register_qobject_ptr, unregister_qobject, ObjectData,
    ObjectId, QObject, ThreadId,
};

struct Probe {
    data: ObjectData,
    thread_changes: u32,
}

impl Probe {
    fn new() -> Self {
        Self {
            data: ObjectData::new(ObjectId::next()),
            thread_changes: 0,
        }
    }
}

impl QObject for Probe {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> &mut ObjectData {
        &mut self.data
    }
    fn event(&mut self, event: &mut Event) -> bool {
        if matches!(event.kind, EventKind::ThreadChange) {
            self.thread_changes += 1;
        }
        false
    }
}

fn another_thread() -> ThreadId {
    ThreadId(
        std::thread::spawn(|| std::thread::current().id())
            .join()
            .unwrap(),
    )
}

#[test]
#[cfg_attr(
    not(miri),
    ignore = "Miri reproduction: undefined behavior is reported only under Miri"
)]
fn an_owned_object_moved_to_another_thread() {
    let target = another_thread();
    // SAFETY: the owner stays on this thread; the only callback that runs is the one
    // `move_to_thread` causes.
    let mut probe = unsafe { register_owned_qobject(Probe::new()) };
    let id = probe.data.id;

    assert_eq!(move_to_thread(&mut probe.data, target, ThreadId::current()), Ok(()));

    assert_eq!(probe.thread_changes, 1, "ThreadChange reached the object");
    // SAFETY: no callback of the object runs.
    unsafe { unregister_qobject(id) };
}

#[test]
#[cfg_attr(
    not(miri),
    ignore = "Miri reproduction: undefined behavior is reported only under Miri"
)]
fn a_variable_registered_by_a_raw_pointer_moved_to_another_thread() {
    let target = another_thread();
    let mut probe = Probe::new();
    let id = probe.data.id;
    // SAFETY: `probe` stays here, unmoved, until it is unregistered below; the pointer is taken
    // with `&raw mut`.
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut probe)) };

    assert_eq!(move_to_thread(&mut probe.data, target, ThreadId::current()), Ok(()));

    assert_eq!(probe.thread_changes, 1, "ThreadChange reached the object");
    // SAFETY: no callback of the object runs.
    unsafe { unregister_qobject(id) };
}
