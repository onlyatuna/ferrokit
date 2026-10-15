// SPDX-License-Identifier: LGPL-3.0-only
//! `QColor::darker` / `QColor::lighter`.
//!
//! `qcolor.cpp:2941-2997` (`lighter`, `darker`), `:2363-2402` (`toHsv`) and `:2215-2290`
//! (`toRgb` from HSV). Qt keeps every component as 16 bits, changes only the HSV value (and, when it
//! overflows in `lighter`, the saturation) with integer arithmetic, and hands out 8-bit components
//! with `qt_div_257` (`QColor::red()`, `qdrawhelper_p.h:886-887`). All of that is reproduced here so
//! the 8-bit result matches Qt bit for bit; the result is therefore always an 8-bit colour.

use tiny_skia::Color;

/// `qRound(float)` for the non-negative values used here.
fn q_round(x: f32) -> i32 {
    (x + 0.5) as i32
}

/// 8-bit component to Qt's 16-bit component (`c * 0x101`).
fn to_u16(c: f32) -> u16 {
    ((c * 255.0).round() as u16) * 0x101
}

/// `QColor::toHsv` for an RGB colour: `(hue, saturation, value)`, hue in 1/100 degree
/// (`u16::MAX` when undefined).
fn rgb_to_hsv(r: u16, g: u16, b: u16) -> (u16, u16, u16) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    if max == min {
        return (u16::MAX, 0, max);
    }
    let chroma = (max - min) as f32;
    let sat = q_round((chroma / max as f32) * u16::MAX as f32) as u16;
    let hue = if max == r {
        let mut h = (g as f32 - b as f32) / chroma;
        if h < 0.0 {
            h += 6.0;
        }
        h
    } else if max == g {
        2.0 + (b as f32 - r as f32) / chroma
    } else {
        4.0 + (r as f32 - g as f32) / chroma
    };
    (q_round(hue * (60.0 * 100.0)) as u16, sat, max)
}

/// `QColor::toRgb` for an HSV colour.
fn hsv_to_rgb(hue: u16, sat: u16, val: u16) -> (u16, u16, u16) {
    if sat == 0 || hue == u16::MAX {
        return (val, val, val);
    }
    let h = if hue == 36000 {
        0.0
    } else {
        hue as f32 / 6000.0
    };
    let s = sat as f32 / u16::MAX as f32;
    let v = val as f32 / u16::MAX as f32;
    let i = h as i32;
    let f = h - i as f32;
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    let (r, g, b) = match i {
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        5 => (v, p, q),
        _ => (v, t, p),
    };
    let to = |x: f32| q_round(x * u16::MAX as f32) as u16;
    (to(r), to(g), to(b))
}

/// `qt_div_257`: a 16-bit component to 8 bits, rounded.
fn div_257(x: u16) -> u8 {
    let x = x as u32 + 128;
    ((x - (x >> 8)) >> 8) as u8
}

/// An 8-bit colour from the HSV components, converted the way `QColor::rgba()` does.
fn from_hsv16(hue: u16, sat: u16, val: u16, alpha: u8) -> Color {
    let (r, g, b) = hsv_to_rgb(hue, sat, val);
    Color::from_rgba8(div_257(r), div_257(g), div_257(b), alpha)
}

fn alpha8(color: Color) -> u8 {
    (color.alpha() * 255.0).round() as u8
}

/// `QColor::darker(factor)`: `factor` 100 leaves the colour unchanged, 200 makes it half as bright.
/// `factor <= 0` returns the colour as is; `factor < 100` is `lighter(10000 / factor)`.
pub fn darker(color: Color, factor: i32) -> Color {
    if factor <= 0 {
        return color;
    }
    if factor < 100 {
        return lighter(color, 10000 / factor);
    }
    let (h, s, v) = rgb_to_hsv(
        to_u16(color.red()),
        to_u16(color.green()),
        to_u16(color.blue()),
    );
    let v = ((v as u32 * 100) / factor as u32) as u16;
    from_hsv16(h, s, v, alpha8(color))
}

/// `QColor::lighter(factor)`: `factor` 100 leaves the colour unchanged, 200 makes it twice as bright.
/// `factor <= 0` returns the colour as is; `factor < 100` is `darker(10000 / factor)`.
pub fn lighter(color: Color, factor: i32) -> Color {
    if factor <= 0 {
        return color;
    }
    if factor < 100 {
        return darker(color, 10000 / factor);
    }
    let (h, s, v) = rgb_to_hsv(
        to_u16(color.red()),
        to_u16(color.green()),
        to_u16(color.blue()),
    );
    let mut s = s as i64;
    let mut v = (factor as i64 * v as i64) / 100;
    if v > u16::MAX as i64 {
        s = (s - (v - u16::MAX as i64)).max(0);
        v = u16::MAX as i64;
    }
    from_hsv16(h, s as u16, v as u16, alpha8(color))
}
