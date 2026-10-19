// SPDX-License-Identifier: LGPL-3.0-only
//! Parity test for `EventTreeDispatcher` DPI propagation.
//! Successive DpiChanged events dispatched through `EventTreeDispatcher` must pass the actual
//! previous DPR as `old_dpr`, rather than hardcoded 1.0.

use std::cell::RefCell;
use std::rc::Rc;

use std::sync::Arc;

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::object::{ObjectData, ObjectId, QObject};
use ferrokit_gui::geometry::primitives::{Rect, Size};
use ferrokit_gui::paint::Painter;
use ferrokit_widgets::hit_test::EventTreeDispatcher;
use ferrokit_widgets::widget::{Widget, WidgetBase, WidgetRef};

struct DpiHistoryWidget {
    base: WidgetBase,
    history: Arc<parking_lot::Mutex<Vec<(f32, f32)>>>,
}

impl DpiHistoryWidget {
    fn new(history: Arc<parking_lot::Mutex<Vec<(f32, f32)>>>) -> Self {
        Self {
            base: WidgetBase::new(),
            history,
        }
    }
}

impl QObject for DpiHistoryWidget {
    fn object_data(&self) -> &ObjectData {
        &self.base.object_data
    }
    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.base.object_data)
    }
}

impl Widget for DpiHistoryWidget {
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
        Size::new(100, 100)
    }
    fn is_visible(&self) -> bool {
        self.base.is_visible()
    }
    fn set_visible(&self, visible: bool) {
        self.base.set_visible(visible);
    }
    fn is_enabled(&self) -> bool {
        self.base.is_enabled()
    }
    fn update(&self) {
        self.base.update();
    }
    fn dirty_rect(&self) -> Option<Rect> {
        self.base.dirty_rect()
    }
    fn clear_dirty(&self) {
        self.base.clear_dirty();
    }
    fn layout(&self) -> Option<&dyn ferrokit_widgets::layout::Layout> {
        None
    }
    fn layout_mut(&mut self) -> Option<&mut Box<dyn ferrokit_widgets::layout::Layout>> {
        None
    }
    fn set_layout(&mut self, _layout: Box<dyn ferrokit_widgets::layout::Layout>) {}
    fn parent_widget(&self) -> Option<ferrokit_widgets::widget::WidgetWeak> {
        self.base.parent_widget()
    }
    fn set_parent_widget(&self, parent: Option<ferrokit_widgets::widget::WidgetWeak>) {
        self.base.set_parent_widget(parent);
    }
    fn window_id(&self) -> Option<ObjectId> {
        self.base.window_id()
    }
    fn set_window_id(&self, id: Option<ObjectId>) {
        self.base.set_window_id(id);
    }
    fn children(&self) -> Vec<WidgetRef> {
        self.base.children()
    }
    fn add_child(&mut self, child: WidgetRef) {
        self.base.add_child(child);
    }
    fn remove_child(&mut self, child_id: ObjectId) {
        self.base.remove_child(child_id);
    }
    fn dpi_changed_event(&mut self, old_dpr: f32, new_dpr: f32) {
        self.history.lock().push((old_dpr, new_dpr));
    }
    fn paint_event(&mut self, _painter: &mut Painter) {}
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

#[test]
fn successive_dpi_changes_preserve_old_dpr() {
    let history = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let root: WidgetRef = Rc::new(RefCell::new(Box::new(DpiHistoryWidget::new(Arc::clone(
        &history,
    )))));

    let mut dispatcher = EventTreeDispatcher::new();

    // 1st change: 96 DPI -> 120 DPI (1.25 DPR)
    let mut ev1 = Event::new_spontaneous(EventKind::DpiChanged {
        dpi_x: 120,
        dpi_y: 120,
    });
    dispatcher.dispatch_event(&root, &mut ev1);

    // 2nd change: 120 DPI -> 144 DPI (1.5 DPR)
    let mut ev2 = Event::new_spontaneous(EventKind::DpiChanged {
        dpi_x: 144,
        dpi_y: 144,
    });
    dispatcher.dispatch_event(&root, &mut ev2);

    let recorded = history.lock().clone();
    assert_eq!(recorded.len(), 2);
    assert_eq!(recorded[0], (1.0, 1.25), "first transition: 1.0 -> 1.25");
    assert_eq!(
        recorded[1],
        (1.25, 1.5),
        "second transition: 1.25 -> 1.5"
    );
}
