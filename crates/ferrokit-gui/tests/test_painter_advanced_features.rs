// SPDX-License-Identifier: LGPL-3.0-only
use ferrokit_gui::geometry::primitives::{PointF, RectF};
use ferrokit_gui::geometry::transform::Transform2D;
use ferrokit_gui::paint::brush::{Brush, LinearGradient, RadialGradient};
use ferrokit_gui::paint::composition::CompositionMode;
use ferrokit_gui::paint::painter::Painter;
use ferrokit_gui::paint::palette::{ColorGroup, ColorRole, Palette};
use ferrokit_gui::paint::path::{FillRule, PainterPath};
use ferrokit_gui::paint::pixmap::Pixmap;
use ferrokit_gui::text::document::TextDocument;
use ferrokit_gui::text::font::Font;
use std::sync::Arc;
use tiny_skia::Color;

#[test]
fn test_painter_path_primitives_and_bounds() {
    let mut path = PainterPath::new();
    assert!(path.is_empty());

    path.move_to(10.0, 10.0);
    path.line_to(50.0, 10.0);
    path.quad_to(70.0, 30.0, 50.0, 50.0);
    path.cubic_to(40.0, 70.0, 20.0, 70.0, 10.0, 50.0);
    path.close_subpath();

    assert!(!path.is_empty());
    assert_eq!(path.current_position(), PointF::new(10.0, 10.0));

    let bounds = path.control_point_rect().expect("Bounding box must exist");
    assert_eq!(bounds.x, 10.0);
    assert_eq!(bounds.y, 10.0);
    assert_eq!(bounds.right(), 70.0);
    assert_eq!(bounds.bottom(), 70.0);

    let skia_path = path.to_skia_path();
    assert!(skia_path.is_some());
}

#[test]
fn test_painter_path_shapes() {
    let mut rect_path = PainterPath::new();
    rect_path.add_rounded_rect(RectF::new(0.0, 0.0, 100.0, 50.0), 10.0, 10.0);
    assert!(!rect_path.is_empty());

    let mut ellipse_path = PainterPath::new();
    ellipse_path.add_ellipse(RectF::new(20.0, 20.0, 60.0, 60.0));
    assert!(!ellipse_path.is_empty());
    let bounds = ellipse_path.control_point_rect().unwrap();
    assert!((bounds.width - 60.0).abs() < 1e-3);
    assert!((bounds.height - 60.0).abs() < 1e-3);
}

#[test]
fn test_linear_and_radial_gradients() {
    let mut pixmap = Pixmap::new(100, 100).unwrap();
    {
        let mut painter = Painter::begin(&mut pixmap);

        // Linear gradient
        let mut linear = LinearGradient::new(PointF::new(0.0, 0.0), PointF::new(100.0, 0.0));
        linear.add_stop(0.0, Color::from_rgba8(255, 0, 0, 255));
        linear.add_stop(1.0, Color::from_rgba8(0, 0, 255, 255));

        painter.set_brush(Brush::linear_gradient(linear));
        painter.set_pen(None);
        painter.draw_rect(RectF::new(0.0, 0.0, 100.0, 100.0));
    }

    // Sample pixels: left edge should be reddish, right edge should be bluish
    let left_pixel = pixmap.pixel(0, 50).unwrap();
    let right_pixel = pixmap.pixel(99, 50).unwrap();
    assert!(
        left_pixel.red() > 200,
        "Left pixel should be red: {:?}",
        left_pixel
    );
    assert!(
        right_pixel.blue() > 200,
        "Right pixel should be blue: {:?}",
        right_pixel
    );

    // Radial gradient
    {
        let mut painter = Painter::begin(&mut pixmap);
        let mut radial = RadialGradient::new(PointF::new(50.0, 50.0), 50.0);
        radial.add_stop(0.0, Color::from_rgba8(0, 255, 0, 255));
        radial.add_stop(1.0, Color::from_rgba8(0, 0, 0, 255));

        painter.set_brush(Brush::radial_gradient(radial));
        painter.draw_rect(RectF::new(0.0, 0.0, 100.0, 100.0));
    }

    let center_pixel = pixmap.pixel(50, 50).unwrap();
    assert!(
        center_pixel.green() > 200,
        "Center pixel should be green: {:?}",
        center_pixel
    );
}

#[test]
fn test_texture_brush() {
    let mut pattern_pix = Pixmap::new(2, 2).unwrap();
    pattern_pix.fill(Color::from_rgba8(255, 255, 0, 255)); // Yellow

    let mut target = Pixmap::new(20, 20).unwrap();
    {
        let mut painter = Painter::begin(&mut target);
        painter.set_brush(Brush::texture(Arc::new(pattern_pix)));
        painter.set_pen(None);
        painter.draw_rect(RectF::new(0.0, 0.0, 20.0, 20.0));
    }

    let sample = target.pixel(10, 10).unwrap();
    assert_eq!(sample.red(), 255);
    assert_eq!(sample.green(), 255);
    assert_eq!(sample.blue(), 0);
}

#[test]
fn test_transform2d_affine_operations() {
    let t = Transform2D::identity();
    assert_eq!(
        t.map_point(PointF::new(10.0, 20.0)),
        PointF::new(10.0, 20.0)
    );

    let t_translate = Transform2D::from_translate(15.0, 25.0);
    assert_eq!(
        t_translate.map_point(PointF::new(5.0, 5.0)),
        PointF::new(20.0, 30.0)
    );

    let t_scale = Transform2D::from_scale(2.0, 3.0);
    assert_eq!(
        t_scale.map_point(PointF::new(10.0, 10.0)),
        PointF::new(20.0, 30.0)
    );

    let t_rotate = Transform2D::from_rotate(90.0);
    let p_rot = t_rotate.map_point(PointF::new(10.0, 0.0));
    assert!((p_rot.x).abs() < 1e-5);
    assert!((p_rot.y - 10.0).abs() < 1e-5);

    let t_shear = Transform2D::from_shear(0.5, 0.0);
    assert_eq!(
        t_shear.map_point(PointF::new(10.0, 20.0)),
        PointF::new(20.0, 20.0)
    );

    // Test inversion
    let inv = t_translate.inverted().unwrap();
    let round_trip = inv.map_point(t_translate.map_point(PointF::new(42.0, 84.0)));
    assert!((round_trip.x - 42.0).abs() < 1e-4);
    assert!((round_trip.y - 84.0).abs() < 1e-4);
}

#[test]
fn test_painter_transform_and_shear() {
    let mut pixmap = Pixmap::new(50, 50).unwrap();
    let mut painter = Painter::begin(&mut pixmap);

    painter.translate(10.0, 10.0);
    painter.rotate(45.0);
    painter.shear(0.2, 0.1);
    let curr_t = painter.transform();
    assert_ne!(curr_t, Transform2D::identity());

    painter.reset_transform();
    assert_eq!(painter.transform(), Transform2D::identity());
}

#[test]
fn test_composition_modes() {
    let mut pixmap = Pixmap::new(10, 10).unwrap();
    pixmap.fill(Color::from_rgba8(255, 0, 0, 255)); // Red background
    {
        let mut painter = Painter::begin(&mut pixmap);
        painter.set_composition_mode(CompositionMode::Clear);
        painter.set_brush(Brush::from_color(Color::BLACK));
        painter.set_pen(None);
        painter.draw_rect(RectF::new(0.0, 0.0, 10.0, 10.0));
    }
    let cleared = pixmap.pixel(5, 5).unwrap();
    assert_eq!(cleared.alpha(), 0);
}

#[test]
fn test_palette_system() {
    let dark_palette = Palette::dark();
    assert_eq!(dark_palette.window(), Color::from_rgba8(30, 30, 30, 255));
    assert_eq!(dark_palette.text(), Color::from_rgba8(220, 220, 220, 255));
    assert_eq!(
        dark_palette.highlight(),
        Color::from_rgba8(0, 122, 255, 255)
    );

    let disabled_txt = dark_palette.color(ColorGroup::Disabled, ColorRole::Text);
    assert_eq!(disabled_txt, Color::from_rgba8(120, 120, 120, 255));

    let light_palette = Palette::light();
    assert_eq!(
        light_palette.window(),
        Color::from_rgba8(240, 240, 240, 255)
    );
    assert_eq!(light_palette.text(), Color::from_rgba8(20, 20, 20, 255));

    let mut custom = Palette::new();
    custom.set_color_for_all(ColorRole::Button, Color::from_rgba8(100, 150, 200, 255));
    assert_eq!(custom.button(), Color::from_rgba8(100, 150, 200, 255));
    assert_eq!(
        custom.color(ColorGroup::Disabled, ColorRole::Button),
        Color::from_rgba8(100, 150, 200, 255)
    );
}

#[test]
fn test_text_document_layout_and_html_rendering() {
    let mut doc = TextDocument::new();
    doc.set_default_font(Font::new("Segoe UI", 12.0));
    doc.set_text_width(120.0); // force line wrapping

    doc.set_html("Hello <b>World</b>!<br/>This is a <i>test</i> with <font color=\"#ff0000\">red</font> text.");

    let lines = doc.layout_lines();
    assert!(
        lines.len() >= 2,
        "Expected multiple lines due to <br/> and wrap, got {}",
        lines.len()
    );

    let size = doc.size();
    assert!(size.width > 0.0);
    assert!(size.height > 0.0);

    // Verify rendering to a Pixmap
    let mut pixmap = Pixmap::new(200, 100).unwrap();
    let mut painter = Painter::begin(&mut pixmap);
    painter.draw_text_document(PointF::new(10.0, 10.0), &doc);
}

#[test]
fn painter_path_defaults_to_odd_even_fill_and_leaves_self_intersection_hole() {
    assert_eq!(FillRule::default(), FillRule::OddEven);
    let mut star = PainterPath::new();
    assert_eq!(star.fill_rule(), FillRule::OddEven);
    assert_eq!(
        PainterPath::with_start_point(PointF::new(1.0, 2.0)).fill_rule(),
        FillRule::OddEven
    );
    for k in 0..5 {
        let a = std::f32::consts::TAU * (k * 2) as f32 / 5.0;
        let pt = PointF::new(20.0 + 17.0 * a.sin(), 20.0 - 17.0 * a.cos());
        if k == 0 {
            star.move_to(pt.x, pt.y);
        } else {
            star.line_to(pt.x, pt.y);
        }
    }
    star.close_subpath();

    let mut odd_even_pm = Pixmap::new(40, 40).unwrap();
    {
        let mut p = Painter::begin(&mut odd_even_pm);
        p.set_pen(None);
        p.set_brush(Brush::Color(Color::WHITE));
        p.fill_painter_path(&star);
    }
    assert_eq!(
        odd_even_pm.pixel(20, 20).unwrap().alpha(),
        0,
        "default OddEven leaves a hole at the pentagram's self-intersecting centre"
    );

    star.set_fill_rule(FillRule::Winding);
    let mut winding_pm = Pixmap::new(40, 40).unwrap();
    {
        let mut p = Painter::begin(&mut winding_pm);
        p.set_pen(None);
        p.set_brush(Brush::Color(Color::WHITE));
        p.fill_painter_path(&star);
    }
    assert_eq!(
        winding_pm.pixel(20, 20).unwrap().alpha(),
        255,
        "explicit Winding fills the pentagram's centre"
    );
}

#[test]
fn hatch_and_texture_brush_phase_matches_qt_raster_engine_at_dpr_1_25() {
    // Build a 6x6 texture where texel (tx, ty) encodes tx in its red channel, matching the
    // 6x6 tile size of `Brush::Hatched`.
    let mut tile = Pixmap::new(6, 6).unwrap();
    for ty in 0..6u32 {
        for tx in 0..6u32 {
            let idx = ((ty * 6 + tx) * 4) as usize;
            tile.data_mut()[idx..idx + 4].copy_from_slice(&[tx as u8, ty as u8, 0, 255]);
        }
    }
    let tex_brush = Brush::texture(Arc::new(tile));

    // Expected texel x-indices measured from Qt 6.11.2 `QRasterPaintEngine` (`QImage` with
    // `setDevicePixelRatio(1.25)` and `p.translate(gx, 0)`):
    let expected_at_1_25: &[(f32, [u8; 15])] = &[
        (0.0, [0, 1, 1, 2, 3, 4, 5, 5, 0, 1, 2, 3, 3, 4, 5]),
        (1.0, [5, 0, 0, 1, 2, 3, 4, 4, 5, 0, 1, 2, 2, 3, 4]),
        (2.0, [4, 5, 5, 0, 1, 2, 3, 3, 4, 5, 0, 1, 1, 2, 3]),
        (3.0, [3, 4, 4, 5, 0, 1, 2, 2, 3, 4, 5, 0, 0, 1, 2]),
        (6.0, [0, 1, 1, 2, 3, 4, 5, 5, 0, 1, 2, 3, 3, 4, 5]),
    ];
    for &(gx, ref expected) in expected_at_1_25 {
        let mut pm = Pixmap::with_dpr(20, 2, 1.25).unwrap();
        {
            let mut p = Painter::begin(&mut pm);
            p.translate(gx, 0.0);
            p.set_pen(None);
            p.set_brush(tex_brush.clone());
            p.draw_rect(RectF::new(-10.0, 0.0, 30.0, 2.0));
        }
        let got: Vec<u8> = (0..15).map(|dx| pm.pixel(dx, 0).unwrap().red()).collect();
        assert_eq!(got, expected, "texel phase at DPR 1.25 gx={gx}");
    }

    // `Brush::Hatched` uses the same 6x6 nearest-neighbour repeat phase at DPR 1.25:
    // translating by 6 logical pixels (7.5 physical pixels, one full 6-texel period) repeats the
    // exact stripe phase at device pixel 0, and `set_brush_origin` shifts the phase identically
    // to `translate`.
    let render_hatch_row = |gx: f32, origin_x: f32| -> Vec<u8> {
        let mut pm = Pixmap::with_dpr(16, 2, 1.25).unwrap();
        {
            let mut p = Painter::begin(&mut pm);
            p.translate(gx, 0.0);
            p.set_brush_origin(PointF::new(origin_x, 0.0));
            p.set_pen(None);
            p.set_brush(Brush::hatched(Color::WHITE));
            p.draw_rect(RectF::new(-10.0, 0.0, 30.0, 2.0));
        }
        (0..16).map(|dx| pm.pixel(dx, 0).unwrap().alpha()).collect()
    };
    let row_0 = render_hatch_row(0.0, 0.0);
    let row_6 = render_hatch_row(6.0, 0.0);
    assert_eq!(
        row_0, row_6,
        "6-px tile repeats after 6 logical pixels (7.5 device pixels)"
    );
    assert_eq!(
        render_hatch_row(1.0, 2.0),
        render_hatch_row(3.0, 0.0),
        "brush_origin shifts pattern phase identically to user-space translation"
    );
    // At DPR 1.25, physical pixels 1 and 2 both sample tile column 1 (alpha <= 2), while
    // physical pixels 6 and 7 both sample tile column 5 (alpha == 255).
    assert_eq!(row_0[6], 255);
    assert_eq!(row_0[7], 255);
}
