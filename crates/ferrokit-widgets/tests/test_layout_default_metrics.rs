// SPDX-License-Identifier: LGPL-3.0-only
//! Qt default layout margins and spacing.
//! qtbase/src/widgets/kernel/qlayout.cpp:117-129 `QLayoutPrivate::getMargin`: explicit
//! margin if set; else 0 for a nested layout; else the owner style's `PM_Layout*Margin`.
//! qtbase/src/widgets/styles/qcommonstyle.cpp:4732-4750 `PM_LayoutLeft/Top/Right/BottomMargin`
//! 11 for a window, 9 for a child widget; `PM_LayoutHorizontalSpacing` 6.
//! qtbase/src/widgets/kernel/qlayoutengine.cpp:390-401 `qSmartSpacing`: parentless -1,
//! widget parent -> style spacing, parent layout -> `parent->spacing()`.
//! PySide6 6.11.2 (`QT_QPA_PLATFORM=windows`, screen DPR 1.25, logicalDotsPerInch 96):
//! - `pixelMetric(PM_LayoutLeftMargin)` window 11, child 9; `PM_LayoutHorizontalSpacing` 6.
//! - top-level QHBoxLayout, Expanding item, owner 200x100: item (11, 11, 178, 78).
//! - child owner 200x100 inside 300x200 parent: item (9, 9, 182, 82).
//! - outer QVBoxLayout margins 0 spacing 1, unset inner QHBoxLayout, two 50x20 items:
//!   `outer.sizeHint().width() == 101`, `inner.spacing() == 1`; explicit inner 6: 106.

use ferrokit_gui::geometry::primitives::{Margins, Rect, Size};
use ferrokit_widgets::layout::Layout;
use ferrokit_widgets::*;
use std::cell::RefCell;
use std::rc::Rc;

/// A 50x20 item. Its hint is pinned by an explicit minimum: `EmptyWidget::size_hint` falls back to
/// the geometry, which an owning box rewrites to the slot size on activation, so the geometry alone
/// would make the hint depend on layout order. `item_size_hint` takes `max(hint, min)`.
fn item() -> WidgetRef {
    let w: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::with_geometry(
        Rect::new(0, 0, 50, 20),
    ))));
    w.borrow().set_minimum_size(Size::new(50, 20));
    w
}

/// A 50x20 item whose hint is fixed: min and max both 50x20 cap the geometry-based hint, so it stays
/// 50 wide however the owning box sizes it. Used only where the test reads a size hint, not a position
/// (a bounded item is centred in a wider slot, so position tests keep `item()`).
fn fixed_item() -> WidgetRef {
    let w = item();
    w.borrow().set_maximum_size(Size::new(50, 20));
    w
}

/// An owner widget of the given size; a window when it has no parent.
fn owner(width: i32, height: i32) -> WidgetRef {
    Rc::new(RefCell::new(Box::new(EmptyWidget::with_geometry(
        Rect::new(0, 0, width, height),
    ))))
}

#[test]
fn top_level_owner_default_margin_is_11() {
    // PySide6: window `PM_LayoutLeftMargin` 11; item (11, 11, 178, 78) in a 200x100 owner.
    let top = owner(200, 100);
    let it = item();
    let mut l = BoxLayout::horizontal();
    l.add_widget(it.clone());
    top.borrow_mut().set_layout(Box::new(l));
    top.borrow().update_layout();
    let g = it.borrow().geometry();
    assert_eq!(
        (g.x, g.y),
        (11, 11),
        "top-level default margin (PySide6: 11)"
    );
}

#[test]
fn child_owner_default_margin_is_9() {
    // PySide6: child `PM_LayoutLeftMargin` 9; item (9, 9, 182, 82) in a 200x100 child of 300x200.
    let parent = owner(300, 200);
    let child = owner(200, 100);
    child
        .borrow()
        .set_parent_widget(Some(Rc::downgrade(&parent)));
    let it = item();
    let mut l = BoxLayout::horizontal();
    l.add_widget(it.clone());
    child.borrow_mut().set_layout(Box::new(l));
    child.borrow().update_layout();
    let g = it.borrow().geometry();
    assert_eq!(
        (g.x, g.y),
        (9, 9),
        "child-owner default margin (PySide6: 9)"
    );
}

#[test]
fn nested_layout_default_margin_is_0() {
    // Qt: a nested layout's margin is 0 (qlayout.cpp:117-129); the holder sits at the outer's 11.
    let top = owner(200, 100);
    let it = item();
    let mut inner = BoxLayout::horizontal();
    inner.add_widget(it.clone());
    let mut outer = BoxLayout::horizontal();
    let holder = outer.add_layout(Box::new(inner), 0);
    top.borrow_mut().set_layout(Box::new(outer));
    top.borrow().update_layout();
    let h = holder.borrow().geometry();
    assert_eq!(
        (h.x, h.y),
        (11, 11),
        "holder placed by the owner's margin (PySide6: 11)"
    );
    let g = it.borrow().geometry();
    // Nested items are holder-relative in ferrokit (residual: Qt is owner-relative, which would give the
    // absolute (11,11) = holder (11,11) + item (0,0)). Nested margin 0 puts the item at holder origin.
    assert_eq!(
        (g.x, g.y),
        (0, 0),
        "nested item at holder origin, nested margin 0"
    );
}

#[test]
fn explicit_margins_win_over_owner_default() {
    // Qt: a user margin >= 0 is used as is (qlayout.cpp:117-129).
    let top = owner(200, 100);
    let it = item();
    let mut l = BoxLayout::horizontal();
    l.set_margins(Margins::new(0, 0, 0, 0));
    l.add_widget(it.clone());
    top.borrow_mut().set_layout(Box::new(l));
    top.borrow().update_layout();
    let g = it.borrow().geometry();
    assert_eq!((g.x, g.y), (0, 0), "explicit set_margins(0) wins over 11");
}

#[test]
fn owned_box_spacing_is_style_6() {
    // Qt: owned layout spacing = PM_LayoutHorizontalSpacing 6 (qcommonstyle.cpp:4732-4750).
    // Size hint of an owned box of two 50x20 items: 11 + 50 + 6 + 50 + 11 = 128.
    let top = owner(200, 100);
    let mut l = BoxLayout::horizontal();
    l.add_widget(fixed_item());
    l.add_widget(fixed_item());
    top.borrow_mut().set_layout(Box::new(l));
    top.borrow().update_layout();
    let hint = top.borrow().layout_ref_mut().unwrap().size_hint();
    assert_eq!(
        hint.width, 128,
        "owned spacing 6 between two 50px items inside 11px margins (PySide6 style 6)"
    );
}

#[test]
fn owned_grid_spacing_is_style_6() {
    // Qt: an owned `QGridLayout` asks the style for both spacings (qgridlayout.cpp:1093-1129), 6.
    // PySide6: two 50x20 items in a top-level owner give hint
    // (128, 42) side by side (11 + 50 + 6 + 50 + 11) and (72, 68) stacked (11 + 20 + 6 + 20 + 11).
    for (horizontal, expected) in [(true, Size::new(128, 42)), (false, Size::new(72, 68))] {
        let top = owner(200, 100);
        let mut l = GridLayout::new();
        l.add_widget(fixed_item(), 0, 0);
        if horizontal {
            l.add_widget(fixed_item(), 0, 1);
        } else {
            l.add_widget(fixed_item(), 1, 0);
        }
        top.borrow_mut().set_layout(Box::new(l));
        top.borrow().update_layout();
        let hint = top.borrow().layout_ref_mut().unwrap().size_hint();
        assert_eq!(hint, expected, "owned grid, horizontal={horizontal}");
    }
}

#[test]
fn parentless_box_spacing_is_minus_one() {
    // Qt: `QLayout::spacing()` with no owner or parent layout is -1 (qlayoutengine.cpp:390-401).
    let l = BoxLayout::horizontal();
    assert_eq!(Layout::spacing(&l), -1);
}

#[test]
fn sublayout_inherits_parent_spacing() {
    // Qt: unset nested spacing = parent's effective spacing (qlayoutengine.cpp:390-401).
    let mut outer = BoxLayout::vertical();
    outer.set_margins(Margins::new(0, 0, 0, 0));
    outer.set_spacing(1);
    let mut inner = BoxLayout::horizontal(); // no set_spacing: inherits 1
    inner.add_widget(fixed_item());
    inner.add_widget(fixed_item());
    outer.add_layout(Box::new(inner), 0);
    assert_eq!(
        outer.size_hint().width,
        101,
        "nested spacing inherits outer 1: 50 + 1 + 50 (PySide6: 101)"
    );
}

#[test]
fn explicit_inner_spacing_wins_over_parent() {
    // PySide6: explicit inner spacing 6 gives 106 (the parent's 1 is not used).
    let mut outer = BoxLayout::vertical();
    outer.set_margins(Margins::new(0, 0, 0, 0));
    outer.set_spacing(1);
    let mut inner = BoxLayout::horizontal();
    inner.set_spacing(6);
    inner.add_widget(fixed_item());
    inner.add_widget(fixed_item());
    outer.add_layout(Box::new(inner), 0);
    assert_eq!(
        outer.size_hint().width,
        106,
        "explicit inner 6 wins: 50 + 6 + 50 (PySide6: 106)"
    );
}
