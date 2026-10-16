// SPDX-License-Identifier: LGPL-3.0-only
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use ferrokit_core::object::{ObjectData, ObjectId, QObject};
use ferrokit_gui::geometry::primitives::{Rect, RectF};
use ferrokit_gui::paint::pixmap::Pixmap;
use ferrokit_gui::paint::{Brush, Painter, Pen};
use ferrokit_gui::tiny_skia::Color;
use ferrokit_widgets::{CustomWidget, Widget, WidgetBase, WidgetRef};

// -----------------------------------------------------------------------------
// 1. Test custom struct overriding Widget::paint_event virtual method
// -----------------------------------------------------------------------------

struct CustomPaintedGauge {
    base: WidgetBase,
    painted_flag: Arc<AtomicBool>,
}

impl CustomPaintedGauge {
    fn new(flag: Arc<AtomicBool>) -> Self {
        Self {
            base: WidgetBase::with_geometry(Rect::new(0, 0, 100, 100)),
            painted_flag: flag,
        }
    }
}

impl QObject for CustomPaintedGauge {
    fn object_data(&self) -> &ObjectData {
        &self.base.object_data
    }
    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.base.object_data)
    }
}

impl Widget for CustomPaintedGauge {
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
    fn is_visible(&self) -> bool {
        self.base.is_visible()
    }
    fn set_visible(&self, visible: bool) {
        self.base.set_visible(visible);
    }
    fn is_enabled(&self) -> bool {
        self.base.is_enabled()
    }
    fn update(&self) {}
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
    fn parent_widget(&self) -> Option<ferrokit_widgets::WidgetWeak> {
        self.base.parent_widget()
    }
    fn set_parent_widget(&self, parent: Option<ferrokit_widgets::WidgetWeak>) {
        self.base.set_parent_widget(parent);
    }
    fn window_id(&self) -> Option<ObjectId> {
        self.base.window_id()
    }
    fn set_window_id(&self, window_id: Option<ObjectId>) {
        self.base.set_window_id(window_id);
    }
    fn children(&self) -> Vec<WidgetRef> {
        Vec::new()
    }
    fn add_child(&mut self, _child: WidgetRef) {}
    fn remove_child(&mut self, _child_id: ObjectId) {}

    // Overriding the virtual paint_event without dirty_rect
    fn paint_event(&mut self, painter: &mut Painter) {
        self.painted_flag.store(true, Ordering::SeqCst);
        painter.set_brush(Brush::Color(Color::from_rgba8(255, 0, 0, 255)));
        painter.draw_rect(RectF::new(0.0, 0.0, 50.0, 50.0));
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

#[test]
fn test_widget_virtual_paint_event_override() {
    let flag = Arc::new(AtomicBool::new(false));
    let mut gauge = CustomPaintedGauge::new(Arc::clone(&flag));

    let mut pixmap = Pixmap::new(100, 100).expect("Pixmap 建立失敗");
    let mut painter = Painter::begin(&mut pixmap);

    gauge.paint_event(&mut painter);
    assert!(flag.load(Ordering::SeqCst), "自訂 paint_event 應成功執行");

    // 檢查繪圖輸出 (50x50 紅色區塊)
    let data = pixmap.data();
    // 座標 (25, 25) 應為紅色 RGBA: [255, 0, 0, 255]
    let idx = (25 * 100 + 25) * 4;
    assert_eq!(data[idx], 255);
    assert_eq!(data[idx + 1], 0);
    assert_eq!(data[idx + 2], 0);
    assert_eq!(data[idx + 3], 255);
}

// -----------------------------------------------------------------------------
// 2. Test EmptyWidget / CustomWidget closure-based paint handler
// -----------------------------------------------------------------------------

#[test]
fn test_custom_widget_paint_handler_closure() {
    let mut widget = CustomWidget::with_geometry(Rect::new(0, 0, 80, 80));
    let executed = Arc::new(AtomicBool::new(false));
    let executed_clone = Arc::clone(&executed);

    widget.set_paint_handler(move |painter| {
        executed_clone.store(true, Ordering::SeqCst);
        painter.set_pen(Pen::from_rgba8(0, 255, 0, 255, 2.0));
        painter.draw_line(
            ferrokit_gui::geometry::primitives::PointF::new(0.0, 0.0),
            ferrokit_gui::geometry::primitives::PointF::new(80.0, 80.0),
        );
    });

    let mut pixmap = Pixmap::new(80, 80).expect("Pixmap 建立失敗");
    let mut painter = Painter::begin(&mut pixmap);

    widget.paint_event(&mut painter);
    assert!(executed.load(Ordering::SeqCst), "自訂繪製閉包應被觸發");
}

