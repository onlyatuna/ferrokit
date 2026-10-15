// SPDX-License-Identifier: LGPL-3.0-only
//! Blending of glyph coverage into a premultiplied RGBA pixel, after Qt's text blit routines
//! (`qt_alphamapblit_*` for grey-scale masks, `qt_alphargbblit_argb32` for LCD masks).

use crate::text::smoothing::{blend_linear, TrcLut};

/// `qRgbAvg` of `qdrawhelper.cpp`: how an LCD mask is reduced to one coverage where the
/// destination is not opaque.
pub(crate) fn rgb_average(cov: [u8; 3]) -> u8 {
    ((cov[0] as u32 * 5 + cov[1] as u32 * 6 + cov[2] as u32 * 5) / 16) as u8
}

/// Source-over of the text colour `target` at coverage `glyph_alpha` times `base_alpha`.
pub(crate) fn blend_grey(px: &mut [u8], glyph_alpha: u8, base_alpha: f32, target: [u32; 3]) {
    let a_factor = (glyph_alpha as f32 / 255.0) * base_alpha;
    let src_a = (a_factor * 255.0).round() as u32;
    let inv_a = 255 - src_a;
    for (channel, &t) in target.iter().enumerate() {
        let src = (t * src_a) / 255;
        px[channel] = ((src + (px[channel] as u32 * inv_a) / 255).min(255)) as u8;
    }
    px[3] = ((src_a + (px[3] as u32 * inv_a) / 255).min(255)) as u8;
}

/// Rounded `x * a / 255` (`BYTE_MUL`).
fn mul_255(x: u32, a: u32) -> u32 {
    let t = x * a + 0x80;
    (t + (t >> 8)) >> 8
}

/// `alphargbblend_argb32` with a gamma table: LCD text over an opaque pixel is blended per
/// channel in linear light; over a pixel that is not opaque it falls back to the grey blend
/// with the averaged coverage.
pub(crate) fn blend_lcd(px: &mut [u8], cov: [u8; 3], base_alpha: f32, target: [u32; 3], lut: &TrcLut) {
    if cov == [0, 0, 0] {
        return;
    }
    let color_alpha = (base_alpha * 255.0).round() as u32;
    if cov == [255, 255, 255] && color_alpha == 255 {
        px[..3].copy_from_slice(&target.map(|t| t as u8));
        px[3] = 255;
        return;
    }
    if px[3] < 255 {
        blend_grey(px, rgb_average(cov), base_alpha, target);
        return;
    }
    for channel in 0..3 {
        let text = if color_alpha == 255 {
            target[channel]
        } else {
            // Text colour over the destination first (premultiplied source-over).
            mul_255(target[channel], color_alpha) + mul_255(px[channel] as u32, 255 - color_alpha)
        };
        let blended = blend_linear(
            lut.to_linear(px[channel]),
            lut.to_linear(text.min(255) as u8),
            cov[channel],
        );
        px[channel] = lut.from_linear(blended);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lcd_blend_over_transparent_pixel_is_grey_blend_of_averaged_coverage() {
        let lut = TrcLut::from_gamma(1.2);
        let mut lcd = [0u8, 0, 0, 0];
        blend_lcd(&mut lcd, [200, 100, 50], 1.0, [226, 232, 240], &lut);
        let mut grey = [0u8, 0, 0, 0];
        blend_grey(&mut grey, rgb_average([200, 100, 50]), 1.0, [226, 232, 240]);
        assert_eq!(lcd, grey);
    }

    #[test]
    fn lcd_blend_full_coverage_replaces_and_zero_coverage_keeps() {
        let lut = TrcLut::from_gamma(1.2);
        let mut px = [22u8, 25, 32, 255];
        blend_lcd(&mut px, [0, 0, 0], 1.0, [226, 232, 240], &lut);
        assert_eq!(px, [22, 25, 32, 255]);
        blend_lcd(&mut px, [255, 255, 255], 1.0, [226, 232, 240], &lut);
        assert_eq!(px, [226, 232, 240, 255]);
    }
}
