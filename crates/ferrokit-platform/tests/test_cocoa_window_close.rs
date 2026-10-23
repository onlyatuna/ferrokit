// SPDX-License-Identifier: LGPL-3.0-only
//! A user close (the title bar's close button) must not free the NSWindow that
//! `CocoaNativeWindow` still owns: `Drop` would then message freed memory. Runs against the mock
//! runtime; AppKit's `releasedWhenClosed` defaults to YES there as on macOS.
#![cfg(not(target_os = "macos"))]

use ferrokit_core::object::ThreadContext;
use ferrokit_gui::geometry::primitives::Rect;
use ferrokit_platform::{CocoaNativeWindow, MockObjcRuntime, ObjcMsg, Sel, WindowFlags};

fn user_close_then_drop(close: impl FnOnce(&CocoaNativeWindow)) {
    ThreadContext::init_current(true, None);
    let mock = MockObjcRuntime::instance();
    let win = CocoaNativeWindow::new("close", Rect::new(0, 0, 200, 100), WindowFlags::empty())
        .expect("window");
    let ns_window = win.ns_window();

    close(&win);
    let data = mock
        .get_object_data(ns_window)
        .expect("the user close leaves the window alive: ferrokit still owns it");
    assert_eq!(data.retain_count, 1);
    assert!(!data.is_visible);

    drop(win);
    assert!(
        mock.get_object_data(ns_window).is_none(),
        "Drop frees the window"
    );
    assert!(
        !mock.was_over_released(ns_window),
        "Drop must not release an already freed window"
    );
}

#[test]
fn perform_close_then_drop_does_not_over_release() {
    user_close_then_drop(|win| unsafe {
        ObjcMsg::send_id(
            win.ns_window(),
            Sel::register("performClose:"),
            win.ns_window(),
        );
    });
}

#[test]
fn close_then_drop_does_not_over_release() {
    user_close_then_drop(|win| unsafe {
        ObjcMsg::send_0(win.ns_window(), Sel::register("close"));
    });
}
