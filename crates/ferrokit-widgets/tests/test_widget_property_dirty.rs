// SPDX-License-Identifier: LGPL-3.0-only
//! `QObject::setProperty` does not repaint or restyle: a style property written to a widget
//! leaves its pending dirty region alone.

use ferrokit_gui::geometry::primitives::Rect;
use ferrokit_widgets::widget::{EmptyWidget, Widget};

#[test]
fn set_property_does_not_dirty_the_widget() {
    let w = EmptyWidget::with_geometry(Rect::new(40, 30, 100, 50));
    w.clear_dirty();
    w.set_property("state", "on");
    assert_eq!(w.dirty_rect(), None);
    assert_eq!(w.property("state").as_deref(), Some("on"));
}

#[test]
fn set_property_keeps_pending_dirt() {
    let w = EmptyWidget::with_geometry(Rect::new(40, 30, 100, 50));
    w.clear_dirty();
    w.widget_base().update_rect(Rect::new(0, 0, 10, 10));
    w.set_property("state", "on");
    assert_eq!(w.dirty_rect(), Some(Rect::new(0, 0, 10, 10)), "only what update() added");
}
