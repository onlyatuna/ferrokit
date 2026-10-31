// SPDX-License-Identifier: LGPL-3.0-only
//! `QLabel` sizing under a style sheet, as measured against PySide6 (Qt 6, Windows):
//! - no padding/border: the size hint is exactly the text (57x14 for a 14px "Antigravity");
//! - with a box (`padding: 1px 4px; border: 1px`) the width grows by border + padding + the advance
//!   of 'x' (a 9px Consolas "Plan: Free" is 65 wide for 50 of text), the height by border + padding;
//! - `font-size: 10.5px` is applied with `QFont::setPixelSize(int)`, i.e. as 11px.
use ferrokit_gui::text::font::Font;
use ferrokit_gui::text::font_metrics::FontMetrics;
use ferrokit_widgets::{Label, Widget};

fn label(text: &str, qss: &str) -> Label {
    let mut l = Label::new(text);
    l.set_font(Font::new("Consolas", 9.0));
    l.set_style_sheet(qss);
    l
}

#[test]
fn a_label_without_a_box_is_exactly_as_wide_as_its_text() {
    let l = label("Plan: Free", "font-size: 9px;");
    let font = Font::new("Consolas", 9.0);
    let text_w = FontMetrics::from_font(&font)
        .horizontal_advance_exact("Plan: Free", &font)
        .ceil() as i32;
    assert_eq!(l.size_hint().width, text_w);
}

#[test]
fn padding_and_border_add_their_size_and_the_x_indent() {
    let plain = label("Plan: Free", "font-size: 9px;").size_hint();
    let boxed = label(
        "Plan: Free",
        "font-size: 9px; padding: 1px 4px; border: 1px solid #333;",
    )
    .size_hint();

    let font = Font::new("Consolas", 9.0);
    let x_indent = FontMetrics::from_font(&font)
        .horizontal_advance_exact("x", &font)
        .round() as i32;
    assert_eq!(boxed.width - plain.width, 4 + 4 + 1 + 1 + x_indent);
    assert_eq!(boxed.height - plain.height, 1 + 1 + 1 + 1);
}

#[test]
fn qss_pixel_sizes_are_rounded_like_qt() {
    let half = label("SESSION 5H", "font-size: 10.5px;").size_hint();
    let eleven = label("SESSION 5H", "font-size: 11px;").size_hint();
    let ten = label("SESSION 5H", "font-size: 10px;").size_hint();
    assert_eq!(half, eleven);
    assert_ne!(half, ten);
}
