// SPDX-License-Identifier: LGPL-3.0-only
//! Event loop and timer fixes from the loop/timer review: killed zero timers, quit semantics,
//! panic containment in timer delivery, ordered expiry, single-shot callbacks behind a filter,
//! and the cost of draining a large posted-event queue.

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::event_loop::{
    install_application_event_filter, remove_application_event_filter, EventLoop,
    GenericEventDispatcher, UnixEventDispatcher,
};
use ferrokit_core::object::{
    register_owned_qobject, ObjectData, ObjectDataMut, ObjectId, OwnedObject, QObject,
};
use ferrokit_core::timer::{
    current_time_ms, kill_object_timer, start_object_timer, Timer, TimerRegistry, TimerType,
};
use ferrokit_core::EventDispatcher;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct Ticker {
    data: ObjectData,
    ticks: Arc<AtomicUsize>,
    panic_on_tick: bool,
}

impl QObject for Ticker {
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
    fn timer_event(&mut self, _timer_id: u64) {
        self.ticks.fetch_add(1, Ordering::SeqCst);
        if self.panic_on_tick {
            panic!("timer slot failure (expected by the test)");
        }
    }
}

fn ticker(panic_on_tick: bool) -> (OwnedObject<Ticker>, ObjectId, Arc<AtomicUsize>) {
    let ticks = Arc::new(AtomicUsize::new(0));
    let data = ObjectData::new(ObjectId::next());
    let id = data.id();
    // SAFETY: the owner stays on this thread and nothing touches the object while a callback of
    // it runs.
    let owner = unsafe {
        register_owned_qobject(Ticker {
            data,
            ticks: Arc::clone(&ticks),
            panic_on_tick,
        })
    };
    (owner, id, ticks)
}

fn pump_until(el: &EventLoop, what: &str, done: impl Fn() -> bool) {
    let end = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        el.process_events(false);
        std::thread::yield_now();
    }
}

/// Qt sends a zero timer only while `timerDict` has it, and `unregisterTimer` removes the pending
/// ones: a timer killed before its queued `ZeroTimer` runs gets no `timer_event`.
#[test]
fn killed_zero_timer_gets_no_tick() {
    let el = EventLoop::new();
    let (_owner, id, ticks) = ticker(false);
    let timer = start_object_timer(id, 0, TimerType::Coarse);
    assert!(timer.is_valid());
    assert!(kill_object_timer(timer));
    for _ in 0..3 {
        el.process_events(false);
    }
    assert_eq!(ticks.load(Ordering::SeqCst), 0);
}

/// A live zero timer still repeats, so the check above does not just drop every `ZeroTimer`.
#[test]
fn live_zero_timer_still_ticks_until_killed() {
    let el = EventLoop::new();
    let (_owner, id, ticks) = ticker(false);
    let timer = start_object_timer(id, 0, TimerType::Coarse);
    pump_until(&el, "two zero timer ticks", || ticks.load(Ordering::SeqCst) >= 2);
    assert!(kill_object_timer(timer));
    el.process_events(false);
    let seen = ticks.load(Ordering::SeqCst);
    el.process_events(false);
    el.process_events(false);
    assert_eq!(ticks.load(Ordering::SeqCst), seen);
}

/// A posted `Quit` for another object ends that loop (`QEventLoop::quit`, `QThread::exit`); it
/// is not `QCoreApplication::exit` and never emits `aboutToQuit`, which would also use up the
/// one emission of the real application quit.
#[test]
fn posted_quit_to_a_plain_receiver_does_not_emit_about_to_quit() {
    use ferrokit_core::application::CoreApplication;
    let app = CoreApplication::new(Vec::new());
    let emitted = Arc::new(AtomicUsize::new(0));
    let sink = Arc::clone(&emitted);
    app.about_to_quit().connect(move |_| {
        sink.fetch_add(1, Ordering::SeqCst);
    });
    let el = EventLoop::new();
    el.handle().post_quit(ObjectId(0), 5);
    assert!(el.process_events(false));
    assert!(el.is_exit_requested());
    assert_eq!(el.return_code(), 5);
    assert_eq!(emitted.load(Ordering::SeqCst), 0);
    drop(app);
}

fn due_registry(receiver: ObjectId, intervals: &[u64]) -> (Mutex<TimerRegistry>, Vec<ferrokit_core::timer::TimerId>) {
    let mut registry = TimerRegistry::new();
    let ids = intervals
        .iter()
        .map(|&interval| registry.register(receiver, interval, TimerType::Precise, false))
        .collect();
    (Mutex::new(registry), ids)
}

fn wait_until_due(registry: &Mutex<TimerRegistry>, count: usize) -> u64 {
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        let now = current_time_ms();
        if registry.lock().unwrap().expired_timers(now).len() >= count {
            return now;
        }
        assert!(Instant::now() < end, "timers never became due");
        std::thread::yield_now();
    }
}

/// Qt's `QTimerInfoList` is sorted by expiry: the 10 ms timer runs before the 1000 ms one even
/// when both are overdue.
#[test]
fn due_timers_come_in_order_of_expiry() {
    let (registry, ids) = due_registry(ObjectId::next(), &[1000, 10, 100]);
    let now = {
        // Make all three due at once, far past their deadlines.
        current_time_ms() + 5_000
    };
    let due = registry.lock().unwrap().expired_timers(now);
    assert_eq!(due, vec![ids[1], ids[2], ids[0]]);
}

/// A timer slot that panics does not leave the timer marked "in delivery" (it would never fire
/// again) and does not take the rest of the batch or the loop down with it. This is the Unix
/// dispatcher's `send_timer_events`, which Linux and macOS core loops use.
#[test]
fn panicking_timer_slot_is_contained_by_the_unix_dispatcher() {
    let (bad, bad_id, bad_ticks) = ticker(true);
    let (good, good_id, good_ticks) = ticker(false);
    let mut registry = TimerRegistry::new();
    let bad_timer = registry.register(bad_id, 1, TimerType::Precise, false);
    let good_timer = registry.register(good_id, 1, TimerType::Precise, false);
    let registry = Mutex::new(registry);
    wait_until_due(&registry, 2);

    let dispatcher = UnixEventDispatcher::new();
    let result = catch_unwind(AssertUnwindSafe(|| dispatcher.send_timer_events(&registry)));
    assert!(result.is_ok(), "the panic escaped from send_timer_events");
    assert_eq!(bad_ticks.load(Ordering::SeqCst), 1);
    assert_eq!(good_ticks.load(Ordering::SeqCst), 1, "the rest of the batch still ran");
    let reg = registry.lock().unwrap();
    assert!(!reg.get(bad_timer).unwrap().in_timer_event, "stuck in delivery");
    assert!(!reg.get(good_timer).unwrap().in_timer_event);
    drop((bad, good));
}

/// The fallback dispatcher delivers timers too (it used to ignore `send_timer_events`).
#[test]
fn generic_dispatcher_delivers_due_timers() {
    let (owner, id, ticks) = ticker(false);
    let (registry, _) = due_registry(id, &[1]);
    wait_until_due(&registry, 1);
    GenericEventDispatcher::new().send_timer_events(&registry);
    assert_eq!(ticks.load(Ordering::SeqCst), 1);
    drop(owner);
}

/// A thread parked in the fallback dispatcher is woken by another thread's `wake_up`, as it must
/// be on targets without a native dispatcher.
#[test]
fn generic_dispatcher_wake_up_unparks_the_loop_thread() {
    let dispatcher = GenericEventDispatcher::new();
    let handle = dispatcher.clone_handle();
    let waker = std::thread::spawn(move || handle.wake_up());
    // No timer, no timeout: only the wake-up can end this wait.
    let result = dispatcher.process_events(true, None);
    waker.join().unwrap();
    assert!(result.is_awoken(), "got {result:?}");
}

/// A `Timer::single_shot` callback still runs when an application event filter consumes the
/// tick of its (internal) receiver; it is not left behind in a thread-local map.
#[test]
fn single_shot_callback_runs_when_a_filter_consumes_the_tick() {
    struct ConsumeTimers {
        data: ObjectData,
        seen: Arc<AtomicUsize>,
    }
    impl QObject for ConsumeTimers {
        fn object_data(&self) -> &ObjectData {
            &self.data
        }
        fn object_data_mut(&mut self) -> ObjectDataMut<'_> {
            ObjectDataMut::new(&mut self.data)
        }
        fn event_filter(&mut self, _watched: ObjectId, event: &mut Event) -> bool {
            if matches!(event.kind, EventKind::Timer { .. }) {
                self.seen.fetch_add(1, Ordering::SeqCst);
                return true;
            }
            false
        }
    }

    let el = EventLoop::new();
    let seen = Arc::new(AtomicUsize::new(0));
    let data = ObjectData::new(ObjectId::next());
    let filter_id = data.id();
    // SAFETY: the owner stays on this thread and nothing touches the object during a callback.
    let _filter = unsafe {
        register_owned_qobject(ConsumeTimers {
            data,
            seen: Arc::clone(&seen),
        })
    };
    install_application_event_filter(filter_id);

    let ran = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&ran);
    Timer::single_shot(1, move || flag.store(true, Ordering::SeqCst));
    pump_until(&el, "the single-shot callback", || ran.load(Ordering::SeqCst));
    assert!(seen.load(Ordering::SeqCst) >= 1, "the filter never saw the tick");
    remove_application_event_filter(filter_id);
}

/// Draining a long queue moved the whole tail for every event (`Vec::remove(0)`), which is
/// quadratic: 100 000 queued calls took minutes. The test has no time bound; it finishes quickly
/// only when the drain is linear.
#[test]
fn draining_a_long_queue_is_linear() {
    const EVENTS: usize = 100_000;
    let el = EventLoop::new();
    let ran = Arc::new(AtomicUsize::new(0));
    for _ in 0..EVENTS {
        let counter = Arc::clone(&ran);
        el.post_event(
            ObjectId(0),
            Event::new(EventKind::MetaCallNoTarget(Box::new(move || {
                counter.fetch_add(1, Ordering::Relaxed);
            }))),
        );
    }
    assert_eq!(el.send_posted_events(), EVENTS);
    assert_eq!(ran.load(Ordering::Relaxed), EVENTS);
}

/// Destroying an object purges its posted events; the counts behind that must stay exact when
/// events leave the queue by delivery, purge and move.
#[test]
fn purging_a_receiver_removes_only_its_events() {
    let el = EventLoop::new();
    let (a_owner, a, a_calls) = ticker(false);
    let (b_owner, b, b_calls) = ticker(false);
    for receiver in [a, b, a, b, a] {
        let counter = Arc::clone(if receiver == a { &a_calls } else { &b_calls });
        el.post_event(
            receiver,
            Event::new(EventKind::MetaCallNoTarget(Box::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
            }))),
        );
    }
    drop(a_owner);
    el.send_posted_events();
    assert_eq!(a_calls.load(Ordering::SeqCst), 0, "a's calls were purged");
    assert_eq!(b_calls.load(Ordering::SeqCst), 2);
    drop(b_owner);
    assert!(el.queue().lock().unwrap().is_empty());
}
