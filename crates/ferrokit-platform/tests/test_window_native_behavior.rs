// SPDX-License-Identifier: LGPL-3.0-only
//! Native window behaviour that needs a real HWND: frame metrics and the event queue pump.
#![cfg(windows)]

use ferrokit_gui::geometry::primitives::Rect;
use ferrokit_platform::dispatch_window_system_event;
use ferrokit_platform::integration::platform;
use ferrokit_platform::window::WindowFlags;
use ferrokit_platform::window_system_interface::{
    ClosureWindowEventHandler, Delivery, WindowSystemEvent,
};
use parking_lot::Mutex;
use std::sync::Arc;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, PeekMessageW, SendMessageW, TranslateMessage, MSG, PM_REMOVE,
};

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
    assert!(
        seen.lock().is_empty(),
        "queued, not run on the posting thread"
    );

    // The message was posted to the window before `join` returned: drain it without waiting.
    // SAFETY: an all-zero `MSG` is a valid value; `msg` outlives the pump calls, which run on this
    // (the window's own) thread, the only one allowed to pump its queue. `win` is still alive.
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
    // SAFETY: `hwnd` is a live window of this thread, supplied by the caller from its own
    // window; the message has integer parameters only.
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
    // SAFETY: `ha`/`hb` are the live handles of `a` and `b`, which outlive the block; the
    // messages are sent on the window-owning thread with integer parameters, and
    // `LoadCursorW(null, IDC_*)` loads a shared system cursor from a static resource id.
    unsafe {
        set_cursor_message(ha);
        assert_eq!(
            GetCursor(),
            LoadCursorW(std::ptr::null_mut(), IDC_IBEAM),
            "window A"
        );
        set_cursor_message(hb);
        assert_eq!(
            GetCursor(),
            LoadCursorW(std::ptr::null_mut(), IDC_CROSS),
            "window B"
        );
        set_cursor_message(ha);
        assert_eq!(
            GetCursor(),
            LoadCursorW(std::ptr::null_mut(), IDC_IBEAM),
            "A again"
        );

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
    // SAFETY: `hwnd` is the live window of `win` (alive to the end of the test), messaged from its
    // own thread with integer parameters only.
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
    // Qt negates WM_MOUSEHWHEEL (positive Win32 = scroll right; Qt positive = scroll left).
    assert_eq!((wheel.x, wheel.y), (-120, 0));
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
    // SAFETY: `hwnd` is the live window of `win` (alive to the end of the test), messaged from its
    // own thread with integer parameters only.
    unsafe {
        SendMessageW(hwnd, WM_SYSKEYDOWN, 0x46, 0x2000_0001); // Alt+F
        SendMessageW(hwnd, WM_SYSKEYUP, 0x46, 0xE000_0001u32 as isize);
    }
    let seen = seen.lock();
    assert!(
        matches!(seen[0], WindowSystemEvent::KeyPress { key: 0x46, .. }),
        "{seen:?}"
    );
    assert!(
        matches!(seen[1], WindowSystemEvent::KeyRelease { key: 0x46, .. }),
        "{seen:?}"
    );
}

#[test]
fn a_cancelled_mouse_mode_forgets_the_pressed_buttons() {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetCapture;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        WM_CANCELMODE, WM_LBUTTONDOWN, WM_RBUTTONDOWN, WM_RBUTTONUP,
    };
    let win = window("Cancel");
    let hwnd = win.native_handle() as HWND;
    // SAFETY: `hwnd` is the live window of `win` (alive to the end of the test), messaged from its
    // own thread with integer parameters only; `GetCapture` takes no arguments.
    unsafe {
        SendMessageW(hwnd, WM_LBUTTONDOWN, 1, 0x0010_0010);
        // A popup or dialog took over: the release of the left button is never seen.
        SendMessageW(hwnd, WM_CANCELMODE, 0, 0);
        SendMessageW(hwnd, WM_RBUTTONDOWN, 2, 0x0010_0010);
        SendMessageW(hwnd, WM_RBUTTONUP, 0, 0x0010_0010);
        assert_ne!(
            GetCapture(),
            hwnd,
            "no button is held: the capture is released"
        );
    }
}

#[test]
fn starting_a_system_move_keeps_the_pressed_button_for_the_loop_end() {
    use ferrokit_platform::window::NativeWindow;
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetCursorPos, SetWindowPos, SWP_NOACTIVATE, SWP_NOZORDER, WM_LBUTTONDOWN,
    };
    const WM_EXITSIZEMOVE: u32 = 0x0232;
    let win = NativeWindow::new("SystemMove", Rect::new(0, 0, 200, 200), WindowFlags::NORMAL)
        .expect("native window");
    let hwnd = win.hwnd();
    let seen = Arc::new(Mutex::new(Vec::<WindowSystemEvent>::new()));
    let sink = Arc::clone(&seen);
    ferrokit_platform::window::set_window_event_handler(
        hwnd,
        Box::new(ClosureWindowEventHandler::new(move |ev| {
            sink.lock().push(ev)
        })),
    );
    // Put the window under the cursor so the loop end sees the release inside it.
    // SAFETY: `hwnd` is the live window of `win`, used from its own thread; `cursor` is a live
    // `POINT` that `GetCursorPos` fills in; the message and `SetWindowPos` take integers only.
    unsafe {
        let mut cursor = POINT { x: 0, y: 0 };
        GetCursorPos(&mut cursor);
        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            cursor.x - 50,
            cursor.y - 50,
            200,
            200,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
        SendMessageW(hwnd, WM_LBUTTONDOWN, 1, 0x0010_0010);
    }
    // `ReleaseCapture` for the native loop must not make the window forget the press.
    assert!(win.start_system_move());
    // SAFETY: `hwnd` is the live window of `win` (still alive), messaged from its own thread with
    // integer parameters only.
    unsafe {
        SendMessageW(hwnd, WM_EXITSIZEMOVE, 0, 0);
    }
    assert!(
        seen.lock()
            .iter()
            .any(|e| matches!(e, WindowSystemEvent::MouseRelease { .. })),
        "the release the move loop swallowed is synthesized: {:?}",
        seen.lock()
    );
}

/// Ending a running composition clears the preedit with an empty event
/// (`QWindowsInputContext::endComposition`); an end without a start sends nothing.
#[test]
fn ending_an_ime_composition_clears_the_preedit() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        WM_IME_COMPOSITION, WM_IME_ENDCOMPOSITION, WM_IME_STARTCOMPOSITION,
    };
    let mut win = window("ImeEnd");
    let hwnd = win.native_handle() as HWND;
    let seen = Arc::new(Mutex::new(Vec::<WindowSystemEvent>::new()));
    let sink = Arc::clone(&seen);
    win.set_event_handler(Box::new(ClosureWindowEventHandler::new(move |ev| {
        sink.lock().push(ev);
    })));
    let count = || {
        seen.lock()
            .iter()
            .filter(|e| matches!(e, WindowSystemEvent::InputMethod { .. }))
            .count()
    };
    // SAFETY: `hwnd` is the live window of `win` (alive to the end of the test), messaged from its
    // own thread with integer parameters only.
    unsafe {
        SendMessageW(hwnd, WM_IME_COMPOSITION, 0, 0);
        SendMessageW(hwnd, WM_IME_ENDCOMPOSITION, 0, 0);
    }
    assert_eq!(count(), 0, "lparam 0 and an unstarted end send nothing");
    // SAFETY: `hwnd` is the live window of `win` (alive to the end of the test), messaged from its
    // own thread with integer parameters only.
    unsafe {
        SendMessageW(hwnd, WM_IME_STARTCOMPOSITION, 0, 0);
        SendMessageW(hwnd, WM_IME_ENDCOMPOSITION, 0, 0);
    }
    assert_eq!(
        count(),
        1,
        "a started composition ends with one empty event"
    );
    assert!(seen.lock().iter().any(|e| matches!(
        e,
        WindowSystemEvent::InputMethod { commit_string, preedit_string, .. }
            if commit_string.is_empty() && preedit_string.is_empty()
    )));
}
