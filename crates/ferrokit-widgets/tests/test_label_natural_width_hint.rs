// SPDX-License-Identifier: LGPL-3.0-only
//! A `Label`'s size hint is the `QTextLine::naturalTextWidth` of its text: the advances plus the
//! negative right bearing of the last glyph, the ink that hangs over it (`QTextLine::layout_helper`).
//! "if" in Arial 16 px has an advance of 7.98 px and a natural width of 8.53 px, so Qt's `QLabel`
//! is 9 px wide, not 8.
//!
//! Expected values are PySide6 6.11.2 measurements (`QLabel::sizeHint().width()`, Windows,
//! DirectWrite engine through a device pixel ratio other than 1, Arial at `setPixelSize(16)`). The
//! word-wrapped label has one word per text: the width Qt's `QLabel` finds for a wrapped paragraph
//! is another matter.
#![cfg(windows)]
use ferrokit_gui::text::font::Font;
use ferrokit_widgets::{Label, Widget};

/// The application device pixel ratio is process-global: tests take turns.
static DPR_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn hint(text: &str, word_wrap: bool, rich: bool) -> i32 {
    let mut label = Label::new(text);
    label.set_font(Font::new("Arial", 16.0));
    label.set_word_wrap(word_wrap);
    label.set_rich(rich);
    label.size_hint().width
}

fn check(cases: &[(&str, i32)], word_wrap: bool, rich: bool) {
    let _guard = DPR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    ferrokit_gui::text::font_database::set_application_device_pixel_ratio(2.0);
    let got: Vec<_> = cases
        .iter()
        .map(|(text, _)| (*text, hint(text, word_wrap, rich)))
        .collect();
    ferrokit_gui::text::font_database::set_application_device_pixel_ratio(1.0);
    assert_eq!(got, cases.to_vec(), "word wrap {word_wrap}, rich {rich}");
}

#[test]
fn a_plain_label_is_as_wide_as_the_natural_width_of_its_text() {
    // (text, Qt's width); the advance alone would give 8 for "if", 18 for "off", 25 for "self",
    // 19 for "ref", 13 for "fff", 9 for "ff" and 8 for "jf".
    check(
        &[
            ("def", 23),
            ("for", 19),
            ("if", 9),
            ("off", 19),
            ("buff", 27),
            ("self", 26),
            ("staff", 31),
            ("ref", 20),
            ("Ref", 26),
            ("fff", 14),
            ("Of", 18),
            ("jf", 9),
            ("rr", 11),
            ("car", 23),
            ("for r", 29),
            ("return", 42),
            ("your", 32),
            ("ff", 10),
            ("Tf", 15),
            ("LAV", 30),
            ("AV", 21),
            ("A", 11),
            ("AA", 22),
            ("VA", 21),
            ("Y", 11),
            ("7", 9),
            ("1", 9),
            // the widest line decides
            ("if\nff", 10),
            ("def\nif", 23),
        ],
        false,
        false,
    );
}

#[test]
fn a_word_wrapped_label_of_one_word_is_as_wide_as_its_natural_width() {
    check(&[("if", 9), ("def", 23), ("ref", 20)], true, false);
}

#[test]
fn a_rich_label_is_as_wide_as_the_natural_width_of_its_text() {
    check(&[("if", 9), ("ref", 20), ("off", 19)], false, true);
}
