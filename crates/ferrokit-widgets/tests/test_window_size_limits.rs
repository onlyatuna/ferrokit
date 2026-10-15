// SPDX-License-Identifier: LGPL-3.0-only
//! A top-level `Window`'s size limits: what `set_maximum_size`, `set_fixed_size` and the root
//! layout's size constraint do to the window's minimum, maximum and geometry
//! (`QWidget::setMaximumSize`/`setFixedSize`, qwidget.cpp:4065-4170; `QLayout::activate`,
//! qlayout.cpp:996-1129).
//!
//! The limits are read back from the window, which keeps them in its platform window; what the
//! window system does with them is covered by `ferrokit-platform/tests/test_window_size_limits.rs`.
//! `FRAMELESS` windows, so a native frame does not change the client size.

use ferrokit_gui::geometry::primitives::{Rect, Size};
use ferrokit_platform::size_limits::WINDOW_SIZE_MAX;
use ferrokit_platform::WindowFlags;
use ferrokit_widgets::{BoxLayout, EmptyWidget, Layout, SizeConstraint, WidgetRef, Window};
use std::cell::RefCell;
use std::rc::Rc;

fn window(rect: Rect) -> Window {
    Window::new("size limits", rect, WindowFlags::FRAMELESS).expect("window")
}

/// A root widget with a horizontal layout over one child whose maximum size is 300x80.
fn root_with_bounded_child(constraint: SizeConstraint) -> (WidgetRef, Size) {
    let root: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::new())));
    let child: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::new())));
    child.borrow().set_maximum_size(Size::new(300, 80));
    let mut layout = BoxLayout::horizontal();
    layout.add_widget(child);
    layout.set_size_constraint(constraint);
    root.borrow_mut().set_layout(Box::new(layout));
    let layout_maximum = root
        .borrow()
        .layout_ref_mut()
        .expect("layout")
        .maximum_size();
    (root, layout_maximum)
}

fn size_of(w: &Window) -> (i32, i32) {
    let g = w.geometry();
    (g.width, g.height)
}

#[test]
fn a_layout_maximum_becomes_the_windows_maximum_and_bounds_its_size() {
    let (root, layout_max) = root_with_bounded_child(SizeConstraint::SetMaximumSize);
    assert!(
        layout_max.width < 600 && layout_max.height < 400,
        "the layout bound is the limit"
    );
    let mut win = window(Rect::new(100, 100, 600, 400));
    win.set_root_widget(root);
    win.show();

    assert_eq!(win.maximum_size(), (layout_max.width, layout_max.height));
    assert_eq!(
        size_of(&win),
        (layout_max.width, layout_max.height),
        "shrunk to the maximum"
    );
}

#[test]
fn a_window_keeps_its_own_maximum_when_the_layout_has_none() {
    // `SetDefaultConstraint` sets no maximum: the maximum the window was given stays, and the
    // window is brought inside it like it is brought above its minimum.
    let (root, _) = root_with_bounded_child(SizeConstraint::SetDefaultConstraint);
    let mut win = window(Rect::new(100, 100, 800, 600));
    win.set_maximum_size(500, 300);
    win.set_root_widget(root);
    win.show();

    assert_eq!(win.maximum_size(), (500, 300));
    assert_eq!(size_of(&win), (500, 300));
}

#[test]
fn the_layouts_maximum_replaces_the_one_the_window_was_given() {
    // `QLayout::activate` calls `setMaximumSize(totalMaximumSize())` whatever it was
    // (qlayout.cpp:1096-1122).
    let (root, layout_max) = root_with_bounded_child(SizeConstraint::SetMaximumSize);
    let mut win = window(Rect::new(100, 100, 200, 50));
    win.set_maximum_size(1000, 1000);
    win.set_root_widget(root);
    win.show();

    assert_eq!(win.maximum_size(), (layout_max.width, layout_max.height));
}

#[test]
fn a_stored_maximum_is_kept_between_zero_and_no_maximum() {
    let mut win = window(Rect::new(0, 0, 100, 100));
    assert_eq!(
        win.maximum_size(),
        (WINDOW_SIZE_MAX, WINDOW_SIZE_MAX),
        "none by default"
    );

    win.set_maximum_size(-5, i32::MAX);
    assert_eq!(win.maximum_size(), (0, WINDOW_SIZE_MAX));

    win.set_maximum_size(WINDOW_SIZE_MAX, WINDOW_SIZE_MAX);
    assert_eq!(
        win.maximum_size(),
        (WINDOW_SIZE_MAX, WINDOW_SIZE_MAX),
        "removed again"
    );
}

#[test]
fn a_fixed_size_sets_both_limits_and_resizes() {
    let mut win = window(Rect::new(10, 20, 400, 300));
    win.set_fixed_size(200, 150);

    assert_eq!(win.minimum_size(), (200, 150));
    assert_eq!(win.maximum_size(), (200, 150));
    let g = win.geometry();
    assert_eq!(
        (g.x, g.y, g.width, g.height),
        (10, 20, 200, 150),
        "same position, fixed size"
    );
}

#[test]
fn a_fixed_size_that_changes_nothing_does_not_resize() {
    // qwidget.cpp:4160-4161: when neither limit changed the call returns before `resize`.
    let resizes = Rc::new(std::cell::Cell::new(0));
    let mut win = window(Rect::new(10, 20, 400, 300));
    win.set_fixed_size(200, 150);
    let seen = Rc::clone(&resizes);
    win.set_resize_handler(move |_| seen.set(seen.get() + 1));

    win.set_fixed_size(200, 150);

    assert_eq!(resizes.get(), 0);
    assert_eq!(size_of(&win), (200, 150));
}

#[test]
fn a_fixed_size_of_no_maximum_unlocks_the_window_without_resizing() {
    // qwidget.cpp:3947-3950 reads a minimum of `QWIDGETSIZE_MAX` as 0, and :4168 does not resize
    // for `QWIDGETSIZE_MAX` on both axes: the Qt idiom for removing a fixed size.
    let mut win = window(Rect::new(10, 20, 400, 300));
    win.set_fixed_size(200, 150);

    win.set_fixed_size(WINDOW_SIZE_MAX, WINDOW_SIZE_MAX);

    assert_eq!(win.minimum_size(), (0, 0));
    assert_eq!(win.maximum_size(), (WINDOW_SIZE_MAX, WINDOW_SIZE_MAX));
    assert_eq!(size_of(&win), (200, 150), "not resized by the unlock");
    win.set_geometry(Rect::new(10, 20, 260, 170));
    assert_eq!(size_of(&win), (260, 170), "and free to resize afterwards");
}

#[test]
fn a_minimum_of_no_maximum_is_no_minimum() {
    // `QWidget::setMinimumSize` reads `QWIDGETSIZE_MAX` as 0 per axis (qwidget.cpp:3947-3950).
    let mut win = window(Rect::new(0, 0, 100, 100));
    win.set_minimum_size(WINDOW_SIZE_MAX, 5);
    assert_eq!(win.minimum_size(), (0, 5));
}

// ---- Programmatic geometry -------------------------------------------------------------------
//
// `WM_GETMINMAXINFO` only limits what the user drags: Windows does not apply it to a programmatic
// resize of a `WS_POPUP` window (a layered popup's style). `QWidget` brings every requested geometry inside
// the limits itself (`setGeometry_sys`, qwidget.cpp:7299-7305: maximum first, then minimum, so the
// minimum wins), and so does `Window::set_geometry`/`set_geometry_silent`.

fn geometry_of(w: &Window) -> (i32, i32, i32, i32) {
    let g = w.geometry();
    (g.x, g.y, g.width, g.height)
}

/// A window with limits 100x80 .. 300x200 at (10, 20).
fn bounded_window() -> Window {
    let mut win = window(Rect::new(10, 20, 200, 150));
    win.set_minimum_size(100, 80);
    win.set_maximum_size(300, 200);
    win
}

#[test]
fn a_geometry_below_the_minimum_above_the_maximum_or_between_is_bounded() {
    let mut win = bounded_window();
    win.set_geometry(Rect::new(10, 20, 50, 30));
    assert_eq!(geometry_of(&win), (10, 20, 100, 80), "below the minimum");
    win.set_geometry(Rect::new(10, 20, 500, 900));
    assert_eq!(geometry_of(&win), (10, 20, 300, 200), "above the maximum");
    win.set_geometry(Rect::new(30, 40, 220, 120));
    assert_eq!(
        geometry_of(&win),
        (30, 40, 220, 120),
        "between: untouched, position kept"
    );
    win.set_geometry(Rect::new(10, 20, 50, 900));
    assert_eq!(
        geometry_of(&win),
        (10, 20, 100, 200),
        "each axis on its own"
    );
}

#[test]
fn only_a_minimum_only_a_maximum_or_neither() {
    let mut win = window(Rect::new(0, 0, 200, 150));
    win.set_geometry(Rect::new(0, 0, 7, 3));
    assert_eq!(size_of(&win), (7, 3), "no limits: as requested");

    win.set_minimum_size(100, 80);
    win.set_geometry(Rect::new(0, 0, 7, 3));
    assert_eq!(size_of(&win), (100, 80), "minimum only");
    win.set_geometry(Rect::new(0, 0, 5000, 4000));
    assert_eq!(size_of(&win), (5000, 4000), "minimum only: no upper bound");

    win.set_minimum_size(0, 0);
    win.set_maximum_size(300, 200);
    win.set_geometry(Rect::new(0, 0, 5000, 4000));
    assert_eq!(size_of(&win), (300, 200), "maximum only");
    win.set_geometry(Rect::new(0, 0, 7, 3));
    assert_eq!(size_of(&win), (7, 3), "maximum only: no lower bound");
}

#[test]
fn a_maximum_below_the_minimum_gives_the_minimum_on_that_axis() {
    // qwidget.cpp:7301-7304: `qMin` with the maximum, then `qMax` with the minimum.
    let mut win = window(Rect::new(0, 0, 200, 150));
    win.set_minimum_size(200, 100);
    win.set_maximum_size(150, 300);
    for requested in [(50, 50), (500, 500), (175, 175)] {
        win.set_geometry(Rect::new(0, 0, requested.0, requested.1));
        let h = requested.1.clamp(100, 300);
        assert_eq!(size_of(&win), (200, h), "requested {requested:?}");
    }

    let mut win = window(Rect::new(0, 0, 200, 150));
    win.set_minimum_size(100, 200);
    win.set_maximum_size(300, 150);
    win.set_geometry(Rect::new(0, 0, 50, 500));
    assert_eq!(size_of(&win), (100, 200), "the same on the other axis");
}

#[test]
fn the_silent_path_applies_the_limits_and_stays_silent() {
    let calls = Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut win = bounded_window();
    let seen = Rc::clone(&calls);
    win.set_resize_handler(move |s| seen.borrow_mut().push((s.width, s.height)));

    win.set_geometry_silent(Rect::new(10, 20, 500, 10));
    assert_eq!(geometry_of(&win), (10, 20, 300, 80));
    assert!(calls.borrow().is_empty(), "no resize notification");

    // The loud path reports the size the window really got, not the one requested.
    win.set_geometry(Rect::new(10, 20, 500, 10));
    assert_eq!(
        geometry_of(&win),
        (10, 20, 300, 80),
        "silent already applied it: no change"
    );
    win.set_geometry(Rect::new(10, 20, 50, 900));
    assert_eq!(*calls.borrow(), vec![(100, 200)]);
}

#[test]
fn the_platform_window_gets_the_bounded_size() {
    let mut win = bounded_window();
    for set in [Window::set_geometry, Window::set_geometry_silent] {
        set(&mut win, Rect::new(10, 20, 900, 900));
        let g = win.geometry();
        let dpr = win.device_pixel_ratio();
        let native = win.physical_geometry();
        assert_eq!(
            (native.width, native.height),
            (
                (g.width as f32 * dpr).round() as i32,
                (g.height as f32 * dpr).round() as i32
            ),
            "the native size is the bounded one"
        );
        assert_eq!((g.width, g.height), (300, 200));
    }
}

#[test]
fn a_window_that_layout_sync_and_geometry_agree_on_is_stable_when_the_limits_conflict() {
    // The window's own maximum (50x50) is below the layout's minimum: the minimum wins in
    // `sync_top_level_minimum` and in `set_geometry` alike, so repeated renders do not move it.
    let root: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::new())));
    let child: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::new())));
    child.borrow().set_minimum_size(Size::new(120, 40));
    let mut layout = BoxLayout::horizontal();
    layout.add_widget(child);
    root.borrow_mut().set_layout(Box::new(layout));
    let mut win = window(Rect::new(100, 100, 400, 300));
    win.set_maximum_size(50, 50);
    win.set_root_widget(root);
    win.show();

    let min = win.minimum_size();
    assert!(
        min.0 > 50 && min.1 > 40,
        "the layout minimum is above the window's maximum"
    );
    assert_eq!(size_of(&win), min, "the minimum wins");
    for _ in 0..3 {
        win.render_and_present();
        assert_eq!(size_of(&win), min, "stable");
    }
}

#[test]
fn a_fixed_size_still_resizes_to_exactly_that_size() {
    let mut win = window(Rect::new(10, 20, 400, 300));
    win.set_fixed_size(200, 150);
    assert_eq!(geometry_of(&win), (10, 20, 200, 150));
    win.set_geometry(Rect::new(10, 20, 999, 999));
    assert_eq!(
        geometry_of(&win),
        (10, 20, 200, 150),
        "a fixed window cannot be resized in code"
    );
}
