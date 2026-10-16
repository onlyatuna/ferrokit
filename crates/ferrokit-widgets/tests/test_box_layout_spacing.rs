// SPDX-License-Identifier: LGPL-3.0-only
//! `QBoxLayout::addSpacing`, `insertSpacing`, `addSpacerItem`, `insertSpacerItem`,
//! `insertStretch`, `setStretchFactor`, `setStretch` and `stretch` (qboxlayout.cpp:844-1138).
//!
//! Spacers report `QSpacerItem` sizes: the hint is the given size, the minimum is 0 where the
//! policy may shrink, the maximum unbounded where it may grow (qlayoutitem.cpp:607-653). They are
//! empty, so the layout spacing falls between the widgets only (qboxlayout.cpp:261-294).
//! Reference values: PySide6 6.11.2, margins 0, spacing 6, items whose `sizeHint` is 50x20 and
//! `minimumSizeHint` 0x0.

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
    let p = Probe {
        base: WidgetBase::new(),
        hint: Size::new(w, h),
    };
    p.base.set_size_policy(QSizePolicy::new(policy, policy));
    Rc::new(RefCell::new(Box::new(p)))
}

/// Lays `layout` out in `width`x`height` and returns (position, length) of every item along it.
fn spans(mut layout: BoxLayout, width: i32, height: i32) -> (Vec<(i32, i32)>, Size, Size) {
    layout.set_margins(Margins::new(0, 0, 0, 0));
    layout.set_spacing(6);
    let vertical = layout.direction() == Direction::TopToBottom;
    let (hint, min) = (layout.size_hint(), layout.minimum_size());
    let items = layout.widgets();
    let container: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::with_geometry(
        Rect::new(0, 0, width, height),
    ))));
    container.borrow_mut().set_layout(Box::new(layout));
    container.borrow().update_layout();
    let spans = items
        .iter()
        .map(|w| {
            let g = w.borrow().geometry();
            if vertical {
                (g.y, g.height)
            } else {
                (g.x, g.width)
            }
        })
        .collect();
    (spans, hint, min)
}

fn spaced(mut layout: BoxLayout) -> BoxLayout {
    layout.add_widget(probe(50, 20, Policy::Preferred));
    layout.add_spacing(20);
    layout.add_widget(probe(50, 20, Policy::Preferred));
    layout
}

#[test]
fn add_spacing_is_a_fixed_gap_beside_one_layout_spacing() {
    let (spans, hint, min) = spans(spaced(BoxLayout::horizontal()), 300, 20);
    assert_eq!(spans, vec![(0, 137), (143, 20), (163, 137)]);
    assert_eq!(hint, Size::new(126, 20));
    assert_eq!(min, Size::new(26, 0));
}

#[test]
fn add_spacing_does_not_shrink_when_the_layout_is_squeezed() {
    let (spans, _, _) = spans(spaced(BoxLayout::horizontal()), 60, 20);
    assert_eq!(spans, vec![(0, 17), (23, 20), (43, 17)]);
}

#[test]
fn add_spacing_runs_along_a_vertical_layout() {
    let (spans, hint, _) = spans(spaced(BoxLayout::vertical()), 20, 300);
    assert_eq!(spans, vec![(0, 137), (143, 20), (163, 137)]);
    assert_eq!(hint, Size::new(50, 66));
}

#[test]
fn a_minimum_spacer_item_keeps_its_size_as_a_minimum_on_both_axes() {
    let mut layout = BoxLayout::horizontal();
    layout.add_widget(probe(50, 20, Policy::Preferred));
    layout.add_spacer_item(SpacerItem::new(40, 10, Policy::Minimum, Policy::Minimum));
    layout.add_widget(probe(50, 20, Policy::Preferred));
    let (spans, _, min) = spans(layout, 300, 20);
    assert_eq!(spans, vec![(0, 127), (133, 40), (173, 127)]);
    assert_eq!(min, Size::new(46, 10));
}

#[test]
fn a_minimum_expanding_spacer_item_shares_with_an_expanding_widget() {
    let mut layout = BoxLayout::horizontal();
    layout.add_widget(probe(50, 20, Policy::Expanding));
    layout.add_spacer_item(SpacerItem::new(
        40,
        10,
        Policy::MinimumExpanding,
        Policy::Minimum,
    ));
    assert_eq!(spans(layout, 300, 20).0, vec![(0, 150), (150, 150)]);
}

#[test]
fn insert_stretch_and_insert_spacing_go_in_at_the_index() {
    let mut layout = BoxLayout::horizontal();
    layout.add_widget(probe(50, 20, Policy::Preferred));
    layout.add_widget(probe(50, 20, Policy::Preferred));
    layout.insert_stretch(1, 1);
    layout.insert_spacing(0, 10);
    assert_eq!(
        spans(layout, 300, 20).0,
        vec![(0, 10), (10, 50), (66, 184), (250, 50)]
    );
}

#[test]
fn an_insert_index_past_the_end_appends() {
    let mut layout = BoxLayout::horizontal();
    layout.add_widget(probe(50, 20, Policy::Preferred));
    layout.add_stretch(0);
    layout.insert_widget(99, probe(50, 20, Policy::Preferred), 0);
    assert_eq!(
        spans(layout, 300, 20).0,
        vec![(0, 50), (56, 194), (250, 50)]
    );
}

#[test]
fn set_stretch_factor_finds_direct_widgets_only_and_set_stretch_lays_out_again() {
    let (a, b) = (
        probe(50, 20, Policy::Preferred),
        probe(50, 20, Policy::Preferred),
    );
    let mut layout = BoxLayout::horizontal();
    layout.add_widget(a);
    layout.add_widget(b.clone());
    assert!(layout.set_stretch_factor(&b, 2));
    assert!(!layout.set_stretch_factor(&probe(50, 20, Policy::Preferred), 1));
    assert_eq!(layout.stretch(1), Some(2));
    assert_eq!(layout.stretch(5), None);
    layout.set_stretch(0, 1);
    assert_eq!(spans(layout, 300, 20).0, vec![(0, 98), (104, 196)]);
}
