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
    DispatchMessageW, GetClientRect, GetWindowRect, PeekMessageW, SetWindowPos, TranslateMessage,
    MSG, PM_REMOVE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER,
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
