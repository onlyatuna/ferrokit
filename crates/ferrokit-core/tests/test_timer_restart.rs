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
    register_owned_qobject, unregister_qobject, ObjectData, ObjectId, QObject,
};
use ferrokit_core::timer::{Timer, TimerType};
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn counting_timer(interval_ms: u64, single_shot: bool) -> (Pin<Box<Timer>>, Arc<AtomicU32>) {
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
    let (mut timer, count) = counting_timer(40, true);

    timer.as_mut().start();
    timer.as_mut().start();

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
    let (mut timer, count) = counting_timer(40, true);

    timer.as_mut().start();
    timer.stop();
    timer.as_mut().start();

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
    let (mut timer, count) = counting_timer(120, true);

    timer.as_mut().start();
    let first = Instant::now();
    assert!(
        !pump_until(&mut el, Duration::from_millis(70), || count
            .load(Ordering::SeqCst)
            >= 1)
    );
    timer.as_mut().start();
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
    let (mut timer, count) = counting_timer(25, false);

    timer.as_mut().start();
    timer.as_mut().start();

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
    let (mut timer, _count) = counting_timer(1000, true);

    timer.as_mut().start();
    assert!(timer.object_data().is_alive());
    timer.as_mut().start();
    assert!(
        timer.object_data().is_alive(),
        "re-registering an object must not kill its own liveness token"
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

    let id = obj.start_timer(20, TimerType::Precise);
    assert!(id.is_valid());
    assert!(
        pump_until(&mut el, Duration::from_secs(2), || ticks
            .load(Ordering::SeqCst)
            >= 3),
        "QObject::start_timer must arm delivery; got {} ticks",
        ticks.load(Ordering::SeqCst)
    );
    obj.kill_timer(id);
    unsafe { unregister_qobject(obj.object_data().id()) };
}

#[test]
fn kill_timer_stops_firing() {
    let mut el = EventLoop::new();
    let ticks = Arc::new(AtomicU32::new(0));
    // SAFETY: the owner stays on this thread until the object is unregistered below; nothing
    // touches the object while a callback of it runs.
    let mut obj = unsafe { register_owned_qobject(TickObject::new(Arc::clone(&ticks))) };

    let id = obj.start_timer(20, TimerType::Precise);
    assert!(pump_until(&mut el, Duration::from_secs(2), || ticks
        .load(Ordering::SeqCst)
        >= 1));
    assert!(obj.kill_timer(id));
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
    unsafe { unregister_qobject(obj.object_data().id()) };
}

#[test]
fn zero_interval_object_timer_fires_repeatedly() {
    let mut el = EventLoop::new();
    let ticks = Arc::new(AtomicU32::new(0));
    // SAFETY: the owner stays on this thread until the object is unregistered below; nothing
    // touches the object while a callback of it runs.
    let mut obj = unsafe { register_owned_qobject(TickObject::new(Arc::clone(&ticks))) };

    let id = obj.start_timer(0, TimerType::Coarse);
    assert!(
        pump_until(&mut el, Duration::from_secs(2), || ticks
            .load(Ordering::SeqCst)
            >= 5),
        "zero-interval object timer must repost ZeroTimer; got {} ticks",
        ticks.load(Ordering::SeqCst)
    );
    obj.kill_timer(id);
    unsafe { unregister_qobject(obj.object_data().id()) };
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
    fn object_data_mut(&mut self) -> &mut ObjectData {
        &mut self.data
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
    let filter = unsafe { register_owned_qobject(FilterSpy::new(Arc::clone(&seen), false)) };
    let filter_id = filter.object_data().id();
    obj.object_data_mut().install_event_filter(filter_id);

    let id = obj.start_timer(20, TimerType::Precise);
    assert!(pump_until(&mut el, Duration::from_secs(2), || ticks
        .load(Ordering::SeqCst)
        >= 1));
    obj.kill_timer(id);
    obj.object_data_mut().remove_event_filter(filter_id);
    assert!(
        seen.load(Ordering::SeqCst) >= 1,
        "timer tick must pass the object event filter; filter saw {} timer events, object got {} ticks",
        seen.load(Ordering::SeqCst),
        ticks.load(Ordering::SeqCst)
    );
    unsafe {
        unregister_qobject(obj.object_data().id());
        unregister_qobject(filter.object_data().id());
    }
}

#[test]
fn timer_event_reaches_event_override() {
    let mut el = EventLoop::new();
    let timers = Arc::new(AtomicU32::new(0));
    // SAFETY: the owner stays on this thread until the object is unregistered below; nothing
    // touches the object while a callback of it runs.
    let mut obj = unsafe { register_owned_qobject(EventSpy::new(Arc::clone(&timers))) };

    let id = obj.start_timer(20, TimerType::Precise);
    assert!(pump_until(&mut el, Duration::from_secs(2), || timers
        .load(Ordering::SeqCst)
        >= 1));
    obj.kill_timer(id);
    assert!(
        timers.load(Ordering::SeqCst) >= 1,
        "QObject::event must receive Timer; got {} Timer events",
        timers.load(Ordering::SeqCst)
    );
    unsafe { unregister_qobject(obj.object_data().id()) };
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
    let filter = unsafe { register_owned_qobject(FilterSpy::new(Arc::clone(&seen), true)) };
    let filter_id = filter.object_data().id();
    obj.object_data_mut().install_event_filter(filter_id);

    let id = obj.start_timer(0, TimerType::Coarse);
    assert!(
        pump_until(&mut el, Duration::from_secs(2), || seen.load(Ordering::SeqCst)
            >= 5),
        "zero-interval timer must keep repeating after a filter consumes ticks; filter saw {} Timer events",
        seen.load(Ordering::SeqCst)
    );
    obj.kill_timer(id);
    obj.object_data_mut().remove_event_filter(filter_id);
    assert_eq!(
        ticks.load(Ordering::SeqCst),
        0,
        "a consuming filter must stop the tick reaching timer_event"
    );
    unsafe {
        unregister_qobject(obj.object_data().id());
        unregister_qobject(filter.object_data().id());
    }
}
