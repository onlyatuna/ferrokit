// SPDX-License-Identifier: LGPL-3.0-only
//! Object state that must follow `QObject::moveToThread`.
//!
//! - Timers are disarmed on the source and re-armed on the target (qobject.cpp:1484-1505).
//! - `ThreadChange` is sent to the whole subtree before the affinity changes (qobject.cpp:1749,
//!   1785-1786).
//! - Queued events of the subtree move to the target thread's queue, in order, even when that
//!   thread has no loop yet (qobject.cpp:1805-1815).
//! - A queued `Auto` connection is decided at emit time from the receiver's live thread.
//! - A foreign-thread `send_event` is rejected in every build. Qt checks this only in debug builds
//!   (qcoreapplication.cpp:1223-1225); the rejection here is a deliberate ferrokit choice.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::event_loop::{
    get_thread_event_sender, post_event_to_thread, post_event_to_thread_with_priority, EventLoop,
};
use ferrokit_core::object::{
    add_owned_child, move_to_thread, query_object_thread, register_qobject, send_event,
    unregister_qobject, ObjectData, ObjectId, QObject, ThreadId,
};
use ferrokit_core::signal::Signal;
use ferrokit_core::timer::{start_object_timer, TimerType};

/// Upper bound for each cross-thread wait. A regression fails here instead of hanging.
const WATCHDOG: Duration = Duration::from_secs(10);

type Ran = Arc<Mutex<Vec<(&'static str, ThreadId)>>>;

/// A thread that becomes the registration thread of moved objects. It reports its id, waits for
/// `go`, creates its `EventLoop` only then, and pumps until `finished` holds or `max_turns` run
/// out. It then unregisters `owned` on its own thread and reports whether `finished` held.
struct Owner {
    tid: ThreadId,
    go: Sender<()>,
    outcome: Receiver<bool>,
    join: JoinHandle<()>,
}

impl Owner {
    fn spawn<F>(owned: Vec<ObjectId>, max_turns: usize, finished: F) -> Self
    where
        F: Fn() -> bool + Send + 'static,
    {
        let (tid_tx, tid_rx) = channel();
        let (go, go_rx) = channel::<()>();
        let (outcome_tx, outcome) = channel();
        let join = std::thread::spawn(move || {
            tid_tx.send(ThreadId::current()).unwrap();
            go_rx.recv().unwrap();
            let el = EventLoop::new();
            for _ in 0..max_turns {
                if finished() {
                    break;
                }
                el.process_events(false);
                std::thread::sleep(Duration::from_millis(1));
            }
            let ok = finished();
            for id in owned {
                // SAFETY: after the move this thread is each object's registration thread, and
                // the test thread keeps the objects alive until `outcome` has been received.
                unsafe { unregister_qobject(id) };
            }
            outcome_tx.send(ok).unwrap();
        });
        let tid = tid_rx
            .recv_timeout(WATCHDOG)
            .expect("owner thread must report its id");
        Self {
            tid,
            go,
            outcome,
            join,
        }
    }

    /// Lets the owner run to completion and returns whether its `finished` condition held.
    fn finish(self) -> bool {
        self.go.send(()).unwrap();
        let ok = self
            .outcome
            .recv_timeout(WATCHDOG)
            .expect("owner thread must finish within the watchdog");
        self.join.join().expect("owner thread must not panic");
        ok
    }
}

/// Object without behaviour of its own. It receives posted events and signal slots.
struct Plain {
    data: ObjectData,
}

impl Plain {
    fn new() -> Self {
        Self {
            data: ObjectData::new(ObjectId::next()),
        }
    }
}

impl QObject for Plain {
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

/// Records the thread on which each of its timer ticks is delivered.
struct TickProbe {
    data: ObjectData,
    ticks: Arc<Mutex<Vec<ThreadId>>>,
}

impl QObject for TickProbe {
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
    fn timer_event(&mut self, _timer_id: u64) {
        self.ticks.lock().unwrap().push(ThreadId::current());
    }
}

/// Records the affinity `query_object_thread` reports while it handles `ThreadChange`.
struct AffinityProbe {
    data: ObjectData,
    seen: Arc<Mutex<Option<ThreadId>>>,
}

impl QObject for AffinityProbe {
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
        if matches!(event.kind, EventKind::ThreadChange) {
            *self.seen.lock().unwrap() = query_object_thread(self.data.id);
            return true;
        }
        false
    }
}

/// Counts the owner callbacks it receives.
struct Counter {
    data: ObjectData,
    calls: Arc<AtomicUsize>,
}

impl QObject for Counter {
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
    fn event(&mut self, _event: &mut Event) -> bool {
        self.calls.fetch_add(1, Ordering::SeqCst);
        true
    }
}

fn record(ran: &Ran, name: &'static str) -> Event {
    let ran = Arc::clone(ran);
    Event::new(EventKind::MetaCall(Box::new(move |_| {
        ran.lock().unwrap().push((name, ThreadId::current()))
    })))
}

/// Timers of a moved object are re-armed on the target thread and keep firing there.
#[test]
fn timers_of_moved_object_keep_firing_on_the_target_thread() {
    let source = ThreadId::current();
    let ticks = Arc::new(Mutex::new(Vec::new()));
    let mut probe = TickProbe {
        data: ObjectData::new(ObjectId::next()),
        ticks: Arc::clone(&ticks),
    };
    let probe_id = probe.data.id;
    // The timer is armed in this thread's registry, so this thread needs a timer context.
    let _source_loop = EventLoop::new();
    // SAFETY: `probe` stays on this thread and alive until the owner has unregistered it.
    unsafe { register_qobject(&mut probe) };
    assert!(start_object_timer(probe_id, 5, TimerType::Precise).is_valid());

    let watched = Arc::clone(&ticks);
    let owner = Owner::spawn(vec![probe_id], 5_000, move || {
        watched.lock().unwrap().len() >= 3
    });
    let owner_tid = owner.tid;
    assert_eq!(move_to_thread(&mut probe.data, owner_tid, source), Ok(()));
    assert!(
        owner.finish(),
        "the timer of a moved object must keep ticking on the target thread"
    );

    let ticks = ticks.lock().unwrap();
    assert!(ticks.len() >= 3, "ticks delivered: {}", ticks.len());
    assert!(
        ticks.iter().all(|t| *t == owner_tid),
        "ticks ran on {ticks:?}, expected only {owner_tid:?}"
    );
}

/// `ThreadChange` runs before the affinity flip, and every object of the subtree reports the
/// target thread afterwards.
#[test]
fn thread_change_precedes_affinity_flip_and_reaches_the_subtree() {
    let source = ThreadId::current();
    let root_seen = Arc::new(Mutex::new(None));
    let child_seen = Arc::new(Mutex::new(None));
    let grand_seen = Arc::new(Mutex::new(None));

    let mut root = AffinityProbe {
        data: ObjectData::new(ObjectId::next()),
        seen: Arc::clone(&root_seen),
    };
    let root_id = root.data.id;
    // SAFETY: `root` stays on this thread and alive until the owner has unregistered it.
    unsafe { register_qobject(&mut root) };

    let child = Box::new(AffinityProbe {
        data: ObjectData::new(ObjectId::next()),
        seen: Arc::clone(&child_seen),
    });
    let child_id = child.data.id;
    // SAFETY: `child` is boxed and owned by `root` at a stable address.
    let _ = unsafe { add_owned_child(root_id, child) }
        .unwrap_or_else(|(e, _)| panic!("attach refused: {e:?}"));
    let grand = Box::new(AffinityProbe {
        data: ObjectData::new(ObjectId::next()),
        seen: Arc::clone(&grand_seen),
    });
    let grand_id = grand.data.id;
    // SAFETY: `grand` is boxed and owned by `child` at a stable address.
    let _ = unsafe { add_owned_child(child_id, grand) }
        .unwrap_or_else(|(e, _)| panic!("attach refused: {e:?}"));

    let owner = Owner::spawn(vec![grand_id, child_id, root_id], 1, || true);
    let owner_tid = owner.tid;
    assert_eq!(move_to_thread(&mut root.data, owner_tid, source), Ok(()));

    for (name, seen) in [
        ("root", &root_seen),
        ("child", &child_seen),
        ("grandchild", &grand_seen),
    ] {
        assert_eq!(
            *seen.lock().unwrap(),
            Some(source),
            "{name}: ThreadChange must observe the source affinity"
        );
    }
    assert_eq!(query_object_thread(child_id), Some(owner_tid));
    assert_eq!(query_object_thread(grand_id), Some(owner_tid));
    assert!(owner.finish());
}

/// A zero-interval timer is driven by posted `ZeroTimer` events. After a move it must still tick
/// at most once per loop turn. The test checks the delivered count, not the queue contents.
#[test]
fn zero_interval_timer_of_moved_object_ticks_at_most_once_per_turn() {
    const TURNS: usize = 20;
    let source = ThreadId::current();
    let ticks = Arc::new(Mutex::new(Vec::new()));
    let mut probe = TickProbe {
        data: ObjectData::new(ObjectId::next()),
        ticks: Arc::clone(&ticks),
    };
    let probe_id = probe.data.id;
    let _source_loop = EventLoop::new();
    // SAFETY: `probe` stays on this thread and alive until the owner has unregistered it.
    unsafe { register_qobject(&mut probe) };
    assert!(start_object_timer(probe_id, 0, TimerType::Precise).is_valid());

    let owner = Owner::spawn(vec![probe_id], TURNS, || false);
    let owner_tid = owner.tid;
    assert_eq!(move_to_thread(&mut probe.data, owner_tid, source), Ok(()));
    let _ = owner.finish();

    let ticks = ticks.lock().unwrap();
    assert!(
        (1..=TURNS).contains(&ticks.len()),
        "{} ticks in {TURNS} turns; at most one per turn is allowed",
        ticks.len()
    );
    assert!(ticks.iter().all(|t| *t == owner_tid));
}

/// Queued events of the moved subtree move to the target thread's queue, in order. Events posted
/// to the subtree after the move are routed there too. The target has no loop until it starts
/// one, so all of them wait in its queue.
#[test]
fn queued_events_of_moved_subtree_wait_for_target_loop_in_order() {
    let source = ThreadId::current();
    let ran: Ran = Arc::new(Mutex::new(Vec::new()));

    let mut root = Plain::new();
    let root_id = root.data.id;
    // SAFETY: `root` stays on this thread and alive until the owner has unregistered it.
    unsafe { register_qobject(&mut root) };
    let child = Box::new(Plain::new());
    let child_id = child.data.id;
    // SAFETY: `child` is boxed and owned by `root` at a stable address.
    let _ = unsafe { add_owned_child(root_id, child) }
        .unwrap_or_else(|(e, _)| panic!("attach refused: {e:?}"));

    // Posted while the subtree is still on this thread.
    post_event_to_thread(source, child_id, record(&ran, "before-move"));

    let watched = Arc::clone(&ran);
    let owner = Owner::spawn(vec![child_id, root_id], 5, move || {
        watched.lock().unwrap().len() >= 2
    });
    let owner_tid = owner.tid;
    assert_eq!(move_to_thread(&mut root.data, owner_tid, source), Ok(()));

    // Posted after the move, to the same receiver.
    post_event_to_thread(source, child_id, record(&ran, "after-move"));

    assert!(
        owner.finish(),
        "both events must be delivered by the target thread's loop"
    );
    assert_eq!(
        *ran.lock().unwrap(),
        vec![("before-move", owner_tid), ("after-move", owner_tid)]
    );
}
#[test]
fn transferred_events_preserve_priority_ordering() {
    let source = ThreadId::current();
    let ran: Ran = Arc::new(Mutex::new(Vec::new()));

    let mut root = Plain::new();
    let root_id = root.data.id;
    // SAFETY: root stays on this thread and alive until owner unregisters it.
    unsafe { register_qobject(&mut root) };

    // Post low priority then high priority while on source thread
    post_event_to_thread_with_priority(source, root_id, record(&ran, "low-priority"), 0);
    post_event_to_thread_with_priority(source, root_id, record(&ran, "high-priority"), 10);

    let watched = Arc::clone(&ran);
    let owner = Owner::spawn(vec![root_id], 5, move || watched.lock().unwrap().len() >= 2);
    let owner_tid = owner.tid;
    assert_eq!(move_to_thread(&mut root.data, owner_tid, source), Ok(()));

    assert!(
        owner.finish(),
        "transferred events must be delivered on target"
    );
    assert_eq!(
        *ran.lock().unwrap(),
        vec![("high-priority", owner_tid), ("low-priority", owner_tid)],
        "Transferred events must respect priority ordering (qobject.cpp:1805-1815)"
    );
}

/// An event added through a direct `EventLoopHandle::post_event` bypasses routing and lands in the
/// source queue. Pumping that queue after the move must hand it to the receiver's current thread.
#[test]
fn post_through_stale_handle_is_forwarded_to_the_owner_thread() {
    let source = ThreadId::current();
    let ran: Ran = Arc::new(Mutex::new(Vec::new()));
    let el = EventLoop::new();
    let stale = get_thread_event_sender(source).expect("this thread's loop must be registered");

    let mut plain = Plain::new();
    let id = plain.data.id;
    // SAFETY: `plain` stays on this thread and alive until the owner has unregistered it.
    unsafe { register_qobject(&mut plain) };

    let watched = Arc::clone(&ran);
    let owner = Owner::spawn(vec![id], 5, move || watched.lock().unwrap().len() >= 1);
    let owner_tid = owner.tid;
    assert_eq!(move_to_thread(&mut plain.data, owner_tid, source), Ok(()));

    stale.post_event(id, record(&ran, "stale"));
    el.process_events(false);

    assert!(
        owner.finish(),
        "the forwarded event must be delivered by the owner thread"
    );
    assert_eq!(*ran.lock().unwrap(), vec![("stale", owner_tid)]);
}

/// `ConnectionType::Auto` (`connect_to`) decides direct or queued delivery at emit time, from the
/// receiver's live thread. After a move the slot must run on the new thread, not the emitter.
#[test]
fn auto_connection_follows_receiver_thread_after_move() {
    let source = ThreadId::current();
    let mut receiver = Plain::new();
    let receiver_id = receiver.data.id;
    // SAFETY: `receiver` stays on this thread and alive until the owner has unregistered it.
    unsafe { register_qobject(&mut receiver) };

    let signal: Signal<i32> = Signal::new();
    let ran_on: Arc<Mutex<Vec<ThreadId>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&ran_on);
    let _conn = signal.connect_to(&receiver, move |_: &i32| {
        sink.lock().unwrap().push(ThreadId::current())
    });

    let watched = Arc::clone(&ran_on);
    let owner = Owner::spawn(vec![receiver_id], 5_000, move || {
        !watched.lock().unwrap().is_empty()
    });
    let owner_tid = owner.tid;
    assert_eq!(
        move_to_thread(&mut receiver.data, owner_tid, source),
        Ok(())
    );

    signal.emit(&7);
    assert!(
        ran_on.lock().unwrap().is_empty(),
        "the slot must not run on the emitting thread"
    );
    assert!(
        owner.finish(),
        "the queued slot must run on the receiver's thread"
    );
    assert_eq!(*ran_on.lock().unwrap(), vec![owner_tid]);
}

/// ferrokit rejects a foreign-thread `send_event`: it returns false and the owner callback does not
/// run. This is a deliberate divergence from Qt's release behaviour.
#[test]
fn send_event_from_foreign_thread_is_rejected_without_running_the_owner_callback() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut obj = Counter {
        data: ObjectData::new(ObjectId::next()),
        calls: Arc::clone(&calls),
    };
    let id = obj.data.id;
    // SAFETY: `obj` stays on this thread and alive until it is unregistered below.
    unsafe { register_qobject(&mut obj) };

    let (result_tx, result_rx) = channel();
    let foreign = std::thread::spawn(move || {
        let mut event = Event::new(EventKind::UpdateRequest);
        result_tx.send(send_event(id, &mut event)).unwrap();
    });
    let delivered = result_rx
        .recv_timeout(WATCHDOG)
        .expect("foreign send_event must return within the watchdog");
    foreign.join().expect("foreign sender must not panic");

    assert!(
        !delivered,
        "a foreign-thread send_event must report failure"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "the owner callback must not run on a foreign thread"
    );
    // SAFETY: the foreign sender has finished, and this thread is the registration thread.
    unsafe { unregister_qobject(id) };
}
