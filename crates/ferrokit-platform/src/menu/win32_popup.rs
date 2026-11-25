// SPDX-License-Identifier: LGPL-3.0-only
//! Win32 modal popup capture, cursor, and message loop helpers.

use ferrokit_gui::geometry::primitives::Point;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, VK_DOWN, VK_ESCAPE, VK_LEFT, VK_RETURN, VK_RIGHT, VK_UP,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, IDC_ARROW, LoadCursorW, MSG, PM_REMOVE, PeekMessageW,
    PostQuitMessage, PostThreadMessageW, SetCursor, SetForegroundWindow, TranslateMessage,
    WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_NULL, WM_RBUTTONDOWN, WM_RBUTTONUP,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopupKey {
    Escape,
    Up,
    Down,
    Left,
    Right,
    Return,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopupEvent {
    MouseMove { screen_pos: Point },
    MouseDown { screen_pos: Point, button: u8 },
    MouseUp { screen_pos: Point, button: u8 },
    KeyDown { key: PopupKey },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopupLoopControl {
    Continue,
    Break,
}

#[derive(Debug, Clone, Copy)]
pub struct PopupLoopWaker {
    thread_id: u32,
}

impl PopupLoopWaker {
    pub fn current_thread() -> Self {
        // SAFETY: `GetCurrentThreadId` takes no arguments and cannot fail.
        let thread_id = unsafe { GetCurrentThreadId() };
        Self { thread_id }
    }

    pub fn wake(&self) {
        // SAFETY: `PostThreadMessageW` takes integers only (`thread_id`, `WM_NULL`, zero
        // parameters); no pointer is involved. If the thread id were stale the call would just
        // fail, which is ignored.
        unsafe { PostThreadMessageW(self.thread_id, WM_NULL, 0, 0) };
    }
}

/// Brings the popup window to the foreground, captures mouse input, and releases capture on drop.
pub struct ModalPopupCapture(());

impl ModalPopupCapture {
    pub fn acquire(hwnd: isize) -> Self {
        let hwnd = hwnd as HWND;
        // SAFETY: `hwnd` is the live native handle of the popup window created on this thread, so
        // it is a window of the calling thread as `SetCapture` requires (an invalid handle would
        // only make the call fail). Both calls take integer/handle arguments only.
        unsafe {
            SetForegroundWindow(hwnd);
            SetCapture(hwnd);
        }
        let guard = Self(());
        guard.show_arrow_cursor();
        guard
    }

    /// Shows the arrow over a popup that holds mouse capture.
    ///
    /// Windows sends no `WM_SETCURSOR` to a window that has the mouse captured, so the capturing
    /// window sets the arrow cursor itself while the popup is open.
    pub fn show_arrow_cursor(&self) {
        // SAFETY: `LoadCursorW` with a null instance and `IDC_ARROW` returns the shared system
        // arrow, which needs no release; `SetCursor` takes that handle by value.
        unsafe { SetCursor(LoadCursorW(std::ptr::null_mut(), IDC_ARROW)) };
    }
}

impl Drop for ModalPopupCapture {
    fn drop(&mut self) {
        // SAFETY: `ReleaseCapture` takes no arguments and only clears the calling thread's mouse
        // capture acquired on this same thread in `ModalPopupCapture::acquire`.
        unsafe { ReleaseCapture() };
    }
}

/// Runs a Win32 modal popup message loop for `hwnd`, returning any intercepted `WM_QUIT` exit code.
pub fn run_modal_popup_loop(
    hwnd: isize,
    capture: ModalPopupCapture,
    should_stop: impl Fn() -> bool,
    mut on_event: impl FnMut(PopupEvent) -> PopupLoopControl,
) -> Option<i32> {
    let mut quit = None;
    // SAFETY: this is the modal message loop of the calling thread. `msg` and `discard_msg` are
    // live `MSG` locals, for which the all-zero pattern is valid (integers and raw pointers), and
    // every Win32 call below gets a pointer to one of them; `GetMessageW` and `PeekMessageW` fill
    // them and `TranslateMessage`/`DispatchMessageW` only read the `msg` just filled. A null
    // window handle means "all windows of this thread".
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        while !ferrokit_core::event_loop::thread_quit_now() && {
            let got = GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0);
            if got == 0 {
                quit = Some(msg.wParam as i32);
            }
            got > 0
        } {
            let mouse_pt = Point::new(msg.pt.x, msg.pt.y);
            let screen_pos = crate::screen::win32_screen::from_native_global_point(mouse_pt, hwnd);
            let control = match msg.message {
                WM_MOUSEMOVE => {
                    capture.show_arrow_cursor();
                    on_event(PopupEvent::MouseMove { screen_pos })
                }
                WM_LBUTTONDOWN => on_event(PopupEvent::MouseDown {
                    screen_pos,
                    button: 1,
                }),
                WM_RBUTTONDOWN => on_event(PopupEvent::MouseDown {
                    screen_pos,
                    button: 2,
                }),
                WM_LBUTTONUP => on_event(PopupEvent::MouseUp {
                    screen_pos,
                    button: 1,
                }),
                WM_RBUTTONUP => on_event(PopupEvent::MouseUp {
                    screen_pos,
                    button: 2,
                }),
                WM_KEYDOWN => {
                    let key = match msg.wParam as u16 {
                        VK_ESCAPE => PopupKey::Escape,
                        VK_UP => PopupKey::Up,
                        VK_DOWN => PopupKey::Down,
                        VK_LEFT => PopupKey::Left,
                        VK_RIGHT => PopupKey::Right,
                        VK_RETURN => PopupKey::Return,
                        _ => PopupKey::Other,
                    };
                    on_event(PopupEvent::KeyDown { key })
                }
                _ => {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                    PopupLoopControl::Continue
                }
            };

            if control == PopupLoopControl::Break || should_stop() {
                break;
            }
        }
        drop(capture);
        // Discard any residual mouse button messages queued during the modal menu.
        let mut discard_msg: MSG = std::mem::zeroed();
        while PeekMessageW(
            &mut discard_msg,
            std::ptr::null_mut(),
            WM_LBUTTONDOWN,
            WM_RBUTTONUP,
            PM_REMOVE,
        ) != 0
        {}
    }
    quit
}

pub fn repost_quit(code: i32) {
    // SAFETY: `PostQuitMessage` takes an integer exit code only and posts `WM_QUIT` to the calling
    // thread's queue; it touches no memory of ours and has no precondition.
    unsafe { PostQuitMessage(code) };
}
