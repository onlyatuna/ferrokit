// SPDX-License-Identifier: LGPL-3.0-only
use ferrokit_gui::geometry::primitives::{Point, PointF, Rect, RectF};
use ferrokit_gui::paint::painter::{Brush, Painter, Pen};
use ferrokit_gui::paint::pixmap::Pixmap;
use ferrokit_gui::text::font::{Font, FontTag, FontWeight};
use ferrokit_gui::text::font_metrics::FontMetrics;
use std::path::Path;

// =============================================================================
// Layer 1: Geometry and layout mathematics tests
// =============================================================================

#[test]
fn test_layer1_rect_operations() {
    let r1 = Rect::new(0, 0, 100, 100);
    let r2 = Rect::new(50, 50, 100, 100);

    // 1. Intersection test
    let inter = r1.intersected(&r2);
    assert_eq!(inter, Rect::new(50, 50, 50, 50));

    // 2. Union test
    let union = r1.united(&r2);
    assert_eq!(union, Rect::new(0, 0, 150, 150));

    // 3. Hit test ([left, right) x [top, bottom))
    assert!(r1.contains(Point::new(50, 50)));
    assert!(r1.contains(Point::new(0, 0)));
    assert!(!r1.contains(Point::new(100, 50)));
    assert!(!r1.contains(Point::new(150, 150)));
}

// =============================================================================
// Layer 2: Font metrics and tabular numbers
// =============================================================================

#[test]
fn test_layer2_font_metrics_and_tabular_numbers() {
    let font = Font::new("Segoe UI", 14.0).with_feature(FontTag::TNUM, 1);
    let metrics = FontMetrics::from_font(&font);

    // 1. Verify tabular numbers (tnum): "11:11:11" and "88:88:88" advance must match
    let w1 = metrics.horizontal_advance("11:11:11", &font);
    let w2 = metrics.horizontal_advance("88:88:88", &font);
    assert!(
        (w1 - w2).abs() < 0.001,
        "Tabular numbers advance must match (w1={}, w2={})",
        w1,
        w2
    );

    // 2. Verify elided text
    let long_text = "Anthropic Claude 3.7 Sonnet Provider Card";
    let elided = metrics.elided_text(long_text, 100.0, &font);
    assert!(elided.ends_with("..."), "Long text must end with ellipsis");
    assert!(
        metrics.horizontal_advance(&elided, &font) <= 100.0,
        "Elided text width must not exceed maximum width"
    );
}

// =============================================================================
// Layer 3: Pixel-level assertions and PNG visual snapshot
// =============================================================================

#[test]
fn test_layer3_draw_rect_pixel_color() {
    let mut surface = Pixmap::new(100, 100).unwrap();
    surface.fill(tiny_skia::Color::TRANSPARENT);

    {
        let mut painter = Painter::begin(&mut surface);
        painter.set_pen(None);
        painter.set_brush(Brush::Color(tiny_skia::Color::from_rgba8(255, 0, 0, 255)));
        painter.draw_rect(RectF::new(20.0, 20.0, 60.0, 60.0));
    }

    // Verify center pixel (50, 50) is pure red
    let data = surface.data();
    let idx = ((50 * 100 + 50) * 4) as usize;
    assert_eq!(&data[idx..idx + 4], &[255, 0, 0, 255]);

    // Verify outside pixel (10, 10) remains transparent
    let outside_idx = ((10 * 100 + 10) * 4) as usize;
    assert_eq!(&data[outside_idx..outside_idx + 4], &[0, 0, 0, 0]);
}

#[test]
fn test_layer3_render_gauge_preview_to_png() {
    let mut surface = Pixmap::new(300, 300).unwrap();
    surface.fill(tiny_skia::Color::from_rgba8(30, 30, 35, 255));

    {
        let mut painter = Painter::begin(&mut surface);

        // 1. Outer ring track background
        painter.set_pen(Pen::new(tiny_skia::Color::from_rgba8(60, 60, 70, 255), 8.0));
        painter.set_brush(Brush::NoBrush);
        painter.draw_arc(RectF::new(20.0, 20.0, 260.0, 260.0), 0.0, 360.0);

        // 2. Outer ring progress bar
        painter.set_pen(Pen::new(
            tiny_skia::Color::from_rgba8(50, 205, 50, 255),
            8.0,
        ));
        painter.draw_arc(RectF::new(20.0, 20.0, 260.0, 260.0), 90.0, -270.0);

        // 3. Inner quota pie slice
        painter.set_pen(None);
        painter.set_brush(Brush::Color(tiny_skia::Color::from_rgba8(0, 150, 255, 180)));
        painter.draw_pie(RectF::new(70.0, 70.0, 160.0, 160.0), 90.0, -120.0);

        // 4. Center text
        let font = Font::new("Arial", 20.0)
            .with_weight(FontWeight::Bold)
            .with_feature(FontTag::TNUM, 1);
        painter.set_pen(Pen::from_rgba8(240, 240, 245, 255, 1.0));
        painter.draw_text(PointF::new(115.0, 155.0), "75%", &font);
    }

    // Save artifact to target/test_output directory
    let out_dir = Path::new("target/test_output");
    std::fs::create_dir_all(out_dir).unwrap();
    let out_file = out_dir.join("gauge_preview.png");
    surface.save_png(&out_file).unwrap();

    assert!(out_file.exists(), "PNG output file must exist");
}
