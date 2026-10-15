// SPDX-License-Identifier: LGPL-3.0-only
//! `QBoxLayout::addStretch(0)` keeps stretch 0 (qboxlayout.cpp:872-879, 981-983).
//!
//! `setupGeom` passes the spacer's stretch through unchanged (qboxlayout.cpp:316; a spacer has no
//! widget to fall back to), and `qGeomCalc` gives a stretch-0 item nothing while a sibling has
//! stretch > 0, but shares the space with the other expanding items when none has
//! (qlayoutengine.cpp:229-235). Reference rectangles: PySide6 6.11.2, a 300x20 `QHBoxLayout` with
//! margins 0 and spacing 6, items whose `sizeHint` is 50x20 and `minimumSizeHint` 0x0.

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
        self.base.object_data.id
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

/// Lays out `layout` in 300x20 and returns (x, width) of every item, spacers included.
fn xs(mut layout: BoxLayout) -> Vec<(i32, i32)> {
    layout.set_margins(Margins::new(0, 0, 0, 0));
    layout.set_spacing(6);
    let items = layout.widgets();
    let container: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::with_geometry(
        Rect::new(0, 0, 300, 20),
    ))));
    container.borrow_mut().set_layout(Box::new(layout));
    container.borrow().update_layout();
    items
        .iter()
        .map(|w| {
            let g = w.borrow().geometry();
            (g.x, g.width)
        })
        .collect()
}

#[test]
fn a_stretch_zero_spacer_gets_nothing_beside_a_stretched_widget() {
    let mut layout = BoxLayout::horizontal();
    layout.add_stretch(0);
    layout.add_widget_with_stretch(probe(50, 20, Policy::Preferred), 1);
    assert_eq!(xs(layout), vec![(0, 0), (0, 300)]);
}

#[test]
fn a_stretch_zero_spacer_gets_nothing_beside_a_stretched_spacer() {
    let mut layout = BoxLayout::horizontal();
    layout.add_stretch(0);
    layout.add_widget_with_stretch(probe(50, 20, Policy::Preferred), 0);
    layout.add_stretch(2);
    assert_eq!(xs(layout), vec![(0, 0), (0, 50), (50, 250)]);
}

#[test]
fn a_stretch_zero_spacer_shares_equally_with_an_expanding_widget() {
    let mut layout = BoxLayout::horizontal();
    layout.add_widget_with_stretch(probe(50, 20, Policy::Expanding), 0);
    layout.add_stretch(0);
    assert_eq!(xs(layout), vec![(0, 150), (150, 150)]);
}

#[test]
fn two_stretch_zero_spacers_centre_a_widget() {
    let mut layout = BoxLayout::horizontal();
    layout.add_stretch(0);
    layout.add_widget_with_stretch(probe(50, 20, Policy::Preferred), 0);
    layout.add_stretch(0);
    assert_eq!(xs(layout), vec![(0, 125), (125, 50), (175, 125)]);
}
