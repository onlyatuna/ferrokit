// SPDX-License-Identifier: LGPL-3.0-only
//! A `Window` owns its platform window, root widget and the widgets below it; dropping the last
//! owner destroys all of them (`QWidget::~QWidget` deletes its children, `qwidget.cpp`).
//!
//! The platform window owns the window event handler, so the handler must not own the platform
//! window back: that cycle kept every window, its backing store and its widget tree alive for the
//! rest of the process.

use ferrokit_gui::geometry::primitives::Rect;
use ferrokit_platform::WindowFlags;
use ferrokit_widgets::widget::{EmptyWidget, Widget, WidgetRef};
use ferrokit_widgets::window::Window;
use std::cell::RefCell;
use std::rc::Rc;

fn new_window() -> Window {
    Window::new(
        "Drop releases graph",
        Rect::new(100, 100, 200, 150),
        WindowFlags::FRAMELESS,
    )
    .expect("create window")
}

#[test]
fn dropping_a_window_releases_its_root_widget() {
    let win = new_window();
    let root = Rc::downgrade(&win.root_widget());
    assert!(root.upgrade().is_some());

    drop(win);

    assert!(
        root.upgrade().is_none(),
        "the root widget outlived the window"
    );
}

#[test]
fn dropping_a_shown_window_releases_the_whole_widget_tree() {
    let mut win = new_window();
    let root: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::new()) as Box<dyn Widget>));
    let child: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::new()) as Box<dyn Widget>));
    root.borrow_mut().add_child(child.clone());
    win.set_root_widget(root.clone());
    win.show();

    let root_weak = Rc::downgrade(&root);
    let child_weak = Rc::downgrade(&child);
    drop(root);
    drop(child);
    assert!(root_weak.upgrade().is_some());
    assert!(child_weak.upgrade().is_some());

    drop(win);

    assert!(root_weak.upgrade().is_none(), "the root widget outlived the window");
    assert!(child_weak.upgrade().is_none(), "a child outlived its window");
}
