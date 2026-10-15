// SPDX-License-Identifier: LGPL-3.0-only
//! Parity tests for `update(const QRect &)` partial update.

use ferrokit_gui::geometry::primitives::Rect;
use ferrokit_widgets::widget::{EmptyWidget, Widget};

#[test]
fn update_rect_accumulates_and_unites_dirty_regions() {
    let w = EmptyWidget::new();
    w.set_geometry(Rect::new(0, 0, 200, 200));
    w.clear_dirty();
    assert_eq!(w.dirty_rect(), None);

    // Initial partial update
    w.update_rect(Rect::new(10, 10, 30, 30));
    assert_eq!(w.dirty_rect(), Some(Rect::new(10, 10, 30, 30)));

    // Second non-overlapping partial update unites bounding box
    w.update_rect(Rect::new(50, 60, 20, 20));
    assert_eq!(w.dirty_rect(), Some(Rect::new(10, 10, 60, 70)));

    // Full update expands to entire geometry
    w.update();
    assert_eq!(w.dirty_rect(), Some(Rect::new(0, 0, 200, 200)));

    w.clear_dirty();
    assert_eq!(w.dirty_rect(), None);
}
