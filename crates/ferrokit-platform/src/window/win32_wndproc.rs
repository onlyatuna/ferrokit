// SPDX-License-Identifier: LGPL-3.0-only
use super::*;

#[cfg(windows)]
pub(super) fn query_keyboard_modifiers() -> KeyboardModifiers {
    // SAFETY: GetKeyState takes only an integer virtual-key code and reads this thread's input
    // state; it has no preconditions.
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
pub(super) fn mouse_buttons_from_mk(wparam: WPARAM) -> ferrokit_core::event::MouseButtons {
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
pub(super) fn get_cursor_global_pos() -> ferrokit_gui::geometry::primitives::Point {
    // SAFETY: POINT is a plain integer struct, so the all-zero value is valid; `pt` is a local out
    // pointer valid for the GetCursorPos call.
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
pub(super) struct LayeredNeeds {
    pub(super) opacity: f32,
    pub(super) click_through: bool,
    /// The window is presented per pixel (`LAYERED` flag, or a layered surface / presenter):
    /// `UpdateLayeredWindow` needs the style for as long as the window lives.
    pub(super) per_pixel: bool,
    /// A DWM backdrop is applied: the style is never stripped while the window has it.
    pub(super) backdrop: bool,
}

#[cfg(windows)]
impl LayeredNeeds {
    /// `SetLayeredWindowAttributes(LWA_ALPHA)` applies to every layered window that is not
    /// presented per pixel, including one whose style is merely kept (backdrop) at opacity 1.0:
    /// skipping the call there would leave the previous alpha in force. A per-pixel window must
    /// not get it: once `UpdateLayeredWindow` drives the window the call is rejected, and it would
    /// also make the next `UpdateLayeredWindow` fail; its opacity is in each present's blend.
    pub(super) fn wants_alpha_attributes(self, ex_style_after: isize) -> bool {
        !self.per_pixel && ex_style_after & WS_EX_LAYERED as isize != 0
    }

    /// The extended style for `ex_style` after this state: layered whenever translucent,
    /// click-through or per-pixel; left untouched while a backdrop is active; otherwise plain.
    pub(super) fn layered_ex_style(self, ex_style: isize) -> isize {
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
pub(super) fn apply_system_window_opacity(hwnd: HWND, needs: LayeredNeeds) {
    use windows_sys::Win32::Graphics::Gdi::InvalidateRect;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GetWindowLongPtrW, IsWindowVisible, LWA_ALPHA, SetLayeredWindowAttributes,
        SetWindowLongPtrW,
    };
    // SAFETY: `hwnd` is the window being styled (validated by user32; stale handles make the
    // calls fail harmlessly); the only pointer argument is null, which InvalidateRect accepts to
    // mean the whole client area (per its documentation).
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
// `hwnd` is passed only to `GetDpiForWindow`, which returns 0 for an invalid handle.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn get_window_dpr(hwnd: HWND) -> f32 {
    // SAFETY: GetDpiForWindow takes only a handle and returns 0 for an invalid one (per its
    // documentation), which the `dpi > 0` check below maps to a scale of 1.0.
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
pub(super) fn window_global_position(hwnd: HWND, native: Point, dpr: f32) -> Point {
    match crate::screen::win32_screen::window_screen_mapping(hwnd as isize) {
        Some((origin, _)) => crate::high_dpi::from_native_global_point(native, origin, dpr),
        None => crate::high_dpi::from_native_point(native, dpr),
    }
}

#[cfg(windows)]
pub(super) fn get_x_lparam(lparam: LPARAM) -> i32 {
    (lparam as usize & 0xffff) as i16 as i32
}

#[cfg(windows)]
pub(super) fn get_y_lparam(lparam: LPARAM) -> i32 {
    ((lparam as usize >> 16) & 0xffff) as i16 as i32
}

#[cfg(windows)]
/// Win32 Window Procedure (wndproc).
///
/// # Safety
/// Called as a window procedure: `hwnd` is a window of the ferrokit class on the calling thread,
/// and `wparam`/`lparam` carry what Windows documents for `msg` (several messages hand over
/// pointers that are dereferenced, e.g. `WM_GETMINMAXINFO`, `WM_NCCALCSIZE`, `WM_DPICHANGED`).
pub(super) unsafe extern "system" fn native_window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: catches unwinding from native_window_proc_inner and falls back to DefWindowProcW.
    unsafe {
        let _t = ferrokit_gui::startup_trace::span_min(3.0, || format!("wndproc msg=0x{msg:04X}"));
        // Python/PySide6 contains a raising handler; unwinding out of an extern "system" fn aborts.
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            native_window_proc_inner(hwnd, msg, wparam, lparam)
        })) {
            Ok(result) => result,
            Err(_) => def_wnd_proc(hwnd, msg, wparam, lparam),
        }
    }
}

#[cfg(windows)]
#[inline]
pub(super) fn def_wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: forwards unhandled or default messages to DefWindowProcW with parameters provided by Windows.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// The body of [`native_window_proc`], whose panics that function contains.
///
/// # Safety
/// As for [`native_window_proc`].
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
                // SAFETY: for WM_NCCALCSIZE with `wParam != 0`, `lParam` points to an
                // `NCCALCSIZE_PARAMS` owned by the sender and valid for the duration of the message
                // (per the WM_NCCALCSIZE documentation); it is dereferenced only here, on this
                // thread, and the exclusive reference does not outlive this arm.
                let ncp = unsafe { &mut *(lparam as *mut NCCALCSIZE_PARAMS) };
                let client_rect = &mut ncp.rgrc[0];
                // SAFETY: IsZoomed, MonitorFromWindow, and GetMonitorInfoW validate hwnd and use valid local struct.
                let _is_zoomed = unsafe {
                    if IsZoomed(hwnd) != 0 {
                        let mut monitor_info: MONITORINFO = std::mem::zeroed();
                        monitor_info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
                        let h_monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
                        if !h_monitor.is_null()
                            && GetMonitorInfoW(h_monitor, &mut monitor_info) != 0
                        {
                            *client_rect = monitor_info.rcWork;
                        }
                        true
                    } else {
                        false
                    }
                };
                return 0;
            }
            def_wnd_proc(hwnd, msg, wparam, lparam)
        }
        WM_NCHITTEST => {
            if let Some(config) = get_window_frameless_config(hwnd) {
                let x = get_x_lparam(lparam);
                let y = get_y_lparam(lparam);

                // SAFETY: GetWindowRect writes to valid local RECT; IsZoomed takes valid HWND.
                let (window_rect, is_zoomed) = unsafe {
                    let mut window_rect: RECT = std::mem::zeroed();
                    GetWindowRect(hwnd, &mut window_rect);
                    (window_rect, IsZoomed(hwnd) != 0)
                };
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
            def_wnd_proc(hwnd, msg, wparam, lparam)
        }
        WM_GETMINMAXINFO => {
            use windows_sys::Win32::UI::WindowsAndMessaging::MINMAXINFO;
            if let Some(limits) = get_window_size_limits(hwnd) {
                // SAFETY: for WM_GETMINMAXINFO `lParam` points to a `MINMAXINFO` owned by the
                // sender and valid for the duration of the message (per the WM_GETMINMAXINFO
                // documentation); it is dereferenced only here, on this thread, and the exclusive
                // reference does not outlive this arm.
                let mmi = unsafe { &mut *(lparam as *mut MINMAXINFO) };
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
            def_wnd_proc(hwnd, msg, wparam, lparam)
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
            def_wnd_proc(hwnd, msg, wparam, lparam)
        }
        windows_sys::Win32::UI::WindowsAndMessaging::WM_EXITSIZEMOVE => {
            set_interactive_resize_flag(hwnd, false);
            // SAFETY: hwnd is valid window receiving WM_ENTERSIZEMOVE on calling thread.
            unsafe { sync_mouse_buttons_after_move_loop(hwnd) };
            dispatch_window_system_event(
                Delivery::Default,
                hwnd,
                WindowSystemEvent::InteractiveResizeEnd,
            );
            crate::resize_debug::exit_size_move();
            def_wnd_proc(hwnd, msg, wparam, lparam)
        }
        WM_DESTROY => {
            set_interactive_resize_flag(hwnd, false);
            unregister_window_event_binding(hwnd);
            crate::dark_mode::window_destroyed(hwnd);
            // A window destroyed other than through `close()` (with its parent, or by a foreign
            // `DestroyWindow`) must give up OLE's drop-target reference while it still exists.
            crate::drag_drop::win32_ole::revoke_on_destroy(hwnd);
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
                return def_wnd_proc(hwnd, msg, wparam, lparam);
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
            // SAFETY: RECT is a plain integer struct, so the all-zero value is valid.
            // SAFETY: RECT is a plain integer struct; GetWindowRect writes to valid local RECT.
            let (x, y) = unsafe {
                let mut r: RECT = std::mem::zeroed();
                if GetWindowRect(hwnd, &mut r) != 0 {
                    let logical_pt = window_global_position(hwnd, Point::new(r.left, r.top), dpr);
                    (logical_pt.x, logical_pt.y)
                } else {
                    (0, 0)
                }
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
            def_wnd_proc(hwnd, msg, wparam, lparam)
        }
        WM_MOVE => {
            // Qt ignores a move while iconic: `QWindowsWindow::handleMoved` only runs the geometry
            // change when `!IsIconic` (qwindowswindow.cpp:2328-2333). Windows reports the iconic
            // position as (-32000, -32000), which must not reach the geometry or the event receiver.
            // SAFETY: `hwnd` is the window this message was dispatched to; IsIconic takes only the
            // handle and validates it.
            // SAFETY: IsIconic validates hwnd.
            if unsafe { IsIconic(hwnd) } != 0 {
                return def_wnd_proc(hwnd, msg, wparam, lparam);
            }
            crate::resize_debug::wm_move_begin(hwnd as isize);
            let dpr = get_window_dpr(hwnd);
            let phys_x = get_x_lparam(lparam);
            let phys_y = get_y_lparam(lparam);
            let pos = window_global_position(hwnd, Point::new(phys_x, phys_y), dpr);
            // SAFETY: RECT is a plain integer struct, so the all-zero value is valid.
            // SAFETY: RECT is plain integer struct; GetWindowRect writes to valid local RECT.
            let (w, h) = unsafe {
                let mut r: RECT = std::mem::zeroed();
                if GetWindowRect(hwnd, &mut r) != 0 {
                    let logical_size = crate::high_dpi::from_native_size(
                        ferrokit_gui::geometry::primitives::Size::new(
                            r.right - r.left,
                            r.bottom - r.top,
                        ),
                        dpr,
                    );
                    (logical_size.width, logical_size.height)
                } else {
                    (0, 0)
                }
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
            def_wnd_proc(hwnd, msg, wparam, lparam)
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
            def_wnd_proc(hwnd, msg, wparam, lparam)
        }
        WM_PAINT => {
            crate::resize_debug::count(crate::resize_debug::Count::WmPaint);
            if let Some((handle, receiver)) = get_window_event_binding(hwnd) {
                handle.post_event(receiver, Event::new_spontaneous(EventKind::Expose));
            }
            def_wnd_proc(hwnd, msg, wparam, lparam)
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
            def_wnd_proc(hwnd, msg, wparam, lparam)
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
            def_wnd_proc(hwnd, msg, wparam, lparam)
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
                // SAFETY: `rect_ptr` is non-null (checked above); for WM_DPICHANGED `lParam` points
                // to the suggested new window RECT, valid for the duration of the message (per the
                // SAFETY: rect_ptr points to suggested RECT from Windows; SetWindowPos uses valid parameters.
                unsafe {
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
            def_wnd_proc(hwnd, msg, wparam, lparam)
        }
        WM_SETTINGCHANGE | WM_THEMECHANGED => {
            crate::integration::platform().theme().refresh();
            def_wnd_proc(hwnd, msg, wparam, lparam)
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
            def_wnd_proc(hwnd, msg, wparam, lparam)
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
                    // SAFETY: GetMessageTime takes no arguments and reads the time of the message
                    // this thread is currently handling (we are inside its wndproc).
                    // SAFETY: GetMessageTime reads time of current message handled on this thread.
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
            release_capture_when_idle(hwnd, mouse_buttons_from_mk(wparam));
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
            // SAFETY: tme points to initialized TRACKMOUSEEVENT struct.
            unsafe { TrackMouseEvent(&mut tme) };

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
            // SAFETY: hwnd is valid window and pt is valid POINT struct.
            unsafe { windows_sys::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut pt) };
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
            def_wnd_proc(hwnd, msg, wparam, lparam)
        }
        WM_CAPTURECHANGED => {
            // `lparam` is the window gaining the capture. Qt 6.12.0's windows plugin has no
            // `WM_CAPTURECHANGED` (nor `WM_CANCELMODE`) handling: a capture change, including the
            // ones the native move/size loop causes, never touches the pressed buttons
            // (`QGuiApplication::mouseButtons()`); they change with mouse events and at
            // `WM_EXITSIZEMOVE` (`sync_mouse_buttons_after_move_loop`).
            let new_owner = lparam as HWND;
            set_auto_capture_flag(hwnd as isize, false);
            if new_owner != hwnd {
                // Alt-Tab, a popup or the system move loop took the capture: the widget layer
                // reconciles an explicit `grabMouse` that no longer has a native grab behind it.
                dispatch_window_system_event(
                    Delivery::Default,
                    hwnd,
                    WindowSystemEvent::MouseGrabLost,
                );
            }
            def_wnd_proc(hwnd, msg, wparam, lparam)
        }
        WM_MOUSELEAVE => {
            LAST_MOUSE_MOVE.with(|last| {
                if matches!(last.get(), Some((h, ..)) if h == hwnd as isize) {
                    last.set(None);
                }
            });
            dispatch_window_system_event(Delivery::Default, hwnd, WindowSystemEvent::MouseLeave);
            def_wnd_proc(hwnd, msg, wparam, lparam)
        }
        WM_CONTEXTMENU => {
            // lParam -1 is the menu key / Shift+F10: no pointer position, so the cursor is the
            // hint when it is over the window and the window centre otherwise (the receiver
            // positions a keyboard menu itself).
            let keyboard = lparam as u32 == u32::MAX;
            let (global_x, global_y) = if keyboard {
                // SAFETY: GetCursorPos and GetWindowRect write to valid local structs.
                let (cursor, rect) = unsafe {
                    let mut cursor: windows_sys::Win32::Foundation::POINT = std::mem::zeroed();
                    GetCursorPos(&mut cursor);
                    let mut rect: RECT = std::mem::zeroed();
                    GetWindowRect(hwnd, &mut rect);
                    (cursor, rect)
                };
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
            // SAFETY: hwnd is valid window and pt is valid POINT struct.
            unsafe { windows_sys::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut pt) };
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
            def_wnd_proc(hwnd, msg, wparam, lparam)
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
            def_wnd_proc(hwnd, msg, wparam, lparam)
        }
        windows_sys::Win32::UI::WindowsAndMessaging::WM_IME_STARTCOMPOSITION => {
            let _ = IME_COMPOSING.try_with(|c| c.borrow_mut().insert(hwnd as isize));
            def_wnd_proc(hwnd, msg, wparam, lparam)
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
            def_wnd_proc(hwnd, msg, wparam, lparam)
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
            def_wnd_proc(hwnd, msg, wparam, lparam)
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
        _ => def_wnd_proc(hwnd, msg, wparam, lparam),
    }
}
