// SPDX-License-Identifier: LGPL-3.0-only
use ferrokit_core::event_loop::EventLoop;
use ferrokit_core::object::{
    ObjectData, ObjectId, QObject, ThreadId, register_qobject_ptr, unregister_qobject,
};
use ferrokit_core::signal::{ConnectionId, ConnectionType, QueuedSignal, Signal};
use std::cell::RefCell;
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex};

// Non-Send, non-Sync, non-Clone, non-'static mock data models
struct NonSendDataModel {
    name: String,
    // Rc is explicitly !Send and !Sync, and NonSendDataModel does NOT implement Clone
    reference: Rc<RefCell<Vec<i32>>>,
}

struct LifetimeBoundDataModel<'a> {
    text: &'a str,
    slice: &'a [u8],
}

struct MockReceiverWidget {
    data: ObjectData,
    #[allow(dead_code)]
    call_count: Arc<AtomicI32>,
}

impl MockReceiverWidget {
    fn new(id: ObjectId, call_count: Arc<AtomicI32>) -> Self {
        Self {
            data: ObjectData::new(id),
            call_count,
        }
    }
}

impl QObject for MockReceiverWidget {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }

    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
    }
}

#[test]
fn test_direct_signal_with_non_send_non_clone_payload() {
    // NonSendDataModel is !Clone and contains Rc. It cannot cross threads or be queued,
    // but works effortlessly with Qt Direct Signal in same thread.
    let signal: Signal<NonSendDataModel> = Signal::new();

    let storage = Arc::new(Mutex::new(Vec::new()));
    let storage_clone = Arc::clone(&storage);

    // Direct connection closure: receives &NonSendDataModel synchronously
    signal.connect(move |model| {
        storage_clone.lock().unwrap().push(model.name.len() as i32);
        storage_clone
            .lock()
            .unwrap()
            .extend(model.reference.borrow().iter());
    });

    let internal_rc = Rc::new(RefCell::new(vec![10, 20, 30]));
    let payload = NonSendDataModel {
        name: "DirectSignal".to_string(),
        reference: internal_rc,
    };

    signal.emit(&payload);

    assert_eq!(*storage.lock().unwrap(), vec![12, 10, 20, 30]);
}

#[test]
fn test_direct_signal_with_borrowed_lifetime_payload() {
    // Payload contains explicit non-'static references (&'a str, &'a [u8])
    let local_string = String::from("Ephemeral Scope Data");
    let local_bytes = [1u8, 2, 3, 4, 5];

    let signal: Signal<LifetimeBoundDataModel<'_>> = Signal::new();

    let captured = Arc::new(Mutex::new(String::new()));
    let cap_clone = Arc::clone(&captured);

    signal.connect(move |data| {
        let mut s = cap_clone.lock().unwrap();
        s.push_str(data.text);
        s.push_str(&format!(":{}", data.slice.len()));
    });

    let payload = LifetimeBoundDataModel {
        text: &local_string,
        slice: &local_bytes,
    };

    signal.emit(&payload);

    assert_eq!(*captured.lock().unwrap(), "Ephemeral Scope Data:5");
}

#[test]
fn test_direct_to_qobject_lifecycle_auto_disconnect() {
    let signal: Signal<NonSendDataModel> = Signal::new();
    let counter = Arc::new(AtomicI32::new(0));

    let receiver = MockReceiverWidget::new(ObjectId::next(), Arc::clone(&counter));
    let receiver_id = receiver.object_data().id();

    let cnt = Arc::clone(&counter);
    signal.connect_direct_to(&receiver, move |_| {
        cnt.fetch_add(1, Ordering::SeqCst);
    });

    let dummy_rc = Rc::new(RefCell::new(vec![]));
    signal.emit(&NonSendDataModel {
        name: "Test1".to_string(),
        reference: Rc::clone(&dummy_rc),
    });
    assert_eq!(counter.load(Ordering::SeqCst), 1);

    // Disconnect when receiver is destroyed
    ferrokit_core::signal::disconnect_all_for_object(receiver_id);

    signal.emit(&NonSendDataModel {
        name: "Test2".to_string(),
        reference: dummy_rc,
    });
    // Should NOT increment because connection was cleaned up
    assert_eq!(counter.load(Ordering::SeqCst), 1);
}

#[test]
fn test_queued_signal_cross_thread_dispatch() {
    // Queued signal requires Send + 'static + Clone
    let signal: QueuedSignal<String> = Signal::new();
    let receiver_thread = ThreadId::current();

    let received = Arc::new(Mutex::new(String::new()));
    let rec_clone = Arc::clone(&received);

    let event_loop = EventLoop::new();

    signal.connect_queued(receiver_thread, move |val| {
        *rec_clone.lock().unwrap() = val.clone();
    });

    // Emit from background thread
    let sig_clone = signal.clone();
    let handle = std::thread::spawn(move || {
        sig_clone.emit(&"Message From Worker Thread".to_string());
    });
    handle.join().unwrap();

    // Before event loop processes, slot has not run
    assert_eq!(*received.lock().unwrap(), "");

    // Process event loop in receiver thread
    let dispatched = event_loop.process_events(false);
    assert!(dispatched);

    assert_eq!(*received.lock().unwrap(), "Message From Worker Thread");
}

#[test]
fn test_blocking_queued_signal_cross_thread_synchronization() {
    let signal: Signal<i32> = Signal::new();
    let main_thread = ThreadId::current();
    let mut receiver = Probe {
        data: ObjectData::new(ObjectId::next()),
    };
    let receiver_id = receiver.data.id();
    // SAFETY: the pointer comes from `&raw mut receiver`, so no reference exists that a later use
    // of `receiver` could invalidate. `receiver` is a local that is not moved or dropped until
    // after the `unregister_qobject` below, and it is not used directly in between (its id was
    // copied out before). This thread is the registration thread, and the queued call reaches the
    // object only through the registry in `process_events` here; the worker only posts by id.
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut receiver)) };

    let processed = Arc::new(AtomicBool::new(false));
    let proc_clone = Arc::clone(&processed);

    let event_loop = EventLoop::new();

    signal.connect_blocking_queued(receiver_id, main_thread, move |val| {
        assert_eq!(*val, 777);
        proc_clone.store(true, Ordering::SeqCst);
    });

    let sig_clone = signal.clone();
    let worker_finished = Arc::new(AtomicBool::new(false));
    let worker_flag = Arc::clone(&worker_finished);

    let worker = std::thread::spawn(move || {
        // This emit blocks until the main thread processes the queued event
        sig_clone.emit(&777);
        worker_flag.store(true, Ordering::SeqCst);
    });

    // The worker cannot finish before this thread runs its call, however long it takes to post it.
    assert!(!worker_finished.load(Ordering::SeqCst));

    // Main thread processes the event once the worker has posted it. How long the worker takes to
    // get there is not part of what this test checks (under Miri it takes many milliseconds of
    // the virtual clock), so pump until the slot has run, with a watchdog.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !processed.load(Ordering::SeqCst) {
        assert!(
            std::time::Instant::now() < deadline,
            "watchdog: the blocking queued call was not delivered within 10s"
        );
        event_loop.process_events(false);
        std::thread::sleep(std::time::Duration::from_millis(1));
    }

    // Worker can now unblock and terminate
    worker.join().unwrap();
    assert!(worker_finished.load(Ordering::SeqCst));
    // SAFETY: `unregister_qobject` needs `receiver` alive and unmoved until it returns, on its
    // registration thread, with no callback active. `receiver` is the local above, dropped only at
    // the end of the fn. This is the registering thread, the worker is joined, and
    // `process_events` has returned, so no callback is active.
    unsafe { unregister_qobject(receiver_id) };
}

struct Probe {
    data: ObjectData,
}

impl QObject for Probe {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }

    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
    }
}

#[test]
fn test_queued_signal_to_unregistered_receiver_never_runs() {
    let signal: Signal<i32> = Signal::new();
    let main_thread = ThreadId::current();
    let mut receiver = Probe {
        data: ObjectData::new(ObjectId::next()),
    };
    let receiver_id = receiver.data.id();
    // SAFETY: the pointer comes from `&raw mut receiver`, so no reference exists that a later use
    // of `receiver` could invalidate. `receiver` is a local that is not moved or dropped until
    // after the `unregister_qobject` below, and it is not used directly in between (its id was
    // copied out before). This thread is the registration thread and the only one that runs its
    // callbacks.
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut receiver)) };

    let ran = Arc::new(AtomicBool::new(false));
    let ran_clone = Arc::clone(&ran);
    signal.connect_object(
        receiver_id,
        main_thread,
        ConnectionType::Queued,
        move |_| {
            ran_clone.store(true, Ordering::SeqCst);
        },
    );

    // Posted to the main thread before its loop exists, so the call waits in the pending queue.
    let sig_clone = signal.clone();
    std::thread::spawn(move || sig_clone.emit(&1))
        .join()
        .unwrap();
    // SAFETY: `unregister_qobject` needs `receiver` alive and unmoved until it returns, on its
    // registration thread, with no callback active. `receiver` is the local above, dropped only at
    // the end of the fn. This is the thread that registered it, and no loop has run and none is
    // running, so no callback is active.
    unsafe { unregister_qobject(receiver_id) };

    let event_loop = EventLoop::new();
    event_loop.send_posted_events();
    assert!(
        !ran.load(Ordering::SeqCst),
        "queued slot must not run after its receiver is destroyed"
    );
}

// Queued connections without a receiver return an invalid connection rather than panicking.
#[test]
fn connect_with_type_queued_without_receiver_is_rejected() {
    let signal: Signal<i32> = Signal::new();
    let id = signal.connect_with_type(ConnectionType::Queued, |_| {});
    assert_eq!(id, ConnectionId::INVALID);
    assert!(!id.is_valid());
}

#[test]
fn connect_with_type_blocking_queued_without_receiver_is_rejected() {
    let signal: Signal<i32> = Signal::new();
    let id = signal.connect_with_type(ConnectionType::BlockingQueued, |_| {});
    assert_eq!(id, ConnectionId::INVALID);
    assert!(!id.is_valid());
}
