// SPDX-License-Identifier: LGPL-3.0-only
//! A child's handler hides or shows its own parent (`QWidget::hide()` from a button slot). The
//! child is mutably borrowed by its event delivery; the Show/Hide walk over the children must skip
//! it instead of panicking.

#[path = "support/probe_widget.rs"]
mod probe_widget;

use ferrokit_gui::geometry::primitives::Rect;
use ferrokit_widgets::widget::{EmptyWidget, WidgetRef, set_widget_visible};
use probe_widget::{Log, ProbeWidget};
use std::cell::RefCell;
use std::rc::Rc;

#[test]
fn hiding_and_showing_a_parent_from_a_child_handler_does_not_panic() {
    let log = Log::default();
    let child = ProbeWidget::new(&log).into_ref();
    let parent: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::with_geometry(
        Rect::new(0, 0, 100, 100),
    ))));
    child
        .borrow()
        .set_parent_widget(Some(Rc::downgrade(&parent)));
    parent.borrow_mut().add_child(Rc::clone(&child));

    let _in_child_handler = child.borrow_mut();
    set_widget_visible(&parent, false);
    set_widget_visible(&parent, true);
}
