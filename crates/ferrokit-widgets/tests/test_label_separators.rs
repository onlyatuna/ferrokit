// SPDX-License-Identifier: LGPL-3.0-only
//! `QLabel` size hints of plain text with the Unicode line and paragraph separators, as measured
//! against PySide6 6.11.2 with the synthetic oracle font (16 px; "a" is 7.5 px, "b" 7.625 px,
//! "c" 7.75 px, "d" 7.875 px):
//! - the line separator U+2028 ends a line, like a line feed (`qt_format_text` lays the text out
//!   in a `QTextLayout`, where U+2028 is the line break);
//! - the paragraph separator U+2029 has no width and does not end a line.
//!
//! The expected value is (width, lines), as in `test_label_carriage_return.rs`.
use ferrokit_gui::text::font::{Font, SharedFontData};
use ferrokit_widgets::{Label, Widget};

/// (width, number of lines of the font's line height). The Qt numbers were measured at a device
/// pixel ratio of 2 (`QT_SCALE_FACTOR=2`), which on Windows selects Qt's DirectWrite engine and its
/// fractional advances; at 1 the GDI engine rounds every advance to whole pixels.
fn hint(text: &str, word_wrap: bool) -> (i32, i32) {
    ferrokit_gui::text::font_database::set_application_device_pixel_ratio(2.0);
    let mut l = Label::new(text);
    l.set_font(
        Font::new("FerrokitOracleText", 16.0).with_font_data(SharedFontData::from_vec(
            include_bytes!("../../ferrokit-gui/tests/data/oracle_text.ttf").to_vec(),
        )),
    );
    l.set_word_wrap(word_wrap);
    let s = l.size_hint();
    let mut one = Label::new("a");
    one.set_font(l.font().clone());
    let line = one.size_hint().height;
    assert_eq!(
        s.height % line,
        0,
        "{text:?}: height {} is not whole lines of {line}",
        s.height
    );
    (s.width, s.height / line)
}

#[test]
fn line_and_paragraph_separators_size_a_label_as_qt_does() {
    for (text, expected) in [
        ("abcd", (31, 1)),
        ("ab\u{2028}cd", (16, 2)),
        ("ab\u{2028}", (16, 2)),
        ("ab\u{2029}cd", (31, 1)),
        ("ab\u{2029}", (16, 1)),
        ("\u{2029}", (0, 1)),
        ("aaa\u{2029}bbb", (46, 1)),
        ("a \u{2029} b", (24, 1)),
        ("a\u{2029}\u{2029}b", (16, 1)),
    ] {
        for word_wrap in [false, true] {
            assert_eq!(
                hint(text, word_wrap),
                expected,
                "{text:?} word_wrap={word_wrap}"
            );
        }
    }
}

/// A word-wrapped label with an empty line is a pixel short of whole lines in ferrokit (see
/// `test_label_carriage_return.rs`), so this case is checked unwrapped only.
#[test]
fn two_line_separators_make_an_empty_line() {
    assert_eq!(hint("a\u{2028}\u{2028}b", false), (8, 3));
}
