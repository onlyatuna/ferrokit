// SPDX-License-Identifier: LGPL-3.0-only
use ferrokit_widgets::*;

// Qt: rowStretch/columnStretch/columnMinimumWidth/rowMinimumHeight (qgridlayout.cpp:1506, 1517,
// 1589, 1566) read back the values set by setRowStretch/setColumnStretch/setColumnMinimumWidth/
// setRowMinimumHeight (qgridlayout.cpp:1494, 1540, 1577, 1554).
#[test]
fn grid_stretch_and_minimum_read_back_what_was_set() {
    let mut grid = GridLayout::new();
    grid.set_row_stretch(1, 3);
    grid.set_column_stretch(2, 5);
    grid.set_column_minimum_width(0, 40);
    grid.set_row_minimum_height(2, 18);
    assert_eq!(grid.row_stretch(1), 3);
    assert_eq!(grid.column_stretch(2), 5);
    assert_eq!(grid.column_minimum_width(0), 40);
    assert_eq!(grid.row_minimum_height(2), 18);
}

// Unset entries read 0 (QGridLayoutPrivate expands with zero-initialised entries; see
// qgridlayout.cpp:88-101 and the setters at :90-99).
#[test]
fn grid_unset_stretch_and_minimum_read_zero() {
    let grid = GridLayout::new();
    assert_eq!(grid.row_stretch(0), 0);
    assert_eq!(grid.column_stretch(0), 0);
    assert_eq!(grid.column_minimum_width(0), 0);
    assert_eq!(grid.row_minimum_height(0), 0);
}

// Qt: horizontalSpacing()/verticalSpacing() (qgridlayout.cpp:1093-1101, 1121-1129).
#[test]
fn grid_per_axis_spacing_reads_back() {
    let mut grid = GridLayout::new();
    grid.set_horizontal_spacing(10);
    grid.set_vertical_spacing(4);
    assert_eq!(grid.horizontal_spacing(), 10);
    assert_eq!(grid.vertical_spacing(), 4);
}
