// SPDX-License-Identifier: LGPL-3.0-only
//! `QWidget::setEnabled` passes the effective state down the tree
//! (qwidget.cpp:3405-3476): a child under a disabled parent is disabled and cannot be enabled on
//! its own, and a child disabled explicitly stays disabled when the parent comes back. Every
//! widget whose state changes repaints (`changeEvent(EnabledChange)`, qwidget.cpp:9491-9492).
//! Reference states: PySide6 6.11.2, a `QWidget` with two `QLabel`s in a `QVBoxLayout`.

use ferrokit_gui::geometry::primitives::Rect;
use ferrokit_widgets::*;
use std::cell::RefCell;
use std::rc::Rc;

fn widget() -> WidgetRef {
    Rc::new(RefCell::new(Box::new(EmptyWidget::with_geometry(Rect::new(0, 0, 50, 20)))))
}

/// A parent with two children in a layout, linked upwards as layout activation does.
fn tree() -> (WidgetRef, WidgetRef, WidgetRef) {
    let (p, a, b) = (widget(), widget(), widget());
    let mut layout = BoxLayout::vertical();
    layout.add_widget(a.clone());
    layout.add_widget(b.clone());
    p.borrow_mut().set_layout(Box::new(layout));
    adopt_tree(&p);
    (p, a, b)
}

fn state(p: &WidgetRef, a: &WidgetRef, b: &WidgetRef) -> (bool, bool, bool) {
    (p.borrow().is_enabled(), a.borrow().is_enabled(), b.borrow().is_enabled())
}

#[test]
fn disabling_a_parent_disables_its_children_until_it_is_enabled_again() {
    let (p, a, b) = tree();
    set_widget_enabled(&p, false);
    assert_eq!(state(&p, &a, &b), (false, false, false));
    set_widget_enabled(&b, true);
    assert_eq!(state(&p, &a, &b), (false, false, false));
    set_widget_enabled(&b, false);
    set_widget_enabled(&p, true);
    assert_eq!(state(&p, &a, &b), (true, true, false));
    set_widget_enabled(&b, true);
    assert_eq!(state(&p, &a, &b), (true, true, true));
}

#[test]
fn an_explicitly_disabled_child_stays_disabled_across_the_parent_toggling() {
    let (p, a, b) = tree();
    set_widget_enabled(&a, false);
    set_widget_enabled(&p, false);
    set_widget_enabled(&p, true);
    assert_eq!(state(&p, &a, &b), (true, false, true));
}

#[test]
fn a_child_disabled_through_its_parent_repaints() {
    let (p, a, _) = tree();
    a.borrow().clear_dirty();
    set_widget_enabled(&p, false);
    assert!(a.borrow().dirty_rect().is_some());
}
