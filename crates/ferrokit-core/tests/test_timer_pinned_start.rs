// SPDX-License-Identifier: LGPL-3.0-only
#![cfg(windows)]
//! A started `Timer` delivers `timeout` through the event loop, and dropping it, also from one of
//! its own slots, unregisters it. These tests use the public API with no `unsafe` at the call site.

use ferrokit_core::event_loop::EventLoop;
use ferrokit_core::object::with_object;
use ferrokit_core::timer::Timer;
use std::cell::RefCell;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[test]
fn started_timer_fires_through_safe_api() {
    let el = EventLoop::new();
    let count = Arc::new(AtomicU32::new(0));
    let c = Arc::clone(&count);

    let timer = Timer::new();
    timer.set_interval(20);
    timer.set_single_shot(true);
    timer.timeout.connect(move |()| {
        c.fetch_add(1, Ordering::SeqCst);
    });
    timer.start();

    let deadline = Instant::now() + Duration::from_millis(1000);
    while count.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
        el.process_events(false);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(
        count.load(Ordering::SeqCst),
        1,
        "started timer never delivered timeout"
    );
}

#[test]
fn dropping_a_started_timer_unregisters_it() {
    let _el = EventLoop::new();
    let timer = Timer::new();
    timer.set_interval(10);
    timer.start();
    let id = timer.object_id();
    assert!(
        with_object(id, |_| ()).is_some(),
        "a started timer must be registered"
    );

    drop(timer);
    assert!(
        with_object(id, |_| ()).is_none(),
        "dropping a started timer must unregister it"
    );
}

thread_local! {
    static HELD: RefCell<Option<Timer>> = const { RefCell::new(None) };
}

#[test]
fn dropping_the_timer_from_its_own_slot_stops_further_ticks() {
    let el = EventLoop::new();
    let ticks = Arc::new(AtomicU32::new(0));
    let t = Arc::clone(&ticks);

    let timer = Timer::new();
    timer.set_interval(10);
    timer.timeout.connect(move |()| {
        t.fetch_add(1, Ordering::SeqCst);
        drop(HELD.with(|h| h.borrow_mut().take()));
    });
    let id = timer.object_id();
    timer.start();
    HELD.with(|h| *h.borrow_mut() = Some(timer));

    let deadline = Instant::now() + Duration::from_millis(300);
    while Instant::now() < deadline {
        el.process_events(false);
        std::thread::sleep(Duration::from_millis(2));
    }

    assert_eq!(
        ticks.load(Ordering::SeqCst),
        1,
        "a timer dropped by its own slot must not tick again"
    );
    assert!(
        with_object(id, |_| ()).is_none(),
        "the dropped timer is still registered"
    );
    assert_eq!(
        el.timer_registry().lock().unwrap().len(),
        0,
        "the dropped timer is still scheduled"
    );
}
