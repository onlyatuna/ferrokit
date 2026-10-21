// SPDX-License-Identifier: LGPL-3.0-only
use crate::widget::WidgetRef;
use ferrokit_core::object::ObjectId;
use ferrokit_gui::geometry::primitives::Point;

/// Manages mouse grabbing and popup auto-dismissal (`QWidget::grabMouse` & Popups).
pub struct PopupManager {
    active_popup: Option<WidgetRef>,
    mouse_grabber: Option<ObjectId>,
    auto_dismiss: bool,
}

impl Default for PopupManager {
    fn default() -> Self {
        Self::new()
    }
}

impl PopupManager {
    /// Creates a new popup manager.
    pub fn new() -> Self {
        Self {
            active_popup: None,
            mouse_grabber: None,
            auto_dismiss: true,
        }
    }

    /// Sets the active popup widget and acquires mouse grab.
    pub fn open_popup(&mut self, popup: WidgetRef, auto_dismiss: bool) {
        let id = popup.borrow().id();
        crate::widget::set_widget_visible(&popup, true);
        self.active_popup = Some(popup);
        self.mouse_grabber = Some(id);
        self.auto_dismiss = auto_dismiss;
    }

    /// Explicitly closes the active popup.
    pub fn close_popup(&mut self) {
        if let Some(popup) = self.active_popup.take() {
            crate::widget::set_widget_visible(&popup, false);
            popup.borrow_mut().update();
        }
        self.mouse_grabber = None;
    }

    /// Returns the ObjectId of the current mouse grabber, if any.
    pub fn mouse_grabber(&self) -> Option<ObjectId> {
        self.mouse_grabber
    }

    /// Sets an explicit mouse grabber widget.
    pub fn set_mouse_grabber(&mut self, grabber_id: Option<ObjectId>) {
        self.mouse_grabber = grabber_id;
    }

    /// Checks if a mouse click should auto-dismiss the active popup.
    ///
    /// Returns true if the popup was dismissed.
    pub fn handle_mouse_press(&mut self, win_pos: Point) -> bool {
        if let Some(popup) = &self.active_popup {
            let geom = popup.borrow().geometry();
            let inside = geom.contains(win_pos);
            if !inside && self.auto_dismiss {
                self.close_popup();
                return true;
            }
        }
        false
    }
}
