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
    let (ran_tx, ran_rx) = channel::<()>();
    let ran_tx = std::sync::Mutex::new(ran_tx);
    let handle = Thread::spawn_with_event_loop(move |sender| {
        let task = Box::new(move |_: &mut dyn std::any::Any| {
            ran_in_setup.store(true, Ordering::SeqCst);
            let _ = ran_tx.lock().unwrap().send(());
        });
        sender.post_event(ObjectId(0), Event::new(EventKind::MetaCall(task)));
    });
    ran_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("the posted event did not run");
    handle.quit();
    let code = with_watchdog(5, move || handle.join());
    assert_eq!(code.unwrap(), 0);
    assert!(ran.load(Ordering::SeqCst));
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
        // SAFETY: `GetCurrentThread()` returns the calling thread's pseudo handle, which is
        // always valid for `GetThreadPriority` on this thread (Win32 docs). It takes no pointers.
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
            // SAFETY: `GetCurrentThread()` returns the calling thread's pseudo handle, which
            // always has the access `SetThreadPriority` needs (Win32 docs). The call takes no
            // pointers.
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
        // SAFETY: `pthread_self()` is the calling thread's own handle, so it is valid. `policy`
        // and `param` are locals that outlive the call and are valid for the writes the call
        // makes. A zeroed `sched_param` is a valid value: the libc struct holds only integers
        // [inferred from the libc definition].
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

#[test]
fn exit_ends_nested_event_loops_too() {
    use ferrokit_core::event_loop::EventLoop;

    let (entered_tx, entered_rx) = channel::<()>();
    let entered_tx = std::sync::Mutex::new(entered_tx);
    let handle = Thread::spawn_with_event_loop(move |sender| {
        let task = Box::new(move |_: &mut dyn std::any::Any| {
            let inner = EventLoop::new();
            let _ = entered_tx.lock().unwrap().send(());
            inner.exec();
        });
        sender.post_event(ObjectId(0), Event::new(EventKind::MetaCall(task)));
    });
    entered_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("nested loop did not start");
    handle.exit(5);
    let code = with_watchdog(10, move || handle.join());
    assert_eq!(code.unwrap(), 5);
}

/// The race behind a flake of `exit_ends_nested_event_loops_too`: `exit` lands after an event
/// has decided to run a nested loop but before that loop is running, so only the outer loop is
/// told. The nested loop must not run on (and hold the outer one forever): like `QEventLoop::exec`
/// while `quitNow` is set (qeventloop.cpp:140-143), it returns -1 at once.
#[test]
fn a_nested_loop_entered_after_exit_returns_at_once() {
    use ferrokit_core::event_loop::EventLoop;

    let (about_to_enter_tx, about_to_enter_rx) = channel::<()>();
    let (exited_tx, exited_rx) = channel::<()>();
    let (inner_code_tx, inner_code_rx) = channel::<i32>();
    let channels = std::sync::Mutex::new((about_to_enter_tx, exited_rx, inner_code_tx));
    let handle = Thread::spawn_with_event_loop(move |sender| {
        let task = Box::new(move |_: &mut dyn std::any::Any| {
            let (about_to_enter, exited, inner_code) = &*channels.lock().unwrap();
            let inner = EventLoop::new();
            let _ = about_to_enter.send(());
            // `exit` is called now, before the nested loop runs.
            let _ = exited.recv_timeout(Duration::from_secs(10));
            let _ = inner_code.send(inner.exec());
        });
        sender.post_event(ObjectId(0), Event::new(EventKind::MetaCall(task)));
    });
    about_to_enter_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("the nested loop was not about to start");
    handle.exit(5);
    exited_tx.send(()).unwrap();
    let code = with_watchdog(10, move || handle.join());
    assert_eq!(code.unwrap(), 5);
    assert_eq!(inner_code_rx.recv_timeout(Duration::ZERO), Ok(-1));
}

/// The flag that makes a nested loop return at once after an exit is cleared when the exited
/// loops have returned: a later `exec` on the same thread delivers events and runs until its own
/// exit.
#[test]
fn a_loop_run_after_the_exited_loops_returned_runs_normally() {
    use ferrokit_core::event_loop::EventLoop;

    let codes = with_watchdog(10, || {
        let outer = EventLoop::new();
        let handle = outer.handle();
        let nested = Arc::new(std::sync::Mutex::new(None));
        let sink = Arc::clone(&nested);
        outer.post_event(
            ObjectId(0),
            Event::new(EventKind::MetaCallNoTarget(Box::new(move || {
                handle.exit(3);
                // Entered while the outer loop is being ended: returns at once.
                *sink.lock().unwrap() = Some(EventLoop::new().exec());
            }))),
        );
        let outer_code = outer.exec();

        // Same thread, after the exited loop returned: runs and delivers its event.
        let later = EventLoop::new();
        let ran = Arc::new(AtomicBool::new(false));
        let ran_in_event = Arc::clone(&ran);
        let later_handle = later.handle();
        later.post_event(
            ObjectId(0),
            Event::new(EventKind::MetaCallNoTarget(Box::new(move || {
                ran_in_event.store(true, Ordering::SeqCst);
                later_handle.exit(8);
            }))),
        );
        let later_code = later.exec();
        let nested_code = nested.lock().unwrap().take();
        (
            outer_code,
            nested_code,
            later_code,
            ran.load(Ordering::SeqCst),
        )
    });
    assert_eq!(codes, (3, Some(-1), 8, true));
}

mod swallowed_exit {
    use std::cell::RefCell;

    use ferrokit_core::event::{Event, EventKind};
    use ferrokit_core::event_loop::install_application_event_filter;
    use ferrokit_core::object::{
        register_owned_qobject, ObjectData, ObjectDataMut, ObjectId, OwnedObject, QObject,
    };

    /// Consumes the posted calls after the first `pass` of them, as an application filter on
    /// the thread may do.
    struct SwallowCalls {
        data: ObjectData,
        pass: u32,
    }

    impl QObject for SwallowCalls {
        fn object_data(&self) -> &ObjectData {
            &self.data
        }
        fn object_data_mut(&mut self) -> ObjectDataMut<'_> {
            ObjectDataMut::new(&mut self.data)
        }
        fn event_filter(&mut self, _watched: ObjectId, event: &mut Event) -> bool {
            if !matches!(event.kind, EventKind::MetaCall(_)) {
                return false;
            }
            match self.pass.checked_sub(1) {
                Some(rest) => {
                    self.pass = rest;
                    false
                }
                None => true,
            }
        }
    }

    thread_local! {
        /// Dropped with the thread, which unregisters the filter.
        static FILTER: RefCell<Option<OwnedObject<SwallowCalls>>> = const { RefCell::new(None) };
    }

    /// Installs the filter on the calling thread.
    pub fn install_on_this_thread(pass: u32) {
        let data = ObjectData::new(ObjectId::next());
        let id = data.id();
        // SAFETY: `register_owned_qobject` needs no access to the object while a callback of it
        // runs. The owner is moved into the `FILTER` thread-local right below and never touched
        // again, so nothing reads the object directly; the registry reaches it only from this
        // thread, which registered it, and `ObjectData::new` does not opt in to thread moves.
        // The thread-local is dropped at thread exit on this thread, after the loop has ended, so
        // no callback is running then.
        let filter = unsafe { register_owned_qobject(SwallowCalls { data, pass }) };
        install_application_event_filter(id);
        FILTER.with(|slot| *slot.borrow_mut() = Some(filter));
    }
}

/// `QThread::exit` ends the loops directly (qthread.cpp:775-788); it does not depend on an event
/// that the thread's filters could consume.
#[test]
fn exit_is_not_swallowed_by_an_application_event_filter() {
    let handle = Thread::spawn_with_event_loop(|_| swallowed_exit::install_on_this_thread(0));
    handle.exit(7);
    let code = with_watchdog(10, move || handle.join());
    assert_eq!(code.unwrap(), 7);
}

/// Same, for a thread that is already inside its loop when `exit` is called.
#[test]
fn exit_reaches_a_running_loop_behind_an_application_event_filter() {
    let (running_tx, running_rx) = channel::<()>();
    let running_tx = std::sync::Mutex::new(running_tx);
    let handle = Thread::spawn_with_event_loop(move |sender| {
        // The filter lets this one call through; it proves the loop is running.
        swallowed_exit::install_on_this_thread(1);
        let running = Box::new(move |_: &mut dyn std::any::Any| {
            let _ = running_tx.lock().unwrap().send(());
        });
        sender.post_event(ObjectId(0), Event::new(EventKind::MetaCall(running)));
    });
    running_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("event loop thread did not run its loop");
    handle.exit(3);
    let code = with_watchdog(10, move || handle.join());
    assert_eq!(code.unwrap(), 3);
}

/// A panic out of the thread's setup must not leave the thread's sender registered.
#[test]
fn event_loop_thread_that_panics_clears_its_sender_registration() {
    use ferrokit_core::object::query_thread_sender;
    use std::cell::RefCell;
    use std::sync::Mutex;

    thread_local! {
        /// Dropped when the thread's locals are destroyed, which is after its body unwound.
        static ENDED: RefCell<Option<std::sync::mpsc::Sender<()>>> = const { RefCell::new(None) };
    }

    let (ended_tx, ended_rx) = channel::<()>();
    let thread_id = Arc::new(Mutex::new(None));
    let recorded = Arc::clone(&thread_id);
    let spawned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        Thread::spawn_with_event_loop(move |_| {
            ENDED.with(|slot| *slot.borrow_mut() = Some(ended_tx));
            *recorded.lock().unwrap() = Some(ThreadId::current());
            panic!("setup failed");
        })
    }));
    assert!(spawned.is_err(), "the spawn must report the failed setup");

    // The sender is dropped, disconnecting the channel, when the thread's locals are destroyed.
    assert_eq!(
        ended_rx.recv_timeout(Duration::from_secs(10)),
        Err(RecvTimeoutError::Disconnected),
        "the thread did not end"
    );
    let id = thread_id.lock().unwrap().expect("setup did not run");
    assert!(
        query_thread_sender(id).is_none(),
        "the sender stayed registered for a thread that has ended"
    );
}
