// SPDX-License-Identifier: LGPL-3.0-only
//! Text smoothing configuration and power-law gamma transfer lookup tables for linear-light glyph
//! compositing.

use std::sync::{Arc, LazyLock, Mutex, RwLock};

/// Platform text-smoothing configuration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextSmoothing {
    /// Whether sub-pixel LCD text smoothing (ClearType) is active.
    pub cleartype: bool,
    /// Gamma exponent used when blending text coverage in linear light.
    pub gamma: f32,
}

impl TextSmoothing {
    /// Default grey-scale text smoothing without gamma adjustment.
    pub const OFF: Self = Self {
        cleartype: false,
        gamma: 1.0,
    };
}

static CURRENT: RwLock<TextSmoothing> = RwLock::new(TextSmoothing::OFF);

/// Sets the text-smoothing configuration for newly created painters.
pub fn set_text_smoothing(smoothing: TextSmoothing) {
    if let Ok(mut current) = CURRENT.write() {
        *current = smoothing;
    }
}

/// Returns the current text-smoothing configuration.
pub fn text_smoothing() -> TextSmoothing {
    CURRENT.read().map(|s| *s).unwrap_or(TextSmoothing::OFF)
}

/// Power-law gamma transfer lookup table between 8-bit sRGB/encoded channels and 16-bit linear
/// values using 8.8 fixed-point quantization (16 substeps per 8-bit unit).
#[derive(Debug)]
pub struct TrcLut {
    to_linear: [u16; 256],
    from_linear: Vec<u16>,
}

impl TrcLut {
    const LINEAR_STEPS: usize = 255 * 16;

    /// Builds a forward and inverse transfer table for `gamma`.
    pub fn from_gamma(gamma: f32) -> Self {
        let mut to_linear = [0u16; 256];
        for (c, slot) in to_linear.iter_mut().enumerate() {
            let norm = c as f32 / 255.0;
            let q = (norm.powf(gamma) * 65280.0).round().clamp(0.0, 65280.0) as u16;
            *slot = q + (q >> 8);
        }
        let inv_gamma = 1.0 / gamma;
        let from_linear = (0..=Self::LINEAR_STEPS)
            .map(|i| {
                let norm = i as f32 / Self::LINEAR_STEPS as f32;
                (norm.powf(inv_gamma).clamp(0.0, 1.0) * 65280.0).round() as u16
            })
            .collect();
        Self {
            to_linear,
            from_linear,
        }
    }

    /// Converts an 8-bit encoded channel to a 16-bit linear value.
    pub fn to_linear(&self, c: u8) -> u16 {
        self.to_linear[usize::from(c)]
    }

    /// Converts a 16-bit linear value back to an 8-bit encoded channel.
    pub fn from_linear(&self, v: u16) -> u8 {
        let scaled = v - (v >> 8);
        let t = u32::from(self.from_linear[(scaled >> 4) as usize]);
        ((t + 0x80) >> 8).min(255) as u8
    }
}

/// Returns a cached [`TrcLut`] for `gamma`.
pub fn gamma_lut(gamma: f32) -> Arc<TrcLut> {
    static LUT_1_7: LazyLock<Arc<TrcLut>> = LazyLock::new(|| Arc::new(TrcLut::from_gamma(1.7)));
    static CACHE: Mutex<Vec<(u32, Arc<TrcLut>)>> = Mutex::new(Vec::new());
    let key = gamma.to_bits();
    if key == 1.7_f32.to_bits() {
        return Arc::clone(&LUT_1_7);
    }
    let Ok(mut cache) = CACHE.lock() else {
        return Arc::new(TrcLut::from_gamma(gamma));
    };
    if let Some((_, lut)) = cache.iter().find(|(k, _)| *k == key) {
        return Arc::clone(lut);
    }
    let lut = Arc::new(TrcLut::from_gamma(gamma));
    if cache.len() >= 64 {
        cache.clear();
    }
    cache.push((key, Arc::clone(&lut)));
    lut
}

/// Linearly interpolates between `d` and `s` by 8-bit `coverage` (`0..=255`).
pub fn blend_linear(d: u16, s: u16, coverage: u8) -> u16 {
    let m = u32::from(coverage) * 257;
    let t = u32::from(d) * (65535 - m) + u32::from(s) * m;
    ((t + (t >> 16) + 0x8000) >> 16) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lut_round_trips_every_channel_value() {
        for gamma in [1.0f32, 1.2, 1.4] {
            let lut = TrcLut::from_gamma(gamma);
            for c in 0..=255u8 {
                assert_eq!(
                    lut.from_linear(lut.to_linear(c)),
                    c,
                    "gamma {gamma} channel {c}"
                );
            }
        }
        let lut = TrcLut::from_gamma(2.31);
        for c in 0..=255u8 {
            let got = lut.from_linear(lut.to_linear(c));
            assert!(
                (got as i32 - c as i32).abs() <= 6,
                "gamma 2.31 channel {c}: {got}"
            );
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
