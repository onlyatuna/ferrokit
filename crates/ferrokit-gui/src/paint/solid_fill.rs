// SPDX-License-Identifier: LGPL-3.0-only
//! Solid-colour SourceOver path fills composited onto premultiplied RGBA surfaces using coverage
//! masks from `tiny-skia`.

use std::cell::RefCell;
use tiny_skia::{FillRule, IntSize, Mask, Path, PixmapMut, PremultipliedColorU8, Transform};

thread_local! {
    /// Reusable coverage mask scratch buffer to avoid per-fill allocations.
    static COVERAGE: RefCell<Option<Mask>> = const { RefCell::new(None) };
}

/// Blinn's 8-bit fixed-point channel product `((x * a) + ((x * a) >> 8) + 0x80) >> 8`.
#[inline(always)]
fn scale_u8(x: u8, a: u8) -> u8 {
    let t = u16::from(x) * u16::from(a);
    ((t + (t >> 8) + 0x80) >> 8) as u8
}

/// Composites a full-coverage solid `src` pixel `[r, g, b, a]` onto `dest`.
#[inline]
fn blend_full_span(dest: &mut [u8], src: [u8; 4]) {
    if src[3] == 255 {
        dest.as_chunks_mut::<4>().0.fill(src);
        return;
    }
    let inv = 255 - src[3];
    for px in dest.as_chunks_mut::<4>().0 {
        for c in 0..4 {
            px[c] = src[c] + scale_u8(px[c], inv);
        }
    }
}

/// Composites a single coverage row onto premultiplied RGBA `dest`.
#[inline]
fn blend_row(dest: &mut [u8], coverage: &[u8], src: [u8; 4]) {
    let mut i = 0;
    while i < coverage.len() {
        match coverage[i] {
            0 => i += 1,
            255 => {
                let end = coverage[i..]
                    .iter()
                    .position(|&c| c != 255)
                    .map_or(coverage.len(), |n| i + n);
                blend_full_span(&mut dest[i * 4..end * 4], src);
                i = end;
            }
            cov => {
                let px: &mut [u8; 4] = (&mut dest[i * 4..i * 4 + 4]).try_into().unwrap();
                let sa = scale_u8(src[3], cov);
                let inv = 255 - sa;
                for c in 0..4 {
                    px[c] = scale_u8(src[c], cov) + scale_u8(px[c], inv);
                }
                i += 1;
            }
        }
    }
}

/// Fills `path` (in user space, mapped by `transform`) with `color` composited SourceOver onto
/// `pixmap`, optionally masked by a device-sized `clip` mask (`0` = clipped out).
pub fn fill_path_source_over(
    pixmap: &mut PixmapMut,
    path: &Path,
    fill_rule: FillRule,
    anti_alias: bool,
    transform: Transform,
    color: PremultipliedColorU8,
    clip: Option<&Mask>,
) {
    let Some(device_path) = path.clone().transform(transform) else {
        return;
    };
    let b = device_path.bounds();
    let (pw, ph) = (pixmap.width() as i32, pixmap.height() as i32);
    let x0 = (b.left().floor() as i32).max(0);
    let y0 = (b.top().floor() as i32).max(0);
    let x1 = (b.right().ceil() as i32).min(pw);
    let y1 = (b.bottom().ceil() as i32).min(ph);
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let (w, h) = ((x1 - x0) as u32, (y1 - y0) as u32);
    let Some(local_path) = device_path.transform(Transform::from_translate(-x0 as f32, -y0 as f32))
    else {
        return;
    };
    let src = [color.red(), color.green(), color.blue(), color.alpha()];

    COVERAGE.with(|scratch| {
        let mut scratch = scratch.borrow_mut();
        let fits = scratch
            .as_ref()
            .is_some_and(|m| m.width() >= w && m.height() >= h);
        if !fits {
            let (sw, sh) = scratch.as_ref().map_or((0, 0), |m| (m.width(), m.height()));
            let size = IntSize::from_wh(sw.max(w), sh.max(h));
            *scratch = size.and_then(|s| Mask::new(s.width(), s.height()));
        }
        let Some(mask) = scratch.as_mut() else {
            return;
        };
        let stride = mask.width() as usize;
        for row in mask.data_mut().chunks_exact_mut(stride).take(h as usize) {
            row[..w as usize].fill(0);
        }
        mask.fill_path(&local_path, fill_rule, anti_alias, Transform::identity());
        let clip_data = clip.map(|m| (m.data(), m.width() as usize));
        let coverage = mask.data_mut();
        let dest = pixmap.data_mut();
        let pstride = pw as usize * 4;
        for row in 0..h as usize {
            let y = y0 as usize + row;
            let cov = &mut coverage[row * stride..row * stride + w as usize];
            if let Some((data, cw)) = clip_data {
                let clip_row = &data[y * cw + x0 as usize..y * cw + x1 as usize];
                apply_clip_mask(cov, clip_row);
            }
            let start = y * pstride + x0 as usize * 4;
            blend_row(&mut dest[start..start + w as usize * 4], cov, src);
        }
    });
}

#[inline]
fn apply_clip_mask(cov: &mut [u8], clip_row: &[u8]) {
    for (c, &k) in cov.iter_mut().zip(clip_row) {
        *c = ((u16::from(*c) * u16::from(k) + 127) / 255) as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiny_skia::{PathBuilder, Pixmap, Rect};

    #[test]
    fn apply_clip_mask_multiplies_antialiased_coverage() {
        let mut cov = [128u8, 255, 255, 255];
        let clip_row = [127u8, 127, 0, 255];
        apply_clip_mask(&mut cov, &clip_row);
        assert_eq!(cov[0], 64);
        assert_eq!(cov[1], 127);
        assert_eq!(cov[2], 0);
        assert_eq!(cov[3], 255);
    }

    // Exercising tiny-skia's rasterizer under Miri hits issue #105 (tiny-skia compares fn
    // pointers to pick tail-stage load/store, which zeroes tail pixels on some Miri seeds).
    #[cfg(not(miri))]
    #[test]
    fn fill_path_source_over_multiplies_antialiased_clip_mask_coverage() {
        let mut pixmap = Pixmap::new(4, 2).unwrap();
        let mut mask = Mask::new(4, 2).unwrap();
        let m = mask.data_mut();
        m[0] = 127;
        m[1] = 127;
        m[2] = 0;
        m[3] = 255;

        let path = PathBuilder::from_rect(Rect::from_xywh(0.5, 0.0, 3.5, 1.0).unwrap());
        let white = PremultipliedColorU8::from_rgba(255, 255, 255, 255).unwrap();
        fill_path_source_over(
            &mut pixmap.as_mut(),
            &path,
            FillRule::Winding,
            true,
            Transform::identity(),
            white,
            Some(&mask),
        );

        let px = pixmap.data();
        assert!(
            (60..=68).contains(&px[3]),
            "expected ~64 alpha at partially covered & partially clipped pixel, got {}",
            px[3]
        );
        assert_eq!(px[7], 127, "full path coverage * 127 clip mask");
        assert_eq!(px[11], 0, "clipped-out pixel must remain transparent");
        assert_eq!(px[15], 255, "unclipped full-coverage pixel must be opaque");
    }
}
