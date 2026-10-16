// SPDX-License-Identifier: LGPL-3.0-only
//! Tests verifying ObjectData encapsulation and memory safety invariants.
//!
//! ObjectData fields (`id`, `parent`, `children`, `thread_id`, `owned_children`) are private,
//! preventing Safe Rust from modifying an object's identity, thread affinity, or hierarchy
//! without invariant checks. Even if ObjectData is swapped, OwnedObject cleans up the registered
//! ID on drop, preventing dangling pointers and use-after-free in the registry.

use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::object::{
    add_owned_child, dispatch_to_object, register_owned_qobject, with_object,
    with_object_mut, ObjectData, ObjectId, OwnedObject, QObject,
};

type Hook = Box<dyn FnOnce(&mut EncapsulatedNode)>;

struct EncapsulatedNode {
    data: ObjectData,
    freed: Arc<AtomicBool>,
    on_user: Option<Hook>,
}

impl Drop for EncapsulatedNode {
    fn drop(&mut self) {
        self.freed.store(true, Ordering::Release);
    }
}

impl QObject for EncapsulatedNode {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> &mut ObjectData {
        &mut self.data
    }
    fn as_qobject_any(&self) -> Option<&dyn Any> {
        Some(self)
    }
    fn event(&mut self, event: &mut Event) -> bool {
        if matches!(event.kind, EventKind::User(_)) {
            if let Some(hook) = self.on_user.take() {
                hook(self);
            }
            return true;
        }
        false
    }
}

fn make_node(freed: Arc<AtomicBool>) -> EncapsulatedNode {
    EncapsulatedNode {
        data: ObjectData::new(ObjectId::next()),
        freed,
        on_user: None,
    }
}

#[test]
fn test_swapping_object_data_does_not_leave_dangling_registry_pointer_or_uaf() {
    let freed = Arc::new(AtomicBool::new(false));
    let mut a = unsafe { register_owned_qobject(make_node(Arc::clone(&freed))) };
    let a_id = OwnedObject::object_id(&a);

    // Swap ObjectData with a fresh, un-registered one
    let mut stolen = ObjectData::new(ObjectId::next());
    std::mem::swap(a.object_data_mut(), &mut stolen);

    // Dropping `a` deallocates its heap memory.
    // OwnedObject::drop ensures `a_id` is unregistered and marked dead,
    // so no dangling pointer remains in GLOBAL_OBJECT_REGISTRY.
    drop(a);
    assert!(freed.load(Ordering::Acquire), "node was dropped");

    // Must return None because a_id was unregistered by OwnedObject::drop
    assert!(
        with_object_mut(a_id, |_| ()).is_none(),
        "registry must not hand out a reference to a deallocated object"
    );

    drop(stolen);
}

#[test]
fn test_encapsulated_getters_return_consistent_views() {
    let freed = Arc::new(AtomicBool::new(false));
    let parent = unsafe { register_owned_qobject(make_node(Arc::clone(&freed))) };
    let parent_id = parent.data.id();

    let child_freed = Arc::new(AtomicBool::new(false));
    let child = Box::new(make_node(Arc::clone(&child_freed)));
    let child_id = child.data.id();

    unsafe { add_owned_child(parent_id, child) }
        .map_err(|(e, _)| e)
        .expect("attach child");

    assert_eq!(parent.data.id(), parent_id);
    assert_eq!(parent.data.children(), &[child_id]);
    assert_eq!(parent.data.owned_children().len(), 1);
    assert_eq!(parent.data.owned_children()[0].object_data().id(), child_id);

    // Safe Rust only gets a shared slice of owned children:
    // parent.data.owned_children() is &[OwnedObject<dyn QObject>],
    // so safe code CANNOT call .clear() or .remove() on it directly!

    drop(parent);
    assert!(freed.load(Ordering::Acquire));
    assert!(child_freed.load(Ordering::Acquire));
}

#[test]
fn test_running_child_cannot_be_cleared_by_safe_code() {
    let parent_freed = Arc::new(AtomicBool::new(false));
    let parent = unsafe { register_owned_qobject(make_node(Arc::clone(&parent_freed))) };
    let parent_id = parent.data.id();

    let child_freed = Arc::new(AtomicBool::new(false));
    let mut child = Box::new(make_node(Arc::clone(&child_freed)));
    let child_id = child.data.id();

    let seen_in_callback = Rc::new(RefCell::new(false));
    let seen_clone = Rc::clone(&seen_in_callback);
    let child_freed_clone = Arc::clone(&child_freed);

    child.on_user = Some(Box::new(move |_me: &mut EncapsulatedNode| {
        // Safe code reaching parent can only inspect owned_children as a slice.
        // It cannot call .clear() directly on parent.data.owned_children because
        // the field is private!
        with_object(parent_id, |p| {
            assert_eq!(p.object_data().owned_children().len(), 1);
        });
        *seen_clone.borrow_mut() = true;
        // Verify child is still alive during callback
        assert!(!child_freed_clone.load(Ordering::Acquire));
    }));

    unsafe { add_owned_child(parent_id, child) }
        .map_err(|(e, _)| e)
        .expect("attach child");

    dispatch_to_object(child_id, &mut Event::new(EventKind::User(Box::new(()))));
    assert!(*seen_in_callback.borrow());

    drop(parent);
    assert!(child_freed.load(Ordering::Acquire));
}

#[test]
fn test_swapping_two_registered_objects_data_and_dropping_one() {
    let freed_a = Arc::new(AtomicBool::new(false));
    let mut a = unsafe { register_owned_qobject(make_node(Arc::clone(&freed_a))) };
    let a_id = OwnedObject::object_id(&a);

    let freed_b = Arc::new(AtomicBool::new(false));
    let mut b = unsafe { register_owned_qobject(make_node(Arc::clone(&freed_b))) };
    let b_id = OwnedObject::object_id(&b);

    let child_a_freed = Arc::new(AtomicBool::new(false));
    let child_a = Box::new(make_node(Arc::clone(&child_a_freed)));
    let child_a_id = child_a.data.id();
    unsafe { add_owned_child(a_id, child_a) }
        .map_err(|(e, _)| e)
        .expect("attach child_a");

    let child_b_freed = Arc::new(AtomicBool::new(false));
    let child_b = Box::new(make_node(Arc::clone(&child_b_freed)));
    let _child_b_id = child_b.data.id();
    unsafe { add_owned_child(b_id, child_b) }
        .map_err(|(e, _)| e)
        .expect("attach child_b");

    // Swap ObjectData between two live registered objects
    std::mem::swap(a.object_data_mut(), b.object_data_mut());

    // Drop A only
    drop(a);

    // A is deallocated; B remains alive
    assert!(freed_a.load(Ordering::Acquire), "node A must be dropped");
    assert!(!freed_b.load(Ordering::Acquire), "node B must remain alive");

    // Child B was carried by A's ObjectData and thus deleted during A's drop
    assert!(child_b_freed.load(Ordering::Acquire), "child B was cleaned up with A's ObjectData");
    // Child A was carried to B's ObjectData and remains alive with B
    assert!(!child_a_freed.load(Ordering::Acquire), "child A remains alive with B");

    // Registry invariants: both a_id (by OwnedObject::drop(a)) and b_id
    // (by ObjectData::drop(a.data)) have been safely unregistered.
    // Neither can be accessed through registry lookup, preventing any dangling pointer or UAF.
    assert!(with_object(a_id, |_| ()).is_none(), "a_id must not be accessible in registry");
    assert!(with_object(b_id, |_| ()).is_none(), "b_id must not be accessible in registry");

    // Surviving node B can still access its local tree and child without panic or UB
    assert_eq!(b.object_data().owned_children().len(), 1);
    assert_eq!(b.object_data().owned_children()[0].object_data().id(), child_a_id);

    // Drop B cleanly
    drop(b);
    assert!(freed_b.load(Ordering::Acquire), "node B must be dropped");
    assert!(child_a_freed.load(Ordering::Acquire), "child A must be dropped with B");
}
