// SPDX-License-Identifier: LGPL-3.0-only
#![cfg(target_os = "macos")]

use ferrokit_gui::geometry::primitives::PointF;
use ferrokit_gui::paint::{Painter, Pen, Pixmap};
use ferrokit_gui::text::font::{Font, StyleHint};
use ferrokit_gui::text::font_database::resolve_font_engines_global;
use ferrokit_gui::text::font_metrics::FontMetrics;
use ferrokit_gui::tiny_skia::Color;

#[test]
fn test_system_ui_font_resolves_and_metrics_match_qt() {
    for hint in [StyleHint::AnyStyle, StyleHint::System] {
        for (px, want_asc, want_desc, want_h, want_avg) in [
            (12.0, 12.0, 3.0, 15.0, 7.0),
            (16.0, 15.0, 3.0, 18.0, 9.0),
            (20.0, 19.0, 4.0, 23.0, 12.0),
        ] {
            let mut font = Font::new("NoSuchFontXYZ", px);
            font.style_hint = hint;
            let m = FontMetrics::from_font(&font);
            assert_eq!(m.ascent, want_asc, "ascent at {px}px with {hint:?}");
            assert_eq!(m.descent, want_desc, "descent at {px}px with {hint:?}");
            assert_eq!(m.height, want_h, "height at {px}px with {hint:?}");
            assert_eq!(
                m.rounded_average_char_width(),
                want_avg,
                "avgCharWidth at {px}px with {hint:?}"
            );
        }
    }
}

/// `QFontMetricsF::horizontalAdvance` of the system UI font, measured with PySide6 6.12.0 on macOS 26
/// under the `cocoa` platform plugin (logical DPI 72, so HarfBuzz's points per em equal the pixel
/// size and its `trak` tracking of SF applies). The rows are 12, 16, 19, 20 and 22 px; 19 and 22
/// land on ties of the tracking's rounding. The `offscreen` plugin (logical DPI 96) gives other
/// numbers (67.5 at 12 px for the first string), which are not what a macOS application sees.
#[test]
fn test_system_ui_font_advances_match_qt_cocoa() {
    const TEXTS: [&str; 4] = ["5h 42% Wg", "0123456789", "AV", "To"];
    const EXPECTED: [(f32, [f32; 4]); 5] = [
        (12.0, [65.53125, 72.640625, 15.21875, 13.09375]),
        (16.0, [81.796875, 90.671875, 19.0625, 16.234375]),
        (19.0, [94.90625, 105.671875, 22.203125, 18.796875]),
        (20.0, [98.59375, 110.703125, 23.1875, 19.5]),
        (22.0, [109.390625, 124.75, 25.953125, 21.6875]),
    ];
    for (px, widths) in EXPECTED {
        let mut font = Font::new("NoSuchFontXYZ", px);
        font.style_hint = StyleHint::System;
        let metrics = FontMetrics::from_font(&font);
        for (text, expected) in TEXTS.iter().zip(widths) {
            assert_eq!(
                metrics.horizontal_advance_exact(text, &font),
                expected,
                "{text:?} at {px}px"
            );
        }
    }
}

#[test]
fn test_system_ui_font_draws_pixels() {
    let mut font = Font::new("NoSuchFontXYZ", 16.0);
    font.style_hint = StyleHint::System;

    let mut pm = Pixmap::with_dpr(300, 60, 1.0).unwrap();
    let mut painter = Painter::begin(&mut pm);
    painter.set_pen(Some(Pen::new(Color::from_rgba8(255, 255, 255, 255), 1.0)));
    painter.draw_text(PointF::new(4.0, 30.0), "5h 42% Wg", &font);
    drop(painter);

    let lit_bytes = pm.data().iter().filter(|&&b| b != 0).count();
    assert!(
        lit_bytes > 0,
        "System UI font must light pixels when drawn, got {lit_bytes}"
    );
}

#[test]
fn test_system_ui_font_outlines() {
    let mut font = Font::new("NoSuchFontXYZ", 16.0);
    font.style_hint = StyleHint::System;
    let engines = resolve_font_engines_global(&font);
    assert!(!engines.is_empty(), "must resolve an engine");
    let face = &engines[0].face;

    for ch in ['W', '5', '%'] {
        let gid = face.glyph_index(ch);
        assert_ne!(gid, 0, "glyph index for {ch:?} must not be 0");
        let outline = face.glyph_outline(gid, 16.0);
        assert!(
            outline.is_some(),
            "outline for {ch:?} (gid {gid}) must be produced by CoreText"
        );
        let path = outline.unwrap();
        assert!(
            path.bounds().width() > 0.0 && path.bounds().height() > 0.0,
            "path bounds for {ch:?} must not be empty"
        );
    }
}

#[test]
fn test_all_style_hints_resolve_on_macos() {
    let hints = [
        StyleHint::Helvetica,
        StyleHint::Times,
        StyleHint::Monospace,
        StyleHint::Cursive,
        StyleHint::Fantasy,
        StyleHint::Courier,
        StyleHint::System,
        StyleHint::AnyStyle,
    ];
    for hint in hints {
        let mut font = Font::new("NoSuchFontXYZ", 16.0);
        font.style_hint = hint;
        let engines = resolve_font_engines_global(&font);
        assert!(
            !engines.is_empty(),
            "style hint {hint:?} must resolve to at least one font engine on macOS"
        );
    }
}
