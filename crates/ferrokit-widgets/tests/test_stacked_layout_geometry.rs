// SPDX-License-Identifier: LGPL-3.0-only
//! `QStackedLayout::setGeometry` (qstackedlayout.cpp:453-468), StackOne mode: only the current
//! widget is placed, with `widget->setGeometry(rect)` on the whole layout rectangle. Contents
//! margins are not applied, other pages keep their geometry, and the widget's own minimum and
//! maximum size clamp the result (`QWidget::setGeometry`). Expected values are from the PySide6
//! 6.11.2 layout oracle (`fixtures/layout_qt_oracle.txt`, `K` records).

use ferrokit_gui::geometry::primitives::{Margins, Rect, Size};
use ferrokit_widgets::layout::Layout;
use ferrokit_widgets::*;
use std::cell::RefCell;
use std::rc::Rc;

fn page(x: i32, y: i32, w: i32, h: i32) -> WidgetRef {
    Rc::new(RefCell::new(Box::new(EmptyWidget::with_geometry(
        Rect::new(x, y, w, h),
    ))))
}

fn geom(w: &WidgetRef) -> (i32, i32, i32, i32) {
    let g = w.borrow().geometry();
    (g.x, g.y, g.width, g.height)
}

#[test]
fn contents_margins_are_not_applied_to_the_current_page() {
    // Oracle: `K 0 6 200 100` -> current page (0,0,200,100) despite margins 6.
    let first = page(0, 0, 60, 20);
    let mut stack = StackedLayout::new();
    stack.add_widget(first.clone());
    stack.set_margins(Margins::new(6, 6, 6, 6));
    Layout::set_geometry(&mut stack, Rect::new(0, 0, 200, 100));
    assert_eq!(geom(&first), (0, 0, 200, 100));
}

#[test]
fn pages_other_than_the_current_one_keep_their_geometry() {
    // Oracle: the second page stays at its own (0,0,100,30) geometry.
    let first = page(0, 0, 60, 20);
    let second = page(0, 0, 100, 30);
    let mut stack = StackedLayout::new();
    stack.add_widget(first.clone());
    stack.add_widget(second.clone());
    Layout::set_geometry(&mut stack, Rect::new(0, 0, 200, 100));
    assert_eq!(geom(&first), (0, 0, 200, 100));
    assert_eq!(geom(&second), (0, 0, 100, 30));
}

#[test]
fn the_current_page_is_clamped_to_its_maximum_size() {
    // Oracle: `K 0 2 316 115`, page max width 125 -> (0,0,125,115).
    let first = page(0, 0, 104, 2);
    first.borrow().set_maximum_size(Size::new(125, 16_777_215));
    let mut stack = StackedLayout::new();
    stack.add_widget(first.clone());
    stack.set_margins(Margins::new(2, 2, 2, 2));
    Layout::set_geometry(&mut stack, Rect::new(0, 0, 316, 115));
    assert_eq!(geom(&first), (0, 0, 125, 115));
}

#[test]
fn switching_pages_places_the_new_current_page() {
    // `QStackedLayout::setCurrentIndex` (qstackedlayout.cpp:275-310) shows the next page; the
    // layout then gives it the whole rectangle.
    let first = page(0, 0, 60, 20);
    let second = page(0, 0, 100, 30);
    let mut stack = StackedLayout::new();
    stack.add_widget(first.clone());
    stack.add_widget(second.clone());
    Layout::set_geometry(&mut stack, Rect::new(0, 0, 200, 100));
    stack.set_current_index(1);
    assert_eq!(geom(&second), (0, 0, 200, 100));
}
