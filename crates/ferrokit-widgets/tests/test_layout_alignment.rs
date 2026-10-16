// SPDX-License-Identifier: LGPL-3.0-only
//! Per-item layout alignment (`QLayoutItem::setAlignment`).
//!
//! An aligned item is not stretched to fill its cell: on an aligned axis it shrinks to its size
//! hint and is placed by the flag, its maximum size on that axis is unbounded and it no longer
//! expands there (`QWidgetItem::setGeometry`, `maximumSize`, `expandingDirections`,
//! qlayoutitem.cpp:408-474, 580-612; `qSmartMaxSize`, qlayoutengine.cpp:354-375).
//!
//! Reference rectangles were measured with PySide6 (`QGridLayout`, `QHBoxLayout`, `QVBoxLayout`,
//! `QStackedLayout`, all `Preferred` or `Expanding` items with a fixed size hint and a zero
//! minimum size hint). A differential test against PySide6 compares random layouts
//! with random alignments against Qt.

use ferrokit_core::event::Event;
use ferrokit_core::object::{ObjectData, ObjectId, QObject};
use ferrokit_gui::geometry::primitives::{Margins, Rect, Size};
use ferrokit_widgets::*;
use std::cell::RefCell;
use std::rc::Rc;

struct Probe {
    base: WidgetBase,
    hint: Size,
}

impl QObject for Probe {
    fn object_data(&self) -> &ObjectData {
        &self.base.object_data
    }
    fn object_data_mut(&mut self) -> &mut ObjectData {
        &mut self.base.object_data
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
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

fn probe(w: i32, h: i32, policy: Policy) -> WidgetRef {
    let p = Probe { base: WidgetBase::new(), hint: Size::new(w, h) };
    p.base.set_size_policy(QSizePolicy::new(policy, policy));
    Rc::new(RefCell::new(Box::new(p)))
}

fn rect_of(w: &WidgetRef) -> (i32, i32, i32, i32) {
    let g = w.borrow().geometry();
    (g.x, g.y, g.width, g.height)
}

fn lay_out(layout: Box<dyn Layout>, width: i32, height: i32) -> WidgetRef {
    let container: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::with_geometry(
        Rect::new(0, 0, width, height),
    ))));
    container.borrow_mut().set_layout(layout);
    container.borrow_mut().set_geometry(Rect::new(0, 0, width, height));
    container.borrow().update_layout();
    container
}

/// A 2-column grid like a usage table: a label and a legend with `AlignVCenter | AlignLeft`, a
/// value with `AlignHCenter`, and one item without alignment. Row 1 and column 1 stretch.
fn table_like_grid(aligned: bool) -> (WidgetRef, [WidgetRef; 4]) {
    let items = [
        probe(60, 18, Policy::Preferred),
        probe(80, 70, Policy::Preferred),
        probe(71, 20, Policy::Preferred),
        probe(50, 30, Policy::Preferred),
    ];
    let left_v_center = if aligned { Alignment::LEFT | Alignment::V_CENTER } else { Alignment::NONE };
    let h_center = if aligned { Alignment::H_CENTER } else { Alignment::NONE };
    let mut grid = GridLayout::new();
    grid.set_margins(Margins::new(0, 0, 0, 0));
    grid.set_horizontal_spacing(4);
    grid.set_vertical_spacing(4);
    grid.add_widget_aligned(items[0].clone(), 0, 0, 1, 1, left_v_center);
    grid.add_widget_aligned(items[1].clone(), 1, 0, 1, 1, left_v_center);
    grid.add_widget_aligned(items[2].clone(), 0, 1, 1, 1, h_center);
    grid.add_widget_aligned(items[3].clone(), 1, 1, 1, 1, Alignment::NONE);
    grid.set_row_stretch(1, 1);
    grid.set_column_stretch(1, 1);
    (lay_out(Box::new(grid), 300, 160), items)
}

#[test]
fn grid_item_alignment_does_not_fill_cell() {
    let (_container, items) = table_like_grid(true);
    assert_eq!(rect_of(&items[0]), (0, 1, 60, 18), "AlignVCenter|AlignLeft: hint size, centred");
    assert_eq!(rect_of(&items[1]), (0, 57, 80, 70), "a tall row does not stretch the legend");
    assert_eq!(rect_of(&items[2]), (156, 0, 71, 20), "AlignHCenter: hint width, centred in the column");
    assert_eq!(rect_of(&items[3]), (84, 24, 216, 136), "an item without alignment still fills");
}

#[test]
fn grid_items_without_alignment_fill_their_cells() {
    let (_container, items) = table_like_grid(false);
    assert_eq!(rect_of(&items[0]), (0, 0, 80, 20));
    assert_eq!(rect_of(&items[1]), (0, 24, 80, 136));
    assert_eq!(rect_of(&items[2]), (84, 0, 216, 20));
    assert_eq!(rect_of(&items[3]), (84, 24, 216, 136));
}

#[test]
fn box_item_alignment_places_item_inside_the_cell() {
    let a = probe(60, 20, Policy::Preferred);
    let b = probe(50, 30, Policy::Preferred);
    let mut h = BoxLayout::horizontal();
    h.set_margins(Margins::new(0, 0, 0, 0));
    h.set_spacing(4);
    h.add_widget_aligned(a.clone(), 0, Alignment::RIGHT | Alignment::BOTTOM);
    h.add_widget_aligned(b.clone(), 1, Alignment::H_CENTER);
    let _c = lay_out(Box::new(h), 300, 100);
    assert_eq!(rect_of(&a), (0, 80, 60, 20));
    assert_eq!(rect_of(&b), (157, 0, 50, 100), "only the aligned axis shrinks");

    let a = probe(60, 20, Policy::Preferred);
    let b = probe(50, 30, Policy::Preferred);
    let mut v = BoxLayout::vertical();
    v.set_margins(Margins::new(0, 0, 0, 0));
    v.set_spacing(4);
    v.add_widget_aligned(a.clone(), 0, Alignment::RIGHT);
    v.add_widget_aligned(b.clone(), 1, Alignment::TOP);
    let _c = lay_out(Box::new(v), 300, 100);
    assert_eq!(rect_of(&a), (240, 0, 60, 20));
    assert_eq!(rect_of(&b), (0, 24, 300, 30));
}

#[test]
fn alignment_removes_the_aligned_axis_from_expanding_directions() {
    let expanding = |align: Alignment| {
        let mut h = BoxLayout::horizontal();
        h.add_widget_aligned(probe(60, 20, Policy::Expanding), 0, align);
        h.expanding_directions()
    };
    assert_eq!(expanding(Alignment::NONE), (true, true));
    assert_eq!(expanding(Alignment::H_CENTER), (false, true));
    assert_eq!(expanding(Alignment::V_CENTER), (true, false));
    assert_eq!(expanding(Alignment::CENTER), (false, false));
}

#[test]
fn aligned_expanding_item_is_placed_not_stretched() {
    let place = |align: Alignment| {
        let w = probe(60, 20, Policy::Expanding);
        let mut h = BoxLayout::horizontal();
        h.set_margins(Margins::new(0, 0, 0, 0));
        h.add_widget_aligned(w.clone(), 0, align);
        let _c = lay_out(Box::new(h), 300, 100);
        rect_of(&w)
    };
    assert_eq!(place(Alignment::NONE), (0, 0, 300, 100));
    assert_eq!(place(Alignment::H_CENTER), (120, 0, 60, 100));
    assert_eq!(place(Alignment::V_CENTER), (0, 40, 300, 20));
    assert_eq!(place(Alignment::CENTER), (120, 40, 60, 20));
}

#[test]
fn set_alignment_applies_to_an_existing_item_and_reports_unknown_widgets() {
    let w = probe(60, 20, Policy::Preferred);
    let stranger = probe(60, 20, Policy::Preferred);
    let mut h = BoxLayout::horizontal();
    h.set_margins(Margins::new(0, 0, 0, 0));
    h.add_widget_with_stretch(w.clone(), 0);
    let container = lay_out(Box::new(h), 300, 100);
    assert_eq!(rect_of(&w), (0, 0, 300, 100));

    {
        let mut c = container.borrow_mut();
        let layout = c.layout_mut().unwrap();
        assert!(layout.set_alignment(&w, Alignment::CENTER));
        assert!(!layout.set_alignment(&stranger, Alignment::CENTER));
    }

    assert_eq!(rect_of(&w), (120, 40, 60, 20), "set_alignment must re-lay the layout out");
}

#[test]
fn stacked_layout_ignores_item_alignment_like_qt() {
    // `QStackedLayout::setGeometry` gives the page the whole rectangle (qstackedlayout.cpp:453).
    let page = probe(60, 20, Policy::Preferred);
    let mut stack = StackedLayout::new();
    stack.add_widget(page.clone());
    let container = lay_out(Box::new(stack), 300, 100);
    {
        let mut c = container.borrow_mut();
        let layout = c.layout_mut().unwrap();
        assert!(layout.set_alignment(&page, Alignment::RIGHT | Alignment::BOTTOM));
    }
    container.borrow().update_layout();
    assert_eq!(rect_of(&page), (0, 0, 300, 100));
}
