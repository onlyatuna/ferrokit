// SPDX-License-Identifier: LGPL-3.0-only
//! `move_to_thread_by_id` moves the registration of an object to another thread, but its
//! `OwnedObject` is `!Send` and stays on the thread it was made on. Dropping that owner then runs
//! on a thread that is not the registration thread, while callbacks of the object run on the
//! registration thread.
//!
//! Scenarios, each expecting the behaviour the owner's documentation promises (a callback
//! never overlaps the destruction of its object; the destruction happens once the callback
//! returned; the timers of a destroyed object are gone). They are meant to fail on a library that
//! does not synchronise the owner's drop with the registration thread.
//!
//! * `the_object_is_freed_when_its_callback_on_the_other_thread_returns`: deterministic. Another
//!   thread is inside a callback when the owner is dropped; the owner's drop is forwarded to that
//!   thread, and the object is freed once the callback returned.
//! * `the_owner_drop_does_not_overlap_a_callback_that_starts_during_it`: a race. The owner's drop
//!   is always forwarded (its thread is not the registration thread), so the callback never meets
//!   the destruction. It reproduced a use after free on a library that destroyed the object on the
//!   owner's thread after only loading the borrow flag (run it under Miri with
//!   `-Zmiri-ignore-leaks`, several seeds, to see that). The race on one thread is covered by the
//!   last two tests of this file.
//! * `the_timers_of_the_object_are_removed_when_the_owner_is_dropped`: deterministic. The object's
//!   timer lives in the registry of the thread it was moved to, but the drop clears the registry
//!   of the dropping thread.
//! * `an_owner_dropped_by_a_callback_on_the_registration_thread_parks_until_the_callback_returns`
//!   and `owner_drops_on_the_registration_thread_destroy_each_object_once_in_or_out_of_a_callback`:
//!   deterministic. Owner and callback on one thread, where the borrow flag decides the order.

use std::hint::black_box;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ferrokit_core::event_loop::EventLoop;
use ferrokit_core::object::{
    move_to_thread_by_id, query_object_thread, register_owned_qobject, with_object_mut, ObjectData,
    ObjectDataMut, ObjectId, OwnedObject, QObject, ThreadId,
};
use ferrokit_core::timer::{start_object_timer, with_thread_timer_context, TimerType};

const WAIT: Duration = Duration::from_secs(10);

struct Probe {
    data: ObjectData,
    hits: u32,
    dropped: Arc<AtomicBool>,
}

impl Probe {
    fn new(dropped: &Arc<AtomicBool>) -> Self {
        Probe {
            data: ObjectData::new(ObjectId::next()),
            hits: 0,
            dropped: Arc::clone(dropped),
        }
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
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
    fn as_qobject_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

fn hits_of(obj: &mut dyn QObject) -> &mut u32 {
    &mut obj
        .as_qobject_any_mut()
        .and_then(|any| any.downcast_mut::<Probe>())
        .expect("a Probe")
        .hits
}

fn spin_until(flag: &AtomicBool) {
    let start = Instant::now();
    let mut n = 0u32;
    while !flag.load(Ordering::Acquire) {
        n += 1;
        if n % 4096 == 0 {
            assert!(start.elapsed() < WAIT, "timed out waiting for a flag");
        }
        std::hint::spin_loop();
    }
}

/// Another thread is inside a callback of the object when the owner is dropped. The owner sees
/// the borrow flag set, so it unregisters the object and parks the allocation until that callback
/// returns. The callback's guard frees what is parked on *its* thread; the allocation was parked on
/// the owner's thread, so it is never freed and `Probe::drop` never runs.
#[test]
fn the_object_is_freed_when_its_callback_on_the_other_thread_returns() {
    let here = ThreadId::current();
    let _el = EventLoop::new();
    let dropped = Arc::new(AtomicBool::new(false));
    // SAFETY: the owner is dropped by this thread only, and no callback of the object runs here.
    let owner = unsafe { register_owned_qobject(Probe::new(&dropped)) };
    let id = OwnedObject::object_id(&owner);

    let (tid_tx, tid_rx) = channel();
    let (moved_tx, moved_rx) = channel::<()>();
    let (inside_tx, inside_rx) = channel::<()>();
    let (go_tx, go_rx) = channel::<()>();
    let worker = std::thread::spawn(move || {
        let _el = EventLoop::new();
        tid_tx.send(ThreadId::current()).unwrap();
        moved_rx.recv_timeout(WAIT).unwrap();
        with_object_mut(id, |obj| {
            *hits_of(obj) += 1;
            inside_tx.send(()).unwrap();
            go_rx.recv_timeout(WAIT).unwrap();
        })
        .expect("the callback runs on the registration thread");
    });
    let there = tid_rx.recv_timeout(WAIT).unwrap();
    assert_eq!(move_to_thread_by_id(id, there, here), Ok(()));
    moved_tx.send(()).unwrap();
    inside_rx.recv_timeout(WAIT).unwrap();

    // The owner is dropped on its own thread while the other thread runs a callback.
    drop(owner);
    go_tx.send(()).unwrap();
    worker.join().unwrap();

    assert!(
        dropped.load(Ordering::SeqCst),
        "the object was never destroyed: its allocation was parked on the thread of the owner, \
         and the callback that had to free it ended on another thread"
    );
}

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    /// The callback started after the registry had dropped the object.
    Missed,
    /// The owner saw the callback's borrow and parked the allocation.
    Parked,
    /// `Probe::drop` ran, and the memory went, while the callback was still inside the object.
    DestroyedDuringCallback,
}

struct Job {
    id: ObjectId,
    ready: Arc<AtomicBool>,
    drop_started: Arc<AtomicBool>,
    drop_done: Arc<AtomicBool>,
    dropped: Arc<AtomicBool>,
}

fn run_job(job: Job) -> Outcome {
    job.ready.store(true, Ordering::Release);
    spin_until(&job.drop_started);
    let seen = with_object_mut(job.id, |obj| {
        if cfg!(miri) {
            // Under Miri the object is touched again after the owner finished its drop, which is
            // a use after free in the interleaving this test looks for. Natively that write would
            // corrupt the heap, so only the owner's `Probe::drop` is observed.
            *hits_of(obj) += 1;
        }
        spin_until(&job.drop_done);
        if cfg!(miri) {
            *hits_of(obj) += 1;
        }
        job.dropped.load(Ordering::SeqCst)
    });
    match seen {
        None => Outcome::Missed,
        Some(false) => Outcome::Parked,
        Some(true) => Outcome::DestroyedDuringCallback,
    }
}

fn worker_loop(jobs: Receiver<Job>, outcomes: Sender<Outcome>, tid: Sender<ThreadId>) {
    let _el = EventLoop::new();
    tid.send(ThreadId::current()).unwrap();
    while let Ok(job) = jobs.recv() {
        outcomes.send(run_job(job)).unwrap();
    }
}

/// The owner is dropped while the registration thread starts a callback on the object. The drop
/// is forwarded to the registration thread, which runs it only between callbacks (here when the
/// worker's loop ends), so no callback meets the destruction. A destruction that only loaded the
/// borrow flag on the owner's thread would let a callback that takes the flag after that load run
/// on an object the owner then destroys.
#[test]
fn the_owner_drop_does_not_overlap_a_callback_that_starts_during_it() {
    let iterations: u32 = if cfg!(miri) { 40 } else { 20_000 };
    let here = ThreadId::current();
    let _el = EventLoop::new();

    let (job_tx, job_rx) = channel();
    let (outcome_tx, outcome_rx) = channel();
    let (tid_tx, tid_rx) = channel();
    let worker = std::thread::spawn(move || worker_loop(job_rx, outcome_tx, tid_tx));
    let there = tid_rx.recv_timeout(WAIT).unwrap();

    let (mut missed, mut parked, mut destroyed) = (0u32, 0u32, 0u32);
    for i in 0..iterations {
        let dropped = Arc::new(AtomicBool::new(false));
        // SAFETY: as above.
        let owner = unsafe { register_owned_qobject(Probe::new(&dropped)) };
        let id = OwnedObject::object_id(&owner);
        assert_eq!(move_to_thread_by_id(id, there, here), Ok(()));

        let ready = Arc::new(AtomicBool::new(false));
        let drop_started = Arc::new(AtomicBool::new(false));
        let drop_done = Arc::new(AtomicBool::new(false));
        job_tx
            .send(Job {
                id,
                ready: Arc::clone(&ready),
                drop_started: Arc::clone(&drop_started),
                drop_done: Arc::clone(&drop_done),
                dropped: Arc::clone(&dropped),
            })
            .unwrap();
        spin_until(&ready);

        // Sweeping the owner's delay makes the worker's callback start before, during and after
        // the owner's drop.
        let owner_spin = ((i as u64 * 37) % 6000) as u32;
        drop_started.store(true, Ordering::Release);
        for k in 0..owner_spin {
            black_box(k);
        }
        drop(owner);
        drop_done.store(true, Ordering::Release);

        match outcome_rx.recv_timeout(WAIT).unwrap() {
            Outcome::Missed => missed += 1,
            Outcome::Parked => parked += 1,
            Outcome::DestroyedDuringCallback => destroyed += 1,
        }
    }
    drop(job_tx);
    worker.join().unwrap();

    eprintln!("iterations={iterations} missed={missed} parked={parked} destroyed_during_callback={destroyed}");
    assert_eq!(
        destroyed, 0,
        "the owner destroyed the object while a callback of it was running on the registration \
         thread ({destroyed} of {iterations} rounds)"
    );
}

fn timers_here() -> usize {
    with_thread_timer_context(|ctx| ctx.registry.lock().unwrap().len())
        .expect("this thread has a loop")
}

/// A timer of an object follows it to the thread it is moved to. Dropping the owner then clears
/// the timer registry of the owner's thread, and the timer stays in the registry it lives in.
#[test]
fn the_timers_of_the_object_are_removed_when_the_owner_is_dropped() {
    let here = ThreadId::current();
    let _el = EventLoop::new();
    let dropped = Arc::new(AtomicBool::new(false));
    // SAFETY: the owner is dropped by this thread only, and no callback of the object runs here.
    let owner = unsafe { register_owned_qobject(Probe::new(&dropped)) };
    let id = OwnedObject::object_id(&owner);
    let base_here = timers_here();
    assert!(start_object_timer(id, 60_000, TimerType::Coarse).is_valid());
    assert_eq!(
        timers_here(),
        base_here + 1,
        "the timer starts in this thread's registry"
    );

    let (tid_tx, tid_rx) = channel();
    let (moved_tx, moved_rx) = channel::<()>();
    let (adopted_tx, adopted_rx) = channel::<usize>();
    let (dropped_tx, dropped_rx) = channel::<()>();
    let (left_tx, left_rx) = channel::<usize>();
    let worker = std::thread::spawn(move || {
        let el = EventLoop::new();
        let base = timers_here();
        tid_tx.send(ThreadId::current()).unwrap();
        moved_rx.recv_timeout(WAIT).unwrap();
        let start = Instant::now();
        while timers_here() == base && start.elapsed() < WAIT {
            el.process_events(false);
            std::thread::sleep(Duration::from_millis(1));
        }
        adopted_tx.send(timers_here() - base).unwrap();
        dropped_rx.recv_timeout(WAIT).unwrap();
        for _ in 0..20 {
            el.process_events(false);
            std::thread::sleep(Duration::from_millis(1));
        }
        left_tx.send(timers_here() - base).unwrap();
    });
    let there = tid_rx.recv_timeout(WAIT).unwrap();
    assert_eq!(move_to_thread_by_id(id, there, here), Ok(()));
    assert_eq!(
        timers_here(),
        base_here,
        "the move took the timer out of this registry"
    );
    moved_tx.send(()).unwrap();
    assert_eq!(
        adopted_rx.recv_timeout(WAIT).unwrap(),
        1,
        "the timer was re-registered on the thread the object moved to"
    );

    drop(owner);
    dropped_tx.send(()).unwrap();
    let left = left_rx.recv_timeout(WAIT).unwrap();
    worker.join().unwrap();

    assert!(dropped.load(Ordering::SeqCst), "the object was destroyed");
    assert_eq!(
        left, 0,
        "the destroyed object's timer is still registered on the thread it was moved to: the \
         owner's drop stopped timers in its own thread's registry"
    );
}

/// The owner and a callback of its object are on the same thread (the registration thread): the
/// callback drops the owner. Nothing is forwarded; the owner finds the borrow flag held, so it
/// unregisters the object at once and parks the allocation, and the callback's guard frees it
/// when the callback returns. The callback can still use the object after the drop.
#[test]
fn an_owner_dropped_by_a_callback_on_the_registration_thread_parks_until_the_callback_returns() {
    let _el = EventLoop::new();
    let dropped = Arc::new(AtomicBool::new(false));
    // SAFETY: the owner is dropped on this thread only, by the callback below.
    let owner = unsafe { register_owned_qobject(Probe::new(&dropped)) };
    let id = OwnedObject::object_id(&owner);

    let (unregistered_inside, dropped_inside, hits) = with_object_mut(id, |obj| {
        *hits_of(obj) += 1;
        drop(owner);
        let unregistered_inside = query_object_thread(id).is_none();
        let dropped_inside = dropped.load(Ordering::SeqCst);
        // The allocation is parked, not freed: the object is still usable here (Miri reports a
        // use after free otherwise).
        *hits_of(obj) += 1;
        (unregistered_inside, dropped_inside, *hits_of(obj))
    })
    .expect("the callback runs on the registration thread");

    assert!(
        unregistered_inside,
        "the owner's drop unregisters the object at once"
    );
    assert!(
        !dropped_inside,
        "the object must not be destroyed while its callback runs"
    );
    assert_eq!(hits, 2);
    assert!(
        dropped.load(Ordering::SeqCst),
        "the callback's guard frees the parked allocation when the callback returns"
    );
}

/// Owner drops on the registration thread, with no callback running, take the borrow flag with the
/// compare-and-swap and free the object at once; interleaved with drops from inside a callback
/// they still destroy every object exactly once.
#[test]
fn owner_drops_on_the_registration_thread_destroy_each_object_once_in_or_out_of_a_callback() {
    let _el = EventLoop::new();
    for i in 0..200u32 {
        let dropped = Arc::new(AtomicBool::new(false));
        // SAFETY: as above.
        let owner = unsafe { register_owned_qobject(Probe::new(&dropped)) };
        let id = OwnedObject::object_id(&owner);
        if i % 2 == 0 {
            drop(owner);
            assert!(
                dropped.load(Ordering::SeqCst),
                "no callback runs: freed at once"
            );
        } else {
            let dropped_inside = with_object_mut(id, |_| {
                drop(owner);
                dropped.load(Ordering::SeqCst)
            })
            .unwrap();
            assert!(!dropped_inside, "a callback runs: the free waits for it");
            assert!(
                dropped.load(Ordering::SeqCst),
                "and happens when it returns"
            );
        }
        assert_eq!(query_object_thread(id), None);
    }
}
