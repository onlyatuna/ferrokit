// SPDX-License-Identifier: LGPL-3.0-only
//! `QCoreApplication::exit` called from a worker thread must end the application thread's `exec`
//! with the requested return code. Qt exits every loop in the application thread's `QThreadData`
//! (qcoreapplication.cpp:1534-1541). The worker is spawned from a timer inside `exec`, so the loop
//! is running before the cross-thread exit is issued.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ferrokit_core::application::CoreApplication;
use ferrokit_core::timer::Timer;

/// Aborts the test process when the body hangs, so a regression fails instead of hanging CI.
struct Watchdog {
    done: Arc<AtomicBool>,
}

impl Watchdog {
    fn arm(limit: Duration) -> Self {
        let done = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&done);
        std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + limit;
            while std::time::Instant::now() < deadline {
                if flag.load(Ordering::SeqCst) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            // Written to the raw stderr so libtest output capture cannot swallow it on exit.
            let _ = std::io::Write::write_all(
                &mut std::io::stderr(),
                format!("watchdog: test body exceeded {limit:?}\n").as_bytes(),
            );
            std::process::exit(101);
        });
        Self { done }
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        self.done.store(true, Ordering::SeqCst);
    }
}

#[test]
fn exit_from_worker_thread_ends_exec_with_requested_code() {
    let _watchdog = Watchdog::arm(Duration::from_secs(15));
    let mut app = CoreApplication::new(vec!["test".to_string()]);

    // Backstop: if the worker's exit is ignored, this fires on the app thread and records it.
    let backstop_fired = Arc::new(AtomicBool::new(false));
    let backstop_flag = Arc::clone(&backstop_fired);
    Timer::single_shot(5_000, move || {
        backstop_flag.store(true, Ordering::SeqCst);
        CoreApplication::exit(99);
    });

    // Spawned from a zero-delay timer that runs inside exec, so the exit is issued while the
    // application thread's loop is running.
    Timer::single_shot(0, || {
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_millis(50));
            CoreApplication::exit(7);
        });
    });

    let code = app.exec();
    assert!(
        !backstop_fired.load(Ordering::SeqCst),
        "backstop fired: exit() from a worker thread did not end exec"
    );
    assert_eq!(code, 7, "exit(7) from a worker thread must end exec with 7");
}

#[test]
fn exit_from_worker_thread_emits_about_to_quit() {
    let _watchdog = Watchdog::arm(Duration::from_secs(15));
    let mut app = CoreApplication::new(vec!["test".to_string()]);
    let emitted = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&emitted);
    app.about_to_quit()
        .connect(move |_| flag.store(true, Ordering::SeqCst));

    Timer::single_shot(0, || {
        std::thread::spawn(|| CoreApplication::exit(3));
    });
    assert_eq!(app.exec(), 3);
    assert!(
        emitted.load(Ordering::SeqCst),
        "aboutToQuit must be emitted for exit() from a worker"
    );
}
