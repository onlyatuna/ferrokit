// SPDX-License-Identifier: LGPL-3.0-only
//! QSS `min/max-width/height` name the content box: Qt adds padding and border when it sets
//! `minimumSize`/`maximumSize` (`QStyleSheetStyle::setGeometry`, `qstylesheetstyle.cpp:2595-2612`),
//! and `QLabel::sizeHint` is expanded to `minimumSize` (`qlabel.cpp:620`) but never taken from
//! `max-height`. Expected numbers are PySide6 measurements.
use ferrokit_widgets::frame::Frame;
use ferrokit_widgets::progress_bar::ProgressBar;
use ferrokit_widgets::{Button, Label, Widget};

const MAX: i32 = 16777215;

fn sizes(w: &dyn Widget) -> ([i32; 2], [i32; 2]) {
    let (min, max) = (w.minimum_size(), w.maximum_size());
    ([min.width, min.height], [max.width, max.height])
}

#[test]
fn test_button_min_max_include_padding_and_border() {
    // a toggle button: padding 1px 4px, border 1px, min-width 18px, max-height 18px
    let b = Button::new("\u{21c4}");
    b.set_style_sheet("QPushButton { padding: 1px 4px; border: 1px solid #888; min-width: 18px; max-height: 18px; font-size: 11px; }");
    assert_eq!(sizes(&b), ([28, 0], [MAX, 22]), "PySide6 B1");

    let b = Button::new("AB");
    b.set_style_sheet("QPushButton { padding: 2px 3px; border: 1px solid #888; max-width: 50px; min-height: 10px; font-size: 11px; }");
    assert_eq!(sizes(&b), ([0, 16], [58, MAX]), "PySide6 B3");
}

#[test]
fn test_label_min_max_include_padding_and_border() {
    let cases: [(&str, ([i32; 2], [i32; 2])); 4] = [
        ("padding: 1px 4px; border: 1px solid #888; min-width: 30px; font-size: 9px;", ([40, 0], [MAX, MAX])),
        ("padding: 2px; border: 1px solid #888; max-height: 15px; font-size: 9px;", ([0, 0], [MAX, 21])),
        ("padding: 2px 3px; border: 1px solid #888; min-height: 20px; font-size: 9px;", ([0, 26], [MAX, MAX])),
        ("padding: 3px; min-height: 12px; max-height: 30px; font-size: 9px;", ([0, 18], [MAX, 36])),
    ];
    for (qss, expected) in cases {
        let l = Label::new("AB");
        l.set_style_sheet(&format!("QLabel {{ {qss} }}"));
        assert_eq!(sizes(&l), expected, "{qss}");
    }
}

#[test]
fn test_label_size_hint_is_expanded_to_minimum_size_and_ignores_max_height() {
    // PySide6: L1 hint 40x(15|16), L3 hint 24x26, L5 hint 22x18 (L5's `max-height: 30px` is no hint)
    // L3's and L5's widths are the text's ("AB" in Segoe UI 9px, installed only on Windows) plus
    // the box; L1's width and every height come from `min-*`.
    let text_width_matches_oracle = cfg!(windows);
    for (qss, w, h, w_from_text) in [
        ("padding: 1px 4px; border: 1px solid #888; min-width: 30px; font-size: 9px;", Some(40), None, false),
        ("padding: 2px 3px; border: 1px solid #888; min-height: 20px; font-size: 9px;", Some(24), Some(26), true),
        ("padding: 3px; min-height: 12px; max-height: 30px; font-size: 9px;", Some(22), Some(18), true),
    ] {
        let l = Label::new("AB");
        l.set_style_sheet(&format!("QLabel {{ {qss} }}"));
        let hint = l.size_hint();
        if let Some(w) = w.filter(|_| !w_from_text || text_width_matches_oracle) {
            assert_eq!(hint.width, w, "{qss}");
        }
        if let Some(h) = h {
            assert_eq!(hint.height, h, "{qss}");
        }
    }
}

#[test]
fn test_frame_min_max_include_padding_and_border() {
    let f = Frame::new();
    f.set_style_sheet("QFrame { border: 2px solid #888; padding: 3px; min-width: 10px; max-width: 40px; min-height: 5px; max-height: 30px; }");
    assert_eq!(sizes(&f), ([20, 15], [50, 40]), "PySide6 F2");

    // divider widgets: no border, so the box is the content box
    let f = Frame::new();
    f.set_style_sheet("QFrame { border: none; min-width: 1px; max-width: 1px; }");
    assert_eq!(sizes(&f), ([1, 0], [1, MAX]), "PySide6 F1");
}

#[test]
fn test_progress_bar_min_max_include_padding_and_border() {
    let p = ProgressBar::new();
    p.set_style_sheet("QProgressBar { border: 1px solid #888; padding: 2px; min-height: 5px; max-height: 5px; }");
    assert_eq!(sizes(&p), ([0, 11], [MAX, 11]), "PySide6 P2");

    let p = ProgressBar::new();
    p.set_style_sheet("QProgressBar { border: none; min-height: 5px; max-height: 5px; }");
    assert_eq!(sizes(&p), ([0, 5], [MAX, 5]), "PySide6 P1");
}
