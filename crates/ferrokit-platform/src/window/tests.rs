// SPDX-License-Identifier: LGPL-3.0-only
use super::*;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GWL_STYLE, GetWindowLongPtrW, IsWindow, WS_CAPTION, WS_EX_LAYERED,
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

    // SAFETY: `win` owns the live window created above; IsWindow/GetWindowLongPtrW take only the
    // handle and read window state, no pointers.
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

    // SAFETY: `frameless_win` owns the live window created above; the calls take only the
    // handle, no pointers.
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

    // SAFETY: `custom_win` owns the live window created above; the calls take only the handle,
    // no pointers.
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

    // SAFETY: `win` owns the live window created above and the test runs on its thread; RECT and
    // NCCALCSIZE_PARAMS are plain C structs, so zeroed is valid; for WM_NCCALCSIZE with
    // wparam=1 the wndproc reads only `rgrc[0]` through the `lparam` pointer to `ncp`, which
    // outlives the call; the WM_NCHITTEST calls pass packed coordinates, no pointers.
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
    // SAFETY: in the closure, `w` owns the live window; GetWindowLongPtrW takes only the handle.
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
    // SAFETY: in the closure, `w` owns the live window; GetWindowLongPtrW takes only the handle.
    let transparent = |w: &NativeWindow| unsafe {
        GetWindowLongPtrW(w.hwnd(), GWL_EXSTYLE) as u32 & WS_EX_TRANSPARENT
    };
    // SAFETY: in the closure, `w` owns the live window; GetWindowLongPtrW takes only the handle.
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
        // SAFETY: `win` owns the live window; IsWindowVisible takes only the handle.
        unsafe { IsWindowVisible(win.hwnd()) },
        0,
        "stays-on-top toggle must not hide the window"
    );
    let transparent =
        // SAFETY: `win` owns the live window; GetWindowLongPtrW takes only the handle.
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

    // SAFETY: `win` owns the live window created above and the test runs on its creating
    // thread, as the wndproc expects; the messages are passed directly to its wndproc with
    // plain integer arguments (no pointers).
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
        assert_ne!(
            ex & WS_EX_TRANSPARENT as isize,
            0,
            "transparent bit untouched"
        );
        assert!(
            n.wants_alpha_attributes(ex),
            "click-through draws via LWA_ALPHA"
        );
    }
    // Turning click-through off at opacity 1.0 releases the style again.
    assert_eq!(
        needs(1.0, false, false, false).layered_ex_style(ex) & LAYERED_BIT,
        0
    );
}

/// A per-pixel window never loses the style, whatever opacity / click-through do, and never
/// gets `SetLayeredWindowAttributes`, which would break `UpdateLayeredWindow`.
#[test]
fn per_pixel_window_keeps_layered_and_skips_alpha_attributes() {
    let mut ex = LAYERED_BIT;
    let steps = [
        (0.5, false),
        (1.0, false),
        (1.0, true),
        (0.5, true),
        (1.0, false),
    ];
    for (opacity, click_through) in steps {
        let n = needs(opacity, click_through, true, false);
        ex = n.layered_ex_style(ex);
        assert_ne!(
            ex & LAYERED_BIT,
            0,
            "opacity {opacity}, click_through {click_through}"
        );
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
    assert_eq!(
        needs(1.0, false, false, false).layered_ex_style(LAYERED_BIT) & LAYERED_BIT,
        0
    );
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
        assert!(
            n.wants_alpha_attributes(ex),
            "opacity {opacity}: alpha must be applied"
        );
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

    assert!(
        !win.suspend_backdrop(),
        "nothing to suspend without a backdrop"
    );
    assert!(
        !win.resume_backdrop(),
        "nothing to resume without a suspension"
    );

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

#[test]
fn native_window_exposes_win32_raw_handles() {
    use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};

    let win = NativeWindow::new(
        "Raw Handle Window",
        Rect::new(20, 20, 200, 150),
        WindowFlags::NORMAL,
    )
    .expect("failed to create window");
    let win_handle = win.window_handle().expect("win32 window handle");
    assert!(matches!(
        win_handle.as_raw(),
        RawWindowHandle::Win32(h) if h.hwnd.get() == win.hwnd() as isize
    ));
    let display_handle = win.display_handle().expect("win32 display handle");
    assert!(matches!(
        display_handle.as_raw(),
        RawDisplayHandle::Windows(_)
    ));
}
