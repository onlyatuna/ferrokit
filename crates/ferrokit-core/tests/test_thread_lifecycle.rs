// SPDX-License-Identifier: LGPL-3.0-only
//! Thread lifecycle: completion state, quit vs. interruption, exit codes, the
//! `started`/`finished` signals, and requested priority. Every blocking call goes through
//! `with_watchdog` or a `recv_timeout`, so a regression fails instead of hanging.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::object::{ObjectId, ThreadId};
use ferrokit_core::thread::*;

/// Runs `op` on a helper thread and waits at most `secs` for its result.
fn with_watchdog<T: Send + 'static>(secs: u64, op: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let _ = tx.send(op());
    });
    match rx.recv_timeout(Duration::from_secs(secs)) {
        Ok(value) => value,
        Err(RecvTimeoutError::Timeout) => panic!("watchdog: operation did not finish in {secs}s"),
        Err(RecvTimeoutError::Disconnected) => panic!("watchdog: operation panicked"),
    }
}

#[test]
fn thread_handle_is_finished_true_after_thread_returns_without_join() {
    let handle = Thread::spawn(|| 7u32);

    let deadline = Instant::now() + Duration::from_secs(5);
    while !handle.is_finished() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        handle.is_finished(),
        "is_finished stayed false after the thread body returned, without join"
    );
    assert_eq!(with_watchdog(5, move || handle.join()).unwrap(), 7);
}

#[test]
fn event_loop_thread_quit_does_not_set_interruption() {
    let handle = Thread::spawn_with_event_loop(|_| {});
    handle.quit();
    let interrupted = handle.is_interruption_requested();
    let code = with_watchdog(5, move || handle.join());
    assert!(
        !interrupted,
        "quit() must not request interruption (qthread.cpp:777-778)"
    );
    assert_eq!(code.unwrap(), 0);
}

#[test]
fn quit_ends_event_loop_after_queued_events_run() {
    let ran = Arc::new(AtomicBool::new(false));
    let ran_in_setup = Arc::clone(&ran);
    let handle = Thread::spawn_with_event_loop(move |sender| {
        let task = Box::new(move |_: &mut dyn std::any::Any| {
            ran_in_setup.store(true, Ordering::SeqCst);
        });
        sender.post_event(ObjectId(0), Event::new(EventKind::MetaCall(task)));
    });
    handle.quit();
    let code = with_watchdog(5, move || handle.join());
    assert_eq!(code.unwrap(), 0);
    assert!(
        ran.load(Ordering::SeqCst),
        "event posted before quit() did not run before the loop exited"
    );
}

#[test]
fn exit_code_is_returned_by_join() {
    let handle = Thread::spawn_with_event_loop(|_| {});
    handle.exit(7);
    let code = with_watchdog(5, move || handle.join());
    assert_eq!(
        code.unwrap(),
        7,
        "exit(7) must make the event loop return 7"
    );
}
#[test]
fn event_loop_thread_handle_drop_quits_and_joins() {
    let ran = Arc::new(AtomicBool::new(false));
    let ran_clone = Arc::clone(&ran);
    {
        let _handle = Thread::spawn_with_event_loop(move |_sender| {
            ran_clone.store(true, Ordering::SeqCst);
        });
        // handle dropped here without explicit join
    }
    assert!(
        ran.load(Ordering::SeqCst),
        "Dropping EventLoopThreadHandle must quit and join rather than detach"
    );
}

#[test]
fn wait_is_false_while_body_runs_and_true_after_return() {
    let (gate_tx, gate_rx) = channel::<()>();
    let handle = Thread::spawn(move || {
        let _ = gate_rx.recv_timeout(Duration::from_secs(5));
    });

    assert!(
        handle.is_running(),
        "a thread with a running body must report is_running"
    );
    assert!(
        !handle.wait(Duration::from_millis(50)),
        "wait returned true while the body was still blocked"
    );
    assert!(!handle.is_finished());

    gate_tx.send(()).expect("body gate closed early");
    assert!(
        handle.wait(Duration::from_secs(5)),
        "wait did not return true after the body returned"
    );
    assert!(handle.is_finished());
    assert!(!handle.is_running());
    with_watchdog(5, move || handle.join()).unwrap();
}

#[test]
fn finished_signal_is_emitted_on_worker_thread() {
    let (gate_tx, gate_rx) = channel::<()>();
    let handle = Thread::spawn(move || {
        let _ = gate_rx.recv_timeout(Duration::from_secs(5));
    });

    let (seen_tx, seen_rx) = channel::<ThreadId>();
    let _connection = handle.finished().connect(move |_: &()| {
        let _ = seen_tx.send(Thread::current_id());
    });
    gate_tx.send(()).expect("body gate closed early");

    let emitted_on = seen_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("finished was not emitted after the body returned");
    assert_eq!(
        emitted_on,
        handle.id(),
        "finished must be emitted on the thread that ran the body (qthread_unix.cpp:489)"
    );
    with_watchdog(5, move || handle.join()).unwrap();
}

#[test]
fn finished_signal_is_emitted_and_wait_returns_when_body_panics() {
    let (gate_tx, gate_rx) = channel::<()>();
    let handle = Thread::spawn(move || -> u32 {
        let _ = gate_rx.recv_timeout(Duration::from_secs(5));
        panic!("expected test panic")
    });

    let (seen_tx, seen_rx) = channel::<()>();
    let _connection = handle.finished().connect(move |_: &()| {
        let _ = seen_tx.send(());
    });
    gate_tx.send(()).expect("body gate closed early");

    seen_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("finished was not emitted when the body panicked");
    assert!(
        handle.wait(Duration::from_secs(5)),
        "wait did not return true after the body panicked"
    );
    assert!(handle.is_finished());
    assert!(with_watchdog(5, move || handle.join()).is_err());
}

#[cfg(windows)]
mod windows_priority {
    use super::*;
    use windows_sys::Win32::System::Threading::{
        GetCurrentThread, GetThreadPriority, SetThreadPriority, THREAD_PRIORITY_HIGHEST,
    };

    fn current_thread_priority() -> i32 {
        unsafe { GetThreadPriority(GetCurrentThread()) }
    }

    #[test]
    fn priority_highest_is_applied_before_body() {
        let handle = Thread::builder()
            .priority(ThreadPriority::Highest)
            .spawn(current_thread_priority)
            .expect("spawn failed");
        let level = with_watchdog(5, move || handle.join()).unwrap();
        assert_eq!(
            level, THREAD_PRIORITY_HIGHEST,
            "requested Highest was not applied"
        );
    }

    #[test]
    fn inherit_copies_creator_priority() {
        let handle = Thread::spawn(|| {
            unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST) };
            // No `.priority(..)` call: the default `Inherit` takes the creator's level.
            let child = Thread::spawn(current_thread_priority);
            child.join().expect("child panicked")
        });
        let level = with_watchdog(5, move || handle.join()).unwrap();
        assert_eq!(
            level, THREAD_PRIORITY_HIGHEST,
            "Inherit must copy the creator's priority (qthread_win.cpp:439-442)"
        );
    }
}

#[cfg(all(unix, target_os = "linux"))]
#[test]
fn idle_priority_sets_sched_idle() {
    fn current_policy() -> i32 {
        unsafe {
            let mut policy = 0;
            let mut param: libc::sched_param = std::mem::zeroed();
            libc::pthread_getschedparam(libc::pthread_self(), &mut policy, &mut param);
            policy
        }
    }

    let handle = Thread::builder()
        .priority(ThreadPriority::Idle)
        .spawn(current_policy)
        .expect("spawn failed");
    let policy = with_watchdog(5, move || handle.join()).unwrap();
    assert_eq!(
        policy,
        libc::SCHED_IDLE,
        "Idle must run as SCHED_IDLE (qthread_unix.cpp:729-767)"
    );
}
