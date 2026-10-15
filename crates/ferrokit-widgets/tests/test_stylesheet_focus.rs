// SPDX-License-Identifier: LGPL-3.0-only
//! The widget with keyboard focus matches QSS `:focus` rules.
//!
//! `pseudoClass(QStyle::State)` maps `State_HasFocus` to `PseudoClass_Focus`
//! (qstylesheetstyle.cpp:1772-1773; `"focus"` is parsed at qcssparser.cpp:309).
//! Reference: PySide6 6.11.2, two `QPushButton`s and a `StrongFocus` `QFrame#F` under one parent
//! sheet `QPushButton:focus { background: red } QFrame#F:focus { background: blue }`: only the
//! focused widget paints its `:focus` background, focus moving from `a` to `b` moves it along,
//! and `clearFocus()` drops it.

use ferrokit_core::event::FocusReason;
use ferrokit_gui::geometry::primitives::Rect;
use ferrokit_gui::tiny_skia::Color;
use ferrokit_widgets::*;
use std::cell::RefCell;
use std::rc::Rc;

const SHEET: &str = "QPushButton { color: #00ff00; } QPushButton:focus { color: #ff0000; } \
                     QFrame#F:focus { color: #0000ff; }";

fn rgb(r: u8, g: u8, b: u8) -> Option<Color> {
    Some(Color::from_rgba8(r, g, b, 255))
}

fn boxed(w: impl Widget + 'static) -> WidgetRef {
    Rc::new(RefCell::new(Box::new(w)))
}

fn button_color(w: &WidgetRef) -> Option<Color> {
    let w = w.borrow();
    let b = w.as_any().downcast_ref::<Button>().unwrap();
    b.resolved_style().color
}

fn frame_color(w: &WidgetRef) -> Option<Color> {
    let w = w.borrow();
    let f = w.as_any().downcast_ref::<Frame>().unwrap();
    f.resolved_style().color
}

#[test]
fn focus_moves_the_focus_rule_between_widgets() {
    let root = boxed(EmptyWidget::with_geometry(Rect::new(0, 0, 100, 100)));
    root.borrow().set_style_sheet(SHEET);
    let a = boxed(Button::new("a"));
    let b = boxed(Button::new("b"));
    let mut frame = Frame::new();
    frame.base.object_data.set_object_name("F");
    frame.base.set_focus_policy(FocusPolicy::StrongFocus);
    let f = boxed(frame);
    let mut layout = BoxLayout::vertical();
    for w in [&a, &b, &f] {
        layout.add_widget(w.clone());
    }
    root.borrow_mut().set_layout(Box::new(layout));
    adopt_tree(&root);

    let colors = || (button_color(&a), button_color(&b), frame_color(&f));
    let mut fm = FocusManager::new();
    assert_eq!(colors(), (rgb(0, 255, 0), rgb(0, 255, 0), None));

    let id = a.borrow().id();
    assert!(fm.set_focus(&root, Some(id), FocusReason::Other));
    assert_eq!(colors(), (rgb(255, 0, 0), rgb(0, 255, 0), None));

    let id = b.borrow().id();
    assert!(fm.set_focus(&root, Some(id), FocusReason::Other));
    assert_eq!(colors(), (rgb(0, 255, 0), rgb(255, 0, 0), None));

    let id = f.borrow().id();
    assert!(fm.set_focus(&root, Some(id), FocusReason::Other));
    assert_eq!(colors(), (rgb(0, 255, 0), rgb(0, 255, 0), rgb(0, 0, 255)));

    fm.clear_focus(&root, FocusReason::Other);
    assert_eq!(colors(), (rgb(0, 255, 0), rgb(0, 255, 0), None));
}
