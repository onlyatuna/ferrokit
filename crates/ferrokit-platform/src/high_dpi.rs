// SPDX-License-Identifier: LGPL-3.0-only
//! High-DPI scaling helpers, matching Qt's `QHighDpi` (qhighdpiscaling.cpp).

use ferrokit_gui::geometry::primitives::{Point, Rect, Size};

/// Converts native physical pixels to device-independent logical pixels (DIPs).
/// Matches `QHighDpi::fromNativePixels`.
#[inline]
pub fn from_native_pixels(val: f32, dpr: f32) -> f32 {
    if dpr > 0.0 { val / dpr } else { val }
}

/// Converts device-independent logical pixels (DIPs) to native physical pixels.
/// Matches `QHighDpi::toNativePixels`.
#[inline]
pub fn to_native_pixels(val: f32, dpr: f32) -> f32 {
    if dpr > 0.0 { val * dpr } else { val }
}

/// Converts a physical point to a logical point, relative to the window.
/// Matches `QHighDpi::fromNativeLocalPosition`; desktop positions use `from_native_global_point`.
#[inline]
pub fn from_native_point(pt: Point, dpr: f32) -> Point {
    if dpr <= 1.0 {
        pt
    } else {
        Point::new(
            (pt.x as f32 / dpr).round() as i32,
            (pt.y as f32 / dpr).round() as i32,
        )
    }
}

/// Converts a logical point to a physical point, relative to the window.
/// Matches `QHighDpi::toNativeLocalPosition`; desktop positions use `to_native_global_point`.
#[inline]
pub fn to_native_point(pt: Point, dpr: f32) -> Point {
    if dpr <= 1.0 {
        pt
    } else {
        Point::new(
            (pt.x as f32 * dpr).round() as i32,
            (pt.y as f32 * dpr).round() as i32,
        )
    }
}

/// Converts a physical size to a logical size.
#[inline]
pub fn from_native_size(size: Size, dpr: f32) -> Size {
    if dpr <= 1.0 {
        size
    } else {
        Size::new(
            (size.width as f32 / dpr).round() as i32,
            (size.height as f32 / dpr).round() as i32,
        )
    }
}

/// Converts a logical size to a physical size.
#[inline]
pub fn to_native_size(size: Size, dpr: f32) -> Size {
    if dpr <= 1.0 {
        size
    } else {
        Size::new(
            (size.width as f32 * dpr).round() as i32,
            (size.height as f32 * dpr).round() as i32,
        )
    }
}

/// Converts a logical rect to a physical rect, relative to the window (a dirty rect).
/// Desktop rects use `to_native_global_rect`.
#[inline]
pub fn to_native_rect(rect: Rect, dpr: f32) -> Rect {
    if dpr <= 1.0 {
        rect
    } else {
        Rect::new(
            (rect.x as f32 * dpr).round() as i32,
            (rect.y as f32 * dpr).round() as i32,
            (rect.width as f32 * dpr).round() as i32,
            (rect.height as f32 * dpr).round() as i32,
        )
    }
}

// Desktop positions map relative to the native top-left of the screen they are on, which is the
// same point in both systems: `QHighDpi::fromNative`/`toNative` with that screen's factor and
// origin (`qhighdpiscaling_p.h:150-158, 270-280`, origin from `qhighdpiscaling.cpp:713-721`).
// Sizes only scale. `qRound` rounds half away from zero, as `f64::round` does.

#[inline]
fn scale_from(v: i32, origin: i32, factor: f64) -> i32 {
    origin + (f64::from(v - origin) * factor).round() as i32
}

#[inline]
fn scale_len(v: i32, factor: f64) -> i32 {
    (f64::from(v) * factor).round() as i32
}

/// Physical desktop rect to logical, on the screen whose physical top-left is `screen_origin`.
#[inline]
pub fn from_native_global_rect(rect: Rect, screen_origin: Point, dpr: f32) -> Rect {
    let factor = 1.0 / f64::from(dpr);
    Rect::new(
        scale_from(rect.x, screen_origin.x, factor),
        scale_from(rect.y, screen_origin.y, factor),
        scale_len(rect.width, factor),
        scale_len(rect.height, factor),
    )
}

/// Logical desktop rect to physical, on the screen whose top-left is `screen_origin`.
#[inline]
pub fn to_native_global_rect(rect: Rect, screen_origin: Point, dpr: f32) -> Rect {
    let factor = f64::from(dpr);
    Rect::new(
        scale_from(rect.x, screen_origin.x, factor),
        scale_from(rect.y, screen_origin.y, factor),
        scale_len(rect.width, factor),
        scale_len(rect.height, factor),
    )
}

/// Physical desktop point to logical, on the screen whose physical top-left is `screen_origin`.
#[inline]
pub fn from_native_global_point(pt: Point, screen_origin: Point, dpr: f32) -> Point {
    let factor = 1.0 / f64::from(dpr);
    Point::new(
        scale_from(pt.x, screen_origin.x, factor),
        scale_from(pt.y, screen_origin.y, factor),
    )
}

/// Logical desktop point to physical, on the screen whose top-left is `screen_origin`.
#[inline]
pub fn to_native_global_point(pt: Point, screen_origin: Point, dpr: f32) -> Point {
    let factor = f64::from(dpr);
    Point::new(
        scale_from(pt.x, screen_origin.x, factor),
        scale_from(pt.y, screen_origin.y, factor),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A window placed at a logical position reads the same position back from the native
    /// position Windows reports (WM_MOVE), on screens away from the desktop origin.
    #[test]
    fn a_desktop_position_reads_back_from_its_native_position() {
        for (origin, dpr) in [
            (Point::new(1920, 0), 1.5),
            (Point::new(-2880, 0), 1.5),
            (Point::new(0, 1080), 1.25),
            (Point::new(-1536, -864), 1.75),
        ] {
            for offset in [0, 1, 7, 333, 1000] {
                let logical = Point::new(origin.x + offset, origin.y + offset / 2);
                let native = to_native_global_point(logical, origin, dpr);
                assert_eq!(
                    from_native_global_point(native, origin, dpr),
                    logical,
                    "{origin:?} {dpr}"
                );
            }
        }
        let rect = Rect::new(2000, 100, 400, 300);
        let origin = Point::new(1920, 0);
        let native = to_native_global_rect(rect, origin, 1.5);
        assert_eq!(native, Rect::new(2040, 150, 600, 450));
        assert_eq!(from_native_global_rect(native, origin, 1.5), rect);
    }
}
