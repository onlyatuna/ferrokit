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
    add_owned_child, dispatch_to_object, query_object_children, query_object_parent,
    register_owned_qobject, with_object, with_object_mut, ObjectData, ObjectId,
    OwnedObject, QObject,
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
