// SPDX-License-Identifier: LGPL-3.0-only
//! StackedLayout set_spacing and spacing.
//!
//! Qt reference:
//! - `QLayout::setSpacing(int spacing)` (`qlayout.cpp:257-262`): stores the value as the inside spacing and invalidates the layout.
//! - `QLayout::spacing()` (`qlayout.cpp:246-255`): returns the stored inside spacing if it is `>= 0`, else smart spacing (-1 when parentless).

use ferrokit_widgets::layout::Layout;
use ferrokit_widgets::stacked::StackedLayout;

#[test]
fn stacked_layout_set_spacing_stores_and_returns_spacing() {
    let mut layout = StackedLayout::new();
    assert_eq!(
        layout.spacing(),
        -1,
        "parentless StackedLayout initial spacing must be -1 (qlayout.cpp:246-255)"
    );
    layout.set_spacing(10);
    assert_eq!(
        layout.spacing(),
        10,
        "StackedLayout::set_spacing must update spacing (qlayout.cpp:257-262)"
    );
}

#[test]
fn stacked_layout_trait_set_spacing_stores_and_returns_spacing() {
    let mut layout: Box<dyn Layout> = Box::new(StackedLayout::new());
    assert_eq!(layout.spacing(), -1);
    layout.set_spacing(15);
    assert_eq!(
        layout.spacing(),
        15,
        "Layout::set_spacing must update spacing on StackedLayout"
    );
}
