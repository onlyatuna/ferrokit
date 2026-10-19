// SPDX-License-Identifier: LGPL-3.0-only
//! Verify move_to_thread updates ObjectData.thread_id on all descendant levels (three levels).
//!
//! Qt reference:
//! - `corelib/kernel/qobject.cpp:1706-1859` `moveToThread`:
//!   Recursively updates thread data / thread affinity for root and all descendants.

use std::ptr::NonNull;

use ferrokit_core::object::{
    add_owned_child, move_to_thread_by_id, register_qobject_ptr, unregister_qobject, ObjectData,
    ObjectId, QObject, ThreadId,
};
#[derive(Debug)]
struct SimpleObject {
    data: ObjectData,
}

impl SimpleObject {
    fn new() -> Self {
        Self {
            data: ObjectData::new(ObjectId::next()),
        }
    }
}

impl QObject for SimpleObject {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
    }
}

#[test]
fn move_to_thread_three_levels_updates_all_thread_id_fields() {
    let source_thread = ThreadId::current();
    let target_thread = std::thread::spawn(ThreadId::current).join().unwrap();

    let mut root = SimpleObject::new();
    let root_id = root.data.id();
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut root)) };

    let child = Box::new(SimpleObject::new());
    let child_id = child.data.id();
    let _ = unsafe { add_owned_child(root_id, child) }
        .map_err(|(e, _)| e)
        .expect("add child");

    let grand = Box::new(SimpleObject::new());
    let grand_id = grand.data.id();
    let _ = unsafe { add_owned_child(child_id, grand) }
        .map_err(|(e, _)| e)
        .expect("add grand");

    let great_grand = Box::new(SimpleObject::new());
    let great_grand_id = great_grand.data.id();
    let _ = unsafe { add_owned_child(grand_id, great_grand) }
        .map_err(|(e, _)| e)
        .expect("add great grand");

    // Before move, all thread_id fields are source_thread
    assert_eq!(root.data.thread_id(), source_thread);

    // Perform move_to_thread
    let res = move_to_thread_by_id(root_id, target_thread, source_thread);
    assert_eq!(res, Ok(()));

    // Verify root ObjectData.thread_id
    assert_eq!(root.data.thread_id(), target_thread);

    // Verify child, grandchild, and great-grandchild ObjectData.thread_id via owned hierarchy
    root.data.owned_children()[0]
        .with(|child_ref| {
                assert_eq!(child_ref.object_data().id(), child_id);
                assert_eq!(
                    child_ref.object_data().thread_id(),
                    target_thread,
                    "child ObjectData.thread_id must be updated"
                );

                child_ref.object_data().owned_children()[0]
                    .with(|grand_ref| {
                        assert_eq!(grand_ref.object_data().id(), grand_id);
                        assert_eq!(
                            grand_ref.object_data().thread_id(),
                            target_thread,
                            "grandchild ObjectData.thread_id must be updated to target_thread"
                        );

                        grand_ref.object_data().owned_children()[0]
                            .with(|great_grand_ref| {
                                assert_eq!(great_grand_ref.object_data().id(), great_grand_id);
                                assert_eq!(
                                    great_grand_ref.object_data().thread_id(),
                                    target_thread,
                                    "great-grandchild ObjectData.thread_id must be updated to target_thread"
                                );
                            })
                            .unwrap();
                    })
                    .unwrap();
            })
            .unwrap();

    // Clean up
    unsafe {
        unregister_qobject(great_grand_id);
        unregister_qobject(grand_id);
        unregister_qobject(child_id);
        unregister_qobject(root_id);
    }
}

#[test]
fn move_to_thread_three_levels_unowned_children_updates_all_thread_id_fields() {
    use ferrokit_core::object::set_parent;

    let source_thread = ThreadId::current();
    let target_thread = std::thread::spawn(ThreadId::current).join().unwrap();

    let mut root = SimpleObject::new();
    let root_id = root.data.id();
    let mut child = SimpleObject::new();
    let child_id = child.data.id();
    let mut grand = SimpleObject::new();
    let grand_id = grand.data.id();

    // The pointers are kept: after registering, a `&mut` taken through a variable would
    // invalidate the registry's pointer (rule 7 in the `object` module documentation).
    let child_ptr = &raw mut child;
    let grand_ptr = &raw mut grand;
    unsafe {
        register_qobject_ptr(NonNull::new_unchecked(&raw mut root));
        register_qobject_ptr(NonNull::new_unchecked(child_ptr));
        register_qobject_ptr(NonNull::new_unchecked(grand_ptr));
    }

    // SAFETY: no callback of either object runs; the pointers are the registered ones.
    set_parent(unsafe { &mut (*child_ptr).data }, Some(root_id)).expect("set child parent");
    set_parent(unsafe { &mut (*grand_ptr).data }, Some(child_id)).expect("set grand parent");

    // Perform move_to_thread
    let res = move_to_thread_by_id(root_id, target_thread, source_thread);
    assert_eq!(res, Ok(()));

    // Verify thread_id fields
    assert_eq!(root.data.thread_id(), target_thread);
    assert_eq!(child.data.thread_id(), target_thread);
    assert_eq!(grand.data.thread_id(), target_thread);

    unsafe {
        unregister_qobject(grand_id);
        unregister_qobject(child_id);
        unregister_qobject(root_id);
    }
}
