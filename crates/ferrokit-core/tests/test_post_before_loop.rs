// SPDX-License-Identifier: LGPL-3.0-only
//! Events posted to a thread whose event loop does not exist yet.
//!
//! Qt queues them on the receiver thread's `postEventList` regardless of whether a dispatcher
//! exists (`QCoreApplication::postEvent`, qcoreapplication.cpp:1694) and delivers them once the
//! thread runs events. Each test uses a fresh thread (std `ThreadId`s are never reused), so the
//! "target thread has no loop yet" state is exact.

use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::event_loop::{EventLoop, post_event_to_thread};
use ferrokit_core::object::{ObjectData, ObjectId, QObject, ThreadId};
use ferrokit_core::object::{register_qobject_ptr, unregister_qobject};
use ferrokit_core::signal::{ConnectionType, Signal};

type Log = Arc<Mutex<Vec<&'static str>>>;

fn log() -> Log {
    Arc::new(Mutex::new(Vec::new()))
}

fn push(log: &Log, name: &'static str) -> Event {
    let log = Arc::clone(log);
    Event::new(EventKind::MetaCall(Box::new(move |_| {
        log.lock().unwrap().push(name)
    })))
}

/// A worker thread that reports its id, waits for `go`, then creates its loop and pumps once.
/// Returns the worker's `ThreadId`, the `go` sender and the join handle.
fn worker_with_late_loop<F>(after_loop: F) -> (ThreadId, Sender<()>, std::thread::JoinHandle<()>)
where
    F: FnOnce() + Send + 'static,
{
    let (id_tx, id_rx) = channel();
    let (go_tx, go_rx): (Sender<()>, Receiver<()>) = channel();
    let handle = std::thread::spawn(move || {
        id_tx.send(ThreadId::current()).unwrap();
        go_rx.recv().unwrap();
        let el = EventLoop::new();
        el.process_events(false);
        after_loop();
    });
    (id_rx.recv().unwrap(), go_tx, handle)
}

#[test]
fn post_before_loop_exists_is_delivered_when_loop_starts() {
    let log = log();
    let seen = Arc::clone(&log);
    let (tid, go, join) = worker_with_late_loop(move || {
        assert_eq!(*seen.lock().unwrap(), vec!["posted-before-loop"]);
    });

    // The worker has no loop yet.
    post_event_to_thread(tid, ObjectId(0), push(&log, "posted-before-loop"));
    go.send(()).unwrap();
    join.join()
        .expect("event posted before the loop existed must be delivered");
}

#[test]
fn events_posted_before_loop_keep_priority_and_fifo_order() {
    use ferrokit_core::event_loop::post_event_to_thread_with_priority;
    let log = log();
    let seen = Arc::clone(&log);
    let (tid, go, join) = worker_with_late_loop(move || {
        assert_eq!(
            *seen.lock().unwrap(),
            vec!["high", "normal-1", "normal-2", "low"]
        );
    });

    post_event_to_thread_with_priority(tid, ObjectId(0), push(&log, "low"), -10);
    post_event_to_thread_with_priority(tid, ObjectId(0), push(&log, "normal-1"), 0);
    post_event_to_thread_with_priority(tid, ObjectId(0), push(&log, "normal-2"), 0);
    post_event_to_thread_with_priority(tid, ObjectId(0), push(&log, "high"), 10);
    go.send(()).unwrap();
    join.join()
        .expect("buffered events must be delivered by priority, FIFO within a priority");
}

#[test]
fn events_posted_after_loop_exists_follow_the_buffered_ones() {
    let log = log();
    let seen = Arc::clone(&log);
    let (id_tx, id_rx) = channel();
    let (go_tx, go_rx): (Sender<()>, Receiver<()>) = channel();
    let (ready_tx, ready_rx): (Sender<()>, Receiver<()>) = channel();
    let (pump_tx, pump_rx): (Sender<()>, Receiver<()>) = channel();
    let join = std::thread::spawn(move || {
        id_tx.send(ThreadId::current()).unwrap();
        go_rx.recv().unwrap();
        let el = EventLoop::new();
        ready_tx.send(()).unwrap();
        pump_rx.recv().unwrap();
        el.process_events(false);
        assert_eq!(*seen.lock().unwrap(), vec!["before-loop", "after-loop"]);
    });
    let tid = id_rx.recv().unwrap();

    post_event_to_thread(tid, ObjectId(0), push(&log, "before-loop"));
    go_tx.send(()).unwrap();
    ready_rx.recv().unwrap();
    post_event_to_thread(tid, ObjectId(0), push(&log, "after-loop"));
    pump_tx.send(()).unwrap();
    join.join()
        .expect("buffered events must precede events posted once the loop exists");
}

#[test]
fn queued_signal_emitted_before_target_loop_exists_is_delivered() {
    let hits = Arc::new(Mutex::new(Vec::<i32>::new()));
    let seen = Arc::clone(&hits);
    let (tid, go, join) = worker_with_late_loop(move || {
        assert_eq!(*seen.lock().unwrap(), vec![7]);
    });

    let signal: Signal<i32> = Signal::new();
    let sink = Arc::clone(&hits);
    signal.connect_object(ObjectId(0), tid, ConnectionType::Queued, move |v| {
        sink.lock().unwrap().push(*v)
    });
    signal.emit(&7);

    assert!(
        hits.lock().unwrap().is_empty(),
        "a queued slot must not run on the emitter"
    );
    go.send(()).unwrap();
    join.join()
        .expect("queued slot emitted before the target loop existed must run later");
}

#[test]
fn single_shot_zero_before_loop_runs_after_earlier_posted_events() {
    use ferrokit_core::timer::Timer;
    let log = log();
    let seen = Arc::clone(&log);
    let (id_tx, id_rx) = channel();
    let (go_tx, go_rx): (Sender<()>, Receiver<()>) = channel();
    let (posted_tx, posted_rx): (Sender<()>, Receiver<()>) = channel();
    let thread_log = Arc::clone(&log);
    let join = std::thread::spawn(move || {
        id_tx.send(ThreadId::current()).unwrap();
        // Wait until another thread has posted an event to us, then schedule the zero timer,
        // all before this thread has any event loop.
        posted_rx.recv().unwrap();
        let cb_log = Arc::clone(&thread_log);
        Timer::single_shot(0, move || cb_log.lock().unwrap().push("single-shot"));
        go_rx.recv().unwrap();
        let el = EventLoop::new();
        el.process_events(false);
        assert_eq!(*seen.lock().unwrap(), vec!["earlier-post", "single-shot"]);
    });
    let tid = id_rx.recv().unwrap();

    post_event_to_thread(tid, ObjectId(0), push(&log, "earlier-post"));
    posted_tx.send(()).unwrap();
    // Give the worker no chance to run ahead: it only creates its loop after `go`.
    go_tx.send(()).unwrap();
    join.join()
        .expect("single_shot(0) before the loop must run after earlier posted events");
}

struct Receiver1 {
    data: ObjectData,
    got: Arc<AtomicBool>,
}

impl QObject for Receiver1 {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
    }
    fn event(&mut self, event: &mut Event) -> bool {
        if matches!(event.kind, EventKind::UpdateRequest) {
            self.got.store(true, Ordering::SeqCst);
            return true;
        }
        false
    }
}

#[test]
fn core_application_post_event_before_receiver_thread_has_loop_is_delivered() {
    use ferrokit_core::application::CoreApplication;
    let got = Arc::new(AtomicBool::new(false));
    let got_w = Arc::clone(&got);
    let (id_tx, id_rx) = channel();
    let (go_tx, go_rx): (Sender<()>, Receiver<()>) = channel();
    let (reg_tx, reg_rx): (Sender<ObjectId>, Receiver<ObjectId>) = channel();
    let join = std::thread::spawn(move || {
        id_tx.send(ThreadId::current()).unwrap();
        let id = ObjectId::next();
        let mut rcv = Receiver1 {
            data: ObjectData::new(id),
            got: got_w,
        };
        // SAFETY: the pointer comes from `&raw mut rcv`, so no reference is created that a later
        // use of `rcv` could invalidate. `rcv` is a local that is not moved or dropped until after
        // `unregister_qobject` below. This thread is the registration thread and runs the
        // callbacks (`process_events`). The only use of `rcv` between callbacks is the read
        // `rcv.got.load`, which rule 7 allows, and `Receiver1` holds an `Arc<AtomicBool>` only.
        unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut rcv)) };
        reg_tx.send(id).unwrap();
        go_rx.recv().unwrap();
        let el = EventLoop::new();
        el.process_events(false);
        let delivered = rcv.got.load(Ordering::SeqCst);
        // SAFETY: `unregister_qobject` needs `rcv` alive and unmoved until it returns, on the
        // registration thread, with no callback active. `rcv` is the local above, in scope until
        // the closure ends. This is the thread that registered it, and `process_events` has
        // returned, so no callback is running.
        unsafe { unregister_qobject(id) };
        assert!(
            delivered,
            "UpdateRequest posted before the loop existed must be delivered"
        );
    });
    let _tid = id_rx.recv().unwrap();
    let rid = reg_rx.recv().unwrap();

    CoreApplication::post_event(rid, Event::new(EventKind::UpdateRequest));
    go_tx.send(()).unwrap();
    join.join()
        .expect("CoreApplication::post_event before the loop existed must deliver");
    assert!(got.load(Ordering::SeqCst));
}
