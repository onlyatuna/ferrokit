// SPDX-License-Identifier: LGPL-3.0-only
//! Label focus policy: `QLabel` keeps `QWidget`'s `NoFocus` default, stores whatever
//! `setFocusPolicy` is given, and joins the Tab chain once the policy has `TabFocus`.
//!
//! `QWidget::setFocusPolicy` stores the policy (qwidget.cpp:7918-7920; default 0 = NoFocus at
//! qwidget.cpp:974); Tab navigation takes a candidate only if `focusPolicy() & Qt::TabFocus`
//! (qwidget.cpp:12365).
//! Reference: PySide6 6.11.2, a `QPushButton` then a `QLabel`: `focusPolicy()` is 0; from the
//! focused button, `focusNextChild()` stays on the button; after `setFocusPolicy(StrongFocus)`,
//! `focusPolicy()` is 11 and `focusNextChild()` lands on the label.

use ferrokit_core::event::FocusReason;
use ferrokit_gui::geometry::primitives::Rect;
use ferrokit_widgets::*;
use std::cell::RefCell;
use std::rc::Rc;

fn boxed(w: impl Widget + 'static) -> WidgetRef {
    Rc::new(RefCell::new(Box::new(w)))
}

#[test]
fn a_label_takes_tab_focus_only_after_its_policy_allows_it() {
    let root = boxed(EmptyWidget::with_geometry(Rect::new(0, 0, 100, 100)));
    let b = boxed(Button::new("b"));
    let l = boxed(Label::new("a"));
    let mut layout = BoxLayout::vertical();
    for w in [&b, &l] {
        layout.add_widget(w.clone());
    }
    root.borrow_mut().set_layout(Box::new(layout));
    adopt_tree(&root);

    let mut fm = FocusManager::new();
    assert_eq!(l.borrow().focus_policy(), FocusPolicy::NoFocus);
    let bid = b.borrow().id();
    assert!(fm.set_focus(&root, Some(bid), FocusReason::Other));
    fm.focus_next(&root);
    assert!(
        b.borrow().has_focus(),
        "a NoFocus label is not in the Tab chain"
    );
    assert!(!l.borrow().has_focus());

    l.borrow().set_focus_policy(FocusPolicy::StrongFocus);
    assert_eq!(l.borrow().focus_policy(), FocusPolicy::StrongFocus);
    assert!(fm.focus_next(&root));
    assert!(l.borrow().has_focus(), "Tab moves to a StrongFocus label");
    assert!(!b.borrow().has_focus());
}
