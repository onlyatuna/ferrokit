// SPDX-License-Identifier: LGPL-3.0-only
//! A child attached by a `ThreadChange` handler during `moveToThread` moves with its new parent.
//!
//! Qt `QObjectPrivate::setThreadData_helper` (qobject.cpp:1706-1859) sends `ThreadChange` to the
//! object and then walks its children as they are at that point, so a child added by a handler is
//! moved too. A child left on the old thread under a moved parent would break the rule that a
//! parent and its children share one thread.

use std::cell::Cell;
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::object::{
    MoveByIdError, ObjectData, ObjectDataMut, ObjectId, QObject, ThreadId, move_to_thread_by_id,
    query_object_thread, register_qobject_ptr, set_parent_by_id, unregister_qobject,
};

/// An `ObjectData` that lets `move_to_thread_by_id` move its object.
fn movable_data(id: ObjectId) -> ObjectData {
    // SAFETY: `Root` holds an `ObjectData` and a `Cell<Option<ObjectId>>`, and `Leaf` an
    // `ObjectData` and an `Arc<AtomicUsize>`: all `Send`, and the `Cell` is not shared with any
    // handle left behind (it is read only by `Root::event`). No `Rc`, no thread-local state.
    unsafe { ObjectData::new(id).with_thread_moves_allowed() }
}

struct Root {
    data: ObjectData,
    late: Cell<Option<ObjectId>>,
}

impl QObject for Root {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ObjectDataMut<'_> {
        ObjectDataMut::new(&mut self.data)
    }
    fn event(&mut self, event: &mut Event) -> bool {
        if matches!(event.kind, EventKind::ThreadChange)
            && let Some(late) = self.late.take()
        {
            set_parent_by_id(late, Some(self.data.id())).expect("attach in the handler");
        }
        false
    }
}

struct Leaf {
    data: ObjectData,
    thread_changes: Arc<AtomicUsize>,
}

impl QObject for Leaf {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ObjectDataMut<'_> {
        ObjectDataMut::new(&mut self.data)
    }
    fn event(&mut self, event: &mut Event) -> bool {
        if matches!(event.kind, EventKind::ThreadChange) {
            self.thread_changes.fetch_add(1, Ordering::SeqCst);
        }
        false
    }
}

#[test]
fn a_child_attached_by_a_thread_change_handler_moves_with_its_parent() {
    let source = ThreadId::current();
    let target = std::thread::spawn(ThreadId::current).join().unwrap();

    let thread_changes = Arc::new(AtomicUsize::new(0));
    let mut late = Leaf {
        data: movable_data(ObjectId::next()),
        thread_changes: Arc::clone(&thread_changes),
    };
    let late_id = late.data.id();
    let root = Root {
        data: movable_data(ObjectId::next()),
        late: Cell::new(Some(late_id)),
    };
    let root_id = root.data.id();
    let root_ptr = Box::into_raw(Box::new(root));
    let late_ptr = &raw mut late;
    // SAFETY: `late` is a local, not moved or dropped before the unregistration below, and its
    // pointer comes from `&raw mut`; `root_ptr` is a `Box::into_raw` allocation freed only below.
    // Neither is touched directly afterwards (`late.data.id()` was read before registering). Their
    // callbacks (the `ThreadChange` events) run on this thread inside `move_to_thread_by_id`.
    unsafe {
        register_qobject_ptr(NonNull::new_unchecked(root_ptr));
        register_qobject_ptr(NonNull::new_unchecked(late_ptr));
    }

    assert_eq!(move_to_thread_by_id(root_id, target, source), Ok(()));

    assert_eq!(query_object_thread(root_id), Some(target));
    assert_eq!(
        query_object_thread(late_id),
        Some(target),
        "the child attached during the move follows its parent"
    );

    assert_eq!(
        thread_changes.load(Ordering::SeqCst),
        1,
        "the attached child gets its own ThreadChange (Qt setThreadData_helper)"
    );

    // SAFETY: the move finished and the target thread has already exited, so no callback of either
    // object can run. `late` is a local still in place; `root_ptr` came from `Box::into_raw` above
    // and is freed exactly once here, after its unregistration. Using the objects from this thread
    // after the move is outside the "registration thread" rule only in that the target thread is
    // gone (the join in the test orders its end before this); no timers exist for unregistration to stop.
    unsafe {
        unregister_qobject(late_id);
        unregister_qobject(root_id);
        drop(Box::from_raw(root_ptr));
    }
}

/// A child that must stay on its thread.
struct Pinned {
    data: ObjectData,
}

impl QObject for Pinned {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ObjectDataMut<'_> {
        ObjectDataMut::new(&mut self.data)
    }
}

/// A handler attaches a child that cannot move. Moving the parent anyway would leave that child
/// on the source thread under a parent on the target thread, so the move fails and changes nothing.
#[test]
fn an_unmovable_child_attached_by_a_handler_stops_the_move() {
    let source = ThreadId::current();
    let target = std::thread::spawn(ThreadId::current).join().unwrap();

    let mut late = Pinned {
        data: ObjectData::new(ObjectId::next()),
    };
    let late_id = late.data.id();
    let root = Root {
        data: movable_data(ObjectId::next()),
        late: Cell::new(Some(late_id)),
    };
    let root_id = root.data.id();
    let root_ptr = Box::into_raw(Box::new(root));
    let late_ptr = &raw mut late;
    // SAFETY: `late` is a local, not moved or dropped before the unregistration below, and its
    // pointer comes from `&raw mut`; `root_ptr` is a `Box::into_raw` allocation freed only below.
    // Neither is touched directly afterwards (`late.data.id()` was read before registering). Their
    // callbacks (the `ThreadChange` events) run on this thread inside `move_to_thread_by_id`.
    unsafe {
        register_qobject_ptr(NonNull::new_unchecked(root_ptr));
        register_qobject_ptr(NonNull::new_unchecked(late_ptr));
    }

    assert_eq!(
        move_to_thread_by_id(root_id, target, source),
        Err(MoveByIdError::NotMovable)
    );
    assert_eq!(
        query_object_thread(root_id),
        Some(source),
        "the parent moved alone"
    );
    assert_eq!(query_object_thread(late_id), Some(source));

    // SAFETY: no callback runs: `move_to_thread_by_id` failed before changing anything and both
    // objects are still registered to this (the source) thread. `late` is a local still in
    // place; `root_ptr` came from `Box::into_raw` above, is freed exactly once here, after its
    // unregistration, and nothing else holds it.
    unsafe {
        unregister_qobject(late_id);
        unregister_qobject(root_id);
        drop(Box::from_raw(root_ptr));
    }
}
