// SPDX-License-Identifier: LGPL-3.0-only
use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::object::{ObjectId, QObject};
use ferrokit_gui::geometry::primitives::{Margins, Rect, Size};
use ferrokit_gui::text::font::Font;
use ferrokit_gui::text::font_metrics::FontMetrics;
use ferrokit_platform::WindowFlags;
use ferrokit_widgets::*;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

struct DpiObserverWidget {
    base: WidgetBase,
    dpi_change_count: Arc<AtomicU32>,
    last_dpr: Arc<parking_lot::Mutex<f32>>,
}

impl DpiObserverWidget {
    fn new(dpi_change_count: Arc<AtomicU32>, last_dpr: Arc<parking_lot::Mutex<f32>>) -> Self {
        Self {
            base: WidgetBase::with_geometry(Rect::new(0, 0, 100, 30)),
            dpi_change_count,
            last_dpr,
        }
    }
}

impl QObject for DpiObserverWidget {
    fn object_data(&self) -> &ferrokit_core::object::ObjectData {
        &self.base.object_data
    }
    fn object_data_mut(&mut self) -> &mut ferrokit_core::object::ObjectData {
        &mut self.base.object_data
    }
    fn event(&mut self, _event: &mut Event) -> bool {
        false
    }
}

impl Widget for DpiObserverWidget {
    fn widget_base(&self) -> &ferrokit_widgets::widget::WidgetBase {
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
        Size::new(100, 30)
    }
    fn minimum_size(&self) -> Size {
        Size::new(50, 20)
    }
    fn maximum_size(&self) -> Size {
        Size::new(300, 100)
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
    fn clear_dirty(&self) {
        self.base.clear_dirty();
    }
    fn dirty_rect(&self) -> Option<Rect> {
        self.base.dirty_rect()
    }
    fn parent_widget(&self) -> Option<WidgetWeak> {
        self.base.parent_widget()
    }
    fn set_parent_widget(&self, parent: Option<WidgetWeak>) {
        self.base.set_parent_widget(parent);
    }
    fn window_id(&self) -> Option<ObjectId> {
        self.base.window_id()
    }
    fn set_window_id(&self, window_id: Option<ObjectId>) {
        self.base.set_window_id(window_id);
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
    fn dpi_changed_event(&mut self, _old_dpr: f32, new_dpr: f32) {
        self.dpi_change_count.fetch_add(1, Ordering::SeqCst);
        *self.last_dpr.lock() = new_dpr;
        self.base.update();
    }
    fn paint_event(&mut self, _painter: &mut ferrokit_gui::paint::Painter) {}
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

#[test]
fn test_per_monitor_v2_dpi_drag_propagation() {
    // 1. Initialize Window at 100% scale (96 DPI)
    let mut window = Window::new(
        "Per-Monitor V2 DPI Sync Test",
        Rect::new(100, 100, 400, 300),
        WindowFlags::FRAMELESS,
    )
    .expect("window create");

    let count = Arc::new(AtomicU32::new(0));
    let last_dpr = Arc::new(parking_lot::Mutex::new(1.0));

    let observer: WidgetRef = Rc::new(RefCell::new(Box::new(DpiObserverWidget::new(
        Arc::clone(&count),
        Arc::clone(&last_dpr),
    ))));

    let label: WidgetRef = Rc::new(RefCell::new(Box::new(Label::new(
        "Claude HUD Real-Time Monitor",
    ))));

    let container: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::new())));
    let mut layout = BoxLayout::vertical();
    layout.set_margins(Margins::new(10, 10, 10, 10));
    layout.set_spacing(8);
    layout.add_widget(Rc::clone(&label));
    layout.add_widget(Rc::clone(&observer));
    container.borrow_mut().set_layout(Box::new(layout));

    window.set_root_widget(container);

    // Initial check at 96 DPI
    assert_eq!(count.load(Ordering::SeqCst), 0);

    // 2. Simulate dragging window across monitor boundary: 100% (96 DPI) -> 175% (168 DPI)
    let mut dpi_event_175 = Event::new_spontaneous(EventKind::DpiChanged {
        dpi_x: 168,
        dpi_y: 168,
    });
    let consumed = window.event(&mut dpi_event_175);
    assert!(consumed);

    // Verify observer received the event and new DPR is 1.75
    assert_eq!(count.load(Ordering::SeqCst), 1);
    let dpr = *last_dpr.lock();
    assert!((dpr - 1.75).abs() < 1e-4, "DPR must be 1.75, got {}", dpr);

    // 3. Verify FontMetrics horizontal advance exact calculation
    let font = Font::new("Segoe UI", 14.0);
    let metrics = FontMetrics::from_font(&font);
    let text = "Claude HUD Real-Time Monitor";
    let advance_exact = metrics.horizontal_advance_exact(text, &font);
    let advance_heuristic = metrics.horizontal_advance(text, &font);

    assert!(advance_exact > 0.0, "Exact advance must be positive");
    assert!(advance_heuristic > 0.0, "Heuristic advance must be positive");

    // 4. Simulate dragging back: 175% (168 DPI) -> 100% (96 DPI)
    let mut dpi_event_100 = Event::new_spontaneous(EventKind::DpiChanged {
        dpi_x: 96,
        dpi_y: 96,
    });
    let consumed_back = window.event(&mut dpi_event_100);
    assert!(consumed_back);

    assert_eq!(count.load(Ordering::SeqCst), 2);
    let restored_dpr = *last_dpr.lock();
    assert!((restored_dpr - 1.0).abs() < 1e-4, "DPR must be restored to 1.0, got {}", restored_dpr);
}
