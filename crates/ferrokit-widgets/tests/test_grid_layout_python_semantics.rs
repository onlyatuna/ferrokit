// SPDX-License-Identifier: LGPL-3.0-only
use ferrokit_widgets::*;

// Qt: QGridLayout::spacing() returns -1 unless horizontal == vertical (qgridlayout.cpp:1150-1158).
#[test]
fn grid_spacing_is_minus_one_when_axes_differ() {
    let mut grid = GridLayout::new();
    grid.set_horizontal_spacing(10);
    grid.set_vertical_spacing(4);
    assert_eq!(grid.spacing(), -1);
}

// Pinned: equal axes return the common value (qgridlayout.cpp:1150-1158).
#[test]
fn grid_spacing_is_common_value_when_axes_equal() {
    let mut grid = GridLayout::new();
    grid.set_spacing(4);
    assert_eq!(grid.spacing(), 4);
}
