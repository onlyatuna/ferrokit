// SPDX-License-Identifier: LGPL-3.0-only
//! `:focus` on `Label` / `ProgressBar`: giving either keyboard focus makes it
//! match QSS `:focus` rules, as it already did for `Button` and `Frame`. `Label` kept the trait's
//! no-op `set_has_focus`; `ProgressBar` already stored the flag through `leaf_widget_common!`.
//!
//! `pseudoClass(QStyle::State)` maps `State_HasFocus` to `PseudoClass_Focus`
//! (qstylesheetstyle.cpp:1772-1773).
//! Reference: PySide6 6.11.2, a `QLabel` and a `QProgressBar` under one parent sheet
//! `QLabel { background: green } QLabel:focus { background: red } QProgressBar { background:
//! green } QProgressBar:focus { background: blue }`: the label is red while focused, the bar blue
//! once the focus moves to it (the label back to green), and `clearFocus()` makes both green. The
//! same holds with their default `NoFocus` policy and with `StrongFocus`: `setFocus()` does not
//! look at the policy, nor does `FocusManager::set_focus`.

use ferrokit_core::event::FocusReason;
use ferrokit_gui::geometry::primitives::Rect;
use ferrokit_gui::tiny_skia::Color;
use ferrokit_widgets::*;
use std::cell::RefCell;
use std::rc::Rc;

const SHEET: &str = "QLabel { background: #00ff00; } QLabel:focus { background: #ff0000; } \
                     QProgressBar { background: #00ff00; } \
                     QProgressBar:focus { background: #0000ff; }";

fn rgb(r: u8, g: u8, b: u8) -> Option<Color> {
    Some(Color::from_rgba8(r, g, b, 255))
}

fn boxed(w: impl Widget + 'static) -> WidgetRef {
    Rc::new(RefCell::new(Box::new(w)))
}

fn label_bg(w: &WidgetRef) -> Option<Color> {
    let w = w.borrow();
    let l = w.as_any().downcast_ref::<Label>().unwrap();
    l.resolved_style().background_color
}

fn bar_bg(w: &WidgetRef) -> Option<Color> {
    let w = w.borrow();
    let g = w.as_any().downcast_ref::<ProgressBar>().unwrap();
    g.resolved_groove_style().background_color
}

#[test]
fn focus_moves_the_focus_rule_between_a_label_and_a_progress_bar() {
    let root = boxed(EmptyWidget::with_geometry(Rect::new(0, 0, 100, 100)));
    root.borrow().set_style_sheet(SHEET);
    let l = boxed(Label::new("a"));
    let g = boxed(ProgressBar::new());
    let mut layout = BoxLayout::vertical();
    for w in [&l, &g] {
        layout.add_widget(w.clone());
    }
    root.borrow_mut().set_layout(Box::new(layout));
    adopt_tree(&root);

    let colors = || (label_bg(&l), bar_bg(&g));
    let green = rgb(0, 255, 0);
    let mut fm = FocusManager::new();
    assert_eq!(colors(), (green, green));

    let id = l.borrow().id();
    assert!(fm.set_focus(&root, Some(id), FocusReason::Other));
    assert!(l.borrow().has_focus());
    assert_eq!(colors(), (rgb(255, 0, 0), green));

    let id = g.borrow().id();
    assert!(fm.set_focus(&root, Some(id), FocusReason::Other));
    assert!(g.borrow().has_focus());
    assert_eq!(colors(), (green, rgb(0, 0, 255)));

    fm.clear_focus(&root, FocusReason::Other);
    assert!(!g.borrow().has_focus());
    assert_eq!(colors(), (green, green));
}
