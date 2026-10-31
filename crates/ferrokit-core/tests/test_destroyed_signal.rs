// SPDX-License-Identifier: LGPL-3.0-only
//! `QObject::destroyed(QObject*)`: every object has a `destroyed` signal that carries its id.
//!
//! Qt `~QObject` emits it first, before the object's connections are cut and before its children
//! are deleted, even when signals of the object are blocked. The receivers get the id of an object
//! that is already dead: no registry access reaches it.

use parking_lot::Mutex;
use std::sync::atomic::Ordering;
use std::sync::mpsc::channel;
use std::sync::Arc;
use std::time::Duration;

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::event_loop::{post_event_to_thread, EventLoop};
use ferrokit_core::object::{
    add_owned_child, move_to_thread_by_id, register_owned_qobject, set_parent_by_id, with_object,
    with_object_mut, ObjectData, ObjectDataMut, ObjectId, OwnedObject, QObject, ThreadId,
};
use ferrokit_core::signal::{ConnectionType, Signal};

/// An `ObjectData` that lets `move_to_thread_by_id` move its object.
fn movable_data(id: ObjectId) -> ObjectData {
    // SAFETY: `Probe` holds an `ObjectData`, a `&'static str` and an `Arc<Mutex<_>>` log, all
    // `Send`; it has no `Rc`, `Cell`/`RefCell` shared with a handle left behind, or thread-local
    // state, so it may be used and dropped on the target thread of a move (docs of the opt-in).
    unsafe { ObjectData::new(id).with_thread_moves_allowed() }
}

const WAIT: Duration = Duration::from_secs(10);

type Log = Arc<Mutex<Vec<String>>>;

fn new_log() -> Log {
    Arc::new(Mutex::new(Vec::new()))
}

fn entries(log: &Log) -> Vec<String> {
    log.lock().clone()
}

struct Probe {
    data: ObjectData,
    name: &'static str,
    log: Log,
}

impl Probe {
    fn new(name: &'static str, log: &Log) -> Self {
        Self {
            data: movable_data(ObjectId::next()),
            name,
            log: Arc::clone(log),
        }
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        self.log.lock().push(format!("{} dropped", self.name));
    }
}

impl QObject for Probe {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ObjectDataMut<'_> {
        ObjectDataMut::new(&mut self.data)
    }
}

fn owned(name: &'static str, log: &Log) -> OwnedObject<Probe> {
    // SAFETY: `register_owned_qobject` allocates the object inside the returned owner, so the
    // registered pointer is the owner's own and the object does not move when the owner does. The
    // owner is used only through `with`/`with_mut` (which refuse during a callback of the object)
    // and dropped by the caller: on this thread, or from another thread, where `OwnedObject::drop`
    // forwards the destruction to the registration thread; a drop inside a callback is parked
    // until it returns (see `destroy_owned`).
    unsafe { register_owned_qobject(Probe::new(name, log)) }
}

/// A slot that logs `label` with the id it received, and checks the dying object is out of reach.
fn log_id(log: &Log, label: &'static str) -> impl Fn(&ObjectId) + Send + Sync + 'static {
    let log = Arc::clone(log);
    move |id: &ObjectId| {
        assert!(
            with_object(*id, |_| ()).is_none() && with_object_mut(*id, |_| ()).is_none(),
            "a slot of `destroyed` must not reach the dying object"
        );
        log.lock().push(format!("{label} {}", id.0));
    }
}

#[test]
fn direct_receivers_get_the_id_once_and_the_object_is_out_of_reach() {
    let log = new_log();
    let owner = owned("a", &log);
    let id = OwnedObject::object_id(&owner);
    let alive = owner.with(|p| p.data.liveness()).unwrap();
    let receiver = owned("r", &log);
    let signal: Signal<ObjectId> = owner.with(|p| p.destroyed().clone()).unwrap();

    signal.connect(log_id(&log, "plain"));
    signal.connect_object(
        OwnedObject::object_id(&receiver),
        ThreadId::current(),
        ConnectionType::Direct,
        log_id(&log, "receiver"),
    );
    signal.connect_object(
        OwnedObject::object_id(&receiver),
        ThreadId::current(),
        ConnectionType::Auto,
        {
            let log = Arc::clone(&log);
            let alive = Arc::clone(&alive);
            move |_| {
                assert!(
                    !alive.load(Ordering::Acquire),
                    "the liveness token QPointer reads is false in the slot"
                );
                log.lock().push("token dead".to_string());
            }
        },
    );

    assert!(
        entries(&log).is_empty(),
        "nothing is emitted while the object lives"
    );
    drop(owner);

    assert_eq!(
        entries(&log),
        vec![
            format!("plain {}", id.0),
            format!("receiver {}", id.0),
            "token dead".to_string(),
            "a dropped".to_string(),
        ],
        "each receiver ran once with the id, and before the object was freed"
    );
    // The object is gone: its signal no longer emits, so the count stays at one.
    signal.emit(&id);
    assert_eq!(entries(&log).len(), 4);
    drop(receiver);
}

#[test]
fn destroyed_is_emitted_when_signals_are_blocked() {
    let log = new_log();
    let mut owner = owned("a", &log);
    let id = OwnedObject::object_id(&owner);
    owner
        .with(|p| p.destroyed().connect(log_id(&log, "destroyed")))
        .unwrap();
    owner.with_mut(|p| p.block_signals(true)).unwrap();
    drop(owner);
    assert_eq!(
        entries(&log),
        vec![format!("destroyed {}", id.0), "a dropped".to_string()]
    );
}

#[test]
fn destroyed_is_emitted_for_an_object_that_was_never_registered() {
    let log = new_log();
    let probe = Probe::new("loose", &log);
    let id = probe.data.id();
    probe.data.destroyed().connect(log_id(&log, "destroyed"));
    drop(probe);
    assert_eq!(
        entries(&log),
        vec![
            "loose dropped".to_string(),
            // Its data is dropped after the type's own `Drop`, as `~QObject` runs after the
            // derived destructor.
            format!("destroyed {}", id.0),
        ]
    );
}

#[test]
fn a_connection_whose_receiver_is_the_dying_object_runs_and_is_cut_afterwards() {
    let log = new_log();
    let dying = owned("dying", &log);
    let dying_id = OwnedObject::object_id(&dying);
    let other = owned("other", &log);
    let other_signal: Signal<String> = other.with(|p| p.object_name_changed().clone()).unwrap();
    other_signal.connect_object(
        dying_id,
        ThreadId::current(),
        ConnectionType::Direct,
        |_: &String| {},
    );
    assert_eq!(other_signal.subscriber_count(), 1);

    // Connected to its own `destroyed`: it is still connected when the signal goes out.
    let own_signal: Signal<ObjectId> = dying.with(|p| p.destroyed().clone()).unwrap();
    own_signal.connect_object(
        dying_id,
        ThreadId::current(),
        ConnectionType::Direct,
        log_id(&log, "self"),
    );
    drop(dying);

    assert_eq!(
        entries(&log),
        vec![format!("self {}", dying_id.0), "dying dropped".to_string()]
    );
    assert_eq!(
        other_signal.subscriber_count(),
        0,
        "a connection that has the dying object as receiver is cut"
    );
    assert_eq!(
        own_signal.subscriber_count(),
        0,
        "so is one the dying object sent"
    );
}

#[test]
fn destroyed_goes_out_before_the_children_are_deleted() {
    let log = new_log();
    let parent = owned("parent", &log);
    let parent_id = OwnedObject::object_id(&parent);
    let child = Probe::new("child", &log);
    let child_id = child.data.id();
    let child_signal = child.data.destroyed().clone();
    let parent_signal: Signal<ObjectId> = parent.with(|p| p.destroyed().clone()).unwrap();
    parent_signal.connect({
        let log = Arc::clone(&log);
        move |id| {
            // The children are still there: they are only deleted after `destroyed`.
            log.lock().push(format!("parent destroyed {}", id.0));
        }
    });
    child_signal.connect(log_id(&log, "child destroyed"));
    // SAFETY: `child` is a fresh object created on this thread (the parent's thread), moved into
    // the `Box` here; the earlier uses (`child.data.id()`, `destroyed().clone()`) copied values, no
    // reference to the child is held across this call, and afterwards it is reached only through
    // the registry and the parent. No callback of the child runs here.
    let added = unsafe { add_owned_child(parent_id, Box::new(child)) };
    assert_eq!(added.map_err(|(e, _)| e), Ok(child_id));

    drop(parent);

    assert_eq!(
        entries(&log),
        vec![
            format!("parent destroyed {}", parent_id.0),
            // The type's own `Drop` runs before its `ObjectData` deletes the children.
            "parent dropped".to_string(),
            format!("child destroyed {}", child_id.0),
            "child dropped".to_string(),
        ]
    );
}

/// The owner is dropped by a callback of the object itself: the allocation is parked until the
/// callback returns, and `destroyed` is emitted when the owner is dropped, not when it is freed.
#[test]
fn destroyed_goes_out_when_the_owner_is_dropped_in_a_callback_of_the_object() {
    let log = new_log();
    let owner = owned("busy", &log);
    let id = OwnedObject::object_id(&owner);
    owner
        .with(|p| p.destroyed().connect(log_id(&log, "destroyed")))
        .unwrap();

    let log_in_callback = Arc::clone(&log);
    with_object_mut(id, move |_object| {
        drop(owner);
        log_in_callback.lock().push("callback goes on".to_string());
    })
    .unwrap();

    assert_eq!(
        entries(&log),
        vec![
            format!("destroyed {}", id.0),
            "callback goes on".to_string(),
            "busy dropped".to_string(),
        ]
    );
}

#[test]
fn delete_later_emits_destroyed_when_the_deferred_delete_is_delivered() {
    let log = new_log();
    let el = EventLoop::new();
    let owner = owned("late", &log);
    let id = OwnedObject::object_id(&owner);
    owner
        .with(|p| p.destroyed().connect(log_id(&log, "destroyed")))
        .unwrap();

    owner.with(|p| p.delete_later()).unwrap();
    assert!(
        entries(&log).is_empty(),
        "deleteLater does not destroy at once"
    );
    assert_eq!(el.flush_deferred_deletes(), 1);
    assert_eq!(entries(&log), vec![format!("destroyed {}", id.0)]);

    // The owner frees the memory later: no second emission.
    drop(owner);
    assert_eq!(
        entries(&log),
        vec![format!("destroyed {}", id.0), "late dropped".to_string()]
    );
}

/// A queued receiver on another thread gets the id when that thread runs its loop, after the
/// object is gone.
#[test]
fn a_queued_receiver_on_another_thread_gets_the_id_after_destruction() {
    let log = new_log();
    let owner = owned("a", &log);
    let id = OwnedObject::object_id(&owner);
    let signal: Signal<ObjectId> = owner.with(|p| p.destroyed().clone()).unwrap();

    let (ready_tx, ready_rx) = channel();
    let (go_tx, go_rx) = channel::<()>();
    let worker_log = Arc::clone(&log);
    let worker = std::thread::spawn(move || {
        let el = EventLoop::new();
        let receiver = owned("r", &worker_log);
        ready_tx
            .send((OwnedObject::object_id(&receiver), ThreadId::current()))
            .unwrap();
        go_rx.recv_timeout(WAIT).unwrap();
        el.process_events(false);
        std::thread::current().id()
    });
    let (receiver_id, receiver_thread) = ready_rx.recv_timeout(WAIT).unwrap();
    let slot_log = Arc::clone(&log);
    signal.connect_object(
        receiver_id,
        receiver_thread,
        ConnectionType::Queued,
        move |dead: &ObjectId| {
            assert!(with_object(*dead, |_| ()).is_none());
            slot_log.lock().push(format!(
                "queued {} on {:?}",
                dead.0,
                std::thread::current().id()
            ));
        },
    );

    drop(owner);
    assert_eq!(
        entries(&log),
        vec!["a dropped".to_string()],
        "a queued receiver does not run on the destroying thread"
    );
    go_tx.send(()).unwrap();
    let worker_thread = worker.join().unwrap();
    assert_eq!(
        entries(&log),
        vec![
            "a dropped".to_string(),
            format!("queued {} on {:?}", id.0, worker_thread),
            "r dropped".to_string(),
        ]
    );
}

/// The owner of an object that lives on another thread is dropped here: the destruction runs on
/// the registration thread, so `destroyed` is emitted there, once, and not from the dropping
/// thread. A queued receiver on this thread gets the id when this thread runs its loop.
#[test]
fn a_destruction_forwarded_to_the_registration_thread_emits_there() {
    let here = ThreadId::current();
    let el = EventLoop::new();
    let log = new_log();
    let owner = owned("moved", &log);
    let id = OwnedObject::object_id(&owner);
    let receiver = owned("home", &log);
    let signal: Signal<ObjectId> = owner.with(|p| p.destroyed().clone()).unwrap();

    let emitted_on = Arc::new(Mutex::new(Vec::new()));
    signal.connect({
        let emitted_on = Arc::clone(&emitted_on);
        move |_| emitted_on.lock().push(std::thread::current().id())
    });
    let slot_log = Arc::clone(&log);
    signal.connect_object(
        OwnedObject::object_id(&receiver),
        here,
        ConnectionType::Queued,
        move |dead: &ObjectId| {
            slot_log.lock().push(format!(
                "home got {} on {:?}",
                dead.0,
                std::thread::current().id()
            ));
        },
    );

    let (tid_tx, tid_rx) = channel();
    let (moved_tx, moved_rx) = channel::<()>();
    let (dropped_tx, dropped_rx) = channel::<()>();
    let worker = std::thread::spawn(move || {
        let el = EventLoop::new();
        tid_tx.send(ThreadId::current()).unwrap();
        moved_rx.recv_timeout(WAIT).unwrap();
        dropped_rx.recv_timeout(WAIT).unwrap();
        el.process_events(false);
        std::thread::current().id()
    });
    let there = tid_rx.recv_timeout(WAIT).unwrap();
    assert_eq!(move_to_thread_by_id(id, there, here), Ok(()));
    moved_tx.send(()).unwrap();

    drop(owner);
    assert!(
        emitted_on.lock().is_empty(),
        "the dropping thread must not emit for an object that lives on another thread"
    );
    dropped_tx.send(()).unwrap();
    let worker_thread = worker.join().unwrap();
    assert_eq!(
        *emitted_on.lock(),
        vec![worker_thread],
        "emitted once, there"
    );

    // The queued receiver is posted to this thread and runs here.
    assert_eq!(entries(&log), vec!["moved dropped".to_string()]);
    el.process_events(false);
    assert_eq!(
        entries(&log),
        vec![
            "moved dropped".to_string(),
            format!("home got {} on {:?}", id.0, std::thread::current().id()),
        ]
    );
    drop(receiver);
}

/// The parent's owner is dropped inside a callback of the parent, so the destruction is parked and
/// the descendants are unregistered at once. The parent's `destroyed` still goes out first, and
/// the parent is already dead when a descendant's slot runs.
#[test]
fn a_parked_parent_emits_before_its_descendants() {
    let log = new_log();
    let parent = owned("parent", &log);
    let parent_id = OwnedObject::object_id(&parent);
    let parent_alive = parent.with(|p| p.data.liveness()).unwrap();
    let child = Probe::new("child", &log);
    let child_id = child.data.id();
    child.data.destroyed().connect({
        let log = Arc::clone(&log);
        let parent_alive = Arc::clone(&parent_alive);
        move |id| {
            assert!(
                !parent_alive.load(Ordering::Acquire),
                "the parent is already dead when a descendant's slot runs"
            );
            log.lock().push(format!("child destroyed {}", id.0));
        }
    });
    parent
        .with(|p| p.destroyed().connect(log_id(&log, "parent destroyed")))
        .unwrap();
    // SAFETY: `child` is a fresh object created on this thread (the parent's thread), moved into
    // the `Box` here; the earlier uses (`child.data.id()`, `destroyed().clone()`, `connect`)
    // copied values and cloned a shared signal, no reference to the child is held across this
    // call, and afterwards it is reached only through the registry and the parent. No callback of
    // the child runs here.
    let added = unsafe { add_owned_child(parent_id, Box::new(child)) };
    assert_eq!(added.map_err(|(e, _)| e), Ok(child_id));

    let log_in_callback = Arc::clone(&log);
    with_object_mut(parent_id, move |_object| {
        drop(parent);
        log_in_callback.lock().push("callback goes on".to_string());
    })
    .unwrap();

    assert_eq!(
        entries(&log),
        vec![
            format!("parent destroyed {}", parent_id.0),
            format!("child destroyed {}", child_id.0),
            "callback goes on".to_string(),
            "parent dropped".to_string(),
            "child dropped".to_string(),
        ]
    );
}

#[test]
fn a_panicking_slot_is_contained_and_the_teardown_completes() {
    let log = new_log();
    let owner = owned("a", &log);
    let id = OwnedObject::object_id(&owner);
    owner
        .with(|p| {
            p.destroyed()
                .connect(|_| panic!("destroyed slot panics on purpose"));
            p.destroyed().connect(log_id(&log, "after the panic"));
        })
        .unwrap();
    drop(owner);
    assert_eq!(
        entries(&log),
        vec![format!("after the panic {}", id.0), "a dropped".to_string()]
    );
}

/// A slot that calls back into the object by its id finds nothing to act on, and does not panic or
/// deadlock.
#[test]
fn a_slot_acting_on_the_dying_id_has_no_effect() {
    let log = new_log();
    let el = EventLoop::new();
    let owner = owned("a", &log);
    let other = owned("other", &log);
    let other_id = OwnedObject::object_id(&other);
    let signal: Signal<ObjectId> = owner.with(|p| p.destroyed().clone()).unwrap();
    signal.connect({
        let log = Arc::clone(&log);
        let signal = signal.clone();
        move |dying| {
            let late = signal.connect({
                let log = Arc::clone(&log);
                move |_| log.lock().push("connected from the slot".to_string())
            });
            let child_result = set_parent_by_id(*dying, Some(other_id));
            let parent_result = set_parent_by_id(other_id, Some(*dying));
            post_event_to_thread(
                ThreadId::current(),
                *dying,
                Event::new(EventKind::DeferredDelete {
                    loop_level: 0,
                    scope_level: 0,
                }),
            );
            log.lock().push(format!(
                "reparent refused {} {} {:?}",
                child_result.is_err(),
                parent_result.is_err(),
                late.0 > 0
            ));
        }
    });

    drop(owner);

    assert_eq!(
        entries(&log),
        vec![
            "reparent refused true true true".to_string(),
            "a dropped".to_string()
        ],
        "the slot connected during the emission does not run in it"
    );
    assert_eq!(
        el.flush_deferred_deletes(),
        0,
        "a deferred delete posted to the dying id is purged"
    );
    assert_eq!(other.with(|p| p.parent()).unwrap(), None);
}

/// Unregistering a live object is its destruction for connections and pointers, so it emits
/// `destroyed` once; the later drop does not emit again.
#[test]
fn unregistering_a_live_object_emits_destroyed_once() {
    let log = new_log();
    let mut owner = owned("a", &log);
    let id = OwnedObject::object_id(&owner);
    owner
        .with(|p| p.destroyed().connect(log_id(&log, "destroyed")))
        .unwrap();
    // SAFETY: `unregister` follows `unregister_qobject`: the memory stays valid because `owner`
    // is alive and unmoved (it is dropped only afterwards), and no registry callback of the object
    // runs: the only code running on it is this `with_mut` closure on the registration thread.
    owner.with_mut(|p| unsafe { p.data.unregister() }).unwrap();
    assert_eq!(entries(&log), vec![format!("destroyed {}", id.0)]);
    drop(owner);
    assert_eq!(
        entries(&log),
        vec![format!("destroyed {}", id.0), "a dropped".to_string()]
    );
}

thread_local! {
    static HELD: std::cell::RefCell<Option<OwnedObject<Probe>>> = const { std::cell::RefCell::new(None) };
}

/// An object owned by a thread local is destroyed while the thread's locals are torn down. The
/// sender stack (touched after the holder, so destroyed before it) is gone by then; emitting
/// `destroyed` must not panic on it, which would abort the process from a TLS destructor.
#[test]
fn an_object_held_by_a_thread_local_emits_destroyed_at_thread_exit() {
    let log = new_log();
    let worker_log = Arc::clone(&log);
    let thread = std::thread::spawn(move || {
        let owner = owned("tls", &worker_log);
        owner
            .with(|p| {
                p.destroyed()
                    .connect(|_| assert_eq!(ferrokit_core::signal::sender(), None))
            })
            .unwrap();
        // The holder registers its destructor first, the sender stack second.
        HELD.with(|held| *held.borrow_mut() = Some(owner));
        let _ = ferrokit_core::signal::sender();
    });
    thread
        .join()
        .expect("the thread's locals were destroyed without a panic");
    assert_eq!(entries(&log), vec!["tls dropped".to_string()]);
}
