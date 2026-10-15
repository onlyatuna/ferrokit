// SPDX-License-Identifier: LGPL-3.0-only
//! Moving or resizing a child must repaint its old footprint in the parent, and a child's paint
//! must stay inside its own geometry.
//!
//! Qt: `QWidgetPrivate::setGeometry_sys` (qwidget.cpp:7297-7410) calls `moveRect` (:7380) or
//! `invalidateBackingStore_resizeHelper` (:7382) on the old rect of a visible widget, so the parent
//! repaints the vacated area (`QWidgetRepaintManager::moveRect`, qwidgetrepaintmanager.cpp:382-430).
//! Dirty regions accumulate: a later `update()` adds to the pending one instead of replacing it.
//! `QWidget::paintEvent` output is clipped to the widget's own rect (`clipRect`, qwidget.cpp:1904-1923).
#![cfg(windows)]

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use ferrokit_gui::geometry::primitives::{Rect, RectF};
use ferrokit_gui::tiny_skia::Color;
use ferrokit_platform::WindowFlags;
use ferrokit_widgets::widget::{EmptyWidget, WidgetRef};
use ferrokit_widgets::window::Window;

const BLUE: (u8, u8, u8, u8) = (0, 0, 255, 255);
const RED: (u8, u8, u8, u8) = (255, 0, 0, 255);

fn new_window() -> Box<Window> {
    Box::new(
        Window::new(
            "Geometry Invalidation",
            Rect::new(0, 0, 300, 200),
            WindowFlags::empty(),
        )
        .expect("create window"),
    )
}

/// Reads one backing-store pixel at logical window coordinates.
fn px(win: &Window, x: i32, y: i32) -> (u8, u8, u8, u8) {
    let bs = win.backing_store();
    let dpr = bs.device_pixel_ratio().round() as i32;
    let p = bs
        .pixmap()
        .pixel((x * dpr) as u32, (y * dpr) as u32)
        .expect("pixel inside backing store");
    (p.red(), p.green(), p.blue(), p.alpha())
}

fn rgba(c: (u8, u8, u8, u8)) -> Color {
    Color::from_rgba8(c.0, c.1, c.2, c.3)
}

/// A full-window widget under the window root that fills itself with `fill()` on every paint.
fn container(fill: impl Fn() -> (u8, u8, u8, u8) + 'static) -> WidgetRef {
    let mut c = EmptyWidget::with_geometry(Rect::new(0, 0, 300, 200));
    c.set_paint_handler(move |painter| {
        painter.fill_rect(
            RectF {
                x: 0.0,
                y: 0.0,
                width: 300.0,
                height: 200.0,
            },
            rgba(fill()),
        );
    });
    Rc::new(RefCell::new(Box::new(c)))
}

fn solid_child(rect: Rect, color: (u8, u8, u8, u8)) -> WidgetRef {
    let mut child = EmptyWidget::with_geometry(rect);
    child.set_background_color(Some(rgba(color)));
    Rc::new(RefCell::new(Box::new(child)))
}

#[test]
fn moved_child_leaves_no_stale_pixels_at_its_old_rect() {
    let mut win = new_window();
    let root = win.root_widget();
    let parent = container(|| BLUE);
    root.borrow_mut().add_child(Rc::clone(&parent));
    let child = solid_child(Rect::new(10, 10, 50, 50), RED);
    parent.borrow_mut().add_child(Rc::clone(&child));
    win.render_and_present();
    assert_eq!(px(&win, 20, 20), RED, "child painted at its initial rect");

    // Moving the child dirties its old rect in the parent, so the parent repaints the vacated area.
    child.borrow().set_geometry(Rect::new(100, 100, 50, 50));
    win.render_and_present();

    assert_eq!(px(&win, 20, 20), BLUE, "old rect shows the parent again");
    assert_eq!(px(&win, 110, 110), RED, "child painted at its new rect");
}

#[test]
fn pending_parent_update_survives_child_move() {
    // Regression guard for union semantics: a pending full-parent update must not be replaced
    // by the smaller old-rect invalidation caused by moving a child.
    let mut win = new_window();
    let root = win.root_widget();
    let phase = Rc::new(Cell::new(false));
    let fill_phase = Rc::clone(&phase);
    let parent = container(move || if fill_phase.get() { BLUE } else { RED });
    root.borrow_mut().add_child(Rc::clone(&parent));
    let child = solid_child(Rect::new(10, 10, 50, 50), RED);
    parent.borrow_mut().add_child(Rc::clone(&child));
    win.render_and_present();
    assert_eq!(
        px(&win, 250, 190),
        RED,
        "parent painted red before the change"
    );

    phase.set(true);
    parent.borrow().update();
    child.borrow().set_geometry(Rect::new(100, 100, 50, 50));
    win.render_and_present();

    assert_eq!(
        px(&win, 250, 190),
        BLUE,
        "pending parent update repaints outside child rects"
    );
}

#[test]
fn child_paint_is_clipped_to_its_own_geometry() {
    let mut win = new_window();
    let root = win.root_widget();
    let parent = container(|| BLUE);
    root.borrow_mut().add_child(Rc::clone(&parent));

    // Paints a red rect far larger than the child's 50x50 geometry.
    let mut child = EmptyWidget::with_geometry(Rect::new(10, 10, 50, 50));
    child.set_paint_handler(|painter| {
        painter.fill_rect(
            RectF {
                x: -20.0,
                y: -20.0,
                width: 200.0,
                height: 200.0,
            },
            rgba(RED),
        );
    });
    let child: WidgetRef = Rc::new(RefCell::new(Box::new(child)));
    parent.borrow_mut().add_child(child);
    win.render_and_present();

    assert_eq!(
        px(&win, 70, 70),
        BLUE,
        "child paint stays inside its geometry"
    );
    assert_eq!(
        px(&win, 0, 0),
        BLUE,
        "child paint stays inside its geometry (top-left)"
    );
}
