// SPDX-License-Identifier: LGPL-3.0-only
//! The cursors Qt draws itself on Windows (`QWindowsCursor::customCursor`, qwindowscursor.cpp:190-405)
//! and the shape table. The pixel expectations come from the bitmap data in that file.

use ferrokit_platform::cursor::CursorShape;
use ferrokit_platform::cursor_bitmap::custom_cursor_image;

#[test]
fn the_shape_table_follows_the_declaration_order() {
    for (i, shape) in CursorShape::ALL.iter().enumerate() {
        assert_eq!(*shape as usize, i);
    }
    // Qt's enum has 22 shapes besides `BitmapCursor`.
    assert_eq!(CursorShape::ALL.len(), 22);
}

#[test]
fn shapes_the_system_provides_have_no_custom_image() {
    for shape in [
        CursorShape::Arrow,
        CursorShape::PointingHand,
        CursorShape::Forbidden,
        CursorShape::WhatsThis,
        CursorShape::Blank,
        CursorShape::Busy,
    ] {
        assert!(custom_cursor_image(shape, 32).is_none(), "{shape:?}");
    }
}

#[test]
fn the_open_hand_is_the_16_pixel_bitmap_with_its_centre_as_hot_spot() {
    let image = custom_cursor_image(CursorShape::OpenHand, 32).unwrap();
    assert_eq!((image.width, image.height), (16, 16));
    assert_eq!((image.hot_x, image.hot_y), (8, 8));
    // Row 0 of `openhand_bits` is 0x80, 0x01 (mask the same): pixels 7 and 8, black and opaque.
    assert_eq!(image.pixel(7, 0), [0, 0, 0, 255]);
    assert_eq!(image.pixel(8, 0), [0, 0, 0, 255]);
    assert_eq!(image.pixel(6, 0)[3], 0);
    assert_eq!(image.pixel(9, 0)[3], 0);
    // Row 1 is data 0x58, 0x0e and mask 0xd8, 0x0f; bit 0 of a byte is the leftmost pixel. Data
    // bits 3, 4, 6 are black pixels (x = 3, 4, 6); mask bit 7 (x = 7) is opaque with a clear
    // data bit: white. Mask bit 2 (x = 2) is clear: transparent.
    assert_eq!(image.pixel(3, 1), [0, 0, 0, 255]);
    assert_eq!(image.pixel(6, 1), [0, 0, 0, 255]);
    assert_eq!(image.pixel(7, 1), [255, 255, 255, 255]);
    assert_eq!(image.pixel(2, 1)[3], 0);
}

#[test]
fn the_closed_hand_has_an_empty_top() {
    let image = custom_cursor_image(CursorShape::ClosedHand, 32).unwrap();
    assert_eq!((image.width, image.height), (16, 16));
    for y in 0..3 {
        for x in 0..16 {
            assert_eq!(image.pixel(x, y)[3], 0, "({x}, {y})");
        }
    }
    // Row 3 is 0xb0, 0x0d in data and mask: x = 4, 5, 7 and 8, 10, 11 are black.
    assert_eq!(image.pixel(4, 3), [0, 0, 0, 255]);
    assert_eq!(image.pixel(6, 3)[3], 0);
}

#[test]
fn the_split_cursors_are_32_pixels() {
    for shape in [CursorShape::SplitV, CursorShape::SplitH] {
        let image = custom_cursor_image(shape, 32).unwrap();
        assert_eq!((image.width, image.height), (32, 32), "{shape:?}");
        assert_eq!((image.hot_x, image.hot_y), (16, 16), "{shape:?}");
        assert!(image.bgra.chunks(4).any(|p| p[3] == 255), "{shape:?}");
    }
}

#[test]
fn a_large_system_cursor_scales_the_bitmap_cursors_but_a_small_change_does_not() {
    // 48 / 32 = 1.5: more than 0.4 off, so the 16 pixel hand becomes 24 pixels.
    let big = custom_cursor_image(CursorShape::OpenHand, 48).unwrap();
    assert_eq!((big.width, big.height), (24, 24));
    assert_eq!((big.hot_x, big.hot_y), (12, 12));
    // 40 / 32 = 1.25: within 0.4, left alone (`qAbs(factor - 1.0) > 0.4`).
    let same = custom_cursor_image(CursorShape::OpenHand, 40).unwrap();
    assert_eq!((same.width, same.height), (16, 16));
    // The scaled image keeps its opaque and its transparent parts.
    assert!(big.bgra.chunks(4).any(|p| p[3] == 255));
    assert!(big.bgra.chunks(4).any(|p| p[3] == 0));
}

#[test]
fn the_drag_cursors_come_from_their_pixmaps_with_the_hot_spot_at_the_corner() {
    let moved = custom_cursor_image(CursorShape::DragMove, 32).unwrap();
    assert_eq!((moved.width, moved.height), (11, 20));
    assert_eq!((moved.hot_x, moved.hot_y), (0, 0));
    // Row 0 is "aa.........": a is white.
    assert_eq!(moved.pixel(0, 0), [255, 255, 255, 255]);
    assert_eq!(moved.pixel(2, 0)[3], 0);
    // Row 1 is "aXa........": X is black.
    assert_eq!(moved.pixel(1, 1), [0, 0, 0, 255]);

    let copy = custom_cursor_image(CursorShape::DragCopy, 32).unwrap();
    let link = custom_cursor_image(CursorShape::DragLink, 32).unwrap();
    assert_eq!((copy.width, copy.height), (24, 30));
    assert_eq!((link.width, link.height), (24, 30));
    // The arrow is the same, the badge differs.
    assert_eq!(copy.pixel(1, 1), link.pixel(1, 1));
    assert_ne!(copy.bgra, link.bgra);
    // Drag cursors are not scaled with the system cursor.
    assert_eq!(custom_cursor_image(CursorShape::DragCopy, 64).unwrap(), copy);
}

#[cfg(windows)]
mod win32 {
    use ferrokit_platform::cursor::{CursorShape, PlatformCursor, Win32Cursor};

    #[test]
    fn every_shape_but_blank_has_a_native_cursor() {
        for shape in CursorShape::ALL {
            let handle = Win32Cursor::native_handle(shape);
            if shape == CursorShape::Blank {
                assert_eq!(handle, 0);
            } else {
                assert_ne!(handle, 0, "{shape:?}");
            }
        }
        // The custom cursors are cached: the same handle comes back.
        assert_eq!(
            Win32Cursor::native_handle(CursorShape::OpenHand),
            Win32Cursor::native_handle(CursorShape::OpenHand)
        );
        assert_ne!(
            Win32Cursor::native_handle(CursorShape::OpenHand),
            Win32Cursor::native_handle(CursorShape::ClosedHand)
        );
    }

    #[test]
    fn the_pointer_position_is_queried_from_windows() {
        assert!(Win32Cursor::new().pos().is_some());
    }

    #[test]
    fn the_override_cursor_is_remembered_until_cleared() {
        let mut cursor = Win32Cursor::new();
        assert_eq!(Win32Cursor::override_shape(), None);
        assert!(cursor.set_override_cursor(CursorShape::Wait));
        assert_eq!(Win32Cursor::override_shape(), Some(CursorShape::Wait));
        assert!(cursor.set_override_cursor(CursorShape::Busy));
        assert_eq!(Win32Cursor::override_shape(), Some(CursorShape::Busy));
        cursor.clear_override_cursor();
        assert_eq!(Win32Cursor::override_shape(), None);
    }
}
