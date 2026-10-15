// SPDX-License-Identifier: LGPL-3.0-only
//! Text and clip under the painter's current transform, against Qt 6 (`qpainter.cpp`):
//! - `drawText` draws transformed glyphs (`QPainter::drawText` under rotate/scale);
//! - `setClipRect(QRectF)` maps the rect through the current matrix and replaces the clip
//!   (`qpainter.cpp:2798-2816`, `qpainter.h:187`).
#![cfg(windows)]

use ferrokit_gui::geometry::primitives::{PointF, RectF};
use ferrokit_gui::paint::{PaintDevice, Painter, Pen, Pixmap};
use ferrokit_gui::text::font::Font;
use ferrokit_gui::tiny_skia::Color;

/// Bounding box `(x0, y0, x1, y1)` of every pixel whose colour channels are not all zero.
fn ink_extent(pm: &Pixmap) -> (u32, u32, u32, u32) {
    let w = pm.width() as u32;
    let data = pm.data();
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
    for (i, px) in data.chunks_exact(4).enumerate() {
        if px[0] | px[1] | px[2] != 0 {
            let x = i as u32 % w;
            let y = i as u32 / w;
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x + 1);
            y1 = y1.max(y + 1);
        }
    }
    (x0, y0, x1, y1)
}

fn ink_size(pm: &Pixmap) -> (u32, u32) {
    let (x0, y0, x1, y1) = ink_extent(pm);
    (x1.saturating_sub(x0), y1.saturating_sub(y0))
}

#[test]
fn rotated_draw_text_rotates_the_glyph() {
    // "I" is a tall, thin stroke upright; rotated 90 degrees it must be wide and flat.
    let font = Font::new("Segoe UI", 40.0);
    let mut pm = Pixmap::with_dpr(160, 160, 1.0).unwrap();
    {
        let mut p = Painter::begin(&mut pm);
        p.set_pen(Some(Pen::new(Color::from_rgba8(255, 255, 255, 255), 1.0)));
        p.translate(60.0, 60.0);
        p.rotate(90.0);
        p.draw_text(PointF::new(0.0, 0.0), "I", &font);
    }
    let (w, h) = ink_size(&pm);
    assert!(
        w > h,
        "rotated 'I' must be wide and flat (glyph rotated, qpainter.cpp drawText under rotation): w={w} h={h}"
    );
}

#[test]
fn scaled_draw_text_scales_the_glyph() {
    let font = Font::new("Segoe UI", 20.0);
    let render = |scale: f32| {
        let mut pm = Pixmap::with_dpr(200, 120, 1.0).unwrap();
        {
            let mut p = Painter::begin(&mut pm);
            p.set_pen(Some(Pen::new(Color::from_rgba8(255, 255, 255, 255), 1.0)));
            p.scale(scale, scale);
            p.draw_text(PointF::new(10.0 / scale, 100.0 / scale), "I", &font);
        }
        ink_size(&pm)
    };
    let (_, h_ref) = render(1.0);
    let (_, h_scaled) = render(2.0);
    assert!(
        h_scaled * 2 >= h_ref * 3,
        "scale(2,2) must enlarge the glyph by ~2x: ref_h={h_ref} scaled_h={h_scaled}"
    );
}

#[test]
fn set_clip_rect_is_mapped_through_the_transform() {
    // Qt: setClipRect maps the rect through the current matrix, so (0,0,10,10) under
    // translate(100,0) clips to x in [100,110).
    let mut pm = Pixmap::with_dpr(200, 20, 1.0).unwrap();
    {
        let mut p = Painter::begin(&mut pm);
        p.translate(100.0, 0.0);
        p.set_clip_rect(RectF::new(0.0, 0.0, 10.0, 10.0));
        p.fill_rect(
            RectF::new(-200.0, -200.0, 400.0, 400.0),
            Color::from_rgba8(255, 0, 0, 255),
        );
    }
    let data = pm.data();
    let red = |x: usize| data[(5 * 200 + x) * 4];
    assert!(
        red(105) > 0,
        "clip (0,0,10,10) under translate(100,0) must cover x=105: red(105)={}",
        red(105)
    );
    assert_eq!(
        red(5),
        0,
        "the clip must not stay at the root origin: red(5)={}",
        red(5)
    );
}
