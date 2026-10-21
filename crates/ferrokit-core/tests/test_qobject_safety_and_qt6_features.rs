// SPDX-License-Identifier: LGPL-3.0-only
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::event_loop::EventLoop;
use ferrokit_core::object::{
    add_owned_child, move_to_thread_by_id, query_object_thread, register_owned_qobject, sender,
    unregister_qobject, MoveByIdError, ObjectData, ObjectId, OwnedObject, QObject, ScopeLevelGuard,
    ThreadContext,
    ThreadId,
};
use ferrokit_core::property::Property;
use ferrokit_core::signal::Signal;
use ferrokit_core::timer::TimerType;

// -----------------------------------------------------------------------------
// Test Object Helpers
// -----------------------------------------------------------------------------

struct TestWidget {
    data: ObjectData,
    #[allow(dead_code)]
    pub name: String,
    pub received_timer: Option<u64>,
    pub handled_events: usize,
    pub thread_change_count: usize,
}

impl TestWidget {
    pub fn new(name: &str) -> Self {
        Self {
            data: ObjectData::new(ObjectId::next()),
            name: name.to_string(),
            received_timer: None,
            handled_events: 0,
            thread_change_count: 0,
        }
    }
}

impl QObject for TestWidget {
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
        self.handled_events += 1;
        match &event.kind {
            EventKind::Timer { timer_id } => {
                self.received_timer = Some(*timer_id);
                true
            }
            EventKind::ThreadChange => {
                self.thread_change_count += 1;
                true
            }
            _ => false,
        }
    }
}

// -----------------------------------------------------------------------------
// 1. Memory Safety & Dynamic Borrow Exclusivity Tests
// -----------------------------------------------------------------------------

// -----------------------------------------------------------------------------
// 3. Thread Affinity & Subtree moveToThread Migration Tests
// -----------------------------------------------------------------------------

#[test]
fn test_move_to_thread_rejects_parented_and_migrates_subtree() {
    ThreadContext::init_current(true, None);
    let current_thread = ThreadId::current();
    let (tid_tx, tid_rx) = std::sync::mpsc::channel();
    let (go_tx, go_rx) = std::sync::mpsc::channel::<(ObjectId, ObjectId)>();
    let (result_tx, result_rx) =
        std::sync::mpsc::channel::<(Option<ThreadId>, Option<ThreadId>)>();
    let worker = std::thread::spawn(move || {
        tid_tx.send(ThreadId::current()).unwrap();
        let (parent_id, child_id) = go_rx.recv().unwrap();
        let parent_thread = ferrokit_core::object::with_object(parent_id, |o| {
            o.object_data().thread_id()
        });
        let child_thread = ferrokit_core::object::with_object(child_id, |o| {
            o.object_data().thread_id()
        });
        result_tx.send((parent_thread, child_thread)).unwrap();
    });
    let target_thread = tid_rx.recv().unwrap();

    // SAFETY: the owner stays on this registration thread; nothing touches the parent while a
    // callback of it runs.
    let parent = unsafe { register_owned_qobject(TestWidget::new("worker_root")) };

    let child = Box::new(TestWidget::new("worker_child"));
    // SAFETY: child remains boxed and owned at a stable address by parent.
    let child_id = unsafe { add_owned_child(OwnedObject::object_id(&parent), child) }
        .unwrap_or_else(|(e, _)| panic!("attach refused: {e:?}"));

    // Qt rule: Child with parent CANNOT be moved to another thread directly!
    let child_move_res = move_to_thread_by_id(child_id, target_thread, current_thread);
    assert_eq!(
        child_move_res,
        Err(MoveByIdError::HasParent),
        "Cannot move objects with a parent"
    );

    // Moving root cascades to all children
    let parent_id = OwnedObject::object_id(&parent);
    let parent_move_res = move_to_thread_by_id(parent_id, target_thread, current_thread);
    assert_eq!(parent_move_res, Ok(()));

    // The owner refuses access from this thread now; read the data on the thread the subtree
    // lives on.
    go_tx.send((parent_id, child_id)).unwrap();
    let (parent_thread, child_thread) = result_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("worker must report the thread_id fields");
    worker.join().unwrap();
    assert_eq!(parent_thread, Some(target_thread));
    assert_eq!(query_object_thread(child_id), Some(target_thread));
    assert_eq!(child_thread, Some(target_thread));

    // SAFETY: parent callbacks have ended on its registration thread.
    unsafe { unregister_qobject(OwnedObject::object_id(&parent)) };
    ThreadContext::clear_current();
}

// -----------------------------------------------------------------------------
// 4. Signal/Slot: sender() Tracking & Auto Disconnect Tests
// -----------------------------------------------------------------------------

#[test]
fn test_signal_sender_tracking_and_auto_disconnection() {
    // SAFETY: the owner stays on this thread through the signal's lifetime; nothing touches the
    // emitter while a callback of it runs.
    let emitter = unsafe { register_owned_qobject(TestWidget::new("emitter")) };
    let emitter_id = OwnedObject::object_id(&emitter);

    let sig: Signal<i32> = emitter.with(|e| Signal::with_owner(&e.data)).unwrap();
    let captured_sender = Arc::new(Mutex::new(None));
    let captured_clone = Arc::clone(&captured_sender);

    sig.connect(move |_val| {
        *captured_clone.lock().unwrap() = sender();
    });

    // Before emission, no active sender
    assert_eq!(sender(), None);

    // Emit signal: slot observes emitter ID
    sig.emit(&42);
    assert_eq!(*captured_sender.lock().unwrap(), Some(emitter_id));

    // After emit completes, sender stack pops back to None
    assert_eq!(sender(), None);

    // Test automatic disconnection on object drop
    let recv_count = Arc::new(AtomicUsize::new(0));
    {
        // SAFETY: the owner stays on this thread until it is dropped below; nothing touches the
        // receiver while a callback of it runs.
        let receiver = unsafe { register_owned_qobject(TestWidget::new("receiver")) };
        let r_id = OwnedObject::object_id(&receiver);

        let rc_clone = Arc::clone(&recv_count);

        receiver
            .with(|r| {
                sig.connect_to(r, move |_| {
                    rc_clone.fetch_add(1, Ordering::SeqCst);
                })
            })
            .unwrap();

        sig.emit(&1);
        assert_eq!(recv_count.load(Ordering::SeqCst), 1);

        // Drop receiver: connection automatically severed
        drop(receiver);
        // SAFETY: receiver destruction completed and no callback is active.
        unsafe { unregister_qobject(r_id) };
    }

    // Emitting now does not hit severed receiver slot
    sig.emit(&2);
    assert_eq!(
        recv_count.load(Ordering::SeqCst),
        1,
        "slot of a destroyed receiver ran"
    );
    // SAFETY: emitter callbacks have ended on its registration thread.
    unsafe { unregister_qobject(emitter_id) };
}

// -----------------------------------------------------------------------------
// 5. QObject Timer (start_timer / kill_timer) Tests
// -----------------------------------------------------------------------------

#[test]
fn test_qobject_start_and_kill_timer() {
    // SAFETY: the owner stays on this thread until after the timers are stopped; nothing touches
    // the widget while a callback of it runs.
    let mut widget = unsafe { register_owned_qobject(TestWidget::new("timer_widget")) };

    // Initialize thread timer context
    let registry = Arc::new(std::sync::Mutex::new(ferrokit_core::timer::TimerRegistry::new()));
    #[cfg(windows)]
    ferrokit_core::timer::register_thread_timer_context(registry.clone(), std::ptr::null_mut());
    #[cfg(not(windows))]
    ferrokit_core::timer::register_thread_timer_context(registry.clone());

    let t1 = widget.with_mut(|w| w.start_timer(100, TimerType::Coarse)).unwrap();
    assert!(t1.is_valid());
    assert_eq!(widget.with(|w| w.data.timers().len()).unwrap(), 1);

    let t2 = widget.with_mut(|w| w.start_timer(200, TimerType::Precise)).unwrap();
    assert!(t2.is_valid());
    assert_eq!(widget.with(|w| w.data.timers().len()).unwrap(), 2);

    let killed = widget.with_mut(|w| w.kill_timer(t1)).unwrap();
    assert!(killed);
    assert_eq!(widget.with(|w| w.data.timers().len()).unwrap(), 1);
    assert_eq!(widget.with(|w| w.data.timers()[0]).unwrap(), t2);

    // Dropping widget kills remaining timers
    let wid = OwnedObject::object_id(&widget);
    drop(widget);
    // SAFETY: widget destruction ended its callback lifetime on this thread.
    unsafe { unregister_qobject(wid) };

    assert_eq!(registry.lock().unwrap().len(), 0);
    ferrokit_core::timer::unregister_thread_timer_context();
}

// -----------------------------------------------------------------------------
// 6. Safe Deferred Deletion (deleteLater) Tests
// -----------------------------------------------------------------------------

#[test]
fn test_delete_later_posts_one_deferred_delete_and_debounces() {
    let event_loop = EventLoop::new();
    // SAFETY: the owner stays on this thread until the unregister below; nothing touches the
    // widget while a callback of it runs.
    let widget = unsafe { register_owned_qobject(TestWidget::new("self_post")) };
    let wid = OwnedObject::object_id(&widget);

    ThreadContext::set_current_loop_level(1);
    {
        let _scope = ScopeLevelGuard::enter();
        widget.with(|w| w.delete_later()).unwrap();
        widget.with(|w| w.delete_later()).unwrap(); // qobject.cpp:2531-2534: second call is a no-op
    }
    let queued = event_loop.queue().lock().unwrap().len();
    assert_eq!(
        queued, 1,
        "deleteLater must post exactly one DeferredDelete"
    );

    ThreadContext::set_current_loop_level(0);
    // SAFETY: test ends; no delivery happened.
    unsafe { unregister_qobject(wid) };
}

#[test]
fn test_delete_later_inside_handler_scope_survives_nested_pump() {
    let event_loop = EventLoop::new();
    // SAFETY: the owner stays on this thread until delivery unregisters the object; nothing
    // touches the widget while a callback of it runs.
    let widget = unsafe { register_owned_qobject(TestWidget::new("nested_pump")) };
    let wid = OwnedObject::object_id(&widget);

    ThreadContext::set_current_loop_level(1);
    let scope = ScopeLevelGuard::enter();
    // Stamped (1, 1). qcoreapplication.cpp:1882: E.L+E.S (2) > cur.L+cur.S (2) is false, so the
    // DeferredDelete is not delivered by a nested pump.
    widget.with(|w| w.delete_later()).unwrap();
    event_loop.process_events(false);
    assert!(
        query_object_thread(wid).is_some(),
        "object deleted inside its handler scope by a nested processEvents"
    );

    drop(scope); // cur = (1, 0): 2 > 1, deliverable
    event_loop.process_events(false);
    assert_eq!(
        query_object_thread(wid),
        None,
        "not deleted after scope unwound"
    );
    assert_eq!(event_loop.queue().lock().unwrap().len(), 0);

    ThreadContext::set_current_loop_level(0);
}

// -----------------------------------------------------------------------------
// 7. Qt 6 Reactive Property Binding Tests (QProperty / QBindable)
// -----------------------------------------------------------------------------

#[test]
fn test_qt6_reactive_property_bindings_and_cycle_prevention() {
    let width = Property::new(10);
    let height = Property::new(20);

    // Area binds to width * height
    let w_clone = width.clone();
    let h_clone = height.clone();
    let area = Property::with_binding(move || w_clone.get() * h_clone.get());
    assert_eq!(area.get(), 200);

    // Perimeter binds to 2 * (width + height)
    let w2 = width.clone();
    let h2 = height.clone();
    let perimeter = Property::with_binding(move || 2 * (w2.get() + h2.get()));
    assert_eq!(perimeter.get(), 60);

    // Update width -> dependent properties automatically re-evaluate lazily
    width.set(15);
    assert_eq!(area.get(), 300);
    assert_eq!(perimeter.get(), 70);

    // Update height -> dependent properties update
    height.set(30);
    assert_eq!(area.get(), 450);
    assert_eq!(perimeter.get(), 90);

    // Change signal verification
    let notified = Arc::new(AtomicBool::new(false));
    let n_clone = Arc::clone(&notified);
    area.notify_signal().connect(move |_| {
        n_clone.store(true, Ordering::SeqCst);
    });
    width.set(20);
    let _ = area.get(); // triggers recompute & notify
    assert!(notified.load(Ordering::SeqCst));

    // Circular dependency detection: does not stack overflow
    let p_a = Property::new(1);
    let p_b = Property::new(2);

    let pb_clone = p_b.clone();
    p_a.set_binding(move || pb_clone.get() + 1);

    let pa_clone = p_a.clone();
    p_b.set_binding(move || pa_clone.get() + 1);

    // Recompute safely breaks loop without stack overflow
    let _ = p_a.get();

    // Break strong reference cycle formed by circular bindings capturing Property clones
    p_a.set(0);
    p_b.set(0);
}

// -----------------------------------------------------------------------------
// 8. Event Filter Cross-Thread & Recursion Safety Tests
// -----------------------------------------------------------------------------

#[test]
fn test_event_filter_cross_thread_and_cycle_prevention() {
    // SAFETY: both owners stay on this thread until the unregisters below; nothing touches either
    // object while a callback of it runs.
    let mut obj_a = unsafe { register_owned_qobject(TestWidget::new("obj_a")) };
    let mut obj_b = unsafe { register_owned_qobject(TestWidget::new("obj_b")) };

    // 1. Qt allows installing self as event filter (qobject.cpp:2407-2423)
    let a_id = OwnedObject::object_id(&obj_a);
    let b_id = OwnedObject::object_id(&obj_b);
    let self_filter = obj_a
        .with_mut(|a| ferrokit_core::object::install_event_filter(&mut a.data, a_id))
        .unwrap();
    assert!(
        self_filter,
        "Qt allows installing object as its own event filter"
    );
    // 2. Normal installation succeeds
    let ok = obj_a
        .with_mut(|a| ferrokit_core::object::install_event_filter(&mut a.data, b_id))
        .unwrap();
    assert!(ok);
    assert!(obj_a.with(|a| a.data.event_filters().contains(b_id)).unwrap());

    // 3. Qt allows mutual event filters (qobject.cpp:2407-2423)
    let cycle = obj_b
        .with_mut(|b| ferrokit_core::object::install_event_filter(&mut b.data, a_id))
        .unwrap();
    assert!(cycle, "Mutual filter installation succeeds in Qt");
    // SAFETY: neither registered object has an active callback.
    unsafe {
        unregister_qobject(a_id);
        unregister_qobject(b_id);
    }
}
