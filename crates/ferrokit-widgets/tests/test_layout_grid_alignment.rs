// SPDX-License-Identifier: LGPL-3.0-only
//! Grid `addLayout` / `removeWidget`, layout-level alignment (`QLayout::setAlignment`
//! on the layout itself), nested-layout alignment forwarding, grid height-for-width and the
//! parentless grid spacing.
//!
//! Reference values: PySide6 6.11.2, offscreen. Container is 200x60 with zero margins, as in
//! `test_layout_alignment.rs`. Height-for-width widget: `heightForWidth(w) = 2 * w`.

use ferrokit_core::event::Event;
use ferrokit_core::object::{ObjectData, ObjectId, QObject};
use ferrokit_gui::geometry::primitives::{Margins, Rect, Size};
use ferrokit_widgets::*;
use std::cell::RefCell;
use std::rc::Rc;

struct Probe {
    base: WidgetBase,
    hint: Size,
    min_hint: Size,
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
        self.min_hint
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
        if self.hfw {
            2 * width
        } else {
            -1
        }
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

fn widget_with(w: i32, h: i32, policy: Policy, hfw: bool) -> WidgetRef {
    let p = Probe {
        base: WidgetBase::new(),
        hint: Size::new(w, h),
        min_hint: Size::new(0, 0),
        hfw,
    };
    p.base.set_size_policy(QSizePolicy::new(policy, policy));
    Rc::new(RefCell::new(Box::new(p)))
}
fn widget_with_min(w: i32, h: i32, min_w: i32, min_h: i32, policy: Policy, hfw: bool) -> WidgetRef {
    let p = Probe {
        base: WidgetBase::new(),
        hint: Size::new(w, h),
        min_hint: Size::new(min_w, min_h),
        hfw,
    };
    p.base.set_size_policy(QSizePolicy::new(policy, policy));
    Rc::new(RefCell::new(Box::new(p)))
}

fn fixed(w: i32, h: i32) -> WidgetRef {
    widget_with(w, h, Policy::Fixed, false)
}

fn rect_of(w: &WidgetRef) -> (i32, i32, i32, i32) {
    let g = w.borrow().geometry();
    (g.x, g.y, g.width, g.height)
}

/// Puts `layout` on a 200x60 container (zero margins are the caller's job) and lays it out.
fn lay_out(layout: Box<dyn Layout>) -> WidgetRef {
    let container: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::with_geometry(
        Rect::new(0, 0, 200, 60),
    ))));
    container.borrow_mut().set_layout(layout);
    container.borrow().update_layout();
    container
}

/// `QLayout::maximumSize` of the layout installed on `container`.
fn maximum_of(container: &WidgetRef) -> (i32, i32) {
    let widget = container.borrow();
    let layout = widget.layout_ref_mut().expect("container has a layout");
    let m = layout.maximum_size();
    (m.width, m.height)
}

// ---------------------------------------------------------------------------------------------
// Grid layout-level alignment bounds the maximum size on the aligned axis.
// ---------------------------------------------------------------------------------------------

fn grid_with_one_fixed_leaf(alignment: Option<Alignment>) -> WidgetRef {
    let mut grid = GridLayout::new();
    grid.set_margins(Margins::new(0, 0, 0, 0));
    grid.add_widget(fixed(40, 20), 0, 0);
    if let Some(a) = alignment {
        grid.set_layout_alignment(a);
    }
    lay_out(Box::new(grid))
}

#[test]
fn grid_layout_alignment_unbounds_maximum_on_aligned_axis() {
    // PySide: unaligned maximumSize (40,20); setAlignment(AlignHCenter) (524287,20);
    // setAlignment(AlignVCenter) (40,524287). qgridlayout.cpp:1216
    assert_eq!(maximum_of(&grid_with_one_fixed_leaf(None)), (40, 20));
    assert_eq!(
        maximum_of(&grid_with_one_fixed_leaf(Some(Alignment::H_CENTER))),
        (524287, 20)
    );
    assert_eq!(
        maximum_of(&grid_with_one_fixed_leaf(Some(Alignment::V_CENTER))),
        (40, 524287)
    );
}

// ---------------------------------------------------------------------------------------------
// Per-item alignment in boxes; an aligned vertical box item unbounds its maximum width.
// ---------------------------------------------------------------------------------------------

#[test]
fn fixed_horizontal_box_centres_an_h_center_leaf() {
    // PySide: QHBoxLayout with a fixed 40x20 leaf, addWidget(leaf, 0, AlignHCenter), 200x60 -> (80,20,40,20).
    let leaf = fixed(40, 20);
    let mut layout = BoxLayout::horizontal();
    layout.set_margins(Margins::new(0, 0, 0, 0));
    layout.add_widget_aligned(leaf.clone(), 0, Alignment::H_CENTER);
    let _container = lay_out(Box::new(layout));
    assert_eq!(rect_of(&leaf), (80, 20, 40, 20));
}

#[test]
fn fixed_vertical_box_align_right_unbounds_width_and_places_right() {
    // PySide: QVBoxLayout, addWidget(leaf, 0, AlignRight): maximumSize (524287,20), leaf (160,20,40,20).
    let leaf = fixed(40, 20);
    let mut layout = BoxLayout::vertical();
    layout.set_margins(Margins::new(0, 0, 0, 0));
    layout.add_widget_aligned(leaf.clone(), 0, Alignment::RIGHT);
    let container = lay_out(Box::new(layout));
    assert_eq!(maximum_of(&container), (524287, 20));
    assert_eq!(rect_of(&leaf), (160, 20, 40, 20));
}

// ---------------------------------------------------------------------------------------------
// Nested layouts: `addLayout` alignment and expanding children.
// ---------------------------------------------------------------------------------------------

#[test]
fn box_add_layout_inner_alignment_places_the_child_layout() {
    // PySide: inner = QHBoxLayout(fixed 40x20 leaf); inner.setAlignment(AlignRight|AlignTop);
    // outer.addLayout(inner) in 200x60 -> leaf (160,0,40,20).
    let leaf = fixed(40, 20);
    let mut inner = BoxLayout::horizontal();
    inner.set_margins(Margins::new(0, 0, 0, 0));
    inner.add_widget_with_stretch(leaf.clone(), 0);
    inner.set_layout_alignment(Alignment::RIGHT | Alignment::TOP);
    let mut outer = BoxLayout::horizontal();
    outer.set_margins(Margins::new(0, 0, 0, 0));
    outer.add_layout(Box::new(inner), 0);
    let _container = lay_out(Box::new(outer));
    assert_eq!(rect_of(&leaf), (160, 0, 40, 20));
}

#[test]
fn box_add_layout_without_alignment_fills_the_container_for_an_expanding_leaf() {
    // PySide: outer.addLayout(inner) with an Expanding 40x20 leaf in inner -> leaf (0,0,200,60).
    let leaf = widget_with(40, 20, Policy::Expanding, false);
    let mut inner = BoxLayout::horizontal();
    inner.set_margins(Margins::new(0, 0, 0, 0));
    inner.add_widget_with_stretch(leaf.clone(), 0);
    let mut outer = BoxLayout::horizontal();
    outer.set_margins(Margins::new(0, 0, 0, 0));
    outer.add_layout(Box::new(inner), 0);
    let _container = lay_out(Box::new(outer));
    assert_eq!(rect_of(&leaf), (0, 0, 200, 60));
}

#[test]
fn grid_add_layout_places_the_child_layout_at_its_cell() {
    // PySide: grid(margins 0, spacing 0): fixed 50x20 at (0,0); inner layout with an Expanding
    // 40x20 leaf at (1,1). The child fills column 1 and row 1 -> leaf (50,20,150,40).
    let leaf = widget_with(40, 20, Policy::Expanding, false);
    let mut inner = BoxLayout::horizontal();
    inner.set_margins(Margins::new(0, 0, 0, 0));
    inner.add_widget_with_stretch(leaf.clone(), 0);
    let mut grid = GridLayout::new();
    grid.set_margins(Margins::new(0, 0, 0, 0));
    grid.set_horizontal_spacing(0);
    grid.set_vertical_spacing(0);
    grid.add_widget(fixed(50, 20), 0, 0);
    let holder = grid.add_layout(Box::new(inner), 1, 1, 1, 1, Alignment::NONE);
    let _container = lay_out(Box::new(grid));
    // Nested-layout leaves are relative to their holder (ferrokit residual: Qt makes them
    // container-relative); compare in container coordinates.
    let h = holder.borrow().geometry();
    let (x, y, w, hgt) = rect_of(&leaf);
    assert_eq!((h.x + x, h.y + y, w, hgt), (50, 20, 150, 40));
}

#[test]
fn reactivating_the_outer_layout_keeps_nested_leaves_in_holder_coordinates() {
    // A nested layout's leaves are children of the holder, so re-running only the outer layout
    // (no widget-tree `update_layout` recursion) must leave them where the full pass put them:
    // (the pattern of a header row built with PySide's `addLayout`).
    let first = fixed(50, 20);
    let leaf = fixed(40, 20);
    let mut inner = BoxLayout::horizontal();
    inner.set_margins(Margins::new(0, 0, 0, 0));
    inner.add_widget_with_stretch(leaf.clone(), 0);
    let mut outer = BoxLayout::horizontal();
    outer.set_margins(Margins::new(0, 0, 0, 0));
    outer.set_spacing(0);
    outer.add_widget_with_stretch(first, 0);
    let holder = outer.add_layout(Box::new(inner), 1);
    let container = lay_out(Box::new(outer));
    let after_full_pass = rect_of(&leaf);
    {
        let widget = container.borrow();
        let mut layout = widget.layout_ref_mut().expect("container has a layout");
        layout.invalidate();
        layout.activate();
    }
    assert_eq!(
        rect_of(&leaf),
        after_full_pass,
        "leaf moved by an outer-only relayout"
    );
    // PySide: first (36,20,50,20), inner layout (122,0,40,60), leaf (122,20,40,20).
    let h = holder.borrow().geometry();
    assert_eq!(
        (h.x + after_full_pass.0, after_full_pass.1),
        (122, 20),
        "leaf in container coordinates"
    );
}

// ---------------------------------------------------------------------------------------------
// Grid removeWidget.
// ---------------------------------------------------------------------------------------------

#[test]
fn grid_remove_widget_takes_the_widget_out_and_reports_it() {
    // PySide: grid with two widgets; removeWidget(a) -> True, count 1; removeWidget(unknown) -> False.
    let a = fixed(10, 10);
    let b = fixed(10, 10);
    let unknown = fixed(10, 10);
    let mut grid = GridLayout::new();
    grid.add_widget(a.clone(), 0, 0);
    grid.add_widget(b.clone(), 0, 1);
    assert!(
        grid.remove_widget(&a),
        "removeWidget of a placed widget returns true"
    );
    assert_eq!(grid.widgets().len(), 1);
    assert!(
        !grid.remove_widget(&unknown),
        "removeWidget of an unknown widget returns false"
    );
    assert_eq!(grid.widgets().len(), 1);
}

// ---------------------------------------------------------------------------------------------
// Spacing defaults: a parentless grid reports -1 (qSmartSpacing, qlayoutengine.cpp:390-401).
// ---------------------------------------------------------------------------------------------

#[test]
fn parentless_grid_spacing_is_minus_one_and_follows_the_axes() {
    // PySide: QGridLayout() -> spacing() -1, horizontal -1, vertical -1. After
    // setHorizontalSpacing(10) and setVerticalSpacing(4): spacing() -1 (axes differ), 10, 4.
    let mut grid = GridLayout::new();
    assert_eq!(grid.spacing(), -1);
    assert_eq!(grid.horizontal_spacing(), -1);
    assert_eq!(grid.vertical_spacing(), -1);
    grid.set_horizontal_spacing(10);
    grid.set_vertical_spacing(4);
    assert_eq!(grid.spacing(), -1);
    assert_eq!(grid.horizontal_spacing(), 10);
    assert_eq!(grid.vertical_spacing(), 4);
}

// ---------------------------------------------------------------------------------------------
// Grid height-for-width.
// ---------------------------------------------------------------------------------------------

#[test]
fn grid_height_for_width_follows_its_height_for_width_child() {
    // Probe child with heightForWidth(w) = 2w: grid.hasHeightForWidth() true, heightForWidth(40) = 80.
    // qgridlayout.cpp:358-372; PySide QLabel(wordWrap) in a grid also reports hasHeightForWidth true.
    let mut grid = GridLayout::new();
    grid.set_margins(Margins::new(0, 0, 0, 0));
    grid.add_widget(widget_with(40, 20, Policy::Preferred, true), 0, 0);
    assert!(grid.has_height_for_width());
    assert_eq!(grid.height_for_width(40), 80);
}
#[test]
fn grid_minimum_height_for_width_uses_row_minimum_size() {
    // Probe: two-row grid with 0 margins, 0 spacing.
    // Row 0: sizeHint (40, 50), minimumSize (40, 20)
    // Row 1: HFW widget with heightForWidth(w) = 2 * w (at w=40, h=80)
    // Total heightForWidth(40) = 50 + 80 = 130
    // Total minimumHeightForWidth(40) = 20 + 80 = 100
    // qgridlayout.cpp:374-382; expected values measured with PySide6
    let mut grid = GridLayout::new();
    grid.set_margins(Margins::new(0, 0, 0, 0));
    grid.set_horizontal_spacing(0);
    grid.set_vertical_spacing(0);
    grid.add_widget(
        widget_with_min(40, 50, 40, 20, Policy::Preferred, false),
        0,
        0,
    );
    grid.add_widget(widget_with(40, 20, Policy::Preferred, true), 1, 0);
    assert!(grid.has_height_for_width());
    assert_eq!(grid.height_for_width(40), 130);
    assert_eq!(grid.minimum_height_for_width(40), 100);
}
