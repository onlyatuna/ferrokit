// SPDX-License-Identifier: LGPL-3.0-only
#![cfg(target_os = "windows")]
use ferrokit_gui::geometry::primitives::PointF;
use ferrokit_gui::paint::painter::{Painter, Pen};
use ferrokit_gui::paint::pixmap::Pixmap;
use ferrokit_gui::text::font::Font;
use tiny_skia::Color;

#[test]
fn test_color_emoji_renders_multi_color_pixels_aligned_with_qt6() {
    let width = 200;
    let height = 60;
    let mut pixmap = Pixmap::new(width, height).expect("create pixmap");
    pixmap.fill(Color::BLACK);

    {
        let mut painter = Painter::begin(&mut pixmap);
        // Text pen color is pure white
        painter.set_pen(Pen::new(Color::WHITE, 1.0));

        let font = Font::new("Segoe UI", 24.0);
        // Draw string starting with emoji 🔄
        painter.draw_text(PointF::new(10.0, 40.0), "🔄 重新整理", &font);
    }

    let data = pixmap.data();

    // Check if there are colored pixels (where R != G or R != B, not grayscale)
    let mut colored_pixels = 0;
    let mut blue_colored_pixels = 0;

    for y in 0..height {
        for x in 0..width {
            let idx = ((y * width + x) * 4) as usize;
            let r = data[idx];
            let g = data[idx + 1];
            let b = data[idx + 2];
            let a = data[idx + 3];

            if a > 0 {
                // If r != g or g != b, it's a chromatic (colored) pixel, not grayscale!
                if r != g || g != b {
                    colored_pixels += 1;
                }
                // Check for Segoe UI Emoji refresh blue circle (#00a6ed: high blue, high green, low red)
                if b > 150 && g > 100 && r < 50 {
                    blue_colored_pixels += 1;
                }
            }
        }
    }

    println!(
        "Total colored pixels: {}, Blue circle pixels: {}",
        colored_pixels, blue_colored_pixels
    );
    assert!(
        colored_pixels > 0,
        "Emoji must render multi-color pixels, not be flattened into monochrome text color!"
    );
    assert!(
        blue_colored_pixels > 0,
        "Refresh emoji 🔄 must render its signature blue background from Segoe UI Emoji COLR table!"
    );
}
