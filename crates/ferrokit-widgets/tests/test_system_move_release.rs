// SPDX-License-Identifier: LGPL-3.0-only
//! `QWindowsContext::handleExitSizeMove` (`qwindowscontext.cpp:1261-1290`): a window dragged by the
//! native move loop never receives `WM_LBUTTONUP` (the loop swallows it), so Qt compares the
//! application's idea of the pressed buttons with the physical state when `WM_EXITSIZEMOVE`
//! arrives and synthesizes the missing release. Inside the window it is a normal
//! `MouseButtonRelease`; outside it is a non-client release, which widgets never see.
//!
//! Real `ferrokit_widgets::Window`, real HWND, real `WM_LBUTTONDOWN`/`WM_LBUTTONUP`/
//! `WM_ENTERSIZEMOVE`/`WM_EXITSIZEMOVE` messages, real cursor position. No physical button may be
//! held while this runs (the test machine's mouse is idle, as in the real move-loop end).
#![cfg(windows)]

use std::cell::Cell;
use std::ptr::null_mut;
use std::rc::Rc;

use ferrokit_core::event::EventKind;
use ferrokit_core::event_loop::EventLoop;
use ferrokit_gui::geometry::primitives::Rect;
use ferrokit_platform::WindowFlags;
use ferrokit_widgets::window::Window;
use windows_sys::Win32::Foundation::{HWND, POINT};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, SendMessageW, SetWindowPos, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER,
    WM_LBUTTONDOWN, WM_LBUTTONUP,
};

const WM_ENTERSIZEMOVE: u32 = 0x0231;
const WM_EXITSIZEMOVE: u32 = 0x0232;

struct Fixture {
    el: EventLoop,
    _win: Box<Window>,
    hwnd: HWND,
    releases: Rc<Cell<u32>>,
}

fn fixture() -> Fixture {
    let el = EventLoop::new();
    let mut win = Box::new(
        Window::new("System Move Release", Rect::new(0, 0, 200, 200), WindowFlags::empty())
            .expect("create window"),
    );
    let hwnd = win.native_handle() as HWND;
    ferrokit_platform::window::register_window_event_binding(hwnd, el.handle(), win.id());
    let releases = Rc::new(Cell::new(0u32));
    let r = Rc::clone(&releases);
    win.set_window_event_handler(move |ev| {
        if matches!(ev.kind, EventKind::MouseButtonRelease { .. }) {
            r.set(r.get() + 1);
        }
    });
    win.render_and_present();
    Fixture { el, _win: win, hwnd, releases }
}

fn cursor() -> POINT {
    let mut pt = POINT { x: 0, y: 0 };
    // SAFETY: valid out pointer.
    unsafe { GetCursorPos(&mut pt) };
    pt
}

/// Moves the window (physical px, size kept) so its top-left is at `(x, y)`.
fn place(fx: &Fixture, x: i32, y: i32) {
    // SAFETY: valid HWND owned by this thread.
    let ok = unsafe {
        SetWindowPos(fx.hwnd, null_mut(), x, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE)
    };
    assert_ne!(ok, 0, "SetWindowPos failed");
}

/// Places the 200x200 window so the cursor is inside it.
fn place_under_cursor(fx: &Fixture) {
    let c = cursor();
    place(fx, c.x - 20, c.y - 20);
}

fn place_away_from_cursor(fx: &Fixture) {
    let c = cursor();
    place(fx, c.x + 5000, c.y + 5000);
}

fn lparam(x: i32, y: i32) -> isize {
    ((x & 0xffff) | ((y & 0xffff) << 16)) as isize
}

fn press(fx: &Fixture) {
    // SAFETY: valid HWND.
    unsafe { SendMessageW(fx.hwnd, WM_LBUTTONDOWN, 1, lparam(20, 20)) };
}

fn real_release(fx: &Fixture) {
    // SAFETY: valid HWND.
    unsafe { SendMessageW(fx.hwnd, WM_LBUTTONUP, 0, lparam(20, 20)) };
}

fn move_loop(fx: &mut Fixture) {
    // SAFETY: valid HWND.
    unsafe {
        SendMessageW(fx.hwnd, WM_ENTERSIZEMOVE, 0, 0);
        SendMessageW(fx.hwnd, WM_EXITSIZEMOVE, 0, 0);
    }
    for _ in 0..4 {
        fx.el.process_events(false);
    }
}

fn drain(fx: &mut Fixture) {
    for _ in 0..4 {
        fx.el.process_events(false);
    }
}

/// What an application-visible release looks like for this window (whatever the event pipeline
/// delivers for a real `WM_LBUTTONUP`).
fn releases_for_a_real_up() -> u32 {
    let mut fx = fixture();
    place_under_cursor(&fx);
    press(&fx);
    real_release(&fx);
    drain(&mut fx);
    fx.releases.get()
}

#[test]
fn move_loop_end_releases_a_button_the_loop_swallowed_the_release_of() {
    let expected = releases_for_a_real_up();
    assert!(expected >= 1, "a real WM_LBUTTONUP must reach the window event handler");

    let mut fx = fixture();
    place_under_cursor(&fx);
    press(&fx);
    drain(&mut fx);
    assert_eq!(fx.releases.get(), 0, "no release before the move loop ends");
    move_loop(&mut fx);
    assert_eq!(
        fx.releases.get(),
        expected,
        "WM_EXITSIZEMOVE must deliver the same release a real WM_LBUTTONUP would"
    );
}

#[test]
fn move_loop_end_without_a_pressed_button_delivers_nothing() {
    let mut fx = fixture();
    place_under_cursor(&fx);
    move_loop(&mut fx);
    assert_eq!(fx.releases.get(), 0);
}

#[test]
fn a_release_that_did_arrive_is_not_delivered_again_at_move_loop_end() {
    let mut fx = fixture();
    place_under_cursor(&fx);
    press(&fx);
    real_release(&fx);
    drain(&mut fx);
    let after_real = fx.releases.get();
    assert!(after_real >= 1);
    move_loop(&mut fx);
    assert_eq!(fx.releases.get(), after_real);
}

#[test]
fn move_loop_end_with_the_cursor_outside_is_a_non_client_release_and_clears_the_button() {
    let mut fx = fixture();
    place_under_cursor(&fx);
    press(&fx);
    place_away_from_cursor(&fx);
    move_loop(&mut fx);
    assert_eq!(fx.releases.get(), 0, "outside the window Qt sends NonClientAreaMouseButtonRelease");

    // The application's button state was synchronized: a later move-loop end releases nothing.
    place_under_cursor(&fx);
    move_loop(&mut fx);
    assert_eq!(fx.releases.get(), 0);
}
