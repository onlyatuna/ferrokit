// SPDX-License-Identifier: LGPL-3.0-only
//! `setWindowOpacity` must take effect whichever presenter a window ends up with
//! (Qt: `qwindowswindow.cpp:494-530` for standard windows, `qwindowsbackingstore.cpp:66` for layered ones).
#![cfg(windows)]

use ferrokit_gui::geometry::Region;
use ferrokit_gui::geometry::primitives::Rect;
use ferrokit_gui::paint::Pixmap;
use ferrokit_gui::tiny_skia::Color;
use ferrokit_platform::PlatformWindow;
use ferrokit_platform::presenter::WindowsPresenter;
use ferrokit_platform::surface::dcomp::convert_rgba_to_staging_bgra;
use ferrokit_platform::window::{NativeWindow, WindowFlags};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetLayeredWindowAttributes, GetWindowLongPtrW, LWA_ALPHA, WS_EX_LAYERED,
};

fn is_layered(hwnd: isize) -> bool {
    // SAFETY: callers pass the handle of a `NativeWindow` they keep alive for the call;
    // `GetWindowLongPtrW` only reads window data and validates the handle.
    unsafe { GetWindowLongPtrW(hwnd as _, GWL_EXSTYLE) & WS_EX_LAYERED as isize != 0 }
}

fn layered_alpha(hwnd: isize) -> Option<u8> {
    let (mut key, mut alpha, mut flags) = (0u32, 0u8, 0u32);
    // SAFETY: callers pass the handle of a `NativeWindow` they keep alive for the call; `key`,
    // `alpha` and `flags` are live locals of the types the API writes (`u32`, `u8`, `u32`).
    let ok = unsafe { GetLayeredWindowAttributes(hwnd as _, &mut key, &mut alpha, &mut flags) };
    (ok != 0 && flags & LWA_ALPHA != 0).then_some(alpha)
}

#[test]
fn dcomp_staging_pixels_are_scaled_by_the_window_opacity() {
    // Premultiplied RGBA: an opaque orange pixel and a half-transparent one.
    let src = [200u8, 100, 50, 255, 100, 50, 25, 128];
    let mut dst = [0u8; 8];

    // Opaque: only the channel order changes.
    convert_rgba_to_staging_bgra(&src, &mut dst, 1.0);
    assert_eq!(dst, [50, 100, 200, 255, 25, 50, 100, 128]);

    // 50 %: a constant alpha of qRound(255 * 0.5) = 128 over a premultiplied pixel scales every
    // channel, alpha included (UpdateLayeredWindow SourceConstantAlpha).
    convert_rgba_to_staging_bgra(&src, &mut dst, 0.5);
    let (b, g, r, a) = (dst[0], dst[1], dst[2], dst[3]);
    assert_eq!(a, 128, "an opaque pixel at 50 % has alpha 128");
    assert!(
        (r as i32 - 100).abs() <= 1 && (g as i32 - 50).abs() <= 1 && (b as i32 - 25).abs() <= 1,
        "colour halves with alpha, got r={r} g={g} b={b}"
    );
    assert!(
        r <= a && g <= a && b <= a,
        "premultiplied colour never exceeds alpha"
    );
    assert!(
        (dst[7] as i32 - 64).abs() <= 1,
        "half-transparent pixel at 50 % has alpha ~64, got {}",
        dst[7]
    );

    // 0 %: nothing is visible.
    convert_rgba_to_staging_bgra(&src, &mut dst, 0.0);
    assert_eq!(dst, [0u8; 8]);
}

#[test]
fn a_standard_window_becomes_translucent_through_the_trait() {
    let mut win = NativeWindow::new(
        "opacity",
        Rect::new(50, 50, 120, 80),
        WindowFlags::FRAMELESS,
    )
    .expect("native window");
    let hwnd = win.hwnd() as isize;
    assert!(!is_layered(hwnd));

    // The window is reached through `dyn PlatformWindow`, as `ferrokit_widgets::Window` does.
    let dynamic: &mut dyn PlatformWindow = &mut win;
    dynamic.set_opacity(0.5);
    assert!(
        is_layered(hwnd),
        "a translucent standard window must be layered"
    );
    assert_eq!(layered_alpha(hwnd), Some(128));

    dynamic.set_opacity(1.0);
    assert!(
        !is_layered(hwnd),
        "an opaque standard window drops the layered style again"
    );
}

/// `WindowTransparentForInput` keeps a window layered whatever its opacity (`setWindowLayered`,
/// `qwindowswindow.cpp:498`), so an opacity round trip must not end click-through, and the
/// layered window is given an opaque alpha rather than left without layered attributes.
#[test]
fn an_opacity_round_trip_keeps_a_click_through_window_layered() {
    let mut win = NativeWindow::new(
        "click-through",
        Rect::new(50, 50, 120, 80),
        WindowFlags::FRAMELESS,
    )
    .expect("native window");
    let hwnd = win.hwnd() as isize;
    let dynamic: &mut dyn PlatformWindow = &mut win;

    dynamic.set_click_through(true);
    assert!(is_layered(hwnd), "click-through needs WS_EX_LAYERED");
    assert_eq!(
        layered_alpha(hwnd),
        Some(255),
        "an opaque click-through window has alpha 255"
    );

    dynamic.set_opacity(0.5);
    assert_eq!(layered_alpha(hwnd), Some(128));
    dynamic.set_opacity(1.0);
    assert!(
        is_layered(hwnd),
        "opacity 1.0 must keep a click-through window layered"
    );
    assert_eq!(layered_alpha(hwnd), Some(255));

    dynamic.set_click_through(false);
    assert!(
        !is_layered(hwnd),
        "an opaque window that stops being click-through is not layered"
    );
}

/// A window created click-through is layered from the start (`setWindowLayered`).
#[test]
fn a_window_created_click_through_is_layered() {
    let win = NativeWindow::new(
        "created-click-through",
        Rect::new(50, 50, 120, 80),
        WindowFlags::FRAMELESS | WindowFlags::CLICK_THROUGH,
    )
    .expect("native window");
    let hwnd = win.hwnd() as isize;
    assert!(is_layered(hwnd));
    assert_eq!(layered_alpha(hwnd), Some(255));
}

/// Needs a machine where `DCompSurface::new` succeeds. It is `#[ignore]`d rather than silently
/// passing without DirectComposition (as `test_dcomp_*` do): run it with `-- --ignored`; where
/// DirectComposition is unavailable it fails loudly instead of reporting a pass.
#[test]
#[ignore = "requires DirectComposition; run with --ignored"]
fn dcomp_window_presents_with_the_window_opacity() {
    let mut win = NativeWindow::new(
        "dcomp opacity",
        Rect::new(50, 50, 120, 80),
        WindowFlags::FRAMELESS | WindowFlags::LAYERED,
    )
    .expect("native window");
    let mut pm = Pixmap::new(120, 80).expect("pixmap");
    pm.fill(Color::from_rgba8(200, 100, 50, 255));
    let full = Region::from_coords(0, 0, 120, 80);

    win.set_opacity(0.5);
    win.present_region(&pm, &full).expect("present");
    let WindowsPresenter::DirectComposition(dcomp) = win.presenter().expect("presenter") else {
        panic!("this machine selected a presenter other than DirectComposition");
    };
    assert_eq!(dcomp.staged_pixel(10, 10).unwrap()[3], 128);

    // Back to opaque with a dirty rectangle that excludes (10, 10): the whole surface is re-staged.
    win.set_opacity(1.0);
    let corner = Region::from_coords(100, 60, 10, 10);
    win.present_region(&pm, &corner).expect("present");
    let WindowsPresenter::DirectComposition(dcomp) = win.presenter().unwrap() else {
        unreachable!()
    };
    assert_eq!(dcomp.staged_pixel(10, 10).unwrap()[3], 255);
}
