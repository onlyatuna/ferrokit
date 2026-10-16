// SPDX-License-Identifier: LGPL-3.0-only
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ferrokit_core::event::{compress_event, CoreCompressor, Event, EventFilterChain, EventKind};
use ferrokit_core::event_loop::{EventLoop, PostedEvent};
use ferrokit_core::object::{
    register_qobject_ptr, unregister_qobject, ObjectData, ObjectId, QObject, ThreadContext,
};
use ferrokit_core::signal::Signal;
use ferrokit_core::timer::Timer;

// =========================================================================
// Level 1: Pure logic unit tests (no event loop required)
// =========================================================================

/// 1. Signal / Slot unit tests
#[test]
fn test_level1_signal_basic_and_scoped_and_highest_id() {
    // 1.1 Basic connection and emit: signal.connect(...) followed by signal.emit(&42)
    let signal = Signal::<i32>::new();
    let hit_val = Arc::new(AtomicUsize::new(0));
    let hit_val_clone = Arc::clone(&hit_val);

    signal.connect(move |val| {
        hit_val_clone.store(*val as usize, Ordering::SeqCst);
    });

    signal.emit(&42);
    assert_eq!(hit_val.load(Ordering::SeqCst), 42);

    // 1.2 RAII automatic disconnection: ScopedConnection disconnects on drop
    let scoped_hit = Arc::new(AtomicUsize::new(0));
    let scoped_hit_clone = Arc::clone(&scoped_hit);

    {
        let _scoped = signal.connect_scoped(move |val| {
            scoped_hit_clone.fetch_add(*val as usize, Ordering::SeqCst);
        });
        signal.emit(&10);
        assert_eq!(scoped_hit.load(Ordering::SeqCst), 10);
    } // _scoped dropped here

    signal.emit(&10);
    assert_eq!(
        scoped_hit.load(Ordering::SeqCst),
        10,
        "ScopedConnection should not trigger after being dropped"
    );

    // 1.3 Emit phase safety: connect() called inside a slot callback should not trigger in current emit cycle (Qt highest_id mechanism)
    let dynamic_signal = Arc::new(Signal::<usize>::new());
    let dynamic_signal_clone = Arc::clone(&dynamic_signal);
    let dynamic_calls = Arc::new(AtomicUsize::new(0));
    let dynamic_calls_clone = Arc::clone(&dynamic_calls);

    dynamic_signal.connect(move |_| {
        let calls = Arc::clone(&dynamic_calls_clone);
        dynamic_signal_clone.connect(move |_| {
            calls.fetch_add(1, Ordering::SeqCst);
        });
    });

    // First emit: triggers slot 1. Slot 2 is connected inside slot 1, but id >= highest_id, so it does not run in this cycle
    dynamic_signal.emit(&1);
    assert_eq!(
        dynamic_calls.load(Ordering::SeqCst),
        0,
        "Dynamically connected slot should not execute in the same emit cycle"
    );

    // Second emit: newly connected slot now triggers
    dynamic_signal.emit(&2);
    assert_eq!(dynamic_calls.load(Ordering::SeqCst), 1);
}

/// 2. Event filter tests
#[test]
fn test_level1_event_filter_lifo_intercept_and_tombstone() {
    let mut chain = EventFilterChain::new();

    // 2.1 LIFO ordering: install filter A, then B; snapshot order is B -> A
    let id_a = ObjectId(100);
    let id_b = ObjectId(200);
    chain.install(id_a);
    chain.install(id_b);
    assert_eq!(chain.snapshot(), vec![id_b, id_a]);

    // 2.2 Intercept semantics: when filter returns FilterResult::Filtered, subsequent filters and target event() must not be called
    struct InterceptFilter {
        data: ObjectData,
        intercept: bool,
    }
    impl QObject for InterceptFilter {
        fn object_data(&self) -> &ObjectData {
            &self.data
        }
        fn object_data_mut(&mut self) -> &mut ObjectData {
            &mut self.data
        }
        fn event_filter(&mut self, _watched: ObjectId, _event: &mut Event) -> bool {
            self.intercept
        }
    }

    struct TargetWidget {
        data: ObjectData,
        event_called: bool,
    }
    impl QObject for TargetWidget {
        fn object_data(&self) -> &ObjectData {
            &self.data
        }
        fn object_data_mut(&mut self) -> &mut ObjectData {
            &mut self.data
        }
        fn event(&mut self, _event: &mut Event) -> bool {
            self.event_called = true;
            true
        }
    }

    let mut filter_b_obj = InterceptFilter {
        data: ObjectData::new(id_b),
        intercept: true, // Intercept
    };
    // SAFETY: both objects remain alive and unmoved on this thread until unregistered below.
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut filter_b_obj)) };

    let mut target = TargetWidget {
        data: ObjectData::new(ObjectId(300)),
        event_called: false,
    };
    // SAFETY: target remains alive and unmoved on this thread until unregistered below.
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut target)) };
    target.object_data_mut().install_event_filter(id_b);

    let mut ev = Event::new(EventKind::UpdateRequest);
    let handled = ferrokit_core::object::send_event(target.data.id, &mut ev);

    assert!(
        handled,
        "Event intercepted and consumed by filter returns true in Qt (qcoreapplication.cpp:1293-1294)"
    );
    assert!(!target.event_called, "Target event() must not be called");

    // 2.3 Tombstone test: remove_event_filter() inside filter callback should not crash iteration
    target.object_data_mut().remove_event_filter(id_b);
    assert_eq!(target.object_data().event_filters.snapshot(), vec![]);

    // SAFETY: the callbacks have returned and these registrations are owner-thread.
    unsafe {
        unregister_qobject(id_b);
        unregister_qobject(target.data.id);
    }
}

/// 3. Event compressor tests
#[test]
fn test_level1_compressor_dedup_and_override() {
    let compressor = CoreCompressor;
    let target = ObjectId(50);
    let mut queue = Vec::new();

    // 3.1 Deduplication: posting duplicate UpdateRequest should compress queue
    queue.push(PostedEvent::new(
        target,
        Event::new(EventKind::UpdateRequest),
        0,
    ));
    let incoming_update = Event::new(EventKind::UpdateRequest);
    let compressed = compress_event(&mut queue, target, &incoming_update, &compressor);
    assert!(compressed);
    assert_eq!(queue.len(), 1);

    // 3.2 Override: posting Quit(0) then Quit(42) should overwrite exit code in-place
    queue.push(PostedEvent::new(
        target,
        Event::new(EventKind::Quit { exit_code: 0 }),
        0,
    ));
    let incoming_quit = Event::new(EventKind::Quit { exit_code: 42 });
    let quit_compressed = compress_event(&mut queue, target, &incoming_quit, &compressor);
    assert!(quit_compressed);
    assert_eq!(queue.len(), 2); // 1 UpdateRequest + 1 Quit
    if let EventKind::Quit { exit_code } = queue[1].event.kind {
        assert_eq!(exit_code, 42, "Latest exit code should overwrite in-place");
    } else {
        panic!("Expected Quit event");
    }
}

// =========================================================================
// Level 2: Event queue and livelock prevention
// =========================================================================

/// Livelock protection test (insertion_offset)
#[test]
fn test_level2_loop_livelock_protection() {
    struct LivelockObject {
        data: ObjectData,
        counter: Arc<AtomicUsize>,
        loop_handle: ferrokit_core::event_loop::EventLoopHandle,
    }

    impl QObject for LivelockObject {
        fn object_data(&self) -> &ObjectData {
            &self.data
        }
        fn object_data_mut(&mut self) -> &mut ObjectData {
            &mut self.data
        }
        fn event(&mut self, _event: &mut Event) -> bool {
            self.counter.fetch_add(1, Ordering::SeqCst);
            // When receiving an event, post another event inside callback
            self.loop_handle
                .post_event(self.data.id, Event::new(EventKind::User(Box::new(()))));
            true
        }
    }

    let event_loop = EventLoop::new();
    let counter = Arc::new(AtomicUsize::new(0));

    let mut receiver = LivelockObject {
        data: ObjectData::new(ObjectId::next()),
        counter: Arc::clone(&counter),
        loop_handle: event_loop.handle(),
    };
    // SAFETY: receiver remains alive and unmoved on this thread until unregistered below.
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut receiver)) };
    let r_id = receiver.data.id;

    event_loop.post_event(r_id, Event::new(EventKind::User(Box::new(()))));

    // Only drain events present at the start of iteration
    event_loop.send_posted_events();

    // Newly posted events must be deferred to the next iteration
    assert_eq!(
        counter.load(Ordering::SeqCst),
        1,
        "Livelock cutoff must ensure only current batch is drained"
    );

    // SAFETY: all event callbacks have returned on the registration thread.
    unsafe { unregister_qobject(r_id) };
}

// =========================================================================
// Level 3: Integration tests (event loop, timer, and cross-thread)
// =========================================================================

/// 1. Timer and event loop exit test (with watchdog timeout)
#[test]
fn test_level3_timer_and_event_loop_exit() {
    let event_loop = EventLoop::new();
    let triggered = Arc::new(AtomicBool::new(false));
    let triggered_clone = Arc::clone(&triggered);

    // Watchdog timer to avoid hanging CI if test stalls
    let watchdog_handle = event_loop.handle();
    let watchdog = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(2000));
        watchdog_handle.post_quit(ObjectId(0), -1);
    });

    // Create 50ms single-shot timer
    let mut timer = Timer::new();
    timer.set_single_shot(true);
    timer.set_interval(50);

    let loop_handle = event_loop.handle();
    timer.timeout.connect(move |()| {
        triggered_clone.store(true, Ordering::SeqCst);
        loop_handle.post_quit(ObjectId(0), 0); // Quit event loop
    });

    timer.as_mut().start();

    // Start event loop
    let exit_code = event_loop.exec();

    assert_eq!(exit_code, 0);
    assert!(
        triggered.load(Ordering::SeqCst),
        "Timer should trigger and exit event loop"
    );

    drop(watchdog);
}

/// 2. Cross-thread queued connection test (Worker -> UI Thread)
#[test]
fn test_level3_cross_thread_queued_connection() {
    let event_loop = EventLoop::new();
    let loop_handle = event_loop.handle();
    let received_data = Arc::new(Mutex::new(None));
    let received_data_clone = Arc::clone(&received_data);

    // Watchdog timer
    let watchdog_handle = event_loop.handle();
    let watchdog = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(2000));
        watchdog_handle.post_quit(ObjectId(0), -1);
    });

    // Create receiver on main thread
    let receiver_thread_id = ThreadContext::current_id();
    let update_signal = Arc::new(Signal::<String>::new());
    let signal_clone = Arc::clone(&update_signal);

    update_signal.connect_queued(receiver_thread_id, move |data: &String| {
        *received_data_clone.lock().unwrap() = Some(data.clone());
        loop_handle.post_quit(ObjectId(0), 0);
    });

    // Spawn worker thread to emit signal
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(30));
        // Cross-thread emission: converted to MetaCall and wakes up main event loop
        signal_clone.emit(&"Hello from worker".to_string());
    });

    // Main thread processes events until quit
    let exit_code = event_loop.exec();

    assert_eq!(exit_code, 0);
    assert_eq!(
        received_data.lock().unwrap().as_deref(),
        Some("Hello from worker")
    );

    drop(watchdog);
}
