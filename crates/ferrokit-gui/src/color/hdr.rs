// SPDX-License-Identifier: LGPL-3.0-only
//! High Dynamic Range (HDR) floating-point color representation (`QRgbaFloat32` equivalent).
//!
//! Supports extended dynamic range values (components > 1.0), linear light operations,
//! premultiplied alpha handling, and standard tone mapping operators (Reinhard, ACES, Exposure).

use std::ops::{Add, Div, Mul, Sub};

/// 32-bit floating point RGBA color supporting Extended Dynamic Range (EDR / HDR).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct HdrColor {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

/// Canonical Qt alias for 32-bit float RGBA color.
pub type RgbaFloat32 = HdrColor;
/// Short alias for floating-point RGBA color.
pub type RgbaFloat = HdrColor;

impl HdrColor {
    /// Opaque black (0, 0, 0, 1).
    pub const BLACK: Self = Self::new(0.0, 0.0, 0.0, 1.0);
    /// Opaque white (1, 1, 1, 1).
    pub const WHITE: Self = Self::new(1.0, 1.0, 1.0, 1.0);
    /// Transparent black (0, 0, 0, 0).
    pub const TRANSPARENT: Self = Self::new(0.0, 0.0, 0.0, 0.0);

    /// Constructs an HDR color from float components.
    #[inline]
    pub const fn new(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }

    /// Constructs an opaque HDR color (alpha = 1.0).
    #[inline]
    pub const fn rgb(r: f32, g: f32, b: f32) -> Self {
        Self { r, g, b, a: 1.0 }
    }

    /// Constructs from standard 8-bit integer RGBA components [0..255].
    #[inline]
    pub fn from_rgba_u8(r: u8, g: u8, b: u8, a: u8) -> Self {
        const INV255: f32 = 1.0 / 255.0;
        Self {
            r: r as f32 * INV255,
            g: g as f32 * INV255,
            b: b as f32 * INV255,
            a: a as f32 * INV255,
        }
    }

    /// Converts to standard clamped 8-bit integer RGBA components.
    #[inline]
    pub fn to_rgba_u8(&self) -> (u8, u8, u8, u8) {
        (
            (self.r * 255.0).round().clamp(0.0, 255.0) as u8,
            (self.g * 255.0).round().clamp(0.0, 255.0) as u8,
            (self.b * 255.0).round().clamp(0.0, 255.0) as u8,
            (self.a * 255.0).round().clamp(0.0, 255.0) as u8,
        )
    }

    /// Returns a new color with RGB channels premultiplied by alpha.
    #[inline]
    pub fn premultiplied(&self) -> Self {
        Self {
            r: self.r * self.a,
            g: self.g * self.a,
            b: self.b * self.a,
            a: self.a,
        }
    }

    /// Returns a new color with alpha demultiplied / unpremultiplied.
    #[inline]
    pub fn unpremultiplied(&self) -> Self {
        if self.a > 1e-6 {
            let inv_a = 1.0 / self.a;
            Self {
                r: self.r * inv_a,
                g: self.g * inv_a,
                b: self.b * inv_a,
                a: self.a,
            }
        } else {
            Self::TRANSPARENT
        }
    }

    /// Returns whether this color exceeds Standard Dynamic Range (any component > 1.0).
    #[inline]
    pub fn is_hdr(&self) -> bool {
        self.r > 1.0 || self.g > 1.0 || self.b > 1.0
    }

    /// Returns perceived luminance (ITU-R BT.709 coefficients).
    #[inline]
    pub fn luminance(&self) -> f32 {
        0.2126 * self.r + 0.7152 * self.g + 0.0722 * self.b
    }

    // --- Tone Mapping Operators (HDR -> SDR [0.0, 1.0]) ---

    /// Simple component-wise clamping to [0.0, 1.0].
    #[inline]
    pub fn tone_map_clamp(&self) -> Self {
        Self {
            r: self.r.clamp(0.0, 1.0),
            g: self.g.clamp(0.0, 1.0),
            b: self.b.clamp(0.0, 1.0),
            a: self.a.clamp(0.0, 1.0),
        }
    }

    /// Standard Reinhard tone mapping: c / (1.0 + c).
    pub fn tone_map_reinhard(&self) -> Self {
        let tm = |c: f32| if c > 0.0 { c / (1.0 + c) } else { 0.0 };
        Self {
            r: tm(self.r),
            g: tm(self.g),
            b: tm(self.b),
            a: self.a,
        }
    }

    /// Extended Reinhard tone mapping with customizable maximum white point luminance.
    pub fn tone_map_reinhard_extended(&self, white_point: f32) -> Self {
        let wp2 = (white_point * white_point).max(1e-4);
        let tm = |c: f32| {
            if c > 0.0 {
                (c * (1.0 + c / wp2)) / (1.0 + c)
            } else {
                0.0
            }
        };
        Self {
            r: tm(self.r).clamp(0.0, 1.0),
            g: tm(self.g).clamp(0.0, 1.0),
            b: tm(self.b).clamp(0.0, 1.0),
            a: self.a,
        }
    }

    /// Krzysztof Narkowicz ACES Filmic curve tone mapping.
    pub fn tone_map_aces(&self) -> Self {
        let tm = |x: f32| {
            let x = x.max(0.0);
            let a = 2.51f32;
            let b = 0.03f32;
            let c = 2.43f32;
            let d = 0.59f32;
            let e = 0.14f32;
            ((x * (a * x + b)) / (x * (c * x + d) + e)).clamp(0.0, 1.0)
        };
        Self {
            r: tm(self.r),
            g: tm(self.g),
            b: tm(self.b),
            a: self.a,
        }
    }

    /// Exposure tone mapping: 1.0 - exp(-c * exposure).
    pub fn tone_map_exposure(&self, exposure: f32) -> Self {
        let tm = |c: f32| {
            if c > 0.0 {
                1.0 - (-c * exposure).exp()
            } else {
                0.0
            }
        };
        Self {
            r: tm(self.r).clamp(0.0, 1.0),
            g: tm(self.g).clamp(0.0, 1.0),
            b: tm(self.b).clamp(0.0, 1.0),
            a: self.a,
        }
    }
}

impl Add for HdrColor {
    type Output = Self;
    #[inline]
    fn add(self, rhs: Self) -> Self::Output {
        Self {
            r: self.r + rhs.r,
            g: self.g + rhs.g,
            b: self.b + rhs.b,
            a: self.a + rhs.a,
        }
    }
}

impl Sub for HdrColor {
    type Output = Self;
    #[inline]
    fn sub(self, rhs: Self) -> Self::Output {
        Self {
            r: self.r - rhs.r,
            g: self.g - rhs.g,
            b: self.b - rhs.b,
            a: self.a - rhs.a,
        }
    }
}

impl Mul<f32> for HdrColor {
    type Output = Self;
    #[inline]
    fn mul(self, rhs: f32) -> Self::Output {
        Self {
            r: self.r * rhs,
            g: self.g * rhs,
            b: self.b * rhs,
            a: self.a * rhs,
        }
    }
}

impl Div<f32> for HdrColor {
    type Output = Self;
    #[inline]
    fn div(self, rhs: f32) -> Self::Output {
        let inv = 1.0 / rhs;
        Self {
            r: self.r * inv,
            g: self.g * inv,
            b: self.b * inv,
            a: self.a * inv,
        }
    }
}
