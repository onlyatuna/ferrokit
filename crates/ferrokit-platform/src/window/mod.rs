// SPDX-License-Identifier: LGPL-3.0-only
// On Windows `HWND` is a pointer; elsewhere it is already an `isize`.
#![cfg_attr(not(windows), allow(clippy::unnecessary_cast))]

pub mod macos;
pub mod macos_input;
#[cfg(windows)]
pub(crate) mod region;
pub mod wayland;
pub mod x11;
#[cfg(not(windows))]
pub use crate::HWND;
#[allow(unused_imports)]
use crate::presenter::SurfacePresenter;
#[allow(unused_imports)]
use crate::surface::PlatformSurface;
#[allow(unused_imports)]
use crate::window_system_interface::{
    Delivery, KeyboardModifiers, MouseButton, WheelDelta, WindowSystemEvent,
    WindowSystemEventHandler,
};
#[allow(unused_imports)]
use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::event_loop::EventLoopHandle;
use ferrokit_core::object::ObjectId;
#[cfg(windows)]
use ferrokit_gui::geometry::primitives::Point;
use ferrokit_gui::geometry::primitives::{Rect, Size};
use std::collections::HashMap;
#[allow(unused_imports)]
use std::ptr;
#[cfg(windows)]
use std::sync::atomic::{AtomicI8, Ordering};
#[allow(unused_imports)]
use std::sync::{Mutex, Once, RwLock};
#[cfg(windows)]
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
#[cfg(windows)]
use windows_sys::Win32::Graphics::Dwm::DwmExtendFrameIntoClientArea;
#[cfg(windows)]
use windows_sys::Win32::Graphics::Gdi::{
    ClientToScreen, GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
};
#[cfg(windows)]
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
#[cfg(windows)]
use windows_sys::Win32::UI::Controls::{MARGINS, WM_MOUSELEAVE};
#[cfg(windows)]
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetCapture, GetKeyState, ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT,
    TrackMouseEvent, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
#[cfg(windows)]
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CS_DBLCLKS, CreateWindowExW, DefWindowProcW, DestroyWindow, GetCursorPos, GetForegroundWindow,
    GetMessageTime, GetWindowRect, HTBOTTOM, HTBOTTOMLEFT, HTBOTTOMRIGHT, HTCAPTION, HTCLIENT,
    HTLEFT, HTRIGHT, HTTOP, HTTOPLEFT, HTTOPRIGHT, IDC_ARROW, IsChild, IsIconic, IsZoomed,
    LoadCursorW, NCCALCSIZE_PARAMS, RegisterClassExW, SIZE_MAXIMIZED, SIZE_MINIMIZED,
    SIZE_RESTORED, SW_HIDE, SW_SHOW, SW_SHOWNOACTIVATE, SWP_FRAMECHANGED, SWP_NOACTIVATE,
    SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SetWindowPos, ShowWindow, WM_CLOSE, WM_CONTEXTMENU,
    WM_DESTROY, WM_DISPLAYCHANGE, WM_DPICHANGED, WM_ERASEBKGND, WM_GETMINMAXINFO, WM_KEYDOWN,
    WM_KEYUP, WM_KILLFOCUS, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDBLCLK,
    WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_MOVE, WM_NCCALCSIZE,
    WM_NCHITTEST, WM_PAINT, WM_RBUTTONDBLCLK, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SETCURSOR,
    WM_SETFOCUS, WM_SETTINGCHANGE, WM_SHOWWINDOW, WM_SIZE, WM_THEMECHANGED, WNDCLASSEXW,
    WS_CAPTION, WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_EX_APPWINDOW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_MAXIMIZEBOX, WS_MINIMIZEBOX,
    WS_OVERLAPPEDWINDOW, WS_POPUP, WS_THICKFRAME,
};
#[cfg(windows)]
use windows_sys::Win32::UI::WindowsAndMessaging::{
    WM_CAPTURECHANGED, WM_MOUSEHWHEEL, WM_SYSKEYDOWN, WM_SYSKEYUP,
};
bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct WindowFlags: u32 {
        const NORMAL                 = 0;
        const FRAMELESS              = 1 << 0;
        const STAYS_ON_TOP           = 1 << 1;
        const TOOL                   = 1 << 2;
        const LAYERED                = 1 << 3;
        const CLICK_THROUGH          = 1 << 4;
        const CUSTOM_FRAMELESS       = 1 << 5;
        /// `Qt::ToolTip`: a frameless, always-on-top tool window that is shown without taking
        /// activation (`WS_EX_NOACTIVATE` + `SW_SHOWNOACTIVATE`, `qwindowswindow.cpp:799-815,
        /// 1019, 2031-2036`). It never becomes the active window, so showing it does not move
        /// keyboard focus away from the window it annotates.
        const TOOLTIP                = 1 << 6;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CustomFramelessConfig {
    pub caption_height: i32,
    pub resize_border: i32,
}

impl Default for CustomFramelessConfig {
    fn default() -> Self {
        Self {
            caption_height: 32,
            resize_border: 8,
        }
    }
}
bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct PlatformWindowStateFlags: u32 {
        const NONE                 = 0;
        const WITHIN_SET_GEOMETRY  = 1 << 0;
        const WITHIN_SET_STYLE     = 1 << 1;
        const WITHIN_SET_PARENT    = 1 << 2;
        const RESIZE_MOVE_ACTIVE   = 1 << 3;
    }
}

pub struct SetGeometryGuard<'a> {
    flags: &'a std::cell::Cell<PlatformWindowStateFlags>,
}

impl<'a> SetGeometryGuard<'a> {
    pub fn new(flags: &'a std::cell::Cell<PlatformWindowStateFlags>) -> Self {
        let cur = flags.get();
        flags.set(cur | PlatformWindowStateFlags::WITHIN_SET_GEOMETRY);
        Self { flags }
    }
}

impl Drop for SetGeometryGuard<'_> {
    fn drop(&mut self) {
        // Critical: ONLY clear WITHIN_SET_GEOMETRY bit, never wipe out other flags set by re-entrant operations!
        let cur = self.flags.get();
        self.flags
            .set(cur & !PlatformWindowStateFlags::WITHIN_SET_GEOMETRY);
    }
}

static WINDOW_EVENT_BINDINGS: RwLock<Option<HashMap<isize, (EventLoopHandle, ObjectId)>>> =
    RwLock::new(None);

static WINDOW_FRAMELESS_CONFIGS: RwLock<Option<HashMap<isize, CustomFramelessConfig>>> =
    RwLock::new(None);

static WINDOW_SIZE_LIMITS: RwLock<Option<HashMap<isize, crate::size_limits::SizeLimits>>> =
    RwLock::new(None);

thread_local! {
    static WINDOW_EVENT_HANDLERS: std::cell::RefCell<HashMap<isize, Box<dyn WindowSystemEventHandler>>> =
        std::cell::RefCell::new(HashMap::new());
}
pub fn set_window_size_limits(hwnd: HWND, limits: crate::size_limits::SizeLimits) {
    let mut writer = ferrokit_core::poison::write_unpoisoned(&WINDOW_SIZE_LIMITS);
    if writer.is_none() {
        *writer = Some(HashMap::new());
    }
    if let Some(map) = writer.as_mut() {
        map.insert(hwnd as isize, limits);
    }
}

pub fn get_window_size_limits(hwnd: HWND) -> Option<crate::size_limits::SizeLimits> {
    let reader = ferrokit_core::poison::read_unpoisoned(&WINDOW_SIZE_LIMITS);
    reader
        .as_ref()
        .and_then(|map| map.get(&(hwnd as isize)).copied())
}

pub fn remove_window_size_limits(hwnd: HWND) {
    let mut writer = ferrokit_core::poison::write_unpoisoned(&WINDOW_SIZE_LIMITS);
    if let Some(map) = writer.as_mut() {
        map.remove(&(hwnd as isize));
    }
}

pub fn register_window_event_binding(hwnd: HWND, handle: EventLoopHandle, receiver: ObjectId) {
    let mut map = ferrokit_core::poison::write_unpoisoned(&WINDOW_EVENT_BINDINGS);
    if map.is_none() {
        *map = Some(HashMap::new());
    }
    map.as_mut()
        .unwrap()
        .insert(hwnd as isize, (handle, receiver));
}

// Only the Windows window procedure (and its test) calls this.
#[cfg_attr(not(windows), allow(dead_code))]
#[inline]
fn wheel_deltas(horizontal: bool, wheel_delta: i32) -> (i32, i32) {
    // Qt negates `WM_MOUSEHWHEEL`: its positive horizontal delta scrolls left, Win32's scrolls right.
    if horizontal {
        (-wheel_delta, 0)
    } else {
        (0, wheel_delta)
    }
}

pub fn unregister_window_event_binding(hwnd: HWND) {
    // Values removed from the shared registries are dropped only after every guard is released:
    // a Drop that reaches back into the registries would otherwise re-enter the same RwLock.
    let removed_binding = {
        let mut map = ferrokit_core::poison::write_unpoisoned(&WINDOW_EVENT_BINDINGS);
        map.as_mut().and_then(|m| m.remove(&(hwnd as isize)))
    };
    let removed_cfg = {
        let mut cfg_map = ferrokit_core::poison::write_unpoisoned(&WINDOW_FRAMELESS_CONFIGS);
        cfg_map.as_mut().and_then(|m| m.remove(&(hwnd as isize)))
    };
    let removed_limits = {
        let mut limits_map = ferrokit_core::poison::write_unpoisoned(&WINDOW_SIZE_LIMITS);
        limits_map.as_mut().and_then(|m| m.remove(&(hwnd as isize)))
    };
    drop((removed_binding, removed_cfg, removed_limits));
    let _ = WINDOW_CURSORS.try_with(|m| m.borrow_mut().remove(&(hwnd as isize)));
    let removed = WINDOW_EVENT_HANDLERS
        .try_with(|map| map.borrow_mut().remove(&(hwnd as isize)))
        .ok()
        .flatten();
    // While the window is dispatching, its running handler is out of the registry: it must be
    // dropped, not put back, when it returns (also when it replaced itself first and the
    // replacement was just removed above).
    let _ = ACTIVE_DISPATCH.try_with(|a| {
        if a.borrow().contains_key(&(hwnd as isize)) {
            let _ = UNREGISTERED_WHILE_ACTIVE.try_with(|u| u.borrow_mut().insert(hwnd as isize));
        }
    });
    // Dropping a handler may drop a window, which unregisters: outside every borrow.
    drop(removed);
    discard_pending_events(hwnd as isize);
}

#[inline]
fn get_window_event_binding(hwnd: HWND) -> Option<(EventLoopHandle, ObjectId)> {
    let map = ferrokit_core::poison::read_unpoisoned(&WINDOW_EVENT_BINDINGS);
    map.as_ref().and_then(|m| m.get(&(hwnd as isize)).cloned())
}

#[inline]
#[allow(dead_code)]
fn get_window_frameless_config(hwnd: HWND) -> Option<CustomFramelessConfig> {
    let map = ferrokit_core::poison::read_unpoisoned(&WINDOW_FRAMELESS_CONFIGS);
    map.as_ref().and_then(|m| m.get(&(hwnd as isize)).copied())
}

pub fn set_window_frameless_config(hwnd: HWND, config: CustomFramelessConfig) {
    let mut map = ferrokit_core::poison::write_unpoisoned(&WINDOW_FRAMELESS_CONFIGS);
    if map.is_none() {
        *map = Some(HashMap::new());
    }
    map.as_mut().unwrap().insert(hwnd as isize, config);
}

thread_local! {
    /// The cursor each window asked for (`QWindow::setCursor`); a window without an entry shows
    /// the application-wide cursor.
    static WINDOW_CURSORS: std::cell::RefCell<HashMap<isize, crate::cursor::CursorShape>> =
        std::cell::RefCell::new(HashMap::new());
}

/// The cursor `hwnd` asked for, if any (UI thread only).
pub fn window_cursor_shape(hwnd: HWND) -> Option<crate::cursor::CursorShape> {
    WINDOW_CURSORS
        .try_with(|m| m.borrow().get(&(hwnd as isize)).copied())
        .ok()
        .flatten()
}

pub fn set_window_cursor_shape(hwnd: HWND, shape: crate::cursor::CursorShape) {
    let _ = WINDOW_CURSORS.try_with(|m| m.borrow_mut().insert(hwnd as isize, shape));
}

/// `QWindow::unsetCursor`: the window asks for no cursor of its own.
pub fn unset_window_cursor_shape(hwnd: HWND) {
    let _ = WINDOW_CURSORS.try_with(|m| m.borrow_mut().remove(&(hwnd as isize)));
}

pub fn set_window_event_handler(hwnd: HWND, handler: Box<dyn WindowSystemEventHandler>) {
    let replaced = WINDOW_EVENT_HANDLERS
        .try_with(|map| map.borrow_mut().insert(hwnd as isize, handler))
        .ok()
        .flatten();
    drop(replaced);
}

static WINDOW_SYSTEM_EVENT_QUEUE: Mutex<Option<Vec<(isize, WindowSystemEvent)>>> = Mutex::new(None);

/// Private window message that tells a window's own thread to deliver the events posted for it
/// (`QWindowSystemInterface::flushWindowSystemEvents` is driven by the event dispatcher in Qt).
/// `PostMessageW` is thread-safe and the message is also dispatched by nested (modal) loops.
#[cfg(windows)]
const WM_FLUSH_WINDOW_SYSTEM_EVENTS: u32 =
    windows_sys::Win32::UI::WindowsAndMessaging::WM_APP + 0x1F;

// `hwnd` is a key into this crate's maps and is passed to `PostMessageW`, which takes any value.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn post_window_system_event(hwnd: HWND, event: WindowSystemEvent) {
    {
        let mut queue = WINDOW_SYSTEM_EVENT_QUEUE
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if queue.is_none() {
            *queue = Some(Vec::new());
        }
        if let Some(q) = queue.as_mut() {
            q.push((hwnd as isize, event));
        }
    }

    #[cfg(windows)]
    // SAFETY: PostMessageW takes no pointers (all arguments are plain integers/handle values) and,
    // per its documentation, fails with an error return for a handle that is not (or no longer) a
    // window, so a stale `hwnd` cannot cause UB here; the return value is intentionally ignored.
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW(
            hwnd,
            WM_FLUSH_WINDOW_SYSTEM_EVENTS,
            0,
            0,
        );
    }
    if let Some((handle, _)) = get_window_event_binding(hwnd) {
        handle.wake_up();
    }
}

/// Whether the calling thread owns the handler of `hwnd` (handlers are thread-local, so only that
/// thread can deliver its events).
fn has_local_handler(hwnd: isize) -> bool {
    WINDOW_EVENT_HANDLERS
        .try_with(|map| map.borrow().contains_key(&hwnd))
        .unwrap_or(false)
        || ACTIVE_DISPATCH
            .try_with(|a| a.borrow().contains_key(&hwnd))
            .unwrap_or(false)
}

/// Whether `hwnd` is a live window of another thread (its events wait for that thread's flush).
fn owned_by_other_thread(hwnd: isize) -> bool {
    #[cfg(windows)]
    {
        // SAFETY: GetWindowThreadProcessId takes no required pointers (null out-pid is allowed per
        // its documentation) and returns 0 for an invalid handle; GetCurrentThreadId has no
        // arguments. A stale `hwnd` therefore only yields tid 0.
        unsafe {
            let tid = windows_sys::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(
                hwnd as HWND,
                ptr::null_mut(),
            );
            tid != 0 && tid != windows_sys::Win32::System::Threading::GetCurrentThreadId()
        }
    }
    #[cfg(not(windows))]
    {
        let _ = hwnd;
        true
    }
}

/// Delivers the posted events whose handlers live on the calling thread; the others stay queued
/// for their own thread.
pub fn flush_window_system_events() -> bool {
    let events: Vec<(isize, WindowSystemEvent)> = {
        let mut queue = WINDOW_SYSTEM_EVENT_QUEUE
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let Some(q) = queue.as_mut() else {
            return false;
        };
        let mut mine = Vec::new();
        let mut kept = Vec::new();
        let mut dropped = Vec::new();
        for entry in std::mem::take(q) {
            if has_local_handler(entry.0) {
                mine.push(entry);
            } else if owned_by_other_thread(entry.0) {
                kept.push(entry);
            } else {
                // No handler anywhere: the window is gone or never had one.
                dropped.push(entry);
            }
        }
        *q = kept;
        drop(queue);
        drop(dropped);
        mine
    };

    if events.is_empty() {
        return false;
    }

    for (hwnd_isize, event) in events {
        send_window_system_event_immediately(hwnd_isize as HWND, event);
    }
    true
}

thread_local! {
    /// HWNDs currently inside the native sizing loop (`WM_ENTERSIZEMOVE`..`WM_EXITSIZEMOVE`).
    static INTERACTIVE_RESIZE_HWNDS: std::cell::RefCell<std::collections::HashSet<isize>> =
        std::cell::RefCell::new(std::collections::HashSet::new());
}

/// Whether `hwnd` is inside the native interactive sizing loop (UI thread only).
pub fn is_interactive_resize(hwnd: HWND) -> bool {
    INTERACTIVE_RESIZE_HWNDS
        .try_with(|s| s.borrow().contains(&(hwnd as isize)))
        .unwrap_or(false)
}

#[cfg(windows)]
fn set_interactive_resize_flag(hwnd: HWND, active: bool) {
    let _ = INTERACTIVE_RESIZE_HWNDS.try_with(|s| {
        let mut s = s.borrow_mut();
        if active {
            s.insert(hwnd as isize);
        } else {
            s.remove(&(hwnd as isize));
        }
    });
}

thread_local! {
    /// `QGuiApplicationPrivate::mouse_buttons`: the buttons the application has seen pressed and
    /// not yet released (UI thread only).
    static APP_MOUSE_BUTTONS: std::cell::Cell<ferrokit_core::event::MouseButtons> =
        const { std::cell::Cell::new(ferrokit_core::event::MouseButtons::NO_BUTTON) };
}

/// `QGuiApplication::mouseButtons` (qguiapplication.cpp:1974): the buttons the application has
/// seen pressed and not yet released. UI thread only.
pub fn app_mouse_buttons() -> ferrokit_core::event::MouseButtons {
    APP_MOUSE_BUTTONS
        .try_with(|b| b.get())
        .unwrap_or(ferrokit_core::event::MouseButtons::NO_BUTTON)
}

/// Stores the button state of a mouse event (`QGuiApplicationPrivate::mouse_buttons = e->buttons`,
/// qguiapplication.cpp:2455): the widget window layer calls it for every press and release, so
/// backends that do not track buttons themselves (and tests with a fake window) report them too.
pub fn set_app_mouse_buttons(buttons: ferrokit_core::event::MouseButtons) {
    let _ = APP_MOUSE_BUTTONS.try_with(|b| b.set(buttons));
}

thread_local! {
    /// `QGuiApplicationPrivate::lastCursorPosition`: the global position of the last mouse event
    /// (UI thread only).
    static LAST_CURSOR_POSITION: std::cell::Cell<ferrokit_gui::geometry::primitives::Point> =
        const { std::cell::Cell::new(ferrokit_gui::geometry::primitives::Point { x: 0, y: 0 }) };
}

/// `QGuiApplicationPrivate::lastCursorPosition` (logical global position).
pub fn last_cursor_position() -> ferrokit_gui::geometry::primitives::Point {
    LAST_CURSOR_POSITION
        .try_with(|p| p.get())
        .unwrap_or(ferrokit_gui::geometry::primitives::Point { x: 0, y: 0 })
}

/// Records the global position of a mouse event (qguiapplication.cpp:2445, 2605).
pub fn set_last_cursor_position(pos: ferrokit_gui::geometry::primitives::Point) {
    let _ = LAST_CURSOR_POSITION.try_with(|p| p.set(pos));
}

thread_local! {
    /// `lastMouseMovePos` in `QWindowsPointerHandler::translateMouseEvent`: the window and native
    /// global position of the last `WM_MOUSEMOVE`; cleared when the window gets `WM_MOUSELEAVE`.
    static LAST_MOUSE_MOVE: std::cell::Cell<Option<(isize, i32, i32)>> = const { std::cell::Cell::new(None) };
}

/// Qt discards a buttonless `WM_MOUSEMOVE` at the position of the previous one
/// (qwindowspointerhandler.cpp:800-809): Windows sends such moves without the mouse moving, e.g.
/// when the capture is released or a window appears under the cursor.
#[cfg(windows)]
fn is_repeated_buttonless_move(hwnd: HWND, wparam: WPARAM, lparam: LPARAM) -> bool {
    let mut pt = windows_sys::Win32::Foundation::POINT {
        x: get_x_lparam(lparam),
        y: get_y_lparam(lparam),
    };
    // SAFETY: `hwnd` is the window that received this message (passed by the wndproc) and user32
    // validates it; `pt` is a local POINT, a valid out pointer for the duration of the call.
    unsafe { ClientToScreen(hwnd, &mut pt) };
    let current = Some((hwnd as isize, pt.x, pt.y));
    let previous = LAST_MOUSE_MOVE.with(|last| last.replace(current));
    wparam == 0 && previous == current
}

/// The button a `WM_{L,R,M}BUTTON{DOWN,UP,DBLCLK}` message is about (already the logical button:
/// Windows swaps for `SM_SWAPBUTTON` before sending).
#[cfg(windows)]
fn button_of_message(msg: u32) -> ferrokit_core::event::MouseButtons {
    use ferrokit_core::event::MouseButtons;
    match msg {
        WM_LBUTTONDOWN | WM_LBUTTONUP | WM_LBUTTONDBLCLK => MouseButtons::LEFT,
        WM_RBUTTONDOWN | WM_RBUTTONUP | WM_RBUTTONDBLCLK => MouseButtons::RIGHT,
        WM_MBUTTONDOWN | WM_MBUTTONUP | WM_MBUTTONDBLCLK => MouseButtons::MIDDLE,
        _ => MouseButtons::NO_BUTTON,
    }
}

#[cfg(windows)]
fn track_button_press(msg: u32) {
    let _ = APP_MOUSE_BUTTONS.try_with(|b| b.set(b.get().union(button_of_message(msg))));
}

#[cfg(windows)]
fn track_button_release(msg: u32) {
    let bit = button_of_message(msg);
    let _ = APP_MOUSE_BUTTONS
        .try_with(|b| b.set(ferrokit_core::event::MouseButtons(b.get().0 & !bit.0)));
}

/// `ReleaseCapture`, then post `SC_DRAGMOVE` / `SC_SIZE*`: `QWindowsWindow::startSystemMove` /
/// `startSystemResize` (qwindowswindow.cpp:3303-3313) do exactly this and nothing else. The
/// native loop swallows the button release; `WM_EXITSIZEMOVE` reconciles the pressed buttons
/// ([`sync_mouse_buttons_after_move_loop`]).
#[cfg(windows)]
fn post_system_command(hwnd: HWND, command: usize) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_SYSCOMMAND};
    // SAFETY: ReleaseCapture takes no arguments and only affects this thread's mouse capture;
    // PostMessageW takes no pointers (user32 validates `hwnd` and fails for a stale one).
    unsafe {
        ReleaseCapture();
        PostMessageW(hwnd, WM_SYSCOMMAND, command, 0);
    }
}

#[cfg(windows)]
thread_local! {
    /// Windows whose capture is the automatic one of a button press (`QWindowsWindow::AutoMouseCapture`);
    /// an explicit `set_mouse_grab_enabled` clears it, so the release of the last button no
    /// longer lets go (qwindowswindow.cpp:3283).
    static AUTO_MOUSE_CAPTURE: std::cell::RefCell<std::collections::HashSet<isize>> =
        std::cell::RefCell::new(std::collections::HashSet::new());
}

#[cfg(windows)]
fn auto_capture_flag(hwnd: isize) -> bool {
    AUTO_MOUSE_CAPTURE
        .try_with(|s| s.borrow().contains(&hwnd))
        .unwrap_or(false)
}

#[cfg(windows)]
fn set_auto_capture_flag(hwnd: isize, on: bool) {
    let _ = AUTO_MOUSE_CAPTURE.try_with(|s| {
        if on {
            s.borrow_mut().insert(hwnd);
        } else {
            s.borrow_mut().remove(&hwnd);
        }
    });
}

/// `QWindowsPointerHandler::handleCaptureRelease` (qwindowspointerhandler.cpp:341-345): a press
/// with no capture captures the mouse automatically.
#[cfg(windows)]
fn capture_on_press(hwnd: HWND) {
    // SAFETY: `hwnd` is the window currently handling a mouse press (called from the wndproc) and
    // user32 validates it; SetCapture/GetCapture take no pointers and run on the window's thread.
    unsafe {
        if GetCapture() != hwnd {
            SetCapture(hwnd);
            set_auto_capture_flag(hwnd as isize, true);
        }
    }
}

/// Release the automatic capture once the release message reports no button held
/// (qwindowspointerhandler.cpp:342-349: `!mouseButtons`, which `translateMouseEvent` takes from the
/// message's key state, not from `QGuiApplication::mouseButtons()`); an explicit grab stays.
#[cfg(windows)]
fn release_capture_when_idle(hwnd: HWND, held: ferrokit_core::event::MouseButtons) {
    let idle = held.is_empty();
    // SAFETY: as in `capture_on_press`: `hwnd` is the window handling this message, validated by
    // user32; GetCapture/ReleaseCapture take no pointers.
    unsafe {
        if idle && GetCapture() == hwnd && auto_capture_flag(hwnd as isize) {
            set_auto_capture_flag(hwnd as isize, false);
            ReleaseCapture();
        }
    }
}

/// `QWindowsWindow::setMouseGrabEnabled` (qwindowswindow.cpp:3269-3292): an explicit grab or
/// release; it overrides the automatic capture. A hidden window cannot grab.
#[cfg(windows)]
fn set_mouse_grab_enabled_native(hwnd: HWND, grab: bool) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::IsWindowVisible;
    if hwnd.is_null() {
        return false;
    }
    // SAFETY: `hwnd` is non-null (checked above) and validated by user32 (stale handles make the
    // calls fail harmlessly); none of the calls takes a pointer.
    unsafe {
        if grab && IsWindowVisible(hwnd) == 0 {
            return false;
        }
        set_auto_capture_flag(hwnd as isize, false);
        if (GetCapture() == hwnd) != grab {
            if grab {
                SetCapture(hwnd);
            } else {
                ReleaseCapture();
            }
        }
        // `SetCapture` can be refused (another thread holds the mouse button down, say).
        grab && GetCapture() == hwnd
    }
}

/// `QWindowsPointerHandler::queryMouseButtons` (qwindowspointerhandler.cpp:226-241): the physical
/// buttons (`GetAsyncKeyState`), left and right swapped when `SM_SWAPBUTTON` is set, X buttons
/// included.
#[cfg(windows)]
fn query_mouse_buttons() -> ferrokit_core::event::MouseButtons {
    use ferrokit_core::event::MouseButtons;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, VK_LBUTTON, VK_MBUTTON, VK_RBUTTON, VK_XBUTTON1, VK_XBUTTON2,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_SWAPBUTTON};
    // SAFETY: GetSystemMetrics and GetAsyncKeyState take only integer arguments and read global
    // input state; they have no preconditions.
    unsafe {
        let swapped = GetSystemMetrics(SM_SWAPBUTTON) != 0;
        let down = |vk| GetAsyncKeyState(vk as i32) < 0;
        let mut result = MouseButtons::NO_BUTTON;
        if down(VK_LBUTTON) {
            result = result.union(if swapped {
                MouseButtons::RIGHT
            } else {
                MouseButtons::LEFT
            });
        }
        if down(VK_RBUTTON) {
            result = result.union(if swapped {
                MouseButtons::LEFT
            } else {
                MouseButtons::RIGHT
            });
        }
        if down(VK_MBUTTON) {
            result = result.union(MouseButtons::MIDDLE);
        }
        if down(VK_XBUTTON1) {
            result = result.union(MouseButtons::BACK);
        }
        if down(VK_XBUTTON2) {
            result = result.union(MouseButtons::FORWARD);
        }
        result
    }
}

/// `QWindowsContext::handleExitSizeMove` (qwindowscontext.cpp:1261-1290). The native move/size
/// loop swallows the button release that ends a drag, so the application still believes the
/// button is down. When the application's buttons differ from the physical ones, every one of
/// left, right and middle that the application has down but the system does not gets a release
/// carrying the physical buttons (`QGuiApplicationPrivate::mouse_buttons = e->buttons`,
/// qguiapplication.cpp:2431, which also takes over buttons the application never saw and drops
/// stale X buttons): as a normal `WM_*BUTTONUP` when the cursor is inside the client area of the
/// window (`platWin->geometry()`), and without a mouse event for the window when it is outside
/// (Qt: `NonClientAreaMouseButtonRelease`, which ferrokit has no counterpart for). A button that
/// is still physically down (the loop was cancelled with Esc) is left alone. X buttons alone
/// never trigger a release, as in Qt.
///
/// # Safety
/// `hwnd` must be a live ferrokit window owned by the calling thread: button-up messages are fed
/// straight to [`native_window_proc`], whose contract then applies.
#[cfg(windows)]
unsafe fn sync_mouse_buttons_after_move_loop(hwnd: HWND) {
    // SAFETY: caller guarantees hwnd is live and owned by calling thread.
    unsafe {
        use ferrokit_core::event::MouseButtons;
        let current = query_mouse_buttons();
        let app = APP_MOUSE_BUTTONS
            .try_with(|b| b.get())
            .unwrap_or(MouseButtons::NO_BUTTON);
        if current == app {
            return;
        }
        let mut pt = windows_sys::Win32::Foundation::POINT { x: 0, y: 0 };
        GetCursorPos(&mut pt);
        // `QWindowsWindow::geometry()` is the client area in screen coordinates.
        let mut client: windows_sys::Win32::Foundation::RECT = std::mem::zeroed();
        windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut client);
        let mut origin = windows_sys::Win32::Foundation::POINT {
            x: client.left,
            y: client.top,
        };
        ClientToScreen(hwnd, &mut origin);
        let inside = pt.x >= origin.x
            && pt.x < origin.x + (client.right - client.left)
            && pt.y >= origin.y
            && pt.y < origin.y + (client.bottom - client.top);
        let mut local = pt;
        windows_sys::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut local);
        let lparam = ((local.x & 0xffff) | ((local.y & 0xffff) << 16)) as LPARAM;
        for (flag, up) in [
            (MouseButtons::LEFT, WM_LBUTTONUP),
            (MouseButtons::RIGHT, WM_RBUTTONUP),
            (MouseButtons::MIDDLE, WM_MBUTTONUP),
        ] {
            if app.contains(flag) && !current.contains(flag) {
                // Every event of this loop carries the physical buttons.
                let _ = APP_MOUSE_BUTTONS.try_with(|b| b.set(current));
                if inside {
                    native_window_proc(hwnd, up, 0, lparam);
                }
            }
        }
    }
}

/// Mirrors the native sizing-loop state onto the surfaces before they are resized.
#[cfg(windows)]
fn sync_interactive_resize(
    hwnd: HWND,
    presenter: &mut Option<crate::presenter::WindowsPresenter>,
    layered_surface: &mut Option<crate::surface::WindowsSurface>,
) {
    let active = is_interactive_resize(hwnd);
    if let Some(p) = presenter {
        p.set_interactive_resize(active);
    }
    if let Some(crate::surface::WindowsSurface::Layered(s)) = layered_surface {
        s.set_interactive_resize(active);
    }
}

thread_local! {
    /// Windows whose handler is running right now. The value holds the events that arrived for
    /// that window meanwhile (`SendEvent` re-entrancy); the outermost call delivers them in order
    /// once its handler returns. Events for any *other* window are delivered at once, so a nested
    /// modal loop started from a handler keeps feeding its own window.
    static ACTIVE_DISPATCH: std::cell::RefCell<HashMap<isize, std::collections::VecDeque<WindowSystemEvent>>> =
        std::cell::RefCell::new(HashMap::new());
    /// Windows unregistered while their handler was running; the handler is dropped, not restored.
    static UNREGISTERED_WHILE_ACTIVE: std::cell::RefCell<std::collections::HashSet<isize>> =
        std::cell::RefCell::new(std::collections::HashSet::new());
}

/// Marks `hwnd` as dispatching for its lifetime. Dropping it (also while unwinding out of a
/// panicking handler) makes the window deliverable again and discards what was queued behind the
/// failed handler.
struct ActiveDispatch(isize);

impl Drop for ActiveDispatch {
    fn drop(&mut self) {
        let queued = ACTIVE_DISPATCH
            .try_with(|a| a.borrow_mut().remove(&self.0))
            .ok()
            .flatten();
        let _ = UNREGISTERED_WHILE_ACTIVE.try_with(|u| u.borrow_mut().remove(&self.0));
        // Dropping events may run arbitrary `Drop` code: do it outside the borrow.
        drop(queued);
    }
}

/// Runs one handler call with the handler taken out of the registry, so the handler may register,
/// replace or unregister handlers (create or drop a window) while it runs. The handler goes back
/// afterwards, also when it panics, unless it was replaced or its window was unregistered.
struct RunningHandler {
    hwnd: isize,
    handler: Option<Box<dyn WindowSystemEventHandler>>,
}

impl Drop for RunningHandler {
    fn drop(&mut self) {
        let Some(handler) = self.handler.take() else {
            return;
        };
        let unregistered = UNREGISTERED_WHILE_ACTIVE
            .try_with(|u| u.borrow_mut().remove(&self.hwnd))
            .unwrap_or(false);
        let leftover = WINDOW_EVENT_HANDLERS
            .try_with(|map| {
                let mut map = map.borrow_mut();
                if unregistered || map.contains_key(&self.hwnd) {
                    // Unregistered, or replaced by a handler set while this one ran.
                    Some(handler)
                } else {
                    map.insert(self.hwnd, handler);
                    None
                }
            })
            .unwrap_or(None);
        drop(leftover);
    }
}

fn run_handler(hwnd: isize, event: WindowSystemEvent) {
    let taken = WINDOW_EVENT_HANDLERS
        .try_with(|map| map.borrow_mut().remove(&hwnd))
        .ok()
        .flatten();
    let Some(handler) = taken else {
        return;
    };
    let mut running = RunningHandler {
        hwnd,
        handler: Some(handler),
    };
    if let Some(handler) = running.handler.as_mut() {
        handler.handle_window_event(event);
    }
}

/// Delivers `event` to `hwnd`'s handler synchronously on the window's thread and returns the
/// `DROPEFFECT_*` the handler accepted it with (`0` = refused). Returns `0` if the window is not
/// on the calling thread, has no handler, or its handler is already running on the stack.
///
/// Unlike [`send_window_system_event_immediately`], an event that arrives while the window's
/// handler is already running is deliberately not queued. OLE needs the answer before
/// `IDropTarget::DragEnter` / `DragOver` / `Drop` returns, and the handler is taken out of the
/// registry for the outer call, so it cannot run now. Running a queued `Drop` after OLE was
/// already told `DROPEFFECT_NONE` would perform a drop the source believes was refused.
pub fn send_window_system_event_accepted(hwnd: HWND, event: WindowSystemEvent) -> u32 {
    use std::collections::hash_map::Entry;
    if !is_window_thread(hwnd) {
        post_window_system_event(hwnd, event);
        return 0;
    }
    let key = hwnd as isize;
    let entered = ACTIVE_DISPATCH
        .try_with(|active| match active.borrow_mut().entry(key) {
            Entry::Occupied(_) => false,
            Entry::Vacant(v) => {
                v.insert(std::collections::VecDeque::new());
                true
            }
        })
        .unwrap_or(false);
    if !entered {
        return 0;
    }
    let _active = ActiveDispatch(key);
    let taken = WINDOW_EVENT_HANDLERS
        .try_with(|map| map.borrow_mut().remove(&key))
        .ok()
        .flatten();
    let accepted = if let Some(handler) = taken {
        let mut running = RunningHandler {
            hwnd: key,
            handler: Some(handler),
        };
        running
            .handler
            .as_mut()
            .map_or(0, |h| h.handle_window_event_accepted(event))
    } else {
        0
    };
    while let Some(ev) = ACTIVE_DISPATCH
        .try_with(|active| {
            active
                .borrow_mut()
                .get_mut(&key)
                .and_then(|q| q.pop_front())
        })
        .unwrap_or(None)
    {
        run_handler(key, ev);
    }
    accepted
}

pub fn send_window_system_event_immediately(hwnd: HWND, event: WindowSystemEvent) {
    use std::collections::hash_map::Entry;
    let key = hwnd as isize;
    let first = ACTIVE_DISPATCH
        .try_with(|active| match active.borrow_mut().entry(key) {
            Entry::Occupied(mut e) => {
                e.get_mut().push_back(event);
                None
            }
            Entry::Vacant(v) => {
                v.insert(std::collections::VecDeque::new());
                Some(event)
            }
        })
        .unwrap_or(None);
    let Some(mut event) = first else {
        return;
    };

    let _active = ActiveDispatch(key);
    loop {
        run_handler(key, event);
        let next = ACTIVE_DISPATCH
            .try_with(|active| {
                active
                    .borrow_mut()
                    .get_mut(&key)
                    .and_then(|q| q.pop_front())
            })
            .unwrap_or(None);
        match next {
            Some(ev) => event = ev,
            None => break,
        }
    }
}

/// Drops every event still queued for `hwnd` (a destroyed window must not receive them, and a
/// recycled handle must not inherit them).
fn discard_pending_events(hwnd: isize) {
    let discarded: Vec<WindowSystemEvent> = {
        let mut queue = WINDOW_SYSTEM_EVENT_QUEUE
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut out = Vec::new();
        if let Some(q) = queue.as_mut() {
            let mut kept = Vec::with_capacity(q.len());
            for (h, ev) in q.drain(..) {
                if h == hwnd {
                    out.push(ev);
                } else {
                    kept.push((h, ev));
                }
            }
            *q = kept;
        }
        out
    };
    drop(discarded);
    let nested: Option<Vec<WindowSystemEvent>> = ACTIVE_DISPATCH
        .try_with(|a| {
            a.borrow_mut()
                .get_mut(&hwnd)
                .map(|q| q.drain(..).collect::<Vec<_>>())
        })
        .unwrap_or(None);
    drop(nested);
}
fn is_window_thread(hwnd: HWND) -> bool {
    #[cfg(windows)]
    {
        if hwnd.is_null() {
            return ferrokit_core::object::ThreadContext::is_main_thread();
        }
        // SAFETY: GetWindowThreadProcessId takes a window handle (validated by user32; `hwnd` is
        // non-null here) and a null out-pid, which its documentation allows; GetCurrentThreadId has
        // no arguments.
        unsafe {
            let win_tid = windows_sys::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(
                hwnd,
                ptr::null_mut(),
            );
            let cur_tid = windows_sys::Win32::System::Threading::GetCurrentThreadId();
            win_tid == cur_tid
        }
    }
    #[cfg(not(windows))]
    {
        let _ = hwnd;
        ferrokit_core::object::ThreadContext::is_main_thread()
    }
}

pub fn dispatch_window_system_event(delivery: Delivery, hwnd: HWND, event: WindowSystemEvent) {
    match delivery {
        Delivery::Synchronous => {
            send_window_system_event_immediately(hwnd, event);
        }
        Delivery::Asynchronous => {
            post_window_system_event(hwnd, event);
        }
        Delivery::Default => {
            if is_window_thread(hwnd) {
                send_window_system_event_immediately(hwnd, event);
            } else {
                post_window_system_event(hwnd, event);
            }
        }
    }
}

pub fn handle_geometry_change(delivery: Delivery, hwnd: HWND, geometry: Rect) {
    let size = Size::new(geometry.width, geometry.height);
    dispatch_window_system_event(
        delivery,
        hwnd,
        WindowSystemEvent::GeometryChange { geometry },
    );
    dispatch_window_system_event(delivery, hwnd, WindowSystemEvent::Resize { size });
}

#[cfg(windows)]
pub fn set_dpi_awareness() -> bool {
    // SAFETY: SetProcessDpiAwarenessContext takes a predefined context constant, not a pointer; it
    // only changes process state and returns 0 (handled by the caller) if already set or refused.
    unsafe {
        use windows_sys::Win32::UI::HiDpi::{
            DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
        };
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) != 0
    }
}

#[cfg(not(windows))]
pub fn set_dpi_awareness() -> bool {
    true
}

#[cfg(not(windows))]
pub type NativeWindow = crate::platform_window::GenericWindow;
#[allow(dead_code)]
#[inline]
pub(crate) fn edges_to_win_orientation(edges: crate::platform_window::WindowEdges) -> usize {
    use crate::platform_window::WindowEdges;
    if edges == WindowEdges::LEFT {
        0xf001 // SC_SIZELEFT
    } else if edges == WindowEdges::RIGHT {
        0xf002 // SC_SIZERIGHT
    } else if edges == WindowEdges::TOP {
        0xf003 // SC_SIZETOP
    } else if edges == (WindowEdges::TOP | WindowEdges::LEFT) {
        0xf004 // SC_SIZETOPLEFT
    } else if edges == (WindowEdges::TOP | WindowEdges::RIGHT) {
        0xf005 // SC_SIZETOPRIGHT
    } else if edges == WindowEdges::BOTTOM {
        0xf006 // SC_SIZEBOTTOM
    } else if edges == (WindowEdges::BOTTOM | WindowEdges::LEFT) {
        0xf007 // SC_SIZEBOTTOMLEFT
    } else if edges == (WindowEdges::BOTTOM | WindowEdges::RIGHT) {
        0xf008 // SC_SIZEBOTTOMRIGHT
    } else {
        0xf000 // SC_SIZE
    }
}
pub fn post_system_move(_hwnd: isize) -> bool {
    #[cfg(windows)]
    if _hwnd != 0 {
        post_system_command(_hwnd as HWND, 0xF012 /*SC_DRAGMOVE*/);
        return true;
    }
    false
}

pub fn post_system_resize(_hwnd: isize, _edges: crate::platform_window::WindowEdges) -> bool {
    #[cfg(windows)]
    if _hwnd != 0 {
        post_system_command(_hwnd as HWND, edges_to_win_orientation(_edges));
        return true;
    }
    false
}

pub fn calc_frameless_edge(
    _hwnd: isize,
    pos: ferrokit_gui::geometry::primitives::Point,
    locked: bool,
) -> crate::platform_window::WindowEdges {
    if locked {
        return crate::platform_window::WindowEdges::empty();
    }
    const M: i32 = 8;
    #[cfg(windows)]
    let (w, h) = crate::window::macos::live_window_size(_hwnd as usize).unwrap_or_else(|| {
        if _hwnd != 0 {
            // SAFETY: RECT is a plain integer struct, so zeroed is valid; `rect` is a local out pointer
            // valid for the call; GetClientRect validates `_hwnd` (non-zero) and on failure leaves the
            // zeroed rect.
            unsafe {
                use windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect;
                let mut rect: windows_sys::Win32::Foundation::RECT = std::mem::zeroed();
                GetClientRect(_hwnd as HWND, &mut rect);
                let dpr = get_window_dpr(_hwnd as HWND);
                (
                    ((rect.right - rect.left) as f32 / dpr).round() as i32,
                    ((rect.bottom - rect.top) as f32 / dpr).round() as i32,
                )
            }
        } else {
            (600, 400)
        }
    });
    #[cfg(not(windows))]
    let (w, h) = crate::window::macos::live_window_size(_hwnd as usize).unwrap_or((600, 400));

    let mut edges = crate::platform_window::WindowEdges::empty();
    if pos.x < M {
        edges |= crate::platform_window::WindowEdges::LEFT;
    } else if pos.x > w - M {
        edges |= crate::platform_window::WindowEdges::RIGHT;
    }
    if pos.y < M {
        edges |= crate::platform_window::WindowEdges::TOP;
    } else if pos.y > h - M {
        edges |= crate::platform_window::WindowEdges::BOTTOM;
    }
    edges
}

#[cfg(all(test, windows))]
mod tests;
#[cfg(windows)]
mod win32;
#[cfg(windows)]
mod win32_wndproc;

#[cfg(windows)]
pub use win32::*;
#[cfg(windows)]
#[allow(unused_imports)]
use win32::{ensure_native_window_class_registered, topmost_insert_after};
#[cfg(windows)]
pub use win32_wndproc::get_window_dpr;
#[cfg(windows)]
#[allow(unused_imports)]
use win32_wndproc::{
    LayeredNeeds, apply_system_window_opacity, get_x_lparam, get_y_lparam, native_window_proc,
};
