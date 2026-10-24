// SPDX-License-Identifier: LGPL-3.0-only
//! Extreme ranges and steps must not overflow the slider arithmetic (debug builds panic on i32
//! overflow): wheel, page click, thumb drag and thumb geometry all saturate.

use ferrokit_gui::geometry::primitives::{Point, Rect};
use ferrokit_widgets::scroll::{Orientation, ScrollBar};
use ferrokit_widgets::widget::Widget;

fn bar() -> ScrollBar {
    let bar = ScrollBar::new(Orientation::Vertical);
    bar.set_geometry(Rect::new(0, 0, 10, 100));
    bar.set_range(i32::MIN, i32::MAX);
    bar
}

#[test]
fn a_huge_single_step_wheel_saturates() {
    let mut bar = bar();
    bar.set_single_step(i32::MAX);
    bar.set_value(0);
    bar.wheel_event(Point::new(0, 0), -120, 0);
    assert_eq!(bar.value(), i32::MAX);
    bar.wheel_event(Point::new(0, 0), 120, 0);
    bar.wheel_event(Point::new(0, 0), 120, 0);
    bar.wheel_event(Point::new(0, 0), 120, 0);
    assert_eq!(bar.value(), i32::MIN);
}

#[test]
fn page_clicks_and_drag_with_a_full_range_saturate() {
    let mut bar = bar();
    bar.set_page_step(i32::MAX);
    bar.set_value(i32::MAX);
    // Thumb fills the track: press at the very end is inside the thumb and starts a drag.
    bar.mouse_press_event(Point::new(5, 99), 1, 0);
    bar.mouse_move_event(Point::new(5, 100));
    bar.mouse_release_event(Point::new(5, 100), 1, 0);
    bar.set_value(i32::MIN);
    bar.set_page_step(1);
    bar.mouse_press_event(Point::new(5, 99), 1, 0);
    bar.set_value(i32::MAX);
    bar.mouse_press_event(Point::new(5, 0), 1, 0);
    bar.set_value(i32::MIN);
    bar.mouse_press_event(Point::new(5, 99), 1, 0);
}
