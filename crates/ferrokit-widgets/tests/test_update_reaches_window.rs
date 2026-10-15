// SPDX-License-Identifier: LGPL-3.0-only
//! `WidgetBase::update()` reaches its window's deferred render without the window being
//! registered as a QObject receiver and without any explicit `render_and_present()`.
#![cfg(windows)]

use std::cell::RefCell;
use std::rc::Rc;

use ferrokit_core::event_loop::EventLoop;
use ferrokit_gui::geometry::primitives::Rect;
use ferrokit_platform::WindowFlags;
use ferrokit_widgets::widget::{EmptyWidget, WidgetRef};
use ferrokit_widgets::window::Window;

struct Fixture {
    el: EventLoop,
    win: Box<Window>,
    probe: WidgetRef,
}

fn fixture() -> Fixture {
    let el = EventLoop::new();
    let win = Box::new(
        Window::new(
            "Update Reaches Window",
            Rect::new(0, 0, 300, 200),
            WindowFlags::empty(),
        )
        .expect("create window"),
    );
    let probe: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::with_geometry(
        Rect::new(0, 0, 50, 50),
    ))));
    win.root_widget().borrow_mut().add_child(Rc::clone(&probe));
    Fixture { el, win, probe }
}

#[test]
fn widget_update_schedules_window_render_without_register_or_direct_render() {
    let fx = fixture();
    let before = fx.win.render_stats();

    fx.probe.borrow().update();
    fx.el.process_events(false);

    let after = fx.win.render_stats();
    assert!(
        after.render_count > before.render_count,
        "widget update must reach the window's deferred render"
    );
    assert_eq!(after.direct_render_count, before.direct_render_count);
}

#[test]
fn repeated_widget_updates_coalesce_into_one_render() {
    let fx = fixture();
    // Settle any construction-time work first.
    fx.el.process_events(false);
    let before = fx.win.render_stats().render_count;

    fx.probe.borrow().update();
    fx.probe.borrow().update();
    fx.probe.borrow().update();
    fx.el.process_events(false);

    assert_eq!(
        fx.win.render_stats().render_count,
        before + 1,
        "three updates in one pump must render once"
    );
}

#[test]
fn hidden_widget_update_renders_nothing() {
    let fx = fixture();
    fx.el.process_events(false);
    fx.probe.borrow_mut().set_visible(false);
    fx.el.process_events(false);
    let before = fx.win.render_stats().render_count;

    fx.probe.borrow().update();
    fx.el.process_events(false);

    assert_eq!(fx.win.render_stats().render_count, before);
}

#[test]
fn window_moved_after_construction_still_receives_updates() {
    let fx = fixture();
    fx.el.process_events(false);
    // Moving the window value is safe: nothing holds its address.
    let Fixture { el, win, probe } = fx;
    let win: Window = *win;
    let before = win.render_stats().render_count;

    probe.borrow().update();
    el.process_events(false);

    assert_eq!(win.render_stats().render_count, before + 1);
}
