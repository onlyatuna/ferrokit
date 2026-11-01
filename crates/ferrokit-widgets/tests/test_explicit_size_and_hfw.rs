// SPDX-License-Identifier: LGPL-3.0-only
//! Explicit `minimumSize` / `maximumSize` / `fixedSize` on a widget are the limits the
//! layout engine uses (`qSmartMinSize` / `qSmartMaxSize`, qlayoutengine.cpp:309-375), and
//! `QWidget::setMinimumSize` / `setMaximumSize` / `setFixedSize` resize the widget when it violates
//! the new limit (qwidget.cpp:3942-4087, 4155-4170). Policy is not changed by `setFixedSize`.
//!
//! A widget with height-for-width is given the height `heightForWidth(width)`
//! in a vertical box (qboxlayout.cpp:755-764) and in a grid cell on an aligned vertical axis
//! (QWidgetItem::setGeometry, qlayoutitem.cpp:432-474).
//!
//! Reference rectangles: PySide6 6.11.2. Box layouts use margins 0 and spacing 6 with stretch-0
//! spacers at both ends, as in `test_box_layout_add_stretch_zero.rs`.

use ferrokit_core::event::Event;
use ferrokit_core::object::{ObjectData, ObjectId, QObject};
use ferrokit_gui::geometry::primitives::{Margins, Rect, Size};
use ferrokit_widgets::*;
use std::cell::RefCell;
use std::rc::Rc;

struct Probe {
    base: WidgetBase,
    hint: Size,
    /// `hasHeightForWidth` with `heightForWidth(w) = 2 * w`.
    hfw: bool,
}

impl QObject for Probe {
    fn object_data(&self) -> &ObjectData {
        &self.base.object_data
    }
    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.base.object_data)
    }
    fn event(&mut self, _event: &mut Event) -> bool {
        false
    }
}

impl Widget for Probe {
    fn widget_base(&self) -> &WidgetBase {
        &self.base
    }
    fn id(&self) -> ObjectId {
        self.base.object_data.id()
    }
    fn geometry(&self) -> Rect {
        self.base.geometry()
    }
    fn set_geometry(&self, rect: Rect) {
        self.base.set_geometry(rect);
    }
    fn size_hint(&self) -> Size {
        self.hint
    }
    fn minimum_size_hint(&self) -> Size {
        Size::new(0, 0)
    }
    fn is_visible(&self) -> bool {
        self.base.is_visible()
    }
    fn set_visible(&self, visible: bool) {
        self.base.set_visible(visible);
    }
    fn is_enabled(&self) -> bool {
        true
    }
    fn update(&self) {}
    fn dirty_rect(&self) -> Option<Rect> {
        None
    }
    fn clear_dirty(&self) {}
    fn parent_widget(&self) -> Option<WidgetWeak> {
        self.base.parent_widget()
    }
    fn set_parent_widget(&self, parent: Option<WidgetWeak>) {
        self.base.set_parent_widget(parent);
    }
    fn window_id(&self) -> Option<ObjectId> {
        None
    }
    fn set_window_id(&self, _window_id: Option<ObjectId>) {}
    fn children(&self) -> Vec<WidgetRef> {
        Vec::new()
    }
    fn add_child(&mut self, _child: WidgetRef) {}
    fn remove_child(&mut self, _child_id: ObjectId) {}
    fn has_height_for_width(&self) -> bool {
        self.hfw
    }
    fn height_for_width(&self, width: i32) -> i32 {
        if self.hfw { 2 * width } else { -1 }
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

fn probe_with(w: i32, h: i32, policy: Policy, hfw: bool) -> WidgetRef {
    let p = Probe {
        base: WidgetBase::new(),
        hint: Size::new(w, h),
        hfw,
    };
    p.base.set_size_policy(QSizePolicy::new(policy, policy));
    Rc::new(RefCell::new(Box::new(p)))
}

fn probe(w: i32, h: i32, policy: Policy) -> WidgetRef {
    probe_with(w, h, policy, false)
}

fn rect_of(w: &WidgetRef) -> (i32, i32, i32, i32) {
    let g = w.borrow().geometry();
    (g.x, g.y, g.width, g.height)
}

/// Puts `layout` on a `width` x `height` container and lays it out.
fn lay_out(layout: Box<dyn Layout>, width: i32, height: i32) -> WidgetRef {
    let container: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::with_geometry(
        Rect::new(0, 0, width, height),
    ))));
    container.borrow_mut().set_layout(layout);
    container.borrow().update_layout();
    container
}

fn horizontal_with_stretch_zero_ends(w: WidgetRef) -> Box<dyn Layout> {
    let mut layout = BoxLayout::horizontal();
    layout.set_margins(Margins::new(0, 0, 0, 0));
    layout.set_spacing(6);
    layout.add_stretch(0);
    layout.add_widget_with_stretch(w, 0);
    layout.add_stretch(0);
    Box::new(layout)
}

#[test]
fn explicit_minimum_width_widens_the_item_in_a_box() {
    // PySide6: Maximum 50x20 hint, setMinimumSize(80, 20), 300x20 box -> (110, 0, 80, 20).
    let w = probe(50, 20, Policy::Maximum);
    w.borrow().set_minimum_size(Size::new(80, 20));
    let _c = lay_out(horizontal_with_stretch_zero_ends(w.clone()), 300, 20);
    assert_eq!(rect_of(&w), (110, 0, 80, 20));
}

#[test]
fn set_fixed_size_caps_an_expanding_item_and_keeps_its_policy() {
    // PySide6: Expanding 50x20 hint, setFixedSize(40, 20), 300x20 box -> (130, 0, 40, 20).
    let w = probe(50, 20, Policy::Expanding);
    w.borrow().set_fixed_size(Size::new(40, 20));
    let _c = lay_out(horizontal_with_stretch_zero_ends(w.clone()), 300, 20);
    assert_eq!(rect_of(&w), (130, 0, 40, 20));
    assert_eq!(
        w.borrow().size_policy(),
        QSizePolicy::new(Policy::Expanding, Policy::Expanding),
        "setFixedSize must not change the size policy"
    );
}

#[test]
fn set_minimum_size_grows_the_widget_and_set_maximum_size_shrinks_it() {
    // PySide6: a 100x30 widget, setMinimumSize(120, 40) -> 120x40 (qwidget.cpp:4005-4028).
    let w1 = probe(50, 20, Policy::Preferred);
    assert_eq!(rect_of(&w1), (0, 0, 100, 30));
    w1.borrow().set_minimum_size(Size::new(120, 40));
    assert_eq!(rect_of(&w1), (0, 0, 120, 40));
    assert_eq!(w1.borrow().minimum_size(), Size::new(120, 40));

    // PySide6: another 100x30 widget, setMaximumSize(60, 10) -> 60x10 (qwidget.cpp:4065-4087).
    // (If min > max, Qt's setGeometry_sys keeps the minimum size).
    let w2 = probe(50, 20, Policy::Preferred);
    assert_eq!(rect_of(&w2), (0, 0, 100, 30));
    w2.borrow().set_maximum_size(Size::new(60, 10));
    assert_eq!(rect_of(&w2), (0, 0, 60, 10));
    assert_eq!(w2.borrow().maximum_size(), Size::new(60, 10));
}

#[test]
fn set_fixed_size_sets_minimum_and_maximum_and_resizes() {
    // setFixedSize(40, 20) on a 100x30 widget -> 40x20, min == max == 40x20 (qwidget.cpp:4155-4170).
    let w = probe(50, 20, Policy::Preferred);
    w.borrow().set_fixed_size(Size::new(40, 20));
    assert_eq!(rect_of(&w), (0, 0, 40, 20));
    assert_eq!(w.borrow().minimum_size(), Size::new(40, 20));
    assert_eq!(w.borrow().maximum_size(), Size::new(40, 20));
}
#[test]
fn set_geometry_clamps_to_explicit_minimum_and_maximum_size() {
    // PySide6: a widget with setMinimumSize(50, 50) and setMaximumSize(150, 150):
    // setGeometry(10, 10, 20, 20) -> (10, 10, 50, 50)
    // setGeometry(10, 10, 200, 200) -> (10, 10, 150, 150)
    // (qwidget.cpp:7286, 7300-7305).
    let w = probe(50, 20, Policy::Preferred);
    w.borrow().set_minimum_size(Size::new(50, 50));
    w.borrow().set_maximum_size(Size::new(150, 150));

    w.borrow().set_geometry(Rect::new(10, 10, 20, 20));
    assert_eq!(rect_of(&w), (10, 10, 50, 50));

    w.borrow().set_geometry(Rect::new(10, 10, 200, 200));
    assert_eq!(rect_of(&w), (10, 10, 150, 150));
}

#[test]
fn a_vertical_box_gives_a_height_for_width_item_the_height_for_its_width() {
    // PySide6 (40x50 TopToBottom box, spacing 6): A = Preferred hint 40x20 with
    // heightForWidth(w) = 2w; B = Maximum 40x20. With HFW: A (0, 0, 40, 44), B (0, 50, 40, 0).
    // Without HFW the same layout gives A (0, 0, 40, 24), B (0, 30, 40, 20).
    let a = probe_with(40, 20, Policy::Preferred, true);
    let b = probe(40, 20, Policy::Maximum);
    let mut layout = BoxLayout::vertical();
    layout.set_margins(Margins::new(0, 0, 0, 0));
    layout.set_spacing(6);
    layout.add_widget_with_stretch(a.clone(), 0);
    layout.add_widget_with_stretch(b.clone(), 0);
    let _c = lay_out(Box::new(layout), 40, 50);
    assert_eq!(rect_of(&a), (0, 0, 40, 44));
    assert_eq!(rect_of(&b), (0, 50, 40, 0));
}

#[test]
fn a_grid_cell_on_an_aligned_vertical_axis_takes_height_for_width() {
    // PySide6: AlignLeft|AlignVCenter, hint 40x20, heightForWidth(w) = 2w, 100x100 grid
    // -> (0, 10, 40, 80).
    let w = probe_with(40, 20, Policy::Preferred, true);
    let mut grid = GridLayout::new();
    grid.set_margins(Margins::new(0, 0, 0, 0));
    grid.add_widget_aligned(w.clone(), 0, 0, 1, 1, Alignment::LEFT | Alignment::V_CENTER);
    let _c = lay_out(Box::new(grid), 100, 100);
    assert_eq!(rect_of(&w), (0, 10, 40, 80));
}
