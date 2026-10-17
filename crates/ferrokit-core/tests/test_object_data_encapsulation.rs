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
    add_owned_child, dispatch_to_object, move_to_thread_by_id, query_object_children,
    query_object_parent, register_owned_qobject, with_object,
    with_object_mut, ObjectData, ObjectId, OwnedObject, QObject, ReparentError,
    ThreadId,
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
    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
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

    // Exchanging content with a fresh, un-registered one via ObjectDataMut proxy
    let mut stolen = ObjectData::new(ObjectId::next());
    a.object_data_mut().swap_content(&mut ferrokit_core::object::ObjectDataMut::new(&mut stolen));

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

    a.object_data_mut().set_object_name("name_a");
    b.object_data_mut().set_object_name("name_b");
    a.block_signals(true);
    b.block_signals(false);

    let child_a_freed = Arc::new(AtomicBool::new(false));
    let child_a = Box::new(make_node(Arc::clone(&child_a_freed)));
    let child_a_id = child_a.data.id();
    unsafe { add_owned_child(a_id, child_a) }
        .map_err(|(e, _)| e)
        .expect("attach child_a");

    let child_b_freed = Arc::new(AtomicBool::new(false));
    let child_b = Box::new(make_node(Arc::clone(&child_b_freed)));
    let child_b_id = child_b.data.id();
    unsafe { add_owned_child(b_id, child_b) }
        .map_err(|(e, _)| e)
        .expect("attach child_b");

    // Initial parent assertions
    assert_eq!(query_object_parent(child_a_id), Some(Some(a_id)));
    assert_eq!(query_object_parent(child_b_id), Some(Some(b_id)));
    assert!(query_object_children(a_id).unwrap().contains(&child_a_id));
    assert!(query_object_children(b_id).unwrap().contains(&child_b_id));

    // Phase 1: Pure property swap (swap_properties / swap_content)
    // Only swaps object_name and dynamic_properties; strictly excludes hierarchy,
    // ownership, signals_blocked, timers, identity and liveness.
    a.object_data_mut().swap_content(&mut b.object_data_mut());
    assert_eq!(a.object_name(), Some("name_b"));
    assert_eq!(b.object_name(), Some("name_a"));
    assert!(a.signals_blocked(), "signals_blocked must NOT be swapped by swap_content");
    assert!(!b.signals_blocked(), "signals_blocked must NOT be swapped by swap_content");
    assert_eq!(a.object_data().owned_children().len(), 1, "swap_content must NOT swap owned_children");
    assert_eq!(b.object_data().owned_children().len(), 1, "swap_content must NOT swap owned_children");
    assert_eq!(query_object_parent(child_a_id), Some(Some(a_id)), "hierarchy must NOT be swapped by swap_content");
    assert_eq!(query_object_parent(child_b_id), Some(Some(b_id)), "hierarchy must NOT be swapped by swap_content");

    // Phase 2: Transactional hierarchy & ownership transfer
    a.object_data_mut()
        .swap_hierarchy_and_ownership(&mut b.object_data_mut())
        .expect("hierarchy swap must succeed");

    // Verification requirement:
    // 交換後（child A 轉移至 B）：
    assert_eq!(
        b.object_data().owned_children()[0].object_data().parent(),
        Some(b_id),
        "child A parent must be Some(b_id) via owned_children after swap"
    );
    with_object(child_a_id, |c| {
        assert_eq!(c.object_data().parent(), Some(b_id), "child A parent must be Some(b_id) via with_object");
    });
    // 驗證 Registry 中 B 的 children 清單與 child A 的父物件記錄同步
    assert_eq!(query_object_parent(child_a_id), Some(Some(b_id)));
    assert!(query_object_children(b_id).unwrap().contains(&child_a_id));

    // child B 轉移至 A：
    assert_eq!(
        a.object_data().owned_children()[0].object_data().parent(),
        Some(a_id),
        "child B parent must be Some(a_id) via owned_children after swap"
    );
    with_object(child_b_id, |c| {
        assert_eq!(c.object_data().parent(), Some(a_id), "child B parent must be Some(a_id) via with_object");
    });
    assert_eq!(query_object_parent(child_b_id), Some(Some(a_id)));
    assert!(query_object_children(a_id).unwrap().contains(&child_b_id));

    // signals_blocked must remain undisturbed
    assert!(a.signals_blocked());
    assert!(!b.signals_blocked());

    // Phase 3: Drop A only
    drop(a);

    // A is deallocated; B remains alive
    assert!(freed_a.load(Ordering::Acquire), "node A must be dropped");
    assert!(!freed_b.load(Ordering::Acquire), "node B must remain alive");

    // Child B was carried by A's ownership and thus deleted during A's drop
    assert!(child_b_freed.load(Ordering::Acquire), "child B was cleaned up with A's drop");
    // Child A was carried to B's ownership and remains alive with B
    assert!(!child_a_freed.load(Ordering::Acquire), "child A remains alive with B");

    // Registry & Identity invariants:
    assert!(with_object(a_id, |_| ()).is_none(), "a_id must not be accessible in registry");
    assert!(with_object(b_id, |_| ()).is_some(), "b_id must remain valid and registered in registry");
    assert_eq!(b.object_data().id(), b_id, "surviving node B must retain its b_id identity");
    assert_eq!(OwnedObject::object_id(&b), b_id, "outer OwnedObject id must match b_id");
    assert!(b.object_data().is_alive(), "surviving node B must remain active/alive");

    // Verification requirement:
    // drop(A) 之後：
    assert_eq!(
        b.object_data().owned_children()[0].object_data().parent(),
        Some(b_id),
        "child A parent must remain Some(b_id) after drop(A)"
    );
    with_object(child_a_id, |c| {
        assert_eq!(c.object_data().parent(), Some(b_id), "child A parent must remain Some(b_id) in registry after drop(A)");
    });
    // 驗證 Registry 中 B 的 children 清單與 child A 的父物件記錄同步
    assert_eq!(query_object_parent(child_a_id), Some(Some(b_id)));
    assert!(query_object_children(b_id).unwrap().contains(&child_a_id));

    // Surviving node B operations and signals_blocked state:
    assert!(!b.signals_blocked(), "surviving B signals_blocked remains false");
    b.object_data_mut().set_object_name("surviving_b");
    assert_eq!(b.object_name(), Some("surviving_b"));
    let mut ev = Event::new(EventKind::User(Box::new(42u32)));
    assert!(dispatch_to_object(b_id, &mut ev), "event dispatch to surviving B must succeed");

    // Drop B cleanly
    drop(b);
    assert!(freed_b.load(Ordering::Acquire), "node B must be dropped");
    assert!(child_a_freed.load(Ordering::Acquire), "child A must be dropped with B");
    assert!(with_object(b_id, |_| ()).is_none(), "b_id must be cleanly unregistered on drop");
    assert!(with_object(child_a_id, |_| ()).is_none(), "child_a_id must be cleanly unregistered on drop");
}

#[test]
fn test_nested_child_cleanup_across_three_levels_of_swapped_objects() {
    let freed_root = Arc::new(AtomicBool::new(false));
    let mut root = unsafe { register_owned_qobject(make_node(Arc::clone(&freed_root))) };
    let root_id = OwnedObject::object_id(&root);

    let freed_peer = Arc::new(AtomicBool::new(false));
    let mut peer = unsafe { register_owned_qobject(make_node(Arc::clone(&freed_peer))) };
    let peer_id = OwnedObject::object_id(&peer);

    // Build 2-level subtree: parent -> grandchild
    let freed_child = Arc::new(AtomicBool::new(false));
    let child = Box::new(make_node(Arc::clone(&freed_child)));
    let child_id = child.data.id();

    let freed_grandchild = Arc::new(AtomicBool::new(false));
    let grandchild = Box::new(make_node(Arc::clone(&freed_grandchild)));
    let grandchild_id = grandchild.data.id();

    unsafe { add_owned_child(root_id, child) }
        .map_err(|(e, _)| e)
        .expect("attach child to root");

    unsafe { add_owned_child(child_id, grandchild) }
        .map_err(|(e, _)| e)
        .expect("attach grandchild to child");

    // Transactionally swap hierarchy between root (child -> grandchild) and peer (empty)
    root.object_data_mut()
        .swap_hierarchy_and_ownership(&mut peer.object_data_mut())
        .expect("hierarchy swap must succeed");

    // Peer now holds child -> grandchild; root is now empty of children
    assert_eq!(root.object_data().owned_children().len(), 0);
    assert_eq!(peer.object_data().owned_children().len(), 1);
    assert_eq!(peer.object_data().owned_children()[0].object_data().parent(), Some(peer_id));
    assert_eq!(query_object_parent(child_id), Some(Some(peer_id)));
    assert!(query_object_children(peer_id).unwrap().contains(&child_id));

    // Drop root: only root is deallocated; peer and its subtree stay alive
    drop(root);
    assert!(freed_root.load(Ordering::Acquire), "root dropped");
    assert!(!freed_peer.load(Ordering::Acquire), "peer alive");
    assert!(!freed_child.load(Ordering::Acquire), "child alive with peer");
    assert!(!freed_grandchild.load(Ordering::Acquire), "grandchild alive with peer");

    // Peer is still validly registered under peer_id with its synchronized subtree
    assert!(with_object(peer_id, |_| ()).is_some());
    assert_eq!(peer.object_data().id(), peer_id);
    assert_eq!(peer.object_data().owned_children()[0].object_data().parent(), Some(peer_id));
    assert_eq!(query_object_parent(child_id), Some(Some(peer_id)));
    assert!(query_object_children(peer_id).unwrap().contains(&child_id));

    // Dropping peer cleanly cascades deletion through all nested levels
    drop(peer);
    assert!(freed_peer.load(Ordering::Acquire), "peer dropped");
    assert!(freed_child.load(Ordering::Acquire), "child cascadingly dropped");
    assert!(freed_grandchild.load(Ordering::Acquire), "grandchild cascadingly dropped");
    assert!(with_object(peer_id, |_| ()).is_none());
    assert!(with_object(child_id, |_| ()).is_none());
    assert!(with_object(grandchild_id, |_| ()).is_none());
}


#[test]
fn test_swap_hierarchy_with_non_owned_child() {
    let freed_a = Arc::new(AtomicBool::new(false));
    let mut a = unsafe { register_owned_qobject(make_node(Arc::clone(&freed_a))) };
    let a_id = OwnedObject::object_id(&a);

    let freed_b = Arc::new(AtomicBool::new(false));
    let mut b = unsafe { register_owned_qobject(make_node(Arc::clone(&freed_b))) };
    let b_id = OwnedObject::object_id(&b);

    let freed_c = Arc::new(AtomicBool::new(false));
    let mut c = unsafe { register_owned_qobject(make_node(Arc::clone(&freed_c))) };
    let c_id = OwnedObject::object_id(&c);

    // Attach C as a non-owned child to A
    c.object_data_mut().set_parent(Some(a_id)).expect("set parent");

    // Bidirectional Registry & Local checks before swap
    assert_eq!(c.object_data().parent(), Some(a_id));
    assert_eq!(query_object_parent(c_id), Some(Some(a_id)));
    assert!(query_object_children(a_id).unwrap().contains(&c_id));
    assert!(!query_object_children(b_id).unwrap().contains(&c_id));
    assert!(a.object_data().children().contains(&c_id));
    assert!(!b.object_data().children().contains(&c_id));

    // Swap hierarchy between A (holds non-owned C) and B (empty)
    a.object_data_mut()
        .swap_hierarchy_and_ownership(&mut b.object_data_mut())
        .expect("swap hierarchy must succeed for non-owned child");

    // Bidirectional Registry & Local checks after swap
    assert_eq!(c.object_data().parent(), Some(b_id), "non-owned child C local parent must be Some(b_id)");
    with_object(c_id, |obj| {
        assert_eq!(obj.object_data().parent(), Some(b_id), "non-owned child C via with_object must be Some(b_id)");
    });
    assert_eq!(query_object_parent(c_id), Some(Some(b_id)), "Registry parent of C must be Some(b_id)");
    assert!(query_object_children(b_id).unwrap().contains(&c_id), "Registry children of B must contain C");
    assert!(!query_object_children(a_id).unwrap().contains(&c_id), "Registry children of A must NOT contain C");
    assert!(b.object_data().children().contains(&c_id), "B local children must contain C");
    assert!(!a.object_data().children().contains(&c_id), "A local children must NOT contain C");

    // Drop A only: A is deallocated; C was not owned by A, so C survives under B!
    drop(a);
    assert!(freed_a.load(Ordering::Acquire), "A is dropped");
    assert!(!freed_b.load(Ordering::Acquire), "B is alive");
    assert!(!freed_c.load(Ordering::Acquire), "non-owned child C remains alive");

    // Post drop(A) checks: C's relationship to surviving B is preserved
    assert_eq!(c.object_data().parent(), Some(b_id));
    assert_eq!(query_object_parent(c_id), Some(Some(b_id)));
    assert!(query_object_children(b_id).unwrap().contains(&c_id));

    // Drop B: B is deallocated; C is a non-owned child held by caller
    drop(b);
    assert!(freed_b.load(Ordering::Acquire), "B is dropped");
    assert!(!freed_c.load(Ordering::Acquire), "C still alive as held by outer OwnedObject");
    assert_eq!(c.object_data().parent(), None);

    // Drop C: cleanly unregistered
    drop(c);
    assert!(freed_c.load(Ordering::Acquire), "C is dropped");
    assert!(with_object(c_id, |_| ()).is_none());
}

#[test]
fn test_swap_hierarchy_rejects_cross_thread() {
    let freed_a = Arc::new(AtomicBool::new(false));
    let mut a = unsafe { register_owned_qobject(make_node(Arc::clone(&freed_a))) };
    let a_id = OwnedObject::object_id(&a);

    let freed_b = Arc::new(AtomicBool::new(false));
    let mut b = unsafe { register_owned_qobject(make_node(Arc::clone(&freed_b))) };
    let b_id = OwnedObject::object_id(&b);

    // Move B to a remote thread
    let remote_thread = std::thread::spawn(|| ThreadId::current()).join().unwrap();
    move_to_thread_by_id(b_id, remote_thread, ThreadId::current()).expect("move B to remote thread");
    assert_eq!(b.object_data().thread(), remote_thread);

    let child_a_freed = Arc::new(AtomicBool::new(false));
    let child_a = Box::new(make_node(Arc::clone(&child_a_freed)));
    let child_a_id = child_a.data.id();
    unsafe { add_owned_child(a_id, child_a) }
        .map_err(|(e, _)| e)
        .expect("attach child_a");

    // Attempting to swap hierarchy across different thread affinities must be rejected!
    let res = a.object_data_mut().swap_hierarchy_and_ownership(&mut b.object_data_mut());
    assert_eq!(res, Err(ReparentError::DifferentThread));

    // Verify zero state mutation
    assert_eq!(a.object_data().owned_children().len(), 1);
    assert!(b.object_data().owned_children().is_empty());
    assert_eq!(query_object_parent(child_a_id), Some(Some(a_id)));
    assert!(query_object_children(a_id).unwrap().contains(&child_a_id));
    assert!(query_object_children(b_id).unwrap().is_empty());

    // Clean drop
    drop(a);
    drop(b);
}

#[test]
fn test_swap_hierarchy_rejects_cycle() {
    let freed_a = Arc::new(AtomicBool::new(false));
    let mut a = unsafe { register_owned_qobject(make_node(Arc::clone(&freed_a))) };
    let a_id = OwnedObject::object_id(&a);

    let freed_b = Arc::new(AtomicBool::new(false));
    let mut b = unsafe { register_owned_qobject(make_node(Arc::clone(&freed_b))) };
    let b_id = OwnedObject::object_id(&b);

    // Make B a child of A
    b.object_data_mut().set_parent(Some(a_id)).expect("set parent");
    assert_eq!(b.object_data().parent(), Some(a_id));
    assert_eq!(query_object_parent(b_id), Some(Some(a_id)));
    assert!(query_object_children(a_id).unwrap().contains(&b_id));

    // Attempting to swap hierarchy between parent A and its own descendant B must be rejected!
    let res = a.object_data_mut().swap_hierarchy_and_ownership(&mut b.object_data_mut());
    assert_eq!(res, Err(ReparentError::InvalidParent));

    // Verify zero state mutation
    assert_eq!(b.object_data().parent(), Some(a_id));
    assert_eq!(query_object_parent(b_id), Some(Some(a_id)));
    assert!(query_object_children(a_id).unwrap().contains(&b_id));
    assert!(b.object_data().children().is_empty());

    // Clean drop
    drop(b);
    drop(a);
}


#[test]
fn test_swap_hierarchy_rejects_missing_registry_record() {
    let freed_a = Arc::new(AtomicBool::new(false));
    let mut a = unsafe { register_owned_qobject(make_node(Arc::clone(&freed_a))) };

    // Node B is not registered in the global registry
    let freed_b = Arc::new(AtomicBool::new(false));
    let mut b = make_node(Arc::clone(&freed_b));

    // Attempt swap with unregistered B
    let res = a.object_data_mut().swap_hierarchy_and_ownership(&mut b.object_data_mut());
    assert_eq!(res, Err(ReparentError::UnknownObject));

    // Flipped direction
    let res = b.object_data_mut().swap_hierarchy_and_ownership(&mut a.object_data_mut());
    assert_eq!(res, Err(ReparentError::UnknownObject));

    // Now test child missing from registry:
    let freed_b2 = Arc::new(AtomicBool::new(false));
    let mut b2 = unsafe { register_owned_qobject(make_node(Arc::clone(&freed_b2))) };
    let a_id = OwnedObject::object_id(&a);

    let freed_c = Arc::new(AtomicBool::new(false));
    let mut c = unsafe { register_owned_qobject(make_node(Arc::clone(&freed_c))) };
    let c_id = OwnedObject::object_id(&c);

    c.object_data_mut().set_parent(Some(a_id)).expect("set parent");
    assert_eq!(query_object_parent(c_id), Some(Some(a_id)));

    // Unregister C so its registry record is missing
    unsafe { ferrokit_core::object::unregister_qobject(c_id) };
    assert_eq!(query_object_parent(c_id), None);

    // Attempt swap - must be rejected with InvalidParent (hierarchy inconsistency)
    let res = a.object_data_mut().swap_hierarchy_and_ownership(&mut b2.object_data_mut());
    assert_eq!(res, Err(ReparentError::InvalidParent));

    drop(c);
    drop(b2);
    drop(a);
}

#[test]
fn test_swap_hierarchy_rejects_when_child_is_borrowed() {
    let freed_a = Arc::new(AtomicBool::new(false));
    let mut a = unsafe { register_owned_qobject(make_node(Arc::clone(&freed_a))) };
    let a_id = OwnedObject::object_id(&a);

    let freed_b = Arc::new(AtomicBool::new(false));
    let mut b = unsafe { register_owned_qobject(make_node(Arc::clone(&freed_b))) };

    let freed_c = Arc::new(AtomicBool::new(false));
    let mut c = unsafe { register_owned_qobject(make_node(Arc::clone(&freed_c))) };
    let c_id = OwnedObject::object_id(&c);

    c.object_data_mut().set_parent(Some(a_id)).expect("set parent");

    // Borrow C via with_object_mut and attempt swap inside the callback
    let result = with_object_mut(c_id, |_child| {
        a.object_data_mut().swap_hierarchy_and_ownership(&mut b.object_data_mut())
    }).expect("with_object_mut ran");

    // Attempting to swap while child is borrowed must return Err(ReparentError::Busy)
    assert_eq!(result, Err(ReparentError::Busy));

    // Verify hierarchy was NOT changed
    assert_eq!(c.object_data().parent(), Some(a_id));
    assert_eq!(query_object_parent(c_id), Some(Some(a_id)));
    assert!(query_object_children(a_id).unwrap().contains(&c_id));
    assert!(b.object_data().children().is_empty());

    drop(c);
    drop(b);
    drop(a);
}

#[test]
fn test_swap_hierarchy_concurrent_visibility_is_atomic() {
    let freed_a = Arc::new(AtomicBool::new(false));
    let mut a = unsafe { register_owned_qobject(make_node(Arc::clone(&freed_a))) };
    let a_id = OwnedObject::object_id(&a);

    let freed_b = Arc::new(AtomicBool::new(false));
    let mut b = unsafe { register_owned_qobject(make_node(Arc::clone(&freed_b))) };
    let b_id = OwnedObject::object_id(&b);

    let freed_c1 = Arc::new(AtomicBool::new(false));
    let mut c1 = unsafe { register_owned_qobject(make_node(Arc::clone(&freed_c1))) };
    let c1_id = OwnedObject::object_id(&c1);
    c1.object_data_mut().set_parent(Some(a_id)).expect("c1 under a");

    let freed_c2 = Arc::new(AtomicBool::new(false));
    let mut c2 = unsafe { register_owned_qobject(make_node(Arc::clone(&freed_c2))) };
    let c2_id = OwnedObject::object_id(&c2);
    c2.object_data_mut().set_parent(Some(b_id)).expect("c2 under b");

    // Background reader continuously checks registry visibility
    let stop = Arc::new(AtomicBool::new(false));
    let inconsistencies = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let reader = std::thread::spawn({
        let stop = Arc::clone(&stop);
        let inconsistencies = Arc::clone(&inconsistencies);
        move || {
            while !stop.load(Ordering::Acquire) {
                if let (Some(Some(p1)), Some(Some(p2))) = (query_object_parent(c1_id), query_object_parent(c2_id)) {
                    // Valid states:
                    // State 0: p1 == a_id && p2 == b_id
                    // State 1: p1 == b_id && p2 == a_id
                    // Any intermediate state (e.g. p1 == b_id && p2 == b_id or p1 == a_id && p2 == a_id) is an invariant violation!
                    if (p1 == a_id && p2 != b_id) || (p1 == b_id && p2 != a_id) {
                        inconsistencies.fetch_add(1, Ordering::SeqCst);
                    }
                }
            }
        }
    });

    // Execute swap on main thread
    let res = a.object_data_mut().swap_hierarchy_and_ownership(&mut b.object_data_mut());
    assert!(res.is_ok());

    stop.store(true, Ordering::Release);
    reader.join().unwrap();

    assert_eq!(
        inconsistencies.load(Ordering::SeqCst),
        0,
        "Concurrent reader observed an inconsistent intermediate hierarchy state!"
    );

    // Verify post-swap state
    assert_eq!(c1.object_data().parent(), Some(b_id));
    assert_eq!(c2.object_data().parent(), Some(a_id));
    assert_eq!(query_object_parent(c1_id), Some(Some(b_id)));
    assert_eq!(query_object_parent(c2_id), Some(Some(a_id)));

    drop(c1);
    drop(c2);
    drop(b);
    drop(a);
}

#[test]
fn test_swap_hierarchy_mixed_owned_and_non_owned_children() {
    let freed_a = Arc::new(AtomicBool::new(false));
    let mut a = unsafe { register_owned_qobject(make_node(Arc::clone(&freed_a))) };
    let a_id = OwnedObject::object_id(&a);

    let freed_b = Arc::new(AtomicBool::new(false));
    let mut b = unsafe { register_owned_qobject(make_node(Arc::clone(&freed_b))) };
    let b_id = OwnedObject::object_id(&b);

    // A gets: 1 owned child (oa) and 1 non-owned child (na)
    let freed_oa = Arc::new(AtomicBool::new(false));
    let oa = Box::new(make_node(Arc::clone(&freed_oa)));
    let oa_id = oa.data.id();
    unsafe { add_owned_child(a_id, oa) }.map_err(|(e, _)| e).expect("add oa");

    let freed_na = Arc::new(AtomicBool::new(false));
    let mut na = unsafe { register_owned_qobject(make_node(Arc::clone(&freed_na))) };
    let na_id = OwnedObject::object_id(&na);
    na.object_data_mut().set_parent(Some(a_id)).expect("set parent na");

    // B gets: 1 owned child (ob) and 1 non-owned child (nb)
    let freed_ob = Arc::new(AtomicBool::new(false));
    let ob = Box::new(make_node(Arc::clone(&freed_ob)));
    let ob_id = ob.data.id();
    unsafe { add_owned_child(b_id, ob) }.map_err(|(e, _)| e).expect("add ob");

    let freed_nb = Arc::new(AtomicBool::new(false));
    let mut nb = unsafe { register_owned_qobject(make_node(Arc::clone(&freed_nb))) };
    let nb_id = OwnedObject::object_id(&nb);
    nb.object_data_mut().set_parent(Some(b_id)).expect("set parent nb");

    // Verify initial setup
    assert_eq!(a.object_data().children().len(), 2);
    assert_eq!(a.object_data().owned_children().len(), 1);
    assert_eq!(b.object_data().children().len(), 2);
    assert_eq!(b.object_data().owned_children().len(), 1);

    // Swap hierarchy
    let res = a.object_data_mut().swap_hierarchy_and_ownership(&mut b.object_data_mut());
    assert!(res.is_ok());

    // Post-swap verification:
    // A now holds ob (owned) and nb (non-owned)
    assert_eq!(a.object_data().owned_children().len(), 1);
    assert_eq!(OwnedObject::object_id(&a.object_data().owned_children()[0]), ob_id);
    assert!(a.object_data().children().contains(&ob_id));
    assert!(a.object_data().children().contains(&nb_id));
    assert_eq!(query_object_parent(ob_id), Some(Some(a_id)));
    assert_eq!(query_object_parent(nb_id), Some(Some(a_id)));

    // B now holds oa (owned) and na (non-owned)
    assert_eq!(b.object_data().owned_children().len(), 1);
    assert_eq!(OwnedObject::object_id(&b.object_data().owned_children()[0]), oa_id);
    assert!(b.object_data().children().contains(&oa_id));
    assert!(b.object_data().children().contains(&na_id));
    assert_eq!(query_object_parent(oa_id), Some(Some(b_id)));
    assert_eq!(query_object_parent(na_id), Some(Some(b_id)));

    // Drop A:
    // ob (which was owned by A) is destroyed!
    // nb (which is non-owned) survives!
    drop(a);
    assert!(freed_oa.load(Ordering::Acquire) == false, "oa still alive under b");
    assert!(freed_ob.load(Ordering::Acquire) == true, "ob destroyed when new parent a dropped");
    assert!(freed_nb.load(Ordering::Acquire) == false, "nb non-owned survives drop(a)");

    // Drop B:
    // oa (which was owned by B) is destroyed!
    // na (which is non-owned) survives!
    drop(b);
    assert!(freed_oa.load(Ordering::Acquire) == true, "oa destroyed when new parent b dropped");
    assert!(freed_na.load(Ordering::Acquire) == false, "na non-owned survives drop(b)");

    // Drop remaining non-owned children
    drop(na);
    drop(nb);
    assert!(freed_na.load(Ordering::Acquire));
    assert!(freed_nb.load(Ordering::Acquire));
}
