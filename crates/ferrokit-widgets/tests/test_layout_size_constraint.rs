// SPDX-License-Identifier: LGPL-3.0-only
//! `QLayout::activate` applies the top-level layout's `SizeConstraint` to its owner
//! (qlayout.cpp:980-1133). With `SetDefaultConstraint` a window's minimum becomes the layout's total
//! minimum size unless that axis has an explicit minimum, a child widget's becomes 0 (or the
//! explicit minimum); `setMinimumSize` / `setMaximumSize` then grow / shrink the owner
//! (qwidget.cpp:4005-4087), and the explicit flags are restored afterwards (qlayout.cpp:1126-1129).
//!
//! Expected values: measured with PySide6 6.11.2, offscreen.
//! Every case is a horizontal box (margins 5, spacing 4) over A (hint 60x30, minimum 50x20) and B
//! (hint 40x50, minimum 30x40): layout minimum 94x50, size hint 114x60.

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

fn probe(hint: (i32, i32), min: (i32, i32)) -> WidgetRef {
    let p = Probe {
        base: WidgetBase::with_geometry(Rect::new(0, 0, hint.0, hint.1)),
        hint: Size::new(hint.0, hint.1),
    };
    let w: WidgetRef = Rc::new(RefCell::new(Box::new(p)));
    w.borrow().set_minimum_size(Size::new(min.0, min.1));
    w
}

/// One case of the probe: the owner, its two items and (for a child owner) the parent.
struct Case {
    owner: WidgetRef,
    a: WidgetRef,
    b: WidgetRef,
    _parent: Option<WidgetRef>,
}

#[derive(Default)]
struct Options {
    child: bool,
    constraint: Option<SizeConstraint>,
    explicit_min: Option<(i32, i32)>,
    max_b: Option<(i32, i32)>,
}

/// `build` of the probe: layout and items, constraint, explicit minimum, resize, activate.
fn build(size: (i32, i32), opts: Options) -> Case {
    let a = probe((60, 30), (50, 20));
    let b = probe((40, 50), (30, 40));
    if let Some((w, h)) = opts.max_b {
        b.borrow().set_maximum_size(Size::new(w, h));
    }
    let mut layout = BoxLayout::horizontal();
    layout.set_margins(Margins::new(5, 5, 5, 5));
    layout.set_spacing(4);
    layout.add_widget(a.clone());
    layout.add_widget(b.clone());
    if let Some(constraint) = opts.constraint {
        layout.set_size_constraint(constraint);
    }
    let owner: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::with_geometry(
        Rect::new(0, 0, size.0, size.1),
    ))));
    let parent = opts.child.then(|| {
        let parent: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::with_geometry(
            Rect::new(0, 0, 800, 700),
        ))));
        owner
            .borrow()
            .set_parent_widget(Some(Rc::downgrade(&parent)));
        parent
    });
    owner.borrow_mut().set_layout(Box::new(layout));
    if let Some((w, h)) = opts.explicit_min {
        owner.borrow().set_minimum_size(Size::new(w, h));
    }
    owner.borrow().update_layout();
    Case {
        owner,
        a,
        b,
        _parent: parent,
    }
}

fn rect_of(w: &WidgetRef) -> (i32, i32, i32, i32) {
    let g = w.borrow().geometry();
    (g.x, g.y, g.width, g.height)
}

/// `name size=WxH min=WxH max=WxH a=x,y,w,h b=x,y,w,h`, the probe's line format.
fn report(name: &str, case: &Case) -> String {
    let owner = case.owner.borrow();
    let (g, min, max) = (owner.geometry(), owner.minimum_size(), owner.maximum_size());
    let (a, b) = (rect_of(&case.a), rect_of(&case.b));
    format!(
        "{name} size={}x{} min={}x{} max={}x{} a={},{},{},{} b={},{},{},{}",
        g.width,
        g.height,
        min.width,
        min.height,
        max.width,
        max.height,
        a.0,
        a.1,
        a.2,
        a.3,
        b.0,
        b.1,
        b.2,
        b.3
    )
}

#[test]
fn default_constraint_grows_a_window_to_the_layout_minimum() {
    let case = build((40, 20), Options::default());
    assert_eq!(
        report("default_window", &case),
        "default_window size=94x50 min=94x50 max=16777215x16777215 a=5,5,50,40 b=59,5,30,40"
    );
}

#[test]
fn explicit_minimum_wins_on_its_axis() {
    let case = build(
        (40, 20),
        Options {
            explicit_min: Some((70, 0)),
            ..Options::default()
        },
    );
    assert_eq!(
        report("default_window_explicit_min_w", &case),
        "default_window_explicit_min_w size=70x50 min=70x50 max=16777215x16777215 a=5,5,50,40 b=37,5,30,40"
    );
    let case = build(
        (40, 20),
        Options {
            explicit_min: Some((70, 10)),
            ..Options::default()
        },
    );
    assert_eq!(
        report("default_window_explicit_min_wh", &case),
        "default_window_explicit_min_wh size=70x20 min=70x10 max=16777215x16777215 a=5,5,50,20 b=37,5,30,40"
    );
}

#[test]
fn default_constraint_does_not_grow_a_child_owner() {
    let case = build(
        (40, 20),
        Options {
            child: true,
            ..Options::default()
        },
    );
    assert_eq!(
        report("default_child", &case),
        "default_child size=40x20 min=0x0 max=16777215x16777215 a=5,5,50,20 b=22,5,30,40"
    );
    let case = build(
        (40, 20),
        Options {
            child: true,
            explicit_min: Some((70, 0)),
            ..Options::default()
        },
    );
    assert_eq!(
        report("default_child_explicit_min_w", &case),
        "default_child_explicit_min_w size=70x20 min=70x0 max=16777215x16777215 a=5,5,50,20 b=37,5,30,40"
    );
}

#[test]
fn fixed_size_constraint_fixes_the_owner_to_the_size_hint() {
    let fixed = || Options {
        constraint: Some(SizeConstraint::SetFixedSize),
        ..Options::default()
    };
    let case = build((300, 200), fixed());
    assert_eq!(
        report("fixed_window", &case),
        "fixed_window size=114x60 min=114x60 max=114x60 a=5,5,60,50 b=69,5,40,50"
    );
    let case = build(
        (300, 200),
        Options {
            child: true,
            ..fixed()
        },
    );
    assert_eq!(
        report("fixed_child", &case),
        "fixed_child size=114x60 min=114x60 max=114x60 a=5,5,60,50 b=69,5,40,50"
    );
}

#[test]
fn no_constraint_leaves_the_window_alone() {
    let case = build(
        (40, 20),
        Options {
            constraint: Some(SizeConstraint::SetNoConstraint),
            ..Options::default()
        },
    );
    assert_eq!(
        report("none_window", &case),
        "none_window size=40x20 min=0x0 max=16777215x16777215 a=5,5,50,20 b=22,5,30,40"
    );
}

#[test]
fn minimum_size_constraint_grows_a_child_owner() {
    let case = build(
        (40, 20),
        Options {
            child: true,
            constraint: Some(SizeConstraint::SetMinimumSize),
            ..Options::default()
        },
    );
    assert_eq!(
        report("minimum_child", &case),
        "minimum_child size=94x50 min=94x50 max=16777215x16777215 a=5,5,50,40 b=59,5,30,40"
    );
}

#[test]
fn maximum_size_constraints_shrink_the_window_to_the_layout_maximum() {
    let case = build(
        (300, 200),
        Options {
            constraint: Some(SizeConstraint::SetMaximumSize),
            max_b: Some((45, 55)),
            ..Options::default()
        },
    );
    assert_eq!(
        report("maximum_window", &case),
        "maximum_window size=300x65 min=0x0 max=524287x65 a=5,5,241,55 b=250,5,45,55"
    );
    let case = build(
        (40, 300),
        Options {
            constraint: Some(SizeConstraint::SetMinAndMaxSize),
            max_b: Some((45, 55)),
            ..Options::default()
        },
    );
    assert_eq!(
        report("min_and_max_window", &case),
        "min_and_max_window size=94x65 min=94x50 max=524287x65 a=5,5,50,55 b=59,5,30,55"
    );
}

/// The layout-derived minimum is not explicit: once the layout minimum drops, the next activation
/// lowers the window minimum again, and the window keeps its size.
#[test]
fn layout_derived_minimum_is_not_explicit() {
    let case = build((40, 20), Options::default());
    case.b.borrow().set_minimum_size(Size::new(10, 10));
    LayoutScheduler::invalidate(&case.owner);
    LayoutScheduler::activate_pending();
    assert_eq!(
        report("default_window_relaxed", &case),
        "default_window_relaxed size=94x50 min=74x30 max=16777215x16777215 a=5,5,50,40 b=59,5,30,40"
    );
}

/// A window owning a `StackedLayout` (margins 8) over one page (hint 60x30, minimum 50x20).
fn stacked_line(name: &str, constraint: Option<SizeConstraint>, size: (i32, i32)) -> String {
    let page = probe((60, 30), (50, 20));
    let mut stack = StackedLayout::new();
    stack.set_margins(Margins::new(8, 8, 8, 8));
    stack.add_widget(page.clone());
    if let Some(constraint) = constraint {
        stack.set_size_constraint(constraint);
    }
    let owner: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::with_geometry(
        Rect::new(0, 0, size.0, size.1),
    ))));
    owner.borrow_mut().set_layout(Box::new(stack));
    owner.borrow().update_layout();
    let (g, min) = (owner.borrow().geometry(), owner.borrow().minimum_size());
    let p = rect_of(&page);
    format!(
        "{name} size={}x{} min={}x{} page={},{},{},{}",
        g.width, g.height, min.width, min.height, p.0, p.1, p.2, p.3
    )
}

/// `QStackedLayout::minimumSize` and `sizeHint` leave out the contents margins
/// (qstackedlayout.cpp:417-448): a window grows to the page's minimum only, and `SetFixedSize`
/// fixes it to the page's hint.
#[test]
fn stacked_window_bounds_leave_out_the_margins() {
    assert_eq!(
        stacked_line("stacked_window", None, (10, 10)),
        "stacked_window size=50x20 min=50x20 page=0,0,50,20"
    );
    assert_eq!(
        stacked_line(
            "stacked_fixed_window",
            Some(SizeConstraint::SetFixedSize),
            (300, 200)
        ),
        "stacked_fixed_window size=60x30 min=60x30 page=0,0,60,30"
    );
}
