// SPDX-License-Identifier: LGPL-3.0-only
//! `Timer::single_shot_with_context` (Qt `QTimer::singleShot(msec, context, functor)`,
//! qsingleshottimer.cpp:46-59): runs once on the context's thread, never early, and is dropped
//! unrun when the context is destroyed first.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::event_loop::{post_event_to_thread, EventLoop};
use ferrokit_core::object::{
    move_to_thread_by_id, register_owned_qobject, ObjectData, ObjectDataMut, ObjectId,
    OwnedObject, QObject, ThreadId,
};
use ferrokit_core::timer::Timer;

const WAIT: Duration = Duration::from_secs(10);

struct Ctx {
    data: ObjectData,
}

impl QObject for Ctx {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ObjectDataMut<'_> {
        ObjectDataMut::new(&mut self.data)
    }
}

fn new_ctx() -> OwnedObject<Ctx> {
    // SAFETY: the owner is dropped by one thread at a time and no object callback runs here.
    unsafe {
        register_owned_qobject(Ctx {
            data: ObjectData::new(ObjectId::next()),
        })
    }
}

/// Sets a flag when dropped: proves the functor's captures were released.
struct Guard(Arc<AtomicBool>);

impl Drop for Guard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// Pumps `el` until `cond` holds or the watchdog expires.
fn pump_until(el: &EventLoop, cond: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + WAIT;
    while !cond() {
        if Instant::now() > deadline {
            return false;
        }
        el.process_events(false);
        std::thread::sleep(Duration::from_millis(1));
    }
    true
}

#[test]
fn fires_once_on_the_context_thread_and_not_early() {
    let el = EventLoop::new();
    let ctx = new_ctx();
    let id = OwnedObject::object_id(&ctx);
    let fired = Arc::new(Mutex::new(Vec::new()));
    let f = Arc::clone(&fired);
    let armed = Instant::now();
    Timer::single_shot_with_context(40, id, move || {
        f.lock().unwrap().push((std::thread::current().id(), Instant::now()));
    });
    assert!(pump_until(&el, || !fired.lock().unwrap().is_empty()), "watchdog: never fired");
    let settle = Instant::now() + Duration::from_millis(100);
    while Instant::now() < settle {
        el.process_events(false);
    }
    let fired = fired.lock().unwrap();
    assert_eq!(fired.len(), 1, "must fire exactly once");
    assert_eq!(fired[0].0, std::thread::current().id());
    assert!(fired[0].1 - armed >= Duration::from_millis(40), "fired early");
}

#[test]
fn zero_interval_runs_queued_on_the_context_thread() {
    let el = EventLoop::new();
    let ctx = new_ctx();
    let ran = Arc::new(AtomicUsize::new(0));
    let r = Arc::clone(&ran);
    Timer::single_shot_with_context(0, OwnedObject::object_id(&ctx), move || {
        r.fetch_add(1, Ordering::SeqCst);
    });
    assert_eq!(ran.load(Ordering::SeqCst), 0, "must be queued, not run inline");
    assert!(pump_until(&el, || ran.load(Ordering::SeqCst) == 1));
}

#[test]
fn context_destroy_cancels_and_drops_the_functor() {
    let el = EventLoop::new();
    let ctx = new_ctx();
    let dropped = Arc::new(AtomicBool::new(false));
    let ran = Arc::new(AtomicBool::new(false));
    let (g, r) = (Guard(Arc::clone(&dropped)), Arc::clone(&ran));
    Timer::single_shot_with_context(50, OwnedObject::object_id(&ctx), move || {
        let _g = &g;
        r.store(true, Ordering::SeqCst);
    });
    assert!(!dropped.load(Ordering::SeqCst));
    drop(ctx);
    assert!(dropped.load(Ordering::SeqCst), "captures must be dropped with the context");
    // Run the loop past the original deadline.
    let end = Instant::now() + Duration::from_millis(150);
    while Instant::now() < end {
        el.process_events(false);
    }
    assert!(!ran.load(Ordering::SeqCst), "cancelled functor must never run");
}

#[test]
fn armed_from_another_thread_runs_on_the_context_thread() {
    let here = ThreadId::current();
    let _el = EventLoop::new();
    let ctx = new_ctx();
    let id = OwnedObject::object_id(&ctx);
    let (tid_tx, tid_rx) = channel();
    let (go_tx, go_rx) = channel::<()>();
    let fired = Arc::new(Mutex::new(Vec::new()));
    let f = Arc::clone(&fired);
    let worker = std::thread::spawn(move || {
        let el = EventLoop::new();
        tid_tx.send(ThreadId::current()).unwrap();
        go_rx.recv_timeout(WAIT).unwrap();
        let ok = pump_until(&el, || !f.lock().unwrap().is_empty());
        let settle = Instant::now() + Duration::from_millis(100);
        while Instant::now() < settle {
            el.process_events(false);
        }
        (ok, std::thread::current().id())
    });
    let there = tid_rx.recv_timeout(WAIT).unwrap();
    assert_eq!(move_to_thread_by_id(id, there, here), Ok(()));
    let f = Arc::clone(&fired);
    let armed = Instant::now();
    Timer::single_shot_with_context(40, id, move || {
        f.lock().unwrap().push((std::thread::current().id(), Instant::now()));
    });
    go_tx.send(()).unwrap();
    let (ok, worker_thread) = worker.join().unwrap();
    assert!(ok, "watchdog: never fired");
    let fired = fired.lock().unwrap();
    assert_eq!(fired.len(), 1);
    assert_eq!(fired[0].0, worker_thread, "must run on the context's thread");
    assert!(fired[0].1 - armed >= Duration::from_millis(40));
    drop(fired);
    drop(ctx);
}

#[test]
fn forwarded_cross_thread_drop_of_the_context_cancels() {
    let here = ThreadId::current();
    let _el = EventLoop::new();
    let ctx = new_ctx();
    let id = OwnedObject::object_id(&ctx);
    let (tid_tx, tid_rx) = channel();
    let (armed_tx, armed_rx) = channel::<()>();
    let (dropped_tx, dropped_rx) = channel::<()>();
    let dropped = Arc::new(AtomicBool::new(false));
    let ran = Arc::new(AtomicBool::new(false));
    let marker = Arc::new(AtomicBool::new(false));
    let (d2, m2) = (Arc::clone(&dropped), Arc::clone(&marker));
    let worker = std::thread::spawn(move || {
        let el = EventLoop::new();
        tid_tx.send(ThreadId::current()).unwrap();
        // The arm and the marker are queued here; running the marker means the arm ran.
        assert!(pump_until(&el, || m2.load(Ordering::SeqCst)), "watchdog: arm not delivered");
        armed_tx.send(()).unwrap();
        dropped_rx.recv_timeout(WAIT).unwrap();
        pump_until(&el, || d2.load(Ordering::SeqCst))
    });
    let there = tid_rx.recv_timeout(WAIT).unwrap();
    assert_eq!(move_to_thread_by_id(id, there, here), Ok(()));
    let (g, r) = (Guard(Arc::clone(&dropped)), Arc::clone(&ran));
    Timer::single_shot_with_context(60_000, id, move || {
        let _g = &g;
        r.store(true, Ordering::SeqCst);
    });
    let m = Arc::clone(&marker);
    post_event_to_thread(
        there,
        ObjectId(0),
        Event::new(EventKind::MetaCall(Box::new(move |_| m.store(true, Ordering::SeqCst)))),
    );
    armed_rx.recv_timeout(WAIT).unwrap();
    assert!(!dropped.load(Ordering::SeqCst), "functor lives while the context does");
    drop(ctx); // forwarded to the context's thread
    dropped_tx.send(()).unwrap();
    assert!(worker.join().unwrap(), "watchdog: functor leaked after forwarded drop");
    assert!(!ran.load(Ordering::SeqCst));
}
