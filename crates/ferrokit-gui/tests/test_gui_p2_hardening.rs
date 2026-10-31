// SPDX-License-Identifier: LGPL-3.0-only
//! Regression tests for GUI hardening: PPM header limits, raw-text HTML elements, 32-bit
//! overflows in scroll/clear/icon sizing, and cursor positions after merging inserts.

use ferrokit_gui::geometry::primitives::{Rect, Size};
use ferrokit_gui::geometry::region::Region;
use ferrokit_gui::image::icon::{Icon, IconMode, IconState};
use ferrokit_gui::image::io::ImageReader;
use ferrokit_gui::image::Image;
use ferrokit_gui::image::ImageFormat;
use ferrokit_gui::paint::pixmap::Pixmap;
use ferrokit_gui::paint::BackingStore;
use ferrokit_gui::text::cursor::TextCursor;
use ferrokit_gui::text::document::TextDocument;

#[test]
fn ppm_zero_width_with_huge_height_is_rejected_fast() {
    assert!(ImageReader::read_from_memory(b"P3\n0 4294967295 255\n").is_err());
    assert!(ImageReader::read_from_memory(b"P3\n4294967295 0 255\n").is_err());
    assert!(ImageReader::read_from_memory(b"P3\n32768 1 255\n").is_err());
}

#[test]
fn html_raw_text_elements_close_case_insensitively() {
    let doc =
        TextDocument::from_html("<STYLE>p { color: red }</Style><p>after</p><TITLE>t</TITLE>x");
    let text = doc.to_plain_text();
    assert!(text.contains("after"), "{text:?}");
    assert!(!text.contains("color"), "{text:?}");
}

#[test]
fn html_eight_digit_hex_is_aarrggbb() {
    let doc = TextDocument::from_html("<span style=\"color:#80ff0000\">x</span>");
    let c = doc.first_block().fragments()[0]
        .char_format()
        .foreground
        .expect("color");
    let u = c.to_color_u8();
    assert_eq!((u.red(), u.green(), u.blue(), u.alpha()), (255, 0, 0, 128));
}

#[test]
fn scroll_with_i32_min_offsets_does_not_overflow() {
    let mut bs = BackingStore::new(Size::new(10, 10), 1.0).unwrap();
    let area = Region::from_rect(Rect::new(0, 0, 10, 10));
    assert!(!bs.scroll(&area, i32::MIN, i32::MIN));
    let wide = Region::from_rect(Rect::new(
        -1_000_000_000,
        -1_000_000_000,
        2_000_000_000,
        2_000_000_000,
    ));
    assert!(!bs.scroll(&wide, i32::MAX, -1));
}

#[test]
fn clear_rect_with_huge_extent_does_not_overflow() {
    let mut p = Pixmap::new(4, 4).unwrap();
    p.fill(tiny_skia::Color::from_rgba8(1, 2, 3, 255));
    p.clear_rect(Rect::new(1, 1, i32::MAX, i32::MAX));
    assert_eq!(p.pixel(0, 0).unwrap().alpha(), 255);
    assert_eq!(p.pixel(3, 3).unwrap().alpha(), 0);
}

#[test]
fn icon_sizing_with_huge_target_does_not_overflow() {
    let icon = Icon::from_image(Image::new(4, 4, ImageFormat::Rgba8888Premultiplied));
    let big = Size::new(100_000, 100_000);
    assert_eq!(
        icon.actual_size(big, IconMode::Normal, IconState::Off),
        Size::new(4, 4)
    );
    let _ = icon.available_sizes(IconMode::Normal, IconState::Off);
}

#[test]
fn cursor_insert_merging_with_following_cluster_lands_after_it() {
    let mut doc = TextDocument::new();
    doc.set_plain_text("a\u{0301}b");
    // "á" is one cluster; delete "a" leaves a lone combining mark cluster, then insert "e" before it.
    let mut c = TextCursor::at_position(0);
    c.move_position(
        &doc,
        ferrokit_gui::text::cursor::MoveOperation::NextCharacter,
        ferrokit_gui::text::cursor::MoveMode::KeepAnchor,
        1,
    );
    c.remove_selected_text(&mut doc);
    assert_eq!(doc.to_plain_text(), "b");
    let mut doc = TextDocument::new();
    doc.set_plain_text("\u{0301}b");
    let mut c = TextCursor::at_position(0);
    c.insert_text(&mut doc, "e");
    assert_eq!(doc.to_plain_text(), "e\u{0301}b");
    assert_eq!(c.position(), 1, "cursor sits after the merged cluster");
}

#[test]
fn cursor_insert_multiline_advances_past_inserted_text() {
    let mut doc = TextDocument::new();
    doc.set_plain_text("xy");
    let mut c = TextCursor::at_position(1);
    c.insert_text(&mut doc, "ab\ncd");
    assert_eq!(doc.to_plain_text(), "xab\ncdy");
    assert_eq!(c.position(), 6);
}

#[test]
fn html_raw_text_end_tag_requires_a_delimiter() {
    let doc = TextDocument::from_html("<style>a</styles>b</style>after");
    let text = doc.to_plain_text();
    assert!(text.contains("after") && !text.contains('b'), "{text:?}");
}
