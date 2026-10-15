// SPDX-License-Identifier: LGPL-3.0-only
use ferrokit_gui::geometry::primitives::{Rect, Size};
use ferrokit_platform::WindowFlags;
use ferrokit_widgets::{BoxLayout, EmptyWidget, Layout, WidgetRef, Window};
use std::cell::RefCell;
use std::rc::Rc;

/// Root widget with a horizontal layout over one child whose minimum size is 120x40.
fn root_with_fixed_child() -> WidgetRef {
    let root: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::new())));
    let child: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::new())));
    child.borrow().set_minimum_size(Size::new(120, 40));
    let mut layout = BoxLayout::horizontal();
    layout.add_widget(child);
    root.borrow_mut().set_layout(Box::new(layout));
    root
}

/// `FRAMELESS` so the native frame does not change the client width: a `NORMAL` window reports
/// `136x40` for a `120x40` `set_geometry`, a platform effect unrelated to the minimum.
fn frameless_window(title: &str, rect: Rect) -> Window {
    Window::new(title, rect, WindowFlags::FRAMELESS).expect("Window creation failed")
}

/// With no explicit top-level minimum, the window minimum follows the root layout
/// minimum, and a window smaller than it grows to it (`qlayout.cpp:1035-1048`, `:1108-1113`).
/// The layout is activated on show (`qlayout.cpp:546-548`), so the window is shown first.
#[test]
fn test_top_level_minimum_follows_root_layout_without_explicit_min() {
    let root = root_with_fixed_child();
    let mut win = frameless_window("Layout Minimum", Rect::new(100, 100, 50, 20));
    win.set_root_widget(root.clone());
    win.show();

    let hint = root.borrow().minimum_size_hint();
    assert!(
        hint.width >= 120 && hint.height >= 40,
        "layout hint must cover the 120x40 child, got {}x{}",
        hint.width,
        hint.height
    );
    assert_eq!(
        win.minimum_size(),
        (hint.width, hint.height),
        "top-level minimum must follow the root layout when no explicit minimum is set"
    );
    let geom = win.geometry();
    assert_eq!(
        (geom.width, geom.height),
        (hint.width, hint.height),
        "a window smaller than the layout minimum must grow to it"
    );
}

/// A hidden window does not activate its layout, so its minimum is not derived
/// until it is shown (Qt activates on show, `qlayout.cpp:546-548`).
#[test]
fn test_hidden_window_does_not_derive_minimum_from_layout() {
    let root = root_with_fixed_child();
    let mut win = frameless_window("Hidden Window", Rect::new(100, 100, 50, 20));
    win.set_root_widget(root.clone());
    win.render_and_present();

    assert_eq!(
        win.minimum_size(),
        (0, 0),
        "a hidden window must not take the layout minimum before show"
    );
    let geom = win.geometry();
    assert_eq!(
        (geom.width, geom.height),
        (50, 20),
        "a hidden window must not grow to the layout minimum before show"
    );
}

/// A layout-derived minimum never shrinks a window that is already larger.
#[test]
fn test_layout_minimum_keeps_larger_toplevel_size() {
    let root = root_with_fixed_child();
    let mut win = frameless_window("Large Window", Rect::new(100, 100, 300, 200));
    win.set_root_widget(root.clone());
    win.show();

    let geom = win.geometry();
    assert_eq!(
        (geom.width, geom.height),
        (300, 200),
        "a window already above the layout minimum must keep its size"
    );
}

/// An explicit minimum (`QWidget::setMinimumSize`) is not overridden by the layout.
/// An explicit width leaves the height on the layout minimum.
#[test]
fn test_explicit_toplevel_min_is_not_overridden_by_layout() {
    let root = root_with_fixed_child();
    let mut win = frameless_window("Explicit Minimum", Rect::new(100, 100, 50, 20));
    win.set_root_widget(root.clone());
    win.set_minimum_size(150, 120);
    win.show();
    assert_eq!(
        win.minimum_size(),
        (150, 120),
        "explicit minimum must win over the layout minimum"
    );

    let width_root = root_with_fixed_child();
    let mut width_only = frameless_window("Explicit Width", Rect::new(100, 100, 50, 20));
    width_only.set_root_widget(width_root.clone());
    width_only.set_minimum_size(150, 0);
    width_only.show();
    let hint = width_root.borrow().minimum_size_hint();
    assert_eq!(
        width_only.minimum_size(),
        (150, hint.height),
        "an explicit width keeps only the width; the height still follows the layout"
    );
}
