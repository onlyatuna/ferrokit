// SPDX-License-Identifier: LGPL-3.0-only
#[allow(unused_imports)]
use crate::presenter::SurfacePresenter;
#[allow(unused_imports)]
use crate::surface::PlatformSurface;
#[allow(unused_imports)]
use crate::window_system_interface::{
    Delivery, KeyboardModifiers, MouseButton, WheelDelta, WindowSystemEvent,
    WindowSystemEventHandler,
};
#[cfg(not(windows))]
pub use crate::HWND;
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
    ClientToScreen, GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
#[cfg(windows)]
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
#[cfg(windows)]
use windows_sys::Win32::UI::Controls::{MARGINS, WM_MOUSELEAVE};
#[cfg(windows)]
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetCapture, GetKeyState, ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE,
    TRACKMOUSEEVENT, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
#[cfg(windows)]
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetCursorPos, GetForegroundWindow,
    GetMessageTime, GetWindowRect, IsChild, IsIconic, IsZoomed, LoadCursorW, RegisterClassExW,
    SetWindowPos, ShowWindow, CS_DBLCLKS, HTBOTTOM, HTBOTTOMLEFT, HTBOTTOMRIGHT, HTCAPTION,
    HTCLIENT, HTLEFT, HTRIGHT, HTTOP, HTTOPLEFT, HTTOPRIGHT, IDC_ARROW, NCCALCSIZE_PARAMS,
    SIZE_MAXIMIZED, SIZE_MINIMIZED, SIZE_RESTORED, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOSIZE, SWP_NOZORDER, SW_HIDE, SW_SHOW, SW_SHOWNOACTIVATE, WM_CLOSE, WM_CONTEXTMENU,
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
    WM_CANCELMODE, WM_CAPTURECHANGED, WM_MOUSEHWHEEL, WM_SYSKEYDOWN, WM_SYSKEYUP,
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
    let mut writer = WINDOW_SIZE_LIMITS.write().unwrap();
    if writer.is_none() {
        *writer = Some(HashMap::new());
    }
    if let Some(map) = writer.as_mut() {
        map.insert(hwnd as isize, limits);
    }
}

pub fn get_window_size_limits(hwnd: HWND) -> Option<crate::size_limits::SizeLimits> {
    let reader = WINDOW_SIZE_LIMITS.read().unwrap();
    reader
        .as_ref()
        .and_then(|map| map.get(&(hwnd as isize)).copied())
}

pub fn remove_window_size_limits(hwnd: HWND) {
    let mut writer = WINDOW_SIZE_LIMITS.write().unwrap();
    if let Some(map) = writer.as_mut() {
        map.remove(&(hwnd as isize));
    }
}

pub fn register_window_event_binding(hwnd: HWND, handle: EventLoopHandle, receiver: ObjectId) {
    let mut map = WINDOW_EVENT_BINDINGS.write().unwrap();
    if map.is_none() {
        *map = Some(HashMap::new());
    }
    map.as_mut()
        .unwrap()
        .insert(hwnd as isize, (handle, receiver));
}

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
        let mut map = WINDOW_EVENT_BINDINGS.write().unwrap();
        map.as_mut().and_then(|m| m.remove(&(hwnd as isize)))
    };
    let removed_cfg = {
        let mut cfg_map = WINDOW_FRAMELESS_CONFIGS.write().unwrap();
        cfg_map.as_mut().and_then(|m| m.remove(&(hwnd as isize)))
    };
    let removed_limits = {
        let mut limits_map = WINDOW_SIZE_LIMITS.write().unwrap();
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
    let map = WINDOW_EVENT_BINDINGS.read().unwrap();
    map.as_ref().and_then(|m| m.get(&(hwnd as isize)).cloned())
}

#[inline]
#[allow(dead_code)]
fn get_window_frameless_config(hwnd: HWND) -> Option<CustomFramelessConfig> {
    let map = WINDOW_FRAMELESS_CONFIGS.read().unwrap();
    map.as_ref().and_then(|m| m.get(&(hwnd as isize)).copied())
}

pub fn set_window_frameless_config(hwnd: HWND, config: CustomFramelessConfig) {
    let mut map = WINDOW_FRAMELESS_CONFIGS.write().unwrap();
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
const WM_FLUSH_WINDOW_SYSTEM_EVENTS: u32 = windows_sys::Win32::UI::WindowsAndMessaging::WM_APP + 0x1F;

pub fn post_window_system_event(hwnd: HWND, event: WindowSystemEvent) {
    {
        let mut queue = WINDOW_SYSTEM_EVENT_QUEUE.lock().unwrap_or_else(|e| e.into_inner());
        if queue.is_none() {
            *queue = Some(Vec::new());
        }
        if let Some(q) = queue.as_mut() {
            q.push((hwnd as isize, event));
        }
    }

    #[cfg(windows)]
    // SAFETY: `PostMessageW` accepts any handle value and fails harmlessly for a stale one.
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
        // SAFETY: any handle value is accepted; an invalid one yields thread id 0.
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
        let mut queue = WINDOW_SYSTEM_EVENT_QUEUE.lock().unwrap_or_else(|e| e.into_inner());
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
    // SAFETY: `hwnd` is the window receiving this message; `pt` is a valid out pointer.
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
    let _ =
        APP_MOUSE_BUTTONS.try_with(|b| b.set(ferrokit_core::event::MouseButtons(b.get().0 & !bit.0)));
}

// `QWindowsPointerHandler::handleCaptureRelease` (qwindowspointerhandler.cpp:336-340): the first
// button down captures the mouse, so moves and the release keep reaching this window outside it.
thread_local! {
    /// Set while this thread releases the capture to hand the mouse to the native move/size loop:
    /// the loop swallows the button release, so the pressed buttons must survive the capture loss.
    static RELEASING_FOR_SYSTEM_LOOP: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// `ReleaseCapture` before `SC_DRAGMOVE` / `SC_SIZE`: the `WM_CAPTURECHANGED` it causes must not
/// forget the pressed buttons (`sync_mouse_buttons_after_move_loop` still needs them).
#[cfg(windows)]
fn release_capture_for_system_loop() {
    let _ = RELEASING_FOR_SYSTEM_LOOP.try_with(|f| f.set(true));
    // SAFETY: plain Win32 call; the capture notification is sent before it returns.
    unsafe { ReleaseCapture() };
    let _ = RELEASING_FOR_SYSTEM_LOOP.try_with(|f| f.set(false));
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
    // SAFETY: plain Win32 calls on a window handle of this process.
    unsafe {
        if GetCapture() != hwnd {
            SetCapture(hwnd);
            set_auto_capture_flag(hwnd as isize, true);
        }
    }
}

/// Release the automatic capture once no button is held (qwindowspointerhandler.cpp:347-352); an
/// explicit grab stays.
#[cfg(windows)]
fn release_capture_when_idle(hwnd: HWND) {
    let idle = APP_MOUSE_BUTTONS
        .try_with(|b| b.get().is_empty())
        .unwrap_or(true);
    // SAFETY: as in `capture_on_press`.
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
    // SAFETY: plain Win32 calls on a window handle of this process.
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

/// `QWindowsPointerHandler::queryMouseButtons`: the physical buttons, as logical buttons.
#[cfg(windows)]
fn query_mouse_buttons() -> ferrokit_core::event::MouseButtons {
    use ferrokit_core::event::MouseButtons;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, VK_LBUTTON, VK_MBUTTON, VK_RBUTTON,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_SWAPBUTTON};
    // SAFETY: plain Win32 queries without pointers.
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
        result
    }
}

/// `QWindowsContext::handleExitSizeMove` (`qwindowscontext.cpp:1261-1290`). The native move/size
/// loop swallows the button release that ends a drag, so the application still believes the
/// button is down. Every button the application has pressed but that is physically up gets its
/// release now: through the normal `WM_*BUTTONUP` path when the cursor is inside the window, and
/// silently (Qt: `NonClientAreaMouseButtonRelease`, which widgets never see) when it is outside.
#[cfg(windows)]
unsafe fn sync_mouse_buttons_after_move_loop(hwnd: HWND) {
    use ferrokit_core::event::MouseButtons;
    let app = APP_MOUSE_BUTTONS
        .try_with(|b| b.get())
        .unwrap_or(MouseButtons::NO_BUTTON);
    let stale = MouseButtons(app.0 & !query_mouse_buttons().0);
    if stale.is_empty() {
        return;
    }
    let _ = APP_MOUSE_BUTTONS.try_with(|b| b.set(MouseButtons(app.0 & !stale.0)));

    let mut pt = windows_sys::Win32::Foundation::POINT { x: 0, y: 0 };
    GetCursorPos(&mut pt);
    let mut rect: windows_sys::Win32::Foundation::RECT = std::mem::zeroed();
    GetWindowRect(hwnd, &mut rect);
    let inside = pt.x >= rect.left && pt.x < rect.right && pt.y >= rect.top && pt.y < rect.bottom;
    if !inside {
        return;
    }
    windows_sys::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut pt);
    let lparam = ((pt.x & 0xffff) | ((pt.y & 0xffff) << 16)) as LPARAM;
    for (flag, up) in [
        (MouseButtons::LEFT, WM_LBUTTONUP),
        (MouseButtons::RIGHT, WM_RBUTTONUP),
        (MouseButtons::MIDDLE, WM_MBUTTONUP),
    ] {
        if stale.contains(flag) {
            native_window_proc(hwnd, up, 0, lparam);
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
            .try_with(|active| active.borrow_mut().get_mut(&key).and_then(|q| q.pop_front()))
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
fn query_keyboard_modifiers() -> KeyboardModifiers {
    unsafe {
        let is_down = |vk: u16| -> bool { (GetKeyState(vk as i32) as u16 & 0x8000) != 0 };
        KeyboardModifiers {
            shift: is_down(VK_SHIFT),
            control: is_down(VK_CONTROL),
            alt: is_down(VK_MENU),
            meta: is_down(VK_LWIN) || is_down(VK_RWIN),
        }
    }
}

/// `QWindowsMouseHandler::keyStateToMouseButtons`: the `MK_*` flags a mouse message carries in
/// `wParam` are the buttons held down while it was generated.
#[cfg(windows)]
fn mouse_buttons_from_mk(wparam: WPARAM) -> ferrokit_core::event::MouseButtons {
    use ferrokit_core::event::MouseButtons;
    const MK_LBUTTON: usize = 0x0001;
    const MK_RBUTTON: usize = 0x0002;
    const MK_MBUTTON: usize = 0x0010;
    const MK_XBUTTON1: usize = 0x0020;
    const MK_XBUTTON2: usize = 0x0040;
    let mut buttons = MouseButtons::NO_BUTTON;
    for (flag, button) in [
        (MK_LBUTTON, MouseButtons::LEFT),
        (MK_RBUTTON, MouseButtons::RIGHT),
        (MK_MBUTTON, MouseButtons::MIDDLE),
        (MK_XBUTTON1, MouseButtons::BACK),
        (MK_XBUTTON2, MouseButtons::FORWARD),
    ] {
        if wparam & flag != 0 {
            buttons = buttons.union(button);
        }
    }
    buttons
}

#[cfg(windows)]
thread_local! {
    /// Windows with an IME composition in progress (`WM_IME_STARTCOMPOSITION` seen).
    static IME_COMPOSING: std::cell::RefCell<std::collections::HashSet<isize>> =
        std::cell::RefCell::new(std::collections::HashSet::new());
}

#[cfg(windows)]
fn get_cursor_global_pos() -> ferrokit_gui::geometry::primitives::Point {
    unsafe {
        let mut pt: windows_sys::Win32::Foundation::POINT = std::mem::zeroed();
        GetCursorPos(&mut pt);
        ferrokit_gui::geometry::primitives::Point::new(pt.x, pt.y)
    }
}

/// Everything that decides whether a window keeps `WS_EX_LAYERED` (see
/// [`LayeredNeeds::layered_ex_style`]).
#[cfg(windows)]
#[derive(Clone, Copy, Debug, PartialEq)]
struct LayeredNeeds {
    opacity: f32,
    click_through: bool,
    /// The window is presented per pixel (`LAYERED` flag, or a layered surface / presenter):
    /// `UpdateLayeredWindow` needs the style for as long as the window lives.
    per_pixel: bool,
    /// A DWM backdrop is applied: the style is never stripped while the window has it.
    backdrop: bool,
}

#[cfg(windows)]
impl LayeredNeeds {
    /// `SetLayeredWindowAttributes(LWA_ALPHA)` applies to every layered window that is not
    /// presented per pixel, including one whose style is merely kept (backdrop) at opacity 1.0:
    /// skipping the call there would leave the previous alpha in force. A per-pixel window must
    /// not get it: once `UpdateLayeredWindow` drives the window the call is rejected, and it would
    /// also make the next `UpdateLayeredWindow` fail; its opacity is in each present's blend.
    fn wants_alpha_attributes(self, ex_style_after: isize) -> bool {
        !self.per_pixel && ex_style_after & WS_EX_LAYERED as isize != 0
    }

    /// The extended style for `ex_style` after this state: layered whenever translucent,
    /// click-through or per-pixel; left untouched while a backdrop is active; otherwise plain.
    fn layered_ex_style(self, ex_style: isize) -> isize {
        let layered = WS_EX_LAYERED as isize;
        if self.click_through || self.opacity < 1.0 || self.per_pixel {
            ex_style | layered
        } else if self.backdrop {
            ex_style
        } else {
            ex_style & !layered
        }
    }
}

/// `QWindowsWindow::setWindowLayered` + `setWindowOpacity` for a window that is not `LAYERED`:
/// a translucent (`opacity < 1`) or click-through (`WindowTransparentForInput`) window needs
/// `WS_EX_LAYERED` plus `SetLayeredWindowAttributes(LWA_ALPHA)` with `qRound(255 * opacity)`;
/// a window that is presented per pixel or has a backdrop keeps the style; any other window drops
/// it again and repaints.
#[cfg(windows)]
fn apply_system_window_opacity(hwnd: HWND, needs: LayeredNeeds) {
    use windows_sys::Win32::Graphics::Gdi::InvalidateRect;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, IsWindowVisible, SetLayeredWindowAttributes, SetWindowLongPtrW,
        GWL_EXSTYLE, LWA_ALPHA,
    };
    unsafe {
        let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let new_style = needs.layered_ex_style(ex_style);
        if new_style != ex_style {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, new_style);
        }
        if needs.wants_alpha_attributes(new_style) {
            let alpha = (needs.opacity * 255.0).round() as u8;
            SetLayeredWindowAttributes(hwnd, 0, alpha, LWA_ALPHA);
        } else if new_style & WS_EX_LAYERED as isize == 0 && IsWindowVisible(hwnd) != 0 {
            InvalidateRect(hwnd, ptr::null(), 1);
        }
    }
}

#[cfg(windows)]
pub fn get_window_dpr(hwnd: HWND) -> f32 {
    unsafe {
        let dpi = windows_sys::Win32::UI::HiDpi::GetDpiForWindow(hwnd);
        if dpi > 0 {
            ferrokit_gui::round_scale_factor(dpi as f64 / 96.0) as f32
        } else {
            1.0
        }
    }
}

/// The logical desktop position of the window's physical top-left `native`: relative to the
/// native top-left of the window's screen, scaled by the window's ratio `dpr`.
#[cfg(windows)]
fn window_global_position(hwnd: HWND, native: Point, dpr: f32) -> Point {
    match crate::screen::win32_screen::window_screen_mapping(hwnd as isize) {
        Some((origin, _)) => crate::high_dpi::from_native_global_point(native, origin, dpr),
        None => crate::high_dpi::from_native_point(native, dpr),
    }
}

#[cfg(windows)]
fn get_x_lparam(lparam: LPARAM) -> i32 {
    (lparam as usize & 0xffff) as i16 as i32
}

#[cfg(windows)]
fn get_y_lparam(lparam: LPARAM) -> i32 {
    ((lparam as usize >> 16) & 0xffff) as i16 as i32
}

#[cfg(windows)]
/// Win32 Window Procedure (wndproc).
unsafe extern "system" fn native_window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let _t = ferrokit_gui::startup_trace::span_min(3.0, || format!("wndproc msg=0x{msg:04X}"));
    // Python/PySide6 contains a raising handler; unwinding out of an extern "system" fn aborts.
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        native_window_proc_inner(hwnd, msg, wparam, lparam)
    })) {
        Ok(result) => result,
        Err(_) => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

#[cfg(windows)]
unsafe fn native_window_proc_inner(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_FLUSH_WINDOW_SYSTEM_EVENTS => {
            flush_window_system_events();
            0
        }
        WM_NCCALCSIZE => {
            crate::resize_debug::count(crate::resize_debug::Count::NcCalcSize);
            if wparam != 0 {
                let ncp = &mut *(lparam as *mut NCCALCSIZE_PARAMS);
                let client_rect = &mut ncp.rgrc[0];

                if IsZoomed(hwnd) != 0 {
                    let mut monitor_info: MONITORINFO = std::mem::zeroed();
                    monitor_info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
                    let h_monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
                    if !h_monitor.is_null() && GetMonitorInfoW(h_monitor, &mut monitor_info) != 0 {
                        *client_rect = monitor_info.rcWork;
                    }
                }
                return 0;
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_NCHITTEST => {
            if let Some(config) = get_window_frameless_config(hwnd) {
                let x = get_x_lparam(lparam);
                let y = get_y_lparam(lparam);

                let mut window_rect: RECT = std::mem::zeroed();
                GetWindowRect(hwnd, &mut window_rect);

                let is_zoomed = IsZoomed(hwnd) != 0;
                let border = if is_zoomed { 0 } else { config.resize_border };

                // An axis whose minimum equals its maximum cannot change size, so it offers no
                // resize zone: the cursor would promise a resize the tracking limits refuse. The
                // tracking limits do not do this by themselves; Qt has its own rule for a window
                // with a native frame (`QWindowsWindow::handleNonClientHitTest`,
                // qwindowswindow.cpp:3493-3525, QTBUG-32663). This zone is ferrokit' own (Qt's
                // frameless windows resize through `startSystemResize`), so only the criterion
                // (`SizeLimits::fixed_axes`) is Qt's.
                let (fixed_w, fixed_h) = get_window_size_limits(hwnd)
                    .map_or((false, false), |limits| limits.fixed_axes());
                let on_left = !fixed_w && x >= window_rect.left && x < window_rect.left + border;
                let on_right = !fixed_w && x <= window_rect.right && x > window_rect.right - border;
                let on_top = !fixed_h && y >= window_rect.top && y < window_rect.top + border;
                let on_bottom =
                    !fixed_h && y <= window_rect.bottom && y > window_rect.bottom - border;

                if on_top && on_left {
                    return HTTOPLEFT as LRESULT;
                }
                if on_top && on_right {
                    return HTTOPRIGHT as LRESULT;
                }
                if on_bottom && on_left {
                    return HTBOTTOMLEFT as LRESULT;
                }
                if on_bottom && on_right {
                    return HTBOTTOMRIGHT as LRESULT;
                }
                if on_left {
                    return HTLEFT as LRESULT;
                }
                if on_right {
                    return HTRIGHT as LRESULT;
                }
                if on_top {
                    return HTTOP as LRESULT;
                }
                if on_bottom {
                    return HTBOTTOM as LRESULT;
                }

                if y < window_rect.top + config.caption_height {
                    return HTCAPTION as LRESULT;
                }

                return HTCLIENT as LRESULT;
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_GETMINMAXINFO => {
            use windows_sys::Win32::UI::WindowsAndMessaging::MINMAXINFO;
            if let Some(limits) = get_window_size_limits(hwnd) {
                let mmi = &mut *(lparam as *mut MINMAXINFO);
                // The limits are logical; the window's own ratio at this moment (after
                // `WM_DPICHANGED` it is already the new one) makes them native, and a bound that
                // is not set leaves the system's value in place
                // (`QWindowsGeometryHint::applyToMinMaxInfo`, qwindowswindow.cpp:1238-1262).
                let track = limits.native_track(get_window_dpr(hwnd));
                if let Some(x) = track.min.0 {
                    mmi.ptMinTrackSize.x = x;
                }
                if let Some(y) = track.min.1 {
                    mmi.ptMinTrackSize.y = y;
                }
                if let Some(x) = track.max.0 {
                    mmi.ptMaxTrackSize.x = x;
                }
                if let Some(y) = track.max.1 {
                    mmi.ptMaxTrackSize.y = y;
                }
                return 0;
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_ERASEBKGND => 1,
        WM_CLOSE => {
            // Widgets see exactly one Close: the window handler turns this request into
            // `close(true)`, so nothing is posted to the bound receiver on top of it.
            dispatch_window_system_event(Delivery::Default, hwnd, WindowSystemEvent::CloseRequest);
            0
        }
        windows_sys::Win32::UI::WindowsAndMessaging::WM_ENTERSIZEMOVE => {
            crate::resize_debug::enter_size_move();
            set_interactive_resize_flag(hwnd, true);
            // Per-HWND: delivered to this window's own event handler.
            dispatch_window_system_event(
                Delivery::Default,
                hwnd,
                WindowSystemEvent::InteractiveResizeStart,
            );
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        windows_sys::Win32::UI::WindowsAndMessaging::WM_EXITSIZEMOVE => {
            set_interactive_resize_flag(hwnd, false);
            sync_mouse_buttons_after_move_loop(hwnd);
            dispatch_window_system_event(
                Delivery::Default,
                hwnd,
                WindowSystemEvent::InteractiveResizeEnd,
            );
            crate::resize_debug::exit_size_move();
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_DESTROY => {
            set_interactive_resize_flag(hwnd, false);
            unregister_window_event_binding(hwnd);
            0
        }
        WM_SIZE => {
            // Qt: `SIZE_MINIMIZED` only changes the window state and leaves the geometry alone
            // (`QWindowsWindow::handleResized`, qwindowswindow.cpp:2370-2377). The 0x0 client size
            // Windows reports while iconic must not reach the geometry or the widget tree.
            if wparam as u32 == SIZE_MINIMIZED {
                dispatch_window_system_event(
                    Delivery::Default,
                    hwnd,
                    WindowSystemEvent::Minimize { minimized: true },
                );
                return DefWindowProcW(hwnd, msg, wparam, lparam);
            }
            let dpr = get_window_dpr(hwnd);
            let phys_w = (lparam as usize & 0xffff) as i32;
            let phys_h = ((lparam as usize >> 16) & 0xffff) as i32;
            crate::resize_debug::wm_size_begin(hwnd as isize, phys_w, phys_h);
            let logical_size = crate::high_dpi::from_native_size(
                ferrokit_gui::geometry::primitives::Size::new(phys_w, phys_h),
                dpr,
            );
            crate::resize_trace::record(
                crate::resize_trace::TraceKind::WmSize,
                hwnd as usize,
                (logical_size.width as u32, logical_size.height as u32),
                (phys_w as u32, phys_h as u32),
            );
            // `fromNativeWindowGeometry` for a top-level window: the position maps against its
            // screen's native top-left (`qwindowsysteminterface.cpp:298`,
            // `qhighdpiscaling_p.h:254-260`).
            let mut r: RECT = unsafe { std::mem::zeroed() };
            let (x, y) = if unsafe { GetWindowRect(hwnd, &mut r) } != 0 {
                let logical_pt = window_global_position(hwnd, Point::new(r.left, r.top), dpr);
                (logical_pt.x, logical_pt.y)
            } else {
                (0, 0)
            };
            let logical_rect = Rect::new(x, y, logical_size.width, logical_size.height);
            crate::resize_debug::note(|| {
                format!(
                    "WM_SIZE phys {}x{} dpr {} -> logical {}x{} (wparam={})",
                    phys_w, phys_h, dpr, logical_size.width, logical_size.height, wparam
                )
            });

            // The single canonical resize delivery: WindowSystemEvent::Resize (geometry, widget
            // Resize, callback, layout, render). No second `EventKind::Resize` is posted to the
            // bound QObject; it used to re-run the callback and a synchronous render.
            handle_geometry_change(Delivery::Default, hwnd, logical_rect);
            // Qt restores the state after the geometry (`QWindowsWindow::handleResized`,
            // qwindowswindow.cpp:2385-2395). The widget layer dedupes repeated restores.
            if wparam as u32 == SIZE_RESTORED || wparam as u32 == SIZE_MAXIMIZED {
                dispatch_window_system_event(
                    Delivery::Default,
                    hwnd,
                    WindowSystemEvent::Minimize { minimized: false },
                );
            }
            crate::resize_debug::wm_size_end(hwnd as isize);
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_MOVE => {
            // Qt ignores a move while iconic: `QWindowsWindow::handleMoved` only runs the geometry
            // change when `!IsIconic` (qwindowswindow.cpp:2328-2333). Windows reports the iconic
            // position as (-32000, -32000), which must not reach the geometry or the event receiver.
            if unsafe { IsIconic(hwnd) } != 0 {
                return DefWindowProcW(hwnd, msg, wparam, lparam);
            }
            crate::resize_debug::wm_move_begin(hwnd as isize);
            let dpr = get_window_dpr(hwnd);
            let phys_x = get_x_lparam(lparam);
            let phys_y = get_y_lparam(lparam);
            let pos = window_global_position(hwnd, Point::new(phys_x, phys_y), dpr);
            let mut r: RECT = unsafe { std::mem::zeroed() };
            let (w, h) = if unsafe { GetWindowRect(hwnd, &mut r) } != 0 {
                let logical_size = crate::high_dpi::from_native_size(
                    ferrokit_gui::geometry::primitives::Size::new(r.right - r.left, r.bottom - r.top),
                    dpr,
                );
                (logical_size.width, logical_size.height)
            } else {
                (0, 0)
            };
            let logical_rect = Rect::new(pos.x, pos.y, w, h);
            handle_geometry_change(Delivery::Default, hwnd, logical_rect);

            if let Some((handle, receiver)) = get_window_event_binding(hwnd) {
                handle.post_event(
                    receiver,
                    Event::new_spontaneous(EventKind::Move {
                        x: pos.x,
                        y: pos.y,
                        old_x: 0,
                        old_y: 0,
                    }),
                );
            }
            crate::resize_debug::wm_move_end(hwnd as isize);
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_SHOWWINDOW => {
            let shown = wparam != 0;
            if let Some((handle, receiver)) = get_window_event_binding(hwnd) {
                let kind = if shown {
                    EventKind::Show
                } else {
                    EventKind::Hide
                };
                handle.post_event(receiver, Event::new_spontaneous(kind));
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_PAINT => {
            crate::resize_debug::count(crate::resize_debug::Count::WmPaint);
            if let Some((handle, receiver)) = get_window_event_binding(hwnd) {
                handle.post_event(receiver, Event::new_spontaneous(EventKind::Expose));
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_SETFOCUS => {
            dispatch_window_system_event(Delivery::Default, hwnd, WindowSystemEvent::FocusIn);
            if let Some((handle, receiver)) = get_window_event_binding(hwnd) {
                handle.post_event(
                    receiver,
                    Event::new_spontaneous(EventKind::FocusIn {
                        reason: ferrokit_core::event::FocusReason::ActiveWindow,
                    }),
                );
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_KILLFOCUS => {
            dispatch_window_system_event(Delivery::Default, hwnd, WindowSystemEvent::FocusOut);
            if let Some((handle, receiver)) = get_window_event_binding(hwnd) {
                handle.post_event(
                    receiver,
                    Event::new_spontaneous(EventKind::FocusOut {
                        reason: ferrokit_core::event::FocusReason::ActiveWindow,
                    }),
                );
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_DPICHANGED => {
            let dpi_x = (wparam as u32) & 0xffff;
            let dpi_y = ((wparam as u32) >> 16) & 0xffff;

            // Qt sends the screen/DPI change before applying the suggested rect
            // (qwindowswindow.cpp:2164-2191), so the handler still sees the old geometry and the
            // resulting WM_MOVE/WM_SIZE are processed with the new ratio already in place.
            dispatch_window_system_event(
                Delivery::Default,
                hwnd,
                WindowSystemEvent::DpiChanged { dpi_x, dpi_y },
            );
            let rect_ptr = lparam as *const RECT;
            if !rect_ptr.is_null() {
                let r = *rect_ptr;
                let width = (r.right - r.left).max(0);
                let height = (r.bottom - r.top).max(0);
                SetWindowPos(
                    hwnd,
                    ptr::null_mut(),
                    r.left,
                    r.top,
                    width,
                    height,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            if let Some((handle, receiver)) = get_window_event_binding(hwnd) {
                handle.post_event(
                    receiver,
                    Event::new_spontaneous(EventKind::DpiChanged { dpi_x, dpi_y }),
                );
            }
            0
        }
        WM_DISPLAYCHANGE => {
            crate::integration::platform().screen_changed().emit(&());
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_SETTINGCHANGE | WM_THEMECHANGED => {
            crate::integration::platform().theme().refresh();
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_SETCURSOR => {
            let hit_test = (lparam & 0xFFFF) as u32;
            if hit_test == HTCLIENT {
                // The application override cursor beats the window's own (`QWindowsWindow::applyCursor`,
                // qwindowswindow.cpp:3762-3767).
                let shape = crate::cursor::win32_cursor::Win32Cursor::override_shape()
                    .or_else(|| window_cursor_shape(hwnd))
                    .unwrap_or_else(crate::cursor::win32_cursor::Win32Cursor::current_global_shape);
                crate::cursor::win32_cursor::Win32Cursor::apply_shape(shape);
                return 1;
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
            track_button_press(msg);
            capture_on_press(hwnd);
            let dpr = get_window_dpr(hwnd);
            let phys_x = get_x_lparam(lparam);
            let phys_y = get_y_lparam(lparam);
            let pos = crate::high_dpi::from_native_point(
                ferrokit_gui::geometry::primitives::Point::new(phys_x, phys_y),
                dpr,
            );
            let phys_global = get_cursor_global_pos();
            let global_pos =
                crate::screen::win32_screen::from_native_global_point(phys_global, hwnd as isize);
            let button = match msg {
                WM_LBUTTONDOWN => MouseButton::Left,
                WM_RBUTTONDOWN => MouseButton::Right,
                WM_MBUTTONDOWN => MouseButton::Middle,
                _ => MouseButton::None,
            };
            let modifiers = query_keyboard_modifiers();

            dispatch_window_system_event(
                Delivery::Default,
                hwnd,
                WindowSystemEvent::MousePress {
                    pos,
                    global_pos,
                    button,
                    modifiers,
                    // `QWindowsContext` stamps mouse messages with `GetMessageTime()` (qwindowscontext.cpp:836).
                    timestamp: unsafe { GetMessageTime() } as u32,
                },
            );

            if let Some((handle, receiver)) = get_window_event_binding(hwnd) {
                let btn_code = match button {
                    MouseButton::Left => 1,
                    MouseButton::Right => 2,
                    MouseButton::Middle => 3,
                    _ => 0,
                };
                handle.post_event(
                    receiver,
                    Event::new_spontaneous(EventKind::MouseButtonPress {
                        x: pos.x,
                        y: pos.y,
                        button: btn_code,
                    }),
                );
            }
            0
        }
        WM_LBUTTONDBLCLK | WM_RBUTTONDBLCLK | WM_MBUTTONDBLCLK => {
            track_button_press(msg);
            capture_on_press(hwnd);
            let dpr = get_window_dpr(hwnd);
            let phys_x = get_x_lparam(lparam);
            let phys_y = get_y_lparam(lparam);
            let pos = crate::high_dpi::from_native_point(
                ferrokit_gui::geometry::primitives::Point::new(phys_x, phys_y),
                dpr,
            );
            let phys_global = get_cursor_global_pos();
            let global_pos =
                crate::screen::win32_screen::from_native_global_point(phys_global, hwnd as isize);
            let button = match msg {
                WM_LBUTTONDBLCLK => MouseButton::Left,
                WM_RBUTTONDBLCLK => MouseButton::Right,
                WM_MBUTTONDBLCLK => MouseButton::Middle,
                _ => MouseButton::None,
            };
            let modifiers = query_keyboard_modifiers();

            dispatch_window_system_event(
                Delivery::Default,
                hwnd,
                WindowSystemEvent::MouseDoubleClick {
                    pos,
                    global_pos,
                    button,
                    modifiers,
                },
            );

            if let Some((handle, receiver)) = get_window_event_binding(hwnd) {
                let btn_code = match button {
                    MouseButton::Left => 1,
                    MouseButton::Right => 2,
                    MouseButton::Middle => 3,
                    _ => 0,
                };
                handle.post_event(
                    receiver,
                    Event::new_spontaneous(EventKind::MouseButtonDblClick {
                        x: pos.x,
                        y: pos.y,
                        button: btn_code,
                    }),
                );
            }
            0
        }
        WM_LBUTTONUP | WM_RBUTTONUP | WM_MBUTTONUP => {
            track_button_release(msg);
            release_capture_when_idle(hwnd);
            let dpr = get_window_dpr(hwnd);
            let phys_x = get_x_lparam(lparam);
            let phys_y = get_y_lparam(lparam);
            let pos = crate::high_dpi::from_native_point(
                ferrokit_gui::geometry::primitives::Point::new(phys_x, phys_y),
                dpr,
            );
            let phys_global = get_cursor_global_pos();
            let global_pos =
                crate::screen::win32_screen::from_native_global_point(phys_global, hwnd as isize);
            let button = match msg {
                WM_LBUTTONUP => MouseButton::Left,
                WM_RBUTTONUP => MouseButton::Right,
                WM_MBUTTONUP => MouseButton::Middle,
                _ => MouseButton::None,
            };
            let modifiers = query_keyboard_modifiers();

            dispatch_window_system_event(
                Delivery::Default,
                hwnd,
                WindowSystemEvent::MouseRelease {
                    pos,
                    global_pos,
                    button,
                    modifiers,
                },
            );

            if let Some((handle, receiver)) = get_window_event_binding(hwnd) {
                let btn_code = match button {
                    MouseButton::Left => 1,
                    MouseButton::Right => 2,
                    MouseButton::Middle => 3,
                    _ => 0,
                };
                handle.post_event(
                    receiver,
                    Event::new_spontaneous(EventKind::MouseButtonRelease {
                        x: pos.x,
                        y: pos.y,
                        button: btn_code,
                    }),
                );
            }
            0
        }
        WM_MOUSEMOVE => {
            let mut tme = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            TrackMouseEvent(&mut tme);

            if is_repeated_buttonless_move(hwnd, wparam, lparam) {
                return 0;
            }

            let dpr = get_window_dpr(hwnd);
            let phys_x = get_x_lparam(lparam);
            let phys_y = get_y_lparam(lparam);
            let pos = crate::high_dpi::from_native_point(
                ferrokit_gui::geometry::primitives::Point::new(phys_x, phys_y),
                dpr,
            );
            let phys_global = get_cursor_global_pos();
            let global_pos =
                crate::screen::win32_screen::from_native_global_point(phys_global, hwnd as isize);

            dispatch_window_system_event(
                Delivery::Default,
                hwnd,
                WindowSystemEvent::MouseMove {
                    pos,
                    global_pos,
                    buttons: mouse_buttons_from_mk(wparam),
                },
            );

            if let Some((handle, receiver)) = get_window_event_binding(hwnd) {
                handle.post_event(
                    receiver,
                    Event::new_spontaneous(EventKind::MouseMove { x: pos.x, y: pos.y }),
                );
            }
            0
        }
        WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
            let horizontal = msg == WM_MOUSEHWHEEL;
            let dpr = get_window_dpr(hwnd);
            let global_x = get_x_lparam(lparam);
            let global_y = get_y_lparam(lparam);
            let phys_global = ferrokit_gui::geometry::primitives::Point::new(global_x, global_y);
            let global_pos =
                crate::screen::win32_screen::from_native_global_point(phys_global, hwnd as isize);

            let mut pt = windows_sys::Win32::Foundation::POINT {
                x: global_x,
                y: global_y,
            };
            windows_sys::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut pt);
            let pos = crate::high_dpi::from_native_point(
                ferrokit_gui::geometry::primitives::Point::new(pt.x, pt.y),
                dpr,
            );
            let (delta_x, delta_y) = wheel_deltas(horizontal, (wparam >> 16) as i16 as i32);
            let modifiers = query_keyboard_modifiers();

            dispatch_window_system_event(
                Delivery::Default,
                hwnd,
                WindowSystemEvent::Wheel {
                    pos,
                    global_pos,
                    delta: WheelDelta {
                        y: delta_y,
                        x: delta_x,
                    },
                    modifiers,
                },
            );

            if let Some((handle, receiver)) = get_window_event_binding(hwnd) {
                handle.post_event(
                    receiver,
                    Event::new_spontaneous(EventKind::Wheel {
                        x: pos.x,
                        y: pos.y,
                        pixel_delta_x: 0,
                        pixel_delta_y: 0,
                        angle_delta_x: delta_x,
                        angle_delta_y: delta_y,
                        modifiers: modifiers.bits(),
                    }),
                );
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_CANCELMODE => {
            // Whatever ended the mouse interaction (a popup, a modal dialog) swallowed the
            // releases: forget the buttons (`QWindowsContext` resets on `WM_CANCELMODE`).
            let _ = APP_MOUSE_BUTTONS.try_with(|b| b.set(ferrokit_core::event::MouseButtons::NO_BUTTON));
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_CAPTURECHANGED => {
            // `lparam` is the window gaining the capture. Capture moving to another window of
            // this thread (a popup, or our own `capture_on_press`) keeps the buttons; capture
            // lost to another thread or to nothing swallows their releases.
            let new_owner = lparam as HWND;
            set_auto_capture_flag(hwnd as isize, false);
            let for_system_loop = RELEASING_FOR_SYSTEM_LOOP
                .try_with(|f| f.get())
                .unwrap_or(false);
            if !for_system_loop && (new_owner.is_null() || !is_window_thread(new_owner)) {
                let _ = APP_MOUSE_BUTTONS
                    .try_with(|b| b.set(ferrokit_core::event::MouseButtons::NO_BUTTON));
            }
            if new_owner != hwnd {
                // Alt-Tab, a popup or the system move loop took the capture: the widget layer
                // reconciles an explicit `grabMouse` that no longer has a native grab behind it.
                dispatch_window_system_event(
                    Delivery::Default,
                    hwnd,
                    WindowSystemEvent::MouseGrabLost,
                );
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_MOUSELEAVE => {
            LAST_MOUSE_MOVE.with(|last| {
                if matches!(last.get(), Some((h, ..)) if h == hwnd as isize) {
                    last.set(None);
                }
            });
            dispatch_window_system_event(Delivery::Default, hwnd, WindowSystemEvent::MouseLeave);
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_CONTEXTMENU => {
            // lParam -1 is the menu key / Shift+F10: no pointer position, so the cursor is the
            // hint when it is over the window and the window centre otherwise (the receiver
            // positions a keyboard menu itself).
            let keyboard = lparam as u32 == u32::MAX;
            let (global_x, global_y) = if keyboard {
                let mut cursor = windows_sys::Win32::Foundation::POINT { x: 0, y: 0 };
                GetCursorPos(&mut cursor);
                let mut rect: RECT = std::mem::zeroed();
                GetWindowRect(hwnd, &mut rect);
                if cursor.x >= rect.left
                    && cursor.x < rect.right
                    && cursor.y >= rect.top
                    && cursor.y < rect.bottom
                {
                    (cursor.x, cursor.y)
                } else {
                    ((rect.left + rect.right) / 2, (rect.top + rect.bottom) / 2)
                }
            } else {
                (get_x_lparam(lparam), get_y_lparam(lparam))
            };
            let mut pt = windows_sys::Win32::Foundation::POINT {
                x: global_x,
                y: global_y,
            };
            windows_sys::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut pt);
            let pos = crate::high_dpi::from_native_point(
                ferrokit_gui::geometry::primitives::Point::new(pt.x, pt.y),
                get_window_dpr(hwnd),
            );
            let global_pos = crate::screen::win32_screen::from_native_global_point(
                ferrokit_gui::geometry::primitives::Point::new(global_x, global_y),
                hwnd as isize,
            );

            if let Some((handle, receiver)) = get_window_event_binding(hwnd) {
                handle.post_event(
                    receiver,
                    Event::new_spontaneous(EventKind::ContextMenu {
                        x: pos.x,
                        y: pos.y,
                        global_x: global_pos.x,
                        global_y: global_pos.y,
                        reason: if keyboard {
                            ferrokit_core::event::ContextMenuReason::Keyboard
                        } else {
                            ferrokit_core::event::ContextMenuReason::Mouse
                        },
                    }),
                );
            }
            0
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            let key = wparam as u32;
            let is_repeat = (lparam & (1 << 30)) != 0;
            let modifiers = query_keyboard_modifiers();

            dispatch_window_system_event(
                Delivery::Default,
                hwnd,
                WindowSystemEvent::KeyPress {
                    key,
                    modifiers,
                    is_repeat,
                },
            );

            if let Some((handle, receiver)) = get_window_event_binding(hwnd) {
                handle.post_event(
                    receiver,
                    Event::new_spontaneous(EventKind::KeyPress {
                        key,
                        modifiers: modifiers.bits(),
                        is_repeat,
                    }),
                );
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_KEYUP | WM_SYSKEYUP => {
            let key = wparam as u32;
            let modifiers = query_keyboard_modifiers();
            dispatch_window_system_event(
                Delivery::Default,
                hwnd,
                WindowSystemEvent::KeyRelease { key, modifiers },
            );

            if let Some((handle, receiver)) = get_window_event_binding(hwnd) {
                handle.post_event(
                    receiver,
                    Event::new_spontaneous(EventKind::KeyRelease {
                        key,
                        modifiers: modifiers.bits(),
                    }),
                );
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        windows_sys::Win32::UI::WindowsAndMessaging::WM_IME_STARTCOMPOSITION => {
            let _ = IME_COMPOSING.try_with(|c| c.borrow_mut().insert(hwnd as isize));
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        windows_sys::Win32::UI::WindowsAndMessaging::WM_IME_COMPOSITION => {
            let mut ime_ctx = crate::ime::Win32InputContext::new(hwnd);
            let (commit_opt, preedit_opt, cursor_pos) = ime_ctx.handle_composition(lparam);

            let commit_string = commit_opt.unwrap_or_default();
            let preedit_string = preedit_opt.unwrap_or_default();

            if !commit_string.is_empty() || !preedit_string.is_empty() {
                dispatch_window_system_event(
                    Delivery::Default,
                    hwnd,
                    WindowSystemEvent::InputMethod {
                        commit_string: commit_string.clone(),
                        preedit_string: preedit_string.clone(),
                        cursor_position: cursor_pos,
                    },
                );

                if let Some((handle, receiver)) = get_window_event_binding(hwnd) {
                    handle.post_event(
                        receiver,
                        Event::new_spontaneous(EventKind::InputMethod {
                            commit_string,
                            preedit_string,
                            cursor_position: cursor_pos,
                        }),
                    );
                }
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        windows_sys::Win32::UI::WindowsAndMessaging::WM_IME_ENDCOMPOSITION => {
            // `QWindowsInputContext::endComposition`: a composition that was running is closed
            // with an empty event so a cancelled preedit is cleared.
            let was_composing = IME_COMPOSING
                .try_with(|c| c.borrow_mut().remove(&(hwnd as isize)))
                .unwrap_or(false);
            if was_composing {
                dispatch_window_system_event(
                    Delivery::Default,
                    hwnd,
                    WindowSystemEvent::InputMethod {
                        commit_string: String::new(),
                        preedit_string: String::new(),
                        cursor_position: 0,
                    },
                );
                if let Some((handle, receiver)) = get_window_event_binding(hwnd) {
                    handle.post_event(
                        receiver,
                        Event::new_spontaneous(EventKind::InputMethod {
                            commit_string: String::new(),
                            preedit_string: String::new(),
                            cursor_position: 0,
                        }),
                    );
                }
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        0x0218 => {
            // WM_POWERBROADCAST. Qt itself only handles PBT_POWERSETTINGCHANGE (qwindowscontext.cpp,
            // qWindowsPowerWindowProc); it has no suspend/resume event, so this is a ferrokit extension.
            let power_event = match wparam as u32 {
                0x0012 | 0x0007 => Some(crate::window_system_interface::PowerEvent::Resume),
                0x0004 => Some(crate::window_system_interface::PowerEvent::Suspend),
                _ => None,
            };
            if let Some(event) = power_event {
                dispatch_window_system_event(
                    Delivery::Default,
                    hwnd,
                    WindowSystemEvent::Power { event },
                );
            }
            1
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

#[cfg(windows)]
pub fn set_dpi_awareness() -> bool {
    unsafe {
        use windows_sys::Win32::UI::HiDpi::{
            SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        };
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) != 0
    }
}

#[cfg(not(windows))]
pub fn set_dpi_awareness() -> bool {
    true
}
#[cfg(windows)]
static DCOMP_ENABLED_OVERRIDE: AtomicI8 = AtomicI8::new(0);

/// Queries whether DirectComposition presenter is enabled via `FERROKIT_ENABLE_DCOMP=1`
/// or programmatic override (`set_dcomp_enabled`).
#[cfg(windows)]
pub fn is_dcomp_enabled() -> bool {
    match DCOMP_ENABLED_OVERRIDE.load(Ordering::Relaxed) {
        1 => true,
        -1 => false,
        _ => {
            static DCOMP_ENV_ONCE: Once = Once::new();
            static DCOMP_ENV_ENABLED: AtomicI8 = AtomicI8::new(0);
            DCOMP_ENV_ONCE.call_once(|| {
                let enabled = std::env::var("FERROKIT_ENABLE_DCOMP")
                    .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                    .unwrap_or(false);
                DCOMP_ENV_ENABLED.store(if enabled { 1 } else { -1 }, Ordering::Relaxed);
            });
            DCOMP_ENV_ENABLED.load(Ordering::Relaxed) == 1
        }
    }
}

/// Overrides DComp policy for testing or runtime control (Some(true) = force enable, Some(false) = force disable, None = follow env).
#[cfg(windows)]
pub fn set_dcomp_enabled(enabled: Option<bool>) {
    let val = match enabled {
        Some(true) => 1,
        Some(false) => -1,
        None => 0,
    };
    DCOMP_ENABLED_OVERRIDE.store(val, Ordering::Relaxed);
}

#[cfg(windows)]
static REGISTER_WINDOW_CLASS_ONCE: Once = Once::new();
#[cfg(windows)]
pub(crate) const NATIVE_WINDOW_CLASS_NAME: &[u16] = &[
    'Q' as u16, 't' as u16, 'r' as u16, 's' as u16, 'N' as u16, 'a' as u16, 't' as u16, 'i' as u16,
    'v' as u16, 'e' as u16, 'W' as u16, 'i' as u16, 'n' as u16, 'd' as u16, 'o' as u16, 'w' as u16,
    'C' as u16, 'l' as u16, 'a' as u16, 's' as u16, 's' as u16, 0,
];

#[cfg(windows)]
fn ensure_native_window_class_registered() {
    REGISTER_WINDOW_CLASS_ONCE.call_once(|| unsafe {
        let h_instance = GetModuleHandleW(ptr::null());
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            // Qt registers every window class with CS_DBLCLKS (qwindowswindowclassdescription.cpp:67).
            style: CS_DBLCLKS,
            lpfnWndProc: Some(native_window_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: h_instance,
            hIcon: ptr::null_mut(),
            hCursor: LoadCursorW(ptr::null_mut(), IDC_ARROW),
            hbrBackground: ptr::null_mut(),
            lpszMenuName: ptr::null(),
            lpszClassName: NATIVE_WINDOW_CLASS_NAME.as_ptr(),
            hIconSm: ptr::null_mut(),
        };
        RegisterClassExW(&wc);
    });
}

#[cfg(windows)]
pub struct NativeWindow {
    hwnd: HWND,
    title: String,
    flags: WindowFlags,
    geometry: Rect,
    drop_target: *mut crate::drag_drop::win32_ole::OleDropTarget,
    opacity: f32,
    size_limits: crate::size_limits::SizeLimits,
    layered_surface: Option<crate::surface::WindowsSurface>,
    presenter: Option<crate::presenter::WindowsPresenter>,
    owner_thread: std::thread::ThreadId,
    state_flags: std::cell::Cell<PlatformWindowStateFlags>,
    ime_enabled: std::cell::Cell<bool>,
    target_pos: Option<ferrokit_gui::geometry::Point>,
    dcomp_downgraded: bool,
    /// A backdrop other than `BackdropType::None` is applied (it keeps `WS_EX_LAYERED` in place).
    backdrop_active: bool,
    /// The requested backdrop and whether its Acrylic blur is suspended.
    backdrop_state: crate::backdrop::BackdropState,
}

// Deliberately neither `Send` nor `Sync`: it owns an `HWND` (window APIs such as `DestroyWindow`
// require the creating thread), a raw OLE drop-target pointer and unsynchronised `Cell` state, and
// the window procedure reaches it from that thread only. `owner_thread` records that thread.

/// `set_stays_on_top`'s `SetWindowPos` insert-after target: `HWND_TOPMOST` for the topmost band,
/// `HWND_NOTOPMOST` to leave it.
#[cfg(windows)]
fn topmost_insert_after(enabled: bool) -> HWND {
    use windows_sys::Win32::UI::WindowsAndMessaging::{HWND_NOTOPMOST, HWND_TOPMOST};
    if enabled {
        HWND_TOPMOST
    } else {
        HWND_NOTOPMOST
    }
}

#[cfg(windows)]
impl NativeWindow {
    pub fn new(title: &str, rect: Rect, flags: WindowFlags) -> Result<Self, &'static str> {
        ensure_native_window_class_registered();

        let mut dw_style = WS_CLIPCHILDREN | WS_CLIPSIBLINGS;
        let mut dw_ex_style = 0u32;

        if flags.contains(WindowFlags::LAYERED) || flags.contains(WindowFlags::TOOLTIP) {
            dw_style |= WS_POPUP;
        } else if flags.contains(WindowFlags::CUSTOM_FRAMELESS) {
            dw_style |= WS_THICKFRAME | WS_CAPTION | WS_MINIMIZEBOX | WS_MAXIMIZEBOX;
        } else if flags.contains(WindowFlags::FRAMELESS) {
            dw_style |= WS_POPUP;
        } else {
            dw_style |= WS_OVERLAPPEDWINDOW;
        }

        if flags.contains(WindowFlags::STAYS_ON_TOP) || flags.contains(WindowFlags::TOOLTIP) {
            dw_ex_style |= WS_EX_TOPMOST;
        }
        if flags.contains(WindowFlags::TOOLTIP) {
            dw_ex_style |= WS_EX_NOACTIVATE;
        }
        if flags.contains(WindowFlags::TOOL) || flags.contains(WindowFlags::TOOLTIP) {
            dw_ex_style |= WS_EX_TOOLWINDOW;
        } else {
            dw_ex_style |= WS_EX_APPWINDOW;
        }
        if flags.contains(WindowFlags::LAYERED) {
            dw_ex_style |= WS_EX_LAYERED;
        }
        if flags.contains(WindowFlags::CLICK_THROUGH) {
            dw_ex_style |= WS_EX_TRANSPARENT;
        }

        let wide_title: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();

        let hwnd = unsafe {
            let h_instance = GetModuleHandleW(ptr::null());
            CreateWindowExW(
                dw_ex_style,
                NATIVE_WINDOW_CLASS_NAME.as_ptr(),
                wide_title.as_ptr(),
                dw_style,
                rect.x,
                rect.y,
                rect.width,
                rect.height,
                ptr::null_mut(),
                ptr::null_mut(),
                h_instance,
                ptr::null(),
            )
        };

        if hwnd.is_null() {
            return Err("CreateWindowExW failed");
        }

        crate::ime::set_window_ime_enabled(hwnd, false);

        if flags.contains(WindowFlags::CUSTOM_FRAMELESS) && !flags.contains(WindowFlags::LAYERED) {
            unsafe {
                let margins = MARGINS {
                    cxLeftWidth: 1,
                    cxRightWidth: 1,
                    cyTopHeight: 0,
                    cyBottomHeight: 1,
                };
                DwmExtendFrameIntoClientArea(hwnd, &margins);
                SetWindowPos(
                    hwnd,
                    ptr::null_mut(),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
                );
            }
            set_window_frameless_config(hwnd, CustomFramelessConfig::default());
        }

        let layered_surface = if flags.contains(WindowFlags::LAYERED) {
            crate::surface::WindowsSurface::create(
                hwnd,
                rect.width.max(1) as u32,
                rect.height.max(1) as u32,
            )
            .ok()
        } else {
            None
        };

        // A click-through window needs `WS_EX_LAYERED` with attributes (`setWindowLayered`).
        if flags.contains(WindowFlags::CLICK_THROUGH) && !flags.contains(WindowFlags::LAYERED) {
            apply_system_window_opacity(
                hwnd,
                LayeredNeeds {
                    opacity: 1.0,
                    click_through: true,
                    per_pixel: layered_surface.is_some(),
                    backdrop: false,
                },
            );
        }

        Ok(Self {
            hwnd,
            title: title.to_string(),
            flags,
            geometry: rect,
            drop_target: std::ptr::null_mut(),
            opacity: 1.0,
            size_limits: crate::size_limits::SizeLimits::default(),
            layered_surface,
            presenter: None,
            owner_thread: std::thread::current().id(),
            state_flags: std::cell::Cell::new(PlatformWindowStateFlags::NONE),
            ime_enabled: std::cell::Cell::new(false),
            target_pos: None,
            dcomp_downgraded: false,
            backdrop_active: false,
            backdrop_state: crate::backdrop::BackdropState::new(),
        })
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn flags(&self) -> WindowFlags {
        self.flags
    }

    pub fn geometry(&self) -> Rect {
        #[cfg(windows)]
        if !self.hwnd.is_null() {
            let mut r: RECT = unsafe { std::mem::zeroed() };
            if unsafe { GetWindowRect(self.hwnd, &mut r) } != 0 {
                return Rect::new(r.left, r.top, r.right - r.left, r.bottom - r.top);
            }
        }
        self.geometry
    }

    pub fn bind_event_loop(&self, handle: EventLoopHandle, receiver: ObjectId) {
        if !self.hwnd.is_null() {
            register_window_event_binding(self.hwnd, handle, receiver);
        }
    }

    pub fn unbind_event_loop(&self) {
        if !self.hwnd.is_null() {
            unregister_window_event_binding(self.hwnd);
        }
    }

    pub fn set_event_handler(&mut self, handler: Box<dyn WindowSystemEventHandler>) {
        if !self.hwnd.is_null() {
            set_window_event_handler(self.hwnd, handler);
        }
    }

    pub fn show(&self) {
        if !self.hwnd.is_null() {
            unsafe {
                let cmd = if self.flags.contains(WindowFlags::TOOLTIP) {
                    SW_SHOWNOACTIVATE
                } else {
                    SW_SHOW
                };
                ShowWindow(self.hwnd, cmd);
            }
        }
    }

    pub fn hide(&self) {
        if !self.hwnd.is_null() {
            unsafe {
                ShowWindow(self.hwnd, SW_HIDE);
            }
        }
    }

    /// `QWindowsWindow::raise`: `SetWindowPos(HWND_TOP, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE)`.
    /// Reorders the window in the z-order without activating it; a topmost window stays in the
    /// topmost band.
    pub fn raise(&self) {
        use windows_sys::Win32::UI::WindowsAndMessaging::HWND_TOP;
        if !self.hwnd.is_null() {
            // SAFETY: `hwnd` is the live window this value owns.
            unsafe {
                SetWindowPos(
                    self.hwnd,
                    HWND_TOP,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
            }
        }
    }

    /// `QWindowsWindow::requestActivateWindow`: `SetForegroundWindow` on a visible window that
    /// takes activation. The system's foreground lock is not worked around: when this process may
    /// not take the foreground, Windows flashes the window's taskbar button instead and the window
    /// does not become active.
    pub fn request_activate_window(&self) {
        use windows_sys::Win32::UI::WindowsAndMessaging::{IsWindowVisible, SetForegroundWindow};
        if self.hwnd.is_null() || self.flags.contains(WindowFlags::TOOLTIP) {
            return;
        }
        // SAFETY: `hwnd` is the live window this value owns.
        unsafe {
            if IsWindowVisible(self.hwnd) != 0 {
                SetForegroundWindow(self.hwnd);
            }
        }
    }

    pub fn set_geometry(&mut self, rect: Rect) {
        if !self.hwnd.is_null() {
            let _guard = SetGeometryGuard::new(&self.state_flags);
            unsafe {
                SetWindowPos(
                    self.hwnd,
                    ptr::null_mut(),
                    rect.x,
                    rect.y,
                    rect.width,
                    rect.height,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            if (rect.width != self.geometry.width || rect.height != self.geometry.height)
                && rect.width > 0
                && rect.height > 0
            {
                sync_interactive_resize(self.hwnd, &mut self.presenter, &mut self.layered_surface);
                if let Some(surface) = &mut self.layered_surface {
                    if surface
                        .resize(rect.width as u32, rect.height as u32)
                        .is_err()
                    {
                        self.layered_surface = None;
                    }
                }
                if let Some(presenter) = &mut self.presenter {
                    if presenter
                        .resize(rect.width as u32, rect.height as u32)
                        .is_err()
                    {
                        self.presenter = None;
                    }
                }
            }
            self.geometry = rect;
        }
    }

    pub fn set_stays_on_top(&mut self, enabled: bool) {
        if !self.hwnd.is_null() {
            let insert_after = topmost_insert_after(enabled);
            unsafe {
                SetWindowPos(
                    self.hwnd,
                    insert_after,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
            }
            self.flags.set(WindowFlags::STAYS_ON_TOP, enabled);
        }
    }

    pub fn set_click_through(&mut self, enabled: bool) {
        if !self.hwnd.is_null() {
            use windows_sys::Win32::UI::WindowsAndMessaging::{
                GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE,
            };
            unsafe {
                let ex_style = GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE) as u32;
                let new_style = if enabled {
                    ex_style | WS_EX_TRANSPARENT
                } else {
                    ex_style & !WS_EX_TRANSPARENT
                };
                SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, new_style as isize);
            }
            self.flags.set(WindowFlags::CLICK_THROUGH, enabled);
            // `qwindowswindow.cpp:498`: Qt requires `WS_EX_LAYERED` for `WindowTransparentForInput`
            // in combination with `WS_EX_TRANSPARENT`, with layered attributes set so the window is
            // drawn. A `LAYERED` window is managed by its presenter.
            if !self.flags.contains(WindowFlags::LAYERED) {
                apply_system_window_opacity(self.hwnd, self.layered_needs());
            }
        }
    }

    pub fn start_system_drag(&self) {
        let _ = self.start_system_move();
    }

    pub fn start_system_move(&self) -> bool {
        if !self.hwnd.is_null() {
            unsafe {
                use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_SYSCOMMAND};
                release_capture_for_system_loop();
                PostMessageW(self.hwnd, WM_SYSCOMMAND, 0xF012 /*SC_DRAGMOVE*/, 0);
            }
            true
        } else {
            false
        }
    }

    pub fn start_system_resize(&self, edges: crate::platform_window::WindowEdges) -> bool {
        if !self.hwnd.is_null() {
            unsafe {
                use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_SYSCOMMAND};
                release_capture_for_system_loop();
                let orientation = edges_to_win_orientation(edges);
                PostMessageW(self.hwnd, WM_SYSCOMMAND, orientation, 0);
            }
            true
        } else {
            false
        }
    }

    /// `QWindowsWindow::setOpacity`. A `LAYERED` window's presenter blends the opacity into every
    /// present; any other window is made layered by the system while it is translucent
    /// (`QWindowsWindow::setWindowLayered` / `setWindowOpacity`, `qwindowswindow.cpp:494-530`).
    pub fn set_opacity(&mut self, opacity: f32) {
        let opacity = opacity.clamp(0.0, 1.0);
        let changed = self.opacity != opacity;
        self.opacity = opacity;
        if let Some(p) = &mut self.presenter {
            p.set_opacity(opacity);
        }
        if changed && !self.hwnd.is_null() && !self.flags.contains(WindowFlags::LAYERED) {
            apply_system_window_opacity(self.hwnd, self.layered_needs());
        }
    }

    pub fn opacity(&self) -> f32 {
        self.opacity
    }

    /// What this window needs `WS_EX_LAYERED` for right now.
    fn layered_needs(&self) -> LayeredNeeds {
        LayeredNeeds {
            opacity: self.opacity,
            click_through: self.flags.contains(WindowFlags::CLICK_THROUGH),
            per_pixel: self.flags.contains(WindowFlags::LAYERED) || self.layered_surface.is_some(),
            backdrop: self.backdrop_active,
        }
    }

    /// Applies the state's effective backdrop and refreshes `backdrop_active` and the layered style.
    fn apply_backdrop_state(&mut self) -> bool {
        let effective = self.backdrop_state.effective();
        let applied = crate::backdrop::set_window_backdrop(
            self.hwnd,
            effective,
            self.backdrop_state.dark_mode(),
        );
        // Tracks the request, not the outcome: the DWM calls can fail after changing part of the
        // state, so a failed request may still leave a backdrop that the style must not strip.
        // `FlatTint` is a backdrop too: it is an accent policy that needs the layered style.
        self.backdrop_active = effective != crate::backdrop::BackdropType::None;
        // Dropping the backdrop lets a window that nothing else keeps layered go plain again.
        let plain = !self.hwnd.is_null() && !self.flags.contains(WindowFlags::LAYERED);
        if !self.backdrop_active && plain {
            apply_system_window_opacity(self.hwnd, self.layered_needs());
        }
        applied
    }

    /// `QWindow::setMinimumSize` (logical pixels). Windows learns it through `WM_GETMINMAXINFO`,
    /// which converts it with the window's ratio at that moment (`SizeLimits::native_track`).
    pub fn set_minimum_size(&mut self, min_w: i32, min_h: i32) {
        self.size_limits.min = crate::size_limits::clamp_size_limit(min_w, min_h);
        self.publish_size_limits();
    }

    pub fn minimum_size(&self) -> (i32, i32) {
        self.size_limits.min
    }

    /// `QWindow::setMaximumSize`, same contract as `set_minimum_size`.
    pub fn set_maximum_size(&mut self, max_w: i32, max_h: i32) {
        self.size_limits.max = crate::size_limits::clamp_size_limit(max_w, max_h);
        self.publish_size_limits();
    }

    pub fn maximum_size(&self) -> (i32, i32) {
        self.size_limits.max
    }

    /// Makes the limits visible to the window procedure, which only has the `HWND`.
    fn publish_size_limits(&self) {
        if !self.hwnd.is_null() {
            set_window_size_limits(self.hwnd, self.size_limits);
        }
    }

    pub fn create_layered_surface(&self) -> Result<crate::layered::LayeredSurface, &'static str> {
        crate::layered::LayeredSurface::new(
            self.hwnd,
            self.geometry.width as u32,
            self.geometry.height as u32,
        )
    }

    pub fn get_or_create_layered_surface(
        &mut self,
        width: u32,
        height: u32,
    ) -> Result<&mut crate::surface::WindowsSurface, &'static str> {
        if self.layered_surface.is_none() {
            self.layered_surface = Some(crate::surface::WindowsSurface::create(
                self.hwnd, width, height,
            )?);
        }
        sync_interactive_resize(self.hwnd, &mut self.presenter, &mut self.layered_surface);
        let surface = self.layered_surface.as_mut().unwrap();
        if surface.width() != width || surface.height() != height {
            if let Err(e) = surface.resize(width, height) {
                self.layered_surface = None;
                return Err(e);
            }
        }
        Ok(self.layered_surface.as_mut().unwrap())
    }

    /// The current presenter, if one has been created (instrumentation/tests).
    pub fn presenter(&self) -> Option<&crate::presenter::WindowsPresenter> {
        self.presenter.as_ref()
    }

    pub fn presenter_mut(&mut self) -> Option<&mut crate::presenter::WindowsPresenter> {
        self.presenter.as_mut()
    }

    pub fn is_dcomp_downgraded(&self) -> bool {
        self.dcomp_downgraded
    }

    pub fn set_dcomp_downgraded(&mut self, downgraded: bool) {
        self.dcomp_downgraded = downgraded;
    }

    /// Sets the presenter directly, for tests / fault injection.
    #[cfg(any(test, feature = "test-hooks"))]
    pub fn set_presenter_for_test(
        &mut self,
        presenter: Option<crate::presenter::WindowsPresenter>,
    ) {
        self.presenter = presenter;
    }
    pub fn get_or_create_presenter(
        &mut self,
        width: u32,
        height: u32,
    ) -> Result<&mut crate::presenter::WindowsPresenter, &'static str> {
        if self.presenter.is_none() {
            let p = if self.flags.contains(WindowFlags::LAYERED) {
                if !self.dcomp_downgraded && is_dcomp_enabled() {
                    if let Ok(dcomp) =
                        crate::surface::dcomp::DCompSurface::new(self.hwnd, width, height)
                    {
                        crate::presenter::WindowsPresenter::DirectComposition(dcomp)
                    } else {
                        let layered = crate::presenter::Win32LayeredPresenter::new(
                            self.hwnd,
                            width,
                            height,
                            self.opacity,
                        )?;
                        crate::presenter::WindowsPresenter::Layered(layered)
                    }
                } else {
                    let layered = crate::presenter::Win32LayeredPresenter::new(
                        self.hwnd,
                        width,
                        height,
                        self.opacity,
                    )?;
                    crate::presenter::WindowsPresenter::Layered(layered)
                }
            } else {
                let dc = crate::presenter::Win32DcPresenter::new(self.hwnd, width, height)?;
                crate::presenter::WindowsPresenter::Dc(dc)
            };
            self.presenter = Some(p);
        }
        sync_interactive_resize(self.hwnd, &mut self.presenter, &mut self.layered_surface);
        let p = self.presenter.as_mut().unwrap();
        if let Err(e) = p.resize(width, height) {
            if let crate::presenter::WindowsPresenter::DirectComposition(_) = p {
                self.dcomp_downgraded = true;
            }
            self.presenter = None;
            return Err(e);
        }
        Ok(self.presenter.as_mut().unwrap())
    }

    pub fn close(&mut self) {
        self.layered_surface = None;
        self.presenter = None;
        if !self.drop_target.is_null() {
            // `DestroyWindow` neither revokes the OLE registration nor releases our reference.
            crate::drag_drop::win32_ole::revoke_drop_target(self.hwnd, self.drop_target);
            self.drop_target = std::ptr::null_mut();
        }
        if !self.hwnd.is_null() {
            // Windows would destroy the windows this one owns with it (QTBUG-35499).
            crate::window_region::release_owned_windows(self.hwnd, NATIVE_WINDOW_CLASS_NAME);
            remove_window_size_limits(self.hwnd);
            unsafe {
                DestroyWindow(self.hwnd);
            }
            self.hwnd = ptr::null_mut();
        }
    }

    pub fn set_custom_frameless_config(&mut self, caption_height: i32, resize_border: i32) {
        if !self.hwnd.is_null() {
            set_window_frameless_config(
                self.hwnd,
                CustomFramelessConfig {
                    caption_height,
                    resize_border,
                },
            );
        }
    }

    pub fn custom_frameless_config(&self) -> Option<CustomFramelessConfig> {
        if !self.hwnd.is_null() {
            get_window_frameless_config(self.hwnd)
        } else {
            None
        }
    }
}

#[cfg(windows)]
impl crate::platform_window::PlatformWindow for NativeWindow {
    fn state_flags(&self) -> PlatformWindowStateFlags {
        self.state_flags.get()
    }

    fn owner_thread(&self) -> std::thread::ThreadId {
        self.owner_thread
    }

    fn show(&self) {
        self.show();
    }

    fn hide(&self) {
        self.hide();
    }

    fn raise(&self) {
        self.raise();
    }

    fn request_activate_window(&self) {
        self.request_activate_window();
    }

    /// `QWindowsWindow::isActive` (`qwindowswindow.cpp:1904-1910`): the window is the foreground
    /// window, or a child of it.
    fn is_active(&self) -> bool {
        if self.hwnd.is_null() {
            return false;
        }
        let foreground = unsafe { GetForegroundWindow() };
        !foreground.is_null()
            && (foreground == self.hwnd || unsafe { IsChild(foreground, self.hwnd) } != 0)
    }

    fn geometry(&self) -> Rect {
        self.geometry()
    }

    fn set_geometry(&mut self, rect: Rect) {
        self.set_geometry(rect);
    }
    fn set_target_pos(&mut self, pos: Option<ferrokit_gui::geometry::Point>) {
        self.target_pos = pos;
    }

    fn set_stays_on_top(&mut self, enabled: bool) {
        self.set_stays_on_top(enabled);
    }

    fn set_click_through(&mut self, enabled: bool) {
        self.set_click_through(enabled);
    }

    fn flags(&self) -> WindowFlags {
        self.flags
    }

    fn start_system_drag(&self) {
        let _ = self.start_system_move();
    }

    fn start_system_move(&self) -> bool {
        self.start_system_move()
    }

    fn start_system_resize(&self, edges: crate::platform_window::WindowEdges) -> bool {
        self.start_system_resize(edges)
    }
    fn set_cursor(&mut self, shape: crate::cursor::CursorShape) {
        set_window_cursor_shape(self.hwnd, shape);
        // An application override cursor stays on top (`QWindowsWindow::applyCursor`).
        if crate::cursor::win32_cursor::Win32Cursor::override_shape().is_none() {
            crate::cursor::win32_cursor::Win32Cursor::apply_shape(shape);
        }
    }

    fn unset_cursor(&mut self) {
        use crate::cursor::win32_cursor::Win32Cursor;
        unset_window_cursor_shape(self.hwnd);
        // A window without a cursor shows the default (`QWindowsWindow::applyCursor`, null cursor).
        if Win32Cursor::override_shape().is_none() {
            Win32Cursor::apply_shape(Win32Cursor::current_global_shape());
        }
    }

    fn set_mouse_grab_enabled(&mut self, grab: bool) -> bool {
        set_mouse_grab_enabled_native(self.hwnd, grab)
    }

    fn has_mouse_grab(&self) -> bool {
        // SAFETY: plain Win32 query.
        !self.hwnd.is_null() && unsafe { GetCapture() } == self.hwnd
    }

    fn set_opacity(&mut self, opacity: f32) {
        self.set_opacity(opacity);
    }

    fn opacity(&self) -> f32 {
        self.opacity
    }

    fn set_minimum_size(&mut self, min_w: i32, min_h: i32) {
        self.set_minimum_size(min_w, min_h);
    }

    fn minimum_size(&self) -> (i32, i32) {
        self.minimum_size()
    }

    fn set_maximum_size(&mut self, max_w: i32, max_h: i32) {
        self.set_maximum_size(max_w, max_h);
    }

    fn maximum_size(&self) -> (i32, i32) {
        self.maximum_size()
    }

    fn present_region(
        &mut self,
        pixmap: &ferrokit_gui::paint::Pixmap,
        dirty: &ferrokit_gui::geometry::Region,
    ) -> Result<(), &'static str> {
        let width = pixmap.physical_width();
        let height = pixmap.physical_height();
        let opacity = self.opacity;
        // Native: the caller mapped it through the screen the window lands on.
        let target_pos = self.target_pos.take();
        let dbg_t = crate::resize_debug::start();
        let res = match self.get_or_create_presenter(width, height) {
            Ok(p) => {
                p.set_opacity(opacity);
                if let crate::presenter::WindowsPresenter::Layered(lp) = p {
                    if let Some(pos) = target_pos {
                        lp.set_target_pos(Some(windows_sys::Win32::Foundation::POINT {
                            x: pos.x,
                            y: pos.y,
                        }));
                    }
                }
                p.present(pixmap, dirty)
            }
            Err(e) => Err(e),
        };

        if res.is_err() {
            crate::resize_debug::count(crate::resize_debug::Count::PresentError);
            if let Some(crate::presenter::WindowsPresenter::DirectComposition(_)) = &self.presenter
            {
                // Runtime DComp Present failure: downgrade this window to Layered GDI permanently
                self.dcomp_downgraded = true;
            }
            self.presenter.take();
            let p = self.get_or_create_presenter(width, height)?;
            if let crate::presenter::WindowsPresenter::Layered(lp) = p {
                if let Some(pos) = target_pos {
                    lp.set_target_pos(Some(windows_sys::Win32::Foundation::POINT {
                        x: pos.x,
                        y: pos.y,
                    }));
                }
            }
            let full = ferrokit_gui::geometry::Region::from_coords(0, 0, width as i32, height as i32);
            let r = p.present(pixmap, &full);
            if r.is_ok() {
                if let Some(pos) = target_pos {
                    self.geometry.x = pos.x;
                    self.geometry.y = pos.y;
                }
                self.geometry.width = width as i32;
                self.geometry.height = height as i32;
            }
            crate::resize_debug::end(crate::resize_debug::Phase::Present, dbg_t);
            return r;
        }

        if let Some(pos) = target_pos {
            self.geometry.x = pos.x;
            self.geometry.y = pos.y;
        }
        self.geometry.width = width as i32;
        self.geometry.height = height as i32;

        crate::resize_debug::end(crate::resize_debug::Phase::Present, dbg_t);
        Ok(())
    }

    fn present(
        &mut self,
        pixmap: &mut ferrokit_gui::paint::Pixmap,
        opacity: f32,
    ) -> Result<(), &'static str> {
        self.set_opacity(opacity);
        let width = pixmap.physical_width();
        let height = pixmap.physical_height();
        let full = ferrokit_gui::geometry::Region::from_coords(0, 0, width as i32, height as i32);
        self.present_region(pixmap, &full)
    }

    fn present_dirty(
        &mut self,
        pixmap: &mut ferrokit_gui::paint::Pixmap,
        opacity: f32,
        dirty_rect: Rect,
    ) -> Result<(), &'static str> {
        self.set_opacity(opacity);
        let region = ferrokit_gui::geometry::Region::from_rect(dirty_rect);
        self.present_region(pixmap, &region)
    }
    fn set_event_handler(&mut self, handler: Box<dyn WindowSystemEventHandler>) {
        self.set_event_handler(handler);
    }

    fn reports_native_double_clicks(&self) -> bool {
        // `WM_*BUTTONDBLCLK` arrives from the OS (`CS_DBLCLKS` on the window class).
        true
    }

    fn native_handle(&self) -> isize {
        self.hwnd as isize
    }

    fn device_pixel_ratio(&self) -> f32 {
        get_window_dpr(self.hwnd)
    }

    fn native_size(&self) -> Option<(u32, u32)> {
        if self.hwnd.is_null() {
            return None;
        }
        let mut r: RECT = unsafe { std::mem::zeroed() };
        // Same rect `UpdateLayeredWindowIndirect` is given as `psize`.
        if unsafe { GetWindowRect(self.hwnd, &mut r) } == 0 {
            return None;
        }
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        (w > 0 && h > 0).then_some((w as u32, h as u32))
    }

    fn set_mask(&mut self, region: &ferrokit_gui::geometry::Region) {
        crate::window_region::set_window_mask(self.hwnd, region);
    }

    fn set_transient_parent(&mut self, parent: Option<isize>) {
        crate::window_region::set_owner(self.hwnd, parent.map(|p| p as HWND));
    }

    fn transient_parent(&self) -> Option<isize> {
        crate::window_region::owner_of(self.hwnd).map(|h| h as isize)
    }

    fn set_backdrop(&mut self, backdrop: crate::backdrop::BackdropType, dark_mode: bool) -> bool {
        self.backdrop_state.request(backdrop, dark_mode);
        self.apply_backdrop_state()
    }

    fn suspend_backdrop(&mut self) -> bool {
        // Tracks the request, not the outcome, like `set_backdrop`.
        let changed = self.backdrop_state.suspend();
        if changed {
            self.apply_backdrop_state();
        }
        changed
    }

    fn resume_backdrop(&mut self) -> bool {
        let changed = self.backdrop_state.resume();
        if changed {
            self.apply_backdrop_state();
        }
        changed
    }

    fn set_ime_focus(&mut self, pos: ferrokit_gui::geometry::primitives::Point) {
        if !self.ime_enabled.replace(true) {
            crate::ime::set_window_ime_enabled(self.hwnd, true);
        }
        let mut ime = crate::ime::Win32InputContext::new(self.hwnd);
        ime.set_micro_focus(pos);
    }

    fn enable_drop_target(&mut self, enabled: bool) -> bool {
        if enabled {
            if !self.drop_target.is_null() {
                return true;
            }
            let hwnd_isize = self.hwnd as isize;
            let callback = move |ev: crate::drag_drop::DropEvent| {
                let hwnd = hwnd_isize as HWND;
                match ev {
                    crate::drag_drop::DropEvent::Enter {
                        pos,
                        formats,
                        effect,
                    } => {
                        dispatch_window_system_event(
                            Delivery::Default,
                            hwnd,
                            WindowSystemEvent::DragEnter {
                                pos,
                                formats,
                                drop_action: effect,
                            },
                        );
                    }
                    crate::drag_drop::DropEvent::Over { pos, effect } => {
                        dispatch_window_system_event(
                            Delivery::Default,
                            hwnd,
                            WindowSystemEvent::DragMove {
                                pos,
                                drop_action: effect,
                            },
                        );
                    }
                    crate::drag_drop::DropEvent::Leave => {
                        dispatch_window_system_event(
                            Delivery::Default,
                            hwnd,
                            WindowSystemEvent::DragLeave,
                        );
                    }
                    crate::drag_drop::DropEvent::Drop {
                        pos,
                        formats,
                        data,
                        effect,
                    } => {
                        dispatch_window_system_event(
                            Delivery::Default,
                            hwnd,
                            WindowSystemEvent::Drop {
                                pos,
                                formats,
                                data,
                                drop_action: effect,
                            },
                        );
                    }
                }
            };
            if let Ok(target) =
                crate::drag_drop::win32_ole::register_drop_target(self.hwnd, callback)
            {
                self.drop_target = target;
                true
            } else {
                false
            }
        } else {
            if !self.drop_target.is_null() {
                crate::drag_drop::win32_ole::revoke_drop_target(self.hwnd, self.drop_target);
                self.drop_target = std::ptr::null_mut();
            }
            true
        }
    }
}

#[cfg(windows)]
impl Drop for NativeWindow {
    fn drop(&mut self) {
        self.close();
    }
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
        unsafe {
            use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_SYSCOMMAND};
            release_capture_for_system_loop();
            PostMessageW(_hwnd as HWND, WM_SYSCOMMAND, 0xF012 /*SC_DRAGMOVE*/, 0);
        }
        return true;
    }
    false
}

pub fn post_system_resize(_hwnd: isize, _edges: crate::platform_window::WindowEdges) -> bool {
    #[cfg(windows)]
    if _hwnd != 0 {
        unsafe {
            use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_SYSCOMMAND};
            release_capture_for_system_loop();
            let orientation = edges_to_win_orientation(_edges);
            PostMessageW(_hwnd as HWND, WM_SYSCOMMAND, orientation, 0);
        }
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
    let (w, h) = if _hwnd != 0 {
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
    };
    #[cfg(not(windows))]
    let (w, h) = (600, 400);

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
mod tests {
    use super::*;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, IsWindow, GWL_EXSTYLE, GWL_STYLE, WS_CAPTION, WS_EX_LAYERED,
        WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
    };

    #[test]
    fn test_window_flags_default_and_bits() {
        let default_flags = WindowFlags::default();
        assert_eq!(default_flags, WindowFlags::NORMAL);

        let combined = WindowFlags::FRAMELESS | WindowFlags::STAYS_ON_TOP | WindowFlags::LAYERED;
        assert!(combined.contains(WindowFlags::FRAMELESS));
        assert!(combined.contains(WindowFlags::STAYS_ON_TOP));
        assert!(combined.contains(WindowFlags::LAYERED));
        assert!(!combined.contains(WindowFlags::TOOL));
    }

    #[test]
    fn test_window_flags_to_win32_styles() {
        let rect = Rect::new(100, 100, 400, 300);
        let win = NativeWindow::new("Style Test Window", rect, WindowFlags::NORMAL)
            .expect("failed to create standard window");

        unsafe {
            assert_ne!(IsWindow(win.hwnd()), 0);
            let style = GetWindowLongPtrW(win.hwnd(), GWL_STYLE) as u32;
            let ex_style = GetWindowLongPtrW(win.hwnd(), GWL_EXSTYLE) as u32;

            assert_ne!(style & WS_OVERLAPPEDWINDOW, 0);
            assert_eq!(style & WS_POPUP, 0);
            assert_eq!(ex_style & WS_EX_LAYERED, 0);
        }

        let frameless_win = NativeWindow::new(
            "Frameless Test Window",
            rect,
            WindowFlags::FRAMELESS
                | WindowFlags::STAYS_ON_TOP
                | WindowFlags::LAYERED
                | WindowFlags::TOOL,
        )
        .expect("failed to create frameless layered window");

        unsafe {
            assert_ne!(IsWindow(frameless_win.hwnd()), 0);
            let style = GetWindowLongPtrW(frameless_win.hwnd(), GWL_STYLE) as u32;
            let ex_style = GetWindowLongPtrW(frameless_win.hwnd(), GWL_EXSTYLE) as u32;

            assert_ne!(style & WS_POPUP, 0);
            assert_ne!(ex_style & WS_EX_TOPMOST, 0);
            assert_ne!(ex_style & WS_EX_LAYERED, 0);
            assert_ne!(ex_style & WS_EX_TOOLWINDOW, 0);
        }

        let custom_win = NativeWindow::new(
            "Custom Frameless Test Window",
            rect,
            WindowFlags::CUSTOM_FRAMELESS | WindowFlags::STAYS_ON_TOP,
        )
        .expect("failed to create custom window");

        unsafe {
            assert_ne!(IsWindow(custom_win.hwnd()), 0);
            let style = GetWindowLongPtrW(custom_win.hwnd(), GWL_STYLE) as u32;
            let ex_style = GetWindowLongPtrW(custom_win.hwnd(), GWL_EXSTYLE) as u32;

            assert_ne!(style & WS_THICKFRAME, 0);
            assert_ne!(style & WS_CAPTION, 0);
            assert_ne!(style & WS_MAXIMIZEBOX, 0);
            assert_eq!(style & WS_POPUP, 0);
            assert_ne!(ex_style & WS_EX_TOPMOST, 0);
        }
    }

    #[test]
    fn test_custom_frameless_nccalcsize_and_nchittest() {
        let rect = Rect::new(200, 200, 600, 400);
        let mut win = NativeWindow::new(
            "NCCalcSize & NCHitTest Window",
            rect,
            WindowFlags::CUSTOM_FRAMELESS,
        )
        .expect("failed to create window");

        win.set_custom_frameless_config(40, 10);
        let cfg = win.custom_frameless_config().expect("failed to get config");
        assert_eq!(cfg.caption_height, 40);
        assert_eq!(cfg.resize_border, 10);

        unsafe {
            let mut ncp: NCCALCSIZE_PARAMS = std::mem::zeroed();
            ncp.rgrc[0] = RECT {
                left: 200,
                top: 200,
                right: 800,
                bottom: 600,
            };
            let ret = native_window_proc(win.hwnd(), WM_NCCALCSIZE, 1, &mut ncp as *mut _ as isize);
            assert_eq!(ret, 0);

            let lparam_topleft = (202 & 0xffff) | ((202 & 0xffff) << 16);
            let hit = native_window_proc(win.hwnd(), WM_NCHITTEST, 0, lparam_topleft);
            assert_eq!(hit, HTTOPLEFT as isize);

            let lparam_bottomright = (798 & 0xffff) | ((598 & 0xffff) << 16);
            let hit = native_window_proc(win.hwnd(), WM_NCHITTEST, 0, lparam_bottomright);
            assert_eq!(hit, HTBOTTOMRIGHT as isize);

            let lparam_caption = (500 & 0xffff) | ((220 & 0xffff) << 16);
            let hit = native_window_proc(win.hwnd(), WM_NCHITTEST, 0, lparam_caption);
            assert_eq!(hit, HTCAPTION as isize);

            let lparam_client = (500 & 0xffff) | ((300 & 0xffff) << 16);
            let hit = native_window_proc(win.hwnd(), WM_NCHITTEST, 0, lparam_client);
            assert_eq!(hit, HTCLIENT as isize);
        }
    }

    #[test]
    fn test_native_window_lifecycle_and_methods() {
        let rect = Rect::new(100, 100, 500, 300);
        let mut win = NativeWindow::new("Lifecycle Window", rect, WindowFlags::FRAMELESS)
            .expect("failed to create window");

        assert_eq!(win.geometry(), rect);
        assert_eq!(win.title(), "Lifecycle Window");

        let new_rect = Rect::new(150, 150, 600, 400);
        win.set_geometry(new_rect);
        assert_eq!(win.geometry(), new_rect);

        win.set_stays_on_top(true);
        assert!(win.flags().contains(WindowFlags::STAYS_ON_TOP));

        win.set_click_through(true);
        assert!(win.flags().contains(WindowFlags::CLICK_THROUGH));

        win.show();
        win.hide();

        win.close();
        assert!(win.hwnd().is_null());
    }

    /// `set_stays_on_top(false)` must clear `WS_EX_TOPMOST` again, not only set it.
    /// Passes on a desktop session; on the GitHub `windows-latest` runner `WS_EX_TOPMOST` stays clear
    /// after `set_stays_on_top(true)`, so it runs only with `--ignored`.
    #[test]
    #[ignore = "needs an interactive desktop: the CI runner does not apply HWND_TOPMOST"]
    fn test_set_stays_on_top_toggles_topmost_exstyle() {
        let mut win = NativeWindow::new(
            "Topmost Toggle",
            Rect::new(100, 100, 300, 200),
            WindowFlags::NORMAL,
        )
        .expect("failed to create window");
        let topmost = |w: &NativeWindow| unsafe {
            GetWindowLongPtrW(w.hwnd(), GWL_EXSTYLE) as u32 & WS_EX_TOPMOST
        };

        win.set_stays_on_top(true);
        assert_ne!(
            topmost(&win),
            0,
            "set_stays_on_top(true) must set WS_EX_TOPMOST"
        );
        win.set_stays_on_top(false);
        assert_eq!(
            topmost(&win),
            0,
            "set_stays_on_top(false) must clear WS_EX_TOPMOST"
        );
    }

    /// `set_click_through` toggles `WS_EX_TRANSPARENT` in both directions.
    #[test]
    fn test_set_click_through_toggles_transparent_exstyle() {
        let mut win = NativeWindow::new(
            "Click Through Toggle",
            Rect::new(100, 100, 300, 200),
            WindowFlags::NORMAL,
        )
        .expect("failed to create window");
        let transparent = |w: &NativeWindow| unsafe {
            GetWindowLongPtrW(w.hwnd(), GWL_EXSTYLE) as u32 & WS_EX_TRANSPARENT
        };
        let layered = |w: &NativeWindow| unsafe {
            GetWindowLongPtrW(w.hwnd(), GWL_EXSTYLE) as u32 & WS_EX_LAYERED
        };

        win.set_click_through(true);
        assert_ne!(
            transparent(&win),
            0,
            "set_click_through(true) must set WS_EX_TRANSPARENT"
        );
        assert_ne!(
            layered(&win),
            0,
            "set_click_through(true) must ensure WS_EX_LAYERED for click-through"
        );
        win.set_click_through(false);
        assert_eq!(
            transparent(&win),
            0,
            "set_click_through(false) must clear WS_EX_TRANSPARENT"
        );
        assert_eq!(
            layered(&win),
            0,
            "set_click_through(false) must restore non-layered style when opacity == 1.0"
        );
    }

    /// `set_stays_on_top` chooses the z-order target for the requested state. The OS
    /// applies the band, so only the choice is asserted; this runs without a desktop.
    #[test]
    fn test_stays_on_top_selects_topmost_insert_after() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{HWND_NOTOPMOST, HWND_TOPMOST};
        assert_eq!(topmost_insert_after(true), HWND_TOPMOST);
        assert_eq!(topmost_insert_after(false), HWND_NOTOPMOST);
    }

    /// Toggling stays-on-top at run time keeps the window visible and click-through
    /// intact. It asserts the stays-on-top flag and visibility, not `WS_EX_TOPMOST`, which the CI
    /// runner does not apply (see `test_set_stays_on_top_toggles_topmost_exstyle`).
    #[test]
    fn test_stays_on_top_toggle_keeps_window_visible_and_click_through() {
        use windows_sys::Win32::UI::WindowsAndMessaging::IsWindowVisible;
        let mut win = NativeWindow::new(
            "Topmost Visible",
            Rect::new(100, 100, 300, 200),
            WindowFlags::NORMAL,
        )
        .expect("failed to create window");
        win.show();
        win.set_click_through(true);

        win.set_stays_on_top(true);
        assert!(win.flags().contains(WindowFlags::STAYS_ON_TOP));
        win.set_stays_on_top(false);
        assert!(!win.flags().contains(WindowFlags::STAYS_ON_TOP));

        assert_ne!(
            unsafe { IsWindowVisible(win.hwnd()) },
            0,
            "stays-on-top toggle must not hide the window"
        );
        let transparent =
            unsafe { GetWindowLongPtrW(win.hwnd(), GWL_EXSTYLE) as u32 & WS_EX_TRANSPARENT };
        assert_ne!(
            transparent, 0,
            "stays-on-top toggle must keep WS_EX_TRANSPARENT"
        );
    }

    #[test]
    fn test_native_window_wndproc_events() {
        let rect = Rect::new(50, 50, 300, 200);
        let win = NativeWindow::new("WndProc Event Window", rect, WindowFlags::NORMAL)
            .expect("failed to create window");

        unsafe {
            let erase_ret = native_window_proc(win.hwnd(), WM_ERASEBKGND, 0, 0);
            assert_eq!(erase_ret, 1);

            let mouse_pos = (50 & 0xffff) | ((60 & 0xffff) << 16);
            let _ = native_window_proc(win.hwnd(), WM_MOUSEMOVE, 0, mouse_pos);
            let _ = native_window_proc(win.hwnd(), WM_LBUTTONDOWN, 0, mouse_pos);
        }
    }

    #[test]
    fn wheel_deltas_negate_horizontal_like_qt() {
        // qwindowspointerhandler.cpp / qwindowsmousehandler.cpp: "Qt horizontal wheel rotation
        // orientation is opposite to the one in WM_MOUSEHWHEEL": delta = -delta.
        assert_eq!(wheel_deltas(true, 120), (-120, 0));
        assert_eq!(wheel_deltas(true, -120), (120, 0));
        assert_eq!(wheel_deltas(false, 120), (0, 120));
    }

    struct DropsWindow(#[allow(dead_code)] Option<NativeWindow>);
    impl WindowSystemEventHandler for DropsWindow {
        fn handle_window_event(&mut self, _event: WindowSystemEvent) {}
    }

    #[test]
    fn unregister_drops_handler_outside_registry_locks() {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rect = Rect::new(50, 50, 300, 200);
            let a = NativeWindow::new("Unregister A", rect, WindowFlags::NORMAL).unwrap();
            let b = NativeWindow::new("Unregister B", rect, WindowFlags::NORMAL).unwrap();
            let hwnd_a = a.hwnd();
            set_window_event_handler(hwnd_a, Box::new(DropsWindow(Some(b))));
            // Dropping A's handler drops window B, which unregisters B and takes the same locks.
            unregister_window_event_binding(hwnd_a);
            drop(a);
            let _ = tx.send(());
        });
        rx.recv_timeout(std::time::Duration::from_secs(20))
            .expect("unregister deadlocked: handler dropped while registry locks held");
    }

    fn needs(opacity: f32, click_through: bool, per_pixel: bool, backdrop: bool) -> LayeredNeeds {
        LayeredNeeds {
            opacity,
            click_through,
            per_pixel,
            backdrop,
        }
    }

    const LAYERED_BIT: isize = WS_EX_LAYERED as isize;

    /// Opacity 0.5 -> 1.0 -> 0.5 on a plain window drops and restores the style, and only the
    /// translucent states ask for `LWA_ALPHA`.
    #[test]
    fn layered_style_follows_opacity_round_trip_on_plain_window() {
        let mut ex = 0isize;
        for (opacity, layered) in [(0.5, true), (1.0, false), (0.5, true)] {
            let n = needs(opacity, false, false, false);
            ex = n.layered_ex_style(ex);
            assert_eq!(ex & LAYERED_BIT != 0, layered, "opacity {opacity}");
            assert_eq!(n.wants_alpha_attributes(ex), layered, "opacity {opacity}");
        }
    }

    /// A click-through window stays layered at opacity 1.0 and through an opacity round trip.
    #[test]
    fn click_through_keeps_layered_across_opacity_round_trip() {
        let mut ex = WS_EX_TRANSPARENT as isize;
        for opacity in [0.5, 1.0, 0.5, 1.0] {
            let n = needs(opacity, true, false, false);
            ex = n.layered_ex_style(ex);
            assert_ne!(ex & LAYERED_BIT, 0, "opacity {opacity}");
            assert_ne!(ex & WS_EX_TRANSPARENT as isize, 0, "transparent bit untouched");
            assert!(n.wants_alpha_attributes(ex), "click-through draws via LWA_ALPHA");
        }
        // Turning click-through off at opacity 1.0 releases the style again.
        assert_eq!(needs(1.0, false, false, false).layered_ex_style(ex) & LAYERED_BIT, 0);
    }

    /// A per-pixel window never loses the style, whatever opacity / click-through do, and never
    /// gets `SetLayeredWindowAttributes`, which would break `UpdateLayeredWindow`.
    #[test]
    fn per_pixel_window_keeps_layered_and_skips_alpha_attributes() {
        let mut ex = LAYERED_BIT;
        let steps = [(0.5, false), (1.0, false), (1.0, true), (0.5, true), (1.0, false)];
        for (opacity, click_through) in steps {
            let n = needs(opacity, click_through, true, false);
            ex = n.layered_ex_style(ex);
            assert_ne!(ex & LAYERED_BIT, 0, "opacity {opacity}, click_through {click_through}");
            assert!(!n.wants_alpha_attributes(ex));
        }
    }

    /// An active backdrop pins the style that is already there, and does not add it by itself.
    #[test]
    fn backdrop_keeps_existing_layered_but_does_not_add_it() {
        let n = needs(1.0, false, false, true);
        assert_ne!(n.layered_ex_style(LAYERED_BIT) & LAYERED_BIT, 0);
        assert_eq!(n.layered_ex_style(0) & LAYERED_BIT, 0);
        // Without the backdrop the same state strips it.
        assert_eq!(needs(1.0, false, false, false).layered_ex_style(LAYERED_BIT) & LAYERED_BIT, 0);
    }

    /// 0.5 -> 1.0 with a backdrop: the style stays, so alpha must be written again (255), or the
    /// window stays at the old 128 while the code believes it is opaque.
    #[test]
    fn backdrop_opacity_round_trip_rewrites_alpha() {
        let mut ex = 0isize;
        for opacity in [0.5, 1.0, 0.5, 1.0] {
            let n = needs(opacity, false, false, true);
            ex = n.layered_ex_style(ex);
            assert_ne!(ex & LAYERED_BIT, 0, "opacity {opacity}");
            assert!(n.wants_alpha_attributes(ex), "opacity {opacity}: alpha must be applied");
        }
        // Not layered (backdrop never added the style): nothing to write.
        assert!(!needs(1.0, false, false, true).wants_alpha_attributes(0));
    }

    /// Suspending Acrylic swaps in the flat tint and resuming restores it; the window keeps
    /// counting as backdrop-active throughout, so its layered style is never stripped.
    #[test]
    fn test_suspend_and_resume_backdrop_keep_backdrop_active() {
        use crate::backdrop::BackdropType;
        use crate::platform_window::PlatformWindow;
        let mut win = NativeWindow::new(
            "Backdrop Suspend",
            Rect::new(100, 100, 300, 200),
            WindowFlags::NORMAL,
        )
        .expect("failed to create window");

        assert!(!win.suspend_backdrop(), "nothing to suspend without a backdrop");
        assert!(!win.resume_backdrop(), "nothing to resume without a suspension");

        win.set_backdrop(BackdropType::Acrylic, true);
        assert!(win.backdrop_active);
        assert!(win.suspend_backdrop());
        assert!(win.backdrop_active, "FlatTint is an active backdrop");
        assert_eq!(win.backdrop_state.effective(), BackdropType::FlatTint);
        assert!(!win.suspend_backdrop(), "already suspended");
        assert!(win.resume_backdrop());
        assert!(win.backdrop_active);
        assert_eq!(win.backdrop_state.effective(), BackdropType::Acrylic);
        assert!(!win.resume_backdrop(), "already resumed");

        win.set_backdrop(BackdropType::None, false);
        assert!(!win.backdrop_active);
    }
}
