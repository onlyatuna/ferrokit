// SPDX-License-Identifier: LGPL-3.0-only
//! `QScrollArea` input: a bar that is no longer needed puts the content back at the origin, a
//! wheel that moves nothing is left to the parent, horizontal wheels use their own axis, and
//! the scroll bars take mouse presses (`QAbstractScrollArea` scroll bar widgets are children of
//! the area, `qabstractscrollarea.cpp:254-266`).

use ferrokit_core::event::{Event, EventKind, MouseButtons};
use ferrokit_gui::geometry::primitives::{Point, Rect};
use ferrokit_widgets::hit_test::{hit_path, EventTreeDispatcher};
use ferrokit_widgets::scroll::{ScrollArea, ScrollBarPolicy};
use ferrokit_widgets::widget::{EmptyWidget, Widget, WidgetRef};
use std::cell::RefCell;
use std::rc::Rc;

fn area(area_w: i32, area_h: i32, content_w: i32, content_h: i32) -> (WidgetRef, WidgetRef) {
    let mut a = ScrollArea::new();
    a.set_geometry(Rect::new(0, 0, area_w, area_h));
    let content: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::with_geometry(
        Rect::new(0, 0, content_w, content_h),
    ))));
    a.set_widget(Rc::clone(&content));
    let a: WidgetRef = Rc::new(RefCell::new(Box::new(a)));
    content.borrow().set_parent_widget(Some(Rc::downgrade(&a)));
    (a, content)
}

fn with_area<R>(a: &WidgetRef, f: impl FnOnce(&mut ScrollArea) -> R) -> R {
    f(a.borrow_mut().as_any_mut().downcast_mut::<ScrollArea>().unwrap())
}

fn wheel(a: &WidgetRef, angle_x: i32, angle_y: i32) -> bool {
    let mut ev = Event::new_spontaneous(EventKind::Wheel {
        x: 5,
        y: 5,
        pixel_delta_x: 0,
        pixel_delta_y: 0,
        angle_delta_x: angle_x,
        angle_delta_y: angle_y,
        modifiers: 0,
    });
    a.borrow_mut().event(&mut ev)
}

#[test]
fn content_returns_to_the_origin_when_the_bar_is_no_longer_needed() {
    let (a, content) = area(110, 100, 100, 300);
    with_area(&a, |s| s.set_scroll_position(0, 50));
    assert_eq!(content.borrow().geometry().y, -50);

    a.borrow().set_geometry(Rect::new(0, 0, 110, 400));

    assert_eq!(with_area(&a, |s| s.scroll_position()), Point::new(0, 0));
    assert_eq!(content.borrow().geometry().y, 0);
}

#[test]
fn an_always_off_bar_keeps_the_content_at_the_origin() {
    let (a, content) = area(110, 100, 100, 300);
    with_area(&a, |s| s.set_scroll_position(0, 50));
    with_area(&a, |s| s.set_vertical_scrollbar_policy(ScrollBarPolicy::AlwaysOff));
    assert_eq!(content.borrow().geometry().y, 0);
    assert!(!wheel(&a, 0, -120), "nothing to scroll: the parent gets the wheel");
}

#[test]
fn a_wheel_that_moves_nothing_is_not_handled() {
    let (a, _content) = area(110, 100, 100, 300);
    assert!(!wheel(&a, 0, 0), "a zero delta scrolls nothing");
    assert!(!wheel(&a, 0, 120), "already at the top: scrolling up moves nothing");
    assert!(wheel(&a, 0, -120));
    assert_eq!(with_area(&a, |s| s.scroll_position()).y, 3);
}

#[test]
fn a_horizontal_wheel_uses_its_own_delta() {
    let (a, _content) = area(100, 100, 300, 300);
    assert!(wheel(&a, -120, 0));
    assert_eq!(with_area(&a, |s| s.scroll_position()), Point::new(3, 0));
    assert!(wheel(&a, 120, 0));
    assert_eq!(with_area(&a, |s| s.scroll_position()), Point::new(0, 0));
    assert!(!wheel(&a, 120, 0), "at the left limit");
}

#[test]
fn a_click_on_the_scroll_bar_reaches_the_bar_not_the_content() {
    let (a, content) = area(110, 100, 100, 300);
    // The bar column is outside the viewport the content is clipped to.
    let path = hit_path(&a, Point::new(105, 90));
    assert_eq!(path.len(), 1, "the hit stops at the scroll area");

    let mut d = EventTreeDispatcher::new();
    let mut press = Event::new_spontaneous(EventKind::MouseButtonPress { x: 105, y: 90, button: 1 });
    d.dispatch_event(&a, &mut press);
    assert_eq!(with_area(&a, |s| s.scroll_position()).y, 100, "page down");
    assert_eq!(content.borrow().geometry().y, -100);
    let mut release = Event::new_spontaneous(EventKind::MouseButtonRelease { x: 105, y: 90, button: 1 });
    d.dispatch_event(&a, &mut release);
}

#[test]
fn dragging_the_thumb_scrolls_the_content() {
    let (a, content) = area(110, 100, 100, 300);
    let mut d = EventTreeDispatcher::new();
    let mut press = Event::new_spontaneous(EventKind::MouseButtonPress { x: 105, y: 5, button: 1 });
    d.dispatch_event(&a, &mut press);
    d.dispatch_mouse_move(&a, Point::new(105, 38), Point::new(105, 38), MouseButtons(1));
    let y = with_area(&a, |s| s.scroll_position()).y;
    assert!(y > 0, "the thumb moved the content: {y}");
    assert_eq!(content.borrow().geometry().y, -y);
}
