// SPDX-License-Identifier: LGPL-3.0-only
use super::*;
use std::ptr::NonNull;
use std::sync::atomic::AtomicUsize;

/// Timer event handler (`QObject::timerEvent`).
#[test]
fn test_timer_event_dispatch() {
    struct MockWidget {
        data: ObjectData,
        received_timer_id: Option<u64>,
    }

    impl MockWidget {
        fn new(id: ObjectId) -> Self {
            Self {
                data: ObjectData::new(id),
                received_timer_id: None,
            }
        }
    }

    impl QObject for MockWidget {
        fn object_data(&self) -> &ObjectData {
            &self.data
        }
        fn object_data_mut(&mut self) -> ObjectDataMut<'_> {
            ObjectDataMut::new(&mut self.data)
        }
        fn timer_event(&mut self, timer_id: u64) {
            self.received_timer_id = Some(timer_id);
        }
    }

    let mut mock = MockWidget::new(ObjectId::next());
    let mut ev = Event::new(EventKind::Timer { timer_id: 99 });
    let handled = mock.event(&mut ev);

    assert!(handled);
    assert_eq!(mock.received_timer_id, Some(99));
}

/// Event filter callback (`QObject::eventFilter`).

#[test]
fn test_event_filter_interception() {
    struct EventSnooper {
        data: ObjectData,
        intercept: bool,
    }

    impl EventSnooper {
        fn new(id: ObjectId, intercept: bool) -> Self {
            Self {
                data: ObjectData::new(id),
                intercept,
            }
        }
    }

    impl QObject for EventSnooper {
        fn object_data(&self) -> &ObjectData {
            &self.data
        }
        fn object_data_mut(&mut self) -> ObjectDataMut<'_> {
            ObjectDataMut::new(&mut self.data)
        }
        fn event_filter(&mut self, _watched: ObjectId, _event: &mut Event) -> bool {
            self.intercept
        }
    }

    struct TargetWidget {
        data: ObjectData,
        event_count: usize,
    }

    impl TargetWidget {
        fn new(id: ObjectId) -> Self {
            Self {
                data: ObjectData::new(id),
                event_count: 0,
            }
        }
    }

    impl QObject for TargetWidget {
        fn object_data(&self) -> &ObjectData {
            &self.data
        }
        fn object_data_mut(&mut self) -> ObjectDataMut<'_> {
            ObjectDataMut::new(&mut self.data)
        }
        fn event(&mut self, _event: &mut Event) -> bool {
            self.event_count += 1;
            true
        }
    }

    let mut snooper = EventSnooper::new(ObjectId::next(), true);
    let mut target = TargetWidget::new(ObjectId::next());

    install_event_filter(
        target.object_data_mut().raw_data_mut(),
        snooper.object_data().id(),
    );

    let mut ev = Event::new(EventKind::UpdateRequest);

    let mut filtered = false;
    for filter_id in target.object_data().event_filters.snapshot() {
        if filter_id == snooper.object_data().id()
            && snooper.event_filter(target.object_data().id(), &mut ev)
        {
            filtered = true;
            break;
        }
    }

    if !filtered {
        target.event(&mut ev);
    }

    assert_eq!(target.event_count, 0);
}

/// A parent that is not registered is accepted and only the child's own link is recorded:
/// the parent's list stays empty. Qt has no such case (a `QObject*` is always a live object),
/// so `set_parent` specifies nothing more for it; the docs of `set_parent` say so.
#[test]
fn test_set_parent_to_an_unregistered_parent_records_only_the_childs_link() {
    let parent = ObjectData::new(ObjectId::next());
    let mut child = ObjectData::new(ObjectId::next());

    set_parent(&mut child, Some(parent.id)).unwrap();
    assert_eq!(child.parent, Some(parent.id));
    assert!(parent.children.is_empty());

    let parent2 = ObjectData::new(ObjectId::next());
    set_parent(&mut child, Some(parent2.id)).unwrap();
    assert_eq!(child.parent, Some(parent2.id));
    assert!(parent.children.is_empty());
    assert!(parent2.children.is_empty());

    set_parent(&mut child, None).unwrap();
    assert_eq!(child.parent, None);
    assert!(parent2.children.is_empty());
}

/// Matches `QObject::deleteLater` (qobject.cpp:2511-2565): the first request posts one
/// stamped `DeferredDelete` to the owning thread and later requests are debounced.
#[test]
fn test_delete_later() {
    struct Probe {
        data: ObjectData,
    }
    impl QObject for Probe {
        fn object_data(&self) -> &ObjectData {
            &self.data
        }
        fn object_data_mut(&mut self) -> ObjectDataMut<'_> {
            ObjectDataMut::new(&mut self.data)
        }
    }

    let el = crate::event_loop::EventLoop::new();
    let mut probe = Probe {
        data: ObjectData::new(ObjectId::next()),
    };
    let id = probe.data.id();
    // SAFETY: `probe` stays at this stack address and is unregistered below before it drops.
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut probe)) };

    probe.delete_later();
    probe.delete_later();

    let queue = el.queue();
    let queue = queue.lock().unwrap();
    assert_eq!(queue.events().len(), 1);
    assert!(matches!(
        queue.events()[0].event.kind,
        EventKind::DeferredDelete {
            loop_level: 0,
            scope_level: 0
        }
    ));
    drop(queue);
    // SAFETY: the registry callback has completed on this thread.
    unsafe { unregister_qobject(id) };
}

/// Core object metadata: ObjectData (`QObjectData` equivalent).
#[test]
fn test_unsafe_registration_dispatches_on_owner_thread() {
    struct EphemeralWidget {
        data: ObjectData,
        events: Arc<AtomicUsize>,
    }
    impl QObject for EphemeralWidget {
        fn object_data(&self) -> &ObjectData {
            &self.data
        }
        fn object_data_mut(&mut self) -> ObjectDataMut<'_> {
            ObjectDataMut::new(&mut self.data)
        }
        fn custom_event(&mut self, _event: &mut Event) {
            self.events.fetch_add(1, Ordering::SeqCst);
        }
    }

    let id = ObjectId::next();
    let events = Arc::new(AtomicUsize::new(0));
    let mut widget = EphemeralWidget {
        data: ObjectData::new(id),
        events: Arc::clone(&events),
    };
    // SAFETY: widget remains at this stack address and alive until unregistered; this
    // thread performs all accesses and no direct alias is used during registry callbacks.
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut widget)) };

    assert!(with_object(id, |_| ()).is_some());
    assert!(with_object_mut(id, |_| ()).is_some());
    let mut ev = Event::new(EventKind::User(Box::new(())));
    assert!(dispatch_to_object(id, &mut ev));
    assert_eq!(events.load(Ordering::SeqCst), 1);
    // SAFETY: the last registry callback has completed on this thread.
    unsafe { unregister_qobject(id) };

    let mut ev = Event::new(EventKind::UpdateRequest);
    assert!(!send_event(id, &mut ev));
}

/// A ChildAdded for a borrowed registered parent is posted and delivered after the
/// borrow ends, not dropped (Qt's setParent_helper sends it through sendEvent,
/// qobject.cpp:2287-2352).
#[test]
fn test_child_added_to_borrowed_parent_is_deferred_not_lost() {
    struct Parent {
        data: ObjectData,
        child_added: Arc<AtomicUsize>,
    }
    impl QObject for Parent {
        fn object_data(&self) -> &ObjectData {
            &self.data
        }
        fn object_data_mut(&mut self) -> ObjectDataMut<'_> {
            ObjectDataMut::new(&mut self.data)
        }
        fn event(&mut self, event: &mut Event) -> bool {
            if matches!(event.kind, EventKind::ChildAdded { .. }) {
                self.child_added.fetch_add(1, Ordering::SeqCst);
                return true;
            }
            false
        }
    }

    let el = crate::event_loop::EventLoop::new();
    let child_added = Arc::new(AtomicUsize::new(0));
    let mut parent = Parent {
        data: ObjectData::new(ObjectId::next()),
        child_added: Arc::clone(&child_added),
    };
    let parent_id = parent.data.id();
    // SAFETY: `parent` stays at this stack address and is unregistered below before it drops.
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut parent)) };
    let mut child = ObjectData::new(ObjectId::next());

    // The parent's borrow is held while `set_parent` runs, as in a re-entrant callback.
    let during = with_object_mut(parent_id, |_| {
        set_parent(&mut child, Some(parent_id)).unwrap();
        el.send_posted_events()
    });
    assert_eq!(during, Some(0));
    assert_eq!(child_added.load(Ordering::SeqCst), 0);
    assert_eq!(
        el.send_posted_events(),
        1,
        "ChildAdded lost: parent was borrowed during set_parent"
    );
    assert_eq!(child_added.load(Ordering::SeqCst), 1);
    // SAFETY: the last callback has completed on this thread.
    unsafe { unregister_qobject(parent_id) };
}

/// A synchronous send to a borrowed receiver returns `false` and is counted, not
/// silently dropped. Qt has no borrow check here (qcoreapplication.cpp:1563-1572); this is the
/// documented Rust deviation.
#[test]
fn test_sync_send_to_borrowed_receiver_is_rejected_visibly() {
    struct Target {
        data: ObjectData,
        events: Arc<AtomicUsize>,
    }
    impl QObject for Target {
        fn object_data(&self) -> &ObjectData {
            &self.data
        }
        fn object_data_mut(&mut self) -> ObjectDataMut<'_> {
            ObjectDataMut::new(&mut self.data)
        }
        fn event(&mut self, _event: &mut Event) -> bool {
            self.events.fetch_add(1, Ordering::SeqCst);
            true
        }
    }

    let events = Arc::new(AtomicUsize::new(0));
    let mut target = Target {
        data: ObjectData::new(ObjectId::next()),
        events: Arc::clone(&events),
    };
    let id = target.data.id();
    // SAFETY: `target` stays at this stack address and is unregistered below before it drops.
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut target)) };

    let rejections_before = crate::event_loop::borrowed_send_rejections();
    let sent = with_object_mut(id, |_| {
        let mut ev = Event::new(EventKind::UpdateRequest);
        send_event(id, &mut ev)
    });
    assert_eq!(
        sent,
        Some(false),
        "a sync send to a borrowed receiver must report failure"
    );
    assert_eq!(
        crate::event_loop::borrowed_send_rejections() - rejections_before,
        1,
        "reentrant send was silently dropped"
    );
    assert_eq!(
        events.load(Ordering::SeqCst),
        0,
        "receiver must not run re-entrantly"
    );
    // SAFETY: the last callback has completed on this thread.
    unsafe { unregister_qobject(id) };
}

/// A child destroyed while its registered parent is borrowed posts `ChildRemoved`
/// instead of dropping it. This is a documented Rust deviation: the borrow forbids the
/// immediate call, and a deferred delivery keeps the notification.
#[test]
fn test_child_removed_on_drop_while_parent_borrowed_is_deferred_not_lost() {
    struct Parent {
        data: ObjectData,
        removed: Arc<AtomicUsize>,
    }
    impl QObject for Parent {
        fn object_data(&self) -> &ObjectData {
            &self.data
        }
        fn object_data_mut(&mut self) -> ObjectDataMut<'_> {
            ObjectDataMut::new(&mut self.data)
        }
        fn event(&mut self, event: &mut Event) -> bool {
            if matches!(event.kind, EventKind::ChildRemoved { .. }) {
                self.removed.fetch_add(1, Ordering::SeqCst);
                return true;
            }
            false
        }
    }

    let el = crate::event_loop::EventLoop::new();
    let removed = Arc::new(AtomicUsize::new(0));
    let mut parent = Parent {
        data: ObjectData::new(ObjectId::next()),
        removed: Arc::clone(&removed),
    };
    let parent_id = parent.data.id();
    // SAFETY: `parent` stays at this stack address and is unregistered below before it drops.
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut parent)) };
    let mut child = ObjectData::new(ObjectId::next());
    set_parent(&mut child, Some(parent_id)).unwrap();

    with_object_mut(parent_id, move |_| drop(child));
    assert_eq!(removed.load(Ordering::SeqCst), 0);
    assert_eq!(
        el.send_posted_events(),
        1,
        "ChildRemoved lost: child dropped while parent was borrowed"
    );
    assert_eq!(removed.load(Ordering::SeqCst), 1);
    // SAFETY: the last callback has completed on this thread.
    unsafe { unregister_qobject(parent_id) };
}

#[test]
fn test_swap_hierarchy_handles_poisoned_registry_lock() {
    struct Node(ObjectData);
    impl QObject for Node {
        fn object_data(&self) -> &ObjectData {
            &self.0
        }
        fn object_data_mut(&mut self) -> ObjectDataMut<'_> {
            ObjectDataMut::new(&mut self.0)
        }
    }

    let mut a = Node(ObjectData::new(ObjectId::next()));
    let mut b = Node(ObjectData::new(ObjectId::next()));
    let a_id = a.0.id();
    let b_id = b.0.id();
    let ptr_a = &raw mut a as *mut dyn QObject;
    register_object_metadata(&a.0, ptr_a);
    let ptr_b = &raw mut b as *mut dyn QObject;
    register_object_metadata(&b.0, ptr_b);

    let record_a = GLOBAL_OBJECT_REGISTRY
        .read()
        .unwrap()
        .get(&a_id)
        .unwrap()
        .clone();

    // Poison node_a's children lock on a background thread
    let handle = std::thread::spawn({
        let record_a = Arc::clone(&record_a);
        move || {
            let _lock = record_a.children.write().unwrap();
            panic!("intentional lock poison");
        }
    });
    let _ = handle.join();
    assert!(record_a.children.is_poisoned());

    // Attempt swap - must return Err(ReparentError::Busy) without panicking
    let before = states(&[&a.0, &b.0]);
    let res = a
        .object_data_mut()
        .swap_hierarchy_and_ownership(&mut b.object_data_mut());
    assert_eq!(res, Err(ReparentError::Busy));
    assert_eq!(
        states(&[&a.0, &b.0]),
        before,
        "a refused swap changed something"
    );

    // SAFETY: `a` and `b` are locals of this test, registered above and not moved since; this
    // thread is their registration thread, and nothing is delivered to them, so no callback of
    // either runs. Both outlive the call (they drop at the end of the function).
    unsafe {
        unregister_qobject(a_id);
        unregister_qobject(b_id);
    }
}

#[test]
fn test_swap_hierarchy_handles_poisoned_ancestor_parent_lock() {
    struct Node(ObjectData);
    impl QObject for Node {
        fn object_data(&self) -> &ObjectData {
            &self.0
        }
        fn object_data_mut(&mut self) -> ObjectDataMut<'_> {
            ObjectDataMut::new(&mut self.0)
        }
    }

    let mut ancestor = Node(ObjectData::new(ObjectId::next()));
    let mut a = Node(ObjectData::new(ObjectId::next()));
    let mut b = Node(ObjectData::new(ObjectId::next()));

    let ancestor_id = ancestor.0.id();
    let a_id = a.0.id();
    let b_id = b.0.id();

    let ptr_ancestor = &raw mut ancestor as *mut dyn QObject;
    register_object_metadata(&ancestor.0, ptr_ancestor);
    let ptr_a = &raw mut a as *mut dyn QObject;
    register_object_metadata(&a.0, ptr_a);
    let ptr_b = &raw mut b as *mut dyn QObject;
    register_object_metadata(&b.0, ptr_b);

    // Make ancestor the parent of a in the registry
    link(a_id, Some(ancestor_id));
    a.0.parent = Some(ancestor_id);

    let record_ancestor = GLOBAL_OBJECT_REGISTRY
        .read()
        .unwrap()
        .get(&ancestor_id)
        .unwrap()
        .clone();
    record_ancestor.children.write().unwrap().push(a_id);
    ancestor.0.children.push(a_id);

    // Poison ancestor's parent lock on a background thread
    let handle = std::thread::spawn({
        let record_ancestor = Arc::clone(&record_ancestor);
        move || {
            let _lock = record_ancestor.parent.write().unwrap();
            panic!("intentional ancestor parent lock poison");
        }
    });
    let _ = handle.join();
    assert!(record_ancestor.parent.is_poisoned());

    // Attempt swap between a and b:
    // would_create_cycle(b_id, a_id) will inspect a's parent chain, encountering ancestor's poisoned parent lock.
    // It must return Err(ReparentError::Busy) without panicking!
    let before = states(&[&a.0, &b.0, &ancestor.0]);
    let res = a
        .object_data_mut()
        .swap_hierarchy_and_ownership(&mut b.object_data_mut());
    assert_eq!(res, Err(ReparentError::Busy));
    assert_eq!(
        states(&[&a.0, &b.0, &ancestor.0]),
        before,
        "a refused swap changed something"
    );

    // SAFETY: `a`, `b` and `ancestor` are locals of this test, registered above and not moved
    // since; this thread is their registration thread and no callback of any runs. They drop
    // only at the end of the function, after this call.
    unsafe {
        unregister_qobject(a_id);
        unregister_qobject(b_id);
        unregister_qobject(ancestor_id);
    }
}

#[test]
fn test_swap_hierarchy_rejects_mismatched_parent_record() {
    struct Node(ObjectData);
    impl QObject for Node {
        fn object_data(&self) -> &ObjectData {
            &self.0
        }
        fn object_data_mut(&mut self) -> ObjectDataMut<'_> {
            ObjectDataMut::new(&mut self.0)
        }
    }

    let mut a = Node(ObjectData::new(ObjectId::next()));
    let mut b = Node(ObjectData::new(ObjectId::next()));
    let mut c = Node(ObjectData::new(ObjectId::next()));
    let a_id = a.0.id();
    let b_id = b.0.id();
    let c_id = c.0.id();

    let ptr_a = &raw mut a as *mut dyn QObject;
    register_object_metadata(&a.0, ptr_a);
    let ptr_b = &raw mut b as *mut dyn QObject;
    register_object_metadata(&b.0, ptr_b);
    let ptr_c = &raw mut c as *mut dyn QObject;
    register_object_metadata(&c.0, ptr_c);

    // Manually inject c_id into a's children, but leave c's registry parent as None
    a.0.children.push(c_id);
    {
        let reg = GLOBAL_OBJECT_REGISTRY.read().unwrap();
        let a_rec = reg.get(&a_id).unwrap();
        a_rec.children.write().unwrap().push(c_id);
    }

    // Registry parent of c is None, which mismatches Some(a_id)!
    let before = states(&[&a.0, &b.0, &c.0]);
    let res = a
        .object_data_mut()
        .swap_hierarchy_and_ownership(&mut b.object_data_mut());
    assert_eq!(res, Err(ReparentError::InvalidParent));
    assert_eq!(
        states(&[&a.0, &b.0, &c.0]),
        before,
        "a refused swap changed something"
    );

    // Cleanup
    a.0.children.retain(|&id| id != c_id);
    // SAFETY: `a`, `b` and `c` are locals of this test, registered above and not moved since;
    // no callback of any runs (none is delivered), and this is their registration thread.
    // They drop only at the end of the function, after this call.
    unsafe {
        unregister_qobject(c_id);
        unregister_qobject(b_id);
        unregister_qobject(a_id);
    }
}

/// The lists the two objects keep are compared with the registry's under the write locks of the
/// two parents: a registry list that names a child the object's own list lacks is refused, and
/// nothing changes (no list is written over).
#[test]
fn test_swap_refuses_a_registry_list_that_disagrees_with_the_objects_own() {
    // SAFETY: the objects are owned here and dropped at the end, on this thread.
    let mut a = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
    // SAFETY: as for `a` above: `b` is owned here and dropped at the end, on this thread.
    let mut b = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
    let (a_id, b_id) = (OwnedObject::object_id(&a), OwnedObject::object_id(&b));
    let listed_only_in_the_registry = ObjectId::next();
    record(a_id)
        .children
        .write()
        .unwrap()
        .push(listed_only_in_the_registry);
    let before = a
        .with(|a| b.with(|b| states(&[&a.0, &b.0])).unwrap())
        .unwrap();

    let res = a
        .with_mut(|a| {
            b.with_mut(|b| {
                a.object_data_mut()
                    .swap_hierarchy_and_ownership(&mut b.object_data_mut())
            })
            .unwrap()
        })
        .unwrap();

    assert_eq!(res, Err(ReparentError::InvalidParent));
    assert_eq!(
        a.with(|a| b.with(|b| states(&[&a.0, &b.0])).unwrap())
            .unwrap(),
        before,
        "a refused swap changed something"
    );
    assert_eq!(
        query_object_children(a_id),
        Some(vec![listed_only_in_the_registry])
    );
    assert_eq!(query_object_children(b_id), Some(Vec::new()));
    record(a_id).children.write().unwrap().clear();
}

#[test]
fn test_set_parent_handles_poisoned_parent_lock() {
    struct Node(ObjectData);
    impl QObject for Node {
        fn object_data(&self) -> &ObjectData {
            &self.0
        }
        fn object_data_mut(&mut self) -> ObjectDataMut<'_> {
            ObjectDataMut::new(&mut self.0)
        }
    }

    let mut parent = Node(ObjectData::new(ObjectId::next()));
    let mut child = Node(ObjectData::new(ObjectId::next()));
    let parent_id = parent.0.id();
    let child_id = child.0.id();

    let ptr_p = &raw mut parent as *mut dyn QObject;
    register_object_metadata(&parent.0, ptr_p);
    let ptr_c = &raw mut child as *mut dyn QObject;
    register_object_metadata(&child.0, ptr_c);

    let record_p = GLOBAL_OBJECT_REGISTRY
        .read()
        .unwrap()
        .get(&parent_id)
        .unwrap()
        .clone();
    let handle = std::thread::spawn({
        let record_p = Arc::clone(&record_p);
        move || {
            let _lock = record_p.children.write().unwrap();
            panic!("poison parent children lock");
        }
    });
    let _ = handle.join();
    assert!(record_p.children.is_poisoned());

    // Attempt set_parent - must fail with Busy, child must stay parentless
    let before = states(&[&child.0, &parent.0]);
    let res = set_parent(&mut child.0, Some(parent_id));
    assert_eq!(res, Err(ReparentError::Busy));
    assert_eq!(
        states(&[&child.0, &parent.0]),
        before,
        "a refused set_parent changed something"
    );
    assert_eq!(child.0.parent(), None);

    // SAFETY: `child` and `parent` are locals of this test, registered above and not moved
    // since; no callback of either runs, and this is their registration thread. They drop only
    // at the end of the function, after this call.
    unsafe {
        unregister_qobject(child_id);
        unregister_qobject(parent_id);
    }
}

#[test]
fn test_reparent_owned_handles_poisoned_parent_lock() {
    struct Node(ObjectData);
    impl QObject for Node {
        fn object_data(&self) -> &ObjectData {
            &self.0
        }
        fn object_data_mut(&mut self) -> ObjectDataMut<'_> {
            ObjectDataMut::new(&mut self.0)
        }
    }

    let mut p1 = Node(ObjectData::new(ObjectId::next()));
    let mut p2 = Node(ObjectData::new(ObjectId::next()));
    let p1_id = p1.0.id();
    let p2_id = p2.0.id();

    let ptr_p1 = &raw mut p1 as *mut dyn QObject;
    register_object_metadata(&p1.0, ptr_p1);
    let ptr_p2 = &raw mut p2 as *mut dyn QObject;
    register_object_metadata(&p2.0, ptr_p2);

    let child_box = Box::new(Node(ObjectData::new(ObjectId::next())));
    // SAFETY: `child_box` is moved into the call and not used again, so the parent's owner is
    // the only way to reach the child, and it keeps the child at its heap address. No callback
    // of the child runs, and this is the registration thread of `p1`.
    let child_id = unsafe {
        add_owned_child(p1_id, child_box)
            .map_err(|(e, _)| e)
            .unwrap()
    };
    // Poison p2's children lock
    let record_p2 = GLOBAL_OBJECT_REGISTRY
        .read()
        .unwrap()
        .get(&p2_id)
        .unwrap()
        .clone();
    let handle = std::thread::spawn({
        let record_p2 = Arc::clone(&record_p2);
        move || {
            let _lock = record_p2.children.write().unwrap();
            panic!("poison p2 children lock");
        }
    });
    let _ = handle.join();
    assert!(record_p2.children.is_poisoned());

    // Attempt reparent_owned_object to p2 - must fail with Busy, p1 must still own child
    let before = p1.0.owned_children[0]
        .with(|c| states(&[&p1.0, &p2.0, c.object_data()]))
        .unwrap();
    let res = reparent_owned_object(child_id, Some(p2_id));
    assert!(matches!(res, Err(ReparentError::Busy)));
    assert_eq!(
        p1.0.owned_children[0]
            .with(|c| states(&[&p1.0, &p2.0, c.object_data()]))
            .unwrap(),
        before,
        "a refused reparent changed something"
    );
    assert_eq!(query_object_parent(child_id), Some(Some(p1_id)));
    assert!(
        p1.0.owned_children
            .iter()
            .any(|c| OwnedObject::object_id(c) == child_id)
    );

    // SAFETY: `p1` and `p2` are locals of this test, registered above and not moved since; no
    // callback of either runs, and this is their registration thread. `p1` still owns the
    // child, which is freed when `p1` drops at the end of the function, after this call.
    unsafe {
        unregister_qobject(p2_id);
        unregister_qobject(p1_id);
    }
}

#[test]
fn test_reparent_owned_rejects_when_child_borrowed() {
    struct Node(ObjectData);
    impl QObject for Node {
        fn object_data(&self) -> &ObjectData {
            &self.0
        }
        fn object_data_mut(&mut self) -> ObjectDataMut<'_> {
            ObjectDataMut::new(&mut self.0)
        }
    }

    let mut p1 = Node(ObjectData::new(ObjectId::next()));
    let mut p2 = Node(ObjectData::new(ObjectId::next()));
    let p1_id = p1.0.id();
    let p2_id = p2.0.id();

    let ptr_p1 = &raw mut p1 as *mut dyn QObject;
    register_object_metadata(&p1.0, ptr_p1);
    let ptr_p2 = &raw mut p2 as *mut dyn QObject;
    register_object_metadata(&p2.0, ptr_p2);

    let child_box = Box::new(Node(ObjectData::new(ObjectId::next())));
    // SAFETY: `child_box` is moved into the call and not used again, so the parent's owner is
    // the only way to reach the child, and it keeps the child at its heap address. No callback
    // of the child runs yet, and this is the registration thread of `p1`.
    let child_id = unsafe {
        add_owned_child(p1_id, child_box)
            .map_err(|(e, _)| e)
            .unwrap()
    };
    // The child's state is read before it is borrowed, and again after the borrow is
    // released: while a callback of it runs, nothing may read it.
    let child_before = p1.0.owned_children[0]
        .with(|c| states(&[c.object_data()]))
        .expect("the child is idle");
    // Borrow child exclusively via borrow_registered
    let child_guard = borrow_registered(child_id).unwrap();

    // Attempt reparent - must reject with Busy
    // The child is borrowed, so only the parents' state can be read here.
    let before = states(&[&p1.0, &p2.0]);
    let res = reparent_owned_object(child_id, Some(p2_id));
    assert!(matches!(res, Err(ReparentError::Busy)));
    assert_eq!(
        states(&[&p1.0, &p2.0]),
        before,
        "a refused reparent changed something"
    );
    drop(child_guard);
    assert_eq!(
        p1.0.owned_children[0].with(|c| states(&[c.object_data()])),
        Some(child_before),
        "a refused reparent changed the child"
    );

    assert_eq!(query_object_parent(child_id), Some(Some(p1_id)));
    assert!(
        p1.0.owned_children
            .iter()
            .any(|c| OwnedObject::object_id(c) == child_id)
    );

    // SAFETY: `p1` and `p2` are locals of this test, registered above and not moved since; the
    // only borrow of a child, `child_guard`, was dropped above, and no callback of either
    // parent runs. This is their registration thread, and they drop after this call.
    unsafe {
        unregister_qobject(p2_id);
        unregister_qobject(p1_id);
    }
}

use std::cell::Cell;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

struct Plain(ObjectData);

impl QObject for Plain {
    fn object_data(&self) -> &ObjectData {
        &self.0
    }
    fn object_data_mut(&mut self) -> ObjectDataMut<'_> {
        ObjectDataMut::new(&mut self.0)
    }
}

/// A swap with an object that is really registered on another thread (moved with
/// `move_to_thread_by_id`) is refused, and nothing changes. The owner cannot lend such an
/// object (its `with_mut` refuses), so the data handle is taken unchecked: no callback can run
/// on the thread the object was moved to, which has ended.
#[test]
fn test_swap_with_an_object_registered_on_another_thread_is_refused() {
    let _serial = serial();
    // SAFETY: `a` is owned here and dropped at the end of the test, on this thread.
    let mut a = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
    // SAFETY: `Plain` holds nothing but its `ObjectData`, so `b` may change thread.
    let b_data = unsafe { ObjectData::new(ObjectId::next()).with_thread_moves_allowed() };
    // SAFETY: `b` is owned here and dropped at the end of the test, on this thread.
    let mut b = unsafe { register_owned_qobject(Plain(b_data)) };
    let (a_id, b_id) = (OwnedObject::object_id(&a), OwnedObject::object_id(&b));
    let child = Box::new(Plain(ObjectData::new(ObjectId::next())));
    let child_id_attached = child.0.id();
    // SAFETY: `child` is moved into the call and not used again; `a` owns it at its heap
    // address. No callback of the child runs, and `a` is registered on this thread.
    unsafe { add_owned_child(a_id, child) }
        .map_err(|(e, _)| e)
        .expect("attach a child to a");

    let remote = std::thread::spawn(ThreadId::current).join().unwrap();
    assert_eq!(
        super::super::thread::move_to_thread_by_id(b_id, remote, ThreadId::current()),
        Ok(())
    );
    assert_eq!(query_object_thread(b_id), Some(remote));
    assert_eq!(
        b.with_mut(|_| ()),
        None,
        "the owner does not reach an object that lives elsewhere"
    );

    // SAFETY: the thread `b` was moved to has ended, so no callback of it can run, and nothing
    // else holds a reference to it.
    let mut b_data = unsafe { b.as_mut_unchecked() }.object_data_mut();
    let res = a
        .with_mut(|a| {
            a.object_data_mut()
                .swap_hierarchy_and_ownership(&mut b_data)
        })
        .unwrap();
    assert_eq!(res, Err(ReparentError::DifferentThread));

    assert_eq!(query_object_parent(child_id_attached), Some(Some(a_id)));
    assert!(
        query_object_children(a_id)
            .unwrap()
            .contains(&child_id_attached)
    );
    assert!(query_object_children(b_id).unwrap().is_empty());
    assert_eq!(a.with(|a| a.object_data().owned_children().len()), Some(1));
    drop(a);
    // `b` is registered on a thread that has ended, which can no longer run the destruction
    // forwarded to it: the owner's drop destroys it here instead.
    drop(b);
    assert_eq!(
        query_object_thread(b_id),
        None,
        "the drop unregistered the object"
    );
}

/// `set_parent` on the data of an owned [`Plain`]. `set_parent` takes the child's borrow itself
/// (and refuses with `Busy` when it is held), so it cannot run inside `with_mut`.
fn set_owned_parent(
    owner: &mut OwnedObject<Plain>,
    parent: Option<ObjectId>,
) -> Result<(), ReparentError> {
    // SAFETY: the reference is only passed to `set_parent`, which takes the object's borrow
    // flag before it touches anything else; no callback of the object runs before that, and
    // none of the callers' objects is borrowed while it is called.
    let data = &mut unsafe { owner.as_mut_unchecked() }.0;
    set_parent(data, parent)
}

/// A child event that a [`Handler`] got, with what the receiver's own `children()` and the
/// registry's parent of the child were at that moment.
#[derive(Debug, Clone, PartialEq)]
struct Delivered {
    receiver: ObjectId,
    what: &'static str,
    child: ObjectId,
    own_children: Vec<ObjectId>,
    registry_parent: Option<Option<ObjectId>>,
}

type Log = Rc<RefCell<Vec<Delivered>>>;
type OnChildEvent = Box<dyn FnMut(&Delivered)>;

/// Object that logs the child events it gets and runs a hook on each, to see what a handler can
/// do and see while a hierarchy change is being finished.
struct Handler {
    data: ObjectData,
    log: Log,
    on_event: Option<OnChildEvent>,
}

impl Handler {
    fn new(log: &Log, on_event: Option<OnChildEvent>) -> Self {
        Self {
            data: ObjectData::new(ObjectId::next()),
            log: Rc::clone(log),
            on_event,
        }
    }
}

impl QObject for Handler {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ObjectDataMut<'_> {
        ObjectDataMut::new(&mut self.data)
    }
    fn event(&mut self, event: &mut Event) -> bool {
        let (what, child) = match &event.kind {
            EventKind::ChildAdded { child_id } => ("added", *child_id),
            EventKind::ChildRemoved { child_id } => ("removed", *child_id),
            _ => return true,
        };
        let delivered = Delivered {
            receiver: self.data.id(),
            what,
            child,
            own_children: self.data.children().to_vec(),
            registry_parent: query_object_parent(child),
        };
        self.log.borrow_mut().push(delivered.clone());
        if let Some(hook) = self.on_event.as_mut() {
            hook(&delivered);
        }
        true
    }
}

/// Tests that hold the registry or `HIERARCHY_MUTATION` for a while, or probe them, take this
/// one, so that they do not meet.
fn serial() -> MutexGuard<'static, ()> {
    static SERIAL: Mutex<()> = Mutex::new(());
    lock_unpoisoned(&SERIAL)
}

const PROBE_WINDOW: Duration = Duration::from_secs(5);

/// How long a test lets a thread that must be blocked try to get on. Only a defective version
/// gets on; the outcome that is checked afterwards does not depend on this time.
fn settle() -> Duration {
    Duration::from_millis(if cfg!(miri) { 2000 } else { 300 })
}

/// Whether `probe` succeeds once within [`PROBE_WINDOW`]. Another test may hold a global lock
/// for an instant, so a failed probe is tried again; a lock that the calling thread holds fails
/// it for the whole window.
fn free_within(mut probe: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + PROBE_WINDOW;
    loop {
        if probe() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn record(id: ObjectId) -> Arc<ObjectRecord> {
    Arc::clone(&GLOBAL_OBJECT_REGISTRY.read().unwrap()[&id])
}

/// Sets the `parent` link of a registered object, as a commit does (the index follows).
fn link(id: ObjectId, parent: Option<ObjectId>) {
    let registry = read_unpoisoned(&GLOBAL_OBJECT_REGISTRY);
    let record = &registry[&id];
    store_link(
        &registry,
        record,
        &mut write_unpoisoned(&record.parent),
        parent,
    );
}

/// None of the locks of a hierarchy change is held by the calling thread (a handler runs on
/// it): the lists and links of the given records, the registry, and `HIERARCHY_MUTATION`.
fn assert_hierarchy_unlocked(records: &[Arc<ObjectRecord>]) {
    for record in records {
        assert!(
            record.children.try_write().is_ok(),
            "the `children` lock of {:?} is held",
            record.id
        );
        assert!(
            record.parent.try_write().is_ok(),
            "the `parent` lock of {:?} is held",
            record.id
        );
    }
    assert!(
        free_within(|| GLOBAL_OBJECT_REGISTRY.try_write().is_ok()),
        "the registry is held"
    );
    assert!(
        free_within(|| HIERARCHY_MUTATION.try_lock().is_ok()),
        "`HIERARCHY_MUTATION` is held"
    );
}

/// What a hierarchy change that fails must leave as it was, for each object: its own links,
/// lists, ownership, and what the registry records for it with its borrow flag.
#[derive(Debug, PartialEq)]
struct State {
    id: ObjectId,
    own: (
        Option<ObjectId>,
        Option<ObjectId>,
        Vec<ObjectId>,
        Vec<ObjectId>,
        bool,
    ),
    registry: Option<(Option<ObjectId>, Vec<ObjectId>, bool, bool)>,
}

fn states(objects: &[&ObjectData]) -> Vec<State> {
    let registry = GLOBAL_OBJECT_REGISTRY.read().unwrap();
    objects
        .iter()
        .map(|data| State {
            id: data.id,
            own: (
                data.parent,
                data.parent_liveness.as_ref().map(|(id, _)| *id),
                data.children.clone(),
                data.owned_children
                    .iter()
                    .map(OwnedObject::object_id)
                    .collect(),
                data.owned_by_parent,
            ),
            registry: registry.get(&data.id).map(|r| {
                // The list (L1) before the link (L2), one lock at a time, like the commits.
                let children = read_unpoisoned(&r.children).clone();
                let parent = *read_unpoisoned(&r.parent);
                (
                    parent,
                    children,
                    r.borrow_state.borrow_flag.load(Ordering::Acquire),
                    r.liveness.load(Ordering::Acquire),
                )
            }),
        })
        .collect()
}

/// No object among `ids` is its own ancestor in the registry.
fn registry_has_no_cycle(ids: &[ObjectId]) -> bool {
    ids.iter().all(|&start| {
        let mut cursor = query_object_parent(start).flatten();
        for _ in 0..64 {
            match cursor {
                None => return true,
                Some(id) if id == start => return false,
                Some(id) => cursor = query_object_parent(id).flatten(),
            }
        }
        false
    })
}

/// Poisons a lock of `record` by panicking on another thread while holding it for writing.
fn poison(record: &Arc<ObjectRecord>, hold: fn(&ObjectRecord)) {
    let record = Arc::clone(record);
    let _ = std::thread::spawn(move || hold(&record)).join();
}

/// A thread that creates what it works on (`setup` runs on it, so its objects belong to it),
/// reports that it is ready, waits for `start`, runs `act` once and sends the result, and keeps
/// what it created alive until `finish`. Dropping a `Side` lets its thread go on and end.
struct Side<R> {
    ready: mpsc::Receiver<()>,
    go: mpsc::Sender<()>,
    result: mpsc::Receiver<R>,
    end: mpsc::Sender<()>,
    handle: std::thread::JoinHandle<()>,
}

fn side<S: 'static, R: Send + 'static>(
    setup: impl FnOnce() -> S + Send + 'static,
    act: impl FnOnce(&mut S) -> R + Send + 'static,
) -> Side<R> {
    let (ready_tx, ready) = mpsc::channel();
    let (go, go_rx) = mpsc::channel();
    let (result_tx, result) = mpsc::channel();
    let (end, end_rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        let mut state = setup();
        let _ = ready_tx.send(());
        if go_rx.recv().is_ok() {
            let _ = result_tx.send(act(&mut state));
        }
        let _ = end_rx.recv();
        drop(state);
    });
    Side {
        ready,
        go,
        result,
        end,
        handle,
    }
}

impl<R> Side<R> {
    fn wait_ready(&self) {
        self.ready.recv().expect("a thread did not get ready");
    }
    fn start(&self) {
        let _ = self.go.send(());
    }
    /// Ends the thread, whether it was started or not, and waits for it.
    fn finish(self) {
        let Side {
            go, end, handle, ..
        } = self;
        drop(go);
        let _ = end.send(());
        handle.join().expect("a thread panicked");
    }
}

/// Holds the next hierarchy operation of the calling thread at the first of `points` it
/// reaches: `reached` is signalled, and the operation goes on when `resume` gets a signal or
/// its sender is dropped.
fn hold_next_operation(
    reached: mpsc::Sender<()>,
    resume: mpsc::Receiver<()>,
    points: &[test_hooks::Point],
) {
    let resume = Rc::new(resume);
    let armed = Rc::new(Cell::new(true));
    for &point in points {
        let reached = reached.clone();
        let resume = Rc::clone(&resume);
        let armed = Rc::clone(&armed);
        test_hooks::on_next(point, move || {
            if armed.replace(false) {
                let _ = reached.send(());
                let _ = resume.recv();
            }
        });
    }
}

/// A handler of `ChildAdded` runs with no lock of the change held, and can register and drop
/// objects and change other links while `set_parent` delivers the event.
#[test]
fn test_set_parent_delivers_events_with_no_hierarchy_lock_held() {
    let _serial = serial();
    let log = Log::default();
    let records: Rc<RefCell<Vec<Arc<ObjectRecord>>>> = Rc::default();
    let ran = Rc::new(Cell::new(0usize));
    let hook: OnChildEvent = {
        let records = Rc::clone(&records);
        let ran = Rc::clone(&ran);
        Box::new(move |_| {
            assert_hierarchy_unlocked(&records.borrow());
            // SAFETY: the extra object is owned and dropped right here, on this thread.
            let extra = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
            let mut other = ObjectData::new(ObjectId::next());
            assert_eq!(
                set_parent(&mut other, Some(OwnedObject::object_id(&extra))),
                Ok(())
            );
            drop(other);
            drop(extra);
            ran.set(ran.get() + 1);
        })
    };
    // SAFETY: the parent is owned here and dropped at the end, on this thread.
    let parent = unsafe { register_owned_qobject(Handler::new(&log, Some(hook))) };
    let parent_id = OwnedObject::object_id(&parent);
    records.borrow_mut().push(record(parent_id));

    let mut child = ObjectData::new(ObjectId::next());
    assert_eq!(set_parent(&mut child, Some(parent_id)), Ok(()));
    assert_eq!(ran.get(), 1);
    assert_eq!(
        *log.borrow(),
        vec![Delivered {
            receiver: parent_id,
            what: "added",
            child: child.id(),
            own_children: vec![child.id()],
            registry_parent: None,
        }]
    );
    assert_eq!(child.parent(), Some(parent_id));
    assert_eq!(query_object_children(parent_id), Some(vec![child.id()]));
    drop(child);
    drop(parent);
}

/// A registered child moves between two parents. Both handlers run with every lock of the
/// change free, including the lists and links of all three records, and see the registry as
/// the finished change: `ChildRemoved` goes to the old parent, whose own list has lost the
/// child; `ChildAdded` to the new one, whose own list has it.
#[test]
fn test_set_parent_of_a_registered_child_delivers_events_in_order_with_no_lock_held() {
    let _serial = serial();
    let log = Log::default();
    let records: Rc<RefCell<Vec<Arc<ObjectRecord>>>> = Rc::default();
    let probe = || -> OnChildEvent {
        let records = Rc::clone(&records);
        Box::new(move |_| assert_hierarchy_unlocked(&records.borrow()))
    };
    // SAFETY: the objects are owned here and dropped at the end, on this thread.
    let p1 = unsafe { register_owned_qobject(Handler::new(&log, Some(probe()))) };
    // SAFETY: as above: `p2` is owned here and dropped at the end of the test, on this thread.
    let p2 = unsafe { register_owned_qobject(Handler::new(&log, Some(probe()))) };
    // SAFETY: as above: `child` is owned here and dropped at the end of the test, on this thread.
    let mut child = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
    let (p1_id, p2_id, child_id) = (
        OwnedObject::object_id(&p1),
        OwnedObject::object_id(&p2),
        OwnedObject::object_id(&child),
    );
    records
        .borrow_mut()
        .extend([record(p1_id), record(p2_id), record(child_id)]);

    assert_eq!(set_owned_parent(&mut child, Some(p1_id)), Ok(()));
    assert_eq!(set_owned_parent(&mut child, Some(p2_id)), Ok(()));

    let delivered = |receiver, what, own_children, parent| Delivered {
        receiver,
        what,
        child: child_id,
        own_children,
        registry_parent: Some(Some(parent)),
    };
    assert_eq!(
        *log.borrow(),
        vec![
            delivered(p1_id, "added", vec![child_id], p1_id),
            delivered(p1_id, "removed", vec![], p2_id),
            delivered(p2_id, "added", vec![child_id], p2_id),
        ]
    );
    assert_eq!(child.with(|c| c.0.parent()).unwrap(), Some(p2_id));
    assert_eq!(query_object_parent(child_id), Some(Some(p2_id)));
    assert_eq!(query_object_children(p1_id), Some(Vec::new()));
    assert_eq!(query_object_children(p2_id), Some(vec![child_id]));
    drop(child);
    drop(p2);
    drop(p1);
}

#[test]
fn test_set_parent_rejects_reentrant_borrow_during_callbacks() {
    let _serial = serial();
    let log = Log::default();
    let attempted_reborrow_success = Rc::new(Cell::new(false));
    let attempted_clone = Rc::clone(&attempted_reborrow_success);
    let child_id_cell = Rc::new(Cell::new(ObjectId(0)));
    let child_id_clone = Rc::clone(&child_id_cell);

    let probe: OnChildEvent = Box::new(move |delivered| {
        if delivered.what == "removed" {
            let cid = child_id_clone.get();
            // Attempting to re-acquire mutable access to child via the registry during ChildRemoved
            // must be rejected because short_term_borrow is held across the callback.
            if with_object_mut(cid, |_| ()).is_some() {
                attempted_clone.set(true);
            }
        }
    });

    // SAFETY: `p1`, `p2` and `child` are owned here and dropped at the end of the test, on this
    // thread; no callback of them runs while the owners use them.
    let p1 = unsafe { register_owned_qobject(Handler::new(&log, Some(probe))) };
    // SAFETY: as above for `p2`.
    let p2 = unsafe { register_owned_qobject(Handler::new(&log, None)) };
    // SAFETY: as above for `child`.
    let mut child = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
    let (p1_id, p2_id, child_id) = (
        OwnedObject::object_id(&p1),
        OwnedObject::object_id(&p2),
        OwnedObject::object_id(&child),
    );
    child_id_cell.set(child_id);

    assert_eq!(set_owned_parent(&mut child, Some(p1_id)), Ok(()));
    assert_eq!(set_owned_parent(&mut child, Some(p2_id)), Ok(()));

    assert!(
        !attempted_reborrow_success.get(),
        "reentrant mutable borrow via registry must be rejected during ChildRemoved callback"
    );
    drop(child);
    drop(p2);
    drop(p1);
}

/// A panic in the old parent's `ChildRemoved` handler does not leave the objects behind the
/// registry: the child's parent and the new parent's list are brought in line first, `ChildAdded`
/// is not sent, and the panic then continues. Nothing stays locked or borrowed.
#[test]
fn test_set_parent_finishes_the_change_when_the_old_parents_handler_panics() {
    let _serial = serial();
    let log = Log::default();
    let panicking: OnChildEvent = Box::new(|delivered| {
        if delivered.what == "removed" {
            panic!("handler of the old parent panics");
        }
    });
    // SAFETY: the objects are owned here and dropped at the end, on this thread.
    let p1 = unsafe { register_owned_qobject(Handler::new(&log, Some(panicking))) };
    // SAFETY: as above for `p2`.
    let p2 = unsafe { register_owned_qobject(Handler::new(&log, None)) };
    // SAFETY: as above for `child`.
    let mut child = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
    let (p1_id, p2_id, child_id) = (
        OwnedObject::object_id(&p1),
        OwnedObject::object_id(&p2),
        OwnedObject::object_id(&child),
    );
    assert_eq!(set_owned_parent(&mut child, Some(p1_id)), Ok(()));

    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        set_owned_parent(&mut child, Some(p2_id))
    }));
    let payload =
        outcome.expect_err("the panic of the handler continues after the change is finished");
    assert_eq!(
        payload.downcast_ref::<&str>(),
        Some(&"handler of the old parent panics")
    );

    assert_eq!(child.with(|c| c.0.parent()).unwrap(), Some(p2_id));
    assert_eq!(query_object_parent(child_id), Some(Some(p2_id)));
    assert_eq!(query_object_children(p1_id), Some(Vec::new()));
    assert_eq!(query_object_children(p2_id), Some(vec![child_id]));
    assert_eq!(
        with_object(p2_id, |p| p.object_data().children().to_vec()),
        Some(vec![child_id])
    );
    assert!(
        !log.borrow().iter().any(|d| d.receiver == p2_id),
        "`ChildAdded` was sent after a panic"
    );

    let records = [record(p1_id), record(p2_id), record(child_id)];
    assert_hierarchy_unlocked(&records);
    for record in &records {
        assert!(
            !record.borrow_state.borrow_flag.load(Ordering::Acquire),
            "{:?} stays borrowed",
            record.id
        );
    }
    assert_eq!(
        set_owned_parent(&mut child, None),
        Ok(()),
        "the child cannot be moved again"
    );
    drop(child);
    drop(p2);
    drop(p1);
}
/// When the old parent's ChildRemoved handler panics during set_parent_by_id,
/// the child's local parent and the new parent's children list are still brought
/// in line with the registry before the panic resumes, and ChildAdded is suppressed.
#[test]
fn test_set_parent_by_id_finishes_the_change_when_the_old_parents_handler_panics() {
    let _serial = serial();
    let log = Log::default();
    let panicking: OnChildEvent = Box::new(|delivered| {
        if delivered.what == "removed" {
            panic!("handler of the old parent panics during set_parent_by_id");
        }
    });
    // SAFETY: `p1`, `p2` and `child` are owned here and dropped at the end of the test, on this
    // thread; no callback of them runs while the owners use them.
    let p1 = unsafe { register_owned_qobject(Handler::new(&log, Some(panicking))) };
    // SAFETY: as above for `p2`.
    let p2 = unsafe { register_owned_qobject(Handler::new(&log, None)) };
    // SAFETY: as above for `child`.
    let child = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
    let (p1_id, p2_id, child_id) = (
        OwnedObject::object_id(&p1),
        OwnedObject::object_id(&p2),
        OwnedObject::object_id(&child),
    );
    assert_eq!(set_parent_by_id(child_id, Some(p1_id)), Ok(()));

    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        set_parent_by_id(child_id, Some(p2_id))
    }));
    let payload =
        outcome.expect_err("the panic of the handler must resume after the change is finished");
    assert_eq!(
        payload.downcast_ref::<&str>(),
        Some(&"handler of the old parent panics during set_parent_by_id")
    );

    // Strict consistency checks:
    assert_eq!(query_object_parent(child_id), Some(Some(p2_id)));
    assert_eq!(query_object_children(p1_id), Some(Vec::new()));
    assert_eq!(query_object_children(p2_id), Some(vec![child_id]));

    assert_eq!(
        with_object(child_id, |c| c.object_data().parent()),
        Some(Some(p2_id))
    );
    assert_eq!(
        with_object(p1_id, |p| p.object_data().children().to_vec()),
        Some(Vec::new())
    );
    assert_eq!(
        with_object(p2_id, |p| p.object_data().children().to_vec()),
        Some(vec![child_id])
    );
    assert!(
        !log.borrow().iter().any(|d| d.receiver == p2_id),
        "`ChildAdded` was sent after a panic"
    );

    let records = [record(p1_id), record(p2_id), record(child_id)];
    assert_hierarchy_unlocked(&records);
    for record in &records {
        assert!(
            !record.borrow_state.borrow_flag.load(Ordering::Acquire),
            "{:?} stays borrowed",
            record.id
        );
        assert!(
            !record.reparenting.load(Ordering::Acquire),
            "{:?} stays reparenting",
            record.id
        );
    }

    drop(child);
    drop(p2);
    drop(p1);
}
/// When a registered child is reparented, the old parent's `ChildRemoved` handler can
/// reentrantly access or mutate the child via `with_object_mut`. All locks and mutable
/// borrows of the child must be released before callbacks execute, and the final state
/// of the registry, child's parent link, and parents' children lists must remain consistent.
#[test]
fn test_set_parent_reentrant_mutation_during_child_removed() {
    let _serial = serial();
    let log = Log::default();
    let reentrant_mutated = Rc::new(Cell::new(false));
    let reentrant_mutated_clone = Rc::clone(&reentrant_mutated);
    let nested_busy_rejected = Rc::new(Cell::new(false));
    let nested_busy_rejected_clone = Rc::clone(&nested_busy_rejected);
    let p3_id_cell = Rc::new(Cell::new(None));
    let p3_id_clone = Rc::clone(&p3_id_cell);

    // The hook runs during `ChildRemoved` on the old parent.
    let hook: OnChildEvent = Box::new(move |delivered| {
        if delivered.what == "removed" {
            let cid = delivered.child;
            if let Some(target_p3) = p3_id_clone.get() {
                let res = set_parent_by_id(cid, Some(target_p3));
                assert_eq!(res, Err(ReparentError::Busy));
                nested_busy_rejected_clone.set(true);
            }
            // Verify with_object_mut succeeds reentrantly without aliasing or deadlock.
            let mutated = with_object_mut(cid, |c| {
                let mut data = c.object_data_mut();
                data.set_object_name("reentrantly_mutated_child");
                true
            });
            if mutated == Some(true) {
                reentrant_mutated_clone.set(true);
            }
        }
    });

    // SAFETY: registered on current thread and owned.
    let p1 = unsafe { register_owned_qobject(Handler::new(&log, Some(hook))) };
    // SAFETY: as above for `p2`.
    let p2 = unsafe { register_owned_qobject(Handler::new(&log, None)) };
    // SAFETY: as above for `p3`.
    let p3 = unsafe { register_owned_qobject(Handler::new(&log, None)) };
    // SAFETY: as above for `child`.
    let child = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
    let (p1_id, p2_id, p3_id, child_id) = (
        OwnedObject::object_id(&p1),
        OwnedObject::object_id(&p2),
        OwnedObject::object_id(&p3),
        OwnedObject::object_id(&child),
    );
    p3_id_cell.set(Some(p3_id));

    assert_eq!(set_parent_by_id(child_id, Some(p1_id)), Ok(()));
    assert_eq!(set_parent_by_id(child_id, Some(p2_id)), Ok(()));
    assert!(
        nested_busy_rejected.get(),
        "nested set_parent_by_id must be rejected with Busy"
    );
    assert!(
        reentrant_mutated.get(),
        "with_object_mut must run during ChildRemoved"
    );

    // Assert that final registry, parent, and children states remain consistent.
    assert_eq!(query_object_parent(child_id), Some(Some(p2_id)));
    assert_eq!(query_object_children(p1_id), Some(Vec::new()));
    assert_eq!(query_object_children(p2_id), Some(vec![child_id]));
    assert_eq!(query_object_children(p3_id), Some(Vec::new()));

    // Assert child local state through with_object and through owned object
    assert_eq!(
        with_object(child_id, |c| c.object_data().parent()),
        Some(Some(p2_id))
    );
    assert_eq!(
        with_object(child_id, |c| c
            .object_data()
            .object_name()
            .map(|s| s.to_string())),
        Some(Some("reentrantly_mutated_child".to_string()))
    );
    assert_eq!(
        with_object(p1_id, |p| p.object_data().children().to_vec()),
        Some(Vec::new())
    );
    assert_eq!(
        with_object(p2_id, |p| p.object_data().children().to_vec()),
        Some(vec![child_id])
    );
    assert_eq!(
        with_object(p3_id, |p| p.object_data().children().to_vec()),
        Some(Vec::new())
    );

    drop(child);
    drop(p3);
    drop(p2);
    drop(p1);
}

/// When set_parent_by_id completes its registry and hierarchy commit while the child object
/// is actively borrowed by a callback that retained a borrow guard across the event,
/// post-commit sync must NOT return Busy or drop the update silently. Instead, the sync
/// is recorded as pending and deterministically applied when that borrow guard drops.
#[test]
fn test_set_parent_by_id_post_commit_sync_deferred_when_child_borrow_held() {
    let _serial = serial();
    let log = Log::default();
    let held_guard: Rc<RefCell<Option<ObjectBorrowGuard>>> = Rc::new(RefCell::new(None));
    let held_guard_clone = Rc::clone(&held_guard);

    let hook: OnChildEvent = Box::new(move |delivered| {
        if delivered.what == "removed" {
            // Borrow the child during ChildRemoved and retain the guard
            let guard = borrow_registered(delivered.child)
                .expect("child should be borrowable during callback");
            *held_guard_clone.borrow_mut() = Some(guard);
        }
    });

    // SAFETY: `p1`, `p2` and `child` are owned here and dropped at the end of the test, on this
    // thread; no callback of them runs while the owners use them.
    let p1 = unsafe { register_owned_qobject(Handler::new(&log, Some(hook))) };
    // SAFETY: as above for `p2`.
    let p2 = unsafe { register_owned_qobject(Handler::new(&log, None)) };
    // SAFETY: as above for `child`.
    let child = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
    let (p1_id, p2_id, child_id) = (
        OwnedObject::object_id(&p1),
        OwnedObject::object_id(&p2),
        OwnedObject::object_id(&child),
    );

    assert_eq!(set_parent_by_id(child_id, Some(p1_id)), Ok(()));
    assert_eq!(query_object_parent(child_id), Some(Some(p1_id)));
    assert_eq!(query_pending_parent_sync(child_id), Some(false));

    // Now reparent to p2. ChildRemoved on p1 will hold child's borrow guard across return.
    assert_eq!(set_parent_by_id(child_id, Some(p2_id)), Ok(()));

    // Registry and hierarchy committed successfully
    assert_eq!(query_object_parent(child_id), Some(Some(p2_id)));
    assert_eq!(query_object_children(p1_id), Some(Vec::new()));
    assert_eq!(query_object_children(p2_id), Some(vec![child_id]));

    // Post-commit sync could not directly borrow child, so it was deferred
    assert_eq!(query_pending_parent_sync(child_id), Some(true));

    // Release the held borrow guard: ObjectBorrowGuard::drop must apply the pending sync!
    drop(held_guard.borrow_mut().take());

    // Now pending parent sync has been applied
    assert_eq!(query_pending_parent_sync(child_id), Some(false));
    assert_eq!(
        with_object(child_id, |c| c.object_data().parent()),
        Some(Some(p2_id))
    );

    drop(child);
    drop(p2);
    drop(p1);
}
/// When a ChildRemoved handler retains an ObjectBorrowGuard on the child and panics,
/// sync_child_local_parent records PostCommitSyncStatus::DeferredToPending before resume_unwind.
/// Prior to resume_unwind, registry and hierarchy lists are fully repaired and ChildAdded
/// is suppressed, while local parent synchronization remains pending and completes deterministically
/// upon release of that ObjectBorrowGuard.
#[test]
fn test_set_parent_by_id_panic_with_retained_child_borrow_defers_local_sync_until_guard_drop() {
    let _serial = serial();
    let log = Log::default();
    let held_guard: Rc<RefCell<Option<ObjectBorrowGuard>>> = Rc::new(RefCell::new(None));
    let held_guard_clone = Rc::clone(&held_guard);

    let hook: OnChildEvent = Box::new(move |delivered| {
        if delivered.what == "removed" {
            // Retain child borrow externally
            let guard = borrow_registered(delivered.child)
                .expect("child should be borrowable during callback");
            *held_guard_clone.borrow_mut() = Some(guard);
            // Then panic in the handler
            panic!("handler panics after retaining child borrow");
        }
    });

    // SAFETY: `p1`, `p2` and `child` are owned here and dropped at the end of the test, on this
    // thread; no callback of them runs while the owners use them.
    let p1 = unsafe { register_owned_qobject(Handler::new(&log, Some(hook))) };
    // SAFETY: as above for `p2`.
    let p2 = unsafe { register_owned_qobject(Handler::new(&log, None)) };
    // SAFETY: as above for `child`.
    let child = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
    let (p1_id, p2_id, child_id) = (
        OwnedObject::object_id(&p1),
        OwnedObject::object_id(&p2),
        OwnedObject::object_id(&child),
    );

    assert_eq!(set_parent_by_id(child_id, Some(p1_id)), Ok(()));
    assert_eq!(query_object_parent(child_id), Some(Some(p1_id)));
    assert_eq!(query_pending_parent_sync(child_id), Some(false));

    // Invoke set_parent_by_id, which must panic and propagate resume_unwind
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        set_parent_by_id(child_id, Some(p2_id))
    }));
    let payload = outcome.expect_err("handler panic must resume unwind");
    assert_eq!(
        payload.downcast_ref::<&str>(),
        Some(&"handler panics after retaining child borrow")
    );

    // Verification PRIOR to dropping held_guard:
    // 1. Global registry and hierarchy lists are repaired and consistent:
    assert_eq!(query_object_parent(child_id), Some(Some(p2_id)));
    assert_eq!(query_object_children(p1_id), Some(Vec::new()));
    assert_eq!(query_object_children(p2_id), Some(vec![child_id]));

    // 2. ChildAdded was suppressed on panic:
    assert!(
        !log.borrow().iter().any(|d| d.receiver == p2_id),
        "`ChildAdded` must be suppressed on panic"
    );

    // 3. Local parent sync remains pending because ObjectBorrowGuard was held across the panic:
    assert_eq!(query_pending_parent_sync(child_id), Some(true));

    // 4. Reparenting flag on child is released by ReparentGuard drop during unwind:
    let records = [record(p1_id), record(p2_id), record(child_id)];
    assert_hierarchy_unlocked(&records);
    assert!(
        !records[2].reparenting.load(Ordering::Acquire),
        "child reparenting flag must be released"
    );

    // Now drop the retained ObjectBorrowGuard:
    drop(held_guard.borrow_mut().take());

    // Verification AFTER dropping held_guard:
    // 1. Pending sync is applied deterministically:
    assert_eq!(query_pending_parent_sync(child_id), Some(false));
    // 2. Local child parent is now in complete agreement with the registry:
    assert_eq!(
        with_object(child_id, |c| c.object_data().parent()),
        Some(Some(p2_id))
    );

    drop(child);
    drop(p2);
    drop(p1);
}
/// When set_parent_by_id is invoked while the child is actively borrowed via with_object_mut,
/// it must be rejected with ReparentError::Busy and perform zero state mutation.
#[test]
fn test_set_parent_by_id_rejected_when_child_is_borrowed() {
    let _serial = serial();
    let log = Log::default();
    // SAFETY: `p1`, `p2` and `child` are owned here and dropped at the end of the test, on this
    // thread; no callback of them runs while the owners use them.
    let p1 = unsafe { register_owned_qobject(Handler::new(&log, None)) };
    // SAFETY: as above for `p2`.
    let p2 = unsafe { register_owned_qobject(Handler::new(&log, None)) };
    // SAFETY: as above for `child`.
    let child = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
    let (p1_id, p2_id, child_id) = (
        OwnedObject::object_id(&p1),
        OwnedObject::object_id(&p2),
        OwnedObject::object_id(&child),
    );
    assert_eq!(set_parent_by_id(child_id, Some(p1_id)), Ok(()));

    // Call set_parent_by_id inside with_object_mut(child_id, ...);
    // assert Err(ReparentError::Busy) and zero state mutation.
    let nested_res = with_object_mut(child_id, |_| set_parent_by_id(child_id, Some(p2_id)));
    assert_eq!(nested_res, Some(Err(ReparentError::Busy)));

    // Verify zero state mutation:
    assert_eq!(query_object_parent(child_id), Some(Some(p1_id)));
    assert_eq!(query_object_children(p1_id), Some(vec![child_id]));
    assert_eq!(query_object_children(p2_id), Some(Vec::new()));
    assert_eq!(
        with_object(child_id, |c| c.object_data().parent()),
        Some(Some(p1_id))
    );
    assert_eq!(
        with_object(p1_id, |p| p.object_data().children().to_vec()),
        Some(vec![child_id])
    );
    assert_eq!(
        with_object(p2_id, |p| p.object_data().children().to_vec()),
        Some(Vec::new())
    );

    drop(child);
    drop(p2);
    drop(p1);
}

/// Using legacy set_parent(&mut child, ...) as the outer operation moving to p2,
/// a nested set_parent_by_id attempted inside the old parent's ChildRemoved callback
/// moving to p3 must be rejected with ReparentError::Busy, and must not bypass
/// protection or desync the registry parent and parent children lists.
#[test]
fn test_legacy_set_parent_outer_operation_rejects_nested_set_parent_by_id() {
    let _serial = serial();
    let log = Log::default();
    let nested_rejected = Rc::new(Cell::new(false));
    let nested_rejected_clone = Rc::clone(&nested_rejected);
    let p3_id_cell = Rc::new(Cell::new(None));
    let p3_id_clone = Rc::clone(&p3_id_cell);

    let hook: OnChildEvent = Box::new(move |delivered| {
        if delivered.what == "removed" {
            let cid = delivered.child;
            let target_p3 = p3_id_clone.get().unwrap();
            let res = set_parent_by_id(cid, Some(target_p3));
            assert_eq!(res, Err(ReparentError::Busy));
            nested_rejected_clone.set(true);
        }
    });

    // SAFETY: `p1`, `p2`, `p3` and `child` are owned here and dropped at the end of the test, on
    // this thread; no callback of them runs while the owners use them.
    let p1 = unsafe { register_owned_qobject(Handler::new(&log, Some(hook))) };
    // SAFETY: as above for `p2`.
    let p2 = unsafe { register_owned_qobject(Handler::new(&log, None)) };
    // SAFETY: as above for `p3`.
    let p3 = unsafe { register_owned_qobject(Handler::new(&log, None)) };
    // SAFETY: as above for `child`.
    let mut child = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
    let (p1_id, p2_id, p3_id, child_id) = (
        OwnedObject::object_id(&p1),
        OwnedObject::object_id(&p2),
        OwnedObject::object_id(&p3),
        OwnedObject::object_id(&child),
    );
    p3_id_cell.set(Some(p3_id));

    // Initial parent is p1
    assert_eq!(set_parent_by_id(child_id, Some(p1_id)), Ok(()));
    assert_eq!(query_object_parent(child_id), Some(Some(p1_id)));
    assert_eq!(query_object_children(p1_id), Some(vec![child_id]));

    // Legacy set_parent(&mut child.0, ...) moving to p2 as outer operation:
    assert_eq!(set_owned_parent(&mut child, Some(p2_id)), Ok(()));
    assert!(
        nested_rejected.get(),
        "nested set_parent_by_id must be rejected with Busy"
    );

    // Assert they do not bypass protection or desync registry parent and parent children list:
    assert_eq!(query_object_parent(child_id), Some(Some(p2_id)));
    assert_eq!(query_object_children(p1_id), Some(Vec::new()));
    assert_eq!(query_object_children(p2_id), Some(vec![child_id]));
    assert_eq!(query_object_children(p3_id), Some(Vec::new()));

    assert_eq!(child.with(|c| c.0.parent()).unwrap(), Some(p2_id));
    assert_eq!(
        with_object(child_id, |c| c.object_data().parent()),
        Some(Some(p2_id))
    );
    assert_eq!(
        with_object(p1_id, |p| p.object_data().children().to_vec()),
        Some(Vec::new())
    );
    assert_eq!(
        with_object(p2_id, |p| p.object_data().children().to_vec()),
        Some(vec![child_id])
    );
    assert_eq!(
        with_object(p3_id, |p| p.object_data().children().to_vec()),
        Some(Vec::new())
    );

    drop(child);
    drop(p3);
    drop(p2);
    drop(p1);
}

/// When a registered child's parent is modified, a nested set_parent attempted on that
/// registered child from inside a callback is rejected with ReparentError::Busy.
#[test]
fn test_set_parent_nested_set_parent_registered_child_rejected_busy() {
    let _serial = serial();
    let log = Log::default();
    let nested_ran = Rc::new(Cell::new(false));
    let nested_ran_clone = Rc::clone(&nested_ran);
    let p3_id_cell = Rc::new(Cell::new(None));
    let p3_id_clone = Rc::clone(&p3_id_cell);
    let hook: OnChildEvent = Box::new(move |delivered| {
        if delivered.what == "removed" {
            let cid = delivered.child;
            let target_p3 = p3_id_clone.get().unwrap();
            // A nested set_parent on the registered child while its parent change
            // is in progress must be rejected with ReparentError::Busy.
            let res = set_parent_by_id(cid, Some(target_p3));
            assert_eq!(res, Err(ReparentError::Busy));
            nested_ran_clone.set(true);
        }
    });
    // SAFETY: `p1`, `p2`, `p3` and `child` are owned here and dropped at the end of the test, on
    // this thread; no callback of them runs while the owners use them.
    let p1 = unsafe { register_owned_qobject(Handler::new(&log, Some(hook))) };
    // SAFETY: as above for `p2`.
    let p2 = unsafe { register_owned_qobject(Handler::new(&log, None)) };
    // SAFETY: as above for `p3`.
    let p3 = unsafe { register_owned_qobject(Handler::new(&log, None)) };
    // SAFETY: as above for `child`.
    let child = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
    let (p1_id, p2_id, p3_id, child_id) = (
        OwnedObject::object_id(&p1),
        OwnedObject::object_id(&p2),
        OwnedObject::object_id(&p3),
        OwnedObject::object_id(&child),
    );
    p3_id_cell.set(Some(p3_id));

    assert_eq!(set_parent_by_id(child_id, Some(p1_id)), Ok(()));
    assert_eq!(set_parent_by_id(child_id, Some(p2_id)), Ok(()));
    assert!(nested_ran.get());

    // Strict consistency checks: since nested was rejected with Busy, parent remains p2!
    assert_eq!(query_object_parent(child_id), Some(Some(p2_id)));
    assert_eq!(query_object_children(p1_id), Some(Vec::new()));
    assert_eq!(query_object_children(p2_id), Some(vec![child_id]));
    assert_eq!(query_object_children(p3_id), Some(Vec::new()));
    assert_eq!(
        with_object(p1_id, |p| p.object_data().children().to_vec()),
        Some(Vec::new())
    );
    assert_eq!(
        with_object(p2_id, |p| p.object_data().children().to_vec()),
        Some(vec![child_id])
    );
    assert_eq!(
        with_object(p3_id, |p| p.object_data().children().to_vec()),
        Some(Vec::new())
    );
    assert_eq!(
        with_object(child_id, |c| c.object_data().parent()),
        Some(Some(p2_id))
    );
    assert_eq!(child.with(|c| c.0.parent()).unwrap(), Some(p2_id));
    drop(child);
    drop(p3);
    drop(p2);
    drop(p1);
}

/// A parent of another thread: as in Qt, the child is taken from its old parent, which gets
/// `ChildRemoved`, and is left without a parent, in the registry too; the call reports it.
#[test]
fn test_set_parent_to_a_parent_of_another_thread_detaches_the_child_and_reports_it() {
    let _serial = serial();
    let log = Log::default();
    let elsewhere = std::thread::spawn(ThreadId::current).join().unwrap();
    // SAFETY: the objects are owned here and dropped at the end, on this thread.
    let p1 = unsafe { register_owned_qobject(Handler::new(&log, None)) };
    // SAFETY: as above for `p2`; its thread affinity is another thread's, its registration
    // thread is this one.
    let p2 = unsafe {
        register_owned_qobject(Handler {
            data: ObjectData::with_thread(ObjectId::next(), elsewhere),
            log: Rc::clone(&log),
            on_event: None,
        })
    };
    // SAFETY: as above for `child`.
    let mut child = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
    let (p1_id, p2_id, child_id) = (
        OwnedObject::object_id(&p1),
        OwnedObject::object_id(&p2),
        OwnedObject::object_id(&child),
    );
    assert_eq!(set_owned_parent(&mut child, Some(p1_id)), Ok(()));
    log.borrow_mut().clear();

    assert_eq!(
        set_owned_parent(&mut child, Some(p2_id)),
        Err(ReparentError::DifferentThread)
    );

    assert_eq!(child.with(|c| c.0.parent()).unwrap(), None);
    assert_eq!(query_object_parent(child_id), Some(None));
    assert_eq!(query_object_children(p1_id), Some(Vec::new()));
    assert_eq!(query_object_children(p2_id), Some(Vec::new()));
    assert_eq!(
        *log.borrow(),
        vec![Delivered {
            receiver: p1_id,
            what: "removed",
            child: child_id,
            own_children: vec![],
            registry_parent: Some(None),
        }]
    );
    drop(child);
    drop(p2);
    drop(p1);
}

/// `ChildRemoved` and `ChildAdded` of `reparent_owned_object` are delivered after its locks
/// are let go: the handlers find every lock of the change free, and see the finished change.
#[test]
fn test_reparent_owned_delivers_events_after_the_locks_are_released() {
    let _serial = serial();
    let log = Log::default();
    let records: Rc<RefCell<Vec<Arc<ObjectRecord>>>> = Rc::default();
    let probe = || -> OnChildEvent {
        let records = Rc::clone(&records);
        Box::new(move |_| {
            assert_hierarchy_unlocked(&records.borrow());
            // SAFETY: the extra object is owned and dropped right here, on this thread.
            drop(unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) });
        })
    };
    // SAFETY: both parents are owned here and dropped at the end, on this thread.
    let p1 = unsafe { register_owned_qobject(Handler::new(&log, Some(probe()))) };
    // SAFETY: as above for `p2`.
    let p2 = unsafe { register_owned_qobject(Handler::new(&log, Some(probe()))) };
    let (p1_id, p2_id) = (OwnedObject::object_id(&p1), OwnedObject::object_id(&p2));
    // SAFETY: the child is handed over to its parent, which keeps it at its address.
    let child_id =
        unsafe { add_owned_child(p1_id, Box::new(Plain(ObjectData::new(ObjectId::next())))) }
            .map_err(|(e, _)| e)
            .unwrap();
    records
        .borrow_mut()
        .extend([record(p1_id), record(p2_id), record(child_id)]);
    log.borrow_mut().clear();

    let res = reparent_owned_object(child_id, Some(p2_id));
    assert!(matches!(res, Ok(None)), "{:?}", res.as_ref().err());
    let delivered = |receiver, what, own_children| Delivered {
        receiver,
        what,
        child: child_id,
        own_children,
        registry_parent: Some(Some(p2_id)),
    };
    assert_eq!(
        *log.borrow(),
        vec![
            delivered(p1_id, "removed", vec![]),
            delivered(p2_id, "added", vec![child_id])
        ]
    );
    assert_eq!(query_object_children(p2_id), Some(vec![child_id]));
    assert_eq!(query_object_children(p1_id), Some(Vec::new()));
    drop(p2);
    drop(p1);
}

/// Unregistering an object unlists it from its parent without holding the object's own
/// `parent` lock while it waits for the parent's `children` lock. `set_parent` takes them in
/// the other order (parent's list, then child's link), so holding both ways round could
/// deadlock.
///
/// The unregistering thread is held at a fixed point (the link has been read and let go, the
/// parent's list is not locked yet), where this thread holds the parent's list, as a thread
/// inside `set_parent` would, and takes the link. Nothing here depends on time.
#[test]
fn test_unregister_does_not_hold_the_childs_parent_link_while_waiting_for_the_parents_list() {
    let _serial = serial();
    let (records_tx, records_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let (reached_tx, reached_rx) = mpsc::channel::<()>();
    let (resume_tx, resume_rx) = mpsc::channel::<()>();
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let unregistering = std::thread::spawn(move || {
        // SAFETY: the parent is owned here and dropped at the end, on this thread.
        let parent = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
        let parent_id = OwnedObject::object_id(&parent);
        // SAFETY: the child is handed over to its parent, which keeps it at its address.
        let child_id = unsafe {
            add_owned_child(
                parent_id,
                Box::new(Plain(ObjectData::new(ObjectId::next()))),
            )
        }
        .map_err(|(e, _)| e)
        .unwrap();
        records_tx
            .send((record(parent_id), record(child_id), parent_id))
            .unwrap();
        go_rx.recv().unwrap();
        test_hooks::on_next(test_hooks::Point::AfterLinkRead, move || {
            let _ = reached_tx.send(());
            let _ = resume_rx.recv();
        });
        // SAFETY: no callback of the child runs; this is its registration thread.
        unsafe { unregister_qobject(child_id) };
        assert_eq!(query_object_children(parent_id), Some(Vec::new()));
        drop(parent);
        let _ = done_tx.send(());
    });

    let (parent_record, child_record, _parent_id) = records_rx.recv().unwrap();
    // (a) This thread holds the parent's list. `Release` lets the other thread go on, and then
    // frees the list, on every exit, so that a failed check ends this test and not the run.
    struct Release<'a>(
        Option<mpsc::Sender<()>>,
        Option<std::sync::RwLockWriteGuard<'a, Vec<ObjectId>>>,
    );
    impl Drop for Release<'_> {
        fn drop(&mut self) {
            drop(self.0.take());
            drop(self.1.take());
        }
    }
    let mut release = Release(
        Some(resume_tx),
        Some(parent_record.children.write().unwrap()),
    );
    go_tx.send(()).unwrap();
    // (b) The other thread has read the child's link, let it go, and stops.
    reached_rx
        .recv()
        .expect("unregister did not get to the point after reading the link");
    // (c) It holds nothing of the child's, so the link is free for a thread that holds the
    // parent's list and needs the link.
    assert!(
        child_record.parent.try_write().is_ok(),
        "unregister held the child's `parent` lock while it waits for the parent's list"
    );
    // (d) It goes on, and now needs the parent's list, which this thread holds. (e) This thread
    // frees the list, and the other one finishes its cleanup.
    drop(release.0.take());
    drop(release.1.take());
    done_rx
        .recv_timeout(Duration::from_secs(60))
        .expect("unregister did not finish once the parent's list was free");
    unregistering.join().unwrap();
}

/// A hierarchy change that is held between its cycle check and its commit while a second one
/// runs. With `HIERARCHY_MUTATION` the second one waits and sees the first one's link (and is
/// refused when it would close a ring); without it, both are accepted. The thread of the
/// second one is not the thread of the objects the first one works on, as `ObjectData::with_thread`
/// allows: each thread only changes its own objects.
#[test]
fn test_two_set_parent_calls_cannot_close_a_cycle() {
    let _serial = serial();
    let affinity = ThreadId::current();
    let (x_id, y_id) = (ObjectId::next(), ObjectId::next());
    let (reached_tx, reached) = mpsc::channel();
    let (resume, resume_rx) = mpsc::channel();
    let x = side(
        // SAFETY: runs on the `side` thread, which keeps the owner until `finish` and drops it
        // there; nothing else uses the object, and no callback of it runs while the owner is used.
        move || unsafe { register_owned_qobject(Plain(ObjectData::with_thread(x_id, affinity))) },
        move |x| {
            hold_next_operation(reached_tx, resume_rx, &[test_hooks::Point::AfterCycleCheck]);
            set_owned_parent(x, Some(y_id))
        },
    );
    let y = side(
        // SAFETY: as for `x`: owned by this `side` thread until `finish`.
        move || unsafe { register_owned_qobject(Plain(ObjectData::with_thread(y_id, affinity))) },
        move |y| set_owned_parent(y, Some(x_id)),
    );
    x.wait_ready();
    y.wait_ready();
    x.start();
    reached
        .recv()
        .expect("the first call did not reach its check");
    let held = HIERARCHY_MUTATION.try_lock().is_err();
    y.start();
    let early = y.result.recv_timeout(settle());
    resume.send(()).unwrap();
    let from_x = x.result.recv().expect("the first call did not return");
    let from_y = early
        .or_else(|_| y.result.recv())
        .expect("the second call did not return");
    let acyclic = registry_has_no_cycle(&[x_id, y_id]);
    x.finish();
    y.finish();
    assert_eq!(from_x, Ok(()));
    assert_eq!(
        from_y,
        Err(ReparentError::InvalidParent),
        "the second link closes a ring and was accepted"
    );
    assert!(acyclic);
    assert!(
        held,
        "`HIERARCHY_MUTATION` is not held between the check and the commit"
    );
}

/// As above, for `reparent_owned_object` (on the objects of its own thread) against a
/// `set_parent` of an object of another thread: x moves under y while w, an ancestor of y,
/// is put under x.
#[test]
fn test_reparent_owned_object_and_set_parent_cannot_close_a_cycle() {
    let _serial = serial();
    let affinity = ThreadId::current();
    let (w_id, x_id, y_id) = (ObjectId::next(), ObjectId::next(), ObjectId::next());
    let (reached_tx, reached) = mpsc::channel();
    let (resume, resume_rx) = mpsc::channel();
    let w = side(
        // SAFETY: as for `x` above: owned by this `side` thread until `finish`.
        move || unsafe { register_owned_qobject(Plain(ObjectData::with_thread(w_id, affinity))) },
        move |w| set_owned_parent(w, Some(x_id)),
    );
    w.wait_ready();
    let r = side(
        move || {
            // SAFETY: the objects are owned here and dropped at the end, on this thread.
            let p = unsafe {
                register_owned_qobject(Plain(ObjectData::with_thread(ObjectId::next(), affinity)))
            };
            // SAFETY: as above for `y`, owned by this `side` thread until `finish`.
            let mut y =
                unsafe { register_owned_qobject(Plain(ObjectData::with_thread(y_id, affinity))) };
            assert_eq!(set_owned_parent(&mut y, Some(w_id)), Ok(()));
            // SAFETY: the new `Box` is moved into the call and not used again; `p` owns it at its
            // heap address. No callback of the child runs, and `p` is registered on this thread.
            let added = unsafe {
                add_owned_child(
                    OwnedObject::object_id(&p),
                    Box::new(Plain(ObjectData::with_thread(x_id, affinity))),
                )
            }
            .map_err(|(e, _)| e)
            .unwrap();
            assert_eq!(added, x_id);
            (p, y)
        },
        move |_| {
            hold_next_operation(reached_tx, resume_rx, &[test_hooks::Point::AfterCycleCheck]);
            reparent_owned_object(x_id, Some(y_id)).map(|released| assert!(released.is_none()))
        },
    );
    r.wait_ready();
    r.start();
    reached
        .recv()
        .expect("the reparenting did not reach its check");
    let held = HIERARCHY_MUTATION.try_lock().is_err();
    w.start();
    let early = w.result.recv_timeout(settle());
    resume.send(()).unwrap();
    let from_r = r.result.recv().expect("the reparenting did not return");
    let from_w = early
        .or_else(|_| w.result.recv())
        .expect("the second call did not return");
    let acyclic = registry_has_no_cycle(&[w_id, x_id, y_id]);
    r.finish();
    w.finish();
    assert_eq!(from_r, Ok(()));
    assert_eq!(
        from_w,
        Err(ReparentError::InvalidParent),
        "the link closes a ring and was accepted"
    );
    assert!(acyclic);
    assert!(
        held,
        "`HIERARCHY_MUTATION` is not held between the check and the commit"
    );
}

/// As above, for `swap_hierarchy_and_ownership`: it moves c from p to q while w, which q is
/// under, is put under c.
#[test]
fn test_swap_hierarchy_and_set_parent_cannot_close_a_cycle() {
    let _serial = serial();
    let affinity = ThreadId::current();
    let (w_id, c_id, p_id, q_id) = (
        ObjectId::next(),
        ObjectId::next(),
        ObjectId::next(),
        ObjectId::next(),
    );
    let (reached_tx, reached) = mpsc::channel();
    let (resume, resume_rx) = mpsc::channel();
    let w = side(
        // SAFETY: runs on the `side` thread, which keeps the owner until `finish` and drops it
        // there; nothing else uses the object, and no callback of it runs while the owner is used.
        move || unsafe { register_owned_qobject(Plain(ObjectData::with_thread(w_id, affinity))) },
        move |w| set_owned_parent(w, Some(c_id)),
    );
    w.wait_ready();
    let s = side(
        move || {
            // SAFETY: the objects are owned here and dropped at the end, on this thread.
            let p =
                unsafe { register_owned_qobject(Plain(ObjectData::with_thread(p_id, affinity))) };
            // SAFETY: as above for `q`, owned by this `side` thread until `finish`.
            let mut q =
                unsafe { register_owned_qobject(Plain(ObjectData::with_thread(q_id, affinity))) };
            assert_eq!(set_owned_parent(&mut q, Some(w_id)), Ok(()));
            // SAFETY: the new `Box` is moved into the call and not used again; `p` owns it at its
            // heap address. No callback of the child runs, and `p` is registered on this thread.
            let added = unsafe {
                add_owned_child(
                    p_id,
                    Box::new(Plain(ObjectData::with_thread(c_id, affinity))),
                )
            }
            .map_err(|(e, _)| e)
            .unwrap();
            assert_eq!(added, c_id);
            (p, q)
        },
        move |state| {
            let (p, q) = state;
            hold_next_operation(reached_tx, resume_rx, &[test_hooks::Point::AfterCycleCheck]);
            p.with_mut(|p| {
                q.with_mut(|q| {
                    p.object_data_mut()
                        .swap_hierarchy_and_ownership(&mut q.object_data_mut())
                })
                .unwrap()
            })
            .unwrap()
        },
    );
    s.wait_ready();
    s.start();
    reached.recv().expect("the swap did not reach its check");
    let held = HIERARCHY_MUTATION.try_lock().is_err();
    w.start();
    let early = w.result.recv_timeout(settle());
    resume.send(()).unwrap();
    let from_s = s.result.recv().expect("the swap did not return");
    let from_w = early
        .or_else(|_| w.result.recv())
        .expect("the second call did not return");
    let acyclic = registry_has_no_cycle(&[w_id, c_id, p_id, q_id]);
    s.finish();
    w.finish();
    assert_eq!(from_s, Ok(()));
    assert_eq!(
        from_w,
        Err(ReparentError::InvalidParent),
        "the link closes a ring and was accepted"
    );
    assert!(acyclic);
    assert!(
        held,
        "`HIERARCHY_MUTATION` is not held between the check and the commit"
    );
}

/// `swap_hierarchy_and_ownership` holds neither list while it waits for the other: a thread
/// that takes the two lists in ascending order, as every list writer does, finds both free
/// while the swap, whose first argument has the higher id, is held before it locks them.
#[test]
fn test_swap_does_not_hold_one_parents_list_while_it_waits_for_the_other() {
    let _serial = serial();
    let affinity = ThreadId::current();
    let (ids_tx, ids) = mpsc::channel();
    let (reached_tx, reached) = mpsc::channel();
    let (resume, resume_rx) = mpsc::channel();
    let swapper = side(
        move || {
            // SAFETY: the objects are owned here and dropped at the end, on this thread.
            let q = unsafe {
                register_owned_qobject(Plain(ObjectData::with_thread(ObjectId::next(), affinity)))
            };
            // SAFETY: as above for `p`, owned by this `side` thread until `finish`.
            let p = unsafe {
                register_owned_qobject(Plain(ObjectData::with_thread(ObjectId::next(), affinity)))
            };
            ids_tx
                .send((OwnedObject::object_id(&p), OwnedObject::object_id(&q)))
                .unwrap();
            (p, q)
        },
        move |state| {
            let (p, q) = state;
            hold_next_operation(reached_tx, resume_rx, &[test_hooks::Point::AfterCycleCheck]);
            p.with_mut(|p| {
                q.with_mut(|q| {
                    p.object_data_mut()
                        .swap_hierarchy_and_ownership(&mut q.object_data_mut())
                })
                .unwrap()
            })
            .unwrap()
        },
    );
    swapper.wait_ready();
    let (p_id, q_id) = ids.recv().unwrap();
    assert!(q_id < p_id);
    let (low, high) = (record(q_id), record(p_id));
    let holder = side(
        || (),
        move |_| {
            let first = low.children.write().unwrap();
            let second = high.children.write().unwrap();
            drop(second);
            drop(first);
        },
    );
    holder.wait_ready();
    swapper.start();
    reached
        .recv()
        .expect("the swap did not reach its hold point");
    holder.start();
    let took_both = holder.result.recv_timeout(PROBE_WINDOW);
    let _ = resume.send(());
    assert!(
        took_both.is_ok(),
        "the swap held a list that a writer in ascending order needs"
    );
    assert_eq!(
        swapper.result.recv().expect("the swap did not return"),
        Ok(())
    );
    swapper.finish();
    holder.finish();
}

/// A list that changed while `swap_hierarchy_and_ownership` was on its way is not overwritten
/// by the swap: whatever order the two run in, the child that was added is in the list of its
/// parent afterwards.
#[test]
fn test_swap_does_not_overwrite_a_list_that_changed_while_it_waited() {
    let _serial = serial();
    let affinity = ThreadId::current();
    let (p_id, q_id, c_id) = (ObjectId::next(), ObjectId::next(), ObjectId::next());
    let (reached_tx, reached) = mpsc::channel();
    let (resume, resume_rx) = mpsc::channel();
    let swapper = side(
        move || {
            // SAFETY: the objects are owned here and dropped at the end, on this thread.
            let p =
                unsafe { register_owned_qobject(Plain(ObjectData::with_thread(p_id, affinity))) };
            // SAFETY: as above for `q`, owned by this `side` thread until `finish`.
            let q =
                unsafe { register_owned_qobject(Plain(ObjectData::with_thread(q_id, affinity))) };
            (p, q)
        },
        move |state| {
            let (p, q) = state;
            hold_next_operation(reached_tx, resume_rx, &[test_hooks::Point::AfterCycleCheck]);
            p.with_mut(|p| {
                q.with_mut(|q| {
                    p.object_data_mut()
                        .swap_hierarchy_and_ownership(&mut q.object_data_mut())
                })
                .unwrap()
            })
            .unwrap()
        },
    );
    let adder = side(
        // SAFETY: runs on the `side` thread, which keeps the owner until `finish` and drops it
        // there; nothing else uses the object, and no callback of it runs while the owner is used.
        move || unsafe { register_owned_qobject(Plain(ObjectData::with_thread(c_id, affinity))) },
        move |c| set_owned_parent(c, Some(p_id)),
    );
    swapper.wait_ready();
    adder.wait_ready();
    swapper.start();
    reached.recv().expect("the swap did not reach its check");
    adder.start();
    let early = adder.result.recv_timeout(settle());
    resume.send(()).unwrap();
    let from_swap = swapper.result.recv().expect("the swap did not return");
    let from_adder = early
        .or_else(|_| adder.result.recv())
        .expect("the adder did not return");
    let parent_of_c = query_object_parent(c_id);
    let list_of_p = query_object_children(p_id);
    swapper.finish();
    adder.finish();
    assert_eq!(from_adder, Ok(()));
    assert_eq!(from_swap, Ok(()));
    assert_eq!(parent_of_c, Some(Some(p_id)));
    assert!(
        list_of_p.is_some_and(|list| list.contains(&c_id)),
        "the child was added to its parent and the swap wrote the old list over it"
    );
}

/// `set_parent` does not let a registered child have a parent that is not registered, and
/// changes nothing when it refuses. (Two unregistered objects can be linked; see the test
/// above.)
#[test]
fn test_set_parent_refuses_an_unregistered_parent_for_a_registered_child() {
    let parent = ObjectData::new(ObjectId::next());
    // SAFETY: the child is owned here and dropped at the end, on this thread.
    let mut child = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
    let before = child.with(|c| states(&[&c.0, &parent])).unwrap();
    assert_eq!(
        set_owned_parent(&mut child, Some(parent.id())),
        Err(ReparentError::InvalidParent)
    );
    assert_eq!(child.with(|c| states(&[&c.0, &parent])).unwrap(), before);
    assert_eq!(child.with(|c| c.0.parent()).unwrap(), None);
}

/// Objects that were linked while unregistered can form a ring that no registry walk saw; the
/// registration that would close it leaves its link out, so the registry never holds a cycle.
#[test]
fn test_registering_objects_that_were_linked_in_a_ring_keeps_the_registry_acyclic() {
    let mut a = Plain(ObjectData::new(ObjectId::next()));
    let mut b = Plain(ObjectData::new(ObjectId::next()));
    let (a_id, b_id) = (a.0.id(), b.0.id());
    assert_eq!(set_parent(&mut a.0, Some(b_id)), Ok(()));
    assert_eq!(set_parent(&mut b.0, Some(a_id)), Ok(()));

    let ptr_a = &raw mut a as *mut dyn QObject;
    register_object_metadata(&a.0, ptr_a);
    let ptr_b = &raw mut b as *mut dyn QObject;
    register_object_metadata(&b.0, ptr_b);

    assert_eq!(query_object_parent(a_id), Some(Some(b_id)));
    assert_eq!(query_object_parent(b_id), Some(None));
    assert!(registry_has_no_cycle(&[a_id, b_id]));
    // SAFETY: `a` and `b` are locals of this test, registered above and not moved since; no
    // callback of either runs, and this is their registration thread. They drop only at the end
    // of the function, after this call.
    unsafe {
        unregister_qobject(a_id);
        unregister_qobject(b_id);
    }
}

/// `add_owned_child` refuses a parent that is a descendant of the child. Only a child that is
/// registered already can have descendants; a `Box` made from the registered pointer lets one
/// reach the function. The `Box` comes back unchanged.
// The test builds a `Box` from the pointer that it registered, then moves and uses that `Box`
// (`add_owned_child(d_id, boxed)`, `returned.0`). That breaks the contract of
// `register_qobject_ptr` (rule 1 of the `crate::object` module documentation) on purpose, to
// check that the registry copes; Miri would report it, which is why the test is ignored under
// Miri (see below). No SAFETY comment can be true here.
#[allow(clippy::undocumented_unsafe_blocks)]
#[test]
// Ignored under Miri without a reason on purpose: the sweep reads `ignored` at the end of the
// line. The `Box` made from the registered pointer breaks the aliasing rules by design.
#[cfg_attr(miri, ignore)]
fn test_add_owned_child_refuses_a_parent_that_is_a_descendant_of_the_child() {
    // Deliberate contract break (see the `allow` above); nothing here is a safety argument.
    let raw = Box::into_raw(Box::new(Plain(ObjectData::new(ObjectId::next()))));
    let b_id = unsafe { (*raw).0.id() };
    unsafe { register_qobject_ptr(NonNull::new_unchecked(raw as *mut dyn QObject)) };
    let boxed = unsafe { Box::from_raw(raw) };
    let mut d = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
    let d_id = OwnedObject::object_id(&d);
    assert_eq!(set_owned_parent(&mut d, Some(b_id)), Ok(()));

    let (error, returned) = unsafe { add_owned_child(d_id, boxed) }.unwrap_err();
    assert_eq!(error, ReparentError::InvalidParent);
    assert_eq!(query_object_parent(b_id), Some(None));
    assert_eq!(query_object_parent(d_id), Some(Some(b_id)));
    assert_eq!(query_object_children(d_id), Some(Vec::new()));
    assert_eq!(returned.0.parent(), None);
    assert!(!returned.0.is_owned_by_parent());
    assert!(registry_has_no_cycle(&[b_id, d_id]));
    // SAFETY: no callback of `b` runs.
    unsafe { unregister_qobject(b_id) };
    drop(returned);
    drop(d);
}

/// An adoption into a parent whose registry list is poisoned is refused with `Busy` and
/// nothing changed, like every other commit on that list; the `Box` comes back.
#[test]
fn test_add_owned_child_refuses_a_parent_whose_list_is_poisoned_and_changes_nothing() {
    // SAFETY: the parent is owned here and dropped at the end, on this thread.
    let parent = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
    let parent_id = OwnedObject::object_id(&parent);
    let parent_record = record(parent_id);
    poison(&parent_record, |r| {
        let _held = r.children.write().unwrap();
        panic!("poison on purpose");
    });
    let boxed = Box::new(Plain(ObjectData::new(ObjectId::next())));
    let child_id = boxed.0.id();
    let before = parent.with(|p| states(&[&p.0])).unwrap();

    // SAFETY: `boxed` is moved into the call; the call refuses it (the parent's registry list is
    // poisoned) and hands the `Box` back unregistered. `parent` is owned here, on this thread.
    let (error, returned) = unsafe { add_owned_child(parent_id, boxed) }.unwrap_err();
    assert_eq!(error, ReparentError::Busy);
    assert_eq!(parent.with(|p| states(&[&p.0])).unwrap(), before);
    assert!(read_unpoisoned(&parent_record.children).is_empty());
    assert_eq!(
        query_object_parent(child_id),
        None,
        "the child was registered"
    );
    assert_eq!(returned.0.parent(), None);
    assert!(!returned.0.is_owned_by_parent());
    drop(returned);
}

/// A `DeferredDelete` finished for an owned child that a callback further up the stack still
/// borrows: its owner leaves the parent's list all the same, and the memory is freed when that
/// callback returns.
#[test]
fn test_destroy_deferred_of_an_owned_child_borrowed_by_an_outer_frame_takes_its_owner() {
    // SAFETY: the parent is owned here and dropped at the end, on this thread.
    let parent = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
    let parent_id = OwnedObject::object_id(&parent);
    // SAFETY: the child is not used directly; the parent owns it.
    let child_id = unsafe {
        add_owned_child(
            parent_id,
            Box::new(Plain(ObjectData::new(ObjectId::next()))),
        )
    }
    .map_err(|(error, _)| error)
    .unwrap();

    with_object_mut(child_id, |_| destroy_deferred(child_id)).unwrap();

    assert!(
        parent
            .with(|p| p.0.owned_children().is_empty() && p.0.children().is_empty())
            .unwrap()
    );
    assert!(with_object(child_id, |_| ()).is_none());
}

/// A `DeferredDelete` of an owned child that lands while a callback of its parent runs is
/// finished when that callback returns, even if the parent was unregistered in between (its
/// own `deleteLater` with an unreachable owner): the child is destroyed then, not when the
/// parent's memory is freed.
#[test]
fn test_deferred_delete_of_owned_child_is_finished_after_the_parent_is_unregistered_mid_callback() {
    struct Tracked(ObjectData, Arc<AtomicBool>);
    impl QObject for Tracked {
        fn object_data(&self) -> &ObjectData {
            &self.0
        }
        fn object_data_mut(&mut self) -> ObjectDataMut<'_> {
            ObjectDataMut::new(&mut self.0)
        }
    }
    impl Drop for Tracked {
        fn drop(&mut self) {
            self.1.store(true, Ordering::SeqCst);
        }
    }
    let dropped = Arc::new(AtomicBool::new(false));
    // SAFETY: the parent is owned here and dropped at the end, on this thread.
    let parent = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
    let parent_id = OwnedObject::object_id(&parent);
    // SAFETY: the child is not used directly; the parent owns it.
    let child_id = unsafe {
        add_owned_child(
            parent_id,
            Box::new(Tracked(
                ObjectData::new(ObjectId::next()),
                Arc::clone(&dropped),
            )),
        )
    }
    .map_err(|(error, _)| error)
    .unwrap();

    with_object_mut(parent_id, |_| {
        destroy_deferred(child_id);
        destroy_deferred(parent_id);
    })
    .unwrap();

    assert!(
        dropped.load(Ordering::SeqCst),
        "the owned child must be destroyed when the parent's callback returns"
    );
    drop(parent);
}
/// Objects whose own lists name each other (the safe `reparent` builds that without any
/// parent link) are registered: a child that is registered under another parent is not listed,
/// so the registry's lists do not form a ring, and dropping the parent of such an object from
/// inside its own callback ends (`unregister_descendants` walks the lists).
#[test]
#[allow(deprecated)]
// Ignored under Miri without a reason on purpose: the sweep reads `ignored` at the end of the
// line. The pointer of the `Box` that is registered first is not used after the `Box` moves.
#[cfg_attr(miri, ignore)]
fn test_registering_objects_whose_lists_name_each_other_leaves_no_ring_in_the_registry() {
    // SAFETY: the owner is dropped at the end, on this thread.
    let q = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
    let q_id = OwnedObject::object_id(&q);
    let mut a = Box::new(Plain(ObjectData::new(ObjectId::next())));
    let mut b = OwnedObject::new(Plain(ObjectData::new(ObjectId::next())));
    let (a_id, b_id) = (a.0.id(), OwnedObject::object_id(&b));
    b.with_mut(|b| reparent(&mut b.0, None, Some(&mut a.0)))
        .unwrap(); // b.parent = a, a.children = [b]
    b.with_mut(|b| reparent(&mut b.0, None, None)).unwrap(); // b's own link is cleared; a still lists b
    b.with_mut(|b| reparent(&mut a.0, None, Some(&mut b.0)))
        .unwrap(); // a.parent = b, b.children = [a]
    reparent(&mut a.0, None, None); // a's own link is cleared; b still lists a

    let a_ptr: *mut Plain = &raw mut *a;
    // SAFETY: the box is kept and not used again before `add_owned_child` takes it over.
    unsafe { register_qobject_ptr(NonNull::new_unchecked(a_ptr as *mut dyn QObject)) };
    // SAFETY: the owner is kept and not used again before it is dropped.
    unsafe { register_qobject_ptr(OwnedObject::as_non_null(&b) as NonNull<dyn QObject>) };
    assert_eq!(query_object_children(a_id), Some(vec![b_id]));
    assert_eq!(
        query_object_children(b_id),
        Some(Vec::new()),
        "the lists of a and b make a ring"
    );

    // SAFETY: the box is not used again.
    unsafe { add_owned_child(q_id, a) }
        .map_err(|(e, _)| e)
        .unwrap();
    assert_eq!(
        query_object_children(a_id),
        Some(Vec::new()),
        "b is not a's child: it is registered without a parent"
    );
    let mut q = Some(q);
    let finished = with_object_mut(a_id, |_| drop(q.take()));
    assert!(finished.is_some());
    assert_eq!(query_object_parent(a_id), None, "a is gone with its parent");
    drop(b);
}

/// `unregister_descendants` ends on a ring in the registry's lists and links, which the registry
/// does not build itself but which a stale or poisoned record could hold; it unregisters what
/// is below the object and leaves the object.
#[test]
fn test_unregister_descendants_ends_on_a_ring() {
    // SAFETY: `a` is owned here and dropped at the end of the test, on this thread; no callback of
    // it runs while the owner uses it.
    let a = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
    // SAFETY: as above for `b`.
    let b = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
    // SAFETY: as above for `c`.
    let c = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
    let (a_id, b_id, c_id) = (
        OwnedObject::object_id(&a),
        OwnedObject::object_id(&b),
        OwnedObject::object_id(&c),
    );
    for (parent, child) in [(a_id, b_id), (b_id, c_id), (c_id, a_id)] {
        record(parent).children.write().unwrap().push(child);
        link(child, Some(parent));
    }

    unregister_descendants(a_id);

    assert_eq!(query_object_parent(b_id), None);
    assert_eq!(query_object_parent(c_id), None);
    assert!(
        query_object_parent(a_id).is_some(),
        "the object itself stays registered"
    );
    record(a_id).children.write().unwrap().clear();
    link(a_id, None);
    drop(c);
    drop(b);
    drop(a);
}

/// A list that names an object whose own record names no parent (the safe `reparent` edits the
/// objects' own links only, so such an entry can be registered) does not make that object a
/// child: dropping the listing object leaves it registered.
#[test]
fn test_dropping_an_object_does_not_destroy_a_listed_object_that_is_not_its_child() {
    let mut a = OwnedObject::new(Plain(ObjectData::new(ObjectId::next())));
    let mut b = OwnedObject::new(Plain(ObjectData::new(ObjectId::next())));
    let b_id = OwnedObject::object_id(&b);
    a.with_mut(|a| {
        b.with_mut(|b| reparent(&mut b.0, None, Some(&mut a.0)))
            .unwrap()
    })
    .unwrap(); // b.parent = a, a.children = [b]
    b.with_mut(|b| reparent(&mut b.0, None, None)).unwrap(); // b's own link is cleared; a still lists b
    // SAFETY: the owners are kept until they are dropped below, on this thread.
    unsafe {
        register_qobject_ptr(OwnedObject::as_non_null(&a) as NonNull<dyn QObject>);
        register_qobject_ptr(OwnedObject::as_non_null(&b) as NonNull<dyn QObject>);
    }
    assert_eq!(query_object_parent(b_id), Some(None));

    drop(a);

    assert_eq!(
        query_object_parent(b_id),
        Some(None),
        "b was destroyed with an object that does not own it"
    );
    assert!(with_object(b_id, |_| ()).is_some());
    drop(b);
}

/// An object that is registered again while it is registered keeps what only the registry
/// knows: a child of another registration thread that `set_parent` listed in the registry
/// (the parent's own list cannot be edited from there). It is not dropped from the list, and the
/// child still loses its parent link when the parent is destroyed.
#[test]
fn test_registering_an_object_again_keeps_the_children_that_only_the_registry_lists() {
    let _serial = serial();
    let affinity = ThreadId::current();
    let x_id = ObjectId::next();
    // SAFETY: the object is owned here and dropped at the end, on this thread.
    let child = unsafe {
        register_owned_qobject(Plain(ObjectData::with_thread(ObjectId::next(), affinity)))
    };
    let child_id = OwnedObject::object_id(&child);
    let x = side(
        // SAFETY: runs on the `side` thread, which keeps the owner until `finish` and drops it
        // there; nothing else uses the object, and no callback of it runs while the owner is used.
        move || unsafe { register_owned_qobject(Plain(ObjectData::with_thread(x_id, affinity))) },
        move |x| set_owned_parent(x, Some(child_id)),
    );
    x.wait_ready();
    x.start();
    assert_eq!(x.result.recv().expect("set_parent did not return"), Ok(()));
    assert_eq!(query_object_children(child_id), Some(vec![x_id]));
    assert!(
        child.with(|c| c.0.children().is_empty()).unwrap(),
        "the parent's own list cannot be edited from another thread"
    );

    // SAFETY: the owner is alive and no callback or `&mut` of the object exists; only a shared
    // reference to its data is formed.
    let child_data = unsafe { &(*OwnedObject::as_non_null(&child).as_ptr()).0 };
    register_object_metadata(
        child_data,
        OwnedObject::as_non_null(&child).as_ptr() as *mut dyn QObject,
    );

    assert_eq!(
        query_object_children(child_id),
        Some(vec![x_id]),
        "re-registration dropped a listed child"
    );
    assert_eq!(query_object_parent(x_id), Some(Some(child_id)));
    drop(child);
    assert_eq!(
        query_object_parent(x_id),
        Some(None),
        "x kept a dead parent"
    );
    x.finish();
}

/// Cleanup of a destroyed object cannot report an error, and skipping a step leaves an id in a
/// parent's list or a record that points to freed memory: it finishes although every lock on
/// its way is poisoned (a poisoned lock here guards a plain list or link).
#[test]
fn test_unregister_finishes_its_cleanup_when_the_locks_of_its_records_are_poisoned() {
    // SAFETY: the parent is owned here and dropped at the end, on this thread.
    let parent = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
    let parent_id = OwnedObject::object_id(&parent);
    // SAFETY: the child is handed over to its parent, which keeps it at its address.
    let child_id = unsafe {
        add_owned_child(
            parent_id,
            Box::new(Plain(ObjectData::new(ObjectId::next()))),
        )
    }
    .map_err(|(e, _)| e)
    .unwrap();
    let (parent_record, child_record) = (record(parent_id), record(child_id));
    poison(&parent_record, |r| {
        let _held = r.children.write().unwrap();
        panic!("poison on purpose");
    });
    poison(&child_record, |r| {
        let _held = r.parent.write().unwrap();
        panic!("poison on purpose");
    });
    poison(&child_record, |r| {
        let _held = r.children.write().unwrap();
        panic!("poison on purpose");
    });
    assert!(parent_record.children.is_poisoned() && child_record.parent.is_poisoned());

    // SAFETY: no callback of the child runs; this is its registration thread.
    unsafe { unregister_qobject(child_id) };
    assert_eq!(query_object_parent(child_id), None);
    assert!(!read_unpoisoned(&parent_record.children).contains(&child_id));
    assert!(!child_record.liveness.load(Ordering::Acquire));
    drop(parent);
}

/// A poisoned thread-affinity lock (a plain `ThreadId`, replaced by one store) does not turn
/// the calls that read it into panics: the queries answer, `set_parent` works, and the calls
/// that need the lock to borrow an object say `Busy`.
#[test]
fn test_poisoned_thread_affinity_locks_do_not_panic_the_hierarchy_calls() {
    // SAFETY: the objects are owned here and dropped at the end, on this thread.
    let parent = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
    // SAFETY: as above for `child`.
    let mut child = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
    let (parent_id, child_id) = (
        OwnedObject::object_id(&parent),
        OwnedObject::object_id(&child),
    );
    let parent_record = record(parent_id);
    poison(&parent_record, |r| {
        let _held = r.thread_id.write().unwrap();
        panic!("poison on purpose");
    });
    poison(&parent_record, |r| {
        let _held = r.registration_thread.write().unwrap();
        panic!("poison on purpose");
    });
    assert!(
        parent_record.thread_id.is_poisoned() && parent_record.registration_thread.is_poisoned()
    );

    assert!(registered_on_current_thread(parent_id));
    assert_eq!(registration_thread_of(parent_id), Some(ThreadId::current()));
    assert_eq!(query_object_thread(parent_id), Some(ThreadId::current()));
    assert_eq!(set_owned_parent(&mut child, Some(parent_id)), Ok(()));
    assert_eq!(query_object_children(parent_id), Some(vec![child_id]));
    assert_eq!(set_owned_parent(&mut child, None), Ok(()));
    // SAFETY: the grandchild is handed over to its parent, which keeps it at its address.
    let owned_id =
        unsafe { add_owned_child(child_id, Box::new(Plain(ObjectData::new(ObjectId::next())))) }
            .map_err(|(e, _)| e)
            .unwrap();
    let before = child.with(|c| states(&[&c.0])).unwrap();
    assert!(matches!(
        reparent_owned_object(owned_id, Some(parent_id)),
        Err(ReparentError::Busy)
    ));
    assert_eq!(child.with(|c| states(&[&c.0])).unwrap(), before);
    drop(child);
    drop(parent);
}

/// A new parent that is registered on another thread (with the affinity of the child) cannot
/// be borrowed here: the answer is the documented `InvalidParent`, not `Busy`, and nothing
/// changes.
#[test]
fn test_reparent_owned_object_answers_invalid_parent_for_a_parent_of_another_registration_thread() {
    let affinity = ThreadId::current();
    let np_id = ObjectId::next();
    let there = side(
        // SAFETY: runs on the `side` thread, which keeps the owner until `finish` and drops it
        // there; nothing else uses the object, and no callback of it runs while the owner is used.
        move || unsafe { register_owned_qobject(Plain(ObjectData::with_thread(np_id, affinity))) },
        |_| (),
    );
    there.wait_ready();
    // SAFETY: the objects are owned here and dropped at the end, on this thread.
    let p = unsafe {
        register_owned_qobject(Plain(ObjectData::with_thread(ObjectId::next(), affinity)))
    };
    // SAFETY: the new `Box` is moved into the call and not used again; `p` owns it at its heap
    // address. No callback of the child runs, and `p` is registered on this thread.
    let child_id = unsafe {
        add_owned_child(
            OwnedObject::object_id(&p),
            Box::new(Plain(ObjectData::with_thread(ObjectId::next(), affinity))),
        )
    }
    .map_err(|(e, _)| e)
    .unwrap();
    let before = p
        .with(|p| {
            p.0.owned_children[0]
                .with(|c| states(&[&p.0, c.object_data()]))
                .unwrap()
        })
        .unwrap();
    let res = reparent_owned_object(child_id, Some(np_id));
    assert!(
        matches!(res, Err(ReparentError::InvalidParent)),
        "{:?}",
        res.as_ref().err()
    );
    assert_eq!(
        p.with(|p| p.0.owned_children[0]
            .with(|c| states(&[&p.0, c.object_data()]))
            .unwrap())
            .unwrap(),
        before
    );
    there.finish();
}

/// An object is not its own parent, also when the call comes from inside its own callback (its
/// borrow is held): the answer is `InvalidParent`, not `Busy`.
#[test]
fn test_reparent_owned_object_answers_invalid_parent_for_its_own_id_inside_its_own_callback() {
    // SAFETY: the objects are owned here and dropped at the end, on this thread.
    let p = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
    // SAFETY: the new `Box` is moved into the call and not used again; `p` owns it at its heap
    // address. No callback of the child runs, and `p` is registered on this thread.
    let child_id = unsafe {
        add_owned_child(
            OwnedObject::object_id(&p),
            Box::new(Plain(ObjectData::new(ObjectId::next()))),
        )
    }
    .map_err(|(e, _)| e)
    .unwrap();
    let inside = with_object_mut(child_id, |_| {
        reparent_owned_object(child_id, Some(child_id)).map(|_| ())
    });
    assert_eq!(inside, Some(Err(ReparentError::InvalidParent)));
}

/// A child that has a parent already is refused: that parent would go on listing it, and
/// dropping that parent would unregister the child that the new parent owns. The `Box` comes
/// back, and neither parent's list changed.
#[test]
fn test_add_owned_child_refuses_a_child_that_has_a_parent_already() {
    // SAFETY: the parents are owned here and dropped at the end, on this thread.
    let a = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
    // SAFETY: as above for `b`.
    let b = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
    let (a_id, b_id) = (OwnedObject::object_id(&a), OwnedObject::object_id(&b));
    let mut boxed = Box::new(Plain(ObjectData::new(ObjectId::next())));
    let child_id = boxed.0.id();
    assert_eq!(set_parent(&mut boxed.0, Some(a_id)), Ok(()));
    assert_eq!(query_object_children(a_id), Some(vec![child_id]));

    // SAFETY: `boxed` is moved into the call; the call refuses it (it has a parent already) and
    // hands the `Box` back unregistered. `b` is owned here, on this thread.
    let (error, returned) = unsafe { add_owned_child(b_id, boxed) }.unwrap_err();
    assert_eq!(error, ReparentError::InvalidParent);
    assert_eq!(returned.0.parent(), Some(a_id));
    assert!(!returned.0.is_owned_by_parent());
    assert_eq!(query_object_children(a_id), Some(vec![child_id]));
    assert_eq!(query_object_children(b_id), Some(Vec::new()));
    assert_eq!(
        query_object_parent(child_id),
        None,
        "the child was registered"
    );
    drop(returned);
    drop(b);
    drop(a);
}

/// An object whose own accessor panics while the child-list edits queued during its callback
/// are applied does not stay borrowed for ever.
#[test]
fn test_borrow_flag_is_released_when_the_accessor_panics_in_the_guards_drop() {
    struct Panicky {
        data: ObjectData,
        armed: Arc<AtomicBool>,
    }
    impl QObject for Panicky {
        fn object_data(&self) -> &ObjectData {
            &self.data
        }
        fn object_data_mut(&mut self) -> ObjectDataMut<'_> {
            assert!(
                !self.armed.load(Ordering::SeqCst),
                "the accessor panics on purpose"
            );
            ObjectDataMut::new(&mut self.data)
        }
    }
    let armed = Arc::new(AtomicBool::new(false));
    // SAFETY: the parent is owned here and dropped at the end, on this thread.
    let parent = unsafe {
        register_owned_qobject(Panicky {
            data: ObjectData::new(ObjectId::next()),
            armed: Arc::clone(&armed),
        })
    };
    let parent_id = OwnedObject::object_id(&parent);
    let mut child = ObjectData::new(ObjectId::next());
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        with_object_mut(parent_id, |_| {
            // The parent is borrowed, so the edit of its own list is queued...
            assert_eq!(set_parent(&mut child, Some(parent_id)), Ok(()));
            // ... and applied by the guard when this callback returns.
            armed.store(true, Ordering::SeqCst);
        })
    }));
    armed.store(false, Ordering::SeqCst);
    assert!(
        outcome.is_err(),
        "the accessor did not panic: the queued edit was not applied by the guard"
    );
    assert!(
        with_object(parent_id, |_| ()).is_some(),
        "the parent stays borrowed"
    );
    drop(child);
    drop(parent);
}

struct ProbeDirectParent(ObjectData);
impl QObject for ProbeDirectParent {
    fn object_data(&self) -> &ObjectData {
        &self.0
    }
    fn object_data_mut(&mut self) -> ObjectDataMut<'_> {
        ObjectDataMut::new(&mut self.0)
    }
}

#[test]
fn test_reentrant_new_parent_deletion_owned_object_path_b() {
    let _serial = serial();
    let log_a = Rc::new(RefCell::new(Vec::new()));
    let p_b_cell = Rc::new(RefCell::new(None));
    let p_b_clone = Rc::clone(&p_b_cell);
    let child_id_cell = Rc::new(Cell::new(ObjectId(0)));
    let child_id_clone = Rc::clone(&child_id_cell);

    let on_event: OnChildEvent = Box::new(move |delivered: &Delivered| {
        if delivered.what == "removed" {
            let cid = child_id_clone.get();
            with_object(cid, |_| {
                let b_opt = p_b_clone.borrow_mut().take();
                drop(b_opt);
            });
        }
    });

    // SAFETY: `p_a` is owned here and dropped at the end of the test, on this thread; no callback
    // of it runs while the owner uses it.
    let p_a = unsafe { register_owned_qobject(Handler::new(&log_a, Some(on_event))) };
    let a_id = OwnedObject::object_id(&p_a);

    // SAFETY: `child` is owned here and dropped at the end of the test, on this thread.
    let child = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
    let child_id = OwnedObject::object_id(&child);
    child_id_cell.set(child_id);
    assert_eq!(set_parent_by_id(child_id, Some(a_id)), Ok(()));
    log_a.borrow_mut().clear();

    // SAFETY: `p_b` goes into `p_b_cell` and is dropped there, on this thread, from inside a
    // callback of the child while `set_parent_by_id` holds `p_b`'s borrow: `OwnedObject::drop`
    // allows that, unregistering at once and freeing the memory after the callback returns.
    let p_b =
        unsafe { register_owned_qobject(ProbeDirectParent(ObjectData::new(ObjectId::next()))) };
    let b_id = OwnedObject::object_id(&p_b);
    *p_b_cell.borrow_mut() = Some(p_b);

    assert_eq!(set_parent_by_id(child_id, Some(b_id)), Ok(()));
    assert_eq!(
        query_object_parent(child_id),
        Some(None),
        "registry parent should be Some(None)"
    );
    assert!(
        with_object(child_id, |_| ()).is_some(),
        "child must remain alive and registered"
    );
    assert_eq!(
        child.with(|c| c.object_data().parent()).unwrap(),
        None,
        "child local parent should be None"
    );

    // Now drop child: A must NOT receive a duplicate ChildRemoved
    drop(child);
    let removed_count = log_a
        .borrow()
        .iter()
        .filter(|d| d.what == "removed" && d.child == child_id)
        .count();
    assert_eq!(
        removed_count, 1,
        "A must receive exactly one ChildRemoved event, no duplicate on child drop"
    );
    drop(p_a);
}

#[test]
// The event callback drops this test's `ProbeDirectParent` by hand (`drop_in_place`, `dealloc`)
// while `set_parent_by_id` holds that object's borrow. The contract of the registration
// functions forbids dropping an object while a callback of it runs, except through
// `OwnedObject`'s deferred destruction (see the `crate::object` module documentation). That is
// broken on purpose, to check that the registry copes. Miri passes it under both Stacked and
// Tree Borrows. The other `unsafe` blocks of the test are covered by the narrow `allow` too.
#[allow(clippy::undocumented_unsafe_blocks)]
fn test_reentrant_new_parent_deletion_direct_pointer_path_a() {
    let _serial = serial();
    let log_a = Rc::new(RefCell::new(Vec::new()));
    let p_b_ptr_cell: Rc<RefCell<Option<*mut ProbeDirectParent>>> = Rc::new(RefCell::new(None));
    let p_b_clone = Rc::clone(&p_b_ptr_cell);
    let child_id_cell = Rc::new(Cell::new(ObjectId(0)));
    let child_id_clone = Rc::clone(&child_id_cell);

    let on_event: OnChildEvent = Box::new(move |delivered: &Delivered| {
        if delivered.what == "removed" {
            let cid = child_id_clone.get();
            with_object(cid, |_| {
                let b_ptr = p_b_clone.borrow_mut().take().unwrap();
                unsafe {
                    std::ptr::drop_in_place(b_ptr);
                    std::alloc::dealloc(
                        b_ptr as *mut u8,
                        std::alloc::Layout::new::<ProbeDirectParent>(),
                    );
                }
            });
        }
    });

    // SAFETY: `p_a` is owned here and dropped at the end of the test, on this thread; no callback
    // of it runs while the owner uses it.
    let p_a = unsafe { register_owned_qobject(Handler::new(&log_a, Some(on_event))) };
    let a_id = OwnedObject::object_id(&p_a);

    // SAFETY: `child` is owned here and dropped at the end of the test, on this thread.
    let child = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
    let child_id = OwnedObject::object_id(&child);
    child_id_cell.set(child_id);
    assert_eq!(set_parent_by_id(child_id, Some(a_id)), Ok(()));
    log_a.borrow_mut().clear();

    let b_layout = std::alloc::Layout::new::<ProbeDirectParent>();
    // SAFETY: `b_layout` is the layout of `ProbeDirectParent`, which is not zero-sized, as
    // `alloc` requires; the result is initialised on the next line before any other use.
    let p_raw = unsafe { std::alloc::alloc(b_layout) as *mut ProbeDirectParent };
    // SAFETY: `p_raw` is the fresh allocation for exactly this layout (non-null unless `alloc`
    // failed, which aborts in a test), so it is valid and aligned for the write.
    unsafe { std::ptr::write(p_raw, ProbeDirectParent(ObjectData::new(ObjectId::next()))) };
    // SAFETY: `p_raw` was initialised on the previous line and nothing else refers to it yet.
    let b_id = unsafe { (*p_raw).0.id() };
    // SAFETY: `p_raw` points to the initialised object, which stays allocated until the event
    // callback below drops it (`ObjectData::drop` unregisters it); only a shared reference to
    // its data is formed, for the duration of the call.
    unsafe { register_object_metadata(&(*p_raw).0, p_raw as *mut dyn QObject) };
    *p_b_ptr_cell.borrow_mut() = Some(p_raw);

    assert_eq!(set_parent_by_id(child_id, Some(b_id)), Ok(()));
    assert_eq!(
        query_object_parent(child_id),
        None,
        "registry parent should be None"
    );
    assert_eq!(
        child.with(|c| c.object_data().parent()).unwrap(),
        None,
        "child local parent should be None"
    );

    // Now drop child: A must NOT receive a duplicate ChildRemoved
    drop(child);
    let removed_count = log_a
        .borrow()
        .iter()
        .filter(|d| d.what == "removed" && d.child == child_id)
        .count();
    assert_eq!(
        removed_count, 1,
        "A must receive exactly one ChildRemoved event, no duplicate on child drop"
    );
    drop(p_a);
}

#[test]
// This test hands the registry an object that it then drops by hand (`drop_in_place`, `dealloc`
// in the event callback) while `set_owned_parent` (`set_parent`) holds that object's borrow. The
// contract of the registration functions forbids dropping an object while a callback of it
// runs, except through `OwnedObject`'s deferred destruction (see the `crate::object` module
// documentation). That is broken on purpose, to check that the registry copes. Miri passes it
// under both Stacked and Tree Borrows. The other `unsafe` blocks are covered by the narrow
// `allow` too.
#[allow(clippy::undocumented_unsafe_blocks)]
fn test_set_parent_reentrant_new_parent_deletion_clears_local_state() {
    let _serial = serial();
    let log_a = Rc::new(RefCell::new(Vec::new()));
    let p_b_ptr_cell: Rc<RefCell<Option<*mut ProbeDirectParent>>> = Rc::new(RefCell::new(None));
    let p_b_clone = Rc::clone(&p_b_ptr_cell);

    let on_event: OnChildEvent = Box::new(move |delivered: &Delivered| {
        if delivered.what == "removed" {
            let b_ptr = p_b_clone.borrow_mut().take().unwrap();
            unsafe {
                std::ptr::drop_in_place(b_ptr);
                std::alloc::dealloc(
                    b_ptr as *mut u8,
                    std::alloc::Layout::new::<ProbeDirectParent>(),
                );
            }
        }
    });

    // SAFETY: `p_a` is owned here and dropped at the end of the test, on this thread; no callback
    // of it runs while the owner uses it.
    let p_a = unsafe { register_owned_qobject(Handler::new(&log_a, Some(on_event))) };
    let a_id = OwnedObject::object_id(&p_a);

    // SAFETY: `child` is owned here and dropped at the end of the test, on this thread.
    let mut child = unsafe { register_owned_qobject(Plain(ObjectData::new(ObjectId::next()))) };
    let child_id = OwnedObject::object_id(&child);
    assert_eq!(set_owned_parent(&mut child, Some(a_id)), Ok(()));
    log_a.borrow_mut().clear();

    let b_layout = std::alloc::Layout::new::<ProbeDirectParent>();
    // SAFETY: `b_layout` is the layout of `ProbeDirectParent`, which is not zero-sized, as
    // `alloc` requires; the result is initialised on the next line before any other use.
    let b_raw = unsafe { std::alloc::alloc(b_layout) as *mut ProbeDirectParent };
    // SAFETY: `b_raw` is the fresh allocation for exactly this layout (non-null unless `alloc`
    // failed, which aborts in a test), so it is valid and aligned for the write.
    unsafe { std::ptr::write(b_raw, ProbeDirectParent(ObjectData::new(ObjectId::next()))) };
    // SAFETY: `b_raw` was initialised on the previous line and nothing else refers to it yet.
    let b_id = unsafe { (*b_raw).0.id() };
    // SAFETY: `b_raw` points to the initialised object, which stays allocated until the event
    // callback drops it (`ObjectData::drop` unregisters it); only a shared reference to its data
    // is formed, for the duration of the call.
    unsafe { register_object_metadata(&(*b_raw).0, b_raw as *mut dyn QObject) };
    *p_b_ptr_cell.borrow_mut() = Some(b_raw);
    let child_rec = record(child_id);

    // Call set_parent(&mut child.0, Some(b_id)):
    assert_eq!(set_owned_parent(&mut child, Some(b_id)), Ok(()));
    // 1. Raw parent field must be None
    assert_eq!(
        child.with(|c| c.0.parent).unwrap(),
        None,
        "raw parent field must be None"
    );
    assert_eq!(
        child.with(|c| c.0.parent()).unwrap(),
        None,
        "child.parent() must be None"
    );

    // 2. Liveness token must be None
    assert!(
        child.with(|c| c.0.parent_liveness.is_none()).unwrap(),
        "parent_liveness token must be None"
    );
    assert_eq!(
        query_object_parent(child_id),
        None,
        "child was unregistered so query_object_parent is None"
    );

    // 4. pending_parent_sync state must be cleared and applied
    assert!(
        !child_rec.has_pending_parent_sync.load(Ordering::Acquire),
        "has_pending_parent_sync must be false"
    );
    assert!(
        lock_unpoisoned(&child_rec.pending_parent_sync).is_none(),
        "pending_parent_sync record must be None"
    );
    drop(child);
    drop(p_a);
}
