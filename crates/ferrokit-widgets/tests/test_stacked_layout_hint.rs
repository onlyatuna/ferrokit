// SPDX-License-Identifier: LGPL-3.0-only
//! `QStackedLayout::sizeHint` and `minimumSize` cover every page, not only the
//! current one (qstackedlayout.cpp:406-442): the hint is the component-wise maximum of the pages'
//! hints, an `Ignored` axis counting as 0; the minimum is the maximum of `qSmartMinSize`.
//!
//! Expected values were measured with PySide6 6.x (`QStackedLayout` on a `QWidget`, offscreen,
//! pages overriding `sizeHint`/`minimumSizeHint`); case letters match the measuring script.

use ferrokit_core::event::Event;
use ferrokit_core::object::{ObjectData, ObjectId, QObject};
use ferrokit_gui::geometry::primitives::{Rect, Size};
use ferrokit_widgets::*;
use std::cell::RefCell;
use std::rc::Rc;

struct Probe {
    base: WidgetBase,
    hint: Size,
    min_hint: Size,
    min: Size,
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
    fn minimum_size(&self) -> Size {
        self.min
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

fn page(hint: (i32, i32), min_hint: (i32, i32), min: (i32, i32), h: Policy, v: Policy) -> WidgetRef {
    let p = Probe {
        base: WidgetBase::new(),
        hint: Size::new(hint.0, hint.1),
        min_hint: Size::new(min_hint.0, min_hint.1),
        min: Size::new(min.0, min.1),
    };
    p.base.set_size_policy(QSizePolicy::new(h, v));
    Rc::new(RefCell::new(Box::new(p)))
}

fn plain(hint: (i32, i32)) -> WidgetRef {
    page(hint, (0, 0), (0, 0), Policy::Preferred, Policy::Preferred)
}

fn stack(pages: Vec<WidgetRef>, current: usize) -> StackedLayout {
    let mut s = StackedLayout::new();
    for p in pages {
        s.add_widget(p);
    }
    s.set_current_index(current);
    s
}

fn hm(s: &StackedLayout) -> ((i32, i32), (i32, i32)) {
    let h = s.size_hint();
    let m = s.minimum_size();
    ((h.width, h.height), (m.width, m.height))
}

#[test]
fn empty_stack_is_zero() {
    // PySide6 case "empty": hint (0,0), min (0,0).
    assert_eq!(hm(&StackedLayout::new()), ((0, 0), (0, 0)));
}

#[test]
fn hint_is_the_maximum_over_all_pages_whichever_is_current() {
    // Case A: pages 100x50 and 60x80 -> hint (100,80) with either page current.
    for current in [0, 1] {
        let s = stack(vec![plain((100, 50)), plain((60, 80))], current);
        assert_eq!(hm(&s).0, (100, 80), "current page {current}");
    }
}

#[test]
fn minimum_is_the_maximum_of_the_pages_smart_minimum() {
    // Case B: explicit minimums (30,10) and (20,40) -> (30,40) while page 0 is current.
    let s = stack(
        vec![
            page((100, 50), (0, 0), (30, 10), Policy::Preferred, Policy::Preferred),
            page((60, 80), (0, 0), (20, 40), Policy::Preferred, Policy::Preferred),
        ],
        0,
    );
    assert_eq!(hm(&s), ((100, 80), (30, 40)));

    // Case C: a Fixed page counts as max(hint, minimumSizeHint) = (100,50) even when it is not the
    // current page; the other page's minimumSizeHint (10,10) is smaller.
    let s = stack(
        vec![
            page((100, 50), (70, 30), (0, 0), Policy::Fixed, Policy::Fixed),
            page((60, 80), (10, 10), (0, 0), Policy::Preferred, Policy::Preferred),
        ],
        1,
    );
    assert_eq!(hm(&s), ((100, 80), (100, 50)));
}

#[test]
fn an_ignored_axis_counts_as_zero() {
    // Case D: page 0 is Ignored horizontally -> its 100 width is dropped, hint (60,50).
    let s = stack(
        vec![
            page((100, 50), (0, 0), (0, 0), Policy::Ignored, Policy::Preferred),
            plain((60, 30)),
        ],
        0,
    );
    assert_eq!(hm(&s), ((60, 50), (0, 0)));

    // Case E: Ignored on both axes (minimumSizeHint 40x40 is ignored too); the other page has an
    // explicit minimum (5,5) -> hint (60,30), min (5,5).
    let s = stack(
        vec![
            page((100, 50), (40, 40), (0, 0), Policy::Ignored, Policy::Ignored),
            page((60, 30), (0, 0), (5, 5), Policy::Preferred, Policy::Preferred),
        ],
        1,
    );
    assert_eq!(hm(&s), ((60, 30), (5, 5)));
}

#[test]
fn removing_a_page_drops_its_contribution() {
    let mut s = stack(vec![plain((100, 50)), plain((60, 80))], 0);
    s.remove_widget(0);
    assert_eq!(hm(&s).0, (60, 80));
}

/// `QStackedLayout` gives the whole rectangle to the current page and hides every other
/// page (qstackedlayout.cpp:453); a hidden page is skipped by hit testing (`QWidget::isVisible`
/// gates the child search). After a page switch the old page must not receive the click.
#[test]
fn stacked_hides_non_current_page_and_hit_test_skips_it() {
    use ferrokit_gui::geometry::primitives::Point;
    use ferrokit_widgets::hit_test::hit_test;

    let page0: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::new())));
    let page1: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::new())));
    let root: WidgetRef = Rc::new(RefCell::new(Box::new(StackedWidget::new())));
    StackedWidget::add_widget(&root, Rc::clone(&page0));
    StackedWidget::add_widget(&root, Rc::clone(&page1));
    root.borrow().set_geometry(Rect::new(0, 0, 200, 100));

    assert!(page0.borrow().is_visible(), "current page must be visible");
    assert!(
        !page1.borrow().is_visible(),
        "non-current page must be hidden"
    );
    let (hit, _) = hit_test(&root, Point::new(10, 10)).expect("point inside the stack");
    assert_eq!(hit.borrow().id(), page0.borrow().id());

    StackedWidget::set_current_index(&root, 1);

    assert!(
        !page0.borrow().is_visible(),
        "old page must be hidden after the switch"
    );
    assert!(
        page1.borrow().is_visible(),
        "new page must be visible after the switch"
    );
    let (hit, _) = hit_test(&root, Point::new(10, 10)).expect("point inside the stack");
    assert_eq!(
        hit.borrow().id(),
        page1.borrow().id(),
        "hit test must skip the hidden page"
    );
}
