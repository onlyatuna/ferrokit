// SPDX-License-Identifier: LGPL-3.0-only
use super::*;

#[test]
fn test_pen_creation_and_customization() {
    let pen = Pen::new(Color::from_rgba8(255, 0, 0, 255), 2.5);
    assert_eq!(pen.width, 2.5);
    assert_eq!(pen.cap, LineCap::Square);
    assert_eq!(pen.join, LineJoin::Bevel);
    assert!(pen.dash_pattern.is_none());

    let customized = pen
        .with_cap(LineCap::Square)
        .with_join(LineJoin::Miter)
        .with_dash_pattern(vec![5.0, 5.0]);

    assert_eq!(customized.cap, LineCap::Square);
    assert_eq!(customized.join, LineJoin::Miter);
    assert_eq!(customized.dash_pattern, Some(vec![5.0, 5.0]));

    let stroke = customized.to_stroke();
    assert_eq!(stroke.width, 2.5);
    assert_eq!(stroke.line_cap, LineCap::Square);
    assert_eq!(stroke.line_join, LineJoin::Miter);
    assert!(stroke.dash.is_some());
}

#[test]
fn gradient_fills_honor_painter_opacity() {
    use crate::paint::brush::{LinearGradient, RadialGradient};
    let red = Color::from_rgba8(255, 0, 0, 255);
    let mut lin = LinearGradient::new(PointF::new(0.0, 0.0), PointF::new(20.0, 0.0));
    lin.add_stop(0.0, red);
    lin.add_stop(1.0, red);
    let mut rad = RadialGradient::new(PointF::new(10.0, 10.0), 30.0);
    rad.add_stop(0.0, red);
    rad.add_stop(1.0, red);
    for brush in [Brush::linear_gradient(lin), Brush::radial_gradient(rad)] {
        let mut pm = crate::paint::pixmap::Pixmap::new(20, 20).unwrap();
        {
            let mut p = Painter::begin(&mut pm);
            p.set_pen(None);
            p.set_brush(brush);
            p.set_opacity(0.5);
            p.draw_rect(RectF::new(0.0, 0.0, 20.0, 20.0));
        }
        let a = pm.pixel(10, 10).unwrap().alpha();
        assert!((126..=129).contains(&a), "alpha {a}");
    }
}

#[test]
fn test_brush_variants_and_default() {
    assert_eq!(Brush::default(), Brush::NoBrush);

    let solid = Brush::from_rgba8(10, 20, 30, 255);
    assert_eq!(solid, Brush::Color(Color::from_rgba8(10, 20, 30, 255)));

    let hatched = Brush::hatched(Color::from_rgba8(255, 255, 0, 180));
    assert_eq!(
        hatched,
        Brush::Hatched {
            color: Color::from_rgba8(255, 255, 0, 180)
        }
    );
}

#[test]
fn test_painter_state_default_and_clone() {
    let state = PainterState::default();
    assert!(state.pen.is_some());
    assert_eq!(state.brush, Brush::NoBrush);
    assert_eq!(state.opacity, 1.0);
    // Qt `QPainter::begin` leaves only TextAntialiasing set (`qpainter.cpp:1847`).
    assert_eq!(state.render_hints, RenderHints::DEFAULT);
    assert!(!state.render_hints.contains(RenderHint::Antialiasing));
    assert!(state.clip_rect.is_none());
    assert_eq!(state.transform, tiny_skia::Transform::identity());

    let mut cloned = state.clone();
    cloned.opacity = 0.5;
    cloned.pen = None;
    cloned.brush = Brush::from_rgba8(255, 0, 0, 255);
    assert_ne!(state, cloned);
    assert_eq!(cloned.opacity, 0.5);
    assert!(cloned.pen.is_none());
}

#[test]
fn test_painter_begin_dpr_transform() {
    use crate::paint::pixmap::Pixmap;

    // DPR 1.0
    let mut pm1 = Pixmap::new(100, 100).unwrap();
    let painter1 = Painter::begin(&mut pm1);
    assert_eq!(painter1.state().transform, tiny_skia::Transform::identity());

    // DPR 2.0
    let mut pm2 = Pixmap::with_dpr(200, 100, 2.0).unwrap();
    let painter2 = Painter::begin(&mut pm2);
    assert_eq!(
        painter2.state().transform,
        tiny_skia::Transform::from_scale(2.0, 2.0)
    );
}

#[test]
fn test_painter_transforms_and_state_stack() {
    let mut pm = Pixmap::new(100, 100).unwrap();
    let mut p = Painter::begin(&mut pm);

    p.set_opacity(0.8);
    p.translate(10.0, 20.0);
    p.save();

    p.scale(2.0, 2.0);
    p.set_opacity(0.4);
    assert_eq!(p.state().opacity, 0.4);

    p.restore();
    assert_eq!(p.state().opacity, 0.8);

    // Returns a mutable reference to the active state.
}

#[test]
fn test_painter_draw_primitives_headless() {
    let mut pm = Pixmap::new(200, 200).unwrap();
    let mut p = Painter::begin(&mut pm);

    // Line
    p.set_pen(Pen::from_rgba8(255, 0, 0, 255, 2.0));
    p.draw_line(PointF::new(0.0, 0.0), PointF::new(50.0, 50.0));

    // Rect
    p.set_brush(Brush::from_rgba8(0, 255, 0, 255));
    p.draw_rect(RectF::new(10.0, 10.0, 40.0, 40.0));

    // Rounded rect
    p.draw_rounded_rect(RectF::new(60.0, 10.0, 40.0, 40.0), 5.0, 5.0);

    // Ellipse
    p.draw_ellipse(RectF::new(10.0, 60.0, 40.0, 40.0));

    let points = [
        PointF::new(0.0, 0.0),
        PointF::new(10.0, 20.0),
        PointF::new(20.0, 10.0),
    ];
    p.draw_polyline(&points);

    assert!(pm.data().iter().any(|&b| b > 0));
}

#[test]
fn test_painter_draw_arc_and_pie() {
    let mut pm = Pixmap::new(200, 200).unwrap();
    let mut p = Painter::begin(&mut pm);

    p.set_pen(Pen::from_rgba8(255, 255, 0, 255, 3.0));
    p.set_brush(Brush::from_rgba8(0, 0, 255, 128));

    // Arc (outer ring of a gauge)
    p.draw_arc(RectF::new(20.0, 20.0, 100.0, 100.0), 90.0, -180.0);

    p.draw_pie(RectF::new(20.0, 20.0, 100.0, 100.0), 0.0, 90.0);

    assert!(pm.data().iter().any(|&b| b > 0));
}

#[test]
fn test_painter_draw_pixmap_and_clip() {
    let mut dest = Pixmap::new(100, 100).unwrap();
    let mut src = Pixmap::new(50, 50).unwrap();
    src.fill(Color::from_rgba8(255, 0, 0, 255));

    let mut p = Painter::begin(&mut dest);
    p.set_clip_rect(RectF::new(10.0, 10.0, 80.0, 80.0));
    p.draw_pixmap(RectF::new(0.0, 0.0, 50.0, 50.0), &src, None);

    assert!(dest.data().iter().any(|&b| b > 0));
}

#[test]
fn clip_change_reuses_the_mask_buffer() {
    let mut dest = Pixmap::new(64, 64).unwrap();
    let mut p = Painter::begin(&mut dest);
    let fill = |p: &mut Painter| p.fill_rect(RectF::new(0.0, 0.0, 64.0, 64.0), Color::WHITE);
    p.set_clip_rect(RectF::new(0.0, 0.0, 32.0, 32.0));
    fill(&mut p);
    let first = p.cached_mask.as_ref().unwrap().data().as_ptr();
    p.set_clip_rect(RectF::new(16.0, 16.0, 32.0, 32.0));
    fill(&mut p);
    let second = p.cached_mask.as_ref().unwrap().data().as_ptr();
    assert_eq!(
        first, second,
        "a clip change must reuse the device-sized mask"
    );
    // Dropping the clip and setting another still reuses the same buffer.
    p.state_mut().clip_rect = None;
    fill(&mut p);
    assert!(p.cached_mask.is_none());
    p.set_clip_rect(RectF::new(8.0, 8.0, 8.0, 8.0));
    fill(&mut p);
    assert_eq!(first, p.cached_mask.as_ref().unwrap().data().as_ptr());
    // Only the new clip's pixels are non-zero: the buffer was cleared where it was dirty.
    let mask = p.cached_mask.as_ref().unwrap();
    let covered = mask.data().iter().filter(|&&a| a != 0).count();
    assert_eq!(covered, 8 * 8);
}

// -------------------------------------------------------------------------
// Returns a mutable reference to the active state.
// -------------------------------------------------------------------------

#[test]
fn test_painter_draw_rect_and_fill() {
    let mut surface = Pixmap::new(100, 100).unwrap();
    // Fill with white
    surface.fill(tiny_skia::Color::WHITE);

    {
        let mut painter = Painter::begin(&mut surface);
        painter.set_pen(None);
        painter.set_brush(Brush::Color(tiny_skia::Color::from_rgba8(255, 0, 0, 255)));

        painter.draw_rect(RectF::new(40.0, 40.0, 20.0, 20.0));
    }

    let data = surface.data();

    let get_pixel = |x: usize, y: usize| -> (u8, u8, u8, u8) {
        let idx = (y * 100 + x) * 4;
        (data[idx], data[idx + 1], data[idx + 2], data[idx + 3])
    };

    assert_eq!(get_pixel(10, 10), (255, 255, 255, 255));

    assert_eq!(get_pixel(50, 50), (255, 0, 0, 255));
}

// -------------------------------------------------------------------------

// -------------------------------------------------------------------------

#[test]
fn test_render_dial_to_png() {
    let mut surface = Pixmap::new(300, 300).unwrap();
    surface.fill(tiny_skia::Color::TRANSPARENT);

    {
        let mut painter = Painter::begin(&mut surface);

        // Returns a mutable reference to the active state.
        let bounds = RectF::new(20.0, 20.0, 260.0, 260.0);
        painter.set_pen(Pen::new(
            tiny_skia::Color::from_rgba8(40, 40, 40, 255),
            14.0,
        ));
        painter.set_brush(Brush::NoBrush);
        painter.draw_ellipse(bounds);

        painter.set_pen(Pen::new(
            tiny_skia::Color::from_rgba8(50, 205, 50, 255),
            14.0,
        ));
        painter.draw_arc(bounds, 90.0, -270.0);

        let inner_bounds = RectF::new(60.0, 60.0, 180.0, 180.0);
        painter.set_pen(None);
        painter.set_brush(Brush::Color(tiny_skia::Color::from_rgba8(255, 140, 0, 200)));
        painter.draw_pie(inner_bounds, 90.0, -120.0);

        let font = Font::new("Arial", 18.0)
            .with_weight(crate::text::font::FontWeight::Bold)
            .with_feature(crate::text::font::FontTag::TNUM, 1);
        painter.set_pen(Pen::from_rgba8(255, 255, 255, 255, 1.0));
        painter.draw_text(PointF::new(105.0, 155.0), "75%", &font);
    }

    // Save PNG
    let out_dir = std::path::Path::new("target");
    if !out_dir.exists() {
        let _ = std::fs::create_dir_all(out_dir);
    }
    let output_path = out_dir.join("test_dial.png");
    surface.save_png(&output_path).expect("failed to save PNG");
    assert!(output_path.exists());
}

#[test]
fn test_painter_draw_text_headless_probe() {
    let mut surface = Pixmap::new(100, 100).unwrap();
    surface.fill(tiny_skia::Color::TRANSPARENT);

    {
        let mut painter = Painter::begin(&mut surface);
        painter.set_pen(Pen::from_rgba8(0, 255, 0, 255, 1.0));
        let font = Font::new("Arial", 16.0);
        painter.draw_text(PointF::new(20.0, 50.0), "HUD", &font);
    }

    // Verify rendered text contains green pixels
    let data = surface.data();
    let has_green = data
        .as_chunks::<4>()
        .0
        .iter()
        .any(|p| p[1] > 50 && p[3] > 0);
    assert!(has_green, "Canvas must contain rendered green text pixels");
}

// Qt's fresh QPainter has renderHints()==TextAntialiasing only, so shapes are aliased.
// QPainter probe (PySide6 6.11.2): 30.3px ellipse with no hint has 0 partial-alpha pixels;
// with Antialiasing it has 114.
#[test]
fn g10_4d_default_shapes_are_aliased_like_qt() {
    let mut pm = Pixmap::new(40, 40).unwrap();
    pm.fill(tiny_skia::Color::TRANSPARENT);
    {
        let mut p = Painter::begin(&mut pm);
        p.set_pen(None);
        p.set_brush(Brush::from_rgba8(255, 0, 0, 255));
        p.draw_ellipse(RectF::new(2.5, 2.5, 30.3, 30.3));
    }
    let partial = pm
        .data()
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|px| px[3] > 0 && px[3] < 255)
        .count();
    assert_eq!(
        partial, 0,
        "fresh QPainter has no Antialiasing hint (Qt renderHints()==TextAntialiasing only): \
         ellipse edge must be aliased, found {partial} partial-alpha pixels"
    );
}

// draw_pixmap samples nearest-neighbour unless SmoothPixmapTransform is set (Qt
// default). Source: left half red, right half blue, 4x4 px. Target: 10x10 px, scale 2.5.
// Nearest yields only pure red or pure blue opaque pixels; bilinear mixes at the seam.
#[test]
fn g10_4d_draw_pixmap_is_nearest_without_smooth_pixmap_transform() {
    let mut src = Pixmap::new(4, 4).unwrap();
    {
        let mut sp = Painter::begin(&mut src);
        sp.set_pen(None);
        sp.set_brush(Brush::from_rgba8(255, 0, 0, 255));
        sp.draw_rect(RectF::new(0.0, 0.0, 2.0, 4.0));
        sp.set_brush(Brush::from_rgba8(0, 0, 255, 255));
        sp.draw_rect(RectF::new(2.0, 0.0, 2.0, 4.0));
    }
    let mut dest = Pixmap::new(10, 10).unwrap();
    {
        let mut p = Painter::begin(&mut dest);
        p.draw_pixmap(RectF::new(0.0, 0.0, 10.0, 10.0), &src, None);
    }
    let stride = 10 * 4;
    for y in 0..10usize {
        for x in 0..10usize {
            let i = y * stride + x * 4;
            let px = &dest.data()[i..i + 4];
            let (r, b) = (px[0], px[2]);
            assert_eq!(px[3], 255, "pixel ({x},{y}) must be opaque");
            let pure_red = r == 255 && b == 0;
            let pure_blue = r == 0 && b == 255;
            assert!(
                pure_red || pure_blue,
                "pixel ({x},{y}) is a mixed colour rgba=({},{},{},{}); Qt nearest sampling gives pure colours",
                px[0],
                px[1],
                px[2],
                px[3]
            );
        }
    }
}

// An aliased cosmetic line with a non-flat cap covers both end pixels. Qt's default
// pen is SquareCap (qpen.cpp:203); QCosmeticStroker::drawLine adds caps unless FlatCap
// (qcosmeticstroker.cpp:263, CapBegin|CapEnd at :356). PySide6 6.11.2 probe, 1px black on
// white, Antialiasing off, same result for Square and Round: H (8,36)-(258,36) is black at
// x=8 and x=258 and white at x=7 and x=259; V (36,8)-(36,258) and D (8,8)-(258,158) likewise.
// FlatCap: the far end pixel (258) stays white.
#[test]
fn g10_4d_aliased_line_includes_endpoint_pixels_like_qt() {
    const W: u32 = 300;
    let red = |pm: &Pixmap, x: usize, y: usize| pm.data()[(y * W as usize + x) * 4];
    let draw = |p1: (f32, f32), p2: (f32, f32), cap: LineCap| {
        let mut pm = Pixmap::new(W, W).unwrap();
        pm.fill(tiny_skia::Color::WHITE);
        {
            let mut p = Painter::begin(&mut pm);
            p.set_pen(Pen::new(tiny_skia::Color::BLACK, 1.0).with_cap(cap));
            p.draw_line(PointF::new(p1.0, p1.1), PointF::new(p2.0, p2.1));
        }
        pm
    };
    // (start, end, pixel before start along the line, pixel after end along the line)
    let lines = [
        (
            (8.0, 36.0),
            (258.0, 36.0),
            (8, 36),
            (258, 36),
            (7, 36),
            (259, 36),
        ),
        (
            (36.0, 8.0),
            (36.0, 258.0),
            (36, 8),
            (36, 258),
            (36, 7),
            (36, 259),
        ),
        (
            (8.0, 8.0),
            (258.0, 158.0),
            (8, 8),
            (258, 158),
            (7, 7),
            (259, 159),
        ),
    ];
    for cap in [LineCap::Square, LineCap::Round] {
        for (p1, p2, start, end, before, after) in lines {
            let pm = draw(p1, p2, cap);
            let msg = format!("{cap:?} {p1:?}->{p2:?}");
            assert_eq!(
                red(&pm, start.0, start.1),
                0,
                "{msg}: start pixel must be drawn"
            );
            assert_eq!(red(&pm, end.0, end.1), 0, "{msg}: end pixel must be drawn");
            assert_eq!(
                red(&pm, before.0, before.1),
                255,
                "{msg}: pixel before start must stay white"
            );
            assert_eq!(
                red(&pm, after.0, after.1),
                255,
                "{msg}: pixel after end must stay white"
            );
        }
    }
    for (p1, p2, start, end, _, _) in lines {
        let pm = draw(p1, p2, LineCap::Butt);
        let msg = format!("FlatCap {p1:?}->{p2:?}");
        assert_eq!(
            red(&pm, start.0, start.1),
            0,
            "{msg}: start pixel must be drawn"
        );
        assert_eq!(
            red(&pm, end.0, end.1),
            255,
            "{msg}: FlatCap must not draw the end pixel"
        );
    }
}

// The endpoint pixel is blended once. Qt draws each pixel of an aliased cosmetic
// line once (QCosmeticStroker::drawLine, qcosmeticstroker.cpp:263/:356). A menu separator
// (menu.rs paint_item): white alpha 31 over (22,25,32) from x=9.0 to x=258.0 at y=36.5.
// PySide6 6.11.2 probe, Antialiasing off: x=8 and x=259 stay (22,25,32); x=9..=258 are all
// (50,53,59), with x=9 and x=258 identical to the interior.
#[test]
fn g10_4d_aliased_line_endpoint_is_blended_once() {
    const W: u32 = 300;
    let bg = [22u8, 25, 32];
    let mut pm = Pixmap::new(W, W).unwrap();
    pm.fill(tiny_skia::Color::from_rgba8(bg[0], bg[1], bg[2], 255));
    {
        let mut p = Painter::begin(&mut pm);
        p.set_render_hint(RenderHint::Antialiasing, false);
        p.set_pen(Pen::new(
            tiny_skia::Color::from_rgba8(255, 255, 255, 31),
            1.0,
        ));
        p.draw_line(PointF::new(9.0, 36.5), PointF::new(258.0, 36.5));
    }
    let px = |x: usize, y: usize| {
        let i = (y * W as usize + x) * 4;
        [pm.data()[i], pm.data()[i + 1], pm.data()[i + 2]]
    };
    assert_eq!(px(8, 36), bg, "pixel before the start must stay background");
    assert_eq!(px(259, 36), bg, "pixel after the end must stay background");
    let interior = px(100, 36);
    for x in 9..=258usize {
        assert_eq!(
            px(x, 36),
            interior,
            "x={x}: each pixel of the line must be blended exactly once"
        );
    }
}

#[test]
fn aliased_line_with_non_finite_or_huge_endpoints_is_bounded() {
    let mut pm = Pixmap::new(16, 16).unwrap();
    {
        let mut p = Painter::begin(&mut pm);
        p.set_render_hint(RenderHint::Antialiasing, false);
        p.set_pen(Pen::new(tiny_skia::Color::from_rgba8(255, 0, 0, 255), 1.0));
        for (a, b) in [
            (PointF::new(1.0, 1.0), PointF::new(f32::INFINITY, 5.0)),
            (PointF::new(f32::NAN, 1.0), PointF::new(4.0, 4.0)),
            (PointF::new(-3.0e9, 2.0), PointF::new(3.0e9, 9.0)),
            (PointF::new(1.0, 1.0), PointF::new(f32::MAX, f32::MAX)),
        ] {
            p.draw_line(a, b);
        }
    }
    assert_eq!(pm.data().len(), 16 * 16 * 4);
}

#[test]
fn arc_span_is_clamped_to_one_full_turn() {
    let rect = RectF::new(0.0, 0.0, 100.0, 100.0);
    let full = create_arc_path(rect, 0.0, 360.0).unwrap();
    let huge = create_arc_path(rect, 0.0, 1.0e9).unwrap();
    assert_eq!(huge.len(), full.len());
    assert_eq!(huge.bounds(), full.bounds());
    let neg_full = create_arc_path(rect, 0.0, -360.0).unwrap();
    let neg_huge = create_arc_path(rect, 0.0, -1.0e9).unwrap();
    assert_eq!(neg_huge.len(), neg_full.len());
    assert_eq!(
        create_pie_path(rect, 0.0, 1.0e9).unwrap().len(),
        create_pie_path(rect, 0.0, 360.0).unwrap().len()
    );
    assert_eq!(
        create_donut_arc_path(rect, 10.0, 0.0, f32::MAX)
            .unwrap()
            .len(),
        create_donut_arc_path(rect, 10.0, 0.0, 360.0).unwrap().len()
    );
    assert!(create_arc_path(rect, 0.0, f32::INFINITY).is_none());
    assert!(create_arc_path(rect, 0.0, f32::NAN).is_none());
}
