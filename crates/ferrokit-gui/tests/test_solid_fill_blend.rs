// SPDX-License-Identifier: LGPL-3.0-only
//! A solid-colour fill composites like Qt's raster engine.
//!
//! `QRasterPaintEngine` fills a solid brush span by span (`blend_color_argb`,
//! gui/painting/qdrawhelper.cpp:4027-4062); SourceOver of a translucent colour is
//! `comp_func_solid_SourceOver` (gui/painting/qcompositionfunctions.cpp:697-711):
//! `dest = color + BYTE_MUL(dest, 255 - alpha(color))`, where `BYTE_MUL`
//! (gui/painting/qdrawhelper_p.h:623-633) is Qt's own approximation of `x * a / 255`.

use ferrokit_gui::geometry::primitives::RectF;
use ferrokit_gui::paint::{Brush, Painter, Pixmap};
use ferrokit_gui::tiny_skia::Color;

/// `BYTE_MUL` for one channel.
fn byte_mul(x: u32, a: u32) -> u32 {
    let t = x * a;
    (t + (t >> 8) + 0x80) >> 8
}

/// A 256 x 3 pixmap whose column `v` is the premultiplied grey `(v, v, v, v)`.
fn grey_ramp() -> Pixmap {
    let mut pm = Pixmap::new(256, 3).unwrap();
    for (i, px) in pm.data_mut().as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let v = (i % 256) as u8;
        px.copy_from_slice(&[v, v, v, v]);
    }
    pm
}

fn fill_whole(pm: &mut Pixmap, color: Color, clip: bool) {
    let mut p = Painter::begin(pm);
    if clip {
        p.set_clip_rect(RectF::new(0.0, 0.0, 256.0, 3.0));
    }
    p.set_brush(Brush::Color(color));
    p.set_pen(None);
    p.draw_rect(RectF::new(0.0, 0.0, 256.0, 3.0));
}

fn assert_source_over(pm: &Pixmap, src: [u32; 4]) {
    let inv = 255 - src[3];
    for v in 0..256u32 {
        let px = pm.pixel(v, 1).unwrap();
        let got = [px.red(), px.green(), px.blue(), px.alpha()].map(u32::from);
        let want = src.map(|c| c + byte_mul(v, inv));
        assert_eq!(got, want, "destination grey {v}");
    }
}

#[test]
fn test_a_translucent_solid_fill_rounds_like_qt_source_over() {
    // Black at alpha 230: premultiplied (0, 0, 0, 230), so dest * 25 / 255 is what is checked.
    let mut pm = grey_ramp();
    fill_whole(&mut pm, Color::from_rgba8(0, 0, 0, 230), false);
    assert_source_over(&pm, [0, 0, 0, 230]);

    let mut pm = grey_ramp();
    fill_whole(&mut pm, Color::from_rgba8(0, 0, 0, 128), true);
    assert_source_over(&pm, [0, 0, 0, 128]);
}

#[test]
fn test_an_opaque_solid_fill_replaces_the_destination() {
    let mut pm = grey_ramp();
    fill_whole(&mut pm, Color::from_rgba8(10, 200, 30, 255), true);
    for v in 0..256u32 {
        let px = pm.pixel(v, 1).unwrap();
        assert_eq!(
            [px.red(), px.green(), px.blue(), px.alpha()],
            [10, 200, 30, 255]
        );
    }
}
