// SPDX-License-Identifier: LGPL-3.0-only
#![cfg(windows)]
//! `Timer::start` is safe because a `Timer` is only reachable as `Pin<Box<Timer>>`. These tests
//! use the public API with no `unsafe` at the call site.

use ferrokit_core::event_loop::EventLoop;
use ferrokit_core::object::with_object;
use ferrokit_core::timer::Timer;
use ferrokit_core::QObject;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[test]
fn started_timer_fires_through_safe_api() {
    let el = EventLoop::new();
    let count = Arc::new(AtomicU32::new(0));
    let c = Arc::clone(&count);

    let mut timer = Timer::new();
    timer.set_interval(20);
    timer.set_single_shot(true);
    timer.timeout.connect(move |()| {
        c.fetch_add(1, Ordering::SeqCst);
    });
    timer.as_mut().start();

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
fn dropping_a_started_timer_unregisters_its_address() {
    let _el = EventLoop::new();
    let mut timer = Timer::new();
    timer.set_interval(10);
    timer.as_mut().start();
    let id = timer.object_data().id();
    assert!(
        with_object(id, |_| ()).is_some(),
        "a started timer must be registered"
    );

    drop(timer);
    assert!(
        with_object(id, |_| ()).is_none(),
        "dropping a started timer must unregister it before its memory is freed"
    );
}
