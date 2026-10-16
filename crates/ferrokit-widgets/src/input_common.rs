// SPDX-License-Identifier: LGPL-3.0-only
//! Shared `Widget` boilerplate for the leaf input widgets (combo box, spin
//! boxes, sliders, dial, progress bar).
//!
//! Every such widget stores its state in a `base: WidgetBase` field and has no
//! children or layout. The macro expands inside an `impl Widget for T` block and
//! provides every trait method except `size_hint`, `update`, focus handling,
//! input handlers and `paint_event`, which each widget implements itself.

macro_rules! leaf_widget_common {
    () => {
        fn widget_base(&self) -> &crate::widget::WidgetBase {
            &self.base
        }

        fn id(&self) -> ferrokit_core::object::ObjectId {
            self.base.object_data.id()
        }

        fn geometry(&self) -> ferrokit_gui::geometry::primitives::Rect {
            self.base.geometry.get()
        }

        fn set_geometry(&self, rect: ferrokit_gui::geometry::primitives::Rect) {
            self.base.set_geometry(rect);
        }

        fn is_visible(&self) -> bool {
            self.base.visible.get()
        }

        fn set_visible(&self, visible: bool) {
            self.base.set_visible(visible);
        }

        fn is_enabled(&self) -> bool {
            self.base.enabled.get()
        }

        fn dirty_rect(&self) -> Option<ferrokit_gui::geometry::primitives::Rect> {
            self.base.dirty.get()
        }

        fn clear_dirty(&self) {
            self.base.dirty.set(None);
        }

        fn layout(&self) -> Option<&dyn crate::layout::Layout> {
            None
        }

        fn layout_mut(&mut self) -> Option<&mut Box<dyn crate::layout::Layout>> {
            None
        }

        fn set_layout(&mut self, _layout: Box<dyn crate::layout::Layout>) {}

        fn parent_widget(&self) -> Option<crate::widget::WidgetWeak> {
            self.base.parent.borrow().clone()
        }

        fn set_parent_widget(&self, parent: Option<crate::widget::WidgetWeak>) {
            *self.base.parent.borrow_mut() = parent;
        }

        fn window_id(&self) -> Option<ferrokit_core::object::ObjectId> {
            self.base.window_id.get()
        }

        fn set_window_id(&self, window_id: Option<ferrokit_core::object::ObjectId>) {
            self.base.window_id.set(window_id);
        }
        fn children(&self) -> Vec<crate::widget::WidgetRef> {
            Vec::new()
        }

        fn add_child(&mut self, _child: crate::widget::WidgetRef) {}

        fn remove_child(&mut self, _child_id: ferrokit_core::object::ObjectId) {}

        fn focus_policy(&self) -> crate::focus::FocusPolicy {
            self.base.focus_policy.get()
        }

        fn set_focus_policy(&self, policy: crate::focus::FocusPolicy) {
            self.base.focus_policy.set(policy);
        }

        fn has_focus(&self) -> bool {
            self.base.has_focus.get()
        }

        fn set_has_focus(&self, focus: bool) {
            self.base.has_focus.set(focus);
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }

        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
    };
}

/// Expands to the `QObject` accessor methods for a widget with a `base: WidgetBase` field.
macro_rules! leaf_qobject_common {
    () => {
        fn object_data(&self) -> &ferrokit_core::object::ObjectData {
            &self.base.object_data
        }

        fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.base.object_data)
    }

        fn as_qobject_any(&self) -> Option<&dyn std::any::Any> {
            Some(self)
        }

        fn as_qobject_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
            Some(self)
        }
    };
}

/// Standard event dispatch from `QObject::event` to the `Widget` virtual handlers.
///
/// Returns `false` for event kinds the leaf input widgets do not consume.
pub(crate) fn dispatch_input_event<W: crate::widget::Widget + ?Sized>(
    widget: &mut W,
    event: &ferrokit_core::event::Event,
) -> bool {
    use ferrokit_core::event::EventKind;
    use ferrokit_gui::geometry::primitives::{Point, Size};
    match &event.kind {
        EventKind::MouseButtonPress { x, y, button } => {
            widget.mouse_press_event(Point::new(*x, *y), *button, 0);
            false
        }
        EventKind::MouseButtonRelease { x, y, button } => {
            widget.mouse_release_event(Point::new(*x, *y), *button, 0);
            false
        }
        EventKind::MouseMove { x, y } => {
            widget.mouse_move_event(Point::new(*x, *y));
            false
        }
        EventKind::Enter { x, y } => {
            widget.enter_event(Point::new(*x, *y));
            false
        }
        EventKind::Leave => {
            widget.leave_event();
            false
        }
        EventKind::Wheel {
            x,
            y,
            angle_delta_y,
            modifiers,
            ..
        } => {
            widget.wheel_event(Point::new(*x, *y), *angle_delta_y, *modifiers);
            // Leaf input widgets have no wheel behaviour: let it reach the parent.
            false
        }
        EventKind::Resize {
            width,
            height,
            old_width,
            old_height,
        } => {
            widget.resize_event(
                Size::new(*width, *height),
                Size::new(*old_width, *old_height),
            );
            true
        }
        EventKind::FocusIn { reason } => {
            widget.focus_in_event(*reason);
            true
        }
        EventKind::FocusOut { reason } => {
            widget.focus_out_event(*reason);
            true
        }
        EventKind::KeyPress {
            key,
            modifiers,
            is_repeat,
        } => {
            widget.key_press_event(*key, *modifiers, *is_repeat);
            true
        }
        EventKind::KeyRelease { key, modifiers } => {
            widget.key_release_event(*key, *modifiers);
            true
        }
        _ => false,
    }
}
