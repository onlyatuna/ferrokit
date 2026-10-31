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
    // SAFETY: mock runtime only (`cfg(not(target_os = "macos"))`): `win.ns_window()` is the live
    // handle of the window `win` owns, and `performClose:` takes one sender object, which is the
    // window itself, matching `send_id`.
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
    // SAFETY: mock runtime only: `win.ns_window()` is the live handle of the window `win` owns;
    // `close` takes no arguments, matching `send_0`.
    user_close_then_drop(|win| unsafe {
        ObjcMsg::send_0(win.ns_window(), Sel::register("close"));
    });
}

#[test]
fn perform_close_without_close_button_keeps_window_open() {
    ThreadContext::init_current(true, None);
    let mock = MockObjcRuntime::instance();
    let win = CocoaNativeWindow::new(
        "borderless",
        Rect::new(0, 0, 200, 100),
        WindowFlags::FRAMELESS,
    )
    .expect("window");
    let ns_window = win.ns_window();
    // SAFETY: mock runtime only: `ns_window` is the live handle of `win`, which is still in scope;
    // `makeKeyAndOrderFront:` takes one sender object, matching `send_id`.
    unsafe { ObjcMsg::send_id(ns_window, Sel::register("makeKeyAndOrderFront:"), ns_window) };

    // SAFETY: mock runtime only: `ns_window` is the live handle of `win`; `performClose:` takes
    // one sender object, matching `send_id`.
    unsafe { ObjcMsg::send_id(ns_window, Sel::register("performClose:"), ns_window) };
    let data = mock.get_object_data(ns_window).expect("window alive");
    assert!(
        data.is_visible,
        "performClose: on a window without a close button beeps and does not close"
    );
}

#[test]
fn zero_size_window_is_created_without_leaking_the_ns_window() {
    ThreadContext::init_current(true, None);
    let mock = MockObjcRuntime::instance();
    let win = CocoaNativeWindow::new("empty", Rect::new(0, 0, 0, 0), WindowFlags::empty())
        .expect("Qt accepts an empty window rect");
    let ns_window = win.ns_window();
    drop(win);
    assert!(mock.get_object_data(ns_window).is_none());
}

#[test]
fn remove_from_superview_without_parent_does_not_release() {
    ThreadContext::init_current(true, None);
    let mock = MockObjcRuntime::instance();
    let class = ferrokit_platform::Class::get("NSView").expect("NSView");
    // SAFETY: mock runtime only: `class` is the registered mock `NSView`; `alloc` takes no
    // arguments, matching `send_class_0`.
    let view = unsafe { ObjcMsg::send_class_0(class, Sel::register("alloc")) };
    // SAFETY: mock runtime only: `view` is the object just allocated, still live (retain count
    // one); `removeFromSuperview` takes no arguments, matching `send_0`. The test deliberately
    // sends it to an object that was never `init`ed or added to a parent, which the mock accepts.
    unsafe { ObjcMsg::send_0(view, Sel::register("removeFromSuperview")) };
    assert_eq!(mock.get_object_data(view).expect("alive").retain_count, 1);
    assert!(!mock.was_over_released(view));
    // SAFETY: mock runtime only: `view` is live with one reference from `alloc` and is released
    // exactly once here, balancing it; `release` takes no arguments.
    unsafe { ObjcMsg::send_0(view, Sel::register("release")) };
}

#[test]
fn bitmap_rep_init_releases_receiver_on_invalid_input() {
    ThreadContext::init_current(true, None);
    let mock = MockObjcRuntime::instance();
    let class = ferrokit_platform::Class::get("NSBitmapImageRep").expect("class");
    // SAFETY: mock runtime only: `class` is the registered mock `NSBitmapImageRep`; `alloc` takes
    // no arguments, matching `send_class_0`.
    let rep = unsafe { ObjcMsg::send_class_0(class, Sel::register("alloc")) };
    // SAFETY: mock runtime only: `rep` is the unconsumed `alloc` result. The 3-byte slice is
    // deliberately shorter than `4 * 4 * 4` bytes, which `send_bitmap_rep_init` checks and
    // answers by releasing the receiver and returning nil (asserted below), so nothing reads past
    // the slice.
    let out = unsafe { ObjcMsg::send_bitmap_rep_init(rep, 4, 4, &[0u8; 3]) };
    assert!(out.is_nil());
    assert!(mock.get_object_data(rep).is_none(), "receiver leaked");
}
