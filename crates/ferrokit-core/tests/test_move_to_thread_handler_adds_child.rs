// SPDX-License-Identifier: LGPL-3.0-only
//! A child attached by a `ThreadChange` handler during `moveToThread` moves with its new parent.
//!
//! Qt `QObjectPrivate::setThreadData_helper` (qobject.cpp:1706-1859) sends `ThreadChange` to the
//! object and then walks its children as they are at that point, so a child added by a handler is
//! moved too. A child left on the old thread under a moved parent would break the rule that a
//! parent and its children share one thread.

use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::ptr::NonNull;

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::object::{
    move_to_thread_by_id, query_object_thread, register_qobject_ptr, set_parent_by_id,
    unregister_qobject, MoveByIdError, ObjectData, ObjectDataMut, ObjectId, QObject, ThreadId,
};

/// An `ObjectData` that lets `move_to_thread_by_id` move its object.
fn movable_data(id: ObjectId) -> ObjectData {
    // SAFETY: `Root` and `Leaf` hold plain fields, a `Cell<Option<ObjectId>>` and an `Arc`, which are `Send`.
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
        if matches!(event.kind, EventKind::ThreadChange) {
            if let Some(late) = self.late.take() {
                set_parent_by_id(late, Some(self.data.id())).expect("attach in the handler");
            }
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
    // SAFETY: both objects stay at these addresses until unregistered below; nothing touches them
    // directly while a callback runs.
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

    // SAFETY: no callback runs; the root was made by `Box::into_raw` above.
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
    // SAFETY: both objects stay at these addresses until unregistered below; nothing touches them
    // directly while a callback runs.
    unsafe {
        register_qobject_ptr(NonNull::new_unchecked(root_ptr));
        register_qobject_ptr(NonNull::new_unchecked(late_ptr));
    }

    assert_eq!(
        move_to_thread_by_id(root_id, target, source),
        Err(MoveByIdError::NotMovable)
    );
    assert_eq!(query_object_thread(root_id), Some(source), "the parent moved alone");
    assert_eq!(query_object_thread(late_id), Some(source));

    // SAFETY: no callback runs; the root was made by `Box::into_raw` above.
    unsafe {
        unregister_qobject(late_id);
        unregister_qobject(root_id);
        drop(Box::from_raw(root_ptr));
    }
}
