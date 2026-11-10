// SPDX-License-Identifier: LGPL-3.0-only
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ferrokit_core::event_loop::EventLoop;
use ferrokit_core::object::ObjectId;
use ferrokit_core::timer::Timer;

/// Timer and event loop exit test (with watchdog timeout)
#[test]
fn test_level3_timer_and_event_loop_exit() {
    let event_loop = EventLoop::new();
    let triggered = Arc::new(AtomicBool::new(false));
    let triggered_clone = Arc::clone(&triggered);

    // Watchdog timer to avoid hanging CI if test stalls
    let watchdog_handle = event_loop.handle();
    let watchdog = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(2000));
        watchdog_handle.post_quit(ObjectId(0), -1);
    });

    // Create 50ms single-shot timer
    let timer = Timer::new();
    timer.set_single_shot(true);
    timer.set_interval(50);

    let loop_handle = event_loop.handle();
    timer.timeout.connect(move |()| {
        triggered_clone.store(true, Ordering::SeqCst);
        loop_handle.post_quit(ObjectId(0), 0); // Quit event loop
    });

    timer.start();

    // Start event loop
    let exit_code = event_loop.exec();

    assert_eq!(exit_code, 0);
    assert!(
        triggered.load(Ordering::SeqCst),
        "Timer should trigger and exit event loop"
    );

    drop(watchdog);
}
