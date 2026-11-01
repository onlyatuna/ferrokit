// SPDX-License-Identifier: LGPL-3.0-only
//! A disabled widget matches QSS `:disabled` rules.
//!
//! `pseudoClass(QStyle::State)` maps a missing `State_Enabled` to `PseudoClass_Disabled`
//! (qstylesheetstyle.cpp:1756-1765; `"disabled"` is parsed at qcssparser.cpp:301).
//! Reference colours: PySide6 6.11.2 — `QLabel:disabled`, `QFrame#F:disabled`,
//! `QPushButton:disabled` and `QProgressBar:disabled` all apply once the widget is disabled.

use ferrokit_gui::tiny_skia::Color;
use ferrokit_widgets::{Button, Frame, Label, ProgressBar, Widget, WidgetRef, set_widget_enabled};
use std::cell::RefCell;
use std::rc::Rc;

fn rgb(r: u8, g: u8, b: u8) -> Option<Color> {
    Some(Color::from_rgba8(r, g, b, 255))
}

fn shared(w: impl Widget + 'static) -> WidgetRef {
    Rc::new(RefCell::new(Box::new(w)))
}

/// Reads `w` as a `W`.
fn read<W: 'static, R>(w: &WidgetRef, f: impl FnOnce(&W) -> R) -> R {
    f(w.borrow().as_any().downcast_ref::<W>().unwrap())
}

#[test]
fn a_disabled_label_uses_the_disabled_rule() {
    let l = shared(Label::new("a"));
    l.borrow()
        .set_style_sheet("QLabel { color: #00ff00; } QLabel:disabled { color: #ff0000; }");
    let color = || read(&l, |l: &Label| l.resolved_style().color);
    assert_eq!(color(), rgb(0, 255, 0));
    set_widget_enabled(&l, false);
    assert_eq!(color(), rgb(255, 0, 0));
    set_widget_enabled(&l, true);
    assert_eq!(color(), rgb(0, 255, 0));
}

#[test]
fn frame_button_and_progress_bar_match_disabled() {
    let mut f = Frame::new();
    f.base.object_data.set_object_name("F");
    let f = shared(f);
    f.borrow()
        .set_style_sheet("QFrame#F:disabled { color: #ff0000; }");
    let b = shared(Button::new("b"));
    b.borrow()
        .set_style_sheet("QPushButton:disabled { color: #ff0000; }");
    let g = shared(ProgressBar::new());
    g.borrow()
        .set_style_sheet("QProgressBar:disabled { color: #ff0000; }");
    let colors = || {
        (
            read(&f, |f: &Frame| f.resolved_style().color),
            read(&b, |b: &Button| b.resolved_style().color),
            read(&g, |g: &ProgressBar| g.resolved_groove_style().color),
        )
    };
    assert_eq!(colors(), (None, None, None));
    set_widget_enabled(&f, false);
    set_widget_enabled(&b, false);
    set_widget_enabled(&g, false);
    let red = rgb(255, 0, 0);
    assert_eq!(colors(), (red, red, red));
}

/// `pseudoClass` sets `PseudoClass_Hover` only inside the `State_Enabled` branch, so a button that
/// was under the mouse when it got disabled takes the `:disabled` rule, not `:hover`.
#[test]
fn a_hovered_button_that_is_disabled_drops_hover() {
    use ferrokit_core::event::{Event, EventKind};
    let b = shared(Button::new("b"));
    b.borrow().set_style_sheet(
        "QPushButton:hover { color: #00ff00; } QPushButton:disabled { color: #ff0000; }",
    );
    b.borrow_mut()
        .event(&mut Event::new_spontaneous(EventKind::Enter { x: 1, y: 1 }));
    let color = || read(&b, |b: &Button| b.resolved_style().color);
    assert_eq!(color(), rgb(0, 255, 0));
    set_widget_enabled(&b, false);
    b.borrow().set_style_sheet(
        "QPushButton:disabled { color: #ff0000; } QPushButton:hover { color: #00ff00; }",
    );
    assert_eq!(color(), rgb(255, 0, 0));
}
