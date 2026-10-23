// SPDX-License-Identifier: LGPL-3.0-only
use crate::focus::FocusPolicy;
use crate::layout::Layout;
use crate::size_policy::{Policy, QSizePolicy};
use crate::widget::{Widget, WidgetBase, WidgetRef, WidgetWeak};
use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::object::{ObjectData, ObjectId, QObject};
use ferrokit_core::signal::Signal;
use ferrokit_gui::geometry::primitives::{Point, Rect, RectF, Size};
use ferrokit_gui::paint::brush::Brush;
use ferrokit_gui::paint::painter::{Painter, RenderHint};
use ferrokit_gui::tiny_skia::Color;

/// Orientation of a scroll bar or slider (`Qt::Orientation`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
    Horizontal,
    Vertical,
}

/// Scrollbar policy for a scroll area (`Qt::ScrollBarPolicy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScrollBarPolicy {
    #[default]
    AsNeeded,
    AlwaysOn,
    AlwaysOff,
}

/// Standalone scrollbar control (`QScrollBar`).
pub struct ScrollBar {
    base: WidgetBase,
    orientation: Orientation,
    minimum: std::cell::Cell<i32>,
    maximum: std::cell::Cell<i32>,
    value: std::cell::Cell<i32>,
    page_step: std::cell::Cell<i32>,
    single_step: std::cell::Cell<i32>,

    dragging: std::cell::Cell<bool>,
    drag_start_coord: std::cell::Cell<i32>,
    drag_start_value: std::cell::Cell<i32>,
    hovered: std::cell::Cell<bool>,

    // Colors
    track_color: Color,
    thumb_color: Color,
    thumb_hover_color: Color,

    pub value_changed: Signal<i32>,
}

impl ScrollBar {
    pub fn new(orientation: Orientation) -> Self {
        let base = WidgetBase::new();
        base.set_focus_policy(FocusPolicy::NoFocus);

        let (w, h) = match orientation {
            Orientation::Horizontal => (100, 10),
            Orientation::Vertical => (10, 100),
        };
        base.set_geometry(Rect::new(0, 0, w, h));
        let value_changed = Signal::with_owner(&base.object_data);

        Self {
            base,
            orientation,
            minimum: std::cell::Cell::new(0),
            maximum: std::cell::Cell::new(100),
            value: std::cell::Cell::new(0),
            page_step: std::cell::Cell::new(10),
            single_step: std::cell::Cell::new(1),

            dragging: std::cell::Cell::new(false),
            drag_start_coord: std::cell::Cell::new(0),
            drag_start_value: std::cell::Cell::new(0),
            hovered: std::cell::Cell::new(false),

            track_color: Color::from_rgba8(240, 240, 240, 120),
            thumb_color: Color::from_rgba8(170, 170, 170, 180),
            thumb_hover_color: Color::from_rgba8(130, 130, 130, 220),

            value_changed,
        }
    }

    pub fn orientation(&self) -> Orientation {
        self.orientation
    }

    pub fn minimum(&self) -> i32 {
        self.minimum.get()
    }

    pub fn maximum(&self) -> i32 {
        self.maximum.get()
    }

    pub fn value(&self) -> i32 {
        self.value.get()
    }

    pub fn page_step(&self) -> i32 {
        self.page_step.get()
    }

    pub fn single_step(&self) -> i32 {
        self.single_step.get()
    }

    pub fn set_range(&self, min: i32, max: i32) {
        let new_max = max.max(min);
        self.minimum.set(min);
        self.maximum.set(new_max);
        self.set_value(self.value.get());
    }

    pub fn set_page_step(&self, step: i32) {
        self.page_step.set(step.max(1));
        self.update();
    }

    pub fn set_single_step(&self, step: i32) {
        self.single_step.set(step.max(1));
    }

    /// One wheel notch scrolls three single steps; a zero delta scrolls nothing. True when the
    /// value changed (`QAbstractSlider::wheelEvent` ignores a wheel that moves nothing).
    fn scroll_by_wheel(&self, delta: i32) -> bool {
        if delta == 0 {
            return false;
        }
        let steps = self.single_step.get().saturating_mul(3);
        let before = self.value.get();
        self.set_value(before.saturating_add(if delta > 0 { -steps } else { steps }));
        self.value.get() != before
    }

    pub fn set_value(&self, val: i32) {
        let clamped = val.clamp(self.minimum.get(), self.maximum.get());
        if self.value.get() != clamped {
            self.value.set(clamped);
            self.value_changed.emit(&clamped);
            self.update();
        }
    }
    fn track_length(&self) -> i32 {
        match self.orientation {
            Orientation::Horizontal => self.base.geometry().width,
            Orientation::Vertical => self.base.geometry().height,
        }
    }

    fn thumb_geometry(&self) -> (i32, i32) {
        let track_len = self.track_length();
        let range = self.maximum.get().saturating_sub(self.minimum.get()).max(0);
        if range == 0 {
            return (0, track_len);
        }

        let total_steps = range as f32 + self.page_step.get() as f32;
        let thumb_len =
            ((track_len as f32 * self.page_step.get() as f32) / total_steps as f32).round() as i32;
        let thumb_len = thumb_len.clamp(16, track_len.max(16));

        let available_track = (track_len - thumb_len).max(0);
        let thumb_offset = ((available_track as f32
            * self.value.get().saturating_sub(self.minimum.get()) as f32)
            / range as f32)
            .round() as i32;

        (thumb_offset, thumb_len)
    }
}

impl QObject for ScrollBar {
    fn object_data(&self) -> &ObjectData {
        &self.base.object_data
    }

    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.base.object_data)
    }

    fn event(&mut self, event: &mut Event) -> bool {
        match &event.kind {
            EventKind::Enter { .. } => {
                self.hovered.set(true);
                self.update();
                true
            }
            EventKind::Leave => {
                self.hovered.set(false);
                self.update();
                true
            }
            EventKind::MouseButtonPress { x, y, button } => {
                self.mouse_press_event(Point::new(*x, *y), *button, 0);
                // QAbstractSlider ignores every button but the left one.
                if *button != 1 {
                    event.ignore();
                }
                true
            }
            EventKind::MouseButtonRelease { x, y, button } => {
                self.mouse_release_event(Point::new(*x, *y), *button, 0);
                if *button != 1 {
                    event.ignore();
                }
                true
            }
            EventKind::MouseMove { x, y } => {
                self.mouse_move_event(Point::new(*x, *y));
                true
            }
            EventKind::Wheel { angle_delta_y, .. } => {
                self.scroll_by_wheel(*angle_delta_y)
            }
            _ => false,
        }
    }

    fn as_qobject_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_qobject_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

impl Widget for ScrollBar {
    fn widget_base(&self) -> &crate::widget::WidgetBase {
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
        match self.orientation {
            Orientation::Horizontal => Size::new(100, 10),
            Orientation::Vertical => Size::new(10, 100),
        }
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

    fn layout(&self) -> Option<&dyn Layout> {
        None
    }

    fn layout_mut(&mut self) -> Option<&mut Box<dyn Layout>> {
        None
    }

    fn set_layout(&mut self, _layout: Box<dyn Layout>) {}

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
        Vec::new()
    }

    fn add_child(&mut self, _child: WidgetRef) {}

    fn remove_child(&mut self, _child_id: ObjectId) {}

    fn mouse_press_event(&mut self, pos: Point, button: u32, _modifiers: u32) {
        if button != 1 {
            return;
        }

        let coord = match self.orientation {
            Orientation::Horizontal => pos.x,
            Orientation::Vertical => pos.y,
        };

        let (thumb_off, thumb_len) = self.thumb_geometry();

        if coord >= thumb_off && coord <= thumb_off + thumb_len {
            // Click inside thumb -> start drag
            self.dragging.set(true);
            self.drag_start_coord.set(coord);
            self.drag_start_value.set(self.value.get());
        } else if coord < thumb_off {
            // Page up
            self.set_value(self.value.get().saturating_sub(self.page_step.get()));
        } else {
            // Page down
            self.set_value(self.value.get().saturating_add(self.page_step.get()));
        }
    }

    fn mouse_release_event(&mut self, _pos: Point, button: u32, _modifiers: u32) {
        if button == 1 {
            self.dragging.set(false);
        }
    }

    fn mouse_move_event(&mut self, pos: Point) {
        if !self.dragging.get() {
            return;
        }

        let coord = match self.orientation {
            Orientation::Horizontal => pos.x,
            Orientation::Vertical => pos.y,
        };

        let delta_pixels = coord.saturating_sub(self.drag_start_coord.get());
        let track_len = self.track_length();
        let (_, thumb_len) = self.thumb_geometry();
        let available_track = (track_len - thumb_len).max(1);
        let range = self.maximum.get().saturating_sub(self.minimum.get());

        let delta_val =
            ((delta_pixels as f32 * range as f32) / available_track as f32).round() as i32;
        self.set_value(self.drag_start_value.get().saturating_add(delta_val));
    }

    fn wheel_event(&mut self, _pos: Point, delta_y: i32, _modifiers: u32) {
        self.scroll_by_wheel(delta_y);
    }

    fn paint_event(&mut self, painter: &mut Painter) {
        // Track and thumb are anti-aliased (`qfusionstyle.cpp:2108-2111`, `qstylesheetstyle.cpp:4370-4373`).
        painter.set_render_hint(RenderHint::Antialiasing, true);
        let geom = self.base.geometry();
        let rect_f = RectF::new(0.0, 0.0, geom.width as f32, geom.height as f32);
        // 1. Draw track
        painter.set_brush(Brush::Color(self.track_color));
        painter.set_pen(None);
        painter.draw_rounded_rect(rect_f, 3.0, 3.0);

        // 2. Draw thumb
        let (thumb_off, thumb_len) = self.thumb_geometry();
        let thumb_rect = match self.orientation {
            Orientation::Horizontal => RectF::new(
                thumb_off as f32,
                1.0,
                thumb_len as f32,
                (geom.height - 2).max(1) as f32,
            ),
            Orientation::Vertical => RectF::new(
                1.0,
                thumb_off as f32,
                (geom.width - 2).max(1) as f32,
                thumb_len as f32,
            ),
        };

        let t_color = if self.dragging.get() || self.hovered.get() {
            self.thumb_hover_color
        } else {
            self.thumb_color
        };
        painter.set_brush(Brush::Color(t_color));
        painter.draw_rounded_rect(thumb_rect, 4.0, 4.0);
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// Scrollable container widget supporting dynamic viewport clipping and auto-scrollbars (`QScrollArea`).
pub struct ScrollArea {
    base: WidgetBase,
    widget: Option<WidgetRef>,
    h_scrollbar: ScrollBar,
    v_scrollbar: ScrollBar,
    h_policy: ScrollBarPolicy,
    v_policy: ScrollBarPolicy,
    scroll_x: std::cell::Cell<i32>,
    scroll_y: std::cell::Cell<i32>,
    /// The bar a mouse press started on; moves and the release go to it.
    bar_grab: std::cell::Cell<Option<Orientation>>,
}

impl ScrollArea {
    /// Creates a new scroll area.
    pub fn new() -> Self {
        let base = WidgetBase::new();
        base.set_size_policy(QSizePolicy::new(Policy::Expanding, Policy::Expanding));

        let h_bar = ScrollBar::new(Orientation::Horizontal);
        let v_bar = ScrollBar::new(Orientation::Vertical);

        Self {
            base,
            widget: None,
            h_scrollbar: h_bar,
            v_scrollbar: v_bar,
            h_policy: ScrollBarPolicy::AsNeeded,
            v_policy: ScrollBarPolicy::AsNeeded,
            scroll_x: std::cell::Cell::new(0),
            scroll_y: std::cell::Cell::new(0),
            bar_grab: std::cell::Cell::new(None),
        }
    }

    /// Sets the child content widget inside the scroll area.
    pub fn set_widget(&mut self, widget: WidgetRef) {
        self.base.add_child(widget.clone());
        self.widget = Some(widget);
        self.update_scrollbars();
    }

    pub fn widget(&self) -> Option<WidgetRef> {
        self.widget.clone()
    }

    pub fn horizontal_scroll_bar(&self) -> &ScrollBar {
        &self.h_scrollbar
    }

    pub fn horizontal_scroll_bar_mut(&mut self) -> &mut ScrollBar {
        &mut self.h_scrollbar
    }

    pub fn vertical_scroll_bar(&self) -> &ScrollBar {
        &self.v_scrollbar
    }

    pub fn vertical_scroll_bar_mut(&mut self) -> &mut ScrollBar {
        &mut self.v_scrollbar
    }

    pub fn set_horizontal_scrollbar_policy(&mut self, policy: ScrollBarPolicy) {
        self.h_policy = policy;
        self.update_scrollbars();
    }

    pub fn set_vertical_scrollbar_policy(&mut self, policy: ScrollBarPolicy) {
        self.v_policy = policy;
        self.update_scrollbars();
    }

    pub fn scroll_position(&self) -> Point {
        Point::new(self.scroll_x.get(), self.scroll_y.get())
    }

    pub fn set_scroll_position(&mut self, x: i32, y: i32) {
        self.h_scrollbar.set_value(x);
        self.v_scrollbar.set_value(y);
        self.scroll_x.set(self.h_scrollbar.value());
        self.scroll_y.set(self.v_scrollbar.value());
        if let Some(w) = &self.widget {
            let b = w.borrow();
            let g = b.geometry();
            b.set_geometry(Rect::new(-self.scroll_x.get(), -self.scroll_y.get(), g.width, g.height));
        }
        self.update();
    }

    /// Calculates the inner viewport bounds excluding any active scrollbars.
    pub fn viewport_rect(&self) -> Rect {
        let geom = self.base.geometry();
        let bar_size = 10;
        let v_visible = self.v_scrollbar.is_visible();
        let h_visible = self.h_scrollbar.is_visible();

        let w = if v_visible {
            (geom.width - bar_size).max(0)
        } else {
            geom.width
        };
        let h = if h_visible {
            (geom.height - bar_size).max(0)
        } else {
            geom.height
        };

        Rect::new(0, 0, w, h)
    }

    fn bar(&self, orientation: Orientation) -> &ScrollBar {
        match orientation {
            Orientation::Horizontal => &self.h_scrollbar,
            Orientation::Vertical => &self.v_scrollbar,
        }
    }

    fn bar_mut(&mut self, orientation: Orientation) -> &mut ScrollBar {
        match orientation {
            Orientation::Horizontal => &mut self.h_scrollbar,
            Orientation::Vertical => &mut self.v_scrollbar,
        }
    }

    /// Reads the bar values and moves the content widget to match.
    fn sync_scroll_position(&self) {
        self.scroll_x.set(self.h_scrollbar.value());
        self.scroll_y.set(self.v_scrollbar.value());
        if let Some(w) = &self.widget {
            let b = w.borrow();
            let g = b.geometry();
            b.set_geometry(Rect::new(-self.scroll_x.get(), -self.scroll_y.get(), g.width, g.height));
        }
        self.update();
    }

    /// A wheel turn: Shift or a horizontal-only delta drives the horizontal bar (with
    /// `angle_delta_x`), anything else the vertical one. True when a bar moved.
    fn scroll_by_wheel(&mut self, angle_x: i32, angle_y: i32, modifiers: u32) -> bool {
        let is_shift = (modifiers & 0x02000000 != 0) || (modifiers & 1 != 0);
        let horizontal = is_shift || (angle_x != 0 && angle_y == 0);
        let (bar, delta) = if horizontal {
            (&self.h_scrollbar, if angle_x != 0 { angle_x } else { angle_y })
        } else {
            (&self.v_scrollbar, angle_y)
        };
        let moved = bar.scroll_by_wheel(delta);
        if moved {
            self.sync_scroll_position();
        }
        moved
    }

    fn update_scrollbars(&self) {
        let geom = self.base.geometry();
        if geom.width <= 0 || geom.height <= 0 {
            return;
        }

        let bar_size = 10;
        let content_size = if let Some(w) = &self.widget {
            let b = w.borrow();
            let g = b.geometry();
            let h = b.size_hint();
            Size::new(g.width.max(h.width), g.height.max(h.height))
        } else {
            Size::new(0, 0)
        };

        // Determine scrollbar visibility
        let show_v = match self.v_policy {
            ScrollBarPolicy::AlwaysOn => true,
            ScrollBarPolicy::AlwaysOff => false,
            ScrollBarPolicy::AsNeeded => content_size.height > geom.height,
        };

        let view_w = if show_v {
            geom.width - bar_size
        } else {
            geom.width
        };

        let show_h = match self.h_policy {
            ScrollBarPolicy::AlwaysOn => true,
            ScrollBarPolicy::AlwaysOff => false,
            ScrollBarPolicy::AsNeeded => content_size.width > view_w,
        };

        let view_h = if show_h {
            geom.height - bar_size
        } else {
            geom.height
        };

        // A bar that is not shown has no range: the content returns to the origin
        // (`QScrollBar` of an `AlwaysOff` / unneeded policy has maximum 0).
        self.v_scrollbar.set_visible(show_v);
        if show_v {
            self.v_scrollbar
                .set_geometry(Rect::new(geom.width - bar_size, 0, bar_size, view_h));
            let max_v = content_size.height.saturating_sub(view_h).max(0);
            self.v_scrollbar.set_range(0, max_v);
            self.v_scrollbar.set_page_step(view_h);
        } else if self.v_policy == ScrollBarPolicy::AlwaysOff {
            // Qt keeps the range of a bar that is switched off; the wheel still scrolls.
            self.v_scrollbar.set_range(0, content_size.height.saturating_sub(view_h).max(0));
            self.v_scrollbar.set_page_step(view_h);
        } else {
            self.v_scrollbar.set_range(0, 0);
        }

        self.h_scrollbar.set_visible(show_h);
        if show_h {
            self.h_scrollbar
                .set_geometry(Rect::new(0, geom.height - bar_size, view_w, bar_size));
            let max_h = content_size.width.saturating_sub(view_w).max(0);
            self.h_scrollbar.set_range(0, max_h);
            self.h_scrollbar.set_page_step(view_w);
        } else if self.h_policy == ScrollBarPolicy::AlwaysOff {
            self.h_scrollbar.set_range(0, content_size.width.saturating_sub(view_w).max(0));
            self.h_scrollbar.set_page_step(view_w);
        } else {
            self.h_scrollbar.set_range(0, 0);
        }

        self.scroll_x.set(self.h_scrollbar.value());
        self.scroll_y.set(self.v_scrollbar.value());

        // Position content widget
        if let Some(w) = &self.widget {
            let child_w = content_size.width.max(view_w);
            let child_h = content_size.height.max(view_h);
            w.borrow().set_geometry(Rect::new(
                -self.scroll_x.get(),
                -self.scroll_y.get(),
                child_w,
                child_h,
            ));
        }
        self.update();
    }
}

impl Default for ScrollArea {
    fn default() -> Self {
        Self::new()
    }
}

impl QObject for ScrollArea {
    fn object_data(&self) -> &ObjectData {
        &self.base.object_data
    }

    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.base.object_data)
    }

    fn event(&mut self, event: &mut Event) -> bool {
        match &event.kind {
            EventKind::Wheel {
                angle_delta_x,
                angle_delta_y,
                modifiers,
                ..
            } => {
                // A wheel that moved nothing (no bar, at a limit) is not handled: the parent
                // gets it, as `QAbstractScrollArea::wheelEvent` ignores it.
                self.scroll_by_wheel(*angle_delta_x, *angle_delta_y, *modifiers)
            }
            EventKind::MouseButtonPress { x, y, button } => {
                let pos = Point::new(*x, *y);
                let bar = [Orientation::Vertical, Orientation::Horizontal]
                    .into_iter()
                    .find(|o| {
                        let bar = self.bar(*o);
                        bar.is_visible() && bar.geometry().contains(pos)
                    });
                let Some(orientation) = bar else {
                    return false;
                };
                self.bar_grab.set(Some(orientation));
                let bar = self.bar_mut(orientation);
                let g = bar.geometry();
                bar.mouse_press_event(Point::new(pos.x - g.x, pos.y - g.y), *button, 0);
                self.sync_scroll_position();
                true
            }
            EventKind::MouseMove { x, y } => {
                let Some(orientation) = self.bar_grab.get() else {
                    return false;
                };
                let bar = self.bar_mut(orientation);
                let g = bar.geometry();
                bar.mouse_move_event(Point::new(*x - g.x, *y - g.y));
                self.sync_scroll_position();
                true
            }
            EventKind::MouseButtonRelease { x, y, button } => {
                let Some(orientation) = self.bar_grab.take() else {
                    return false;
                };
                let bar = self.bar_mut(orientation);
                let g = bar.geometry();
                bar.mouse_release_event(Point::new(*x - g.x, *y - g.y), *button, 0);
                self.sync_scroll_position();
                true
            }
            _ => false,
        }
    }

    fn as_qobject_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_qobject_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

impl Widget for ScrollArea {
    fn widget_base(&self) -> &crate::widget::WidgetBase {
        &self.base
    }

    fn id(&self) -> ObjectId {
        self.base.object_data.id()
    }

    fn geometry(&self) -> Rect {
        self.base.geometry()
    }

    fn set_geometry(&self, rect: Rect) {
        if self.base.geometry() != rect {
            self.base.set_geometry(rect);
            self.update_scrollbars();
        }
    }

    fn size_hint(&self) -> Size {
        Size::new(200, 200)
    }

    fn child_resized(&self, child: ObjectId) {
        let Some(content) = &self.widget else { return };
        if content.try_borrow().is_ok_and(|c| c.id() == child) {
            self.update_scrollbars();
        }
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

    fn layout(&self) -> Option<&dyn Layout> {
        None
    }

    fn layout_mut(&mut self) -> Option<&mut Box<dyn Layout>> {
        None
    }

    fn set_layout(&mut self, _layout: Box<dyn Layout>) {}

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
        if let Some(w) = &self.widget {
            w.borrow().set_window_id(window_id);
        }
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

    fn wheel_event(&mut self, _pos: Point, delta_y: i32, modifiers: u32) {
        self.scroll_by_wheel(0, delta_y, modifiers);
    }

    fn children_clip_rect(&self) -> Option<Rect> {
        Some(self.viewport_rect())
    }

    fn paint_over_children(&mut self, painter: &mut Painter) {
        if self.v_scrollbar.is_visible() {
            painter.save();
            let g = self.v_scrollbar.geometry();
            painter.translate(g.x as f32, g.y as f32);
            self.v_scrollbar.paint_event(painter);
            painter.restore();
        }

        if self.h_scrollbar.is_visible() {
            painter.save();
            let g = self.h_scrollbar.geometry();
            painter.translate(g.x as f32, g.y as f32);
            self.h_scrollbar.paint_event(painter);
            painter.restore();
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
