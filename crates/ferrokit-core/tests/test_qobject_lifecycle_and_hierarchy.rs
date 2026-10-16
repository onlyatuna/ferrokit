// SPDX-License-Identifier: LGPL-3.0-only
use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::event_loop::{post_event_to_thread, EventLoop};
use ferrokit_core::object::{
    add_owned_child, move_to_thread_by_id, query_object_thread, register_owned_qobject,
    register_qobject_ptr, unregister_qobject, ObjectData, ObjectId, OwnedObject, QObject,
    QObjectExt, QPointer, SignalBlocker, ThreadId,
};
use ferrokit_core::signal::Signal;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::channel;
use std::sync::Arc;
use std::time::Duration;

// --- Test Structures ---

struct MockContainer {
    data: ObjectData,
    child_removed_count: Arc<AtomicUsize>,
    child_added_count: Arc<AtomicUsize>,
    thread_changed: Arc<AtomicBool>,
}

impl MockContainer {
    fn new(name: &str) -> Self {
        let mut data = ObjectData::new(ObjectId::next());
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
    fn object_data_mut(&mut self) -> &mut ObjectData {
        &mut self.data
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
        let mut data = ObjectData::new(id);
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
    fn object_data_mut(&mut self) -> &mut ObjectData {
        &mut self.data
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
        let mut data = ObjectData::new(ObjectId::next());
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
    fn object_data_mut(&mut self) -> &mut ObjectData {
        &mut self.data
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
    let parent_id = parent.data.id;
    // SAFETY: parent stays on this thread and alive until the explicit drop.
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

        let child_id = child.data.id;
        // SAFETY: parent is registered; the Box stays at a stable address until cascade drop.
        let _ = unsafe { add_owned_child(parent_id, child) }
            .unwrap_or_else(|(e, _)| panic!("attach refused: {e:?}"));
        // SAFETY: child was registered by the attach above and stays owned by parent.
        let _ = unsafe { add_owned_child(child_id, grandchild) }
            .unwrap_or_else(|(e, _)| panic!("attach refused: {e:?}"));
    }

    assert_eq!(parent.data.owned_children.len(), 1);
    assert_eq!(parent.data.children.len(), 1);
    assert!(child_liveness.load(Ordering::SeqCst));
    assert!(grandchild_liveness.load(Ordering::SeqCst));

    // Dropping parent must recursively cascade-destroy child and grandchild
    drop(parent);

    // Both child and grandchild should be marked dead
    assert!(!child_liveness.load(Ordering::SeqCst));
    assert!(!grandchild_liveness.load(Ordering::SeqCst));

    // SAFETY: parent destruction and all callbacks are complete on this thread.
    unsafe { unregister_qobject(parent_id) };
}

#[test]
fn test_child_unlink_notifies_parent() {
    let mut parent = MockContainer::new("parent");
    let parent_id = parent.data.id;
    // SAFETY: parent stays on this thread and alive until unregistered.
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut parent)) };

    {
        let mut child = MockContainer::new("child");
        let child_id = child.data.id;
        // SAFETY: child stays on this thread and alive until unregistered.
        unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut child)) };

        child.data.set_parent(Some(parent_id)).unwrap();
        assert_eq!(parent.data.children.len(), 1);
        assert_eq!(parent.child_added_count.load(Ordering::SeqCst), 1);

        // Explicitly unparenting
        child.data.set_parent(None).unwrap();
        assert_eq!(parent.data.children.len(), 0);
        assert_eq!(parent.child_removed_count.load(Ordering::SeqCst), 1);

        // SAFETY: child callback is no longer active and this is its registration thread.
        unsafe { unregister_qobject(child_id) };
    }

    // SAFETY: parent has no active callbacks and remains on this thread.
    unsafe { unregister_qobject(parent_id) };
}

// --- 2. Object Retrieval & Hierarchy Query Tests ---

#[test]
fn test_object_hierarchy_search_find_child() {
    let mut root = MockContainer::new("window");
    // SAFETY: root stays on this thread and alive through all hierarchy queries.
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut root)) };

    let panel = Box::new(MockContainer::new("panel"));
    let panel_id = panel.data.id;
    let btn1 = Box::new(MockButton::new("ok_button", "OK"));
    let btn2 = Box::new(MockButton::new("cancel_button", "Cancel"));
    let label = Box::new(MockLabel::new("status_label", "Ready"));

    // SAFETY: panel stays boxed at a stable address while root owns it; the attach registers it.
    let _ = unsafe { add_owned_child(root.data.id, panel) }
        .unwrap_or_else(|(e, _)| panic!("attach refused: {e:?}"));
    // SAFETY: all children remain boxed at stable addresses while panel owns them.
    let _ = unsafe { add_owned_child(panel_id, btn1) }
        .unwrap_or_else(|(e, _)| panic!("attach refused: {e:?}"));
    let _ = unsafe { add_owned_child(panel_id, btn2) }
        .unwrap_or_else(|(e, _)| panic!("attach refused: {e:?}"));
    let _ = unsafe { add_owned_child(panel_id, label) }
        .unwrap_or_else(|(e, _)| panic!("attach refused: {e:?}"));

    // Query by specific name and type
    let ok_btn = root.find_child::<MockButton>("ok_button");
    assert!(ok_btn.is_some());
    assert_eq!(ok_btn.unwrap().text, "OK");

    let cancel_btn = root.find_child::<MockButton>("cancel_button");
    assert!(cancel_btn.is_some());
    assert_eq!(cancel_btn.unwrap().text, "Cancel");

    let status_lbl = root.find_child::<MockLabel>("status_label");
    assert!(status_lbl.is_some());
    assert_eq!(status_lbl.unwrap().text, "Ready");

    // Query non-existent name
    let missing = root.find_child::<MockButton>("non_existent");
    assert!(missing.is_none());

    // Query with empty name matches first child of type
    let first_btn = root.find_child::<MockButton>("");
    assert!(first_btn.is_some());

    // Query all children of type MockButton
    let all_buttons = root.find_children::<MockButton>(None);
    assert_eq!(all_buttons.len(), 2);

    // Mutable find_child_mut
    if let Some(btn) = root.find_child_mut::<MockButton>("ok_button") {
        btn.text = "Confirmed".to_string();
    }
    let updated_ok = root.find_child::<MockButton>("ok_button").unwrap();
    assert_eq!(updated_ok.text, "Confirmed");
}

#[test]
fn test_find_child_searches_direct_children_before_descendants() {
    // corelib/kernel/qobject.cpp:2242-2256 qt_qFindChild_helper
    // Qt scans direct children first, and only searches deeper descendants if no direct child matches.
    let mut root = MockContainer::new("root");
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut root)) };

    let panel = Box::new(MockContainer::new("panel"));
    let panel_id = panel.data.id;
    let grandchild_btn = Box::new(MockButton::new("action_btn", "from_grandchild"));
    let direct_btn = Box::new(MockButton::new("action_btn", "from_direct_child"));
    let direct_btn_id = direct_btn.data.id;

    unsafe {
        add_owned_child(root.data.id, panel).ok().unwrap();
        add_owned_child(panel_id, grandchild_btn).ok().unwrap();
        add_owned_child(root.data.id, direct_btn).ok().unwrap();
    }

    let found = root.find_child::<MockButton>("action_btn");
    assert!(found.is_some());
    assert_eq!(
        found.unwrap().text,
        "from_direct_child",
        "find_child must return shallowest matching child (qt_qFindChild_helper)"
    );

    let found_id = root.find_child_id("action_btn", std::any::TypeId::of::<MockButton>());
    assert_eq!(
        found_id,
        Some(direct_btn_id),
        "find_child_id must return shallowest matching child ID"
    );

    let found_mut = root.find_child_mut::<MockButton>("action_btn");
    assert!(found_mut.is_some());
    assert_eq!(
        found_mut.unwrap().text,
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
    let mut parent = MockContainer::new("p");
    let mut child = MockButton::new("c", "Click");
    let child_id = child.object_data().id;
    let parent_id = parent.object_data().id;

    assert_eq!(child.parent(), None);
    assert!(child.children_ids().is_empty());
    assert!(child.object_data().children().is_empty());
    assert_eq!(child.thread(), ThreadId::current());

    parent.object_data_mut().add_child(child_id);
    child.object_data_mut().parent = Some(parent_id);

    assert_eq!(child.parent(), Some(parent_id));
    assert_eq!(parent.children_ids(), vec![child_id]);
    assert_eq!(parent.object_data().children(), &[child_id]);
    assert_eq!(parent.thread(), ThreadId::current());
}

// --- 3. Thread Migration Cascading Tests ---

#[test]
fn test_move_to_thread_cascades_children_and_events() {
    let source = ThreadId::current();
    let (ran_tx, ran_rx) = channel::<ThreadId>();
    let mut parent = MockContainer::new("worker_root");
    // SAFETY: parent stays on this thread and alive until the worker has unregistered it.
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut parent)) };
    let parent_id = parent.data.id;
    let child = Box::new(MockContainer::new("worker_child"));
    let child_id = child.data.id;
    // SAFETY: child remains boxed and owned by parent at a stable address.
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
    let worker = std::thread::spawn(move || {
        tid_tx.send(ThreadId::current()).unwrap();
        go_rx.recv().unwrap();
        // The target thread has no loop until now: the moved event waits in its queue.
        let el = EventLoop::new();
        el.process_events(false);
        // SAFETY: after the move this thread is the registration thread of both objects, and
        // the test thread keeps them alive until `done` is received.
        unsafe { unregister_qobject(child_id) };
        unsafe { unregister_qobject(parent_id) };
        done_tx.send(()).unwrap();
    });
    let target = tid_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("worker thread must report its id");

    assert_eq!(move_to_thread_by_id(parent_id, target, source), Ok(()));

    // Affinity and ThreadChange.
    assert_eq!(parent.data.thread_id, target);
    assert_eq!(
        parent.data.owned_children[0].object_data().thread_id,
        target
    );
    assert_eq!(query_object_thread(child_id), Some(target));
    assert!(parent.thread_changed.load(Ordering::SeqCst));

    // The queued event reaches the target thread's loop, not the source.
    go_tx.send(()).unwrap();
    let ran_on = ran_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("moved event must run on the target thread");
    assert_eq!(ran_on, target);
    done_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("worker thread must finish");
    worker.join().expect("worker thread must not panic");
}

// --- 4. RAII Signal Blocker Tests ---

#[test]
fn test_raii_signal_blocker() {
    let mut btn = MockButton::new("my_btn", "Click Me");
    // SAFETY: button stays on this thread and alive until unregistered.
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

    // SAFETY: button callbacks have returned on this registration thread.
    unsafe { unregister_qobject(btn.data.id) };
}

// --- 5. Guarded Pointer (QPointer) & Generational Liveness Tests ---

#[test]
fn test_qpointer_and_generational_liveness() {
    // SAFETY: the owner stays on this thread; no callback of the widget runs in this test.
    let registered = unsafe { register_owned_qobject(MockButton::new("guarded_btn", "Submit")) };
    let id = OwnedObject::object_id(&registered);

    let qptr: QPointer<MockButton> = QPointer::new(&*registered);
    assert!(!qptr.is_null());
    assert!(qptr.is_valid());
    assert_eq!(qptr.id(), Some(id));

    // Generational identity before drop
    let gen_id_before = registered.data.generational_id();
    assert_eq!(gen_id_before.id, id);
    assert_eq!(gen_id_before.generation, 1);

    // Drop the target object
    drop(registered);

    // Guarded pointer automatically invalidated without dangling pointer
    assert!(qptr.is_null());
    assert!(!qptr.is_valid());
    assert_eq!(qptr.id(), None);

    // SAFETY: dropping the owner ended its callback lifetime.
    unsafe { unregister_qobject(id) };
}

/// Re-registering a live object (Qt re-registration path, e.g. Timer restart) must
/// keep it reachable. `register_object_metadata` only retires the previous record when the
/// liveness token differs, so the same object registered twice must still be `Some` in the registry.
#[test]
fn test_double_register_keeps_object_reachable() {
    // SAFETY: the owner stays on this thread; no callback of the widget runs in this test.
    let registered =
        unsafe { register_owned_qobject(MockButton::new("double_reg_btn", "Again")) };
    let id = OwnedObject::object_id(&registered);
    let qptr: QPointer<MockButton> = QPointer::new(&*registered);

    // SAFETY: the object stays at this address while `registered` lives, and the pointer is the
    // owner's own, the one the first registration used.
    unsafe { register_qobject_ptr(OwnedObject::<MockButton>::as_non_null(&registered)) };

    assert!(
        qptr.is_valid(),
        "second registration must not kill the live object"
    );
    assert_eq!(qptr.id(), Some(id));
    let found = ferrokit_core::object::with_object(id, |obj| obj.object_data().id);
    assert_eq!(
        found,
        Some(id),
        "with_object must still resolve the double-registered object"
    );

    // SAFETY: no callbacks are active on this registration thread.
    unsafe { unregister_qobject(id) };
}
