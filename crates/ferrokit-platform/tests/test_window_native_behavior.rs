// SPDX-License-Identifier: LGPL-3.0-only
//! Native window behaviour that needs a real HWND: frame metrics and the event queue pump.
#![cfg(windows)]

use ferrokit_gui::geometry::primitives::Rect;
use ferrokit_platform::integration::platform;
use ferrokit_platform::window::WindowFlags;
use ferrokit_platform::window_system_interface::{
    ClosureWindowEventHandler, Delivery, WindowSystemEvent,
};
use ferrokit_platform::dispatch_window_system_event;
use parking_lot::Mutex;
use std::sync::Arc;
use windows_sys::Win32::Foundation::{HWND, RECT};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetClientRect, GetWindowRect, PeekMessageW, SendMessageW, SetWindowPos,
    TranslateMessage, MSG, PM_REMOVE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER,
};

#[test]
fn a_normal_window_keeps_its_native_frame() {
    let win = platform()
        .create_window("Frame", Rect::new(50, 50, 400, 300), WindowFlags::NORMAL)
        .expect("create window");
    let hwnd = win.native_handle() as HWND;
    let (mut client, mut outer): (RECT, RECT) = unsafe { std::mem::zeroed() };
    unsafe {
        // A resize recalculates the client area (`WM_NCCALCSIZE`, `wParam` = TRUE).
        SetWindowPos(hwnd, std::ptr::null_mut(), 0, 0, 420, 320, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
        GetClientRect(hwnd, &mut client);
        GetWindowRect(hwnd, &mut outer);
    }
    assert!(
        client.right - client.left < outer.right - outer.left
            && client.bottom - client.top < outer.bottom - outer.top,
        "client {}x{} must be smaller than the window {}x{}: caption and borders stay",
        client.right - client.left,
        client.bottom - client.top,
        outer.right - outer.left,
        outer.bottom - outer.top
    );
}

#[test]
fn events_posted_from_another_thread_are_delivered_by_the_message_pump() {
    let mut win = platform()
        .create_window("Pump", Rect::new(50, 50, 300, 200), WindowFlags::NORMAL)
        .expect("create window");
    let hwnd = win.native_handle() as HWND;
    let seen = Arc::new(Mutex::new(Vec::<WindowSystemEvent>::new()));
    let sink = Arc::clone(&seen);
    win.set_event_handler(Box::new(ClosureWindowEventHandler::new(move |ev| {
        sink.lock().push(ev);
    })));

    let raw = hwnd as isize;
    std::thread::spawn(move || {
        dispatch_window_system_event(
            Delivery::Default,
            raw as HWND,
            WindowSystemEvent::CloseRequest,
        );
    })
    .join()
    .unwrap();
    assert!(seen.lock().is_empty(), "queued, not run on the posting thread");

    // The message was posted to the window before `join` returned: drain it without waiting.
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    let seen = seen.lock();
    assert_eq!(seen.len(), 1, "got {seen:?}");
    assert!(matches!(seen[0], WindowSystemEvent::CloseRequest));
}

fn window(title: &str) -> Box<dyn ferrokit_platform::platform_window::PlatformWindow> {
    platform()
        .create_window(title, Rect::new(60, 60, 300, 200), WindowFlags::NORMAL)
        .expect("create window")
}

fn set_cursor_message(hwnd: HWND) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{HTCLIENT, WM_MOUSEMOVE, WM_SETCURSOR};
    unsafe {
        SendMessageW(
            hwnd,
            WM_SETCURSOR,
            hwnd as usize,
            (HTCLIENT as isize) | ((WM_MOUSEMOVE as isize) << 16),
        );
    }
}

#[test]
fn each_window_keeps_its_own_cursor_and_blank_hides_it() {
    use ferrokit_platform::cursor::CursorShape;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetCursor, LoadCursorW, IDC_CROSS, IDC_IBEAM,
    };
    let mut a = window("Cursor A");
    let mut b = window("Cursor B");
    a.set_cursor(CursorShape::IBeam);
    b.set_cursor(CursorShape::Cross);
    let (ha, hb) = (a.native_handle() as HWND, b.native_handle() as HWND);
    unsafe {
        set_cursor_message(ha);
        assert_eq!(GetCursor(), LoadCursorW(std::ptr::null_mut(), IDC_IBEAM), "window A");
        set_cursor_message(hb);
        assert_eq!(GetCursor(), LoadCursorW(std::ptr::null_mut(), IDC_CROSS), "window B");
        set_cursor_message(ha);
        assert_eq!(GetCursor(), LoadCursorW(std::ptr::null_mut(), IDC_IBEAM), "A again");

        a.set_cursor(CursorShape::Blank);
        set_cursor_message(ha);
        assert!(GetCursor().is_null(), "Qt::BlankCursor hides the cursor");
    }
}

#[test]
fn a_horizontal_wheel_message_is_delivered_with_its_own_axis() {
    use windows_sys::Win32::UI::WindowsAndMessaging::WM_MOUSEHWHEEL;
    let mut win = window("HWheel");
    let hwnd = win.native_handle() as HWND;
    let seen = Arc::new(Mutex::new(Vec::<WindowSystemEvent>::new()));
    let sink = Arc::clone(&seen);
    win.set_event_handler(Box::new(ClosureWindowEventHandler::new(move |ev| {
        sink.lock().push(ev);
    })));
    unsafe {
        SendMessageW(hwnd, WM_MOUSEHWHEEL, (120usize) << 16, 0);
    }
    let seen = seen.lock();
    let wheel = seen
        .iter()
        .find_map(|e| match e {
            WindowSystemEvent::Wheel { delta, .. } => Some(*delta),
            _ => None,
        })
        .expect("a Wheel event");
    assert_eq!((wheel.x, wheel.y), (120, 0));
}

#[test]
fn an_alt_key_combination_reaches_the_handler() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{WM_SYSKEYDOWN, WM_SYSKEYUP};
    let mut win = window("SysKey");
    let hwnd = win.native_handle() as HWND;
    let seen = Arc::new(Mutex::new(Vec::<WindowSystemEvent>::new()));
    let sink = Arc::clone(&seen);
    win.set_event_handler(Box::new(ClosureWindowEventHandler::new(move |ev| {
        sink.lock().push(ev);
    })));
    unsafe {
        SendMessageW(hwnd, WM_SYSKEYDOWN, 0x46, 0x2000_0001); // Alt+F
        SendMessageW(hwnd, WM_SYSKEYUP, 0x46, 0xE000_0001u32 as isize);
    }
    let seen = seen.lock();
    assert!(matches!(seen[0], WindowSystemEvent::KeyPress { key: 0x46, .. }), "{seen:?}");
    assert!(matches!(seen[1], WindowSystemEvent::KeyRelease { key: 0x46, .. }), "{seen:?}");
}

#[test]
fn a_cancelled_mouse_mode_forgets_the_pressed_buttons() {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetCapture;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        WM_CANCELMODE, WM_LBUTTONDOWN, WM_RBUTTONDOWN, WM_RBUTTONUP,
    };
    let win = window("Cancel");
    let hwnd = win.native_handle() as HWND;
    unsafe {
        SendMessageW(hwnd, WM_LBUTTONDOWN, 1, 0x0010_0010);
        // A popup or dialog took over: the release of the left button is never seen.
        SendMessageW(hwnd, WM_CANCELMODE, 0, 0);
        SendMessageW(hwnd, WM_RBUTTONDOWN, 2, 0x0010_0010);
        SendMessageW(hwnd, WM_RBUTTONUP, 0, 0x0010_0010);
        assert_ne!(GetCapture(), hwnd, "no button is held: the capture is released");
    }
}
