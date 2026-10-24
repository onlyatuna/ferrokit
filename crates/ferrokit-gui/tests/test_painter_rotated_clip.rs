// SPDX-License-Identifier: LGPL-3.0-only
//! A rect clip under a rotated transform stays a rotated rectangle (a transformed path clip),
//! not its bounding box: `QPainter::setClipRect(QRectF)` goes to `QPaintEngine::clip(QVectorPath)`
//! (`qpainter.cpp:2744-2770`), and `QRasterPaintEngine::clip(const QRect&)` falls back to
//! `QPaintEngineEx::clip` once the matrix is more than a scale (`qpaintengine_raster.cpp:1212`).

use ferrokit_gui::geometry::primitives::RectF;
use ferrokit_gui::paint::{Painter, Pixmap};
use ferrokit_gui::tiny_skia::Color;

fn red() -> Color {
    Color::from_rgba8(255, 0, 0, 255)
}
const SIZE: u32 = 100;

fn red_at(pm: &Pixmap, x: u32, y: u32) -> u8 {
    pm.data()[((y * SIZE + x) * 4) as usize]
}

/// Diamond with half-diagonal ~28.3 centred on (50, 50): the 40x40 rect rotated by 45 degrees.
fn rotate_about_centre(p: &mut Painter) {
    p.translate(50.0, 50.0);
    p.rotate(45.0);
}

#[test]
fn rotated_set_clip_rect_clips_to_the_rotated_rectangle() {
    let mut pm = Pixmap::with_dpr(SIZE, SIZE, 1.0).unwrap();
    {
        let mut p = Painter::begin(&mut pm);
        rotate_about_centre(&mut p);
        p.set_clip_rect(RectF::new(-20.0, -20.0, 40.0, 40.0));
        p.reset_transform();
        p.fill_rect(RectF::new(0.0, 0.0, 100.0, 100.0), red());
    }
    assert_eq!(red_at(&pm, 50, 50), 255, "diamond centre is inside the clip");
    assert_eq!(red_at(&pm, 70, 50), 255, "(20,0) from the centre is inside the diamond");
    assert_eq!(red_at(&pm, 50, 30), 255, "(0,-20) from the centre is inside the diamond");
    // Inside the bounding box (+-28.3) but outside the diamond (|dx|+|dy| = 40 > 28.3).
    assert_eq!(red_at(&pm, 70, 70), 0, "bbox corner must stay clipped by the rotated clip");
    assert_eq!(red_at(&pm, 30, 30), 0, "bbox corner must stay clipped by the rotated clip");
    assert_eq!(red_at(&pm, 70, 30), 0, "bbox corner must stay clipped by the rotated clip");
}

#[test]
fn rotated_clip_to_rect_intersects_with_the_existing_clip() {
    let mut pm = Pixmap::with_dpr(SIZE, SIZE, 1.0).unwrap();
    {
        let mut p = Painter::begin(&mut pm);
        // Axis-aligned clip first: only the top half (y < 50) survives.
        p.set_clip_rect(RectF::new(0.0, 0.0, 100.0, 50.0));
        rotate_about_centre(&mut p);
        p.clip_to_rect(RectF::new(-20.0, -20.0, 40.0, 40.0));
        p.reset_transform();
        p.fill_rect(RectF::new(0.0, 0.0, 100.0, 100.0), red());
    }
    assert_eq!(red_at(&pm, 50, 40), 255, "inside both the rect and the diamond");
    assert_eq!(red_at(&pm, 50, 60), 0, "inside the diamond but below the axis-aligned clip");
    assert_eq!(red_at(&pm, 70, 40), 0, "inside the bbox and the rect but outside the diamond");
}

#[test]
fn two_rotated_clips_intersect_exactly() {
    let mut pm = Pixmap::with_dpr(SIZE, SIZE, 1.0).unwrap();
    {
        let mut p = Painter::begin(&mut pm);
        rotate_about_centre(&mut p);
        p.set_clip_rect(RectF::new(-20.0, -20.0, 40.0, 40.0));
        p.reset_transform();
        // An upright square [50,90]x[50,90] cuts the diamond's left/top corners off.
        p.clip_to_rect(RectF::new(50.0, 50.0, 40.0, 40.0));
        p.fill_rect(RectF::new(0.0, 0.0, 100.0, 100.0), red());
    }
    assert_eq!(red_at(&pm, 60, 60), 255, "inside diamond and upright square");
    assert_eq!(red_at(&pm, 45, 45), 0, "inside the diamond but outside the upright square");
    assert_eq!(red_at(&pm, 75, 75), 0, "inside the upright square but outside the diamond");
}

#[test]
fn changing_clip_clears_the_previous_clip_coverage() {
    let mut pm = Pixmap::with_dpr(SIZE, SIZE, 1.0).unwrap();
    {
        let mut p = Painter::begin(&mut pm);
        p.set_clip_rect(RectF::new(0.0, 0.0, 100.0, 100.0));
        p.fill_rect(RectF::new(0.0, 0.0, 100.0, 100.0), Color::from_rgba8(0, 0, 255, 255));
        p.set_clip_rect(RectF::new(10.0, 10.0, 10.0, 10.0));
        p.fill_rect(RectF::new(0.0, 0.0, 100.0, 100.0), red());
        // A clip replaced by an empty one clips everything, whatever the mask held before.
        p.set_clip_rect(RectF::new(0.0, 0.0, 0.0, 0.0));
        p.fill_rect(RectF::new(0.0, 0.0, 100.0, 100.0), Color::from_rgba8(0, 255, 0, 255));
    }
    let px = |x: u32, y: u32| {
        let i = ((y * SIZE + x) * 4) as usize;
        (pm.data()[i], pm.data()[i + 1], pm.data()[i + 2])
    };
    assert_eq!(px(15, 15), (255, 0, 0), "small clip repainted red");
    assert_eq!(px(50, 50), (0, 0, 255), "outside the small clip keeps the first fill");
    assert_eq!(px(5, 5), (0, 0, 255), "empty clip paints nothing");
}

/// Glyph blits honour the rotated clip too (they bypass the mask otherwise).
#[cfg(windows)]
#[test]
fn text_under_a_rotated_clip_stays_inside_the_diamond() {
    use ferrokit_gui::geometry::primitives::PointF;
    use ferrokit_gui::paint::Pen;
    use ferrokit_gui::text::font::Font;

    let font = Font::new("Segoe UI", 40.0);
    let mut pm = Pixmap::with_dpr(SIZE, SIZE, 1.0).unwrap();
    {
        let mut p = Painter::begin(&mut pm);
        p.set_pen(Some(Pen::new(Color::from_rgba8(255, 255, 255, 255), 1.0)));
        rotate_about_centre(&mut p);
        p.set_clip_rect(RectF::new(-20.0, -20.0, 40.0, 40.0));
        p.reset_transform();
        p.draw_text(PointF::new(0.0, 70.0), "WWWW", &font);
    }
    let mut ink = 0;
    for y in 0..SIZE {
        for x in 0..SIZE {
            let i = ((y * SIZE + x) * 4) as usize;
            let d = pm.data();
            if d[i] | d[i + 1] | d[i + 2] != 0 {
                ink += 1;
                let (dx, dy) = ((x as f32 + 0.5 - 50.0).abs(), (y as f32 + 0.5 - 50.0).abs());
                assert!(
                    dx + dy <= 28.3 + 1.5,
                    "glyph pixel ({x},{y}) lies outside the rotated clip"
                );
            }
        }
    }
    assert!(ink > 0, "some of the text must be visible inside the diamond");
}
