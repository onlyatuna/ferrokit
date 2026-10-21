// SPDX-License-Identifier: LGPL-3.0-only
//! A leaf widget that logs every virtual handler Qt would call on it, so tests can count how
//! often each one ran. Include with `#[path = "support/probe_widget.rs"] mod probe_widget;`.
#![allow(dead_code)]

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::object::{ObjectData, ObjectDataMut, ObjectId, QObject};
use ferrokit_gui::geometry::primitives::{Point, Rect, Size};
use ferrokit_gui::paint::Painter;
use ferrokit_widgets::focus::FocusPolicy;
use ferrokit_widgets::layout::Layout;
use ferrokit_widgets::widget::{Widget, WidgetBase, WidgetRef, WidgetWeak};
use std::cell::RefCell;
use std::rc::Rc;

pub type Log = Rc<RefCell<Vec<String>>>;

pub struct ProbeWidget {
    base: WidgetBase,
    pub log: Log,
    /// Whether `key_press_event` accepts the key (the widget's `event` then returns true).
    pub consume_keys: bool,
    /// Runs from `mouse_press_event`, with this widget's `RefCell` mutably borrowed.
    pub on_press: Option<Box<dyn FnMut()>>,
}

impl ProbeWidget {
    pub fn new(log: &Log) -> Self {
        Self {
            base: WidgetBase::with_geometry(Rect::new(0, 0, 50, 20)),
            log: Rc::clone(log),
            consume_keys: false,
            on_press: None,
        }
    }

    pub fn with_focus(self, policy: FocusPolicy) -> Self {
        self.base.set_focus_policy(policy);
        self
    }

    pub fn into_ref(self) -> WidgetRef {
        Rc::new(RefCell::new(Box::new(self)))
    }

    fn note(&self, what: impl Into<String>) {
        self.log.borrow_mut().push(what.into());
    }
}

impl QObject for ProbeWidget {
    fn object_data(&self) -> &ObjectData {
        &self.base.object_data
    }
    fn object_data_mut(&mut self) -> ObjectDataMut<'_> {
        ObjectDataMut::new(&mut self.base.object_data)
    }
    fn event(&mut self, event: &mut Event) -> bool {
        match &event.kind {
            EventKind::FocusIn { reason } => {
                self.focus_in_event(*reason);
                true
            }
            EventKind::FocusOut { reason } => {
                self.focus_out_event(*reason);
                true
            }
            EventKind::Resize {
                width,
                height,
                old_width,
                old_height,
            } => {
                self.resize_event(
                    Size::new(*width, *height),
                    Size::new(*old_width, *old_height),
                );
                true
            }
            EventKind::KeyPress {
                key,
                modifiers,
                is_repeat,
            } => {
                self.key_press_event(*key, *modifiers, *is_repeat);
                self.consume_keys
            }
            EventKind::MouseButtonPress { x, y, button } => {
                self.mouse_press_event(Point::new(*x, *y), *button, 0);
                true
            }
            _ => false,
        }
    }
}

impl Widget for ProbeWidget {
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
        Size::new(50, 20)
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
    fn focus_policy(&self) -> FocusPolicy {
        self.base.focus_policy()
    }
    fn set_focus_policy(&self, policy: FocusPolicy) {
        self.base.set_focus_policy(policy);
    }
    fn has_focus(&self) -> bool {
        self.base.has_focus()
    }
    fn set_has_focus(&self, focus: bool) {
        self.base.set_has_focus(focus);
    }
    fn dpi_changed_event(&mut self, old_dpr: f32, new_dpr: f32) {
        self.note(format!("dpi {old_dpr}->{new_dpr}"));
    }
    fn focus_in_event(&mut self, _reason: ferrokit_core::event::FocusReason) {
        self.note("focus_in");
    }
    fn focus_out_event(&mut self, _reason: ferrokit_core::event::FocusReason) {
        self.note("focus_out");
    }
    fn resize_event(&self, new_size: Size, old_size: Size) {
        self.note(format!(
            "resize {}x{} (was {}x{})",
            new_size.width, new_size.height, old_size.width, old_size.height
        ));
    }
    fn key_press_event(&mut self, key: u32, _modifiers: u32, _is_repeat: bool) {
        self.note(format!("key {key:#x}"));
    }
    fn mouse_press_event(&mut self, _pos: Point, _button: u32, _modifiers: u32) {
        self.note("press");
        if let Some(hook) = self.on_press.as_mut() {
            hook();
        }
    }
    fn paint_event(&mut self, _painter: &mut Painter) {}
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
