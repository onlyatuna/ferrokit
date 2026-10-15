// SPDX-License-Identifier: LGPL-3.0-only
//! Default size_hint and minimum_size_hint of EmptyWidget and Widget trait.
//!
//! Qt reference:
//! - `QWidget::sizeHint()` (`qwidget.cpp:8880-8893`): returns the layout's total size hint if there is a layout, else `QSize(-1, -1)`.
//! - `QWidget::minimumSizeHint()` (`qwidget.cpp:8906-8919`): returns the layout's total minimum size if there is a layout, else `QSize(-1, -1)`.

use ferrokit_gui::geometry::primitives::Size;
use ferrokit_widgets::widget::{EmptyWidget, Widget};

#[test]
fn empty_widget_without_layout_size_hint_is_invalid() {
    let w = EmptyWidget::new();
    assert_eq!(
        w.size_hint(),
        Size::new(-1, -1),
        "EmptyWidget without layout must have size_hint (-1, -1) (qwidget.cpp:8884)"
    );
}

#[test]
fn empty_widget_without_layout_minimum_size_hint_is_invalid() {
    let w = EmptyWidget::new();
    assert_eq!(
        w.minimum_size_hint(),
        Size::new(-1, -1),
        "EmptyWidget without layout must have minimum_size_hint (-1, -1) (qwidget.cpp:8911)"
    );
}
