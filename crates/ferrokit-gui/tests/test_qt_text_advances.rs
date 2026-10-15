// SPDX-License-Identifier: LGPL-3.0-only
//! Text widths must equal what `QFontMetricsF::horizontalAdvance` returns on Windows (Qt 6.11.2,
//! PySide6), at every device pixel ratio. The expected numbers were measured there; the wide sweep
//! over fonts, sizes and strings is a separate PySide6 comparison script.
//!
//! What they pin down (all read from Qt's sources):
//! * ratio 1 is laid out by Qt's GDI engine: whole-pixel advances;
//! * any other ratio by its DirectWrite engine: design advances truncated to 26.6, the same at
//!   125%, 150% and 200%;
//! * a `kern` table is scaled before it is split over the pair (`AV` is -126 font units: -63/64 px at
//!   16 px, where splitting the units first gives -62/64).
#![cfg(windows)]

use ferrokit_gui::text::font::{Font, FontWeight};
use ferrokit_gui::text::font_database::set_application_device_pixel_ratio;
use ferrokit_gui::text::font_metrics::FontMetrics;

fn width(text: &str, px: f32, weight: FontWeight) -> f32 {
    let font = Font::new("Microsoft JhengHei UI", px).with_weight(weight);
    FontMetrics::from_font(&font).horizontal_advance_exact(text, &font)
}

// The process-wide ratio is shared state: one test, run in order.
#[test]
fn widths_match_qt_at_every_device_pixel_ratio() {
    if !std::path::Path::new("C:/Windows/Fonts/msjhbd.ttc").exists() {
        return;
    }

    // GDI engine.
    set_application_device_pixel_ratio(1.0);
    assert_eq!(width("Claude Code", 14.0, FontWeight::SemiBold), 86.0);

    // DirectWrite engine; the ratio itself does not matter.
    for dpr in [1.25, 1.5, 2.0] {
        set_application_device_pixel_ratio(dpr);
        assert_eq!(width("Claude Code", 14.0, FontWeight::SemiBold), 85.765625, "x{dpr}");
        assert_eq!(width("AV", 16.0, FontWeight::Normal), 20.765625, "AV x{dpr}");
        assert_eq!(width("To", 16.0, FontWeight::Normal), 17.296875, "To x{dpr}");
        assert_eq!(width("Wa", 16.0, FontWeight::Normal), 24.171875, "Wa x{dpr}");
    }
    set_application_device_pixel_ratio(1.0);
}
