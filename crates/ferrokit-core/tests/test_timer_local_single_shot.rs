// SPDX-License-Identifier: LGPL-3.0-only
//! `Timer::single_shot_local`: a single-shot functor that is not `Send`.
//!
//! The closure stays in the calling thread's table; the timer or the posted event carries only a
//! key. Every test runs on a fresh thread, so "no loop yet" and "loop ended" states are exact, and
//! no assertion runs inside a `Drop` (a failing one there would abort the process); a `Drop`
//! records the thread it ran on and the test thread checks it.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::thread::ThreadId;
use std::time::{Duration, Instant};

use ferrokit_core::event_loop::EventLoop;
use ferrokit_core::timer::Timer;

/// Records the thread that drops it.
struct DropProbe {
    dropped_on: Arc<Mutex<Option<ThreadId>>>,
    // `Rc` makes the probe, and so the closure that captures it, `!Send`.
    _not_send: Rc<()>,
}

impl Drop for DropProbe {
    fn drop(&mut self) {
        *self.dropped_on.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(std::thread::current().id());
    }
}

fn pump_until(el: &EventLoop, limit: Duration, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline && !done() {
        el.process_events(false);
        std::thread::yield_now();
    }
}

fn on_fresh_thread<R: Send + 'static>(f: impl FnOnce() -> R + Send + 'static) -> R {
    std::thread::spawn(f)
        .join()
        .expect("worker thread panicked")
}

#[test]
fn a_non_send_capture_runs_once_on_the_owner_thread_for_zero_ms() {
    let (runs, ran_on) = on_fresh_thread(|| {
        let el = EventLoop::new();
        let count = Rc::new(Cell::new(0u32));
        let ran_on = Rc::new(Cell::new(None));
        let (c, r) = (Rc::clone(&count), Rc::clone(&ran_on));
        Timer::single_shot_local(0, move || {
            c.set(c.get() + 1);
            r.set(Some(std::thread::current().id()));
        });
        assert_eq!(count.get(), 0, "0 ms is queued, not run inside the call");
        pump_until(&el, Duration::from_secs(5), || count.get() > 0);
        for _ in 0..20 {
            el.process_events(false);
        }
        (
            count.get(),
            ran_on.get().map(|id| id == std::thread::current().id()),
        )
    });
    assert_eq!(runs, 1);
    assert_eq!(ran_on, Some(true));
}

#[test]
fn a_non_send_capture_runs_once_on_the_owner_thread_for_a_non_zero_interval() {
    let (runs, ran_on) = on_fresh_thread(|| {
        let el = EventLoop::new();
        let count = Rc::new(Cell::new(0u32));
        let ran_on = Rc::new(Cell::new(None));
        let (c, r) = (Rc::clone(&count), Rc::clone(&ran_on));
        Timer::single_shot_local(20, move || {
            c.set(c.get() + 1);
            r.set(Some(std::thread::current().id()));
        });
        pump_until(&el, Duration::from_secs(5), || count.get() > 0);
        // A second delivery would show up while the loop keeps running.
        let settle = Instant::now() + Duration::from_millis(100);
        while Instant::now() < settle {
            el.process_events(false);
            std::thread::yield_now();
        }
        (
            count.get(),
            ran_on.get().map(|id| id == std::thread::current().id()),
        )
    });
    assert_eq!(runs, 1);
    assert_eq!(ran_on, Some(true));
}

#[test]
fn a_timer_armed_before_the_thread_has_a_loop_fires_once_the_loop_exists() {
    let runs = on_fresh_thread(|| {
        let count = Rc::new(Cell::new(0u32));
        let c = Rc::clone(&count);
        // No `EventLoop` on this thread yet.
        Timer::single_shot_local(20, move || c.set(c.get() + 1));
        assert_eq!(
            count.get(),
            0,
            "the closure must not run before a loop exists"
        );
        let el = EventLoop::new();
        pump_until(&el, Duration::from_secs(5), || count.get() > 0);
        count.get()
    });
    assert_eq!(runs, 1);
}

#[test]
fn a_zero_ms_call_before_the_thread_has_a_loop_runs_once_the_loop_exists() {
    let runs = on_fresh_thread(|| {
        let count = Rc::new(Cell::new(0u32));
        let c = Rc::clone(&count);
        Timer::single_shot_local(0, move || c.set(c.get() + 1));
        assert_eq!(
            count.get(),
            0,
            "the closure must not run before a loop exists"
        );
        let el = EventLoop::new();
        pump_until(&el, Duration::from_secs(5), || count.get() > 0);
        count.get()
    });
    assert_eq!(runs, 1);
}

#[test]
fn an_armed_closure_that_never_fires_is_dropped_on_the_owner_thread() {
    let dropped_on = Arc::new(Mutex::new(None));
    let probe_slot = Arc::clone(&dropped_on);
    let owner = on_fresh_thread(move || {
        let el = EventLoop::new();
        let probe = DropProbe {
            dropped_on: probe_slot,
            _not_send: Rc::new(()),
        };
        // Far in the future: the loop and the thread end first.
        Timer::single_shot_local(3_600_000, move || drop(probe));
        el.process_events(false);
        drop(el);
        std::thread::current().id()
    });
    let seen = *dropped_on.lock().unwrap();
    assert_eq!(
        seen,
        Some(owner),
        "the closure is destroyed on the owner thread, unrun"
    );
}

#[test]
fn a_queued_zero_ms_closure_that_never_runs_is_dropped_on_the_owner_thread() {
    let dropped_on = Arc::new(Mutex::new(None));
    let probe_slot = Arc::clone(&dropped_on);
    let owner = on_fresh_thread(move || {
        let probe = DropProbe {
            dropped_on: probe_slot,
            _not_send: Rc::new(()),
        };
        // Queued, and the thread ends without ever running a loop.
        Timer::single_shot_local(0, move || drop(probe));
        std::thread::current().id()
    });
    let seen = *dropped_on.lock().unwrap();
    assert_eq!(
        seen,
        Some(owner),
        "the closure is destroyed on the owner thread, unrun"
    );
}

#[test]
fn a_panic_in_the_closure_is_contained_and_later_closures_still_run() {
    let (zero, armed) = on_fresh_thread(|| {
        let el = EventLoop::new();
        let zero = Rc::new(Cell::new(0u32));
        let armed = Rc::new(Cell::new(0u32));
        Timer::single_shot_local(0, || {
            panic!("expected panic in a zero-ms local single shot")
        });
        let z = Rc::clone(&zero);
        Timer::single_shot_local(0, move || z.set(z.get() + 1));
        pump_until(&el, Duration::from_secs(5), || zero.get() > 0);

        Timer::single_shot_local(10, || {
            panic!("expected panic in an armed local single shot")
        });
        let a = Rc::clone(&armed);
        Timer::single_shot_local(30, move || a.set(a.get() + 1));
        pump_until(&el, Duration::from_secs(5), || armed.get() > 0);
        (zero.get(), armed.get())
    });
    assert_eq!((zero, armed), (1, 1));
}

#[test]
fn a_queued_zero_ms_event_dropped_on_another_thread_still_drops_the_closure_on_the_owner() {
    let dropped_on = Arc::new(Mutex::new(None));
    let probe_slot = Arc::clone(&dropped_on);
    let owner = on_fresh_thread(move || {
        let el = EventLoop::new();
        let probe = DropProbe {
            dropped_on: probe_slot,
            _not_send: Rc::new(()),
        };
        Timer::single_shot_local(0, move || drop(probe));
        let queue = Arc::clone(el.queue());
        std::thread::spawn(move || {
            let events = std::mem::take(&mut *queue.lock().unwrap());
            assert!(!events.is_empty());
            drop(events);
        })
        .join()
        .unwrap();
        assert!(el.process_events(false));
        std::thread::current().id()
    });
    assert_eq!(*dropped_on.lock().unwrap(), Some(owner));
}

#[test]
fn a_capture_whose_drop_arms_single_shot_at_thread_exit_does_not_panic() {
    struct ExitArmer {
        dropped_on: Arc<Mutex<Option<ThreadId>>>,
        _not_send: Rc<()>,
    }
    impl Drop for ExitArmer {
        fn drop(&mut self) {
            Timer::single_shot(10, || {});
            Timer::single_shot_local(10, || {});
            *self.dropped_on.lock().unwrap_or_else(|e| e.into_inner()) =
                Some(std::thread::current().id());
        }
    }

    let dropped_on = Arc::new(Mutex::new(None));
    let probe_slot = Arc::clone(&dropped_on);
    let owner = on_fresh_thread(move || {
        let el = EventLoop::new();
        let armer = ExitArmer {
            dropped_on: probe_slot,
            _not_send: Rc::new(()),
        };
        Timer::single_shot_local(3_600_000, move || drop(armer));
        drop(el);
        std::thread::current().id()
    });
    assert_eq!(*dropped_on.lock().unwrap(), Some(owner));
}
