// SPDX-License-Identifier: LGPL-3.0-only
//! A `WM_QUIT` seen by a nested loop is `QCoreApplication::quit` in Qt
//! (`QEventDispatcherWin32::processEvents`): it ends every loop of the thread. The message is
//! consumed by the inner loop, so an outer loop that is not ended with it would run forever.
#![cfg(windows)]

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::event_loop::EventLoop;
use ferrokit_core::object::ObjectId;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Arc;
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
            // SAFETY: posts WM_QUIT to this thread's own queue.
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
