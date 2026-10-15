// SPDX-License-Identifier: LGPL-3.0-only
//! Attribute selectors `[prop="v"]` match the widget's own dynamic properties on every
//! widget kind (`QPushButton`, `QFrame`, `QProgressBar`, `QMenu`), not only `QLabel`. The value the
//! selector compares is the one set with `set_property` on that widget.

use ferrokit_gui::tiny_skia::Color;
use ferrokit_widgets::*;

fn red() -> Option<Color> {
    Some(Color::from_rgba8(255, 0, 0, 255))
}

#[test]
fn attribute_selector_on_button_and_frame() {
    let b = Button::new("x");
    b.set_style_sheet(r#"QPushButton[state="muted"] { color: #ff0000; }"#);
    b.set_property("state", "muted");
    assert_eq!(b.resolved_style().color, red());
    b.set_property("state", "ok");
    assert_ne!(b.resolved_style().color, red());

    let f = Frame::new();
    f.set_style_sheet(r#"QFrame[state="muted"] { color: #ff0000; }"#);
    f.set_property("state", "muted");
    assert_eq!(f.resolved_style().color, red());
    f.set_property("state", "ok");
    assert_ne!(f.resolved_style().color, red());
}

#[test]
fn attribute_selector_on_progress_bar_groove() {
    let p = ProgressBar::new();
    p.set_style_sheet(r#"QProgressBar[state="warn"] { color: #ff0000; }"#);
    p.set_property("state", "warn");
    assert_eq!(p.resolved_groove_style().color, red());
    p.set_property("state", "ok");
    assert_ne!(p.resolved_groove_style().color, red());
}

#[test]
fn attribute_selector_on_menu_changes_size_hint() {
    let m = Menu::new("t");
    m.set_style_sheet(r#"QMenu[state="muted"] { padding: 13px 0px; }"#);
    let unmatched = m.size_hint().height;
    m.set_property("state", "muted");
    let matched = m.size_hint().height;
    assert_ne!(matched, unmatched);
}
