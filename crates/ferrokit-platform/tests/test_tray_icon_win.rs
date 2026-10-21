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
use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetGuiResources};
use windows_sys::Win32::UI::Shell::NIN_SELECT;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetIconInfo, IsWindow, SendMessageW, ICONINFO, WM_CONTEXTMENU,
    WM_LBUTTONDBLCLK, WM_LBUTTONUP,
};

const GR_USEROBJECTS: u32 = 1;

fn pixmap() -> Pixmap {
    let mut p = Pixmap::new(16, 16).expect("pixmap");
    p.fill(Color::from_rgba8(10, 120, 200, 255));
    p
}

fn send(hwnd: HWND, event: u32) {
    unsafe {
        SendMessageW(hwnd, WM_TRAY_CALLBACK, 0, event as isize);
    }
}

#[test]
fn the_release_that_ends_a_double_click_is_not_a_click() {
    let tray = TrayIcon::from_pixmap("t", &pixmap()).expect("tray");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    tray.on_activated.connect(move |a| sink.lock().push(*a));

    send(tray.hwnd(), WM_LBUTTONDBLCLK);
    send(tray.hwnd(), NIN_SELECT);
    send(tray.hwnd(), WM_LBUTTONUP);
    assert_eq!(
        *seen.lock(),
        vec![TrayActivation::DoubleClick, TrayActivation::Trigger],
        "double click, ignored release, then an ordinary click"
    );
}

#[test]
fn a_slot_may_drop_the_tray_icon_while_it_is_being_signalled() {
    // The icon is `!Send`: its address travels as an integer, the slot runs on this thread.
    let tray = TrayIcon::from_pixmap("t", &pixmap()).expect("tray");
    let hwnd = tray.hwnd();
    let address = Arc::new(Mutex::new(Box::into_raw(tray) as usize));
    let a = Arc::clone(&address);
    // A "Quit" entry: the icon goes away from inside the signal that is being emitted.
    unsafe { &*(*address.lock() as *const TrayIcon) }
        .on_context_menu_requested
        .connect(move |_| {
            let addr = std::mem::take(&mut *a.lock());
            if addr != 0 {
                drop(unsafe { Box::from_raw(addr as *mut TrayIcon) });
            }
        });

    send(hwnd, WM_CONTEXTMENU);

    assert_eq!(*address.lock(), 0, "the slot ran and dropped the icon");
    assert_eq!(unsafe { IsWindow(hwnd) }, 0, "the tray window died with the icon");
}

#[test]
fn a_tray_created_from_a_pixmap_releases_its_icon() {
    let objects = || unsafe { GetGuiResources(GetCurrentProcess(), GR_USEROBJECTS) };
    let before = objects();
    for _ in 0..60 {
        let tray = TrayIcon::from_pixmap("leak", &pixmap()).expect("tray");
        drop(tray);
    }
    let leaked = objects().saturating_sub(before);
    assert!(leaked < 10, "{leaked} USER objects (icons) leaked by 60 trays");
}

#[test]
fn replacing_the_icon_leaves_a_caller_owned_icon_alone() {
    let caller_icon = TrayIcon::create_hicon_from_pixmap(&pixmap()).expect("icon");
    let mut tray = TrayIcon::new("t", caller_icon).expect("tray");

    PlatformTrayIcon::set_icon(&mut *tray, &pixmap()).expect("set_icon");

    let mut info: ICONINFO = unsafe { std::mem::zeroed() };
    assert_ne!(
        unsafe { GetIconInfo(caller_icon, &mut info) },
        0,
        "the caller's icon is still alive: the tray never owned it"
    );
    unsafe {
        DeleteObject(info.hbmColor);
        DeleteObject(info.hbmMask);
    }
    drop(tray);
    TrayIcon::destroy_hicon(caller_icon);
}
