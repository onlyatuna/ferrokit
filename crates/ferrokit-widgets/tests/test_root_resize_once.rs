// SPDX-License-Identifier: LGPL-3.0-only
//! A resize of the window reaches the root widget's `resizeEvent` once (`QWidget::setGeometry`
//! sends one `QResizeEvent`), whether it comes from the window system or from `set_geometry`.

#[path = "support/headless.rs"]
mod headless;

use ferrokit_gui::geometry::primitives::{Rect, Size};
use ferrokit_platform::{WindowFlags, WindowSystemEvent};
use ferrokit_widgets::widget::EmptyWidget;
use ferrokit_widgets::window::Window;
use std::cell::RefCell;
use std::rc::Rc;

fn watch_root(win: &Window) -> Rc<RefCell<Vec<(Size, Size)>>> {
    let seen: Rc<RefCell<Vec<(Size, Size)>>> = Rc::default();
    let log = Rc::clone(&seen);
    let root = win.root_widget();
    let b = root.borrow();
    let empty = b
        .as_any()
        .downcast_ref::<EmptyWidget>()
        .expect("the default root is an EmptyWidget");
    empty.set_resize_handler(move |new, old| log.borrow_mut().push((new, old)));
    seen
}

#[test]
fn a_native_resize_calls_the_root_resize_handler_once() {
    let _g = headless::install();
    let win = Window::new("r", Rect::new(0, 0, 200, 100), WindowFlags::FRAMELESS).unwrap();
    let idx = headless::last_handler();
    let seen = watch_root(&win);

    headless::send(
        idx,
        WindowSystemEvent::Resize {
            size: Size::new(300, 150),
        },
    );

    assert_eq!(*seen.borrow(), [(Size::new(300, 150), Size::new(200, 100))]);
}

#[test]
fn set_geometry_calls_the_root_resize_handler_once() {
    let _g = headless::install();
    let mut win = Window::new("r", Rect::new(0, 0, 200, 100), WindowFlags::FRAMELESS).unwrap();
    let seen = watch_root(&win);

    win.set_geometry(Rect::new(0, 0, 320, 160));

    assert_eq!(seen.borrow().len(), 1, "{:?}", seen.borrow());
    assert_eq!(seen.borrow()[0].0, Size::new(320, 160));
}

#[test]
fn a_new_root_widget_learns_its_initial_size() {
    let _g = headless::install();
    let mut win = Window::new("r", Rect::new(0, 0, 200, 100), WindowFlags::FRAMELESS).unwrap();
    let seen: Rc<RefCell<Vec<(Size, Size)>>> = Rc::default();
    let log = Rc::clone(&seen);
    let root = EmptyWidget::with_geometry(Rect::new(0, 0, 50, 50));
    root.set_resize_handler(move |new, old| log.borrow_mut().push((new, old)));

    win.set_root_widget(Rc::new(RefCell::new(Box::new(root))));

    assert_eq!(*seen.borrow(), [(Size::new(200, 100), Size::new(50, 50))]);
}

#[test]
fn a_programmatic_window_resize_event_reaches_the_root_once() {
    use ferrokit_core::event::{Event, EventKind};
    use ferrokit_core::object::QObject;
    let _g = headless::install();
    let mut win = Window::new("r", Rect::new(0, 0, 200, 100), WindowFlags::FRAMELESS).unwrap();
    let seen = watch_root(&win);

    let mut ev = Event::new_spontaneous(EventKind::Resize {
        width: 260,
        height: 120,
        old_width: 200,
        old_height: 100,
    });
    assert!(win.event(&mut ev));

    assert_eq!(*seen.borrow(), [(Size::new(260, 120), Size::new(200, 100))]);
}
