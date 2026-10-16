// SPDX-License-Identifier: LGPL-3.0-only
//! `QSizePolicy::retainSizeWhenHidden` and right-to-left layout direction for box and
//! grid layouts.
//!
//! - A hidden widget whose size policy retains its size keeps its layout slot and its spacing
//!   (`QWidgetItem::isEmpty`, qlayoutitem.cpp:691-693; `QBoxLayout` spacing, qboxlayout.cpp:258-296).
//! - Under `RightToLeft` a box or grid mirrors its cells: `QBoxLayout::setGeometry`
//!   (qboxlayout.cpp:788-790) and `QGridLayoutPrivate::distribute` (qgridlayout.cpp:921-922).
//! - Explicit horizontal alignment flips unless `AlignAbsolute` is set
//!   (`QStyle::visualAlignment`, qguiapplication_p.h:176-186; `QWidgetItem::setGeometry`,
//!   qlayoutitem.cpp:451-455). An item with no horizontal flag gets `AlignLeft` first, so it also
//!   flips to the right under RTL.
//!
//! Every reference rectangle was measured with PySide6 6.11.2 offscreen.
//!
//! The direction is process-wide, so every test in this binary takes `LOCK` and sets the direction
//! through `with_direction`, which restores left-to-right when it is dropped.

use ferrokit_core::event::Event;
use ferrokit_core::object::{ObjectData, ObjectId, QObject};
use ferrokit_gui::application::{GuiApplication, LayoutDirection};
use ferrokit_gui::geometry::primitives::{Margins, Rect, Size};
use ferrokit_widgets::layout_engine::item_set_geometry_with;
use ferrokit_widgets::*;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Mutex;

static LOCK: Mutex<()> = Mutex::new(());

struct DirectionGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
}

fn with_direction(direction: LayoutDirection) -> DirectionGuard {
    let guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    GuiApplication::set_layout_direction(direction);
    DirectionGuard { _lock: guard }
}

impl Drop for DirectionGuard {
    fn drop(&mut self) {
        GuiApplication::set_layout_direction(LayoutDirection::LeftToRight);
    }
}

struct Probe {
    base: WidgetBase,
    hint: Size,
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
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

fn probe(w: i32, h: i32, policy: Policy) -> WidgetRef {
    let p = Probe {
        base: WidgetBase::new(),
        hint: Size::new(w, h),
    };
    p.base.set_size_policy(QSizePolicy::new(policy, policy));
    Rc::new(RefCell::new(Box::new(p)))
}

fn probe_with(w: i32, h: i32, policy: QSizePolicy) -> WidgetRef {
    let p = Probe {
        base: WidgetBase::new(),
        hint: Size::new(w, h),
    };
    p.base.set_size_policy(policy);
    Rc::new(RefCell::new(Box::new(p)))
}

fn hide(w: &WidgetRef) {
    w.borrow().set_visible(false);
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
    container
        .borrow_mut()
        .set_geometry(Rect::new(0, 0, width, height));
    container.borrow().update_layout();
    container
}

/// a (60x20), b (50x30, hidden, retained), c (40x20); spacing 4; trailing stretch; 300x100.
fn retained_box() -> (WidgetRef, WidgetRef, WidgetRef, WidgetRef) {
    let a = probe_with(60, 20, QSizePolicy::fixed());
    let b = probe_with(
        50,
        30,
        QSizePolicy::fixed().with_retain_size_when_hidden(true),
    );
    let c = probe_with(40, 20, QSizePolicy::fixed());
    hide(&b);
    let mut h = BoxLayout::horizontal();
    h.set_margins(Margins::new(0, 0, 0, 0));
    h.set_spacing(4);
    h.add_widget_aligned(a.clone(), 0, Alignment::NONE);
    h.add_widget_aligned(b.clone(), 0, Alignment::NONE);
    h.add_widget_aligned(c.clone(), 0, Alignment::NONE);
    h.add_stretch(1);
    let container = lay_out(Box::new(h), 300, 100);
    (a, b, c, container)
}

/// The same three items in a grid: columns 0-2, column 3 stretched, horizontal spacing 4.
fn retained_grid() -> (WidgetRef, WidgetRef, WidgetRef, WidgetRef) {
    let a = probe_with(60, 20, QSizePolicy::fixed());
    let b = probe_with(
        50,
        30,
        QSizePolicy::fixed().with_retain_size_when_hidden(true),
    );
    let c = probe_with(40, 20, QSizePolicy::fixed());
    hide(&b);
    let mut g = GridLayout::new();
    g.set_margins(Margins::new(0, 0, 0, 0));
    g.set_horizontal_spacing(4);
    g.add_widget(a.clone(), 0, 0);
    g.add_widget(b.clone(), 0, 1);
    g.add_widget(c.clone(), 0, 2);
    g.set_column_stretch(3, 3);
    let container = lay_out(Box::new(g), 300, 100);
    (a, b, c, container)
}

#[test]
fn retained_hidden_box_item_keeps_its_slot_ltr() {
    let _dir = with_direction(LayoutDirection::LeftToRight);
    let (a, b, c, _container) = retained_box();
    assert_eq!(rect_of(&a), (0, 40, 60, 20), "visible item a");
    assert_eq!(
        rect_of(&b),
        (64, 35, 50, 30),
        "hidden but retained: keeps its slot"
    );
    assert_eq!(
        rect_of(&c),
        (118, 40, 40, 20),
        "c follows the retained slot and spacing"
    );
}

#[test]
fn rtl_box_mirrors_order_and_retained_slots() {
    let _dir = with_direction(LayoutDirection::RightToLeft);
    let (a, b, c, _container) = retained_box();
    assert_eq!(
        rect_of(&a),
        (240, 40, 60, 20),
        "RTL: the first item sits at the right edge"
    );
    assert_eq!(
        rect_of(&b),
        (186, 35, 50, 30),
        "RTL: retained slot mirrored"
    );
    assert_eq!(
        rect_of(&c),
        (142, 40, 40, 20),
        "RTL: c follows the mirrored slot"
    );
}
#[test]
fn hidden_non_retained_item_preserves_geometry() {
    let a = probe_with(60, 20, QSizePolicy::fixed());
    let b = probe_with(50, 30, QSizePolicy::fixed());
    let mut h = BoxLayout::horizontal();
    h.set_margins(Margins::new(0, 0, 0, 0));
    h.set_spacing(4);
    h.add_widget_aligned(a.clone(), 0, Alignment::NONE);
    h.add_widget_aligned(b.clone(), 0, Alignment::NONE);
    h.add_stretch(1);
    let container = lay_out(Box::new(h), 300, 100);
    assert_eq!(rect_of(&b), (64, 35, 50, 30));

    hide(&b);
    item_set_geometry_with(
        &**b.borrow(),
        Rect::new(10, 10, 100, 100),
        Alignment::NONE,
        ferrokit_widgets::layout_engine::ItemLimits::of(&**b.borrow(), Alignment::NONE),
    );
    // Qt `QWidgetItem::setGeometry` returns early for an empty item (qlayoutitem.cpp:410).
    assert_eq!(
        rect_of(&b),
        (64, 35, 50, 30),
        "Hidden non-retained item must preserve its geometry instead of being zeroed (qlayoutitem.cpp:410)"
    );
}

#[test]
fn retained_hidden_grid_item_keeps_its_slot_ltr() {
    let _dir = with_direction(LayoutDirection::LeftToRight);
    let (a, b, c, _container) = retained_grid();
    assert_eq!(rect_of(&a), (0, 40, 60, 20), "grid LTR: visible item a");
    assert_eq!(
        rect_of(&b),
        (64, 35, 50, 30),
        "grid LTR: hidden but retained keeps its column"
    );
    assert_eq!(
        rect_of(&c),
        (118, 40, 40, 20),
        "grid LTR: c follows the retained column"
    );
}

#[test]
fn rtl_grid_mirrors_columns_and_retained_slots() {
    let _dir = with_direction(LayoutDirection::RightToLeft);
    let (a, b, c, _container) = retained_grid();
    assert_eq!(
        rect_of(&a),
        (240, 40, 60, 20),
        "grid RTL: column 0 at the right edge"
    );
    assert_eq!(
        rect_of(&b),
        (186, 35, 50, 30),
        "grid RTL: retained column mirrored"
    );
    assert_eq!(
        rect_of(&c),
        (142, 40, 40, 20),
        "grid RTL: column 2 mirrored"
    );
}

#[test]
fn rtl_flips_horizontal_alignment_unless_absolute() {
    let _dir = with_direction(LayoutDirection::RightToLeft);
    let logical = probe(60, 20, Policy::Preferred);
    let absolute = probe(60, 20, Policy::Preferred);
    let mut v = BoxLayout::vertical();
    v.set_margins(Margins::new(0, 0, 0, 0));
    v.add_widget_aligned(logical.clone(), 0, Alignment::LEFT);
    v.add_widget_aligned(absolute.clone(), 0, Alignment::LEFT | Alignment::ABSOLUTE);
    let _container = lay_out(Box::new(v), 300, 100);
    assert_eq!(
        rect_of(&logical).0,
        240,
        "RTL: logical LEFT is placed at the right edge"
    );
    assert_eq!(
        rect_of(&absolute).0,
        0,
        "RTL: LEFT|ABSOLUTE stays physical left"
    );
}

#[test]
fn rtl_unaligned_fixed_item_sits_at_the_right_edge_ltr_at_the_left() {
    // No horizontal flag means AlignLeft (`QStyle::visualAlignment`), which flips under RTL.
    let item = probe_with(60, 20, QSizePolicy::fixed());
    let mut v = BoxLayout::vertical();
    v.set_margins(Margins::new(0, 0, 0, 0));
    v.add_widget_aligned(item.clone(), 0, Alignment::NONE);
    v.add_stretch(1);
    {
        let _dir = with_direction(LayoutDirection::LeftToRight);
        let _container = lay_out(Box::new(v), 300, 100);
        assert_eq!(rect_of(&item).0, 0, "LTR: unaligned fixed item at the left");
    }
    let item = probe_with(60, 20, QSizePolicy::fixed());
    let mut v = BoxLayout::vertical();
    v.set_margins(Margins::new(0, 0, 0, 0));
    v.add_widget_aligned(item.clone(), 0, Alignment::NONE);
    v.add_stretch(1);
    let _dir = with_direction(LayoutDirection::RightToLeft);
    let _container = lay_out(Box::new(v), 300, 100);
    assert_eq!(
        rect_of(&item).0,
        240,
        "RTL: unaligned fixed item at the right edge"
    );
}

#[test]
fn grid_unaligned_fixed_item_is_centred_by_column_distribution_in_both_directions() {
    // The item's column cannot grow past its maximum (60), so `qGeomCalc` centres the column
    // (qlayoutengine.cpp:286-287). The item itself stays at its cell's left.
    for direction in [LayoutDirection::LeftToRight, LayoutDirection::RightToLeft] {
        let _dir = with_direction(direction);
        let item = probe_with(60, 20, QSizePolicy::fixed());
        let mut g = GridLayout::new();
        g.set_margins(Margins::new(0, 0, 0, 0));
        g.add_widget(item.clone(), 0, 0);
        let _container = lay_out(Box::new(g), 300, 100);
        assert_eq!(
            rect_of(&item),
            (120, 40, 60, 20),
            "{direction:?}: centred column"
        );
    }
}
