// SPDX-License-Identifier: LGPL-3.0-only
//! `QLabel` size hints of text with carriage returns, as measured against PySide6 6.11.2 with the
//! synthetic oracle font (16 px; every advance is a multiple of 1/64 px, so the numbers do not
//! depend on the platform's font engine; "a" is 7.5 px, "b" 7.625 px, a space 4 px):
//! - a `"\r"` before a line feed (`"\r\n"`, `"\r\r\n"`) has no width; the line feed breaks the line;
//! - any other `"\r"` is as wide as a space (`QLabel` sizes through `qt_format_text`, unlike a bare
//!   `QTextLayout`, where `"\r"` has no width anywhere).
//!
//! The same sizes come out with and without word wrap. The expected value is (width, lines): the
//! height of a one-line label is the font's own, so the test compares the height in whole lines.
use ferrokit_gui::text::font::{Font, SharedFontData};
use ferrokit_widgets::{Label, Widget};

/// Qt's DirectWrite engine (any device pixel ratio but 1) gives this font 12.5 + 3.5 = 16 px
/// lines; at a ratio of 1 its GDI-style rounding makes them 17 px with a leading of -1, which
/// spaces the lines 16 apart (measured), so whole lines of the one-line height only hold under
/// DirectWrite. The ratio is process-wide: this sets it for one measurement and puts it back.
struct DirectWriteRatio(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);

static RATIO: std::sync::Mutex<()> = std::sync::Mutex::new(());

impl DirectWriteRatio {
    fn set() -> Self {
        let guard = RATIO.lock().unwrap_or_else(|e| e.into_inner());
        ferrokit_gui::text::font_database::set_application_device_pixel_ratio(1.25);
        Self(guard)
    }
}

impl Drop for DirectWriteRatio {
    fn drop(&mut self) {
        ferrokit_gui::text::font_database::set_application_device_pixel_ratio(1.0);
    }
}

/// (width, number of lines of the font's line height).
fn hint(text: &str, word_wrap: bool) -> (i32, i32) {
    let _ratio = DirectWriteRatio::set();
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
fn carriage_returns_size_a_label_as_qt_does() {
    for (text, expected) in [
        ("a", (8, 1)),
        ("a\rb", (20, 1)),
        ("a\r\rb", (24, 1)),
        ("\r", (4, 1)),
        ("a\r", (12, 1)),
        ("\ra", (12, 1)),
        ("a\r\nb", (8, 2)),
        ("a\r\r\nb", (8, 2)),
        ("a\r\n", (8, 2)),
        ("\r\nb", (8, 2)),
        ("a\n\rb", (12, 2)),
        ("a\n\r", (8, 2)),
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

/// An empty line is a whole line in a word-wrapped label too (it was a pixel short of one when the
/// line height was guessed).
#[test]
fn a_carriage_return_line_feed_pair_alone_on_a_line_is_an_empty_line() {
    for word_wrap in [false, true] {
        assert_eq!(
            hint("a\r\n\r\nb", word_wrap),
            (8, 3),
            "word_wrap={word_wrap}"
        );
    }
}
