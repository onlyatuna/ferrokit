// SPDX-License-Identifier: LGPL-3.0-only
use super::*;

#[test]
fn test_timer_id() {
    assert_eq!(TimerId::INVALID, TimerId(0));
    assert!(!TimerId::INVALID.is_valid());
    assert_eq!(TimerId::default(), TimerId::INVALID);

    let valid_id = TimerId(1);
    assert!(valid_id.is_valid());
    assert_ne!(valid_id, TimerId::INVALID);
    assert_eq!(valid_id.0, 1);
}
#[test]
fn test_timer_id_global_monotonicity() {
    let id1 = allocate_timer_id();
    let id2 = allocate_timer_id();
    assert!(id1.is_valid());
    assert!(id2.is_valid());
    assert_ne!(
        id1, id2,
        "TimerId must be unique globally across registries"
    );
    assert!(
        id2.0 > id1.0,
        "TimerId must increment monotonically globally"
    );
}

#[test]
fn test_timer_type_defaults() {
    assert_eq!(TimerType::default(), TimerType::Coarse);
    assert_ne!(TimerType::Precise, TimerType::Coarse);
    assert_ne!(TimerType::VeryCoarse, TimerType::Coarse);
}

#[test]
fn test_timer_entry_creation() {
    let entry = TimerEntry::new(TimerId(10), 100, 1100, TimerType::Precise, ObjectId(99));
    assert_eq!(entry.id, TimerId(10));
    assert_eq!(entry.interval_ms, 100);
    assert_eq!(entry.next_fire_ms, 1100);
    assert_eq!(entry.timer_type, TimerType::Precise);
    assert_eq!(entry.receiver, ObjectId(99));
    assert_eq!(entry.win32_timer_id, 0);
    assert!(!entry.in_timer_event);
    assert!(!entry.single_shot);
}

#[test]
fn test_calculate_next_timeout() {
    // 1. Precise
    let mut t1 = TimerType::Precise;
    let (inv1, to1) = calculate_next_timeout(&mut t1, 15, 100);
    assert_eq!(t1, TimerType::Precise);
    assert_eq!(inv1, 15);
    assert_eq!(to1, 115);

    let mut t2 = TimerType::Coarse;
    let (inv2, to2) = calculate_next_timeout(&mut t2, 10, 100);
    assert_eq!(t2, TimerType::Precise);
    assert_eq!(inv2, 10);
    assert_eq!(to2, 110);

    let mut t3 = TimerType::Coarse;
    let (inv3, to3) = calculate_next_timeout(&mut t3, 25000, 5500);
    assert_eq!(t3, TimerType::VeryCoarse);
    assert_eq!(inv3, 25000);
    assert_eq!(to3, 5000 + 25000);

    let mut t4 = TimerType::VeryCoarse;
    let (inv4, to4) = calculate_next_timeout(&mut t4, 1400, 1200);
    assert_eq!(t4, TimerType::VeryCoarse);
    assert_eq!(inv4, 1000);
    assert_eq!(to4, 1000 + 1000);
}

/// Expected outcome of `calculate_next_timeout`, computed in 128-bit arithmetic that cannot
/// overflow, for the type the call settles on.
fn expected_next_timeout(settled: TimerType, interval: u64, now: u64) -> (u64, u64) {
    let max = u64::MAX as u128;
    let (adjusted, floor_now) = match settled {
        TimerType::VeryCoarse => {
            let rounded = ((interval as u128 + 500) / 1000 * 1000).min(max / 1000 * 1000);
            (rounded.max(1000), now as u128 / 1000 * 1000)
        }
        TimerType::Precise | TimerType::Coarse => (interval as u128, now as u128),
    };
    (adjusted as u64, (floor_now + adjusted).min(max) as u64)
}

#[test]
fn calculate_next_timeout_saturates_instead_of_overflowing() {
    let intervals = [
        1,
        999,
        20_000,
        u64::MAX / 2,
        i64::MAX as u64,
        u64::MAX - 501,
        u64::MAX - 500,
        u64::MAX - 499,
        u64::MAX - 1,
        u64::MAX,
    ];
    let clocks = [0, 999, 1_000_000, u64::MAX / 2, u64::MAX - 1, u64::MAX];
    for start in [TimerType::Precise, TimerType::Coarse, TimerType::VeryCoarse] {
        for interval in intervals {
            for now in clocks {
                let mut settled = start;
                let got = calculate_next_timeout(&mut settled, interval, now);
                assert_eq!(
                    got,
                    expected_next_timeout(settled, interval, now),
                    "{start:?} interval {interval} clock {now}"
                );
            }
        }
    }
}

#[test]
fn an_unreachable_deadline_is_clamped_to_the_far_future() {
    // The largest multiple of 1000 that fits is the rounded very coarse interval.
    let largest_rounded = u64::MAX / 1000 * 1000;
    let mut very_coarse = TimerType::VeryCoarse;
    assert_eq!(
        calculate_next_timeout(&mut very_coarse, u64::MAX, 1000),
        (largest_rounded, u64::MAX)
    );

    let mut coarse = TimerType::Coarse;
    assert_eq!(
        calculate_next_timeout(&mut coarse, u64::MAX, 1000),
        (largest_rounded, u64::MAX)
    );
    assert_eq!(coarse, TimerType::VeryCoarse);

    let mut precise = TimerType::Precise;
    assert_eq!(
        calculate_next_timeout(&mut precise, u64::MAX, 1000),
        (u64::MAX, u64::MAX)
    );

    // A saturated deadline is far beyond every clock reading, so the timer is registered but
    // never reported as expired.
    for ty in [TimerType::Precise, TimerType::Coarse, TimerType::VeryCoarse] {
        let mut registry = TimerRegistry::new();
        let id = registry.register(ObjectId(1), u64::MAX, ty, false);
        assert!(registry.get(id).unwrap().next_fire_ms >= largest_rounded);
        assert!(registry.next_deadline().unwrap() >= largest_rounded);
        assert!(registry.expired_timers(largest_rounded - 1).is_empty());
    }
}

#[test]
fn test_timer_registry_crud_and_deadline() {
    let mut registry = TimerRegistry::new();
    assert!(registry.is_empty());
    assert_eq!(registry.next_deadline(), None);

    let receiver = ObjectId(100);
    let id1 = registry.register(receiver, 500, TimerType::Precise, false);
    assert!(id1.is_valid());
    assert_eq!(registry.len(), 1);

    let entry1 = registry.get(id1).expect("entry1 should exist");
    assert_eq!(entry1.interval_ms, 500);
    assert!(!entry1.single_shot);

    let id2 = registry.register(receiver, 100, TimerType::Precise, true);
    assert!(id2.is_valid());
    assert_ne!(id1, id2);
    assert_eq!(registry.len(), 2);

    let deadline = registry.next_deadline().unwrap();
    let entry2 = registry.get(id2).unwrap();
    assert_eq!(deadline, entry2.next_fire_ms);

    if let Some(entry_mut) = registry.get_mut(id1) {
        entry_mut.in_timer_event = true;
    }
    assert!(registry.get(id1).unwrap().in_timer_event);

    let removed2 = registry.unregister(id2).expect("id2 removed");
    assert_eq!(removed2.id, id2);
    assert_eq!(registry.len(), 1);
    assert_eq!(
        registry.next_deadline(),
        Some(registry.get(id1).unwrap().next_fire_ms)
    );

    registry.unregister(id1);
    assert!(registry.is_empty());
    assert_eq!(registry.next_deadline(), None);
}

#[test]
fn test_timer_start_stop_and_timeout_order() {
    use std::sync::{Arc, Mutex};

    let mut registry = TimerRegistry::new();
    let timer = Timer::new();
    timer.set_interval(100);
    timer.set_single_shot(true);

    let log = Arc::new(Mutex::new(Vec::<&'static str>::new()));
    let log2 = Arc::clone(&log);
    timer.timeout.connect(move |_| {
        log2.lock().unwrap().push("timeout");
    });

    timer.start_with_registry(&mut registry);
    assert!(timer.is_active());
    let id = timer.timer_id();
    assert!(registry.get(id).is_some());

    timer.timer_event_with_registry(&TimerEvent::new(id), &mut registry);
    assert!(!timer.is_active());
    assert!(registry.get(id).is_none());
    assert_eq!(*log.lock().unwrap(), ["timeout"]);
}

#[test]
fn test_timer_ergonomic_api_with_event_loop() {
    use crate::event_loop::EventLoop;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    let event_loop = EventLoop::new();
    let timer = Timer::new();
    timer.set_interval(10);
    timer.set_single_shot(true);

    let fired = Arc::new(AtomicBool::new(false));
    let f_clone = Arc::clone(&fired);
    timer.timeout.connect(move |_| {
        f_clone.store(true, Ordering::SeqCst);
    });

    timer.start();
    assert!(timer.is_active());

    let deadline = std::time::Instant::now() + Duration::from_millis(500);
    while !fired.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
        event_loop.process_events(false);
        std::thread::sleep(Duration::from_millis(5));
    }

    assert!(fired.load(Ordering::SeqCst));
    assert!(!timer.is_active());
}

#[cfg(windows)]
fn spin_until(
    dispatcher: &mut crate::event_loop::Win32EventDispatcher,
    registry: &Mutex<TimerRegistry>,
    timeout: std::time::Duration,
    mut condition: impl FnMut() -> bool,
) -> bool {
    let start = std::time::Instant::now();
    while !condition() {
        if start.elapsed() > timeout {
            return false;
        }
        dispatcher.process_events_with_timers(false, None, registry);
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    true
}

#[cfg(windows)]
#[test]
fn test_single_shot_timer() {
    use crate::event_loop::Win32EventDispatcher;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    let mut dispatcher = Win32EventDispatcher::new();
    let registry = Mutex::new(TimerRegistry::new());

    let counter = Arc::new(AtomicU32::new(0));
    let counter_clone = counter.clone();

    let timer = Timer::new();
    timer.set_single_shot(true);
    timer.set_interval(30);
    timer.timeout.connect(move |_| {
        counter_clone.fetch_add(1, Ordering::SeqCst);
    });

    timer.start_with(&mut registry.lock().unwrap(), &mut dispatcher);
    assert!(timer.is_active());

    let ok = spin_until(
        &mut dispatcher,
        &registry,
        Duration::from_millis(500),
        || counter.load(Ordering::SeqCst) >= 1,
    );

    assert!(ok, "Timer failed to fire within timeout");
    assert_eq!(counter.load(Ordering::SeqCst), 1);

    std::thread::sleep(Duration::from_millis(50));
    dispatcher.process_events_with_timers(false, None, &registry);
    assert_eq!(counter.load(Ordering::SeqCst), 1);
    assert!(
        !timer.is_active(),
        "Single shot timer should be inactive after firing"
    );
}

#[cfg(windows)]
#[test]
fn test_timer_cancellation() {
    use crate::event_loop::Win32EventDispatcher;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    let mut dispatcher = Win32EventDispatcher::new();
    let registry = Mutex::new(TimerRegistry::new());

    let counter = Arc::new(AtomicU32::new(0));
    let counter_clone = counter.clone();

    let timer = Timer::new();
    timer.set_interval(50);
    timer.timeout.connect(move |_| {
        counter_clone.fetch_add(1, Ordering::SeqCst);
    });

    timer.start_with(&mut registry.lock().unwrap(), &mut dispatcher);

    timer.stop_with(&mut registry.lock().unwrap(), &mut dispatcher);
    assert!(!timer.is_active());

    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_millis(100) {
        dispatcher.process_events_with_timers(false, None, &registry);
        std::thread::sleep(Duration::from_millis(5));
    }

    assert_eq!(counter.load(Ordering::SeqCst), 0);
}
#[test]
fn test_zero_timer_immediate_dispatch() {
    use crate::event_loop::EventLoop;
    use std::sync::atomic::{AtomicBool, Ordering};

    let event_loop = EventLoop::new();

    let timer = Timer::new();
    timer.set_interval(0);
    timer.set_single_shot(true);

    let fired = Arc::new(AtomicBool::new(false));
    let f_clone = Arc::clone(&fired);
    timer.timeout.connect(move |_| {
        f_clone.store(true, Ordering::SeqCst);
    });

    timer.start();
    assert!(timer.is_active());

    assert!(!fired.load(Ordering::SeqCst));

    let processed = event_loop.process_events(false);
    assert!(processed);
    assert!(
        fired.load(Ordering::SeqCst),
        "Zero timer should fire immediately in the first process_events call"
    );
    assert!(
        !timer.is_active(),
        "Single shot zero timer should be inactive after firing"
    );
    assert_eq!(
        event_loop.timer_registry().lock().unwrap().len(),
        0,
        "fired single-shot zero timer must unregister from the registry"
    );
}

#[test]
fn zero_single_shot_leaves_registry_empty() {
    use crate::event_loop::EventLoop;
    use std::sync::atomic::{AtomicBool, Ordering};

    let el = EventLoop::new();
    let fired = Arc::new(AtomicBool::new(false));
    let f = Arc::clone(&fired);
    let timer = Timer::new();
    timer.set_interval(0);
    timer.set_single_shot(true);
    timer.timeout.connect(move |_| {
        f.store(true, Ordering::SeqCst);
    });
    timer.start();
    el.process_events(false);
    assert!(fired.load(Ordering::SeqCst));
    assert_eq!(
        el.timer_registry().lock().unwrap().len(),
        0,
        "fired zero single-shot must unregister"
    );
    assert_eq!(
        el.next_timeout(),
        None,
        "idle loop must not spin on a fired zero timer"
    );
}

#[test]
fn zero_timer_repeats_each_iteration() {
    use crate::event_loop::EventLoop;
    use std::sync::atomic::{AtomicU32, Ordering};

    let el = EventLoop::new();
    let count = Arc::new(AtomicU32::new(0));
    let c = Arc::clone(&count);
    let timer = Timer::new();
    timer.set_interval(0);
    timer.timeout.connect(move |_| {
        c.fetch_add(1, Ordering::SeqCst);
    });
    timer.start();
    for _ in 0..100 {
        if count.load(Ordering::SeqCst) >= 5 {
            break;
        }
        el.process_events(false);
    }
    assert!(
        count.load(Ordering::SeqCst) >= 5,
        "repeating zero timer must fire on successive iterations, got {}",
        count.load(Ordering::SeqCst)
    );
    timer.stop();
}

#[test]
fn set_interval_on_active_timer_restarts_with_new_interval() {
    use crate::event_loop::EventLoop;
    use std::sync::atomic::{AtomicU32, Ordering};

    let el = EventLoop::new();
    let count = Arc::new(AtomicU32::new(0));
    let c = Arc::clone(&count);
    let timer = Timer::new();
    timer.set_interval(60_000);
    timer.timeout.connect(move |_| {
        c.fetch_add(1, Ordering::SeqCst);
    });
    timer.start();
    timer.set_interval(0);
    for _ in 0..100 {
        if count.load(Ordering::SeqCst) >= 1 {
            break;
        }
        el.process_events(false);
    }
    assert!(
        count.load(Ordering::SeqCst) >= 1,
        "QTimer::setInterval on an active timer must restart it with the new interval"
    );
    timer.stop();
}

/// Pumps `el` for `ms` milliseconds and returns how many times `count` advanced.
fn pump_for(
    el: &mut crate::event_loop::EventLoop,
    ms: u64,
    count: &std::sync::atomic::AtomicU32,
) -> u32 {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(ms);
    while std::time::Instant::now() < deadline {
        el.process_events(false);
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    count.load(std::sync::atomic::Ordering::SeqCst)
}

#[test]
fn set_single_shot_true_on_active_timer_fires_once() {
    use std::sync::atomic::{AtomicU32, Ordering};

    let mut el = crate::event_loop::EventLoop::new();
    let count = Arc::new(AtomicU32::new(0));
    let c = Arc::clone(&count);
    let timer = Timer::new();
    timer.set_interval(15);
    timer.timeout.connect(move |_| {
        c.fetch_add(1, Ordering::SeqCst);
    });
    timer.start();
    timer.set_single_shot(true);
    // QTimer::timerEvent reads `single` at fire time.
    assert_eq!(pump_for(&mut el, 300, &count), 1);
    assert!(!timer.is_active());
}

#[test]
fn set_single_shot_false_on_active_timer_keeps_repeating() {
    use std::sync::atomic::{AtomicU32, Ordering};

    let mut el = crate::event_loop::EventLoop::new();
    let count = Arc::new(AtomicU32::new(0));
    let c = Arc::clone(&count);
    let timer = Timer::new();
    timer.set_interval(15);
    timer.set_single_shot(true);
    timer.timeout.connect(move |_| {
        c.fetch_add(1, Ordering::SeqCst);
    });
    timer.start();
    timer.set_single_shot(false);
    let fired = pump_for(&mut el, 300, &count);
    assert!(
        fired >= 2,
        "a timer made repeating while active must keep firing, got {fired}"
    );
    assert!(timer.is_active());
    timer.stop();
}

#[test]
fn test_timer_remaining_time() {
    use crate::event_loop::EventLoop;

    let timer = Timer::new();
    timer.set_interval(1000);

    assert_eq!(timer.remaining_time(), -1);

    let _event_loop = EventLoop::new();
    timer.start();
    assert!(timer.is_active());

    let rem = timer.remaining_time();
    assert!(
        rem > 0 && rem <= 1000,
        "remaining_time should be positive within interval, got {}",
        rem
    );

    timer.stop();
    assert_eq!(timer.remaining_time(), -1);
}

#[test]
fn test_qtimer_single_shot_static_api_zero_delay() {
    use crate::event_loop::EventLoop;
    use std::sync::atomic::{AtomicBool, Ordering};

    let event_loop = EventLoop::new();
    let executed = Arc::new(AtomicBool::new(false));
    let ex_clone = Arc::clone(&executed);

    Timer::single_shot(0, move || {
        ex_clone.store(true, Ordering::SeqCst);
    });

    assert!(!executed.load(Ordering::SeqCst));
    let processed = event_loop.process_events(false);
    assert!(processed);
    assert!(executed.load(Ordering::SeqCst));
}

// ---- Lifecycle of a `Timer` whose owner is dropped by one of its own slots.
//
// `Signal::emit` contains a panicking slot, so a slot never asserts: it records what it saw
// and the test asserts afterwards.

use std::sync::atomic::AtomicUsize;

thread_local! {
    static HOLD: RefCell<Option<Timer>> = const { RefCell::new(None) };
}

fn core_drops() -> usize {
    CORE_DROPS.with(|drops| drops.get())
}

fn hold(timer: Timer) {
    HOLD.with(|h| *h.borrow_mut() = Some(timer));
}

fn take_held() -> Option<Timer> {
    HOLD.with(|h| h.borrow_mut().take())
}

/// A thread timer context with no event loop: `start` arms the returned registry.
fn test_context() -> Arc<Mutex<TimerRegistry>> {
    let registry = Arc::new(Mutex::new(TimerRegistry::new()));
    #[cfg(windows)]
    register_thread_timer_context(Arc::clone(&registry), std::ptr::null_mut());
    #[cfg(not(windows))]
    register_thread_timer_context(Arc::clone(&registry));
    registry
}

fn deliver(timer_object: ObjectId, timer_id: TimerId) -> crate::event_loop::TimerDelivery {
    crate::event_loop::deliver_timer_event(timer_object, timer_id.0 as u64)
}

#[test]
fn owner_dropped_by_its_own_slot_is_freed_after_the_callback_returns() {
    use crate::event_loop::TimerDelivery;
    use crate::object::qobject::registered_on_current_thread;

    let registry = test_context();
    let timer = Timer::new();
    let id = timer.object_id();
    timer.set_interval(1000);
    let timer_id = timer.start();
    assert!(timer_id.is_valid());
    assert!(registered_on_current_thread(id));
    assert_eq!(registry.lock().unwrap().len(), 1);

    let retained = timer.timeout.clone();
    let in_slot_drops = Arc::new(AtomicUsize::new(usize::MAX));
    let in_slot_registered = Arc::new(AtomicUsize::new(usize::MAX));
    let in_slot_timers = Arc::new(AtomicUsize::new(usize::MAX));
    let second = Arc::new(AtomicUsize::new(0));
    {
        let (d, r, t, reg) = (
            Arc::clone(&in_slot_drops),
            Arc::clone(&in_slot_registered),
            Arc::clone(&in_slot_timers),
            Arc::clone(&registry),
        );
        timer.timeout.connect(move |_| {
            drop(take_held());
            d.store(core_drops(), Ordering::SeqCst);
            r.store(registered_on_current_thread(id) as usize, Ordering::SeqCst);
            t.store(reg.lock().unwrap().len(), Ordering::SeqCst);
        });
        let s = Arc::clone(&second);
        timer.timeout.connect(move |_| {
            s.fetch_add(1, Ordering::SeqCst);
        });
    }
    hold(timer);

    assert!(matches!(deliver(id, timer_id), TimerDelivery::Delivered));

    assert_eq!(
        in_slot_drops.load(Ordering::SeqCst),
        0,
        "object destroyed inside its callback"
    );
    assert_eq!(
        in_slot_registered.load(Ordering::SeqCst),
        0,
        "still registered after the drop"
    );
    assert_eq!(
        in_slot_timers.load(Ordering::SeqCst),
        0,
        "timer still scheduled after the drop"
    );
    assert_eq!(
        second.load(Ordering::SeqCst),
        0,
        "a slot ran for a destroyed owner"
    );
    assert_eq!(
        core_drops(),
        1,
        "object not freed once the callback returned"
    );
    assert!(!registered_on_current_thread(id));
    assert!(matches!(
        deliver(id, timer_id),
        TimerDelivery::NotDeliverable
    ));
    retained.emit(&());
    assert_eq!(
        second.load(Ordering::SeqCst),
        0,
        "a retained signal still reaches slots"
    );
    assert_eq!(registry.lock().unwrap().len(), 0);
    unregister_thread_timer_context();
}

#[test]
fn slot_may_restart_stop_and_reconfigure_its_own_timer() {
    use crate::event_loop::TimerDelivery;

    let registry = test_context();
    let timer = Timer::new();
    let id = timer.object_id();
    timer.set_interval(1000);
    let first = timer.start();
    let calls = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&calls);
    timer.timeout.connect(move |_| {
        HOLD.with(|h| {
            let h = h.borrow();
            let t = h.as_ref().expect("held");
            t.stop();
            t.set_interval(500);
            t.set_single_shot(false);
            t.start();
        });
        c.fetch_add(1, Ordering::SeqCst);
    });
    hold(timer);

    assert!(matches!(deliver(id, first), TimerDelivery::Delivered));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let timer = take_held().expect("held");
    assert!(timer.is_active());
    assert_ne!(timer.timer_id(), first, "restart must schedule a new timer");
    assert_eq!(timer.interval(), 500);
    assert_eq!(registry.lock().unwrap().len(), 1);
    assert_eq!(core_drops(), 0);
    drop(timer);
    assert_eq!(core_drops(), 1);
    assert_eq!(registry.lock().unwrap().len(), 0);
    unregister_thread_timer_context();
}

#[test]
fn single_shot_slot_may_restart_and_then_drop_its_owner() {
    use crate::event_loop::TimerDelivery;

    let registry = test_context();
    let timer = Timer::new();
    let id = timer.object_id();
    timer.set_interval(1000);
    timer.set_single_shot(true);
    let first = timer.start();
    let calls = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&calls);
    timer.timeout.connect(move |_| {
        if c.fetch_add(1, Ordering::SeqCst) == 0 {
            HOLD.with(|h| {
                h.borrow().as_ref().expect("held").start();
            });
        } else {
            drop(take_held());
        }
    });
    hold(timer);

    assert!(matches!(deliver(id, first), TimerDelivery::Delivered));
    let second_id = HOLD.with(|h| h.borrow().as_ref().expect("held").timer_id());
    assert!(second_id.is_valid() && second_id != first);
    assert_eq!(
        registry.lock().unwrap().len(),
        1,
        "single-shot stop ran before the slot"
    );
    assert_eq!(core_drops(), 0);

    assert!(matches!(deliver(id, second_id), TimerDelivery::Delivered));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(core_drops(), 1);
    assert_eq!(registry.lock().unwrap().len(), 0);
    unregister_thread_timer_context();
}

#[test]
fn owner_dropped_outside_a_callback_is_freed_at_once() {
    use crate::object::qobject::registered_on_current_thread;

    let registry = test_context();
    let never_started = Timer::new();
    let id = never_started.object_id();
    assert!(registered_on_current_thread(id), "registered by `new`");
    drop(never_started);
    assert_eq!(core_drops(), 1);
    assert!(!registered_on_current_thread(id));

    let started = Timer::new();
    started.set_interval(1000);
    assert!(started.start().is_valid());
    assert_eq!(registry.lock().unwrap().len(), 1);
    drop(started);
    assert_eq!(core_drops(), 2);
    assert_eq!(registry.lock().unwrap().len(), 0);
    unregister_thread_timer_context();
}

#[test]
fn unregister_removes_the_timer_when_stop_was_blocked_by_a_context_borrow() {
    let registry = test_context();
    let timer = Timer::new();
    timer.set_interval(1000);
    assert!(timer.start().is_valid());
    assert_eq!(registry.lock().unwrap().len(), 1);

    // `stop` cannot reach the context while it is mutably borrowed: it skips the disarm and
    // still marks the timer inactive.
    THREAD_TIMER_CONTEXT.with(|ctx| {
        let _held = ctx.borrow_mut();
        timer.stop();
    });
    assert!(!timer.is_active());
    assert_eq!(
        registry.lock().unwrap().len(),
        1,
        "the blocked stop must not reach the registry"
    );

    drop(timer);
    assert_eq!(core_drops(), 1);
    assert_eq!(
        registry.lock().unwrap().len(),
        0,
        "unregistering the object must remove the entry `stop` could not"
    );
    unregister_thread_timer_context();
}

#[test]
fn panicking_slot_after_the_owner_was_dropped_still_frees_the_object_once() {
    use crate::event_loop::TimerDelivery;

    let _registry = test_context();
    let timer = Timer::new();
    let id = timer.object_id();
    timer.set_interval(1000);
    let timer_id = timer.start();
    timer.timeout.connect(|_| {
        drop(take_held());
        panic!("slot panics after dropping its owner");
    });
    hold(timer);

    assert!(matches!(deliver(id, timer_id), TimerDelivery::Delivered));
    assert_eq!(core_drops(), 1);
    unregister_thread_timer_context();
}

#[test]
fn win32_timer_elapse_caps_at_user_timer_maximum_and_begin_delivery_waits_for_deadline() {
    assert_eq!(WIN32_TIMER_MAX_MS, 0x7FFF_FFFF);
    assert_eq!(win32_timer_elapse(0), 0);
    assert_eq!(win32_timer_elapse(0x7FFF_FFFE), 0x7FFF_FFFE);
    assert_eq!(win32_timer_elapse(0x7FFF_FFFF), 0x7FFF_FFFF);
    assert_eq!(win32_timer_elapse(0x8000_0000), 0x7FFF_FFFF);
    assert_eq!(win32_timer_elapse(u32::MAX as u64), 0x7FFF_FFFF);
    assert_eq!(win32_timer_elapse(u64::MAX), 0x7FFF_FFFF);

    let registry = Mutex::new(TimerRegistry::new());
    let id = registry
        .lock()
        .unwrap()
        .register(ObjectId(99), u64::MAX, TimerType::Precise, true);
    let deadline = registry.lock().unwrap().get(id).unwrap().next_fire_ms;
    assert_eq!(deadline, u64::MAX);

    assert!(
        begin_timer_delivery(&registry, id, WIN32_TIMER_MAX_MS).is_none(),
        "unexpired timer must not begin delivery at USER_TIMER_MAXIMUM"
    );
    assert!(
        begin_timer_delivery(&registry, id, u64::MAX - 1).is_none(),
        "unexpired timer must not begin delivery before next_fire_ms"
    );
    let entry = registry.lock().unwrap().get(id).unwrap().clone();
    assert!(!entry.in_timer_event);
    assert_eq!(entry.next_fire_ms, u64::MAX);

    assert_eq!(
        begin_timer_delivery(&registry, id, u64::MAX),
        Some((ObjectId(99), true))
    );
}
