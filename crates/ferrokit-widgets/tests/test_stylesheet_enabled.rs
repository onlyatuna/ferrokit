// SPDX-License-Identifier: LGPL-3.0-only
//! An enabled widget matches QSS `:enabled` rules, a disabled one
//! does not.
//!
//! `pseudoClass(QStyle::State)` sets `PseudoClass_Enabled` with `State_Enabled` and
//! `PseudoClass_Disabled` without it, never both (qstylesheetstyle.cpp:1756-1765; `"enabled"` is
//! parsed at qcssparser.cpp:304).
//! Reference: PySide6 6.11.2, sheet `S { color: blue } S:enabled { color: green }
//! S:disabled { color: red }` for `QLabel`, `QFrame#F`, `QPushButton` and `QProgressBar`: green
//! while enabled, red when disabled, green again when enabled; without the `:disabled` rule a
//! disabled widget is blue.

use ferrokit_gui::tiny_skia::Color;
use ferrokit_widgets::{Button, Frame, Label, ProgressBar, Widget, WidgetRef, set_widget_enabled};
use std::cell::RefCell;
use std::rc::Rc;

fn rgb(r: u8, g: u8, b: u8) -> Option<Color> {
    Some(Color::from_rgba8(r, g, b, 255))
}

const BLUE: (u8, u8, u8) = (0, 0, 255);
const GREEN: (u8, u8, u8) = (0, 255, 0);
const RED: (u8, u8, u8) = (255, 0, 0);

fn sheet(sel: &str, with_disabled: bool) -> String {
    let mut s = format!("{sel} {{ color: #0000ff; }} {sel}:enabled {{ color: #00ff00; }}");
    if with_disabled {
        s.push_str(&format!(" {sel}:disabled {{ color: #ff0000; }}"));
    }
    s
}

/// Runs the oracle sequence on one widget of type `W`; `color` reads its resolved colour.
fn check<W: Widget + 'static>(sel: &str, w: W, color: fn(&W) -> Option<Color>) {
    let w: WidgetRef = Rc::new(RefCell::new(Box::new(w)));
    let color = || color(w.borrow().as_any().downcast_ref::<W>().unwrap());
    let c = |(r, g, b): (u8, u8, u8)| rgb(r, g, b);
    w.borrow().set_style_sheet(&sheet(sel, true));
    assert_eq!(color(), c(GREEN), "{sel} enabled");
    set_widget_enabled(&w, false);
    assert_eq!(color(), c(RED), "{sel} disabled");
    set_widget_enabled(&w, true);
    assert_eq!(color(), c(GREEN), "{sel} enabled again");
    w.borrow().set_style_sheet(&sheet(sel, false));
    set_widget_enabled(&w, false);
    assert_eq!(color(), c(BLUE), "{sel} disabled, no :disabled rule");
}

#[test]
fn a_label_matches_enabled_only_while_enabled() {
    check("QLabel", Label::new("a"), |l| l.resolved_style().color);
}

#[test]
fn a_frame_matches_enabled_only_while_enabled() {
    let mut f = Frame::new();
    f.base.object_data.set_object_name("F");
    check("QFrame#F", f, |f| f.resolved_style().color);
}

#[test]
fn a_button_matches_enabled_only_while_enabled() {
    check("QPushButton", Button::new("b"), |b| {
        b.resolved_style().color
    });
}

#[test]
fn a_progress_bar_matches_enabled_only_while_enabled() {
    check("QProgressBar", ProgressBar::new(), |g| {
        g.resolved_groove_style().color
    });
}
