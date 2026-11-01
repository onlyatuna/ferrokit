// SPDX-License-Identifier: LGPL-3.0-only
//! `Timer::single_shot_with_context` (Qt `QTimer::singleShot(msec, context, functor)`,
//! qsingleshottimer.cpp:46-59): runs once on the context's thread, never early, follows the
//! context across `move_to_thread`, and is dropped unrun when the context is destroyed first.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::event_loop::{EventLoop, post_event_to_thread};
use ferrokit_core::object::{
    ObjectData, ObjectDataMut, ObjectId, OwnedObject, QObject, ThreadId, move_to_thread_by_id,
    register_owned_qobject, with_object_mut,
};
use ferrokit_core::timer::Timer;

/// An `ObjectData` that lets `move_to_thread_by_id` move its object.
fn movable_data(id: ObjectId) -> ObjectData {
    // SAFETY: `Ctx` holds nothing but its `ObjectData`, which is `Send`; no `Rc`, no shared `Cell`,
    // no thread-local state.
    unsafe { ObjectData::new(id).with_thread_moves_allowed() }
}

const WAIT: Duration = Duration::from_secs(10);
/// Clock measurement tolerance, not a latency allowance.
const TOLERANCE: Duration = Duration::from_millis(1);
/// Never reached by a test: a shot with this interval is pending until cancelled.
const FAR: u64 = 3_600_000;

type Seen = Arc<Mutex<Vec<(std::thread::ThreadId, Instant)>>>;

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
    // SAFETY: the returned owner is a test local, used only for its id and dropped by the test.
    // `Ctx` has no callbacks of its own and no event is sent to it. After `move_to_thread_by_id`
    // the drop happens on a non-registration thread, where `OwnedObject::drop` forwards the
    // destruction to the registration thread (see `destroy_owned`); the tests assert exactly that.
    unsafe {
        register_owned_qobject(Ctx {
            data: movable_data(ObjectId::next()),
        })
    }
}

/// Records the thread it is dropped on: proves the functor's captures were released.
#[derive(Clone, Default)]
struct DropLog(Arc<Mutex<Option<std::thread::ThreadId>>>);

struct Guard(DropLog);

impl DropLog {
    fn guard(&self) -> Guard {
        Guard(self.clone())
    }
    fn dropped_on(&self) -> Option<std::thread::ThreadId> {
        *self.0.lock().unwrap()
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        *(self.0).0.lock().unwrap() = Some(std::thread::current().id());
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

/// Posts a marker to `thread` and returns its flag; once set, everything queued before ran.
fn marker(thread: ThreadId) -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    let f = Arc::clone(&flag);
    post_event_to_thread(
        thread,
        ObjectId(0),
        Event::new(EventKind::MetaCall(Box::new(move |_| {
            f.store(true, Ordering::SeqCst)
        }))),
    );
    flag
}

fn recorder(seen: &Seen) -> impl FnOnce() + Send + 'static + use<> {
    let seen = Arc::clone(seen);
    move || {
        seen.lock()
            .unwrap()
            .push((std::thread::current().id(), Instant::now()))
    }
}

#[test]
fn fires_once_on_the_context_thread_and_not_early() {
    const INTERVAL: Duration = Duration::from_millis(20);
    let el = EventLoop::new();
    let ctx = new_ctx();
    let seen: Seen = Default::default();
    let start = Instant::now();
    Timer::single_shot_with_context(
        INTERVAL.as_millis() as u64,
        OwnedObject::object_id(&ctx),
        recorder(&seen),
    );
    assert!(
        pump_until(&el, || !seen.lock().unwrap().is_empty()),
        "watchdog: never fired"
    );
    let done = marker(ThreadId::current());
    assert!(pump_until(&el, || done.load(Ordering::SeqCst)));
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1, "must fire exactly once");
    assert_eq!(seen[0].0, std::thread::current().id());
    assert!(
        seen[0].1 + TOLERANCE >= start + INTERVAL,
        "fired before its expiry"
    );
}

#[test]
fn zero_interval_is_queued_and_can_reach_its_context() {
    let el = EventLoop::new();
    let ctx = new_ctx();
    let id = OwnedObject::object_id(&ctx);
    let reached = Arc::new(Mutex::new(None));
    let r = Arc::clone(&reached);
    Timer::single_shot_with_context(0, id, move || {
        *r.lock().unwrap() = Some(with_object_mut(id, |_| ()).is_some());
    });
    assert!(
        reached.lock().unwrap().is_none(),
        "must be queued, not run inline"
    );
    assert!(pump_until(&el, || reached.lock().unwrap().is_some()));
    assert_eq!(
        *reached.lock().unwrap(),
        Some(true),
        "functor must be able to borrow its context"
    );
}

#[test]
fn context_destroy_cancels_and_drops_the_functor() {
    let el = EventLoop::new();
    let ctx = new_ctx();
    let log = DropLog::default();
    let ran = Arc::new(AtomicBool::new(false));
    let (g, r) = (log.guard(), Arc::clone(&ran));
    Timer::single_shot_with_context(FAR, OwnedObject::object_id(&ctx), move || {
        let _g = &g;
        r.store(true, Ordering::SeqCst);
    });
    assert!(log.dropped_on().is_none());
    drop(ctx);
    assert_eq!(
        log.dropped_on(),
        Some(std::thread::current().id()),
        "captures must drop with the context"
    );
    let done = marker(ThreadId::current());
    assert!(pump_until(&el, || done.load(Ordering::SeqCst)));
    assert!(
        !ran.load(Ordering::SeqCst),
        "cancelled functor must never run"
    );
}

#[test]
fn armed_from_another_thread_runs_on_the_context_thread() {
    const INTERVAL: Duration = Duration::from_millis(20);
    let here = ThreadId::current();
    let _el = EventLoop::new();
    let ctx = new_ctx();
    let id = OwnedObject::object_id(&ctx);
    let (tid_tx, tid_rx) = channel();
    let (go_tx, go_rx) = channel::<()>();
    let seen: Seen = Default::default();
    let s = Arc::clone(&seen);
    let worker = std::thread::spawn(move || {
        let el = EventLoop::new();
        tid_tx.send(ThreadId::current()).unwrap();
        go_rx.recv_timeout(WAIT).unwrap();
        let ok = pump_until(&el, || !s.lock().unwrap().is_empty());
        (ok, std::thread::current().id())
    });
    let there = tid_rx.recv_timeout(WAIT).unwrap();
    assert_eq!(move_to_thread_by_id(id, there, here), Ok(()));
    let start = Instant::now();
    Timer::single_shot_with_context(INTERVAL.as_millis() as u64, id, recorder(&seen));
    go_tx.send(()).unwrap();
    let (ok, worker_thread) = worker.join().unwrap();
    assert!(ok, "watchdog: never fired");
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0, worker_thread, "must run on the context's thread");
    assert!(
        seen[0].1 + TOLERANCE >= start + INTERVAL,
        "fired before its expiry"
    );
    drop(seen);
    drop(ctx);
}

#[test]
fn forwarded_cross_thread_drop_of_the_context_cancels() {
    let here = ThreadId::current();
    let _el = EventLoop::new();
    let ctx = new_ctx();
    let id = OwnedObject::object_id(&ctx);
    let (tid_tx, tid_rx) = channel();
    let (armed_tx, armed_rx) = channel::<Arc<AtomicBool>>();
    let (dropped_tx, dropped_rx) = channel::<()>();
    let log = DropLog::default();
    let ran = Arc::new(AtomicBool::new(false));
    let l = log.clone();
    let worker = std::thread::spawn(move || {
        let el = EventLoop::new();
        tid_tx.send(ThreadId::current()).unwrap();
        let armed: Arc<AtomicBool> = armed_rx.recv_timeout(WAIT).unwrap();
        // The arm is queued before the marker, so the marker running means the arm ran.
        assert!(
            pump_until(&el, || armed.load(Ordering::SeqCst)),
            "watchdog: arm not delivered"
        );
        dropped_tx.send(()).unwrap();
        pump_until(&el, || l.dropped_on().is_some());
        (l.dropped_on(), std::thread::current().id())
    });
    let there = tid_rx.recv_timeout(WAIT).unwrap();
    assert_eq!(move_to_thread_by_id(id, there, here), Ok(()));
    let (g, r) = (log.guard(), Arc::clone(&ran));
    Timer::single_shot_with_context(FAR, id, move || {
        let _g = &g;
        r.store(true, Ordering::SeqCst);
    });
    armed_tx.send(marker(there)).unwrap();
    dropped_rx.recv_timeout(WAIT).unwrap();
    assert!(
        log.dropped_on().is_none(),
        "functor lives while the context does"
    );
    drop(ctx); // forwarded to the context's thread
    let (dropped_on, worker_thread) = worker.join().unwrap();
    assert_eq!(
        dropped_on,
        Some(worker_thread),
        "functor must drop on the context's thread"
    );
    assert!(!ran.load(Ordering::SeqCst));
}

#[test]
fn a_pending_shot_follows_its_context_to_the_new_thread_and_fires_there() {
    let here = ThreadId::current();
    let _el = EventLoop::new();
    let ctx = new_ctx();
    let id = OwnedObject::object_id(&ctx);
    let seen: Seen = Default::default();
    // Armed on this thread, whose loop never runs again after the move.
    Timer::single_shot_with_context(20, id, recorder(&seen));

    let (tid_tx, tid_rx) = channel();
    let s = Arc::clone(&seen);
    let worker = std::thread::spawn(move || {
        let el = EventLoop::new();
        tid_tx.send(ThreadId::current()).unwrap();
        let ok = pump_until(&el, || !s.lock().unwrap().is_empty());
        (ok, std::thread::current().id())
    });
    let there = tid_rx.recv_timeout(WAIT).unwrap();
    assert_eq!(move_to_thread_by_id(id, there, here), Ok(()));
    let (ok, worker_thread) = worker.join().unwrap();
    assert!(ok, "watchdog: the shot did not fire after the move");
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(
        seen[0].0, worker_thread,
        "must fire on the context's new thread"
    );
    drop(seen);
    drop(ctx);
}

#[test]
fn destroying_a_moved_context_cancels_its_pending_shot_on_the_new_thread() {
    let here = ThreadId::current();
    let _el = EventLoop::new();
    let ctx = new_ctx();
    let id = OwnedObject::object_id(&ctx);
    let log = DropLog::default();
    let ran = Arc::new(AtomicBool::new(false));
    let (g, r) = (log.guard(), Arc::clone(&ran));
    Timer::single_shot_with_context(FAR, id, move || {
        let _g = &g;
        r.store(true, Ordering::SeqCst);
    });

    let (tid_tx, tid_rx) = channel();
    let (armed_tx, armed_rx) = channel::<Arc<AtomicBool>>();
    let (dropped_tx, dropped_rx) = channel::<()>();
    let l = log.clone();
    let worker = std::thread::spawn(move || {
        let el = EventLoop::new();
        tid_tx.send(ThreadId::current()).unwrap();
        let armed: Arc<AtomicBool> = armed_rx.recv_timeout(WAIT).unwrap();
        assert!(
            pump_until(&el, || armed.load(Ordering::SeqCst)),
            "watchdog: adopt not delivered"
        );
        dropped_tx.send(()).unwrap();
        pump_until(&el, || l.dropped_on().is_some());
        (l.dropped_on(), std::thread::current().id())
    });
    let there = tid_rx.recv_timeout(WAIT).unwrap();
    assert_eq!(move_to_thread_by_id(id, there, here), Ok(()));
    armed_tx.send(marker(there)).unwrap();
    dropped_rx.recv_timeout(WAIT).unwrap();
    assert!(
        log.dropped_on().is_none(),
        "functor lives while the context does"
    );
    drop(ctx);
    let (dropped_on, worker_thread) = worker.join().unwrap();
    assert_eq!(
        dropped_on,
        Some(worker_thread),
        "functor must drop on the new thread"
    );
    assert!(!ran.load(Ordering::SeqCst));
}

#[test]
fn the_functor_is_not_shared_between_shots_of_one_context() {
    let el = EventLoop::new();
    let ctx = new_ctx();
    let id = OwnedObject::object_id(&ctx);
    let count = Arc::new(AtomicUsize::new(0));
    for _ in 0..3 {
        let c = Arc::clone(&count);
        Timer::single_shot_with_context(5, id, move || {
            c.fetch_add(1, Ordering::SeqCst);
        });
    }
    assert!(pump_until(&el, || count.load(Ordering::SeqCst) == 3));
}
