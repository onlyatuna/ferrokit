// SPDX-License-Identifier: LGPL-3.0-only
//! A `Timer` stays on the thread that created it.
//!
//! The handle is `!Send` and shares `Rc` state with the object the registry calls. Moving that
//! object to another thread would run its callbacks there while the handle keeps using the state
//! here, so `move_to_thread_by_id` refuses the object and every ancestor of it
//! (`ObjectData::can_move_to_thread`). A refusal sends no event, changes no affinity and re-arms
//! nothing on the target thread.

use std::cell::{Cell, RefCell};
use std::mem::ManuallyDrop;
use std::ptr::NonNull;
use std::rc::Rc;
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
    add_owned_child, dispatch_to_object, move_to_thread_by_id, query_object_children,
    query_object_thread, register_owned_qobject, register_qobject_ptr, reparent, set_parent,
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

/// Counts the `ThreadChange` events it receives. Whether it may change thread is set by `probe_data`.
struct Probe {
    data: ObjectData,
    thread_changes: u32,
}

impl QObject for Probe {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ObjectDataMut<'_> {
        ObjectDataMut::new(&mut self.data)
    }
    fn as_qobject_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn event(&mut self, event: &mut Event) -> bool {
        if matches!(event.kind, EventKind::ThreadChange) {
            self.thread_changes += 1;
        }
        false
    }
}

/// The `ObjectData` of a `Probe` that may change thread (`movable`) or stays where it is made.
fn probe_data(movable: bool) -> ObjectData {
    let data = ObjectData::new(ObjectId::next());
    if movable {
        // SAFETY: `Probe` holds plain counters; nothing of it is shared with another thread.
        unsafe { data.with_thread_moves_allowed() }
    } else {
        data
    }
}

fn probe(movable: bool) -> OwnedObject<Probe> {
    // SAFETY: the owner stays on this thread and the tests use it only between callbacks.
    unsafe {
        register_owned_qobject(Probe {
            data: probe_data(movable),
            thread_changes: 0,
        })
    }
}

/// A registered `Probe` whose `ObjectData` the test must lend as `&mut` to the deprecated
/// by-reference APIs (`move_to_thread`, `reparent`). An `OwnedObject` only lends it through
/// `with_mut`, which marks the object borrowed and so changes what those APIs answer. The box is
/// registered by pointer and every access goes through that pointer, scoped to one call; the test
/// never holds a reference across a callback.
struct RawRoot(NonNull<Probe>);

impl RawRoot {
    fn new(movable: bool) -> Self {
        let ptr = Box::into_raw(Box::new(Probe {
            data: probe_data(movable),
            thread_changes: 0,
        }));
        // SAFETY: the allocation is owned by this `RawRoot` until `drop`, never moves, and is
        // accessed only through `ptr`.
        unsafe {
            register_qobject_ptr(NonNull::new_unchecked(ptr as *mut dyn QObject));
            Self(NonNull::new_unchecked(ptr))
        }
    }

    fn read<R>(&self, f: impl FnOnce(&Probe) -> R) -> R {
        // SAFETY: no callback of the object runs while `f` does, and no other reference is live.
        unsafe { f(&*self.0.as_ptr()) }
    }

    fn with_data<R>(&self, f: impl FnOnce(&mut ObjectData) -> R) -> R {
        // SAFETY: as in `read`; the borrow ends with `f`.
        unsafe { f(&mut (*self.0.as_ptr()).data) }
    }

    fn id(&self) -> ObjectId {
        self.read(|p| p.data.id())
    }

    fn changes(&self) -> u32 {
        self.read(|p| p.thread_changes)
    }

    fn thread_id(&self) -> ThreadId {
        self.read(|p| p.data.thread_id())
    }
}

impl Drop for RawRoot {
    fn drop(&mut self) {
        // SAFETY: the pointer came from `Box::into_raw` and is dropped once.
        drop(unsafe { Box::from_raw(self.0.as_ptr()) });
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

/// A live thread that runs closures on request. An object moved to its id belongs to it, so the
/// owner here no longer lends the object's data (`OwnedObject::with` answers `None` off the
/// registration thread); the tests read it through the registry on this thread instead.
struct Host {
    tid: ThreadId,
    jobs: Option<Sender<Box<dyn FnOnce() + Send>>>,
    join: Option<JoinHandle<()>>,
}

impl Host {
    fn spawn() -> Self {
        let (tid_tx, tid_rx) = channel();
        let (jobs, jobs_rx) = channel::<Box<dyn FnOnce() + Send>>();
        let join = std::thread::spawn(move || {
            tid_tx.send(ThreadId::current()).unwrap();
            while let Ok(job) = jobs_rx.recv() {
                job();
            }
        });
        let tid = tid_rx
            .recv_timeout(WATCHDOG)
            .expect("host thread must report its id");
        Self {
            tid,
            jobs: Some(jobs),
            join: Some(join),
        }
    }

    fn run<R: Send + 'static>(&self, f: impl FnOnce() -> R + Send + 'static) -> R {
        let (tx, rx) = channel();
        self.jobs
            .as_ref()
            .unwrap()
            .send(Box::new(move || {
                let _ = tx.send(f());
            }))
            .unwrap();
        rx.recv_timeout(WATCHDOG)
            .expect("host thread must answer within the watchdog")
    }

    /// `(thread_changes, ObjectData::thread_id)` of the `Probe` registered as `id`, read on this
    /// thread, which the object lives on.
    fn probe_state(&self, id: ObjectId) -> (u32, ThreadId) {
        self.run(move || {
            with_object(id, |o| {
                let p = o
                    .as_qobject_any()
                    .and_then(|any| any.downcast_ref::<Probe>())
                    .expect("the id names a Probe");
                (p.thread_changes, p.data.thread_id())
            })
            .expect("the object must be readable on its own thread")
        })
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.jobs.take();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
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
        let (root_id, timer_id) = (OwnedObject::object_id(&root), timer.object_id());
        let ids = [
            OwnedObject::object_id(&first),
            OwnedObject::object_id(&middle),
            OwnedObject::object_id(&last),
        ];

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
                node.with(|n| n.thread_changes).unwrap(),
                0,
                "{layout}: {name} got a ThreadChange"
            );
            assert_eq!(
                query_object_thread(OwnedObject::object_id(node)),
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
    let host = Host::spawn();
    let target = host.tid;

    let plain = probe(true);
    let plain_id = OwnedObject::object_id(&plain);
    assert_eq!(move_to_thread_by_id(plain_id, target, here), Ok(()));
    assert_eq!(query_object_thread(plain_id), Some(target));
    assert_eq!(
        host.probe_state(plain_id),
        (1, target),
        "ThreadChange must reach a moved object"
    );
    assert!(plain.with(|_| ()).is_none());
    // SAFETY: no callback of the object runs.
    unsafe { unregister_qobject(plain_id) };

    let pinned = probe(false);
    let pinned_id = OwnedObject::object_id(&pinned);
    assert_eq!(
        move_to_thread_by_id(pinned_id, target, here),
        Err(MoveByIdError::NotMovable)
    );
    assert_eq!(pinned.with(|p| p.thread_changes).unwrap(), 0);
    assert_eq!(query_object_thread(pinned_id), Some(here));
    assert_eq!(pinned.with(|p| p.data.thread_id()).unwrap(), here);
}

/// A parent whose subtree is movable still moves, and takes its children along.
#[test]
fn a_subtree_without_a_refusing_object_still_moves_as_a_whole() {
    let here = ThreadId::current();
    let host = Host::spawn();
    let target = host.tid;
    let root = probe(true);
    let child = probe(true);
    let (root_id, child_id) = (
        OwnedObject::object_id(&root),
        OwnedObject::object_id(&child),
    );
    set_parent_by_id(child_id, Some(root_id)).unwrap();

    assert_eq!(move_to_thread_by_id(root_id, target, here), Ok(()));

    assert_eq!(
        (host.probe_state(root_id), host.probe_state(child_id)),
        ((1, target), (1, target))
    );
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
    assert!(pump_until(&el, || ticks.load(Ordering::SeqCst) > restarted_at));

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
    let root = RawRoot::new(true);
    let root_id = root.id();
    let (timer, _ticks) = counting_timer(5);
    timer.start();
    set_parent_by_id(timer.object_id(), Some(root_id)).unwrap();

    assert_eq!(
        root.with_data(|d| move_to_thread(d, another_thread(), here)),
        Err(MoveError::NotMovable)
    );

    assert_eq!(root.changes(), 0, "no ThreadChange before the refusal");
    assert_eq!(root.thread_id(), here);
    assert_eq!(query_object_thread(timer.object_id()), Some(here));
    assert!(timer.is_active());
}

/// Runs a one-shot callback from `event` while the registry holds this object's callback borrow.
struct BorrowedMid {
    data: ObjectData,
    thread_changes: u32,
    on_update: Option<Box<dyn FnOnce()>>,
}

impl QObject for BorrowedMid {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ObjectDataMut<'_> {
        ObjectDataMut::new(&mut self.data)
    }
    fn event(&mut self, event: &mut Event) -> bool {
        match event.kind {
            EventKind::ThreadChange => {
                self.thread_changes += 1;
                false
            }
            EventKind::UpdateRequest => {
                if let Some(run) = self.on_update.take() {
                    run();
                }
                true
            }
            _ => false,
        }
    }
}

thread_local! {
    static ROOT_FOR_SLOT: RefCell<Option<RawRoot>> = const { RefCell::new(None) };
    static OUTCOME_IN_SLOT: Cell<Option<Result<(), MoveError>>> = const { Cell::new(None) };
}

/// When a `Timer` descendant (or an ancestor between the root and the `Timer`) is currently
/// borrowed by a running callback, `with_object` cannot borrow it again. The deprecated
/// `move_to_thread` must refuse the move with `MoveError::Busy` before sending any event,
/// detaching any timer or changing any affinity or registration thread, rather than treating the
/// failed inspection borrow as permission to move.
#[test]
#[allow(deprecated)]
fn the_deprecated_move_refuses_when_a_timer_descendant_is_in_its_callback() {
    let here = ThreadId::current();
    let el = EventLoop::new();
    let watcher = Watcher::spawn();
    let watcher_tid = watcher.tid;

    let root = RawRoot::new(true);
    let root_id = root.id();
    let timer = Timer::new();
    timer.set_interval(5);
    let timer_id = timer.object_id();
    set_parent_by_id(timer_id, Some(root_id)).unwrap();
    ROOT_FOR_SLOT.with(|slot| *slot.borrow_mut() = Some(root));
    OUTCOME_IN_SLOT.with(|out| out.set(None));

    let ticks = Arc::new(AtomicU32::new(0));
    let ticks_in_slot = Arc::clone(&ticks);
    timer.timeout.connect(move |()| {
        OUTCOME_IN_SLOT.with(|out| {
            if out.get().is_none() {
                ROOT_FOR_SLOT.with(|slot| {
                    let borrow = slot.borrow();
                    let root = borrow.as_ref().expect("root stays in thread-local");
                    let res = root.with_data(|d| move_to_thread(d, watcher_tid, here));
                    out.set(Some(res));
                });
            }
        });
        ticks_in_slot.fetch_add(1, Ordering::SeqCst);
    });

    let armed = timer.start();
    assert!(armed.is_valid());
    let scheduled_here = el.timer_registry().lock().unwrap().len();

    assert!(
        pump_until(&el, || OUTCOME_IN_SLOT.with(|out| out.get().is_some())),
        "the timer must fire its first callback"
    );
    assert_eq!(
        OUTCOME_IN_SLOT.with(|out| out.get()),
        Some(Err(MoveError::Busy)),
        "a timer child in its callback borrow must refuse the move"
    );
    assert!(
        pump_until(&el, || ticks.load(Ordering::SeqCst) >= 2),
        "the timer must keep ticking on its creating thread"
    );
    let root = ROOT_FOR_SLOT
        .with(|slot| slot.borrow_mut().take())
        .expect("root must still be present");
    assert_eq!(
        root.changes(),
        0,
        "no ThreadChange may be sent before the refusal"
    );
    assert_eq!(root.thread_id(), here);
    assert_eq!(query_object_thread(root_id), Some(here));
    assert_eq!(query_object_thread(timer_id), Some(here));
    assert_eq!(with_object(timer_id, |obj| obj.thread()), Some(here));
    assert_eq!(timer.timer_id(), armed);
    assert!(timer.is_active());
    assert_eq!(el.timer_registry().lock().unwrap().len(), scheduled_here);
    assert_eq!(
        watcher.timers_after_pumping(),
        0,
        "nothing may be re-armed on the target thread"
    );
    // Also cover a Timer deeper in the subtree when an intermediate ancestor holds a callback borrow.
    let target = another_thread();
    let root2 = Rc::new(RawRoot::new(true));
    let root2_id = root2.id();
    let mid_outcome: Rc<Cell<Option<Result<(), MoveError>>>> = Rc::new(Cell::new(None));
    let mid_outcome_in_cb = Rc::clone(&mid_outcome);
    let root2_in_cb = Rc::clone(&root2);
    // SAFETY: `mid` stays on this thread and is only touched through `OwnedObject` after the
    // callback returns.
    let mid = unsafe {
        register_owned_qobject(BorrowedMid {
            data: ObjectData::new(ObjectId::next()),
            thread_changes: 0,
            on_update: Some(Box::new(move || {
                let res = root2_in_cb.with_data(|d| move_to_thread(d, target, here));
                mid_outcome_in_cb.set(Some(res));
            })),
        })
    };
    let mid_id = OwnedObject::object_id(&mid);
    let (nested_timer, _nested_ticks) = counting_timer(5);
    nested_timer.start();
    set_parent_by_id(mid_id, Some(root2_id)).unwrap();
    set_parent_by_id(nested_timer.object_id(), Some(mid_id)).unwrap();

    assert!(dispatch_to_object(
        mid_id,
        &mut Event::new(EventKind::UpdateRequest)
    ));
    assert_eq!(mid_outcome.get(), Some(Err(MoveError::Busy)));
    assert_eq!(root2.changes(), 0);
    assert_eq!(mid.with(|m| m.thread_changes).unwrap(), 0);
    assert_eq!(root2.thread_id(), here);
    assert_eq!(query_object_thread(root2_id), Some(here));
    assert_eq!(query_object_thread(mid_id), Some(here));
    assert_eq!(query_object_thread(nested_timer.object_id()), Some(here));
    assert!(nested_timer.is_active());
}

/// Unregistered owned descendants (for which `with_object` returns `None` because they are not in
/// the global registry) keep working with `move_to_thread` when they are movable, while still
/// refusing when an unregistered owned descendant or a child below it cannot move.
#[test]
#[allow(deprecated)]
fn the_deprecated_move_keeps_unregistered_owned_descendants_working() {
    let here = ThreadId::current();
    let target = another_thread();

    let root = RawRoot::new(true);
    let root_id = root.id();
    let child = Box::new(Probe {
        data: probe_data(true),
        thread_changes: 0,
    });
    let child_id = child.data.id();
    // SAFETY: `root` owns `child` at a stable heap address on this thread.
    unsafe { add_owned_child(root_id, child) }.unwrap_or_else(|(e, _)| panic!("{e:?}"));
    let grand = Box::new(Probe {
        data: probe_data(true),
        thread_changes: 0,
    });
    let grand_id = grand.data.id();
    // SAFETY: `child` owns `grand` at a stable heap address on this thread.
    unsafe { add_owned_child(child_id, grand) }.unwrap_or_else(|(e, _)| panic!("{e:?}"));
    // SAFETY: no callback is running; unregister `root`, `child` and `grand` so the successful
    // deprecated `move_to_thread(&mut root.data, ...)` does not also deliver `ThreadChange` to
    // `root_id` through the registry while `&mut root.data` is held.
    unsafe {
        unregister_qobject(grand_id);
        unregister_qobject(child_id);
        unregister_qobject(root_id);
    }
    assert!(with_object(child_id, |_| ()).is_none());
    assert!(with_object(grand_id, |_| ()).is_none());

    assert_eq!(root.with_data(|d| move_to_thread(d, target, here)), Ok(()));
    assert_eq!(root.thread_id(), target);
    let (child_thread, grand_thread) = root.with_data(|d| {
        d.owned_children()[0]
            .with(|c| {
                (
                    c.object_data().thread_id(),
                    c.object_data().owned_children()[0]
                        .with(|g| g.object_data().thread_id())
                        .unwrap(),
                )
            })
            .unwrap()
    });
    assert_eq!(child_thread, target);
    assert_eq!(grand_thread, target);
    // An unregistered owned descendant that did not opt in still refuses.
    let root_pinned = RawRoot::new(true);
    let root_pinned_id = root_pinned.id();
    let pinned_child = Box::new(Probe {
        data: probe_data(false),
        thread_changes: 0,
    });
    let pinned_child_id = pinned_child.data.id();
    // SAFETY: `root_pinned` owns `pinned_child` on this thread.
    unsafe { add_owned_child(root_pinned_id, pinned_child) }
        .unwrap_or_else(|(e, _)| panic!("{e:?}"));
    // SAFETY: no callback runs on `pinned_child`.
    unsafe { unregister_qobject(pinned_child_id) };
    assert!(with_object(pinned_child_id, |_| ()).is_none());

    assert_eq!(
        root_pinned.with_data(|d| move_to_thread(d, target, here)),
        Err(MoveError::NotMovable)
    );
    assert_eq!(root_pinned.changes(), 0);
    assert_eq!(root_pinned.thread_id(), here);
    assert_eq!(
        root_pinned.with_data(|d| d.owned_children()[0]
            .with(|c| c.object_data().thread_id())
            .unwrap()),
        here
    );

    // A Timer attached below an owned child that is later unregistered is still reached through
    // `owned_children` and refuses the move.
    let root_with_timer = RawRoot::new(true);
    let root_with_timer_id = root_with_timer.id();
    let mid_child = Box::new(Probe {
        data: probe_data(true),
        thread_changes: 0,
    });
    let mid_child_id = mid_child.data.id();
    // SAFETY: `root_with_timer` owns `mid_child` on this thread.
    unsafe { add_owned_child(root_with_timer_id, mid_child) }
        .unwrap_or_else(|(e, _)| panic!("{e:?}"));
    let timer = Timer::new();
    set_parent_by_id(timer.object_id(), Some(mid_child_id)).unwrap();
    // SAFETY: no callback runs on `mid_child`.
    unsafe { unregister_qobject(mid_child_id) };
    assert!(with_object(mid_child_id, |_| ()).is_none());

    assert_eq!(
        root_with_timer.with_data(|d| move_to_thread(d, target, here)),
        Err(MoveError::NotMovable)
    );
    assert_eq!(root_with_timer.changes(), 0);
    assert_eq!(root_with_timer.thread_id(), here);
    assert_eq!(
        root_with_timer.with_data(|d| d.owned_children()[0]
            .with(|c| c.object_data().thread_id())
            .unwrap()),
        here
    );
    assert_eq!(query_object_thread(timer.object_id()), Some(here));
}

/// Runs a callback when it receives `EventKind::ChildRemoved`. During a child's `ObjectData::drop`,
/// `ChildRemoved` is delivered after the child's `liveness` token is set to `false` and before
/// `unregister_qobject` removes the child's record from the global registry.
struct OnChildRemoved {
    data: ObjectData,
    on_removed: Option<Box<dyn FnOnce()>>,
}

impl QObject for OnChildRemoved {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ObjectDataMut<'_> {
        ObjectDataMut::new(&mut self.data)
    }
    fn event(&mut self, event: &mut Event) -> bool {
        if matches!(event.kind, EventKind::ChildRemoved { .. }) {
            if let Some(run) = self.on_removed.take() {
                run();
            }
            return true;
        }
        false
    }
}

/// An intermediate node in the subtree that has no live registry record (either because it was
/// never registered, or because its `liveness` token is already `false` while its registry record
/// still lists its children) must still have its registered descendants inspected by both
/// `move_to_thread_by_id` and the deprecated `move_to_thread`.
#[test]
#[allow(deprecated)]
fn an_intermediate_node_without_a_live_record_still_exposes_its_registered_descendants() {
    let here = ThreadId::current();
    let el = EventLoop::new();
    let watcher = Watcher::spawn();
    let watcher_tid = watcher.tid;

    // 1. Intermediate node was never registered, and a registered holder + Timer sit below it.
    let root = RawRoot::new(true);
    let root_id = root.id();
    let mut mid = ObjectData::new(ObjectId::next());
    let mid_id = mid.id();
    root.with_data(|d| reparent(&mut mid, None, Some(d)));
    assert!(with_object(mid_id, |_| ()).is_none());

    let mut holder_raw = Probe {
        data: probe_data(true),
        thread_changes: 0,
    };
    set_parent(&mut holder_raw.data, Some(mid_id)).unwrap();
    // SAFETY: `holder` stays on this thread and is only used between callbacks.
    let holder = unsafe { register_owned_qobject(holder_raw) };
    let holder_id = OwnedObject::object_id(&holder);

    let (timer, ticks) = counting_timer(5);
    let armed = timer.start();
    assert!(armed.is_valid());
    let timer_id = timer.object_id();
    set_parent_by_id(timer_id, Some(holder_id)).unwrap();
    let scheduled_here = el.timer_registry().lock().unwrap().len();

    assert_eq!(
        move_to_thread_by_id(root_id, watcher_tid, here),
        Err(MoveByIdError::NotMovable)
    );
    assert_eq!(
        root.with_data(|d| move_to_thread(d, watcher_tid, here)),
        Err(MoveError::NotMovable)
    );
    assert_eq!(root.changes(), 0);
    assert_eq!(holder.with(|h| h.thread_changes).unwrap(), 0);
    assert_eq!(root.thread_id(), here);
    assert_eq!(holder.with(|h| h.data.thread_id()).unwrap(), here);
    assert_eq!(query_object_thread(root_id), Some(here));
    assert_eq!(query_object_thread(holder_id), Some(here));
    assert_eq!(query_object_thread(timer_id), Some(here));
    assert_eq!(timer.timer_id(), armed);
    assert!(timer.is_active());
    assert_eq!(el.timer_registry().lock().unwrap().len(), scheduled_here);
    assert_eq!(watcher.timers_after_pumping(), 0);
    assert!(
        pump_until(&el, || ticks.load(Ordering::SeqCst) >= 2),
        "the timer below the unregistered intermediate node must keep ticking here"
    );

    // 2. When the registered descendant below the unregistered intermediate node is in a callback
    //    borrow, both APIs refuse with `Busy` before any event or state change.
    let target = another_thread();
    let root_busy = Rc::new(RawRoot::new(true));
    let root_busy_id = root_busy.id();
    let mut mid_busy = ObjectData::new(ObjectId::next());
    let mid_busy_id = mid_busy.id();
    root_busy.with_data(|d| reparent(&mut mid_busy, None, Some(d)));

    let by_id_res: Rc<Cell<Option<Result<(), MoveByIdError>>>> = Rc::new(Cell::new(None));
    let by_ref_res: Rc<Cell<Option<Result<(), MoveError>>>> = Rc::new(Cell::new(None));
    let by_id_in_cb = Rc::clone(&by_id_res);
    let by_ref_in_cb = Rc::clone(&by_ref_res);
    let root_in_cb = Rc::clone(&root_busy);
    let mut busy_child_raw = BorrowedMid {
        data: ObjectData::new(ObjectId::next()),
        thread_changes: 0,
        on_update: Some(Box::new(move || {
            by_id_in_cb.set(Some(move_to_thread_by_id(root_busy_id, target, here)));
            by_ref_in_cb.set(Some(
                root_in_cb.with_data(|d| move_to_thread(d, target, here)),
            ));
        })),
    };
    set_parent(&mut busy_child_raw.data, Some(mid_busy_id)).unwrap();
    // SAFETY: `busy_child` stays on this thread and is only touched after its callback returns.
    let busy_child = unsafe { register_owned_qobject(busy_child_raw) };
    let busy_child_id = OwnedObject::object_id(&busy_child);
    let (timer_busy, _ticks_busy) = counting_timer(5);
    timer_busy.start();
    set_parent_by_id(timer_busy.object_id(), Some(busy_child_id)).unwrap();

    assert!(dispatch_to_object(
        busy_child_id,
        &mut Event::new(EventKind::UpdateRequest)
    ));
    assert_eq!(by_id_res.get(), Some(Err(MoveByIdError::Busy)));
    assert_eq!(by_ref_res.get(), Some(Err(MoveError::Busy)));
    assert_eq!(root_busy.changes(), 0);
    assert_eq!(busy_child.with(|b| b.thread_changes).unwrap(), 0);
    assert_eq!(query_object_thread(root_busy_id), Some(here));
    assert_eq!(query_object_thread(busy_child_id), Some(here));
    assert_eq!(query_object_thread(timer_busy.object_id()), Some(here));

    // 3. Intermediate node HAS a registry record whose `liveness` is `false` (observed during its
    //    `Drop` after `liveness` is cleared and before `unregister_qobject` removes the record),
    //    with a direct `Timer` child in `record.children` (`registered_children_of(dying_id)`).
    let root_dead = Rc::new(RawRoot::new(true));
    let root_dead_id = root_dead.id();
    let (timer_under_dying, _ticks_dying) = counting_timer(5);
    timer_under_dying.start();
    let timer_under_dying_id = timer_under_dying.object_id();
    let dead_by_id: Rc<Cell<Option<Result<(), MoveByIdError>>>> = Rc::new(Cell::new(None));
    let dead_by_ref: Rc<Cell<Option<Result<(), MoveError>>>> = Rc::new(Cell::new(None));
    let timer_thread_in_cb: Rc<Cell<Option<ThreadId>>> = Rc::new(Cell::new(None));
    let dead_by_id_cb = Rc::clone(&dead_by_id);
    let dead_by_ref_cb = Rc::clone(&dead_by_ref);
    let timer_thread_cb = Rc::clone(&timer_thread_in_cb);
    let root_dead_cb = Rc::clone(&root_dead);
    // SAFETY: `notifier` stays on this thread until the end of the block.
    let notifier = unsafe {
        register_owned_qobject(OnChildRemoved {
            data: ObjectData::new(ObjectId::next()),
            on_removed: Some(Box::new(move || {
                dead_by_id_cb.set(Some(move_to_thread_by_id(root_dead_id, target, here)));
                dead_by_ref_cb.set(Some(
                    root_dead_cb.with_data(|d| move_to_thread(d, target, here)),
                ));
                timer_thread_cb.set(query_object_thread(timer_under_dying_id));
            })),
        })
    };
    let mut dying_mid = ManuallyDrop::new(Probe {
        data: probe_data(true),
        thread_changes: 0,
    });
    let dying_mid_ptr: *mut Probe = (&raw mut dying_mid).cast::<Probe>();
    // SAFETY: `dying_mid` stays in `ManuallyDrop` on this stack frame and is not registered yet;
    // every access goes through `dying_mid_ptr`.
    let dying_mid_id = unsafe { (*dying_mid_ptr).data.id() };
    // List `dying_mid_id` in `root_dead.data.children` while leaving `dying_mid.data.parent` clear
    // before registration; then attach `dying_mid` to `notifier` so dropping `dying_mid` in place
    // fires `notifier`'s `ChildRemoved` while `dying_mid`'s record still exists in the registry
    // with `liveness == false` and `children == [timer]`.
    unsafe {
        root_dead.with_data(|d| reparent(&mut (*dying_mid_ptr).data, None, Some(d)));
        reparent(&mut (*dying_mid_ptr).data, None, None);
    }
    // SAFETY: `dying_mid` stays in place at `dying_mid_ptr` in `ManuallyDrop` until after
    // `ObjectData::drop` finishes `unregister_qobject(dying_mid_id)` inside `drop_in_place` below,
    // and every owner access goes through `dying_mid_ptr`.
    unsafe {
        register_qobject_ptr(NonNull::new_unchecked(dying_mid_ptr as *mut dyn QObject));
    }
    set_parent_by_id(dying_mid_id, Some(OwnedObject::object_id(&notifier))).unwrap();
    set_parent_by_id(timer_under_dying_id, Some(dying_mid_id)).unwrap();
    assert_eq!(
        with_object(dying_mid_id, |obj| obj.object_data().can_move_to_thread()),
        Some(true)
    );
    // SAFETY: `dying_mid_ptr` is the registered pointer, no callback on `dying_mid` is running,
    // `ObjectData::drop` unregisters `dying_mid_id` before returning while the object remains at
    // `dying_mid_ptr`, and `ManuallyDrop` prevents a second drop when `dying_mid` leaves scope.
    unsafe {
        std::ptr::drop_in_place(dying_mid_ptr);
    }

    assert_eq!(query_object_thread(dying_mid_id), None);
    assert_eq!(dead_by_id.get(), Some(Err(MoveByIdError::NotMovable)));
    assert_eq!(dead_by_ref.get(), Some(Err(MoveError::NotMovable)));
    assert_eq!(root_dead.changes(), 0);
    assert_eq!(query_object_thread(root_dead_id), Some(here));
    assert_eq!(timer_thread_in_cb.get(), Some(here));

    // 4. A movable subtree with an unregistered intermediate node and a movable registered
    //    descendant below it still moves as a whole.
    let root_ok = RawRoot::new(true);
    let root_ok_id = root_ok.id();
    let mut mid_ok = ObjectData::new(ObjectId::next());
    let mid_ok_id = mid_ok.id();
    root_ok.with_data(|d| reparent(&mut mid_ok, None, Some(d)));
    let mut leaf_ok_raw = Probe {
        data: probe_data(true),
        thread_changes: 0,
    };
    set_parent(&mut leaf_ok_raw.data, Some(mid_ok_id)).unwrap();
    // SAFETY: `leaf_ok` stays on this thread and is unregistered below.
    let leaf_ok = unsafe { register_owned_qobject(leaf_ok_raw) };
    let leaf_ok_id = OwnedObject::object_id(&leaf_ok);

    let host_ok = Host::spawn();
    assert_eq!(move_to_thread_by_id(root_ok_id, host_ok.tid, here), Ok(()));
    assert_eq!(
        (root_ok.changes(), host_ok.probe_state(leaf_ok_id)),
        (1, (1, host_ok.tid))
    );
    assert_eq!(query_object_thread(root_ok_id), Some(host_ok.tid));
    assert_eq!(query_object_thread(leaf_ok_id), Some(host_ok.tid));
    // SAFETY: no callback of `root_ok` or `leaf_ok` runs.
    unsafe {
        unregister_qobject(leaf_ok_id);
        unregister_qobject(root_ok_id);
    }
}
