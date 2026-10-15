// SPDX-License-Identifier: LGPL-3.0-only
//! `Timer::single_shot(ms > 0)` armed on a thread that has no event loop yet.
//!
//! Qt registers a single-shot timer on the thread's existing dispatcher (`qsingleshottimer.cpp:15-59`)
//! and keeps it until it fires; the arm is never dropped. The timer must therefore fire once a loop
//! runs on that thread. Each test uses a fresh thread, so the "no loop yet" state is exact.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use ferrokit_core::event_loop::EventLoop;
use ferrokit_core::timer::Timer;

#[test]
fn single_shot_ms_before_loop_fires_after_loop_starts() {
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let (done_tx, done_rx) = mpsc::channel::<usize>();
    let fired = Arc::new(AtomicUsize::new(0));
    let fired_worker = Arc::clone(&fired);
    let join = std::thread::spawn(move || {
        // Armed BEFORE this thread has an EventLoop.
        let f = Arc::clone(&fired_worker);
        Timer::single_shot(30, move || {
            f.fetch_add(1, Ordering::SeqCst);
        });
        go_rx.recv().unwrap();
        let el = EventLoop::new();
        let deadline = Instant::now() + Duration::from_millis(1000);
        while Instant::now() < deadline && fired_worker.load(Ordering::SeqCst) == 0 {
            el.process_events(false);
            std::thread::sleep(Duration::from_millis(5));
        }
        // Keep pumping briefly: a second delivery would show up here.
        let settle = Instant::now() + Duration::from_millis(100);
        while Instant::now() < settle {
            el.process_events(false);
            std::thread::sleep(Duration::from_millis(5));
        }
        done_tx.send(fired_worker.load(Ordering::SeqCst)).unwrap();
    });
    go_tx.send(()).unwrap();
    // Watchdog: the worker must report even if the timer never fires.
    let count = done_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("watchdog: worker thread did not finish its loop");
    join.join().unwrap();
    assert_eq!(
        count, 1,
        "single_shot(30) armed before the loop must fire exactly once after the loop runs"
    );
}
