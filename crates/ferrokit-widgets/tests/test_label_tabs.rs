// SPDX-License-Identifier: LGPL-3.0-only
//! `QLabel` size hints of text with tabs, as measured against PySide6 6.11.2 with the synthetic
//! oracle font (16 px; "a" is 7.5 px, "b" 7.625 px, "x" 7.75 px): `QLabel` sizes through
//! `qt_format_text` with `Qt::TextExpandTabs`, where a tab advances to the next multiple of eight
//! times the width of an "x" (62 px here) from the start of the line, with and without word wrap.
//! The expected value is (width, lines of the font's line height).
use ferrokit_gui::text::font::{Font, SharedFontData};
use ferrokit_gui::text::font_database::set_application_device_pixel_ratio;
use ferrokit_widgets::{Label, Widget};

fn hint(text: &str, word_wrap: bool) -> (i32, i32) {
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
    assert_eq!(s.height % line, 0, "{text:?}: height {} is not whole lines of {line}", s.height);
    (s.width, s.height / line)
}

#[test]
fn tabs_advance_a_label_to_stops_of_eight_x_widths_as_in_qt() {
    // The numbers are Qt's DirectWrite engine's (a device pixel ratio other than 1).
    set_application_device_pixel_ratio(2.0);
    for (text, expected) in [
        ("a\tb", (70, 1)),
        ("\tb", (70, 1)),
        ("a\t", (62, 1)),
        ("\t", (62, 1)),
        ("aa\tbb", (78, 1)),
        ("aaaaaaa\tb", (70, 1)),
        ("aaaaaaaaaaaa\tb", (132, 1)),
        ("a\t\tb", (132, 1)),
        ("a\tb\n\tb", (70, 2)),
        ("a\tbb\nbbbbbbbbbbbbb\tb", (132, 2)),
        ("a\r\tb", (70, 1)),
    ] {
        for word_wrap in [false, true] {
            assert_eq!(hint(text, word_wrap), expected, "{text:?} word_wrap={word_wrap}");
        }
    }
}
