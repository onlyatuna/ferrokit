// SPDX-License-Identifier: LGPL-3.0-only
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use ferrokit_core::signal::Signal;
use ferrokit_core::thread::*;

// =============================================================================
// 1. Synchronization Primitives Tests
// =============================================================================

#[test]
fn test_sync_mutex_timed_locking() {
    let mutex = Arc::new(Mutex::new(10));
    assert_eq!(*mutex.lock().unwrap(), 10);

    // Lock on main thread
    let guard = mutex.lock().unwrap();

    let m_clone = Arc::clone(&mutex);
    let worker_acquired = Arc::new(AtomicBool::new(false));
    let worker_flag = Arc::clone(&worker_acquired);

    let handle = std::thread::spawn(move || {
        // try_lock should fail immediately
        assert!(m_clone.try_lock().is_none());
        // try_lock_for 20ms should time out
        let res = m_clone.try_lock_for(Duration::from_millis(20));
        assert!(res.is_none());
        worker_flag.store(true, Ordering::SeqCst);
    });

    handle.join().unwrap();
    assert!(worker_acquired.load(Ordering::SeqCst));

    drop(guard);
    // After dropping guard, try_lock succeeds
    assert!(mutex.try_lock().is_some());
}

#[test]
fn test_sync_recursive_mutex() {
    let rec_mutex = RecursiveMutex::new(100);

    // Re-entrant locking on the same thread
    let g1 = rec_mutex.lock();
    assert_eq!(*g1, 100);

    let g2 = rec_mutex.lock();
    assert_eq!(*g2, 100);

    let g3 = rec_mutex
        .try_lock()
        .expect("re-entrant try_lock should succeed");
    assert_eq!(*g3, 100);

    drop(g3);
    drop(g2);
    drop(g1);

    // Now another thread can acquire it
    let rec_arc = Arc::new(rec_mutex);
    let rec_clone = Arc::clone(&rec_arc);
    let handle = std::thread::spawn(move || {
        let mut g = rec_clone.lock();
        *g = 200;
    });
    handle.join().unwrap();

    assert_eq!(*rec_arc.lock(), 200);
}

#[test]
fn test_sync_semaphore_and_wait_condition() {
    // Semaphore testing
    let sem = Semaphore::new(2);
    assert_eq!(sem.available(), 2);

    sem.acquire(2);
    assert_eq!(sem.available(), 0);
    assert!(!sem.try_acquire(1));
    assert!(!sem.try_acquire_for(1, Duration::from_millis(15)));

    sem.release(1);
    assert_eq!(sem.available(), 1);
    assert!(sem.try_acquire(1));

    // WaitCondition testing
    let mutex = Arc::new(Mutex::new(false));
    let cond = Arc::new(WaitCondition::new());

    let m_clone = Arc::clone(&mutex);
    let c_clone = Arc::clone(&cond);

    let handle = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(30));
        let mut g = m_clone.lock().unwrap();
        *g = true;
        c_clone.wake_one();
    });

    let mut guard = mutex.lock().unwrap();
    while !*guard {
        guard = cond.wait(guard);
    }
    assert!(*guard);

    handle.join().unwrap();
}

// =============================================================================
// 2. ThreadPool & TaskHandle Tests
// =============================================================================

#[test]
fn test_thread_pool_execution_and_cancellation() {
    let pool = ThreadPool::builder()
        .max_threads(3)
        .name_prefix("test-worker")
        .build();

    // 1. Basic task execution
    let handle1 = pool.spawn(|| 20 + 22);
    let res1 = handle1.join();
    assert_eq!(res1, Ok(42));

    // 2. Cancellable task
    let handle2 = pool.spawn_cancellable(|token| {
        let mut count = 0;
        for _ in 0..100 {
            if token.is_canceled() {
                return -1;
            }
            std::thread::sleep(Duration::from_millis(5));
            count += 1;
        }
        count
    });

    std::thread::sleep(Duration::from_millis(10));
    handle2.cancel();

    let res2 = handle2.join();
    // Result is either the cooperative return -1 or canceled error
    assert!(res2 == Ok(-1) || res2 == Err(TaskError::Canceled));

    // 3. Global thread pool
    let global_handle = ThreadPool::global().spawn(|| "hello from global pool");
    assert_eq!(global_handle.join(), Ok("hello from global pool"));
}

// =============================================================================
// 3. Future, Promise, Watcher & Synchronizer Tests
// =============================================================================

#[test]
fn test_future_promise_and_watcher() {
    let el = ferrokit_core::event_loop::EventLoop::new();
    let promise: Promise<String> = Promise::new();
    let future = promise.future();
    let watcher = future.watcher();

    // Qt: resultReadyAt is queued to the watcher's event loop, so the slot runs only
    // after process_events (qfuturewatcher.cpp:521-537).
    let received_result = Arc::new(StdMutex::new(String::new()));
    let rec_res = Arc::clone(&received_result);
    watcher.result_ready().connect(move |val| {
        *rec_res.lock().unwrap() = val.clone();
    });

    let progress_val = Arc::new(AtomicI32::new(0));
    let p_val = Arc::clone(&progress_val);
    watcher.progress_changed().connect(move |v| {
        p_val.store(*v, Ordering::SeqCst);
    });

    // Update progress
    promise.set_progress_range(0, 100);
    promise.set_progress_value(45);
    assert_eq!(future.progress_value(), 45);
    el.process_events(false);
    assert_eq!(progress_val.load(Ordering::SeqCst), 45);

    // Fulfill promise
    promise.set_value("computation complete".to_string());
    el.process_events(false);

    assert!(future.is_finished());
    assert_eq!(future.result(), Some("computation complete".to_string()));
    assert_eq!(*received_result.lock().unwrap(), "computation complete");

    // FutureSynchronizer
    let mut synchronizer = FutureSynchronizer::new();
    synchronizer.add_future(future);
    synchronizer.wait_for_finished();
    assert_eq!(synchronizer.futures().len(), 1);
}

// =============================================================================
// 4. Thread Spawning & Event Loop Integration Tests
// =============================================================================

#[test]
fn test_thread_spawn_and_interruption() {
    let handle = Thread::spawn(|| {
        assert!(!Thread::is_main_thread());
        let mut count = 0;
        while !Thread::is_interruption_requested() && count < 100 {
            Thread::msleep(5);
            count += 1;
        }
        count
    });

    std::thread::sleep(Duration::from_millis(15));
    handle.request_interruption();

    let count = handle.join().unwrap();
    assert!(count < 100);
    assert!(Thread::ideal_thread_count() > 0);
}

#[test]
fn test_thread_with_event_loop() {
    let worker_processed = Arc::new(AtomicBool::new(false));
    let proc_flag = Arc::clone(&worker_processed);

    let handle = Thread::spawn_with_event_loop(move |sender| {
        let sender_clone = sender.clone();
        let flag = Arc::clone(&proc_flag);
        // Post an event inside the event loop
        let meta_event = ferrokit_core::event::Event::new(ferrokit_core::event::EventKind::MetaCall(
            Box::new(move |_| {
                flag.store(true, Ordering::SeqCst);
                // After processing, quit the event loop
                let quit_event = ferrokit_core::event::Event::new(ferrokit_core::event::EventKind::Quit {
                    exit_code: 42,
                });
                sender_clone.post_event(ferrokit_core::object::ObjectId(0), quit_event);
            }),
        ));
        sender.post_event(ferrokit_core::object::ObjectId(0), meta_event);
    });

    let exit_code = handle.join().unwrap();
    assert_eq!(exit_code, 42);
    assert!(worker_processed.load(Ordering::SeqCst));
}

// =============================================================================
// 5. Channel and Signal Integration Tests
// =============================================================================

#[test]
fn test_channel_basic_and_signal_integration() {
    let (sender, receiver) = channel::<i32>();

    // Connect to a Qt Signal
    let signal: Signal<i32> = Signal::new();
    let signal_received = Arc::new(AtomicI32::new(0));
    let sig_rec = Arc::clone(&signal_received);

    signal.connect(move |val| {
        sig_rec.store(*val, Ordering::SeqCst);
    });

    receiver.connect_to_signal(&signal);

    sender.send(999).unwrap();

    assert_eq!(signal_received.load(Ordering::SeqCst), 999);
    assert_eq!(receiver.recv().unwrap(), 999);

    // Bounded channel
    let (b_tx, b_rx) = bounded::<i32>(2);
    b_tx.send(1).unwrap();
    b_tx.send(2).unwrap();
    assert_eq!(b_rx.try_recv().unwrap(), 1);
    assert_eq!(b_rx.try_recv().unwrap(), 2);
    assert!(b_rx.try_recv().is_err());
}

// =============================================================================
// 6. Async Boundary (block_on & spawn_async) Tests
// =============================================================================

#[test]
fn test_async_boundary_block_on_and_spawn() {
    // 1. block_on
    let result = block_on(async {
        let a = 10;
        let b = 32;
        a + b
    });
    assert_eq!(result, 42);

    // 2. spawn_async
    let task = spawn_async(async {
        std::thread::sleep(Duration::from_millis(10));
        "async task complete"
    });

    let output = task.join().unwrap();
    assert_eq!(output, "async task complete");

    // 3. Executor
    let executor = Executor::new();
    let exec_task = executor.spawn(async { 100 * 2 });
    assert_eq!(exec_task.join().unwrap(), 200);
}
