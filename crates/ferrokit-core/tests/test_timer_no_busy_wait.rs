// SPDX-License-Identifier: LGPL-3.0-only
//! A loop waiting for a coarse timer sleeps until the timer's message arrives; it does not spin.

#![cfg(windows)]

use ferrokit_core::event_loop::EventLoop;
use ferrokit_core::timer::Timer;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// `SetTimer` posts its `WM_TIMER` up to one system clock tick (about 15 ms) after the deadline in
/// the registry. The loop used to return from every wait at once for a deadline that had passed,
/// so each of these fires cost tens of thousands of turns of the loop (about 9 ms of processor
/// time per fire at a 100 ms interval).
#[test]
fn waiting_for_a_coarse_timer_does_not_spin() {
    let fired = Arc::new(AtomicU32::new(0));
    let el = EventLoop::new();
    let timer = Timer::new();
    timer.set_interval(100);
    {
        let fired = Arc::clone(&fired);
        timer.timeout.connect(move |()| {
            fired.fetch_add(1, Ordering::Relaxed);
        });
    }
    timer.start();

    let start = Instant::now();
    let mut turns = 0u64;
    while fired.load(Ordering::Relaxed) < 5 {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "the timer did not fire five times"
        );
        el.process_events(true);
        turns += 1;
    }
    // A turn is one wake: a message, or the 1 ms the wait takes once the deadline has passed.
    // Five fires take a few dozen turns; the spin took about 50,000 per fire.
    assert!(
        turns < 2_000,
        "the loop took {turns} turns for five timer fires"
    );
}
