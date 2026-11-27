// SPDX-License-Identifier: LGPL-3.0-only
//! Glyph-mask compositing into premultiplied RGBA surfaces for grey-scale, sub-pixel LCD, and
//! general Porter-Duff blend modes.

use crate::text::smoothing::{TrcLut, blend_linear};
use tiny_skia::{
    BlendMode, Color, FilterQuality, Paint, Pattern, PixmapMut, PixmapRef, Rect, Shader,
    SpreadMode, Transform,
};

/// Source pixel stream for a masked blit.
pub(crate) enum MaskedSource<'a> {
    /// Uniform pen colour (including alpha).
    Solid(Color),
    /// Premultiplied RGBA image (`w * h * 4`) with an overall opacity multiplier.
    Image(&'a [u8], f32),
}

/// Weighted luminance average `(5*R + 6*G + 5*B) / 16` for reducing an RGB sub-pixel mask to
/// scalar alpha coverage over translucent destinations.
pub(crate) fn rgb_average(cov: [u8; 3]) -> u8 {
    ((u32::from(cov[0]) * 5 + u32::from(cov[1]) * 6 + u32::from(cov[2]) * 5) / 16) as u8
}

/// Blinn's 8-bit fixed-point channel scaling `((x * a) + ((x * a) >> 8) + 0x80) >> 8`.
#[inline(always)]
fn mul_255(x: u32, a: u32) -> u32 {
    let t = x * a;
    (t + (t >> 8) + 0x80) >> 8
}

/// Composites a grey-scale glyph coverage value over premultiplied RGBA pixel `px` using
/// SourceOver.
pub(crate) fn blend_grey(px: &mut [u8], glyph_alpha: u8, base_alpha: f32, target: [u32; 3]) {
    let pen_a = ((base_alpha * 255.0).round() as u32).min(255);
    let coverage = u32::from(glyph_alpha);
    let src_a = mul_255(pen_a, coverage);
    let inv_a = 255 - src_a;
    for (channel, &t) in target.iter().enumerate() {
        let src = mul_255(mul_255(t, pen_a), coverage);
        px[channel] = (src + mul_255(u32::from(px[channel]), inv_a)).min(255) as u8;
    }
    px[3] = (src_a + mul_255(u32::from(px[3]), inv_a)).min(255) as u8;
}

/// Composites an RGB sub-pixel LCD mask in linear light over an opaque destination pixel; falls
/// back to [`blend_grey`] with [`rgb_average`] when the destination is translucent.
pub(crate) fn blend_lcd(
    px: &mut [u8],
    cov: [u8; 3],
    base_alpha: f32,
    target: [u32; 3],
    lut: &TrcLut,
) {
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
            mul_255(target[channel], color_alpha)
                + mul_255(u32::from(px[channel]), 255 - color_alpha)
        };
        let blended = blend_linear(
            lut.to_linear(px[channel]),
            lut.to_linear(text.min(255) as u8),
            cov[channel],
        );
        px[channel] = lut.from_linear(blended);
    }
}

/// Composites a grey-scale glyph coverage value in linear light over an opaque destination pixel
/// using `lut`; falls back to [`blend_grey`] when coverage is full or destination is translucent.
pub(crate) fn blend_grey_gamma(
    px: &mut [u8],
    glyph_alpha: u8,
    base_alpha: f32,
    target: [u32; 3],
    lut: &TrcLut,
) {
    if glyph_alpha == 0 {
        return;
    }
    if glyph_alpha == 255 || px[3] < 255 {
        blend_grey(px, glyph_alpha, base_alpha, target);
        return;
    }
    let pen_a = ((base_alpha * 255.0).round() as u32).min(255);
    let source: [u8; 3] = if pen_a == 255 {
        target.map(|t| t.min(255) as u8)
    } else {
        let mut composed = [px[0], px[1], px[2], 255];
        blend_grey(&mut composed, 255, base_alpha, target);
        [composed[0], composed[1], composed[2]]
    };
    for channel in 0..3 {
        let blended = blend_linear(
            lut.to_linear(px[channel]),
            lut.to_linear(source[channel]),
            glyph_alpha,
        );
        px[channel] = lut.from_linear(blended);
    }
}

/// Composites `source` into the `w` x `h` box at `(x0, y0)` of premultiplied RGBA surface `pix`
/// (`pw` x `ph`) with Porter-Duff / blend `mode`, weighted per pixel by `cov` (`w * h`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn composite_masked(
    pix: &mut [u8],
    (pw, ph): (u32, u32),
    (x0, y0): (i32, i32),
    (w, h): (u32, u32),
    cov: &[u8],
    source: MaskedSource<'_>,
    mode: BlendMode,
    scratch: &mut Vec<u8>,
) {
    let ix0 = x0.max(0);
    let iy0 = y0.max(0);
    let ix1 = (x0 as i64 + w as i64).min(pw as i64) as i32;
    let iy1 = (y0 as i64 + h as i64).min(ph as i64) as i32;
    if ix0 >= ix1 || iy0 >= iy1 || cov.len() < w as usize * h as usize {
        return;
    }
    let (sw, sh) = ((ix1 - ix0) as u32, (iy1 - iy0) as u32);
    let row_bytes = sw as usize * 4;
    scratch.clear();
    for row in 0..sh as usize {
        let dst = ((iy0 as usize + row) * pw as usize + ix0 as usize) * 4;
        scratch.extend_from_slice(&pix[dst..dst + row_bytes]);
    }
    let Some(mut canvas) = PixmapMut::from_bytes(scratch, sw, sh) else {
        return;
    };
    let shader = match source {
        MaskedSource::Solid(c) => Shader::SolidColor(c),
        MaskedSource::Image(rgba, opacity) => {
            if (w as usize)
                .checked_mul(h as usize)
                .and_then(|n| n.checked_mul(4))
                != Some(rgba.len())
            {
                return;
            }
            let Some(image) = PixmapRef::from_bytes(rgba, w, h) else {
                return;
            };
            Pattern::new(
                image,
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
        canvas.fill_rect(rect, &paint, Transform::identity(), None);
    }
    let result = canvas.data_mut();
    for row in 0..sh as usize {
        let dst = ((iy0 as usize + row) * pw as usize + ix0 as usize) * 4;
        let cov_row = (iy0 - y0) as usize + row;
        let cov_start = cov_row * w as usize + (ix0 - x0) as usize;
        let result_row = &result[row * row_bytes..(row + 1) * row_bytes];
        let out = &mut pix[dst..dst + row_bytes];
        for ((out, new), &c) in out
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(result_row.as_chunks::<4>().0)
            .zip(&cov[cov_start..cov_start + sw as usize])
        {
            for (old, &new) in out.iter_mut().zip(new) {
                let (o, n, c) = (*old as i32, new as i32, c as i32);
                *old = (o + ((n - o) * c + 127 * (n - o).signum()) / 255) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grey_blend_of_two_overlapping_glyphs_matches_qt() {
        let mut px = [255u8, 255, 255, 255];
        blend_grey(&mut px, 49, 1.0, [30, 120, 200]);
        assert_eq!(px, [212, 229, 244, 255]);
        blend_grey(&mut px, 58, 1.0, [30, 120, 200]);
        assert_eq!(px, [171, 204, 233, 255]);
    }

    #[test]
    fn grey_blend_of_full_and_zero_coverage_is_exact() {
        let mut px = [10u8, 20, 30, 255];
        blend_grey(&mut px, 255, 1.0, [226, 232, 240]);
        assert_eq!(px, [226, 232, 240, 255]);
        blend_grey(&mut px, 0, 1.0, [1, 2, 3]);
        assert_eq!(px, [226, 232, 240, 255]);
    }

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
        let surface = [10u8, 20, 30, 255, 10, 20, 30, 255];
        let run = |mode| {
            let mut px = surface;
            composite_masked(
                &mut px,
                (2, 1),
                (0, 0),
                (2, 1),
                &[255, 0],
                src(),
                mode,
                &mut Vec::new(),
            );
            px
        };
        assert_eq!(run(BlendMode::SourceOver)[..4], [200, 100, 50, 255]);
        assert_eq!(run(BlendMode::Clear)[..4], [0, 0, 0, 0]);
        assert_eq!(run(BlendMode::DestinationOver)[..4], [10, 20, 30, 255]);
        assert_eq!(run(BlendMode::Plus)[..4], [210, 120, 80, 255]);
        assert_eq!(run(BlendMode::Clear)[4..], surface[4..]);
        assert_eq!(run(BlendMode::Source)[4..], surface[4..]);
    }

    #[allow(clippy::too_many_arguments)]
    fn composite_masked_reference(
        pix: &mut [u8],
        (pw, ph): (u32, u32),
        (x0, y0): (i32, i32),
        (w, h): (u32, u32),
        cov: &[u8],
        source: MaskedSource<'_>,
        mode: BlendMode,
    ) {
        use tiny_skia::{IntSize, Pixmap};
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
            mask_data
                .extend_from_slice(&cov[cy * w as usize + cx..cy * w as usize + cx + sw as usize]);
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

    #[test]
    fn composite_masked_matches_the_allocating_reference() {
        use crate::paint::composition::CompositionMode::*;
        let modes = [
            SourceOver,
            DestinationOver,
            Clear,
            Source,
            Destination,
            SourceIn,
            DestinationIn,
            SourceOut,
            DestinationOut,
            SourceAtop,
            DestinationAtop,
            Xor,
            Plus,
            Multiply,
            Screen,
            Overlay,
            Darken,
            Lighten,
            ColorDodge,
            ColorBurn,
            HardLight,
            SoftLight,
            Difference,
            Exclusion,
        ];
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move |bound: u32| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            ((state >> 33) as u32) % bound
        };
        let mut scratch = Vec::new();
        for round in 0..60 {
            let (pw, ph) = [(7u32, 5u32), (1, 1), (16, 9), (3, 12)][next(4) as usize];
            let (w, h) = (1 + next(9), 1 + next(9));
            let (x0, y0) = (next(pw + 8) as i32 - 6, next(ph + 8) as i32 - 6);
            let mut premul = |n: usize| -> Vec<u8> {
                (0..n)
                    .flat_map(|_| {
                        let a = next(256);
                        [
                            next(a + 1) as u8,
                            next(a + 1) as u8,
                            next(a + 1) as u8,
                            a as u8,
                        ]
                    })
                    .collect()
            };
            let surface = premul((pw * ph) as usize);
            let image = premul((w * h) as usize);
            let cov: Vec<u8> = (0..w * h)
                .map(|_| match next(4) {
                    0 => 0,
                    1 => 255,
                    _ => next(256) as u8,
                })
                .collect();
            let color = Color::from_rgba8(
                next(256) as u8,
                next(256) as u8,
                next(256) as u8,
                next(256) as u8,
            );
            let opacity = [1.0, 0.5, 0.0, 0.37][next(4) as usize];
            for mode in modes {
                let sources = || {
                    [
                        MaskedSource::Solid(color),
                        MaskedSource::Image(&image, opacity),
                    ]
                };
                for (index, source) in sources().into_iter().enumerate() {
                    if cfg!(debug_assertions) && index == 0 && matches!(mode, Overlay | HardLight) {
                        continue;
                    }
                    let mut expected = surface.clone();
                    composite_masked_reference(
                        &mut expected,
                        (pw, ph),
                        (x0, y0),
                        (w, h),
                        &cov,
                        source,
                        mode.into(),
                    );
                    let mut actual = surface.clone();
                    composite_masked(
                        &mut actual,
                        (pw, ph),
                        (x0, y0),
                        (w, h),
                        &cov,
                        sources().into_iter().nth(index).unwrap(),
                        mode.into(),
                        &mut scratch,
                    );
                    assert_eq!(
                        actual, expected,
                        "round {round}, {mode:?}, source {index}, surface {pw}x{ph}, box {w}x{h} at ({x0}, {y0})"
                    );
                }
            }
        }
    }

    #[test]
    fn composite_masked_ignores_a_short_coverage_or_a_mis_sized_image() {
        let surface = [9u8; 4 * 4];
        let mut scratch = vec![77u8; 100];
        let mut px = surface;
        let solid = MaskedSource::Solid(Color::from_rgba8(1, 2, 3, 255));
        composite_masked(
            &mut px,
            (2, 2),
            (0, 0),
            (2, 2),
            &[255; 3],
            solid,
            BlendMode::Source,
            &mut scratch,
        );
        assert_eq!(px, surface, "coverage shorter than the box");
        let image = MaskedSource::Image(&[255; 15], 1.0);
        composite_masked(
            &mut px,
            (2, 2),
            (0, 0),
            (2, 2),
            &[255; 4],
            image,
            BlendMode::Source,
            &mut scratch,
        );
        assert_eq!(px, surface, "image shorter than the box");
        let image = MaskedSource::Image(&[255; 17], 1.0);
        composite_masked(
            &mut px,
            (2, 2),
            (0, 0),
            (2, 2),
            &[255; 4],
            image,
            BlendMode::Source,
            &mut scratch,
        );
        assert_eq!(px, surface, "image longer than the box");
    }
}
