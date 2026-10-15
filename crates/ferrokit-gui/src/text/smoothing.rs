// SPDX-License-Identifier: LGPL-3.0-only
//! Sub-pixel (ClearType) text smoothing settings and the gamma table Qt blends LCD text with.
//!
//! Qt's raster engine decides once, when painting starts, whether glyphs are drawn as LCD masks
//! (`QRasterPaintEngine::begin`: Windows only, when the system font smoothing type is ClearType).
//! The system value is read by the platform layer, which hands it over through
//! [`set_text_smoothing`]; every [`Painter`](crate::paint::painter::Painter) copies it in `begin`.
//!
//! The gamma table mirrors `QColorTrcLut` (`qcolortrclut.cpp`, `qcolortrclut_p.h`).

use std::sync::{Arc, Mutex, RwLock};

/// What the platform says about text smoothing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextSmoothing {
    /// The system smooths fonts with ClearType (`SPI_GETFONTSMOOTHINGTYPE`).
    pub cleartype: bool,
    /// Gamma LCD text is blended with (`QWindowsFontDatabase::fontSmoothingGamma`).
    pub gamma: f32,
}

impl TextSmoothing {
    /// Grey-scale text, what Qt does on every platform but Windows.
    pub const OFF: Self = Self {
        cleartype: false,
        gamma: 1.0,
    };
}

static CURRENT: RwLock<TextSmoothing> = RwLock::new(TextSmoothing::OFF);

/// Sets the smoothing new painters start with.
pub fn set_text_smoothing(smoothing: TextSmoothing) {
    if let Ok(mut current) = CURRENT.write() {
        *current = smoothing;
    }
}

/// The smoothing new painters start with.
pub fn text_smoothing() -> TextSmoothing {
    CURRENT.read().map(|s| *s).unwrap_or(TextSmoothing::OFF)
}

/// Pure-gamma transfer table (`QColorTrcLut::fromGamma`).
#[derive(Debug)]
pub struct TrcLut {
    to_linear: Vec<u16>,
    from_linear: Vec<u16>,
}

impl TrcLut {
    /// Index scale of the tables: `1 << SHIFT_UP` entries per 8-bit step.
    const SHIFT_UP: u32 = 4;
    const SHIFT_DOWN: u32 = 8 - Self::SHIFT_UP;
    const RESOLUTION: usize = (1 << Self::SHIFT_UP) * 255;

    /// `QColorTrcLut::setFromGamma` for both directions.
    pub fn from_gamma(gamma: f32) -> Self {
        let i_res = 1.0 / Self::RESOLUTION as f32;
        let to_linear = (0..=Self::RESOLUTION)
            .map(|i| {
                let v = (i as f32 * i_res).powf(gamma) * (255.0 * 256.0);
                (v.round() as i32).clamp(0, 65280) as u16
            })
            .collect();
        let i_gamma = 1.0 / gamma;
        let from_linear = (0..=Self::RESOLUTION)
            .map(|i| {
                let v = (i as f32 * i_res).powf(i_gamma).clamp(0.0, 1.0) * (255.0 * 256.0);
                v.round() as u16
            })
            .collect();
        Self {
            to_linear,
            from_linear,
        }
    }

    /// An 8-bit channel as a 16-bit linear value (`toLinear64`).
    pub fn to_linear(&self, c: u8) -> u16 {
        let v = self.to_linear[(c as usize) << Self::SHIFT_UP];
        v + (v >> 8)
    }

    /// A 16-bit linear value as an 8-bit channel (`fromLinear64`).
    pub fn from_linear(&self, v: u16) -> u8 {
        let v = v - (v >> 8);
        let t = self.from_linear[(v >> Self::SHIFT_DOWN) as usize] as u32;
        ((t + 0x80) >> 8).min(255) as u8
    }
}

/// The shared table for `gamma`; tables are tiny but built per gamma only once.
pub fn gamma_lut(gamma: f32) -> Arc<TrcLut> {
    static CACHE: Mutex<Vec<(u32, Arc<TrcLut>)>> = Mutex::new(Vec::new());
    let key = gamma.to_bits();
    let Ok(mut cache) = CACHE.lock() else {
        return Arc::new(TrcLut::from_gamma(gamma));
    };
    if let Some((_, lut)) = cache.iter().find(|(k, _)| *k == key) {
        return Arc::clone(lut);
    }
    let lut = Arc::new(TrcLut::from_gamma(gamma));
    cache.push((key, Arc::clone(&lut)));
    lut
}

/// `rgbBlend(QRgba64, QRgba64, uint)` of one channel: `s * m + d * (1 - m)` with the 8-bit
/// coverage `m` widened to 16 bits.
pub fn blend_linear(d: u16, s: u16, coverage: u8) -> u16 {
    let m = coverage as u32 * 257;
    let mut t = d as u32 * (65535 - m) + s as u32 * m;
    t += t >> 16;
    t += 0x8000;
    (t >> 16) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lut_round_trips_every_channel_value() {
        // Gammas used for LCD text (1.0 to 1.4): every channel round-trips exactly.
        for gamma in [1.0f32, 1.2, 1.4] {
            let lut = TrcLut::from_gamma(gamma);
            for c in 0..=255u8 {
                assert_eq!(lut.from_linear(lut.to_linear(c)), c, "gamma {gamma} channel {c}");
            }
        }
        // Higher gammas (such as the 2.31 used for Windows A8 text) compress the darkest values
        // into few table entries, so the round-trip error reaches 6 for c in 1..=10.
        let lut = TrcLut::from_gamma(2.31);
        for c in 0..=255u8 {
            let got = lut.from_linear(lut.to_linear(c));
            assert!((got as i32 - c as i32).abs() <= 6, "gamma 2.31 channel {c}: {got}");
        }
    }

    #[test]
    fn blend_linear_endpoints_and_midpoint() {
        assert_eq!(blend_linear(1000, 60000, 0), 1000);
        assert_eq!(blend_linear(1000, 60000, 255), 60000);
        let mid = blend_linear(0, 65280, 128) as i32;
        assert!((mid - 65280 * 128 / 255).abs() <= 130, "{mid}");
    }
}
