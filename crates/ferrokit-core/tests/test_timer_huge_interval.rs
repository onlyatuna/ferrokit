// SPDX-License-Identifier: LGPL-3.0-only
//! A timer interval near `u64::MAX` is accepted through the safe API: its deadline saturates, the
//! timer never fires, and destroying objects afterwards still completes.
//!
//! Each test runs on its own thread, and a thread's timer registry belongs to its event loop, so
//! nothing here is shared with another test.

use ferrokit_core::event_loop::EventLoop;
use ferrokit_core::object::{register_owned_qobject, with_object, ObjectData, ObjectId, QObject};
use ferrokit_core::timer::{Timer, TimerType};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::Arc;

struct Node {
    data: ObjectData,
    dropped: Arc<AtomicUsize>,
}

impl Node {
    fn new(dropped: &Arc<AtomicUsize>) -> Self {
        Self {
            data: ObjectData::new(ObjectId::next()),
            dropped: Arc::clone(dropped),
        }
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        self.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

impl QObject for Node {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
    }
}

/// Number of timers registered on the loop's thread; fails if the registry lock is poisoned.
fn registry_len(el: &EventLoop) -> usize {
    match el.timer_registry().lock() {
        Ok(registry) => registry.len(),
        Err(_) => panic!("the timer registry lock is poisoned"),
    }
}

const HUGE_INTERVALS: [u64; 6] = [
    u64::MAX,
    u64::MAX - 1,
    u64::MAX - 499,
    u64::MAX - 500,
    u64::MAX - 501,
    i64::MAX as u64,
];

/// Starts a timer of `ty` with `interval_ms` and returns it, failing the test with a message when
/// starting it panics.
fn start_timer(ty: TimerType, interval_ms: u64, fired: &Arc<AtomicU32>) -> Timer {
    let timer = Timer::new();
    timer.set_timer_type(ty);
    let counter = Arc::clone(fired);
    timer.timeout.connect(move |()| {
        counter.fetch_add(1, Ordering::SeqCst);
    });
    let started = catch_unwind(AssertUnwindSafe(|| timer.start_with_interval(interval_ms)));
    if started.is_err() {
        // Dropping a timer whose start panicked could panic again while unwinding.
        std::mem::forget(timer);
        panic!("starting a {ty:?} timer with interval {interval_ms} panicked");
    }
    timer
}

#[test]
fn a_huge_interval_neither_panics_nor_poisons_and_later_destruction_completes() {
    let el = EventLoop::new();
    let fired = Arc::new(AtomicU32::new(0));
    let dropped = Arc::new(AtomicUsize::new(0));

    for ty in [TimerType::Precise, TimerType::Coarse, TimerType::VeryCoarse] {
        for interval in HUGE_INTERVALS {
            let timer = start_timer(ty, interval, &fired);
            assert!(timer.is_active(), "{ty:?} {interval}");
            let timer_id = timer.object_id();

            // Destroying another object after this start must run to the end and free it.
            // SAFETY: `owned` stays on this thread and is only dropped (line `drop(owned)` below),
            // which unregisters it. No event loop runs between registration and the drop, and
            // `Node` has no callbacks that touch itself, so no callback of it is active.
            let owned = unsafe { register_owned_qobject(Node::new(&dropped)) };
            let owned_id = owned.with(|o| o.object_data().id()).unwrap();
            let before = dropped.load(Ordering::SeqCst);
            let destroyed = catch_unwind(AssertUnwindSafe(|| drop(owned)));
            assert!(
                destroyed.is_ok(),
                "destroying an object panicked after a {ty:?} timer with interval {interval}"
            );
            assert_eq!(dropped.load(Ordering::SeqCst), before + 1, "object leaked");
            assert!(with_object(owned_id, |_| ()).is_none());

            // The timer itself stops and unregisters.
            let destroyed = catch_unwind(AssertUnwindSafe(|| drop(timer)));
            assert!(destroyed.is_ok(), "destroying the timer panicked");
            assert!(with_object(timer_id, |_| ()).is_none());
        }
    }

    assert_eq!(registry_len(&el), 0, "every timer was unregistered");
    assert_eq!(fired.load(Ordering::SeqCst), 0);
}

#[test]
fn a_huge_interval_timer_stays_pending_without_firing_or_a_negative_remaining_time() {
    let el = EventLoop::new();
    let fired = Arc::new(AtomicU32::new(0));

    for ty in [TimerType::Precise, TimerType::Coarse, TimerType::VeryCoarse] {
        let timer = start_timer(ty, u64::MAX, &fired);

        // The remaining time is huge and positive, never wrapped to a negative number.
        assert!(
            timer.remaining_time() > 0,
            "{ty:?}: remaining time was {}",
            timer.remaining_time()
        );

        // Running the loop waits for the next deadline: it must neither panic converting it to
        // a wait nor deliver the timer.
        for _ in 0..3 {
            el.process_events(false);
        }
        assert_eq!(fired.load(Ordering::SeqCst), 0, "{ty:?} fired early");
        assert!(timer.is_active());
        drop(timer);
    }
}
