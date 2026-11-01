// SPDX-License-Identifier: LGPL-3.0-only
//! Tab stops of `TextDocument`, as measured against PySide6 6.11.2 (`QTextDocument` with a document
//! margin of 0, `QTextOption::setTabStopDistance` on its default text option) with the synthetic
//! oracle font (16 px; "a" is 7.5 px, "b" 7.625 px). The line layout itself is covered by the
//! tab cases of `text_layout_oracle.txt`.
use ferrokit_gui::text::TextDocument;
use ferrokit_gui::text::font::{Font, SharedFontData};
use ferrokit_gui::text::font_database::set_application_device_pixel_ratio;

fn document(text: &str, tab_stop: Option<f32>, width: Option<f32>) -> (f32, usize) {
    let font = Font::new("FerrokitOracleText", 16.0).with_font_data(SharedFontData::from_vec(
        include_bytes!("data/oracle_text.ttf").to_vec(),
    ));
    let mut one = TextDocument::from_plain_text("a");
    one.set_default_font(font.clone());
    let line = one.size().height;
    let mut doc = TextDocument::from_plain_text(text);
    doc.set_default_font(font);
    if let Some(distance) = tab_stop {
        doc.set_tab_stop_distance(distance);
    }
    if let Some(w) = width {
        doc.set_text_width(w);
    }
    let size = doc.size();
    (size.width, (size.height / line).round() as usize)
}

#[test]
fn a_document_expands_tabs_to_its_tab_stop_distance_as_qt_does() {
    // The numbers are Qt's DirectWrite engine's (a device pixel ratio other than 1).
    set_application_device_pixel_ratio(2.0);
    // (text, tab stop distance, text width) -> (idealWidth, lines), from QTextDocument; ferrokit's `size().width` is the ideal width.
    assert_eq!(document("a\tb", None, None), (87.625, 1));
    assert_eq!(document("aaa\tb", Some(30.0), None), (37.625, 1));
    assert_eq!(document("aaa\tb", Some(20.0), None), (47.625, 1));
    assert_eq!(document("a\tb\n\tb", Some(20.0), None), (27.625, 2));
    assert_eq!(document("a\tb", None, Some(50.0)), (7.625, 2));
    assert_eq!(document("a\t\tb", Some(20.0), Some(30.0)), (20.0, 2));
}
