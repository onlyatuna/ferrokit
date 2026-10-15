// SPDX-License-Identifier: LGPL-3.0-only
//! `QWindowsCursor::size` (qwindowscursor.cpp:675-692) and `screenCursorSize` (:162-179).
//! Expected values are worked out by hand from that source with `SM_CXCURSOR` = 32 (the Windows
//! default), i.e. `baseSize` = 16, not by running the port.

use ferrokit_platform::cursor::{windows_cursor_size, GenericCursor, PlatformCursor};

#[test]
fn without_the_registry_value_the_cursor_is_half_the_base_size() {
    // qwindowscursor.cpp:681-682: `QSize(baseSize / 2, baseSize / 2)` = 8.
    assert_eq!(windows_cursor_size(32, None, 1.0, 1.0), 8);
}

#[test]
fn the_default_registry_value_gives_the_base_size_at_any_dpr() {
    // CursorBaseSize 32: scaled value = 32 * dpr; (v - 32) / 16 = 0 for dpr 1.0 and 1.25 (v = 40).
    assert_eq!(windows_cursor_size(32, Some(32), 1.0, 1.0), 16);
    assert_eq!(windows_cursor_size(32, Some(32), 1.25, 1.25), 16);
}

#[test]
fn each_accessibility_step_adds_eight_pixels() {
    // 48 -> (48 - 32) / 16 = 1 step; 256 -> (256 - 32) / 16 = 14 steps of 8 px: 16 + 14 * 8.
    assert_eq!(windows_cursor_size(32, Some(48), 1.0, 1.0), 24);
    assert_eq!(windows_cursor_size(32, Some(256), 1.0, 1.0), 128);
}

#[test]
fn a_screen_with_another_dpi_rescales_the_primary_cursor_width() {
    // Screen dpr 2.0 on a primary 1.0: width 64, base 32; value 32 * 2 = 64; (64 - 64) / 32 = 0.
    assert_eq!(windows_cursor_size(32, Some(32), 2.0, 1.0), 32);
    // Same screen without the registry value: 64 / 2 / 2 = 16.
    assert_eq!(windows_cursor_size(32, None, 2.0, 1.0), 16);
}

#[test]
fn the_platform_default_is_sixteen_by_sixteen() {
    // QPlatformCursor::size (qplatformcursor.cpp:100-103).
    assert_eq!(GenericCursor::new().size(1.0, 1.0), (16, 16));
    assert_eq!(GenericCursor::new().size(2.0, 1.0), (16, 16));
}
