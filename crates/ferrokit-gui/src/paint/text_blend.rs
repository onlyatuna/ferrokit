// SPDX-License-Identifier: LGPL-3.0-only
//! Blending of glyph coverage into a premultiplied RGBA pixel, after Qt's text blit routines
//! (`qt_alphamapblit_*` for grey-scale masks, `qt_alphargbblit_argb32` for LCD masks).

use crate::text::smoothing::{blend_linear, TrcLut};
use tiny_skia::{
    BlendMode, Color, FilterQuality, IntSize, Paint, Pattern, Pixmap, Rect, Shader,
    SpreadMode, Transform,
};

/// What a masked blit composites onto the destination.
pub(crate) enum MaskedSource<'a> {
    /// One colour (alpha included), as for a glyph drawn with the pen.
    Solid(Color),
    /// Premultiplied RGBA pixels (`w * h * 4`) with a whole-image opacity, as for colour glyphs.
    Image(&'a [u8], f32),
}

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

/// Composites `source` into the `w` x `h` box at `(x0, y0)` of the premultiplied RGBA surface `pix`
/// (`pw` x `ph`) with the Porter-Duff / blend `mode`, weighted per pixel by `cov` (`w * h`): where
/// the coverage is partial the result is the destination lerped towards the composited pixel, as
/// Qt's mask blits do. Covers every mode, unlike [`blend_grey`]/[`blend_lcd`] (source-over only).
#[allow(clippy::too_many_arguments)]
pub(crate) fn composite_masked(
    pix: &mut [u8],
    (pw, ph): (u32, u32),
    (x0, y0): (i32, i32),
    (w, h): (u32, u32),
    cov: &[u8],
    source: MaskedSource<'_>,
    mode: BlendMode,
) {
    let ix0 = x0.max(0);
    let iy0 = y0.max(0);
    let ix1 = (x0 as i64 + w as i64).min(pw as i64) as i32;
    let iy1 = (y0 as i64 + h as i64).min(ph as i64) as i32;
    if ix0 >= ix1 || iy0 >= iy1 || cov.len() < (w * h) as usize {
        return;
    }
    let (sw, sh) = ((ix1 - ix0) as u32, (iy1 - iy0) as u32);
    let Some(mut scratch) = Pixmap::new(sw, sh) else {
        return;
    };
    let mut mask_data = Vec::with_capacity((sw * sh) as usize);
    let mut before = Vec::with_capacity((sw * sh * 4) as usize);
    for row in 0..sh as usize {
        let dy = iy0 as usize + row;
        let dst = (dy * pw as usize + ix0 as usize) * 4;
        scratch.data_mut()[row * sw as usize * 4..(row + 1) * sw as usize * 4]
            .copy_from_slice(&pix[dst..dst + sw as usize * 4]);
        before.extend_from_slice(&pix[dst..dst + sw as usize * 4]);
        let cy = (iy0 - y0) as usize + row;
        let cx = (ix0 - x0) as usize;
        mask_data.extend_from_slice(&cov[cy * w as usize + cx..cy * w as usize + cx + sw as usize]);
    }
    let image;
    let shader = match source {
        MaskedSource::Solid(c) => Shader::SolidColor(c),
        MaskedSource::Image(rgba, opacity) => {
            let Some(size) = IntSize::from_wh(w, h) else {
                return;
            };
            let Some(img) = Pixmap::from_vec(rgba.to_vec(), size) else {
                return;
            };
            image = img;
            Pattern::new(
                image.as_ref(),
                SpreadMode::Pad,
                FilterQuality::Nearest,
                opacity,
                Transform::from_translate((x0 - ix0) as f32, (y0 - iy0) as f32),
            )
        }
    };
    let paint = Paint {
        shader,
        blend_mode: mode,
        anti_alias: false,
        ..Default::default()
    };
    if let Some(rect) = Rect::from_xywh(0.0, 0.0, sw as f32, sh as f32) {
        scratch.fill_rect(rect, &paint, Transform::identity(), None);
    }
    // The composited pixel is applied at the coverage: dst + (result - dst) * cov / 255.
    let result = scratch.data();
    for row in 0..sh as usize {
        let dst = ((iy0 as usize + row) * pw as usize + ix0 as usize) * 4;
        for i in 0..sw as usize * 4 {
            let old = before[row * sw as usize * 4 + i] as i32;
            let new = result[row * sw as usize * 4 + i] as i32;
            let c = mask_data[row * sw as usize + i / 4] as i32;
            pix[dst + i] = (old + ((new - old) * c + 127 * (new - old).signum()) / 255) as u8;
        }
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

    #[test]
    fn composite_masked_applies_the_blend_mode_weighted_by_coverage() {
        let src = || MaskedSource::Solid(Color::from_rgba8(200, 100, 50, 255));
        // 2x1 surface of opaque (10, 20, 30); coverage 255 then 0.
        let surface = [10u8, 20, 30, 255, 10, 20, 30, 255];
        let run = |mode| {
            let mut px = surface;
            composite_masked(&mut px, (2, 1), (0, 0), (2, 1), &[255, 0], src(), mode);
            px
        };
        assert_eq!(run(BlendMode::SourceOver)[..4], [200, 100, 50, 255]);
        assert_eq!(run(BlendMode::Clear)[..4], [0, 0, 0, 0]);
        assert_eq!(run(BlendMode::DestinationOver)[..4], [10, 20, 30, 255]);
        assert_eq!(run(BlendMode::Plus)[..4], [210, 120, 80, 255]);
        // Zero coverage leaves the destination alone in every mode.
        assert_eq!(run(BlendMode::Clear)[4..], surface[4..]);
        assert_eq!(run(BlendMode::Source)[4..], surface[4..]);
    }
}
