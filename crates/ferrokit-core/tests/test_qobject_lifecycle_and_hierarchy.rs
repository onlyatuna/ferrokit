// SPDX-License-Identifier: LGPL-3.0-only
use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::event_loop::{EventLoop, post_event_to_thread};
use ferrokit_core::object::{
    ObjectData, ObjectId, OwnedObject, QObject, QObjectExt, QPointer, SignalBlocker, ThreadId,
    add_owned_child, move_to_thread_by_id, query_object_thread, register_owned_qobject,
    register_qobject_ptr, unregister_qobject, with_object,
};
use ferrokit_core::signal::Signal;
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::channel;
use std::time::Duration;

/// An `ObjectData` that lets `move_to_thread_by_id` move its object.
fn movable_data(id: ObjectId) -> ObjectData {
    // SAFETY: the mock widgets hold `Arc` handles, a `Signal` (an `Arc<Mutex<_>>` plus ids and
    // atomics) and plain fields, none of which is `Rc`, `Cell`, thread-local or a raw pointer
    // [inferred for `Signal` from its definition]. Only `MockContainer` is moved to another thread
    // in this file (`test_move_to_thread_cascades_children_and_events`).
    unsafe { ObjectData::new(id).with_thread_moves_allowed() }
}

// --- Test Structures ---

struct MockContainer {
    data: ObjectData,
    child_removed_count: Arc<AtomicUsize>,
    child_added_count: Arc<AtomicUsize>,
    thread_changed: Arc<AtomicBool>,
}

impl MockContainer {
    fn new(name: &str) -> Self {
        let mut data = movable_data(ObjectId::next());
        data.set_object_name(name);
        Self {
            data,
            child_removed_count: Arc::new(AtomicUsize::new(0)),
            child_added_count: Arc::new(AtomicUsize::new(0)),
            thread_changed: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl QObject for MockContainer {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
    }
    fn as_qobject_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn as_qobject_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
    fn event(&mut self, event: &mut Event) -> bool {
        match &event.kind {
            EventKind::ChildAdded { .. } => {
                self.child_added_count.fetch_add(1, Ordering::SeqCst);
                true
            }
            EventKind::ChildRemoved { .. } => {
                self.child_removed_count.fetch_add(1, Ordering::SeqCst);
                true
            }
            EventKind::ThreadChange => {
                self.thread_changed.store(true, Ordering::SeqCst);
                true
            }
            _ => false,
        }
    }
}

struct MockButton {
    data: ObjectData,
    pub clicked: Signal<()>,
    pub text: String,
}

impl MockButton {
    fn new(name: &str, text: &str) -> Self {
        let id = ObjectId::next();
        let mut data = movable_data(id);
        data.set_object_name(name);
        let clicked = Signal::with_owner(&data);
        Self {
            data,
            clicked,
            text: text.to_string(),
        }
    }
}

impl QObject for MockButton {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
    }
    fn as_qobject_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn as_qobject_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

struct MockLabel {
    data: ObjectData,
    pub text: String,
}

impl MockLabel {
    fn new(name: &str, text: &str) -> Self {
        let mut data = movable_data(ObjectId::next());
        data.set_object_name(name);
        Self {
            data,
            text: text.to_string(),
        }
    }
}

impl QObject for MockLabel {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
    }
    fn as_qobject_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn as_qobject_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

// --- 1. Ownership & Deletion Cascade Tests ---

#[test]
fn test_ownership_and_deletion_cascade() {
    let mut parent = MockContainer::new("root_parent");
    let parent_id = parent.data.id();
    // SAFETY: `parent` is a local on its registration thread, registered with `&raw mut`, so no
    // reference exists at registration; until `drop(parent)` below the test only reads it
    // directly (rule 7). `drop(parent)` MOVES the registered object into the callee's argument
    // (rule 5 says the address must stay until `unregister_qobject` returns). That is harmless
    // here only if the registry's old pointer is never dereferenced after the move: the drop
    // runs `ObjectData::drop` on the moved copy, which clears the children's parent links
    // first (`delete_children`) so no `ChildRemoved` goes back to `parent`, and no event or
    // slot is pending [inferred from `delete_children`; not checked under Miri].
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut parent)) };

    let child_liveness: Arc<AtomicBool>;
    let grandchild_liveness: Arc<AtomicBool>;

    {
        let child = Box::new(MockContainer::new("child_1"));
        child_liveness = child.data.liveness();
        assert!(child_liveness.load(Ordering::SeqCst));

        let grandchild = Box::new(MockContainer::new("grandchild_1"));
        grandchild_liveness = grandchild.data.liveness();
        assert!(grandchild_liveness.load(Ordering::SeqCst));

        let child_id = child.data.id();
        // SAFETY: `child` is a fresh box with no reference taken from it (only the `Arc` liveness
        // token), and `owned_children` of `parent` keeps it at that address. `parent` is registered
        // on this thread, the registration thread. No callback runs.
        let _ = unsafe { add_owned_child(parent_id, child) }
            .unwrap_or_else(|(e, _)| panic!("attach refused: {e:?}"));
        // SAFETY: `child_id` was registered by the attach above and is on this thread, so child and
        // grandchild share a registration thread. `grandchild` is a fresh box with no reference
        // taken from it (only the `Arc` liveness token), and `owned_children` of `child` keeps it
        // at that address. No callback runs.
        let _ = unsafe { add_owned_child(child_id, grandchild) }
            .unwrap_or_else(|(e, _)| panic!("attach refused: {e:?}"));
    }

    assert_eq!(parent.data.owned_children().len(), 1);
    assert_eq!(parent.data.children().len(), 1);
    assert!(child_liveness.load(Ordering::SeqCst));
    assert!(grandchild_liveness.load(Ordering::SeqCst));

    // Dropping parent must recursively cascade-destroy child and grandchild
    drop(parent);

    // Both child and grandchild should be marked dead
    assert!(!child_liveness.load(Ordering::SeqCst));
    assert!(!grandchild_liveness.load(Ordering::SeqCst));

    // SAFETY: `drop(parent)` already unregistered the object in place and freed it, so this is a
    // second call for an id the registry no longer holds; it works from ids only and never reads
    // the dropped object. No callback runs: this is the only thread and the drop has returned.
    unsafe { unregister_qobject(parent_id) };
}

#[test]
fn test_child_unlink_notifies_parent() {
    let mut parent = MockContainer::new("parent");
    let parent_id = parent.data.id();
    // SAFETY: `parent` is a local that stays on this thread, unmoved, until unregistered at the
    // end of the test; it is registered with `&raw mut`, so no reference exists at registration.
    // The test afterwards only reads `parent` through the variable (rule 7); its callbacks
    // (`ChildAdded`, `ChildRemoved`) run on this thread, the registration thread.
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut parent)) };

    {
        let mut child = MockContainer::new("child");
        let child_id = child.data.id();
        let child_ptr = &raw mut child;
        // SAFETY: `child` is a local that stays on this thread, unmoved, until unregistered below;
        // the pointer is `&raw mut` and kept as `child_ptr`, so no reference exists that a later
        // callback could invalidate. No callback runs.
        unsafe { register_qobject_ptr(NonNull::new_unchecked(child_ptr)) };

        // SAFETY: `child_ptr` is the `&raw mut` pointer registered for `child` (a live local not
        // moved here), so this `&mut` of its `data` derives from it. `set_parent` delivers
        // `ChildAdded` to `parent` only, a different object, never to `child` (see
        // `set_parent_by_id`), so nothing else touches `child` while the borrow lives.
        unsafe { (*child_ptr).data.set_parent(Some(parent_id)).unwrap() };
        assert_eq!(parent.data.children().len(), 1);
        assert_eq!(parent.child_added_count.load(Ordering::SeqCst), 1);

        // Explicitly unparenting
        // SAFETY: as for the call above: `child_ptr` is the registered pointer, and the
        // `ChildRemoved` callback goes to `parent`, a different object.
        unsafe { (*child_ptr).data.set_parent(None).unwrap() };
        assert_eq!(parent.data.children().len(), 0);
        assert_eq!(parent.child_removed_count.load(Ordering::SeqCst), 1);

        // SAFETY: `child` is a live local on its registration thread, unmoved until after this
        // call, as `unregister_qobject` requires, and no callback of it runs: `set_parent` has
        // returned. No timers exist [inferred].
        unsafe { unregister_qobject(child_id) };
    }

    // SAFETY: `parent` is a live local on its registration thread, unmoved until after this call,
    // as `unregister_qobject` requires, and no callback of it runs: `set_parent` has returned. No
    // timers exist [inferred].
    unsafe { unregister_qobject(parent_id) };
}

// --- 2. Object Retrieval & Hierarchy Query Tests ---

#[test]
fn test_object_hierarchy_search_find_child() {
    let mut root = MockContainer::new("window");
    // SAFETY: `root` is a local that stays on this thread, unmoved, until it drops at the end of
    // the test, which unregisters it in place (`ObjectData::drop`). It is registered with `&raw
    // mut` and then only read directly (rule 7) before the `&mut root` of `with_child_mut`. The
    // test sends no event to `root`, so the registry's pointer is not dereferenced after that
    // borrow [inferred].
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut root)) };

    let panel = Box::new(MockContainer::new("panel"));
    let panel_id = panel.data.id();
    let btn1 = Box::new(MockButton::new("ok_button", "OK"));
    let btn2 = Box::new(MockButton::new("cancel_button", "Cancel"));
    let label = Box::new(MockLabel::new("status_label", "Ready"));

    // SAFETY: `panel` is a fresh box with no reference taken from it (only its id), `owned_children`
    // of `root` keeps it at that address, and `root` is registered on this thread, the
    // registration thread; no callback runs.
    let _ = unsafe { add_owned_child(root.data.id(), panel) }
        .unwrap_or_else(|(e, _)| panic!("attach refused: {e:?}"));
    // SAFETY: `btn1`, `btn2` and `label` are fresh boxes with no reference taken from them;
    // `owned_children` of `panel` keeps each at its address, and `panel` was registered on this
    // thread by the call above. No callback of any of them runs.
    let _ = unsafe { add_owned_child(panel_id, btn1) }
        .unwrap_or_else(|(e, _)| panic!("attach refused: {e:?}"));
    // SAFETY: as for `btn1`: `btn2` is a fresh box with no reference taken from it, `panel` keeps
    // it at that address, and no callback runs.
    let _ = unsafe { add_owned_child(panel_id, btn2) }
        .unwrap_or_else(|(e, _)| panic!("attach refused: {e:?}"));
    // SAFETY: as for `btn1`: `label` is a fresh box with no reference taken from it, `panel` keeps
    // it at that address, and no callback runs.
    let _ = unsafe { add_owned_child(panel_id, label) }
        .unwrap_or_else(|(e, _)| panic!("attach refused: {e:?}"));

    // Query by specific name and type
    let ok_btn = root.with_child::<MockButton, _>("ok_button", |b| b.text.clone());
    assert!(ok_btn.is_some());
    assert_eq!(ok_btn.unwrap(), "OK");

    let cancel_btn = root.with_child::<MockButton, _>("cancel_button", |b| b.text.clone());
    assert!(cancel_btn.is_some());
    assert_eq!(cancel_btn.unwrap(), "Cancel");

    let status_lbl = root.with_child::<MockLabel, _>("status_label", |l| l.text.clone());
    assert!(status_lbl.is_some());
    assert_eq!(status_lbl.unwrap(), "Ready");

    // Query non-existent name
    let missing = root.with_child::<MockButton, _>("non_existent", |_| ());
    assert!(missing.is_none());

    // Query with empty name matches first child of type
    let first_btn = root.with_child::<MockButton, _>("", |_| ());
    assert!(first_btn.is_some());

    // Query all children of type MockButton
    let mut all_buttons = 0;
    root.for_each_child::<MockButton>(None, |_| all_buttons += 1);
    assert_eq!(all_buttons, 2);

    // Mutable find_child_mut
    root.with_child_mut::<MockButton, _>("ok_button", |btn| {
        btn.text = "Confirmed".to_string();
    });
    let updated_ok = root
        .with_child::<MockButton, _>("ok_button", |b| b.text.clone())
        .unwrap();
    assert_eq!(updated_ok, "Confirmed");
}

#[test]
fn test_find_child_searches_direct_children_before_descendants() {
    // corelib/kernel/qobject.cpp:2242-2256 qt_qFindChild_helper
    // Qt scans direct children first, and only searches deeper descendants if no direct child matches.
    let mut root = MockContainer::new("root");
    // SAFETY: `root` is a local that stays on this thread, unmoved, until it drops at the end of
    // the test, which unregisters it in place (`ObjectData::drop`). It is registered with `&raw
    // mut` and then only read directly (rule 7) before the `&mut root` of `with_child_mut`. The
    // test sends no event to `root`, so the registry's pointer is not dereferenced after that
    // borrow [inferred].
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut root)) };

    let panel = Box::new(MockContainer::new("panel"));
    let panel_id = panel.data.id();
    let grandchild_btn = Box::new(MockButton::new("action_btn", "from_grandchild"));
    let direct_btn = Box::new(MockButton::new("action_btn", "from_direct_child"));
    let direct_btn_id = direct_btn.data.id();

    // SAFETY: `panel`, `grandchild_btn` and `direct_btn` are fresh boxes with no reference taken
    // from them (only ids); `owned_children` keeps each at its address. `root` is a registered
    // local on this thread and `panel_id` was registered by the first call, so every parent is on
    // the same registration thread as its child. No callback of any of them runs between the calls.
    unsafe {
        add_owned_child(root.data.id(), panel).ok().unwrap();
        add_owned_child(panel_id, grandchild_btn).ok().unwrap();
        add_owned_child(root.data.id(), direct_btn).ok().unwrap();
    }

    let found = root.with_child::<MockButton, _>("action_btn", |b| b.text.clone());
    assert!(found.is_some());
    assert_eq!(
        found.unwrap(),
        "from_direct_child",
        "find_child must return shallowest matching child (qt_qFindChild_helper)"
    );

    let found_id = root.find_child_id("action_btn", std::any::TypeId::of::<MockButton>());
    assert_eq!(
        found_id,
        Some(direct_btn_id),
        "find_child_id must return shallowest matching child ID"
    );

    let found_mut = root.with_child_mut::<MockButton, _>("action_btn", |b| b.text.clone());
    assert!(found_mut.is_some());
    assert_eq!(
        found_mut.unwrap(),
        "from_direct_child",
        "find_child_mut must return shallowest matching child"
    );
}

#[test]
fn test_object_name_changed_signal() {
    // corelib/kernel/qobject.cpp:1363-1373
    // QObject::setObjectName emits objectNameChanged when name changes (observed with PySide6).
    let mut obj = MockButton::new("", "");
    let emissions = Arc::new(std::sync::Mutex::new(Vec::new()));
    let em_clone = Arc::clone(&emissions);
    obj.object_name_changed().connect(move |name| {
        em_clone.lock().unwrap().push(name.clone());
    });

    obj.set_object_name("a");
    obj.set_object_name("a");
    obj.set_object_name("");
    obj.set_object_name("");

    let got = emissions.lock().unwrap().clone();
    assert_eq!(
        got,
        vec!["a".to_string(), "".to_string()],
        "setObjectName must emit objectNameChanged only on distinct changes (qobject.cpp:1371)"
    );
}
#[test]
fn test_qobject_hierarchy_accessors() {
    // SAFETY: `register_owned_qobject` requires that nothing outside a callback touches the object
    // during one: the test reaches `parent` only through `with`, which refuses while a callback
    // runs, and it defines no callback beyond counting events. The owner stays on this thread, the
    // registration thread, and is dropped when the test ends, after `child`.
    let parent = unsafe { register_owned_qobject(MockContainer::new("p")) };
    let mut child = MockButton::new("c", "Click");
    let child_id = child.object_data().id();
    let parent_id = OwnedObject::object_id(&parent);

    assert_eq!(child.parent(), None);
    assert!(child.children_ids().is_empty());
    assert!(child.object_data().children().is_empty());
    assert_eq!(child.thread(), ThreadId::current());

    child.object_data_mut().set_parent(Some(parent_id)).unwrap();
    assert_eq!(child.parent(), Some(parent_id));
    assert_eq!(parent.with(|p| p.children_ids()).unwrap(), vec![child_id]);
    assert_eq!(
        parent
            .with(|p| p.object_data().children().to_vec())
            .unwrap(),
        vec![child_id]
    );
    assert_eq!(parent.with(|p| p.thread()).unwrap(), ThreadId::current());
}

// --- 3. Thread Migration Cascading Tests ---

#[test]
fn test_move_to_thread_cascades_children_and_events() {
    let source = ThreadId::current();
    let (ran_tx, ran_rx) = channel::<ThreadId>();
    let mut parent = MockContainer::new("worker_root");
    // SAFETY: `parent` is a local that stays unmoved until the test ends, and the worker unregisters
    // it before that (the test waits for `done`). It is registered with `&raw mut`, and the test
    // only reads it afterwards (rule 7). The registration thread changes to the worker's during
    // the test; the test thread reads `parent` directly only before `go_tx` releases the worker,
    // and the worker does not touch it until then, so the two threads never overlap on it.
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut parent)) };
    let parent_id = parent.data.id();
    let child = Box::new(MockContainer::new("worker_child"));
    let child_id = child.data.id();
    // SAFETY: `child` is a fresh `Box` with no reference taken from it (only the `Arc` liveness
    // token), registered under `parent` on this thread, and `owned_children` keeps it at that
    // address; no callback runs.
    let _ = unsafe { add_owned_child(parent_id, child) }
        .unwrap_or_else(|(e, _)| panic!("attach refused: {e:?}"));
    // Posted while the subtree is still on this thread: the event must follow the child.
    let posted = Event::new(EventKind::MetaCall(Box::new(move |_| {
        ran_tx.send(ThreadId::current()).unwrap()
    })));
    post_event_to_thread(source, child_id, posted);

    let (tid_tx, tid_rx) = channel();
    let (go_tx, go_rx) = channel::<()>();
    let (done_tx, done_rx) = channel::<()>();
    let (child_thread_tx, child_thread_rx) = channel::<Option<ThreadId>>();
    let worker = std::thread::spawn(move || {
        tid_tx.send(ThreadId::current()).unwrap();
        go_rx.recv().unwrap();
        // The target thread has no loop until now: the moved event waits in its queue.
        let el = EventLoop::new();
        el.process_events(false);
        let child_thread = with_object(child_id, |c| c.object_data().thread_id());
        child_thread_tx.send(child_thread).unwrap();
        // SAFETY: after the move this thread is the registration thread of both objects. `parent`
        // is a local of the test thread and `child` a box in its `owned_children`; the test thread
        // blocks on `done_rx` and then joins this thread, so both stay allocated and unmoved until
        // both unregistrations return. No callback runs: the registry callbacks of this thread
        // ended with `process_events`, and the test thread has stopped touching `parent` since
        // `go_tx`.
        unsafe { unregister_qobject(child_id) };
        // SAFETY: as above.
        unsafe { unregister_qobject(parent_id) };
        done_tx.send(()).unwrap();
    });
    let target = tid_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("worker thread must report its id");

    assert_eq!(move_to_thread_by_id(parent_id, target, source), Ok(()));

    // Affinity and ThreadChange.
    assert_eq!(parent.data.thread_id(), target);
    assert_eq!(query_object_thread(child_id), Some(target));
    assert!(parent.thread_changed.load(Ordering::SeqCst));

    // The queued event reaches the target thread's loop, not the source.
    go_tx.send(()).unwrap();
    let ran_on = ran_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("moved event must run on the target thread");
    assert_eq!(ran_on, target);
    // Child ObjectData.thread_id, read on the thread the subtree now lives on.
    assert_eq!(
        child_thread_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("worker must report the child's thread_id"),
        Some(target)
    );
    done_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("worker thread must finish");
    worker.join().expect("worker thread must not panic");
}

// --- 4. RAII Signal Blocker Tests ---

#[test]
fn test_raii_signal_blocker() {
    let mut btn = MockButton::new("my_btn", "Click Me");
    // SAFETY: `btn` is a local that stays on this thread, unmoved, until unregistered at the end
    // of the test; it is registered with `&raw mut`, so no reference exists at registration. The
    // test later takes `&mut btn` (`SignalBlocker::new`, `block_signals`), which rule 7 says
    // invalidates the registered pointer, and that is harmless here only because the registry
    // never dereferences the pointer again: the object has no callback, and `Signal::emit` calls
    // the connected closures, not the object [inferred from the test: the only registry use after
    // registration is `unregister_qobject`, which never touches the object].
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut btn)) };

    let click_count = Arc::new(AtomicUsize::new(0));
    let counter_clone = click_count.clone();

    btn.clicked.connect(move |_| {
        counter_clone.fetch_add(1, Ordering::SeqCst);
    });

    // Initial emit works
    btn.clicked.emit(&());
    assert_eq!(click_count.load(Ordering::SeqCst), 1);

    // Block with RAII SignalBlocker
    {
        let mut blocker = SignalBlocker::new(&mut btn);
        assert!(btn.signals_blocked());

        // Emit while blocked should be suppressed
        btn.clicked.emit(&());
        assert_eq!(click_count.load(Ordering::SeqCst), 1);

        // Temporarily unblock and reblock
        blocker.unblock();
        assert!(!btn.signals_blocked());
        btn.clicked.emit(&());
        assert_eq!(click_count.load(Ordering::SeqCst), 2);

        blocker.reblock();
        assert!(btn.signals_blocked());
        btn.clicked.emit(&());
        assert_eq!(click_count.load(Ordering::SeqCst), 2);
    }

    // After blocker dropped, signals automatically restored
    assert!(!btn.signals_blocked());
    btn.clicked.emit(&());
    assert_eq!(click_count.load(Ordering::SeqCst), 3);

    // QObject::block_signals convenience method
    let prev = btn.block_signals(true);
    assert!(!prev);
    assert!(btn.signals_blocked());
    btn.clicked.emit(&());
    assert_eq!(click_count.load(Ordering::SeqCst), 3);

    let prev2 = btn.block_signals(false);
    assert!(prev2);
    assert!(!btn.signals_blocked());
    btn.clicked.emit(&());
    assert_eq!(click_count.load(Ordering::SeqCst), 4);

    // SAFETY: `btn` is a live local on its registration thread, unmoved until after this call, as
    // `unregister_qobject` requires, and no callback of it runs: signal emission calls closures,
    // not callbacks of `btn` [inferred]. No timers exist [inferred]. `btn`'s later drop unregisters
    // again, a no-op.
    unsafe { unregister_qobject(btn.data.id()) };
}

// --- 5. Guarded Pointer (QPointer) & Generational Liveness Tests ---

#[test]
fn test_qpointer_and_generational_liveness() {
    // SAFETY: `register_owned_qobject` requires that nothing outside a callback touches the object
    // during one: the test reaches it only through `registered.with`, which refuses while a
    // callback runs, and this widget defines no callback. The owner stays on this thread, the
    // registration thread, until `drop(registered)`.
    let registered = unsafe { register_owned_qobject(MockButton::new("guarded_btn", "Submit")) };
    let id = OwnedObject::object_id(&registered);

    let qptr: QPointer<MockButton> = registered.with(QPointer::new).unwrap();
    assert!(!qptr.is_null());
    assert!(qptr.is_valid());
    assert_eq!(qptr.id(), Some(id));

    // Generational identity before drop
    let gen_id_before = registered.with(|r| r.data.generational_id()).unwrap();
    assert_eq!(gen_id_before.id, id);
    assert_eq!(gen_id_before.generation, 1);

    // Drop the target object
    drop(registered);

    // Guarded pointer automatically invalidated without dangling pointer
    assert!(qptr.is_null());
    assert!(!qptr.is_valid());
    assert_eq!(qptr.id(), None);

    // SAFETY: `drop(registered)` above already unregistered the object (`OwnedObject::drop`) and
    // freed its memory, so this is a second call for an id the registry no longer holds; it works
    // from ids only and never reads or writes the freed object (`unregister_qobject` is
    // memory-neutral). No callback runs: this is the only thread.
    unsafe { unregister_qobject(id) };
}

/// Re-registering a live object (Qt re-registration path) must
/// keep it reachable. `register_object_metadata` only retires the previous record when the
/// liveness token differs, so the same object registered twice must still be `Some` in the registry.
#[test]
fn test_double_register_keeps_object_reachable() {
    // SAFETY: `register_owned_qobject` requires that nothing outside a callback touches the object
    // during one: the test reaches it only through `registered.with`, which refuses while a
    // callback runs, and this widget defines no callback. The owner stays on this thread, the
    // registration thread, and is dropped here when the test ends.
    let registered = unsafe { register_owned_qobject(MockButton::new("double_reg_btn", "Again")) };
    let id = OwnedObject::object_id(&registered);
    let qptr: QPointer<MockButton> = registered.with(QPointer::new).unwrap();

    // SAFETY: `as_non_null(&registered)` is the owner's own heap pointer, the same address the
    // first `register_owned_qobject` registered, and the object stays there while `registered`
    // lives (the owner may move, the object does not). Re-registration gives the registry this
    // pointer, and the old one is the same pointer, so nothing uses a different, stale one. No
    // callback runs: `with` on the line above has returned, and this is the only thread.
    unsafe { register_qobject_ptr(OwnedObject::<MockButton>::as_non_null(&registered)) };

    assert!(
        qptr.is_valid(),
        "second registration must not kill the live object"
    );
    assert_eq!(qptr.id(), Some(id));
    let found = ferrokit_core::object::with_object(id, |obj| obj.object_data().id());
    assert_eq!(
        found,
        Some(id),
        "with_object must still resolve the double-registered object"
    );

    // SAFETY: `registered` is still alive here (it drops at the end of the test), so the memory is
    // valid, and no callback of the widget runs: `with_object` above has returned and this is the
    // only thread. No timers exist [inferred]. The owner's later drop unregisters again, a no-op.
    unsafe { unregister_qobject(id) };
}
