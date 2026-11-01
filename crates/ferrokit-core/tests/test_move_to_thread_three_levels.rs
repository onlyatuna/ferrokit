// SPDX-License-Identifier: LGPL-3.0-only
//! Verify move_to_thread updates ObjectData.thread_id on all descendant levels (three levels).
//!
//! Qt reference:
//! - `corelib/kernel/qobject.cpp:1706-1859` `moveToThread`:
//!   Recursively updates thread data / thread affinity for root and all descendants.

use std::ptr::NonNull;

use ferrokit_core::object::{
    ObjectData, ObjectId, OwnedObject, QObject, ThreadId, add_owned_child, move_to_thread_by_id,
    register_qobject_ptr, unregister_qobject, with_object,
};

/// An `ObjectData` that lets `move_to_thread_by_id` move its object.
fn movable_data(id: ObjectId) -> ObjectData {
    // SAFETY: `SimpleObject` holds nothing but its `ObjectData`, which has no `Rc`, thread-local
    // or shared `!Send` state [inferred from the struct definition below], so using and dropping
    // it on another thread after `move_to_thread_by_id` is sound.
    unsafe { ObjectData::new(id).with_thread_moves_allowed() }
}
#[derive(Debug)]
struct SimpleObject {
    data: ObjectData,
}

impl SimpleObject {
    fn new() -> Self {
        Self {
            data: movable_data(ObjectId::next()),
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

    let mut root = SimpleObject::new();
    let root_id = root.data.id();
    // SAFETY: `root` is a local that stays here, unmoved, until it is unregistered at the end of
    // the test; the pointer is taken with `&raw mut`, so no reference is made, and `root` is only
    // read afterwards (rule 7 allows reads). Everything runs on this thread, the registration thread.
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut root)) };

    let child = Box::new(SimpleObject::new());
    let child_id = child.data.id();
    // SAFETY: `child` is a fresh `Box` whose address is not used before this call; ownership goes
    // to `root`'s `owned_children`, which keeps it at that address. `root` is registered on this
    // thread (line above), so child and parent share the registration thread, and no callback
    // is running.
    let _ = unsafe { add_owned_child(root_id, child) }
        .map_err(|(e, _)| e)
        .expect("add child");

    let grand = Box::new(SimpleObject::new());
    let grand_id = grand.data.id();
    // SAFETY: as for `child` above: `grand` is a fresh `Box` handed over without a prior
    // reference to it, and the parent `child_id` was registered on this thread by the call above.
    let _ = unsafe { add_owned_child(child_id, grand) }
        .map_err(|(e, _)| e)
        .expect("add grand");

    let great_grand = Box::new(SimpleObject::new());
    let great_grand_id = great_grand.data.id();
    // SAFETY: as for `child` above: `great_grand` is a fresh `Box` handed over without a prior
    // reference to it, and the parent `grand_id` was registered on this thread by the call above.
    let _ = unsafe { add_owned_child(grand_id, great_grand) }
        .map_err(|(e, _)| e)
        .expect("add great grand");

    // Before move, all thread_id fields are source_thread
    assert_eq!(root.data.thread_id(), source_thread);

    let (tid_tx, tid_rx) = std::sync::mpsc::channel();
    let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
    let worker = std::thread::spawn(move || {
        tid_tx.send(ThreadId::current()).unwrap();
        go_rx.recv().unwrap();
        let chain = [child_id, grand_id, great_grand_id];
        let names = ["child", "grandchild", "great-grandchild"];
        for (i, (&id, name)) in chain.iter().zip(names).enumerate() {
            let (seen_id, thread, first_child) = with_object(id, |o| {
                (
                    o.object_data().id(),
                    o.object_data().thread_id(),
                    o.object_data()
                        .owned_children()
                        .first()
                        .map(OwnedObject::object_id),
                )
            })
            .unwrap_or_else(|| panic!("{name} must be reachable on its thread"));
            assert_eq!(seen_id, id);
            assert_eq!(
                thread,
                ThreadId::current(),
                "{name} ObjectData.thread_id must be updated"
            );
            assert_eq!(first_child, chain.get(i + 1).copied());
        }
    });
    let target_thread = tid_rx.recv().unwrap();
    let res = move_to_thread_by_id(root_id, target_thread, source_thread);
    assert_eq!(res, Ok(()));

    // Verify root ObjectData.thread_id
    assert_eq!(root.data.thread_id(), target_thread);

    // Verify child, grandchild, and great-grandchild ObjectData.thread_id on the thread the
    // subtree now lives on (an owner refuses access from any other thread).
    go_tx.send(()).unwrap();
    worker.join().expect("worker assertions must hold");

    // Clean up
    // SAFETY: `root` is a local of this test, unmoved and undropped until after this block, and
    // its children are heap boxes owned by `root.owned_children`, so their memory stays allocated
    // too; that is `unregister_qobject`'s requirement. No callback is running: the worker thread
    // has been joined and this thread is not inside an event. Nothing here has timers
    // [inferred], so calling from a thread other than the target thread is harmless.
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
    // SAFETY: `root`, `child` and `grand` are locals that outlive every use of the registry in
    // this test (they are unregistered at the end, before they drop) and are not moved. Each
    // pointer comes from `&raw mut`, so no reference exists that a later callback could
    // invalidate; `child` and `grand` keep their pointers (above) for later writes. Everything
    // runs on this thread, the registration thread, and no callback is active.
    unsafe {
        register_qobject_ptr(NonNull::new_unchecked(&raw mut root));
        register_qobject_ptr(NonNull::new_unchecked(child_ptr));
        register_qobject_ptr(NonNull::new_unchecked(grand_ptr));
    }

    // SAFETY: `child_ptr` is the pointer registered for `child` above (`child` is a local that is
    // not moved or dropped here), so the `&mut` to its `data` is derived from it. The registry
    // delivers `ChildAdded` only to `root`, a different object, which never touches `child`
    // [inferred from set_parent's documented event delivery], so nothing else uses the object
    // while this borrow lives.
    set_parent(unsafe { &mut (*child_ptr).data }, Some(root_id)).expect("set child parent");
    // SAFETY: same as above for `grand_ptr`/`grand`; the new parent `child` is a different object.
    set_parent(unsafe { &mut (*grand_ptr).data }, Some(child_id)).expect("set grand parent");

    // Perform move_to_thread
    let res = move_to_thread_by_id(root_id, target_thread, source_thread);
    assert_eq!(res, Ok(()));

    // Verify thread_id fields
    assert_eq!(root.data.thread_id(), target_thread);
    assert_eq!(child.data.thread_id(), target_thread);
    assert_eq!(grand.data.thread_id(), target_thread);

    // SAFETY: `root`, `child` and `grand` are locals of this test, unmoved and undropped until
    // after this block, which is the `unregister_qobject` requirement on their memory. No
    // callback is running (the test is single-threaded and no event is being delivered), and none
    // of the three has timers, so unregistering from the source thread after the move to
    // `target_thread` has no timer registry to miss [inferred: nothing here creates a timer].
    unsafe {
        unregister_qobject(grand_id);
        unregister_qobject(child_id);
        unregister_qobject(root_id);
    }
}
