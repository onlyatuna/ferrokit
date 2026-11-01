// SPDX-License-Identifier: LGPL-3.0-only
//! The Win32 tray icon: click semantics, re-entrancy, icon ownership.
#![cfg(windows)]

use ferrokit_gui::paint::Pixmap;
use ferrokit_gui::tiny_skia::Color;
use ferrokit_platform::platform_tray::PlatformTrayIcon;
use ferrokit_platform::tray_icon::{TrayActivation, TrayIcon, WM_TRAY_CALLBACK};
use parking_lot::Mutex;
use std::sync::Arc;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::Graphics::Gdi::DeleteObject;
use windows_sys::Win32::UI::Shell::NIN_SELECT;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetIconInfo, ICONINFO, IsWindow, SendMessageW, WM_CLOSE, WM_CONTEXTMENU, WM_LBUTTONDBLCLK,
    WM_LBUTTONUP, WM_MBUTTONUP, WM_RBUTTONUP,
};

fn icon() -> ferrokit_gui::image::Icon {
    ferrokit_gui::image::Icon::from_pixmap(&pixmap())
}

fn pixmap() -> Pixmap {
    let mut p = Pixmap::new(16, 16).expect("pixmap");
    p.fill(Color::from_rgba8(10, 120, 200, 255));
    p
}

fn send(hwnd: HWND, event: u32) {
    // SAFETY: `hwnd` is a live tray window the caller got from `TrayIcon::hwnd` and keeps the icon
    // alive for; the message carries only integers (no pointer lparam), so user32 needs nothing else.
    unsafe {
        SendMessageW(hwnd, WM_TRAY_CALLBACK, 0, event as isize);
    }
}

#[test]
fn the_release_that_ends_a_double_click_is_not_a_click() {
    let tray = TrayIcon::from_icon("t", &icon()).expect("tray");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    tray.on_activated.connect(move |a| sink.lock().push(*a));

    send(tray.hwnd(), WM_LBUTTONDBLCLK);
    send(tray.hwnd(), NIN_SELECT);
    send(tray.hwnd(), NIN_SELECT);
    assert_eq!(
        *seen.lock(),
        vec![TrayActivation::DoubleClick, TrayActivation::Trigger],
        "double click, ignored release, then an ordinary click"
    );
}

/// Under `NOTIFYICON_VERSION_4` the raw button-up messages accompany `NIN_SELECT` /
/// `WM_CONTEXTMENU`; Qt acts on the latter only, so a click is one `Trigger`, not two.
#[test]
fn raw_button_up_messages_do_not_activate_the_icon() {
    let tray = TrayIcon::from_icon("t", &icon()).expect("tray");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    tray.on_activated.connect(move |a| sink.lock().push(*a));

    send(tray.hwnd(), WM_LBUTTONUP);
    send(tray.hwnd(), WM_RBUTTONUP);
    assert!(
        seen.lock().is_empty(),
        "button-up messages alone activate nothing"
    );
    send(tray.hwnd(), NIN_SELECT);
    send(tray.hwnd(), WM_CONTEXTMENU);
    assert_eq!(
        *seen.lock(),
        vec![TrayActivation::Trigger, TrayActivation::Context]
    );
}

#[test]
fn a_slot_may_drop_the_tray_icon_while_it_is_being_signalled() {
    // The icon is `!Send`: its address travels as an integer, the slot runs on this thread.
    let tray = TrayIcon::from_icon("t", &icon()).expect("tray");
    let hwnd = tray.hwnd();
    let address = Arc::new(Mutex::new(Box::into_raw(tray) as usize));
    let a = Arc::clone(&address);
    // A "Quit" entry: the icon goes away from inside the signal that is being emitted.
    // SAFETY: `address` holds the pointer from `Box::into_raw(tray)` above; it is zeroed (under the
    // lock) before the slot frees the box, and the slot has not run yet (no message sent), so the
    // pointee is alive. The shared reference lives only for the `connect` call.
    unsafe { &*(*address.lock() as *const TrayIcon) }
        .on_context_menu_requested
        .connect(move |_| {
            let addr = std::mem::take(&mut *a.lock());
            if addr != 0 {
                // SAFETY: `addr` is the `Box::into_raw` pointer from above; `mem::take` zeroed the
                // shared slot, so this is the only place that frees it and it is freed exactly once.
                // The tray's window procedure re-checks `tray_is_alive` after the slot returns.
                drop(unsafe { Box::from_raw(addr as *mut TrayIcon) });
            }
        });

    send(hwnd, WM_CONTEXTMENU);

    assert_eq!(*address.lock(), 0, "the slot ran and dropped the icon");
    assert_eq!(
        // SAFETY: `IsWindow` accepts any handle value, including that of a destroyed window.
        unsafe { IsWindow(hwnd) },
        0,
        "the tray window died with the icon"
    );
}

#[test]
fn a_tray_releases_the_icon_it_made() {
    let trays: Vec<_> = (0..3)
        .map(|_| TrayIcon::from_icon("leak", &icon()).expect("tray"))
        .collect();
    let icons: Vec<_> = trays.iter().map(|t| t.icon()).collect();
    let alive = |icon| {
        // SAFETY: an all-zero `ICONINFO` (integers, a bool and null handles) is a valid value.
        let mut info: ICONINFO = unsafe { std::mem::zeroed() };
        // SAFETY: `icon` is an `HICON` the tray still holds (or already destroyed: `GetIconInfo`
        // then fails and reports 0); `info` is a live `ICONINFO` it fills in.
        let ok = unsafe { GetIconInfo(icon, &mut info) } != 0;
        if ok {
            // SAFETY: `GetIconInfo` succeeded, so the caller owns copies of both bitmaps and
            // must delete them; they are not used again.
            unsafe {
                DeleteObject(info.hbmColor);
                DeleteObject(info.hbmMask);
            }
        }
        ok
    };
    assert!(
        icons.iter().all(|&i| alive(i)),
        "icons exist while the trays do"
    );
    drop(trays);
    for icon in icons {
        assert!(!alive(icon), "the tray destroyed the icon it created");
    }
}

#[test]
fn replacing_the_icon_leaves_a_caller_owned_icon_alone() {
    let caller_icon = TrayIcon::create_hicon_from_pixmap(&pixmap()).expect("icon");
    let mut tray = TrayIcon::new("t", caller_icon).expect("tray");

    PlatformTrayIcon::set_icon(&mut *tray, &icon()).expect("set_icon");

    // SAFETY: an all-zero `ICONINFO` (integers, a bool and null handles) is a valid value.
    let mut info: ICONINFO = unsafe { std::mem::zeroed() };
    assert_ne!(
        // SAFETY: `caller_icon` was created above by `create_hicon_from_pixmap` and is destroyed
        // only at the end of the test; `info` is a live `ICONINFO` it fills in.
        unsafe { GetIconInfo(caller_icon, &mut info) },
        0,
        "the caller's icon is still alive: the tray never owned it"
    );
    // SAFETY: `GetIconInfo` succeeded (asserted), so the caller owns copies of both bitmaps and
    // must delete them; they are not used again.
    unsafe {
        DeleteObject(info.hbmColor);
        DeleteObject(info.hbmMask);
    }
    drop(tray);
    TrayIcon::destroy_hicon(caller_icon);
}

#[test]
fn a_middle_button_release_is_a_middle_click() {
    // `QWindowsSystemTrayIcon::winEvent`: `WM_MBUTTONUP` -> `activated(MiddleClick)`.
    let tray = TrayIcon::from_icon("t", &icon()).expect("tray");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    tray.on_activated.connect(move |a| sink.lock().push(*a));

    send(tray.hwnd(), WM_MBUTTONUP);
    assert_eq!(*seen.lock(), vec![TrayActivation::MiddleClick]);
}

#[test]
fn the_context_menu_is_requested_where_the_shell_says() {
    // Under NOTIFYICON_VERSION_4 `wParam` is the anchor point (physical desktop pixels, signed
    // 16 bits each); Qt reports that point, not the cursor.
    let tray = TrayIcon::from_icon("t", &icon()).expect("tray");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    tray.on_context_menu_requested
        .connect(move |p| sink.lock().push(*p));

    for (x, y) in [(123, 456), (-5, -7)] {
        let packed = ((x as u16 as usize) | ((y as u16 as usize) << 16)) as isize;
        // SAFETY: `tray.hwnd()` is the live tray window of this thread (`tray` outlives the loop);
        // the message carries integers only, no pointers.
        unsafe {
            SendMessageW(
                tray.hwnd(),
                WM_TRAY_CALLBACK,
                packed as usize,
                WM_CONTEXTMENU as isize,
            );
        }
    }
    let expected: Vec<_> = [(123, 456), (-5, -7)]
        .iter()
        .map(|&(x, y)| {
            ferrokit_platform::screen::native_cursor_to_logical(
                ferrokit_gui::geometry::primitives::Point::new(x, y),
            )
        })
        .collect();
    assert_eq!(*seen.lock(), expected);
}

#[test]
fn a_hidden_icon_has_an_empty_geometry() {
    // `QSystemTrayIcon::geometry()` is an empty rectangle until the icon is shown (PySide6:
    // `QRect(0, 0, 0, 0)` before `show()` and after `hide()`).
    let mut tray = TrayIcon::from_icon("t", &icon()).expect("tray");
    let empty = ferrokit_gui::geometry::primitives::Rect::new(0, 0, 0, 0);
    assert_eq!(tray.geometry(), empty);
    tray.show().expect("show");
    tray.hide().expect("hide");
    assert_eq!(tray.geometry(), empty);
}

#[test]
fn the_tooltip_and_availability_are_reported() {
    let mut tray = TrayIcon::from_icon("first", &icon()).expect("tray");
    assert_eq!(tray.tooltip(), "first");
    tray.set_tooltip("second").expect("set_tooltip");
    assert_eq!(tray.tooltip(), "second");
    // PySide6: `QSystemTrayIcon.isSystemTrayAvailable()` is True.
    assert!(TrayIcon::is_system_tray_available());
}

#[test]
fn closing_the_tray_window_quits_the_application() {
    // PySide6: a `WM_CLOSE` to the tray window sends the application a Quit event, emits
    // `aboutToQuit` and makes `exec()` return 0.
    use ferrokit_core::application::CoreApplication;
    use ferrokit_core::timer::Timer;
    use std::sync::atomic::{AtomicBool, Ordering};

    CoreApplication::reset_for_test();
    let mut app = CoreApplication::new(vec!["tray_close".to_string()]);
    let tray = TrayIcon::from_icon("t", &icon()).expect("tray");
    let hwnd = tray.hwnd() as usize;
    let about_to_quit = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&about_to_quit);
    let _conn = app
        .about_to_quit()
        .connect(move |_| flag.store(true, Ordering::SeqCst));
    // SAFETY: `hwnd` is the tray window's handle, and `tray` is only dropped after `app.exec()`
    // returns, so the window is alive whenever the timer fires; the message holds integers only.
    Timer::single_shot(10, move || unsafe {
        SendMessageW(hwnd as HWND, WM_CLOSE, 0, 0);
    });
    Timer::single_shot(2000, || CoreApplication::exit(7)); // watchdog
    let code = app.exec();
    assert_eq!(code, 0, "the watchdog ended exec instead of the Quit");
    assert!(about_to_quit.load(Ordering::SeqCst));
    drop(tray);
}

fn icon_alive(icon: windows_sys::Win32::UI::WindowsAndMessaging::HICON) -> bool {
    // SAFETY: an all-zero `ICONINFO` (integers, a bool and null handles) is a valid value.
    let mut info: ICONINFO = unsafe { std::mem::zeroed() };
    // SAFETY: `icon` is an `HICON` the caller got from the tray (or already destroyed: then
    // `GetIconInfo` fails and returns 0); `info` is a live `ICONINFO` it fills in.
    let ok = unsafe { GetIconInfo(icon, &mut info) } != 0;
    if ok {
        // SAFETY: `GetIconInfo` succeeded, so the caller owns copies of both bitmaps and must
        // delete them; they are not used again.
        unsafe {
            DeleteObject(info.hbmColor);
            DeleteObject(info.hbmMask);
        }
    }
    ok
}

fn icon_width(icon: windows_sys::Win32::UI::WindowsAndMessaging::HICON) -> i32 {
    use windows_sys::Win32::Graphics::Gdi::{BITMAP, GetObjectW};
    // SAFETY: an all-zero `ICONINFO` (integers, a bool and null handles) is a valid value.
    let mut info: ICONINFO = unsafe { std::mem::zeroed() };
    // SAFETY: `icon` is the live `HICON` of a tray the caller still holds; `info` is a live
    // `ICONINFO` it fills in.
    assert!(unsafe { GetIconInfo(icon, &mut info) } != 0);
    // SAFETY: an all-zero `BITMAP` (integers and a null pointer) is a valid value.
    let mut bm: BITMAP = unsafe { std::mem::zeroed() };
    // SAFETY: `info.hbmColor` is the colour bitmap `GetIconInfo` just returned (asserted), `bm` is a
    // live `BITMAP` and the size passed is `size_of::<BITMAP>()`; both bitmaps are caller-owned
    // copies, deleted once and not used afterwards.
    unsafe {
        GetObjectW(
            info.hbmColor,
            std::mem::size_of::<BITMAP>() as i32,
            &mut bm as *mut _ as *mut _,
        );
        DeleteObject(info.hbmColor);
        DeleteObject(info.hbmMask);
    }
    bm.bmWidth
}

fn icon_of_sizes(sizes: &[u32]) -> ferrokit_gui::image::Icon {
    use ferrokit_gui::image::{Icon, IconMode, IconState};
    let mut icon = Icon::default();
    for &s in sizes {
        let mut p = Pixmap::new(s, s).expect("pixmap");
        p.fill(Color::from_rgba8(1, 2, 3, 255));
        icon.add_pixmap(&p, IconMode::Normal, IconState::Off);
    }
    icon
}

#[test]
fn an_icon_is_shown_at_the_size_of_a_small_icon() {
    // `QWindowsSystemTrayIcon::createIcon`: `QIcon::actualSize(SM_CXSMICON x SM_CYSMICON)`.
    // PySide6 (no high DPI scaling): images of 16 and 64 px give the requested size, an image
    // of 16 px never more than 16 and images of 16 and 32 px never more than 32.
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSMICON};
    // SAFETY: `GetSystemMetrics` takes a metric index and no pointers.
    let small = unsafe { GetSystemMetrics(SM_CXSMICON) };
    for (sizes, expected) in [
        (vec![16, 64], small),
        (vec![16], small.min(16)),
        (vec![16, 32], small.min(32)),
    ] {
        let tray = TrayIcon::from_icon("t", &icon_of_sizes(&sizes)).expect("tray");
        assert_eq!(icon_width(tray.icon()), expected, "images {sizes:?}");
    }
}

#[test]
fn a_new_taskbar_builds_the_icon_again_from_the_kept_icon() {
    // `QWindowsSystemTrayIcon::winEvent` on `TaskbarCreated`: `updateIcon(oldIcon)` makes a new
    // `HICON` (the size of a small icon may have changed with the DPI), then `NIM_ADD`.
    use windows_sys::Win32::UI::WindowsAndMessaging::RegisterWindowMessageW;
    let name: Vec<u16> = "TaskbarCreated\0".encode_utf16().collect();
    // SAFETY: `name` is a NUL-terminated UTF-16 string (explicit `\0`) that outlives the call.
    let msg = unsafe { RegisterWindowMessageW(name.as_ptr()) };

    let kept = TrayIcon::from_icon("kept", &icon_of_sizes(&[16, 64])).expect("tray");
    let first = kept.icon();
    // SAFETY: `kept.hwnd()` is the live tray window of this thread; the message is a registered
    // id with zero (non-pointer) parameters.
    unsafe { SendMessageW(kept.hwnd(), msg, 0, 0) };
    assert_ne!(kept.icon(), first, "a new icon was made");
    assert!(icon_alive(kept.icon()));
    assert!(
        !icon_alive(first),
        "the old icon, which the tray made, is destroyed"
    );

    // A raw `HICON` given by the caller has no source to build from: it stays, and is not
    // destroyed.
    let caller_icon = TrayIcon::create_hicon_from_pixmap(&pixmap()).expect("icon");
    let plain = TrayIcon::new("plain", caller_icon).expect("tray");
    // SAFETY: `plain.hwnd()` is the live tray window of this thread; the message is a registered
    // id with zero (non-pointer) parameters.
    unsafe { SendMessageW(plain.hwnd(), msg, 0, 0) };
    assert_eq!(plain.icon(), caller_icon);
    assert!(icon_alive(caller_icon));
    drop(plain);
    TrayIcon::destroy_hicon(caller_icon);
}

#[test]
fn the_tray_window_lets_taskbar_created_through_the_message_filter() {
    // `QWindowsSystemTrayIcon::ensureInstalled`: `ChangeWindowMessageFilterEx(hwnd, TaskbarCreated,
    // MSGFLT_ALLOW)`, so a tray in an elevated process still hears the restarted Explorer. Asking
    // again reports whether the message was already allowed for this window.
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CHANGEFILTERSTRUCT, ChangeWindowMessageFilterEx, MSGFLT_ALLOW, RegisterWindowMessageW,
    };
    // MSGFLTINFO_NONE is 0 (the message was not allowed before this call).
    const MSGFLTINFO_ALREADYALLOWED_FORWINDOW: u32 = 1;
    let name: Vec<u16> = "TaskbarCreated\0".encode_utf16().collect();
    // SAFETY: `name` is a NUL-terminated UTF-16 string (explicit `\0`) that outlives the call.
    let msg = unsafe { RegisterWindowMessageW(name.as_ptr()) };
    let tray = TrayIcon::from_icon("t", &icon()).expect("tray");
    // SAFETY: an all-zero `CHANGEFILTERSTRUCT` (integers) is a valid value.
    let mut change: CHANGEFILTERSTRUCT = unsafe { std::mem::zeroed() };
    change.cbSize = std::mem::size_of::<CHANGEFILTERSTRUCT>() as u32;
    // SAFETY: `tray.hwnd()` is the live tray window; `change` is a live `CHANGEFILTERSTRUCT` with
    // `cbSize` set on the previous line, as the API documentation requires.
    let ok = unsafe { ChangeWindowMessageFilterEx(tray.hwnd(), msg, MSGFLT_ALLOW, &mut change) };
    assert_ne!(ok, 0);
    assert_eq!(change.ExtStatus, MSGFLTINFO_ALREADYALLOWED_FORWINDOW);
}
