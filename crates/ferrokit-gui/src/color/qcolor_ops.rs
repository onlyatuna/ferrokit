// SPDX-License-Identifier: LGPL-3.0-only
//! `QColor::darker` / `QColor::lighter` on the painter's `tiny_skia::Color`.
//!
//! Both delegate to [`super::qcolor::Color::darker`] / [`super::qcolor::Color::lighter`], which
//! follow `qcolor.cpp` (v6.11.2) bit for bit; the colour is rounded to its 8-bit RGBA value first and
//! the result is always an 8-bit colour.

use super::qcolor::Color;
use tiny_skia::Color as SkiaColor;

/// `QColor::darker(factor)`: `factor` 100 leaves the colour unchanged, 200 makes it half as bright.
/// `factor <= 0` returns the colour as is; `factor < 100` is `lighter(10000 / factor)`.
pub fn darker(color: SkiaColor, factor: i32) -> SkiaColor {
    adjust(color, factor, |c| c.darker(factor))
}

/// `QColor::lighter(factor)`: `factor` 100 leaves the colour unchanged, 200 makes it twice as bright.
/// `factor <= 0` returns the colour as is; `factor < 100` is `darker(10000 / factor)`.
pub fn lighter(color: SkiaColor, factor: i32) -> SkiaColor {
    adjust(color, factor, |c| c.lighter(factor))
}

fn adjust(color: SkiaColor, factor: i32, op: impl FnOnce(&Color) -> Color) -> SkiaColor {
    if factor <= 0 {
        return color;
    }
    op(&Color::from(color))
        .to_tiny_skia()
        .expect("an RGB colour stays valid")
}
