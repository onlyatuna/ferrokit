// SPDX-License-Identifier: LGPL-3.0-only
//! `WidgetCommandQueue::flush` is called at the end of every dispatch, also a nested one made from
//! a widget's own handler. A layout request for a widget that is borrowed there waits for the next
//! flush instead of panicking.

use ferrokit_gui::geometry::primitives::Rect;
use ferrokit_widgets::command::WidgetCommandQueue;
use ferrokit_widgets::widget::{EmptyWidget, WidgetRef};
use std::cell::RefCell;
use std::rc::Rc;

#[test]
fn a_layout_request_for_a_borrowed_widget_is_kept_for_the_next_flush() {
    WidgetCommandQueue::clear();
    let w: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::with_geometry(Rect::new(0, 0, 10, 10)))));
    WidgetCommandQueue::request_layout(&w);
    {
        let _in_its_handler = w.borrow_mut();
        WidgetCommandQueue::flush();
        assert_eq!(WidgetCommandQueue::pending_count(), 1, "kept");
    }
    WidgetCommandQueue::flush();
    assert_eq!(WidgetCommandQueue::pending_count(), 0);
}
