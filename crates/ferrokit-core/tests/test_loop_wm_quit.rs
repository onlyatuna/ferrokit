// SPDX-License-Identifier: LGPL-3.0-only
//! A `WM_QUIT` seen by a nested loop is `QCoreApplication::quit` in Qt
//! (`QEventDispatcherWin32::processEvents`): it ends every loop of the thread. The message is
//! consumed by the inner loop, so an outer loop that is not ended with it would run forever.
#![cfg(windows)]

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::event_loop::EventLoop;
use ferrokit_core::object::ObjectId;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::time::Duration;
use windows_sys::Win32::UI::WindowsAndMessaging::PostQuitMessage;

#[test]
fn wm_quit_in_a_nested_loop_ends_the_outer_loop_too() {
    let outer = EventLoop::new();
    let inner_code = Arc::new(AtomicI32::new(i32::MIN));
    let sink = Arc::clone(&inner_code);
    outer.post_event(
        ObjectId(0),
        Event::new(EventKind::MetaCallNoTarget(Box::new(move || {
            let inner = EventLoop::new();
            // SAFETY: `PostQuitMessage` takes only an integer exit code and no pointer, so it has
            // no memory precondition; it posts `WM_QUIT` to the calling thread's queue, which is
            // this (the loop) thread, so the nested `inner.exec()` on the next line consumes it.
            unsafe { PostQuitMessage(7) };
            sink.store(inner.exec(), Ordering::SeqCst);
        }))),
    );

    // Without the fix the outer loop never ends; this only turns that hang into a failure.
    let handle = outer.handle();
    let watchdog = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(20));
        handle.exit(-1);
    });

    let outer_code = outer.exec();
    assert_eq!(
        inner_code.load(Ordering::SeqCst),
        7,
        "the inner loop ends with the code"
    );
    assert_eq!(outer_code, 7, "the outer loop ends with it too");
    drop(watchdog);
}

/// After a nested loop consumed a `WM_QUIT`, a loop entered before the outer one returns (from
/// the same event) returns -1 at once, as `QEventLoop::exec` does while `quitNow` is set: it must
/// not run on and hold the outer loop.
#[test]
fn a_loop_entered_after_a_consumed_wm_quit_returns_at_once() {
    let outer = EventLoop::new();
    let codes = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = Arc::clone(&codes);
    outer.post_event(
        ObjectId(0),
        Event::new(EventKind::MetaCallNoTarget(Box::new(move || {
            let first = EventLoop::new();
            // SAFETY: as in the test above: `PostQuitMessage` takes only an integer and posts to
            // this thread's queue, which `first.exec()` consumes.
            unsafe { PostQuitMessage(7) };
            let first_code = first.exec();
            let second = EventLoop::new();
            let second_code = second.exec();
            sink.lock().unwrap().extend([first_code, second_code]);
        }))),
    );

    // Without the fix the second loop runs until this fires.
    let fired = Arc::new(AtomicBool::new(false));
    let watchdog = {
        let handle = outer.handle();
        let fired = Arc::clone(&fired);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(10));
            fired.store(true, Ordering::SeqCst);
            handle.exit(-2);
        })
    };

    let outer_code = outer.exec();
    assert!(!fired.load(Ordering::SeqCst), "the second loop ran on");
    assert_eq!(*codes.lock().unwrap(), vec![7, -1]);
    assert_eq!(outer_code, 7);
    drop(watchdog);
}
