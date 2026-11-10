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
    DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, SendMessageW, TranslateMessage,
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
        GetCursor, IDC_CROSS, IDC_IBEAM, LoadCursorW,
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

/// The existing move-loop end: the release the loop swallowed is synthesized for a cursor inside
/// the client area.
#[test]
fn starting_a_system_move_keeps_the_pressed_button_for_the_loop_end() {
    use ferrokit_platform::window::NativeWindow;
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
    place_client_under_cursor(hwnd);
    press_left(hwnd);
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

const WM_ENTERSIZEMOVE: u32 = 0x0231;
const WM_EXITSIZEMOVE: u32 = 0x0232;

/// Puts the window's outer top-left corner `dx`, `dy` away from the cursor: `(-50, -50)` puts the
/// cursor inside it, far positive values put it outside.
fn place_window_relative_to_cursor(hwnd: HWND, dx: i32, dy: i32) {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetCursorPos, SWP_NOACTIVATE, SWP_NOZORDER, SetWindowPos,
    };
    // SAFETY: `hwnd` is a live window of this thread, supplied by the caller from its own window;
    // `cursor` is a live `POINT` that `GetCursorPos` fills in; `SetWindowPos` takes integers only.
    unsafe {
        let mut cursor = POINT { x: 0, y: 0 };
        GetCursorPos(&mut cursor);
        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            cursor.x + dx,
            cursor.y + dy,
            200,
            200,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

/// The top-left corner of the window's client area in screen coordinates.
fn client_origin(hwnd: HWND) -> windows_sys::Win32::Foundation::POINT {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::Graphics::Gdi::ClientToScreen;
    let mut origin = POINT { x: 0, y: 0 };
    // SAFETY: `hwnd` is a live window of this thread; `origin` is a live `POINT` that
    // `ClientToScreen` updates.
    unsafe { ClientToScreen(hwnd, &mut origin) };
    origin
}

/// Puts the window so that the cursor is 20 px inside its client area (not merely inside the
/// frame: the title bar is taller at a high DPI scale).
fn place_client_under_cursor(hwnd: HWND) {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetCursorPos, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SetWindowPos,
    };
    place_window_relative_to_cursor(hwnd, -50, -50);
    let origin = client_origin(hwnd);
    // SAFETY: `hwnd` is a live window of this thread; `cursor` is a live `POINT` that
    // `GetCursorPos` fills in; `SetWindowPos` takes integers only.
    unsafe {
        let mut cursor = POINT { x: 0, y: 0 };
        GetCursorPos(&mut cursor);
        let mut rect: windows_sys::Win32::Foundation::RECT = std::mem::zeroed();
        windows_sys::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut rect);
        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            rect.left + (cursor.x - 20 - origin.x),
            rect.top + (cursor.y - 20 - origin.y),
            0,
            0,
            SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOSIZE,
        );
    }
}

fn press_left(hwnd: HWND) {
    use windows_sys::Win32::UI::WindowsAndMessaging::WM_LBUTTONDOWN;
    // SAFETY: `hwnd` is a live window of this thread; the message has integer parameters only.
    unsafe { SendMessageW(hwnd, WM_LBUTTONDOWN, 1, 0x0010_0010) };
}

/// What the system sends when its move loop starts: the loop takes the capture from the window
/// (`lParam` 0, no new owner; seen 140 ms after the `WM_SYSCOMMAND` with a real press on
/// Windows 11), then announces itself.
fn move_loop_starts(hwnd: HWND) {
    use windows_sys::Win32::UI::WindowsAndMessaging::WM_CAPTURECHANGED;
    // SAFETY: `hwnd` is a live window of this thread; the messages have integer parameters only.
    unsafe {
        SendMessageW(hwnd, WM_CAPTURECHANGED, 0, 0);
        SendMessageW(hwnd, WM_ENTERSIZEMOVE, 0, 0);
    }
}

/// What the system sends when its move loop ends: another capture change without a new owner,
/// then the exit notification.
fn move_loop_ends(hwnd: HWND) {
    use windows_sys::Win32::UI::WindowsAndMessaging::WM_CAPTURECHANGED;
    // SAFETY: `hwnd` is a live window of this thread; the messages have integer parameters only.
    unsafe {
        SendMessageW(hwnd, WM_CAPTURECHANGED, 0, 0);
        SendMessageW(hwnd, WM_EXITSIZEMOVE, 0, 0);
    }
}

/// The move loop takes the mouse from the window with capture changes of its own and swallows the
/// release. Qt does nothing with them: the windows plugin of Qt 6.12.0 has no
/// `WM_CAPTURECHANGED` handling, `QGuiApplication::mouseButtons()` keeps the held button for the
/// whole loop and `WM_EXITSIZEMOVE` reconciles it (`QWindowsContext::handleExitSizeMove`,
/// qwindowscontext.cpp:1261-1290). An application that asks "is a button held?" while its window
/// is dragged (to show where the window will dock, say) must get yes.
#[test]
fn the_move_loops_own_capture_changes_keep_the_pressed_button_until_the_loop_ends() {
    use ferrokit_core::event::MouseButtons;
    use ferrokit_platform::window::{NativeWindow, app_mouse_buttons};
    use windows_sys::Win32::UI::WindowsAndMessaging::WM_CAPTURECHANGED;
    let win = NativeWindow::new("MoveLoop", Rect::new(0, 0, 200, 200), WindowFlags::NORMAL)
        .expect("native window");
    let hwnd = win.hwnd();
    place_window_relative_to_cursor(hwnd, -50, -50);
    press_left(hwnd);
    assert_eq!(app_mouse_buttons(), MouseButtons::LEFT);
    assert!(win.start_system_move());
    move_loop_starts(hwnd);
    assert_eq!(
        app_mouse_buttons(),
        MouseButtons::LEFT,
        "the held button is not forgotten while the loop runs"
    );
    // SAFETY: `hwnd` is the live window of `win`, messaged from its own thread; integers only.
    unsafe { SendMessageW(hwnd, WM_CAPTURECHANGED, 0, 0) };
    assert_eq!(
        app_mouse_buttons(),
        MouseButtons::LEFT,
        "nor by the capture change that precedes the loop's end"
    );
    // SAFETY: as above.
    unsafe { SendMessageW(hwnd, WM_EXITSIZEMOVE, 0, 0) };
    assert!(
        app_mouse_buttons().is_empty(),
        "the loop end syncs the button that is physically up: {:?}",
        app_mouse_buttons()
    );
}

/// A system resize is the same kind of loop (`startSystemResize` releases the capture and posts
/// `SC_SIZE*` just as `startSystemMove` posts `SC_DRAGMOVE`, qwindowswindow.cpp:3303-3313).
#[test]
fn the_resize_loops_own_capture_changes_keep_the_pressed_button_until_the_loop_ends() {
    use ferrokit_core::event::MouseButtons;
    use ferrokit_platform::platform_window::WindowEdges;
    use ferrokit_platform::window::{NativeWindow, app_mouse_buttons};
    let win = NativeWindow::new("ResizeLoop", Rect::new(0, 0, 200, 200), WindowFlags::NORMAL)
        .expect("native window");
    let hwnd = win.hwnd();
    place_window_relative_to_cursor(hwnd, -50, -50);
    press_left(hwnd);
    assert!(win.start_system_resize(WindowEdges::BOTTOM | WindowEdges::RIGHT));
    move_loop_starts(hwnd);
    assert_eq!(app_mouse_buttons(), MouseButtons::LEFT);
    move_loop_ends(hwnd);
    assert!(app_mouse_buttons().is_empty());
}

/// Qt 6.12.0's windows plugin has no `WM_CAPTURECHANGED` or `WM_CANCELMODE` handling: neither a
/// capture change (to nothing, to a foreign window) nor a cancelled mouse mode touches
/// `QGuiApplication::mouseButtons()`; it changes with mouse events and at `WM_EXITSIZEMOVE`.
#[test]
fn a_capture_change_or_a_cancelled_mouse_mode_keeps_the_pressed_buttons() {
    use ferrokit_core::event::MouseButtons;
    use ferrokit_platform::window::{NativeWindow, app_mouse_buttons};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetDesktopWindow, WM_CANCELMODE, WM_CAPTURECHANGED,
    };
    let win = NativeWindow::new("Capture", Rect::new(0, 0, 200, 200), WindowFlags::NORMAL)
        .expect("native window");
    let hwnd = win.hwnd();
    press_left(hwnd);
    // SAFETY: `hwnd` is the live window of `win`, messaged from its own thread; `GetDesktopWindow`
    // takes no arguments and its result is only passed along as an integer.
    unsafe {
        SendMessageW(hwnd, WM_CAPTURECHANGED, 0, 0);
        assert_eq!(
            app_mouse_buttons(),
            MouseButtons::LEFT,
            "capture to nothing"
        );
        SendMessageW(hwnd, WM_CAPTURECHANGED, 0, GetDesktopWindow() as isize);
        assert_eq!(
            app_mouse_buttons(),
            MouseButtons::LEFT,
            "capture to a window of another process"
        );
        SendMessageW(hwnd, WM_CANCELMODE, 0, 0);
    }
    assert_eq!(
        app_mouse_buttons(),
        MouseButtons::LEFT,
        "cancelled mouse mode"
    );
}

/// A second drag works like the first.
#[test]
fn a_second_system_move_keeps_the_button_too() {
    use ferrokit_core::event::MouseButtons;
    use ferrokit_platform::window::{NativeWindow, app_mouse_buttons};
    let win = NativeWindow::new("TwoMoves", Rect::new(0, 0, 200, 200), WindowFlags::NORMAL)
        .expect("native window");
    let hwnd = win.hwnd();
    place_client_under_cursor(hwnd);
    for _ in 0..2 {
        press_left(hwnd);
        assert!(win.start_system_move());
        move_loop_starts(hwnd);
        assert_eq!(app_mouse_buttons(), MouseButtons::LEFT);
        move_loop_ends(hwnd);
        assert!(app_mouse_buttons().is_empty());
    }
}

/// A loop that ends with the cursor outside the window: the swallowed release is reconciled
/// without a mouse event for the window (Qt: `NonClientAreaMouseButtonRelease`, which ferrokit has
/// no counterpart for).
#[test]
fn a_move_loop_that_ends_outside_the_window_forgets_the_button_without_a_release() {
    use ferrokit_platform::window::{NativeWindow, app_mouse_buttons};
    let win = NativeWindow::new("MoveAway", Rect::new(0, 0, 200, 200), WindowFlags::NORMAL)
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
    place_window_relative_to_cursor(hwnd, -50, -50);
    press_left(hwnd);
    assert!(win.start_system_move());
    move_loop_starts(hwnd);
    // The loop moved the window away from the cursor.
    place_window_relative_to_cursor(hwnd, 400, 400);
    seen.lock().clear();
    move_loop_ends(hwnd);
    assert!(app_mouse_buttons().is_empty());
    assert!(
        !seen
            .lock()
            .iter()
            .any(|e| matches!(e, WindowSystemEvent::MouseRelease { .. })),
        "no release reaches the window: {:?}",
        seen.lock()
    );
}

/// `handleExitSizeMove` compares the application's buttons with the physical ones. Left, right and
/// middle that the application has down and the system does not get a release; every release
/// carries the physical buttons, so a button the application had down is dropped with them
/// (`mouse_buttons = e->buttons`, qguiapplication.cpp:2431), the X ones included.
#[test]
fn the_loop_end_releases_every_stale_button_and_takes_the_physical_ones_over() {
    use ferrokit_core::event::MouseButtons;
    use ferrokit_platform::MouseButton;
    use ferrokit_platform::window::{NativeWindow, app_mouse_buttons, set_app_mouse_buttons};
    let win = NativeWindow::new(
        "StaleButtons",
        Rect::new(0, 0, 200, 200),
        WindowFlags::NORMAL,
    )
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
    place_client_under_cursor(hwnd);
    set_app_mouse_buttons(
        MouseButtons::LEFT
            .union(MouseButtons::RIGHT)
            .union(MouseButtons::MIDDLE)
            .union(MouseButtons::BACK),
    );
    seen.lock().clear();
    // SAFETY: `hwnd` is the live window of `win`, messaged from its own thread; integers only.
    unsafe { SendMessageW(hwnd, WM_EXITSIZEMOVE, 0, 0) };
    assert!(
        app_mouse_buttons().is_empty(),
        "no button is physically down: {:?}",
        app_mouse_buttons()
    );
    let released: Vec<_> = seen
        .lock()
        .iter()
        .filter_map(|e| match e {
            WindowSystemEvent::MouseRelease { button, .. } => Some(*button),
            _ => None,
        })
        .collect();
    assert_eq!(
        released,
        vec![MouseButton::Left, MouseButton::Right, MouseButton::Middle],
        "left, right, middle in that order, and no release for the X button"
    );
}

/// Only left, right and middle are released (`for (Qt::MouseButton button : {Qt::LeftButton,
/// Qt::RightButton, Qt::MiddleButton})`): a stale X button alone changes nothing, however long it
/// has been stuck.
#[test]
fn a_stale_x_button_alone_is_not_released_at_the_loop_end() {
    use ferrokit_core::event::MouseButtons;
    use ferrokit_platform::window::{NativeWindow, app_mouse_buttons, set_app_mouse_buttons};
    let win = NativeWindow::new("StaleX", Rect::new(0, 0, 200, 200), WindowFlags::NORMAL)
        .expect("native window");
    let hwnd = win.hwnd();
    set_app_mouse_buttons(MouseButtons::BACK);
    // SAFETY: `hwnd` is the live window of `win`, messaged from its own thread; integers only.
    unsafe { SendMessageW(hwnd, WM_EXITSIZEMOVE, 0, 0) };
    assert_eq!(app_mouse_buttons(), MouseButtons::BACK);
}

/// "Inside" is the client geometry of the window (`platWin->geometry()`), not its frame: a loop
/// that ends over the title bar is a non-client release, which the window does not get as a mouse
/// release.
#[test]
fn a_move_loop_that_ends_over_the_title_bar_sends_no_client_release() {
    use ferrokit_platform::window::{NativeWindow, app_mouse_buttons};
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let win = NativeWindow::new("TitleBar", Rect::new(0, 0, 200, 200), WindowFlags::NORMAL)
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
    place_window_relative_to_cursor(hwnd, -50, -4);
    let client = client_origin(hwnd);
    let mut cursor = POINT { x: 0, y: 0 };
    // SAFETY: `cursor` is a live `POINT` that `GetCursorPos` fills in.
    unsafe { GetCursorPos(&mut cursor) };
    assert!(
        cursor.y < client.y,
        "premise: the cursor is above the client area ({} < {})",
        cursor.y,
        client.y
    );
    press_left(hwnd);
    assert!(win.start_system_move());
    move_loop_starts(hwnd);
    seen.lock().clear();
    move_loop_ends(hwnd);
    assert!(app_mouse_buttons().is_empty());
    assert!(
        !seen
            .lock()
            .iter()
            .any(|e| matches!(e, WindowSystemEvent::MouseRelease { .. })),
        "the title bar is not the client area: {:?}",
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
