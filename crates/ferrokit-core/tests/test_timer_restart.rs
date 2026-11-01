// SPDX-License-Identifier: LGPL-3.0-only
//! `QTimer::start()` on an already-running timer restarts it (Qt: `QTimer::start` stops the
//! running timer, then starts a new one). A restarted timer must still deliver `timeout`.
//!
//! Regression: restarting a `Timer` used to re-register the underlying object, which
//! marked the object's own liveness token dead, so the dispatcher refused to deliver the
//! `timer_event` and the restarted timer never fired.
#![cfg(windows)]

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::event_loop::EventLoop;
use ferrokit_core::object::{
    ObjectData, ObjectId, QObject, register_owned_qobject, unregister_qobject, with_object,
};
use ferrokit_core::timer::{Timer, TimerType};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

fn counting_timer(interval_ms: u64, single_shot: bool) -> (Timer, Arc<AtomicU32>) {
    let count = Arc::new(AtomicU32::new(0));
    let c = Arc::clone(&count);
    let timer = Timer::new();
    timer.set_interval(interval_ms);
    timer.set_single_shot(single_shot);
    timer.timeout.connect(move |_| {
        c.fetch_add(1, Ordering::SeqCst);
    });
    (timer, count)
}

/// Pumps the loop until `cond` or `limit` elapses; returns whether `cond` became true.
fn pump_until(el: &mut EventLoop, limit: Duration, cond: impl Fn() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < limit {
        el.process_events(false);
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    cond()
}

#[test]
fn restarted_single_shot_timer_still_fires() {
    let mut el = EventLoop::new();
    let (timer, count) = counting_timer(40, true);

    timer.start();
    timer.start();

    assert!(
        pump_until(&mut el, Duration::from_millis(500), || count
            .load(Ordering::SeqCst)
            >= 1),
        "a timer that was start()ed twice never fired"
    );
    assert_eq!(count.load(Ordering::SeqCst), 1, "single-shot fires once");
    assert!(!timer.is_active());
}

#[test]
fn stop_then_start_fires() {
    let mut el = EventLoop::new();
    let (timer, count) = counting_timer(40, true);

    timer.start();
    timer.stop();
    timer.start();

    assert!(
        pump_until(&mut el, Duration::from_millis(500), || count
            .load(Ordering::SeqCst)
            >= 1),
        "stop() followed by start() never fired"
    );
}

#[test]
fn restart_postpones_the_deadline() {
    // This is how the tooltip wake-up timer is used: every mouse move restarts it, and the
    // timeout must be measured from the *last* start.
    let mut el = EventLoop::new();
    let (timer, count) = counting_timer(120, true);

    timer.start();
    let first = Instant::now();
    assert!(!pump_until(&mut el, Duration::from_millis(70), || count
        .load(Ordering::SeqCst)
        >= 1));
    timer.start();
    let restarted = Instant::now();

    // Past the first deadline (120 ms after `first`) but before the restarted one.
    let before_restarted_deadline = pump_until(
        &mut el,
        (first + Duration::from_millis(150)).saturating_duration_since(Instant::now()),
        || count.load(Ordering::SeqCst) >= 1,
    );
    assert!(
        !before_restarted_deadline,
        "restart must discard the original deadline"
    );

    assert!(pump_until(&mut el, Duration::from_millis(500), || count
        .load(Ordering::SeqCst)
        >= 1));
    assert!(
        restarted.elapsed() >= Duration::from_millis(100),
        "fired {:?} after restart, expected about 120 ms",
        restarted.elapsed()
    );
}

#[test]
fn restarted_repeating_timer_keeps_firing() {
    let mut el = EventLoop::new();
    let (timer, count) = counting_timer(25, false);

    timer.start();
    timer.start();

    assert!(
        pump_until(&mut el, Duration::from_millis(600), || count
            .load(Ordering::SeqCst)
            >= 3),
        "restarted repeating timer fired {} times",
        count.load(Ordering::SeqCst)
    );
    timer.stop();
}

#[test]
fn restarting_a_timer_keeps_the_object_alive() {
    let _el = EventLoop::new();
    let (timer, _count) = counting_timer(1000, true);
    let id = timer.object_id();

    timer.start();
    assert!(with_object(id, |_| ()).is_some());
    timer.start();
    assert!(
        with_object(id, |_| ()).is_some(),
        "restarting a timer must not unregister its object"
    );
    timer.stop();
}

/// QObject whose `timer_event` counts deliveries (object-timer path, `QObject::startTimer`).
struct TickObject {
    data: ObjectData,
    ticks: Arc<AtomicU32>,
}

impl TickObject {
    fn new(ticks: Arc<AtomicU32>) -> Self {
        Self {
            data: ObjectData::new(ObjectId::next()),
            ticks,
        }
    }
}

impl QObject for TickObject {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
    }
    fn as_qobject_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn as_qobject_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
    fn timer_event(&mut self, _timer_id: u64) {
        self.ticks.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn start_timer_fires_timer_event() {
    let mut el = EventLoop::new();
    let ticks = Arc::new(AtomicU32::new(0));
    // SAFETY: the owner stays on this thread until the object is unregistered below; nothing
    // touches the object while a callback of it runs.
    let mut obj = unsafe { register_owned_qobject(TickObject::new(Arc::clone(&ticks))) };

    let id = obj
        .with_mut(|o| o.start_timer(20, TimerType::Precise))
        .unwrap();
    assert!(id.is_valid());
    assert!(
        pump_until(&mut el, Duration::from_secs(2), || ticks
            .load(Ordering::SeqCst)
            >= 3),
        "QObject::start_timer must arm delivery; got {} ticks",
        ticks.load(Ordering::SeqCst)
    );
    obj.with_mut(|o| o.kill_timer(id)).unwrap();
    // SAFETY: `obj` (created above in this fn) is alive and unmoved until it drops at the end of the fn, this
    // is the registration thread, and no pump runs here, so no callback of it is active.
    unsafe { unregister_qobject(obj.with(|o| o.object_data().id()).unwrap()) };
}

#[test]
fn kill_timer_stops_firing() {
    let mut el = EventLoop::new();
    let ticks = Arc::new(AtomicU32::new(0));
    // SAFETY: the owner stays on this thread until the object is unregistered below; nothing
    // touches the object while a callback of it runs.
    let mut obj = unsafe { register_owned_qobject(TickObject::new(Arc::clone(&ticks))) };

    let id = obj
        .with_mut(|o| o.start_timer(20, TimerType::Precise))
        .unwrap();
    assert!(pump_until(&mut el, Duration::from_secs(2), || ticks
        .load(Ordering::SeqCst)
        >= 1));
    assert!(obj.with_mut(|o| o.kill_timer(id)).unwrap());
    let snapshot = ticks.load(Ordering::SeqCst);
    let settle = Instant::now() + Duration::from_millis(200);
    while Instant::now() < settle {
        el.process_events(false);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(
        ticks.load(Ordering::SeqCst),
        snapshot,
        "killed object timer must stop"
    );
    // SAFETY: `obj` (created above in this fn) is alive and unmoved until it drops at the end of the fn, this
    // is the registration thread, and no pump runs here, so no callback of it is active.
    unsafe { unregister_qobject(obj.with(|o| o.object_data().id()).unwrap()) };
}

#[test]
fn zero_interval_object_timer_fires_repeatedly() {
    let mut el = EventLoop::new();
    let ticks = Arc::new(AtomicU32::new(0));
    // SAFETY: the owner stays on this thread until the object is unregistered below; nothing
    // touches the object while a callback of it runs.
    let mut obj = unsafe { register_owned_qobject(TickObject::new(Arc::clone(&ticks))) };

    let id = obj
        .with_mut(|o| o.start_timer(0, TimerType::Coarse))
        .unwrap();
    assert!(
        pump_until(&mut el, Duration::from_secs(2), || ticks
            .load(Ordering::SeqCst)
            >= 5),
        "zero-interval object timer must repost ZeroTimer; got {} ticks",
        ticks.load(Ordering::SeqCst)
    );
    obj.with_mut(|o| o.kill_timer(id)).unwrap();
    // SAFETY: `obj` (created above in this fn) is alive and unmoved until it drops at the end of the fn, this
    // is the registration thread, and no pump runs here, so no callback of it is active.
    unsafe { unregister_qobject(obj.with(|o| o.object_data().id()).unwrap()) };
}

/// Object filter that counts `Timer` events it sees and optionally consumes them.
struct FilterSpy {
    data: ObjectData,
    seen: Arc<AtomicU32>,
    consume: bool,
}

impl FilterSpy {
    fn new(seen: Arc<AtomicU32>, consume: bool) -> Self {
        Self {
            data: ObjectData::new(ObjectId::next()),
            seen,
            consume,
        }
    }
}

impl QObject for FilterSpy {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
    }
    fn as_qobject_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn as_qobject_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
    fn event_filter(&mut self, _watched: ObjectId, event: &mut Event) -> bool {
        if matches!(event.kind, EventKind::Timer { .. }) {
            self.seen.fetch_add(1, Ordering::SeqCst);
            return self.consume;
        }
        false
    }
}

/// QObject that overrides `event()` and counts `Timer` events.
struct EventSpy {
    data: ObjectData,
    timers: Arc<AtomicU32>,
}

impl EventSpy {
    fn new(timers: Arc<AtomicU32>) -> Self {
        Self {
            data: ObjectData::new(ObjectId::next()),
            timers,
        }
    }
}

impl QObject for EventSpy {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
    }
    fn as_qobject_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn as_qobject_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
    fn event(&mut self, event: &mut Event) -> bool {
        if matches!(event.kind, EventKind::Timer { .. }) {
            self.timers.fetch_add(1, Ordering::SeqCst);
            return true;
        }
        false
    }
}

#[test]
fn timer_event_passes_event_filter() {
    let mut el = EventLoop::new();
    let ticks = Arc::new(AtomicU32::new(0));
    let seen = Arc::new(AtomicU32::new(0));
    // SAFETY: both owners stay on this thread until the unregisters below; nothing touches either
    // object while a callback of it runs.
    let mut obj = unsafe { register_owned_qobject(TickObject::new(Arc::clone(&ticks))) };
    // SAFETY: same contract as the registration above: `filter` stays owned on this thread until
    // it is unregistered below, and nothing touches it while one of its callbacks runs.
    let filter = unsafe { register_owned_qobject(FilterSpy::new(Arc::clone(&seen), false)) };
    let filter_id = filter.with(|f| f.object_data().id()).unwrap();
    obj.with_mut(|o| o.object_data_mut().install_event_filter(filter_id))
        .unwrap();

    let id = obj
        .with_mut(|o| o.start_timer(20, TimerType::Precise))
        .unwrap();
    assert!(pump_until(&mut el, Duration::from_secs(2), || ticks
        .load(Ordering::SeqCst)
        >= 1));
    obj.with_mut(|o| o.kill_timer(id)).unwrap();
    obj.with_mut(|o| o.object_data_mut().remove_event_filter(filter_id))
        .unwrap();
    assert!(
        seen.load(Ordering::SeqCst) >= 1,
        "timer tick must pass the object event filter; filter saw {} timer events, object got {} ticks",
        seen.load(Ordering::SeqCst),
        ticks.load(Ordering::SeqCst)
    );
    // SAFETY: `unregister_qobject` needs both objects alive and unmoved until it returns, and to run on
    // the registration thread with no callback of them active. `obj` and `filter` are owners
    // created in this fn, still in scope, and neither is dropped before this block ends; this is
    // the registering thread and no pump (`el`) is running, so no callback is active.
    unsafe {
        unregister_qobject(obj.with(|o| o.object_data().id()).unwrap());
        unregister_qobject(filter.with(|f| f.object_data().id()).unwrap());
    }
}

#[test]
fn timer_event_reaches_event_override() {
    let mut el = EventLoop::new();
    let timers = Arc::new(AtomicU32::new(0));
    // SAFETY: the owner stays on this thread until the object is unregistered below; nothing
    // touches the object while a callback of it runs.
    let mut obj = unsafe { register_owned_qobject(EventSpy::new(Arc::clone(&timers))) };

    let id = obj
        .with_mut(|o| o.start_timer(20, TimerType::Precise))
        .unwrap();
    assert!(pump_until(&mut el, Duration::from_secs(2), || timers
        .load(Ordering::SeqCst)
        >= 1));
    obj.with_mut(|o| o.kill_timer(id)).unwrap();
    assert!(
        timers.load(Ordering::SeqCst) >= 1,
        "QObject::event must receive Timer; got {} Timer events",
        timers.load(Ordering::SeqCst)
    );
    // SAFETY: `obj` (created above in this fn) is alive and unmoved until it drops at the end of the fn, this
    // is the registration thread, and no pump runs here, so no callback of it is active.
    unsafe { unregister_qobject(obj.with(|o| o.object_data().id()).unwrap()) };
}

/// Qt posts `QZeroTimerEvent` to the dispatcher, not to the timer object, so object filters never
/// see it. Each delivered `QTimerEvent` is sent through filters, and the zero timer is reposted
/// after the send regardless of the filter result (qeventdispatcher_win.cpp:886-899). A filter that
/// consumes every tick therefore must not stop the zero-interval timer.
#[test]
fn zero_timer_keeps_repeating_when_filter_consumes_ticks() {
    let mut el = EventLoop::new();
    let ticks = Arc::new(AtomicU32::new(0));
    let seen = Arc::new(AtomicU32::new(0));
    // SAFETY: both owners stay on this thread until the unregisters below; nothing touches either
    // object while a callback of it runs.
    let mut obj = unsafe { register_owned_qobject(TickObject::new(Arc::clone(&ticks))) };
    // SAFETY: same contract as the registration above: `filter` stays owned on this thread until
    // it is unregistered below, and nothing touches it while one of its callbacks runs.
    let filter = unsafe { register_owned_qobject(FilterSpy::new(Arc::clone(&seen), true)) };
    let filter_id = filter.with(|f| f.object_data().id()).unwrap();
    obj.with_mut(|o| o.object_data_mut().install_event_filter(filter_id))
        .unwrap();

    let id = obj
        .with_mut(|o| o.start_timer(0, TimerType::Coarse))
        .unwrap();
    assert!(
        pump_until(&mut el, Duration::from_secs(2), || seen
            .load(Ordering::SeqCst)
            >= 5),
        "zero-interval timer must keep repeating after a filter consumes ticks; filter saw {} Timer events",
        seen.load(Ordering::SeqCst)
    );
    obj.with_mut(|o| o.kill_timer(id)).unwrap();
    obj.with_mut(|o| o.object_data_mut().remove_event_filter(filter_id))
        .unwrap();
    assert_eq!(
        ticks.load(Ordering::SeqCst),
        0,
        "a consuming filter must stop the tick reaching timer_event"
    );
    // SAFETY: `unregister_qobject` needs both objects alive and unmoved until it returns, and to run on
    // the registration thread with no callback of them active. `obj` and `filter` are owners
    // created in this fn, still in scope, and neither is dropped before this block ends; this is
    // the registering thread and no pump (`el`) is running, so no callback is active.
    unsafe {
        unregister_qobject(obj.with(|o| o.object_data().id()).unwrap());
        unregister_qobject(filter.with(|f| f.object_data().id()).unwrap());
    }
}
