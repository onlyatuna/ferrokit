// SPDX-License-Identifier: LGPL-3.0-only
//! `qGeomCalc` (`qlayoutengine.cpp`) on equal-stretch chains, the shape of a `QGridLayout` whose
//! columns all have stretch 1: each entry's smart size hint is its minimum, an entry that needs
//! more than its share keeps its minimum and the others share the rest.
use ferrokit_widgets::layout_engine::{q_geom_calc, LayoutStruct, LAYOUT_SIZE_MAX};

fn stretch_column(min: i32) -> LayoutStruct {
    let mut data = LayoutStruct::default();
    data.init(1, min);
    data.empty = false;
    data.expansive = true;
    data
}

fn sizes(mins: &[i32], space: i32) -> Vec<i32> {
    let mut chain: Vec<LayoutStruct> = mins.iter().map(|&m| stretch_column(m)).collect();
    let n = chain.len();
    q_geom_calc(&mut chain, 0, n, 0, space, 0);
    chain.iter().map(|d| d.size).collect()
}

#[test]
fn items_above_their_share_keep_their_minimum_and_the_rest_fits() {
    // Three equal-stretch columns in 630 px; the first two need 218 and 227 (their shares are 210).
    assert_eq!(sizes(&[218, 227, 175], 630), vec![218, 227, 185]);
}

#[test]
fn giving_one_item_its_minimum_can_push_the_next_over_its_share() {
    // After the first is given 300 the others share 150 each, below the second's 160.
    assert_eq!(sizes(&[300, 160, 0], 600), vec![300, 160, 140]);
}

#[test]
fn equal_stretch_without_minimum_pressure_stays_equal() {
    assert_eq!(sizes(&[0, 0, 0], 600), vec![200, 200, 200]);
}

#[test]
fn below_the_minimums_the_biggest_items_give_way_first() {
    // Qt takes the missing 200 px from the biggest entries: both end at 200, not 300.
    assert_eq!(sizes(&[300, 300], 400), vec![200, 200]);
    assert_eq!(sizes(&[100, 300], 300), vec![100, 200]);
}

#[test]
fn minimums_that_sum_to_the_space_leave_no_spare_pixels() {
    // The table's header columns in PySide6 (109 + 121 + 99 px of cells in 329 px).
    assert_eq!(sizes(&[109, 121, 99], 329), vec![109, 121, 99]);
}

#[test]
fn spare_pixels_go_to_the_start_and_end_spacers_of_an_empty_chain() {
    // A packed header row: [spacer][18 icon][5][86 name][spacer] in 110 px puts the spare pixel in
    // the left spacer (PySide6: icon x 1, name x 24).
    let mut chain = vec![LayoutStruct::default(); 4];
    chain[0].init(1, 0);
    chain[0].expansive = true;
    for (i, (hint, spacing)) in [(18, 5), (86, 0)].into_iter().enumerate() {
        let data = &mut chain[i + 1];
        data.init(0, hint);
        data.empty = false;
        data.spacing = spacing;
    }
    chain[3].init(1, 0);
    chain[3].expansive = true;
    chain[0].maximum_size = LAYOUT_SIZE_MAX;
    q_geom_calc(&mut chain, 0, 4, 0, 110, -1);
    assert_eq!((chain[1].pos, chain[2].pos), (1, 24));
}
