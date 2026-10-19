// SPDX-License-Identifier: LGPL-3.0-only
//! A `Timer` stays on the thread that created it.
//!
//! The handle is `!Send` and shares `Rc` state with the object the registry calls. Moving that
//! object to another thread would run its callbacks there while the handle keeps using the state
//! here, so `move_to_thread_by_id` refuses the object and every ancestor of it
//! (`QObject::can_move_to_thread`). A refusal sends no event, changes no affinity and re-arms
//! nothing on the target thread.

use std::cell::RefCell;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::event_loop::EventLoop;
#[allow(deprecated)]
use ferrokit_core::object::move_to_thread;
use ferrokit_core::object::{
    move_to_thread_by_id, query_object_children, query_object_thread, register_owned_qobject,
    set_parent_by_id, unregister_qobject, with_object, MoveByIdError, MoveError, ObjectData,
    ObjectDataMut, ObjectId, OwnedObject, QObject, ThreadId,
};
use ferrokit_core::timer::Timer;

/// Upper bound for each wait. A regression fails here instead of hanging.
const WATCHDOG: Duration = Duration::from_secs(10);

/// An id of another thread. The thread has exited; moves to it are only bookkeeping.
fn another_thread() -> ThreadId {
    std::thread::spawn(ThreadId::current).join().unwrap()
}

/// Counts the `ThreadChange` events it receives and answers the move policy as told.
struct Probe {
    data: ObjectData,
    movable: bool,
    thread_changes: u32,
}

impl QObject for Probe {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ObjectDataMut<'_> {
        ObjectDataMut::new(&mut self.data)
    }
    fn can_move_to_thread(&self) -> bool {
        self.movable
    }
    fn event(&mut self, event: &mut Event) -> bool {
        if matches!(event.kind, EventKind::ThreadChange) {
            self.thread_changes += 1;
        }
        false
    }
}

fn probe(movable: bool) -> OwnedObject<Probe> {
    // SAFETY: the owner stays on this thread and the tests use it only between callbacks.
    unsafe {
        register_owned_qobject(Probe {
            data: ObjectData::new(ObjectId::next()),
            movable,
            thread_changes: 0,
        })
    }
}

/// A second thread with its own event loop. It reports its id, waits for `go`, pumps for a while
/// and reports how many timers its registry then holds.
struct Watcher {
    tid: ThreadId,
    go: Sender<()>,
    timers: Receiver<usize>,
    join: JoinHandle<()>,
}

impl Watcher {
    fn spawn() -> Self {
        let (tid_tx, tid_rx) = channel();
        let (go, go_rx) = channel::<()>();
        let (timers_tx, timers) = channel();
        let join = std::thread::spawn(move || {
            tid_tx.send(ThreadId::current()).unwrap();
            go_rx.recv().unwrap();
            let el = EventLoop::new();
            for _ in 0..50 {
                el.process_events(false);
                std::thread::sleep(Duration::from_millis(1));
            }
            timers_tx
                .send(el.timer_registry().lock().unwrap().len())
                .unwrap();
        });
        let tid = tid_rx
            .recv_timeout(WATCHDOG)
            .expect("watcher thread must report its id");
        Self {
            tid,
            go,
            timers,
            join,
        }
    }

    /// Lets the watcher pump, and returns the number of timers its registry holds afterwards.
    fn timers_after_pumping(self) -> usize {
        self.go.send(()).unwrap();
        let timers = self
            .timers
            .recv_timeout(WATCHDOG)
            .expect("watcher thread must finish within the watchdog");
        self.join.join().expect("watcher thread must not panic");
        timers
    }
}

fn pump_until(el: &EventLoop, mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + WATCHDOG;
    while !done() {
        if Instant::now() > deadline {
            return false;
        }
        el.process_events(false);
        std::thread::sleep(Duration::from_millis(1));
    }
    true
}

fn pump_for(el: &EventLoop, span: Duration) {
    let deadline = Instant::now() + span;
    while Instant::now() < deadline {
        el.process_events(false);
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn counting_timer(interval_ms: u64) -> (Timer, Arc<AtomicU32>) {
    let ticks = Arc::new(AtomicU32::new(0));
    let counter = Arc::clone(&ticks);
    let timer = Timer::new();
    timer.set_interval(interval_ms);
    timer.timeout.connect(move |()| {
        counter.fetch_add(1, Ordering::SeqCst);
    });
    (timer, ticks)
}

/// The refusal is explicit, nothing is re-armed on the target, and the timer keeps its schedule,
/// its affinity and its ticks on the thread it was made on.
#[test]
fn a_timer_refuses_a_thread_move_and_keeps_ticking_where_it_is() {
    let here = ThreadId::current();
    let el = EventLoop::new();
    let (timer, ticks) = counting_timer(5);
    let armed = timer.start();
    assert!(armed.is_valid());
    let id = timer.object_id();
    let watcher = Watcher::spawn();
    let scheduled_here = el.timer_registry().lock().unwrap().len();

    assert_eq!(
        move_to_thread_by_id(id, watcher.tid, here),
        Err(MoveByIdError::NotMovable)
    );

    assert_eq!(
        query_object_thread(id),
        Some(here),
        "affinity must not change"
    );
    assert_eq!(
        with_object(id, |object| object.thread()),
        Some(here),
        "the object must stay registered here"
    );
    assert_eq!(timer.timer_id(), armed, "the schedule must not be replaced");
    assert!(timer.is_active());
    let registry = el.timer_registry().lock().unwrap();
    assert_eq!(registry.len(), scheduled_here);
    assert_eq!(registry.get(armed).map(|entry| entry.receiver), Some(id));
    drop(registry);
    assert_eq!(
        watcher.timers_after_pumping(),
        0,
        "nothing may be re-armed on the target thread"
    );
    assert!(
        pump_until(&el, || ticks.load(Ordering::SeqCst) >= 2),
        "the timer must keep ticking on its own thread"
    );
}

/// An idle timer is refused the same way, and the answer does not depend on it being armed.
#[test]
fn an_idle_timer_is_refused_too() {
    let (timer, _ticks) = counting_timer(5);
    assert_eq!(
        move_to_thread_by_id(timer.object_id(), another_thread(), ThreadId::current()),
        Err(MoveByIdError::NotMovable)
    );
    assert_eq!(
        query_object_thread(timer.object_id()),
        Some(ThreadId::current())
    );
}

/// A move to the thread the object is already on changes nothing, as in Qt, and so is not refused.
#[test]
fn moving_a_timer_to_the_thread_it_is_on_is_a_no_op() {
    let here = ThreadId::current();
    let timer = Timer::new();
    assert_eq!(move_to_thread_by_id(timer.object_id(), here, here), Ok(()));
    assert_eq!(query_object_thread(timer.object_id()), Some(here));
}

/// A timer anywhere below the root refuses the move of the whole subtree before the first
/// `ThreadChange`, wherever it sits among the children.
#[test]
fn a_timer_in_the_subtree_refuses_the_whole_move_before_any_event() {
    let here = ThreadId::current();
    let el = EventLoop::new();
    let target = another_thread();

    for layout in ["timer first", "timer last", "timer below a child"] {
        let root = probe(true);
        let first = probe(true);
        let middle = probe(true);
        let last = probe(true);
        let (timer, ticks) = counting_timer(5);
        timer.start();
        let (root_id, timer_id) = (root.data.id(), timer.object_id());
        let ids = [first.data.id(), middle.data.id(), last.data.id()];

        match layout {
            "timer first" => {
                set_parent_by_id(timer_id, Some(root_id)).unwrap();
                for id in ids {
                    set_parent_by_id(id, Some(root_id)).unwrap();
                }
            }
            "timer last" => {
                for id in ids {
                    set_parent_by_id(id, Some(root_id)).unwrap();
                }
                set_parent_by_id(timer_id, Some(root_id)).unwrap();
            }
            _ => {
                for id in ids {
                    set_parent_by_id(id, Some(root_id)).unwrap();
                }
                set_parent_by_id(timer_id, Some(ids[1])).unwrap();
            }
        }
        let children_before = query_object_children(root_id);

        assert_eq!(
            move_to_thread_by_id(root_id, target, here),
            Err(MoveByIdError::NotMovable),
            "{layout}"
        );

        for (name, node) in [
            ("root", &root),
            ("first", &first),
            ("middle", &middle),
            ("last", &last),
        ] {
            assert_eq!(
                node.thread_changes, 0,
                "{layout}: {name} got a ThreadChange"
            );
            assert_eq!(
                query_object_thread(node.data.id()),
                Some(here),
                "{layout}: {name} changed thread"
            );
        }
        assert_eq!(query_object_thread(timer_id), Some(here), "{layout}: timer");
        assert_eq!(query_object_children(root_id), children_before, "{layout}");
        assert!(timer.is_active(), "{layout}");
        assert!(
            pump_until(&el, || ticks.load(Ordering::SeqCst) >= 1),
            "{layout}: the timer must keep ticking here"
        );
    }
}

/// The hook is not specific to `Timer`: any object can refuse, and objects that do not still move
/// with an event, as before.
#[test]
fn an_object_moves_unless_it_refuses() {
    let here = ThreadId::current();
    let target = another_thread();

    let plain = probe(true);
    let plain_id = plain.data.id();
    assert_eq!(move_to_thread_by_id(plain_id, target, here), Ok(()));
    assert_eq!(
        plain.thread_changes, 1,
        "ThreadChange must reach a moved object"
    );
    assert_eq!(query_object_thread(plain_id), Some(target));
    assert_eq!(plain.data.thread_id(), target);
    // SAFETY: no callback of the object runs.
    unsafe { unregister_qobject(plain_id) };

    let pinned = probe(false);
    let pinned_id = pinned.data.id();
    assert_eq!(
        move_to_thread_by_id(pinned_id, target, here),
        Err(MoveByIdError::NotMovable)
    );
    assert_eq!(pinned.thread_changes, 0);
    assert_eq!(query_object_thread(pinned_id), Some(here));
    assert_eq!(pinned.data.thread_id(), here);
}

/// A parent whose subtree is movable still moves, and takes its children along.
#[test]
fn a_subtree_without_a_refusing_object_still_moves_as_a_whole() {
    let here = ThreadId::current();
    let target = another_thread();
    let root = probe(true);
    let child = probe(true);
    let (root_id, child_id) = (root.data.id(), child.data.id());
    set_parent_by_id(child_id, Some(root_id)).unwrap();

    assert_eq!(move_to_thread_by_id(root_id, target, here), Ok(()));

    assert_eq!((root.thread_changes, child.thread_changes), (1, 1));
    assert_eq!(query_object_thread(root_id), Some(target));
    assert_eq!(query_object_thread(child_id), Some(target));
    // SAFETY: no callback of the objects runs.
    unsafe {
        unregister_qobject(child_id);
        unregister_qobject(root_id);
    }
}

thread_local! {
    static HELD: RefCell<Option<Timer>> = const { RefCell::new(None) };
}

/// After a refused move the timer starts, stops and takes a new interval as before, and a timer
/// may still drop itself from its own slot.
#[test]
fn a_timer_works_on_its_own_thread_after_a_refused_move() {
    let here = ThreadId::current();
    let el = EventLoop::new();
    let (timer, ticks) = counting_timer(5);
    assert_eq!(
        move_to_thread_by_id(timer.object_id(), another_thread(), here),
        Err(MoveByIdError::NotMovable)
    );

    timer.start();
    assert!(pump_until(&el, || ticks.load(Ordering::SeqCst) >= 1));
    timer.stop();
    assert!(!timer.is_active());
    let stopped_at = ticks.load(Ordering::SeqCst);
    pump_for(&el, Duration::from_millis(40));
    assert_eq!(
        ticks.load(Ordering::SeqCst),
        stopped_at,
        "a stopped timer must not tick"
    );

    timer.set_interval(2);
    assert_eq!(timer.interval(), 2);
    assert!(
        !timer.is_active(),
        "a new interval must not start an idle timer"
    );
    timer.start();
    assert!(pump_until(&el, || ticks.load(Ordering::SeqCst) >= stopped_at + 2));
    timer.set_interval(3);
    assert!(
        timer.is_active(),
        "an active timer is restarted with the new interval"
    );
    let restarted_at = ticks.load(Ordering::SeqCst);
    assert!(pump_until(&el, || ticks.load(Ordering::SeqCst) >= restarted_at + 1));

    let drops = Arc::new(AtomicU32::new(0));
    let counted = Arc::clone(&drops);
    let selfish = Timer::new();
    selfish.set_interval(5);
    selfish.timeout.connect(move |()| {
        counted.fetch_add(1, Ordering::SeqCst);
        drop(HELD.with(|held| held.borrow_mut().take()));
    });
    let selfish_id = selfish.object_id();
    selfish.start();
    HELD.with(|held| *held.borrow_mut() = Some(selfish));
    assert!(pump_until(&el, || drops.load(Ordering::SeqCst) >= 1));
    pump_for(&el, Duration::from_millis(30));
    assert_eq!(
        drops.load(Ordering::SeqCst),
        1,
        "a dropped timer must not tick again"
    );
    assert!(
        with_object(selfish_id, |_| ()).is_none(),
        "the timer that dropped itself is still registered"
    );
    drop(timer);
    assert_eq!(
        el.timer_registry().lock().unwrap().len(),
        0,
        "no timer may stay scheduled"
    );
}

/// The deprecated by-reference move gives the same answer for a timer below its root.
#[test]
#[allow(deprecated)]
fn the_deprecated_move_refuses_a_subtree_with_a_timer_too() {
    let here = ThreadId::current();
    let _el = EventLoop::new();
    let mut root = probe(true);
    let root_id = root.data.id();
    let (timer, _ticks) = counting_timer(5);
    timer.start();
    set_parent_by_id(timer.object_id(), Some(root_id)).unwrap();

    assert_eq!(
        move_to_thread(&mut root.data, another_thread(), here),
        Err(MoveError::NotMovable)
    );

    assert_eq!(root.thread_changes, 0, "no ThreadChange before the refusal");
    assert_eq!(root.data.thread_id(), here);
    assert_eq!(query_object_thread(timer.object_id()), Some(here));
    assert!(timer.is_active());
}
