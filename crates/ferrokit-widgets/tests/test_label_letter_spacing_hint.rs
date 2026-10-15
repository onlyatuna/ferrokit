// SPDX-License-Identifier: LGPL-3.0-only
//! `letter-spacing` in a `Label`'s size hint (a card title style).
//!
//! Qt stores the spacing as `QFixed::fromReal(0.4)` = 25/64 px, truncated to 26.6 fixed point
//! (`qfont.cpp` `setLetterSpacing`, `qfixed_p.h`), and adds it to every glyph advance
//! (`QTextEngine::shapeText`). Adding 0.4 exactly makes a 28-character title a pixel wider at DPR 1.0
//! (184 instead of 183). Expected values are PySide6 6.11.2 measurements on Windows (DirectWrite)
//! at DPR 1.25 and DPR 1.0.
#![cfg(windows)]
use ferrokit_core::object::QObject;
use ferrokit_widgets::{Label, Widget};

const SHEET: &str = "\
QLabel { color: #e2e8f0; font-family: 'Segoe UI', 'SF Pro Display', 'Microsoft JhengHei', sans-serif; }
QLabel#Title { font-size: 9.5px; font-weight: 800; letter-spacing: 0.4px; color: #a855f7; }";

/// The application device pixel ratio is process-global: tests take turns.
static DPR_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn hint(text: &str) -> (i32, i32) {
    let mut label = Label::new(text);
    label.set_object_name("Title");
    label.set_style_sheet(SHEET);
    let h = label.size_hint();
    (h.width, h.height)
}

fn check(dpr: f32, expected: [(&str, (i32, i32)); 4]) {
    let _guard = DPR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    ferrokit_gui::text::font_database::set_application_device_pixel_ratio(dpr);
    let got: Vec<_> = expected.iter().map(|(t, _)| (*t, hint(t))).collect();
    ferrokit_gui::text::font_database::set_application_device_pixel_ratio(1.0);
    assert_eq!(got, expected.to_vec(), "DPR {dpr}");
}

#[test]
fn letter_spacing_hint_matches_pyside6_at_dpr_1_25() {
    check(
        1.25,
        [
            ("OPENAI CODEX", (78, 14)),
            ("OPENAI CODEX EXTRA LONG NAME", (182, 14)),
            ("AAAAAAAAAA", (75, 14)),
            ("OPENAI CODEX EXTRA", (115, 14)),
        ],
    );
}

#[test]
fn letter_spacing_hint_matches_pyside6_at_dpr_1_0() {
    check(
        1.0,
        [
            ("OPENAI CODEX", (78, 14)),
            ("OPENAI CODEX EXTRA LONG NAME", (183, 14)),
            ("AAAAAAAAAA", (74, 14)),
            ("OPENAI CODEX EXTRA", (116, 14)),
        ],
    );
}
