// SPDX-License-Identifier: LGPL-3.0-only
//! Qt's Windows plugin registers its window class with `CS_DBLCLKS`
//! (`qwindowswindowclassdescription.cpp:67`), so the OS reports double clicks, and the generic
//! layer delivers `Press, Release, DblClick, Release` (`qguiapplication.cpp:2495-2540`): the second
//! press of a double click is *not* delivered as a press. A native window that maps
//! `WM_LBUTTONDBLCLK` to a plain `MousePress` makes a double click indistinguishable from two
//! clicks.

#![cfg(windows)]

use ferrokit_gui::geometry::primitives::Rect;
use ferrokit_platform::*;
use std::sync::{Arc, Mutex};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CS_DBLCLKS, GCL_STYLE, GetClassLongPtrW, SendMessageW, WM_LBUTTONDBLCLK, WM_RBUTTONDBLCLK,
};

fn window_with_log() -> (
    Box<dyn PlatformWindow>,
    Arc<Mutex<Vec<WindowSystemEvent>>>,
    HWND,
) {
    let platform = Win32PlatformIntegration::default();
    let mut win = platform
        .create_window(
            "double click",
            Rect::new(100, 100, 400, 300),
            WindowFlags::NORMAL,
        )
        .expect("native window");
    let log = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&log);
    win.set_event_handler(Box::new(ClosureWindowEventHandler::new(move |e| {
        sink.lock().unwrap().push(e);
    })));
    let hwnd = win.native_handle() as HWND;
    (win, log, hwnd)
}

#[test]
fn native_window_class_asks_the_os_for_double_clicks() {
    let (_win, _log, hwnd) = window_with_log();
    // SAFETY: `hwnd` is the handle of `_win`, which `window_with_log` returned and which is alive
    // until the end of the test; `GetClassLongPtrW` only reads the window class.
    let style = unsafe { GetClassLongPtrW(hwnd, GCL_STYLE) } as u32;
    assert_ne!(
        style & CS_DBLCLKS,
        0,
        "without CS_DBLCLKS Windows never sends WM_*BUTTONDBLCLK"
    );
}

#[test]
fn double_click_message_is_a_double_click_event_not_a_second_press() {
    let (_win, log, hwnd) = window_with_log();
    let pos = (50 & 0xffff) | ((80 & 0xffff) << 16);
    // SAFETY: `hwnd` is the live window of `_win` (alive to the end of the test), messaged from its
    // own thread with integer parameters only.
    unsafe {
        SendMessageW(hwnd, WM_LBUTTONDBLCLK, 0, pos);
        SendMessageW(hwnd, WM_RBUTTONDBLCLK, 0, pos);
    }
    let events = log.lock().unwrap().clone();
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, WindowSystemEvent::MousePress { .. })),
        "a double click must not be reported as a press: {events:?}"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            WindowSystemEvent::MouseDoubleClick { pos, button: MouseButton::Left, .. }
                if pos.x == 50 && pos.y == 80
        )),
        "left double click missing: {events:?}"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            WindowSystemEvent::MouseDoubleClick {
                button: MouseButton::Right,
                ..
            }
        )),
        "right double click missing: {events:?}"
    );
}
