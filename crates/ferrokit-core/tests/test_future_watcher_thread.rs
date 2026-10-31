// SPDX-License-Identifier: LGPL-3.0-only
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use ferrokit_core::event_loop::EventLoop;
use ferrokit_core::object::ThreadId;
use ferrokit_core::thread::Promise;

/// Qt: QFutureWatcher delivers `finished()` from its own event loop (qfuturewatcher.cpp:485-495).
#[test]
fn future_watcher_finished_runs_on_watcher_thread() {
    let el = EventLoop::new();
    let watcher_thread = ThreadId::current();

    let promise = Promise::<String>::new();
    let watcher = promise.future().watcher();

    let log: Arc<Mutex<Vec<ThreadId>>> = Arc::new(Mutex::new(Vec::new()));
    let slot_log = Arc::clone(&log);
    watcher.finished().connect(move |_: &()| {
        slot_log.lock().unwrap().push(ThreadId::current());
    });

    std::thread::spawn(move || promise.set_value("done".to_string()))
        .join()
        .unwrap();

    assert!(
        log.lock().unwrap().is_empty(),
        "finished ran before the watcher thread pumped its loop"
    );
    el.process_events(false);

    assert_eq!(
        *log.lock().unwrap(),
        vec![watcher_thread],
        "finished handler must run on the watcher's thread"
    );
}

/// Qt: resultReadyAt is queued to the watcher's loop (qfuturewatcher.cpp:521-537).
#[test]
fn future_watcher_result_ready_carries_value_on_watcher_thread() {
    let el = EventLoop::new();
    let watcher_thread = ThreadId::current();

    let promise = Promise::<String>::new();
    let watcher = promise.future().watcher();

    let log: Arc<Mutex<Vec<(ThreadId, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let slot_log = Arc::clone(&log);
    watcher.result_ready().connect(move |v: &String| {
        slot_log
            .lock()
            .unwrap()
            .push((ThreadId::current(), v.clone()));
    });

    std::thread::spawn(move || promise.add_result("partial".to_string()))
        .join()
        .unwrap();

    assert!(
        log.lock().unwrap().is_empty(),
        "result_ready ran before the watcher thread pumped its loop"
    );
    el.process_events(false);

    assert_eq!(
        *log.lock().unwrap(),
        vec![(watcher_thread, "partial".to_string())]
    );
}

type RangeLog = Arc<Mutex<Vec<(ThreadId, (i32, i32))>>>;

/// Qt: progressValueChanged and progressRangeChanged are queued to the watcher's loop
/// (qfuturewatcher.cpp:540-550).
#[test]
fn future_watcher_progress_runs_on_watcher_thread() {
    let el = EventLoop::new();
    let watcher_thread = ThreadId::current();

    let promise = Promise::<String>::new();
    let watcher = promise.future().watcher();

    let value_log: Arc<Mutex<Vec<(ThreadId, i32)>>> = Arc::new(Mutex::new(Vec::new()));
    let slot_values = Arc::clone(&value_log);
    watcher.progress_changed().connect(move |v: &i32| {
        slot_values.lock().unwrap().push((ThreadId::current(), *v));
    });

    let range_log: RangeLog = Arc::new(Mutex::new(Vec::new()));
    let slot_range = Arc::clone(&range_log);
    watcher
        .progress_range_changed()
        .connect(move |r: &(i32, i32)| {
            slot_range.lock().unwrap().push((ThreadId::current(), *r));
        });

    std::thread::spawn(move || {
        promise.set_progress_range(0, 100);
        promise.set_progress_value(45);
    })
    .join()
    .unwrap();

    assert!(
        value_log.lock().unwrap().is_empty() && range_log.lock().unwrap().is_empty(),
        "progress ran before the watcher thread pumped its loop"
    );
    el.process_events(false);

    assert_eq!(*range_log.lock().unwrap(), vec![(watcher_thread, (0, 100))]);
    assert_eq!(*value_log.lock().unwrap(), vec![(watcher_thread, 45)]);
}

/// Qt: `finished()` is emitted once, from `reportFinished`, which is guarded by `!isFinished()`
/// (qfutureinterface.cpp:458-466). Results precede it in the same queue (qfutureinterface.cpp:536-560).
#[test]
fn future_watcher_finished_emitted_once_after_results() {
    let el = EventLoop::new();

    let promise = Promise::<String>::new();
    let watcher = promise.future().watcher();

    let events: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
    let ev_result = Arc::clone(&events);
    watcher.result_ready().connect(move |_: &String| {
        ev_result.lock().unwrap().push("result");
    });
    let ev_finished = Arc::clone(&events);
    watcher.finished().connect(move |_: &()| {
        ev_finished.lock().unwrap().push("finished");
    });

    promise.add_result("a".to_string());
    promise.add_result("b".to_string());
    promise.finish();
    promise.finish();
    el.process_events(false);

    assert_eq!(
        *events.lock().unwrap(),
        vec!["result", "result", "finished"]
    );
}

/// Qt: cancel sends `canceled()` to the watcher's loop (qfuturewatcher.cpp:496-499).
#[test]
fn future_watcher_canceled_runs_on_watcher_thread() {
    let el = EventLoop::new();
    let watcher_thread = ThreadId::current();

    let promise = Promise::<String>::new();
    let watcher = promise.future().watcher();

    let log: Arc<Mutex<Vec<ThreadId>>> = Arc::new(Mutex::new(Vec::new()));
    let slot_log = Arc::clone(&log);
    watcher.canceled().connect(move |_: &()| {
        slot_log.lock().unwrap().push(ThreadId::current());
    });

    std::thread::spawn(move || promise.cancel()).join().unwrap();

    assert!(log.lock().unwrap().is_empty());
    el.process_events(false);
    assert_eq!(*log.lock().unwrap(), vec![watcher_thread]);
}

/// Qt: a destroyed QFutureWatcher receives no further callouts (removePostedEvents,
/// qfuturewatcher.cpp:480-483).
#[test]
fn dropped_future_watcher_receives_nothing() {
    let el = EventLoop::new();

    let promise = Promise::<String>::new();
    let watcher = promise.future().watcher();

    let calls = Arc::new(AtomicUsize::new(0));
    let c_finished = Arc::clone(&calls);
    watcher.finished().connect(move |_: &()| {
        c_finished.fetch_add(1, Ordering::SeqCst);
    });
    let c_progress = Arc::clone(&calls);
    watcher.progress_changed().connect(move |_: &i32| {
        c_progress.fetch_add(1, Ordering::SeqCst);
    });
    drop(watcher);

    promise.set_progress_value(10);
    promise.set_value("late".to_string());
    el.process_events(false);

    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "dropped watcher must not receive callouts"
    );
}

/// Qt: a watcher's callouts go to the thread that owns it (QObject thread affinity,
/// qobject.cpp:991-993), not to the thread that produced the result.
#[test]
fn future_watcher_delivers_to_thread_that_created_it() {
    let main_thread = ThreadId::current();
    let promise = Promise::<String>::new();
    let future = promise.future();

    let (ready_tx, ready_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let el = EventLoop::new();
        let watcher = future.watcher();

        let log: Arc<Mutex<Vec<ThreadId>>> = Arc::new(Mutex::new(Vec::new()));
        let slot_log = Arc::clone(&log);
        watcher.finished().connect(move |_: &()| {
            slot_log.lock().unwrap().push(ThreadId::current());
        });
        ready_tx.send(()).unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        while log.lock().unwrap().is_empty() && Instant::now() < deadline {
            el.process_events(false);
            std::thread::sleep(Duration::from_millis(5));
        }
        let got = log.lock().unwrap().clone();
        (ThreadId::current(), got)
    });

    ready_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("worker registered its watcher");
    promise.set_value("done".to_string());

    let (worker_thread, got) = worker.join().unwrap();
    assert_ne!(worker_thread, main_thread);
    assert_eq!(got, vec![worker_thread]);
}
