// SPDX-License-Identifier: LGPL-3.0-only
//! Qt layout metric caching:
//! widgets/kernel/qboxlayout.cpp:219-361, 590-625, 735-775
//! widgets/kernel/qgridlayout.cpp:719-745, 880-928, 1181-1205, 1320-1328
//! widgets/kernel/qlayoutitem.cpp:408-474, 580-612
//! widgets/kernel/qlayoutengine.cpp:354-375
//!
//! 1. When a layout is not dirty:
//!    - `size_hint()`, `minimum_size()`, and `maximum_size()` return cached layout metrics
//!      without re-querying child widget size hints / minimum sizes / maximum sizes.
//!    - `set_geometry()` during a resize (rect != geometry) distributes space using the cached
//!      layout items / geomArray without re-querying child widget metrics.
//!    - Invalidation (`invalidate()`, content changes, size policy changes) clears the metric cache.
//! 2. `item_set_geometry` respects Qt smart-max: Fixed and Maximum policies are constrained to
//!    their size hints even under `Alignment::NONE`.
//! 3. Native window resize (`WindowSystemEvent::Resize`) routes through `request_layout`,
//!    redistributing geometry without invalidating the child size metric cache.

use ferrokit_core::event::Event;
use ferrokit_core::object::{ObjectData, ObjectId, QObject};
use ferrokit_gui::geometry::primitives::{Rect, Size};
use ferrokit_widgets::*;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};

struct MetricCountingProbe {
    base: WidgetBase,
    hint: RefCell<Size>,
    hint_queries: Rc<AtomicUsize>,
    min_queries: Rc<AtomicUsize>,
    max_queries: Rc<AtomicUsize>,
    set_geom_calls: Rc<AtomicUsize>,
}

impl MetricCountingProbe {
    fn new(
        w: i32,
        h: i32,
    ) -> (
        WidgetRef,
        Rc<AtomicUsize>,
        Rc<AtomicUsize>,
        Rc<AtomicUsize>,
        Rc<AtomicUsize>,
    ) {
        let hint_queries = Rc::new(AtomicUsize::new(0));
        let min_queries = Rc::new(AtomicUsize::new(0));
        let max_queries = Rc::new(AtomicUsize::new(0));
        let set_geom_calls = Rc::new(AtomicUsize::new(0));
        let probe = Self {
            base: WidgetBase::new(),
            hint: RefCell::new(Size::new(w, h)),
            hint_queries: Rc::clone(&hint_queries),
            min_queries: Rc::clone(&min_queries),
            max_queries: Rc::clone(&max_queries),
            set_geom_calls: Rc::clone(&set_geom_calls),
        };
        (
            Rc::new(RefCell::new(Box::new(probe))),
            hint_queries,
            min_queries,
            max_queries,
            set_geom_calls,
        )
    }
}

impl QObject for MetricCountingProbe {
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

impl Widget for MetricCountingProbe {
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
        self.set_geom_calls.fetch_add(1, Ordering::SeqCst);
        self.base.set_geometry(rect);
    }
    fn size_hint(&self) -> Size {
        self.hint_queries.fetch_add(1, Ordering::SeqCst);
        *self.hint.borrow()
    }
    fn minimum_size_hint(&self) -> Size {
        self.min_queries.fetch_add(1, Ordering::SeqCst);
        Size::new(10, 10)
    }
    fn maximum_size(&self) -> Size {
        self.max_queries.fetch_add(1, Ordering::SeqCst);
        Size::new(16777215, 16777215)
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

/// A container that, like `QWidget::setGeometry` (qwidget.cpp:7328), asks for no repaint when
/// its geometry did not change, and counts the repaint requests it gets.
struct UpdateCountingContainer {
    base: WidgetBase,
    updates: Rc<AtomicUsize>,
}

impl UpdateCountingContainer {
    fn with_layout(layout: Box<dyn Layout>) -> (WidgetRef, Rc<AtomicUsize>) {
        let updates = Rc::new(AtomicUsize::new(0));
        let mut container = Self {
            base: WidgetBase::new(),
            updates: Rc::clone(&updates),
        };
        container.set_layout(layout);
        (Rc::new(RefCell::new(Box::new(container))), updates)
    }
}

impl QObject for UpdateCountingContainer {
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

impl Widget for UpdateCountingContainer {
    fn widget_base(&self) -> &WidgetBase {
        &self.base
    }
    fn id(&self) -> ObjectId {
        self.base.object_data.id()
    }
    fn geometry(&self) -> Rect {
        self.base.geometry.get()
    }
    fn set_geometry(&self, rect: Rect) {
        if self.base.geometry.get() != rect {
            self.base.geometry.set(rect);
            self.update();
        }
    }
    fn size_hint(&self) -> Size {
        self.base.layout.borrow().as_ref().unwrap().size_hint()
    }
    fn minimum_size_hint(&self) -> Size {
        self.base.layout.borrow().as_ref().unwrap().minimum_size()
    }
    fn maximum_size(&self) -> Size {
        Size::new(16777215, 16777215)
    }
    fn is_visible(&self) -> bool {
        self.base.visible.get()
    }
    fn set_visible(&self, visible: bool) {
        self.base.visible.set(visible);
    }
    fn is_enabled(&self) -> bool {
        true
    }
    fn update(&self) {
        self.updates.fetch_add(1, Ordering::SeqCst);
        self.base.update();
    }
    fn dirty_rect(&self) -> Option<Rect> {
        self.base.dirty.get()
    }
    fn clear_dirty(&self) {
        self.base.dirty.set(None);
    }
    fn layout_mut(&mut self) -> Option<&mut Box<dyn Layout>> {
        self.base.layout.get_mut().as_mut()
    }
    fn layout_ref_mut(&self) -> Option<std::cell::RefMut<'_, Box<dyn Layout>>> {
        std::cell::RefMut::filter_map(self.base.layout.try_borrow_mut().ok()?, |l| l.as_mut()).ok()
    }
    fn set_layout(&mut self, layout: Box<dyn Layout>) {
        *self.base.layout.get_mut() = Some(layout);
    }
    fn parent_widget(&self) -> Option<WidgetWeak> {
        self.base.parent_widget()
    }
    fn set_parent_widget(&self, parent: Option<WidgetWeak>) {
        self.base.set_parent_widget(parent);
    }
    fn window_id(&self) -> Option<ObjectId> {
        self.base.window_id.get()
    }
    fn set_window_id(&self, window_id: Option<ObjectId>) {
        self.base.window_id.set(window_id);
    }
    fn children(&self) -> Vec<WidgetRef> {
        self.base.children.borrow().clone()
    }
    fn add_child(&mut self, child: WidgetRef) {
        self.base.children.borrow_mut().push(child);
    }
    fn remove_child(&mut self, child_id: ObjectId) {
        self.base
            .children
            .borrow_mut()
            .retain(|c| c.borrow().id() != child_id);
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

#[test]
fn test_box_layout_caches_child_metrics_and_does_not_requery_during_resize() {
    let mut layout = BoxLayout::vertical();
    layout.set_spacing(0);

    let (p1, h1, m1, x1, g1) = MetricCountingProbe::new(50, 30);
    let (p2, h2, m2, x2, g2) = MetricCountingProbe::new(60, 40);

    layout.add_widget(p1);
    layout.add_widget(p2);

    // Invalidate and reset all counters
    layout.invalidate();
    h1.store(0, Ordering::SeqCst);
    h2.store(0, Ordering::SeqCst);
    m1.store(0, Ordering::SeqCst);
    m2.store(0, Ordering::SeqCst);
    x1.store(0, Ordering::SeqCst);
    x2.store(0, Ordering::SeqCst);
    g1.store(0, Ordering::SeqCst);
    g2.store(0, Ordering::SeqCst);

    // Initial query: computes setupGeom once
    let hint = layout.size_hint();
    assert_eq!(hint, Size::new(60, 70));
    let initial_h1 = h1.load(Ordering::SeqCst);
    let initial_h2 = h2.load(Ordering::SeqCst);
    let initial_m1 = m1.load(Ordering::SeqCst);
    let initial_m2 = m2.load(Ordering::SeqCst);
    let initial_x1 = x1.load(Ordering::SeqCst);
    let initial_x2 = x2.load(Ordering::SeqCst);
    assert!(initial_h1 > 0, "p1 queried on initial setupGeom");
    assert!(initial_h2 > 0, "p2 queried on initial setupGeom");

    // Second query without changes: must reuse cached metric, 0 additional queries across ALL metrics
    let hint2 = layout.size_hint();
    assert_eq!(hint2, hint);
    assert_eq!(
        h1.load(Ordering::SeqCst),
        initial_h1,
        "p1 size_hint cached on 2nd query"
    );
    assert_eq!(
        h2.load(Ordering::SeqCst),
        initial_h2,
        "p2 size_hint cached on 2nd query"
    );
    assert_eq!(
        m1.load(Ordering::SeqCst),
        initial_m1,
        "p1 min_hint cached on 2nd query"
    );
    assert_eq!(
        m2.load(Ordering::SeqCst),
        initial_m2,
        "p2 min_hint cached on 2nd query"
    );
    assert_eq!(
        x1.load(Ordering::SeqCst),
        initial_x1,
        "p1 max cached on 2nd query"
    );
    assert_eq!(
        x2.load(Ordering::SeqCst),
        initial_x2,
        "p2 max cached on 2nd query"
    );

    // Querying minimum_size must also use cached metrics from setupGeom without re-querying
    let min = layout.minimum_size();
    assert_eq!(min, Size::new(10, 20));
    assert_eq!(
        h1.load(Ordering::SeqCst),
        initial_h1,
        "size_hint cached on minimum_size"
    );
    assert_eq!(
        m1.load(Ordering::SeqCst),
        initial_m1,
        "min_hint cached on minimum_size"
    );
    assert_eq!(
        x1.load(Ordering::SeqCst),
        initial_x1,
        "max cached on minimum_size"
    );

    // Resizing layout (set_geometry) must distribute space without re-querying child size metrics
    layout.set_geometry(Rect::new(0, 0, 200, 300));
    assert_eq!(
        h1.load(Ordering::SeqCst),
        initial_h1,
        "p1 size_hint cached during set_geometry"
    );
    assert_eq!(
        h2.load(Ordering::SeqCst),
        initial_h2,
        "p2 size_hint cached during set_geometry"
    );
    assert_eq!(
        m1.load(Ordering::SeqCst),
        initial_m1,
        "p1 min_hint cached during set_geometry"
    );
    assert_eq!(
        x1.load(Ordering::SeqCst),
        initial_x1,
        "p1 max cached during set_geometry"
    );

    // Each child's set_geometry must be called exactly once per activation pass (no double activation)
    assert_eq!(
        g1.load(Ordering::SeqCst),
        1,
        "p1 set_geometry called exactly once"
    );
    assert_eq!(
        g2.load(Ordering::SeqCst),
        1,
        "p2 set_geometry called exactly once"
    );

    // Second resize (e.g. interactive window drag): still no re-queries
    layout.set_geometry(Rect::new(0, 0, 220, 310));
    assert_eq!(
        h1.load(Ordering::SeqCst),
        initial_h1,
        "p1 size_hint cached on 2nd resize"
    );
    assert_eq!(
        h2.load(Ordering::SeqCst),
        initial_h2,
        "p2 size_hint cached on 2nd resize"
    );
    assert_eq!(
        g1.load(Ordering::SeqCst),
        2,
        "p1 set_geometry called once per resize"
    );
    assert_eq!(
        g2.load(Ordering::SeqCst),
        2,
        "p2 set_geometry called once per resize"
    );

    // Calling activate() again with identical geometry must be a clean no-op
    layout.activate();
    assert_eq!(
        g1.load(Ordering::SeqCst),
        2,
        "redundant activate() is a no-op"
    );
    assert_eq!(
        g2.load(Ordering::SeqCst),
        2,
        "redundant activate() is a no-op"
    );

    // Invalidation: now and only now must child metrics be requeried on next layout/query
    layout.invalidate();
    let hint3 = layout.size_hint();
    assert_eq!(hint3, hint);
    assert_eq!(
        h1.load(Ordering::SeqCst),
        initial_h1 * 2,
        "p1 size_hint requeried after invalidate"
    );
    assert_eq!(
        h2.load(Ordering::SeqCst),
        initial_h2 * 2,
        "p2 size_hint requeried after invalidate"
    );
}

#[test]
fn test_grid_layout_caches_child_metrics_and_does_not_requery_during_resize() {
    let mut layout = GridLayout::new();
    layout.set_horizontal_spacing(0);
    layout.set_vertical_spacing(0);

    let (p1, h1, m1, x1, g1) = MetricCountingProbe::new(50, 30);
    let (p2, h2, m2, x2, g2) = MetricCountingProbe::new(60, 40);

    layout.add_widget(p1, 0, 0);
    layout.add_widget(p2, 1, 0);

    // Invalidate and reset counters
    layout.invalidate();
    h1.store(0, Ordering::SeqCst);
    h2.store(0, Ordering::SeqCst);
    m1.store(0, Ordering::SeqCst);
    m2.store(0, Ordering::SeqCst);
    x1.store(0, Ordering::SeqCst);
    x2.store(0, Ordering::SeqCst);
    g1.store(0, Ordering::SeqCst);
    g2.store(0, Ordering::SeqCst);

    // Initial query
    let hint = layout.size_hint();
    assert_eq!(hint, Size::new(60, 70));
    let initial_h1 = h1.load(Ordering::SeqCst);
    let initial_h2 = h2.load(Ordering::SeqCst);
    assert!(initial_h1 > 0, "p1 queried on initial setup_layout_data");
    assert!(initial_h2 > 0, "p2 queried on initial setup_layout_data");

    // Second query: must reuse cached metric
    let hint2 = layout.size_hint();
    assert_eq!(hint2, hint);
    assert_eq!(
        h1.load(Ordering::SeqCst),
        initial_h1,
        "p1 size_hint cached on 2nd query"
    );
    assert_eq!(
        h2.load(Ordering::SeqCst),
        initial_h2,
        "p2 size_hint cached on 2nd query"
    );

    // Resizing grid layout must distribute space without re-querying child size metrics
    layout.set_geometry(Rect::new(0, 0, 200, 300));
    assert_eq!(
        h1.load(Ordering::SeqCst),
        initial_h1,
        "p1 size_hint cached during set_geometry"
    );
    assert_eq!(
        h2.load(Ordering::SeqCst),
        initial_h2,
        "p2 size_hint cached during set_geometry"
    );
    assert_eq!(g1.load(Ordering::SeqCst), 1, "p1 set_geometry called once");
    assert_eq!(g2.load(Ordering::SeqCst), 1, "p2 set_geometry called once");

    // Second resize: still no re-queries
    layout.set_geometry(Rect::new(0, 0, 220, 310));
    assert_eq!(
        h1.load(Ordering::SeqCst),
        initial_h1,
        "p1 size_hint cached on 2nd resize"
    );
    assert_eq!(
        h2.load(Ordering::SeqCst),
        initial_h2,
        "p2 size_hint cached on 2nd resize"
    );
    assert_eq!(
        g1.load(Ordering::SeqCst),
        2,
        "p1 set_geometry called once per resize"
    );
    assert_eq!(
        g2.load(Ordering::SeqCst),
        2,
        "p2 set_geometry called once per resize"
    );

    // Redundant activate is a no-op
    layout.activate();
    assert_eq!(
        g1.load(Ordering::SeqCst),
        2,
        "redundant activate() is a no-op"
    );

    // Invalidation: now requeried
    layout.invalidate();
    let hint3 = layout.size_hint();
    assert_eq!(hint3, hint);
    assert_eq!(
        h1.load(Ordering::SeqCst),
        initial_h1 * 2,
        "p1 size_hint requeried after invalidate"
    );
    assert_eq!(
        h2.load(Ordering::SeqCst),
        initial_h2 * 2,
        "p2 size_hint requeried after invalidate"
    );
}

#[test]
fn test_item_set_geometry_respects_smart_max_for_fixed_maximum_preferred() {
    // 1. Fixed policy: must clamp to size hint even under Alignment::NONE
    let mut layout_fixed = BoxLayout::horizontal();
    let (p_fixed, _, _, _, _) = MetricCountingProbe::new(50, 30);
    p_fixed
        .borrow()
        .widget_base()
        .set_size_policy(QSizePolicy::new(Policy::Fixed, Policy::Fixed));
    layout_fixed.add_widget(Rc::clone(&p_fixed));
    layout_fixed.set_geometry(Rect::new(0, 0, 200, 300));

    let g_fixed = p_fixed.borrow().geometry();
    assert_eq!(
        g_fixed.width, 50,
        "Fixed policy widget width must clamp to hint width"
    );
    assert_eq!(
        g_fixed.height, 30,
        "Fixed policy widget height must clamp to hint height"
    );

    // 2. Maximum policy: must not expand past size hint
    let mut layout_max = BoxLayout::horizontal();
    let (p_max, _, _, _, _) = MetricCountingProbe::new(50, 30);
    p_max
        .borrow()
        .widget_base()
        .set_size_policy(QSizePolicy::new(Policy::Maximum, Policy::Maximum));
    layout_max.add_widget(Rc::clone(&p_max));
    layout_max.set_geometry(Rect::new(0, 0, 200, 300));

    let g_max = p_max.borrow().geometry();
    assert_eq!(
        g_max.width, 50,
        "Maximum policy widget width must not exceed hint width"
    );
    assert_eq!(
        g_max.height, 30,
        "Maximum policy widget height must not exceed hint height"
    );

    // 3. Preferred policy: must expand to fill available space
    let mut layout_pref = BoxLayout::horizontal();
    let (p_pref, _, _, _, _) = MetricCountingProbe::new(50, 30);
    p_pref
        .borrow()
        .widget_base()
        .set_size_policy(QSizePolicy::new(Policy::Preferred, Policy::Preferred));
    layout_pref.add_widget(Rc::clone(&p_pref));
    layout_pref.set_geometry(Rect::new(0, 0, 200, 300));

    let g_pref = p_pref.borrow().geometry();
    assert_eq!(
        g_pref.width, 200,
        "Preferred policy widget width must expand to fill cell"
    );
    assert_eq!(
        g_pref.height, 300,
        "Preferred policy widget height must expand to fill cell"
    );
}

#[test]
fn test_window_resize_event_reuses_layout_metric_cache() {
    let mut layout = BoxLayout::vertical();
    layout.set_spacing(0);

    let (p1, h1, _, _, _) = MetricCountingProbe::new(50, 30);
    let (p2, h2, _, _, _) = MetricCountingProbe::new(60, 40);
    layout.add_widget(Rc::clone(&p1));
    layout.add_widget(Rc::clone(&p2));

    let container: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::new())));
    container.borrow_mut().set_layout(Box::new(layout));

    let mut win = window::Window::new(
        "ResizeTest",
        Rect::new(0, 0, 200, 200),
        ferrokit_platform::window::WindowFlags::FRAMELESS,
    )
    .unwrap();
    win.set_root_widget(container);
    win.render_and_present();

    let initial_h1 = h1.load(Ordering::SeqCst);
    let initial_h2 = h2.load(Ordering::SeqCst);
    assert!(initial_h1 > 0, "metrics queried on initial show");

    // Simulate native resize event via Window::set_geometry / WindowSystemEvent::Resize path
    win.set_geometry(Rect::new(0, 0, 300, 400));

    // The window resized, layout ran, and child widgets' size metrics were NOT re-queried!
    assert_eq!(
        h1.load(Ordering::SeqCst),
        initial_h1,
        "window resize must not re-query child size hint"
    );
    assert_eq!(
        h2.load(Ordering::SeqCst),
        initial_h2,
        "window resize must not re-query child size hint"
    );

    // Second resize
    win.set_geometry(Rect::new(0, 0, 350, 450));
    assert_eq!(
        h1.load(Ordering::SeqCst),
        initial_h1,
        "second window resize must not re-query child size hint"
    );
    assert_eq!(
        h2.load(Ordering::SeqCst),
        initial_h2,
        "second window resize must not re-query child size hint"
    );
}

/// A real application tree: root layout -> `StackedWidget` -> page container -> labels. A window
/// resize changes the page's size; the page's layout must lay out again with its cached metrics.
#[test]
fn test_window_resize_through_a_stacked_widget_reuses_the_page_metric_cache() {
    let mut page_layout = BoxLayout::vertical();
    page_layout.set_margins(ferrokit_gui::geometry::primitives::Margins::new(0, 0, 0, 0));
    let (p1, h1, _, _, _) = MetricCountingProbe::new(50, 30);
    page_layout.add_widget(Rc::clone(&p1));
    let page: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::new())));
    page.borrow_mut().set_layout(Box::new(page_layout));

    let stack: WidgetRef = Rc::new(RefCell::new(Box::new(stacked::StackedWidget::new())));
    if let Some(s) = stack
        .borrow_mut()
        .as_any_mut()
        .downcast_mut::<stacked::StackedWidget>()
    {
        s.add_widget(Rc::clone(&page));
        s.set_current_index(0);
    }
    let mut root_layout = BoxLayout::vertical();
    root_layout.set_margins(ferrokit_gui::geometry::primitives::Margins::new(0, 0, 0, 0));
    root_layout.add_widget(Rc::clone(&stack));
    let root: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::new())));
    root.borrow_mut().set_layout(Box::new(root_layout));

    let mut win = window::Window::new(
        "StackedResizeTest",
        Rect::new(0, 0, 200, 200),
        ferrokit_platform::window::WindowFlags::FRAMELESS,
    )
    .unwrap();
    win.set_root_widget(root);
    win.render_and_present();
    let settled = h1.load(Ordering::SeqCst);
    assert!(settled > 0, "metrics queried on initial show");

    win.set_geometry(Rect::new(0, 0, 300, 400));
    assert_eq!(
        h1.load(Ordering::SeqCst),
        settled,
        "a resize through the stacked widget must not re-query the page's children"
    );
    assert_eq!(
        page.borrow().geometry().width,
        300,
        "the page took the new width"
    );
    assert_eq!(
        p1.borrow().geometry().width,
        300,
        "the page was laid out again"
    );
}

/// Invalidation goes the way Qt's does: a widget whose size hint changed calls `updateGeometry`,
/// the parent layout drops its metrics, and the next pass uses the new hint.
#[test]
fn test_a_changed_size_hint_reaches_the_cached_layout_through_update_geometry() {
    let mut layout = BoxLayout::horizontal();
    layout.set_margins(ferrokit_gui::geometry::primitives::Margins::new(0, 0, 0, 0));
    let (p1, h1, _, _, _) = MetricCountingProbe::new(50, 30);
    p1.borrow()
        .widget_base()
        .set_size_policy(QSizePolicy::new(Policy::Fixed, Policy::Fixed));
    layout.add_widget(Rc::clone(&p1));
    let root: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::new())));
    root.borrow_mut().set_layout(Box::new(layout));

    let mut win = window::Window::new(
        "HintChangeTest",
        Rect::new(0, 0, 200, 200),
        ferrokit_platform::window::WindowFlags::FRAMELESS,
    )
    .unwrap();
    win.set_root_widget(root);
    win.render_and_present();
    assert_eq!(p1.borrow().geometry().width, 50);

    // A resize alone keeps the cached hint.
    let settled = h1.load(Ordering::SeqCst);
    win.set_geometry(Rect::new(0, 0, 250, 200));
    assert_eq!(h1.load(Ordering::SeqCst), settled);

    // The content changes: new hint, then `updateGeometry` as every ferrokit setter does.
    {
        let w = p1.borrow();
        let probe = w.as_any().downcast_ref::<MetricCountingProbe>().unwrap();
        *probe.hint.borrow_mut() = Size::new(90, 30);
        probe.widget_base().update_geometry();
    }
    win.render_and_present();
    assert!(
        h1.load(Ordering::SeqCst) > settled,
        "the layout re-queried after updateGeometry"
    );
    assert_eq!(
        p1.borrow().geometry().width,
        90,
        "the new hint reached the geometry"
    );
}

/// An aligned item on an `Ignored` axis is cut to the widget's own size hint, not to the item's
/// (which `Ignored` makes 0) qlayoutitem.cpp:432-447, 670-673, and keeps that after a
/// resize that reuses the cached limits.
#[test]
fn test_an_aligned_item_on_an_ignored_axis_takes_the_widget_size_hint() {
    let (aligned, _, _, _, _) = MetricCountingProbe::new(50, 30);
    aligned
        .borrow()
        .widget_base()
        .set_size_policy(QSizePolicy::new(Policy::Ignored, Policy::Ignored));
    let mut layout = BoxLayout::vertical();
    layout.add_widget_aligned(Rc::clone(&aligned), 0, Alignment::LEFT | Alignment::TOP);
    let root: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::new())));
    root.borrow_mut().set_layout(Box::new(layout));

    let mut win = window::Window::new(
        "AlignedIgnoredTest",
        Rect::new(0, 0, 200, 200),
        ferrokit_platform::window::WindowFlags::FRAMELESS,
    )
    .unwrap();
    win.set_root_widget(root);
    win.render_and_present();
    assert_eq!(aligned.borrow().geometry().size(), Size::new(50, 30));

    win.set_geometry(Rect::new(0, 0, 300, 260));
    win.render_and_present();
    assert_eq!(
        aligned.borrow().geometry().size(),
        Size::new(50, 30),
        "the cached aligned size survives a resize"
    );
}

/// The aligned size is cached with the other metrics, so it must be dropped like them: a new size
/// hint announced through `updateGeometry` reaches an aligned item's geometry.
#[test]
fn test_a_changed_size_hint_reaches_an_aligned_item() {
    let (aligned, _, _, _, _) = MetricCountingProbe::new(50, 30);
    let mut layout = BoxLayout::vertical();
    layout.add_widget_aligned(Rc::clone(&aligned), 0, Alignment::LEFT | Alignment::TOP);
    let root: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::new())));
    root.borrow_mut().set_layout(Box::new(layout));

    let mut win = window::Window::new(
        "AlignedHintChangeTest",
        Rect::new(0, 0, 200, 200),
        ferrokit_platform::window::WindowFlags::FRAMELESS,
    )
    .unwrap();
    win.set_root_widget(root);
    win.render_and_present();
    assert_eq!(aligned.borrow().geometry().size(), Size::new(50, 30));

    {
        let w = aligned.borrow();
        let probe = w.as_any().downcast_ref::<MetricCountingProbe>().unwrap();
        *probe.hint.borrow_mut() = Size::new(90, 40);
        probe.widget_base().update_geometry();
    }
    win.render_and_present();
    assert_eq!(
        aligned.borrow().geometry().size(),
        Size::new(90, 40),
        "the aligned item took the new hint"
    );
}

/// root (VBox) -> [mid container (VBox) -> leaf, sibling]. A resize lays out `mid` again, which
/// must not invalidate the root layout's metrics: `QLayout` handles a resize of an activated
/// layout with `doResize` only, and `mw->updateGeometry()` runs only at the end of `activate()`
/// after an invalidation qlayout.cpp:528-533, 983-995, 1131. The sibling's queries
/// show whether the root layout rebuilt its metrics.
#[test]
fn test_a_resize_keeps_the_ancestor_metric_caches_of_a_nested_layout() {
    let (leaf, leaf_hint, leaf_min, leaf_max, _) = MetricCountingProbe::new(50, 30);
    let mut mid_layout = BoxLayout::vertical();
    mid_layout.set_margins(ferrokit_gui::geometry::primitives::Margins::new(0, 0, 0, 0));
    mid_layout.add_widget(Rc::clone(&leaf));
    let mid: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::new())));
    mid.borrow_mut().set_layout(Box::new(mid_layout));

    let (sibling, sib_hint, sib_min, sib_max, _) = MetricCountingProbe::new(40, 20);
    let mut root_layout = BoxLayout::vertical();
    root_layout.set_margins(ferrokit_gui::geometry::primitives::Margins::new(0, 0, 0, 0));
    root_layout.add_widget(Rc::clone(&mid));
    root_layout.add_widget(Rc::clone(&sibling));
    let root: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::new())));
    root.borrow_mut().set_layout(Box::new(root_layout));

    let mut win = window::Window::new(
        "NestedResizeTest",
        Rect::new(0, 0, 200, 200),
        ferrokit_platform::window::WindowFlags::FRAMELESS,
    )
    .unwrap();
    win.set_root_widget(root);
    win.render_and_present();
    win.render_and_present();
    let counts = || {
        [
            &leaf_hint, &leaf_min, &leaf_max, &sib_hint, &sib_min, &sib_max,
        ]
        .map(|c| c.load(Ordering::SeqCst))
    };
    let settled = counts();

    for width in [260, 320] {
        win.set_geometry(Rect::new(0, 0, width, 300));
        win.render_and_present();
        assert_eq!(
            counts(),
            settled,
            "a resize to width {width} must not rebuild any layout's metrics (leaf h/min/max, sibling h/min/max)"
        );
        assert_eq!(
            leaf.borrow().geometry().width,
            width,
            "the nested layout ran"
        );
    }
}

/// An aligned item is cut to its size hint, which `QWidgetItemV2` caches with the other metrics
/// qlayoutitem.cpp:432-447, 778-789: laying it out again at a new size asks the widget
/// nothing.
#[test]
fn test_a_resize_does_not_requery_an_aligned_item() {
    let (aligned, hint_q, min_q, max_q, _) = MetricCountingProbe::new(50, 30);
    let mut layout = BoxLayout::vertical();
    layout.add_widget_aligned(Rc::clone(&aligned), 0, Alignment::LEFT | Alignment::TOP);
    let root: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::new())));
    root.borrow_mut().set_layout(Box::new(layout));

    let mut win = window::Window::new(
        "AlignedResizeTest",
        Rect::new(0, 0, 200, 200),
        ferrokit_platform::window::WindowFlags::FRAMELESS,
    )
    .unwrap();
    win.set_root_widget(root);
    win.render_and_present();
    let counts = || [&hint_q, &min_q, &max_q].map(|c| c.load(Ordering::SeqCst));
    let settled = counts();

    win.set_geometry(Rect::new(0, 0, 300, 400));
    win.render_and_present();
    assert_eq!(
        counts(),
        settled,
        "an aligned item's hint/min/max must come from the layout's cache"
    );
    assert_eq!(
        aligned.borrow().geometry().size(),
        Size::new(50, 30),
        "aligned item cut to its hint"
    );
}

/// root (VBox) -> [container aligned LEFT (VBox) -> leaf]. The container's width is cut to its
/// size hint; its height follows the cell. Qt sends a resize, and so lays the container's layout
/// out again and repaints it, only when the widget's own size changed: `setGeometry_sys`
/// compares the clamped widget size, not the rectangle the parent layout offered
/// qwidget.cpp:7297-7329; qlayout.cpp:528-530.
#[test]
fn test_an_aligned_container_is_laid_out_again_only_when_its_own_size_changes() {
    let (leaf, _, _, _, leaf_set_geom) = MetricCountingProbe::new(50, 30);
    let mut inner = BoxLayout::vertical();
    inner.add_widget(Rc::clone(&leaf));
    let (container, container_updates) = UpdateCountingContainer::with_layout(Box::new(inner));

    let mut layout = BoxLayout::vertical();
    layout.add_widget_aligned(Rc::clone(&container), 0, Alignment::LEFT);
    let root: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::new())));
    root.borrow_mut().set_layout(Box::new(layout));

    let mut win = window::Window::new(
        "AlignedContainerResizeTest",
        Rect::new(0, 0, 200, 200),
        ferrokit_platform::window::WindowFlags::FRAMELESS,
    )
    .unwrap();
    win.set_root_widget(root);
    win.render_and_present();
    win.render_and_present();
    let container_size = container.borrow().geometry().size();
    let leaf_height = leaf.borrow().geometry().height;
    let settled = leaf_set_geom.load(Ordering::SeqCst);
    let settled_updates = container_updates.load(Ordering::SeqCst);

    for width in [260, 320] {
        win.set_geometry(Rect::new(0, 0, width, 200));
        win.render_and_present();
        assert_eq!(
            container.borrow().geometry().size(),
            container_size,
            "a wider cell leaves the aligned container's size alone"
        );
        assert_eq!(
            leaf_set_geom.load(Ordering::SeqCst),
            settled,
            "width {width}: the container kept its size, so its layout must not run again"
        );
        assert_eq!(
            container_updates.load(Ordering::SeqCst),
            settled_updates,
            "width {width}: the container kept its geometry, so nothing may ask it to repaint"
        );
    }

    win.set_geometry(Rect::new(0, 0, 320, 300));
    win.render_and_present();
    let taller = container.borrow().geometry().height;
    assert!(
        taller > container_size.height,
        "the container follows the cell's height"
    );
    assert_eq!(
        leaf.borrow().geometry().height,
        leaf_height + (taller - container_size.height),
        "the container's size changed, so its layout ran and the leaf got the extra height"
    );
}
