// SPDX-License-Identifier: LGPL-3.0-only
//! Regression tests for [`move_to_thread_by_id`].
//!
//! Taking `&mut ObjectData` of the moved object across `ThreadChange` delivery and recursive
//! `owned_children` updates overlapped the registry's callback borrow under Stacked Borrows and
//! Tree Borrows. [`move_to_thread_by_id`] takes [`ObjectId`] instead and separates validation,
//! `ThreadChange` delivery, and per-node `thread_id` updates into distinct short-lived borrows.

use std::cell::Cell;
use std::ptr::NonNull;
use std::rc::Rc;

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::object::{
    dispatch_to_object, move_to_thread_by_id, register_owned_qobject, register_qobject_ptr,
    unregister_qobject, MoveByIdError, ObjectData, ObjectId, QObject, ThreadId,
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
    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
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
fn an_owned_object_moved_to_another_thread() {
    let target = another_thread();
    // SAFETY: the owner stays on this thread; the only callback that runs is the one
    // `move_to_thread_by_id` causes.
    let probe = unsafe { register_owned_qobject(Probe::new()) };
    let id = probe.with(|p| p.data.id()).unwrap();

    assert_eq!(
        move_to_thread_by_id(id, target, ThreadId::current()),
        Ok(())
    );

    assert_eq!(
        probe.with(|p| p.thread_changes).unwrap(),
        1,
        "ThreadChange reached the object"
    );
    assert_eq!(probe.with(|p| p.data.thread_id()).unwrap(), target);
    // SAFETY: no callback of the object runs.
    unsafe { unregister_qobject(id) };
}

#[test]
fn a_variable_registered_by_a_raw_pointer_moved_to_another_thread() {
    let target = another_thread();
    let mut probe = Probe::new();
    let id = probe.data.id();
    // SAFETY: `probe` stays here, unmoved, until it is unregistered below; the pointer is taken
    // with `&raw mut`.
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut probe)) };

    assert_eq!(
        move_to_thread_by_id(id, target, ThreadId::current()),
        Ok(())
    );

    assert_eq!(probe.thread_changes, 1, "ThreadChange reached the object");
    assert_eq!(probe.data.thread_id(), target);
    // SAFETY: no callback of the object runs.
    unsafe { unregister_qobject(id) };
}

struct BusyProbe {
    data: ObjectData,
    target: ThreadId,
    result: Rc<Cell<Option<Result<(), MoveByIdError>>>>,
}

impl QObject for BusyProbe {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
    }
    fn event(&mut self, event: &mut Event) -> bool {
        if matches!(event.kind, EventKind::UpdateRequest) {
            let res = move_to_thread_by_id(self.data.id(), self.target, ThreadId::current());
            self.result.set(Some(res));
        }
        true
    }
}

#[test]
fn move_to_thread_by_id_rejects_unknown_and_busy_objects() {
    let target = another_thread();
    assert_eq!(
        move_to_thread_by_id(ObjectId::next(), target, ThreadId::current()),
        Err(MoveByIdError::UnknownObject)
    );

    let result = Rc::new(Cell::new(None));
    // SAFETY: the owner stays on this thread and is dropped after callbacks finish.
    let probe = unsafe {
        register_owned_qobject(BusyProbe {
            data: ObjectData::new(ObjectId::next()),
            target,
            result: Rc::clone(&result),
        })
    };
    let id = probe.with(|p| p.data.id()).unwrap();

    dispatch_to_object(id, &mut Event::new(EventKind::UpdateRequest));
    assert_eq!(
        result.get(),
        Some(Err(MoveByIdError::Busy)),
        "moving an object whose callback currently holds its borrow must return Busy"
    );
    // SAFETY: no callback runs.
    unsafe { unregister_qobject(id) };
}
