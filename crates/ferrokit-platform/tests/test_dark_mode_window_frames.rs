// SPDX-License-Identifier: LGPL-3.0-only
//! A window's dark frame attribute follows the colour scheme the way Qt's does
//! (`QWindowsWindow::setDarkBorder`, qwindowswindow.cpp:3857-3874: at creation under a dark
//! scheme, and for every window when the effective scheme changes, qwindowstheme.cpp:690-697).
//!
//! The scheme is driven by an application override (`requestColorScheme`), which only lives in
//! this process; the system's own theme is never touched. Windows stay hidden.
#![cfg(windows)]

use ferrokit_gui::geometry::primitives::Rect;
use ferrokit_platform::backdrop::{BackdropType, set_window_backdrop};
use ferrokit_platform::window::{NativeWindow, WindowFlags};
use ferrokit_platform::{ColorScheme, PlatformWindow, platform};
use windows_sys::Win32::Graphics::Dwm::{DWMWA_USE_IMMERSIVE_DARK_MODE, DwmGetWindowAttribute};

fn frame_is_dark(window: &NativeWindow) -> bool {
    let mut value: i32 = -1;
    // SAFETY: `window.hwnd()` is the live window borrowed here; `value` is a live 4-byte `i32`,
    // the size of the `BOOL` this attribute writes, and its size is passed as the buffer length.
    let hr = unsafe {
        DwmGetWindowAttribute(
            window.hwnd(),
            DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
            &mut value as *mut i32 as *mut _,
            std::mem::size_of::<i32>() as u32,
        )
    };
    assert!(hr >= 0, "DwmGetWindowAttribute failed: {hr:#x}");
    value != 0
}

fn new_window(title: &str, flags: WindowFlags) -> NativeWindow {
    NativeWindow::new(title, Rect::new(0, 0, 200, 100), flags).expect("window")
}

#[test]
fn window_frames_follow_the_requested_colour_scheme() {
    let theme = platform().theme();

    // A window made under a dark scheme starts dark; a frameless one has no frame to darken.
    theme.request_color_scheme(ColorScheme::Dark);
    let framed = new_window("dark frame", WindowFlags::NORMAL);
    let frameless = new_window("frameless", WindowFlags::FRAMELESS);
    assert!(frame_is_dark(&framed), "created under Dark");
    assert!(!frame_is_dark(&frameless), "frameless stays light");

    // The scheme moving reaches the windows that already exist.
    theme.request_color_scheme(ColorScheme::Light);
    assert!(!frame_is_dark(&framed), "Dark -> Light");
    theme.request_color_scheme(ColorScheme::Dark);
    assert!(frame_is_dark(&framed), "Light -> Dark");
    assert!(!frame_is_dark(&frameless), "frameless stays light");

    // A window made under a light scheme starts light.
    theme.request_color_scheme(ColorScheme::Light);
    let light = new_window("light frame", WindowFlags::NORMAL);
    assert!(!frame_is_dark(&light), "created under Light");

    // A backdrop sets the attribute from its own flag, which the scheme then leaves alone.
    // `PlatformWindow::set_backdrop` and the public `set_window_backdrop` both do this.
    let mut owned = new_window("backdrop owns the frame", WindowFlags::NORMAL);
    owned.set_backdrop(BackdropType::FlatTint, false);
    let direct = new_window("direct backdrop owns the frame", WindowFlags::NORMAL);
    set_window_backdrop(direct.hwnd(), BackdropType::FlatTint, false);
    theme.request_color_scheme(ColorScheme::Dark);
    assert!(frame_is_dark(&framed));
    assert!(frame_is_dark(&light), "Light -> Dark");
    assert!(!frame_is_dark(&owned), "the application's own value stays");
    assert!(
        !frame_is_dark(&direct),
        "set_window_backdrop marks the window too"
    );

    // `BackdropType::None` gives the frame back to the scheme, at once, and ignores `dark_mode`.
    owned.set_backdrop(BackdropType::None, false);
    assert!(frame_is_dark(&owned), "None: the Dark scheme applies again");
    set_window_backdrop(direct.hwnd(), BackdropType::None, false);
    assert!(frame_is_dark(&direct), "None through the public function");
    theme.request_color_scheme(ColorScheme::Light);
    assert!(
        !frame_is_dark(&owned),
        "and it follows the scheme afterwards"
    );
    assert!(!frame_is_dark(&direct));
    theme.request_color_scheme(ColorScheme::Dark);

    // HUD-like windows count as frameless: the scheme writes nothing for them, and
    // `BackdropType::None` keeps writing the caller's own `dark_mode` (neither the scheme nor a
    // light default replaces it).
    let hud = new_window(
        "hud",
        WindowFlags::FRAMELESS
            | WindowFlags::CUSTOM_FRAMELESS
            | WindowFlags::LAYERED
            | WindowFlags::TOOL,
    );
    theme.request_color_scheme(ColorScheme::Light);
    set_window_backdrop(hud.hwnd(), BackdropType::None, true);
    assert!(frame_is_dark(&hud), "None writes the caller's dark flag");
    theme.request_color_scheme(ColorScheme::Dark);
    theme.request_color_scheme(ColorScheme::Light);
    assert!(
        frame_is_dark(&hud),
        "the scheme does not write a frameless window"
    );
    set_window_backdrop(hud.hwnd(), BackdropType::None, false);
    assert!(!frame_is_dark(&hud));
    theme.request_color_scheme(ColorScheme::Dark);
    assert!(!frame_is_dark(&hud), "not even a dark scheme");
    theme.request_color_scheme(ColorScheme::Light);

    // A closed window is no longer tracked and does not disturb the others.
    drop(framed);
    theme.request_color_scheme(ColorScheme::Light);
    assert!(!frame_is_dark(&light));

    theme.request_color_scheme(ColorScheme::Unknown);
}
