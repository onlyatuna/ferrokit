// SPDX-License-Identifier: LGPL-3.0-only
//! Nested layouts. Qt adds a layout as a child layout (`QBoxLayout::addLayout`), so its
//! `maximumSize` caps the outer item (qboxlayout.cpp:253-357, 612-621).

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

// Qt: QBoxLayout::setupGeom vertical branch sums child maxima (qboxlayout.cpp:320) and takes the
// cross-axis max (:324-325); maximumSize is that max bounded by QLAYOUTSIZE_MAX (:612-621).
// Fixed children have max = hint (qSmartMaxSize, qlayoutitem.cpp:637-644).
#[test]
fn nested_vertical_box_max_is_sum_of_heights_and_max_of_widths() {
    let mut inner = BoxLayout::vertical();
    inner.set_spacing(0);
    inner.add_widget(probe(50, 20, Policy::Fixed));
    inner.add_widget(probe(30, 10, Policy::Fixed));
    let max = inner.maximum_size();
    assert_eq!((max.width, max.height), (50, 30));
}

// A PySide6 application can use addLayout (no wrapper widget). A nested fixed-size box must
// keep its maximum width when placed beside a Preferred sibling.
#[test]
fn nested_layout_added_with_add_layout_is_capped_by_its_max() {
    let mut outer = BoxLayout::horizontal();
    outer.set_margins(Margins::new(0, 0, 0, 0));
    outer.set_spacing(0);
    let mut inner = BoxLayout::vertical();
    inner.set_margins(Margins::new(0, 0, 0, 0));
    inner.set_spacing(0);
    let fixed = probe(50, 20, Policy::Fixed);
    inner.add_widget(fixed.clone());
    outer.add_layout(Box::new(inner), 0);
    let sibling = probe(20, 20, Policy::Preferred);
    outer.add_widget(sibling.clone());
    lay_out(Box::new(outer), 300, 100);
    assert_eq!(rect_of(&fixed).2, 50);
    assert_eq!(rect_of(&sibling).2, 250);
}
