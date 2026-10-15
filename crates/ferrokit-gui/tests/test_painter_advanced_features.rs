// SPDX-License-Identifier: LGPL-3.0-only
use ferrokit_gui::geometry::primitives::{PointF, RectF};
use ferrokit_gui::geometry::transform::Transform2D;
use ferrokit_gui::paint::brush::{Brush, LinearGradient, RadialGradient};
use ferrokit_gui::paint::composition::CompositionMode;
use ferrokit_gui::paint::painter::Painter;
use ferrokit_gui::paint::palette::{ColorGroup, ColorRole, Palette};
use ferrokit_gui::paint::path::PainterPath;
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
