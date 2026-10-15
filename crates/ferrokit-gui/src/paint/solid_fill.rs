// SPDX-License-Identifier: LGPL-3.0-only
//! Solid-colour SourceOver fills composited the way `QRasterPaintEngine` does.
//!
//! Qt rasterises a path into coverage spans and blends each span with
//! `blend_color_argb` (qdrawhelper.cpp:4027-4062), which for SourceOver calls
//! `comp_func_solid_SourceOver` (qcompositionfunctions.cpp:697-711):
//! source scaled by the coverage, plus the destination scaled by the inverse of that alpha, or a plain fill
//! where the colour is opaque and the coverage full. tiny-skia's coverage is used as is; only
//! the compositing is Qt's. The premultiplied RGBA layout does not matter to the arithmetic:
//! every channel is treated alike.

use std::cell::RefCell;
use tiny_skia::{FillRule, IntSize, Mask, Path, PixmapMut, PremultipliedColorU8, Transform};

thread_local! {
    /// Coverage scratch, kept between fills: it only grows, so steady painting allocates nothing.
    static COVERAGE: RefCell<Option<Mask>> = const { RefCell::new(None) };
}

/// `BYTE_MUL` (64-bit build, qdrawhelper_p.h:603-608): every channel of the pixel `x` times
/// `a / 255`, with Qt's approximation of the division (not always the nearest integer). The four
/// channels are spread to 16-bit lanes so one multiply does them all.
#[inline(always)]
fn byte_mul(x: u32, a: u32) -> u32 {
    let mut t = ((u64::from(x) | (u64::from(x) << 24)) & 0x00ff_00ff_00ff_00ff) * u64::from(a);
    t = (t + ((t >> 8) & 0x00ff_00ff_00ff_00ff) + 0x0080_0080_0080_0080) >> 8;
    t &= 0x00ff_00ff_00ff_00ff;
    (t as u32) | ((t >> 24) as u32)
}

/// One channel of `BYTE_MUL`; the same arithmetic as the packed form, in a `u16` lane (at most
/// 65407), which the compiler vectorises.
#[inline(always)]
fn byte_mul_channel(x: u16, a: u16) -> u16 {
    let t = x * a;
    (t + (t >> 8) + 0x80) >> 8
}

/// A full-coverage span: `qt_memfill` for an opaque colour, else `comp_func_solid_SourceOver`
/// with the colour as is (`blend_color_argb`, qdrawhelper.cpp:4027-4062).
#[inline]
fn blend_full_span(dest: &mut [u8], color: u32) {
    let bytes = color.to_le_bytes();
    if bytes[3] == 255 {
        dest.as_chunks_mut::<4>().0.fill(bytes);
        return;
    }
    let inv = 255 - u16::from(bytes[3]);
    // Four pixels at a time, the colour repeated per pixel: the same factor for every byte.
    let pattern: [u16; 16] = std::array::from_fn(|i| u16::from(bytes[i % 4]));
    let (chunks, rest) = dest.as_chunks_mut::<16>();
    for chunk in chunks {
        for i in 0..16 {
            chunk[i] = (pattern[i] + byte_mul_channel(u16::from(chunk[i]), inv)) as u8;
        }
    }
    for (i, b) in rest.iter_mut().enumerate() {
        *b = (pattern[i] + byte_mul_channel(u16::from(*b), inv)) as u8;
    }
}

/// `comp_func_solid_SourceOver` over one row: the source scaled by the coverage, plus the
/// destination scaled by the inverse of that alpha. Coverage 0 is no span; runs of full coverage go
/// to [`blend_full_span`], like the long spans Qt's rasteriser produces for a shape's inside.
#[inline]
fn blend_row(dest: &mut [u8], coverage: &[u8], color: u32) {
    let mut i = 0;
    while i < coverage.len() {
        match coverage[i] {
            0 => i += 1,
            255 => {
                let end = coverage[i..]
                    .iter()
                    .position(|&c| c != 255)
                    .map_or(coverage.len(), |n| i + n);
                blend_full_span(&mut dest[i * 4..end * 4], color);
                i = end;
            }
            cov => {
                let px: &mut [u8; 4] = (&mut dest[i * 4..i * 4 + 4]).try_into().unwrap();
                let c = byte_mul(color, u32::from(cov));
                *px = (c + byte_mul(u32::from_le_bytes(*px), 255 - (c >> 24))).to_le_bytes();
                i += 1;
            }
        }
    }
}

/// Fills `path` (in user space, mapped by `transform`) with `color` composited SourceOver onto
/// `pixmap`, within `clip` when given (a device-sized mask, 0 = clipped out).
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
    let color = u32::from_le_bytes([color.red(), color.green(), color.blue(), color.alpha()]);

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
        let Some(mask) = scratch.as_mut() else { return };
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
            // The clip mask is drawn without anti-aliasing, so it holds only 0 and 255.
            if let Some((data, cw)) = clip_data {
                let clip_row = &data[y * cw + x0 as usize..y * cw + x1 as usize];
                for (c, &k) in cov.iter_mut().zip(clip_row) {
                    *c &= k;
                }
            }
            let start = y * pstride + x0 as usize * 4;
            blend_row(&mut dest[start..start + w as usize * 4], cov, color);
        }
    });
}
