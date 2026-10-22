// SPDX-License-Identifier: LGPL-3.0-only
use crate::focus::{find_widget_by_id, FocusManager};
use crate::popup::PopupManager;
use crate::widget::WidgetRef;
use ferrokit_core::event::FocusReason;
use ferrokit_core::event::{Event, EventKind, EventPointPos, MouseButtons};
use ferrokit_core::object::ObjectId;
use ferrokit_gui::geometry::primitives::{Point, Rect};

pub struct EventTreeDispatcher {
    last_hovered: Option<(ObjectId, WidgetRef)>,
    focus_manager: FocusManager,
    popup_manager: PopupManager,
    /// `qt_button_down`: the widget that took the press grab. It receives moves and the release
    /// while a button is held (qwidgetwindow.cpp:666-668).
    press_grab: Option<(ObjectId, WidgetRef)>,
    /// One bit per button held down (`1 << (button - 1)`); the grab ends with the last button.
    held_buttons: u32,
    last_dpr: f32,
}

impl Default for EventTreeDispatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl EventTreeDispatcher {
    pub fn new() -> Self {
        Self {
            last_hovered: None,
            focus_manager: FocusManager::new(),
            popup_manager: PopupManager::new(),
            press_grab: None,
            held_buttons: 0,
            last_dpr: 1.0,
        }
    }

    pub fn popup_manager(&self) -> &PopupManager {
        &self.popup_manager
    }

    pub fn popup_manager_mut(&mut self) -> &mut PopupManager {
        &mut self.popup_manager
    }

    pub fn focus_manager(&self) -> &FocusManager {
        &self.focus_manager
    }

    pub fn focus_manager_mut(&mut self) -> &mut FocusManager {
        &mut self.focus_manager
    }
    pub fn handle_mouse_leave(&mut self) {
        if let Some((_old_id, old_widget)) = self.last_hovered.take() {
            // Leaving the window leaves the whole chain (qapplication.cpp:2082-2101 with no new receiver).
            for w in ancestors(&old_widget) {
                let mut leave_ev = Event::new_spontaneous(EventKind::Leave);
                let _ = w.borrow_mut().event(&mut leave_ev);
            }
        }
        crate::tooltip::leave();
        crate::command::WidgetCommandQueue::flush();
    }

    pub fn dispatch_event(&mut self, root: &WidgetRef, event: &mut Event) -> bool {
        crate::tooltip::user_input(&event.kind);
        self.focus_manager.process_pending(root);
        let result = self.dispatch_event_internal(root, event, MouseButtons(0), None);
        self.focus_manager.process_pending(root);
        crate::layout_scheduler::LayoutScheduler::flush_deferred(root);
        crate::command::WidgetCommandQueue::flush();
        result
    }

    /// A spontaneous mouse move from the window system. Besides what `dispatch_event` does for a
    /// `MouseMove`, it is what `QApplication::notify` sees as a mouse move with a global position
    /// and button state: with no button down it starts the tool tip wake-up for the widget under
    /// the cursor (qapplication.cpp:2712-2737).
    pub fn dispatch_mouse_move(
        &mut self,
        root: &WidgetRef,
        win_pos: Point,
        global_pos: Point,
        buttons: MouseButtons,
    ) -> bool {
        let mut event = Event::new_spontaneous(EventKind::MouseMove {
            x: win_pos.x,
            y: win_pos.y,
        });
        let tool_tip_pos = buttons.is_empty().then_some(global_pos);
        self.focus_manager.process_pending(root);
        let result = self.dispatch_event_internal(root, &mut event, buttons, tool_tip_pos);
        self.focus_manager.process_pending(root);
        crate::layout_scheduler::LayoutScheduler::flush_deferred(root);
        crate::command::WidgetCommandQueue::flush();
        result
    }

    /// `tool_tip_global_pos` is set for a mouse move that may start a tool tip.
    fn dispatch_event_internal(
        &mut self,
        root: &WidgetRef,
        event: &mut Event,
        buttons: MouseButtons,
        tool_tip_global_pos: Option<Point>,
    ) -> bool {
        match &event.kind {
            EventKind::MouseMove { x, y } => {
                let win_pos = Point::new(*x, *y);
                if buttons.is_empty() {
                    // No button is down, so a grab left behind means a release went missing.
                    self.held_buttons = 0;
                    self.press_grab = None;
                } else if let Some((_, grab)) = self.press_grab.clone() {
                    // Qt pickMouseReceiver (qapplication.cpp:2255-2283): with a button held, the
                    // press target gets the move wherever the cursor is now.
                    return deliver_with_propagation(&path_to(&grab, win_pos), |pos| {
                        EventKind::MouseMove { x: pos.x, y: pos.y }
                    });
                }

                let path = hit_path(root, win_pos);
                match path.last().map(|s| (s.widget.clone(), s.pos)) {
                    Some((target, local_pos)) => {
                        if buttons.is_empty() {
                            // Enter/Leave are not sent while a button is down (qapplication.cpp:2316).
                            self.update_hover(&target, win_pos);
                        }
                        if let Some(global_pos) = tool_tip_global_pos {
                            crate::tooltip::mouse_moved(&target, local_pos, global_pos);
                        }
                        if buttons.is_empty() {
                            // Buttonless move: QApplication::notify walks the chain (qapplication.cpp:2738-2766).
                            return deliver_tracking_move(&path);
                        }
                        let mut local_event = Event::new_spontaneous(EventKind::MouseMove {
                            x: local_pos.x,
                            y: local_pos.y,
                        });
                        target.borrow_mut().event(&mut local_event)
                    }
                    None => {
                        self.handle_mouse_leave();
                        false
                    }
                }
            }
            EventKind::MouseButtonPress { x, y, button } => {
                let win_pos = Point::new(*x, *y);
                let _dismissed = self.popup_manager.handle_mouse_press(win_pos);

                // Check mouse grabber
                if let Some(grabber_id) = self.popup_manager.mouse_grabber() {
                    if let Some(grabber) = find_widget_by_id(root, grabber_id) {
                        let g = grabber.borrow().geometry();
                        let mut local_event = Event::new_spontaneous(EventKind::MouseButtonPress {
                            x: win_pos.x - g.x,
                            y: win_pos.y - g.y,
                            button: *button,
                        });
                        return grabber.borrow_mut().event(&mut local_event);
                    }
                }

                let path = hit_path(root, win_pos);
                self.note_press(*button, path.last());
                if let Some(target) = path.last() {
                    if *button == 1 {
                        self.focus_manager.handle_mouse_click(root, &target.widget);
                    }
                }
                let button = *button;
                deliver_with_propagation(&path, |pos| EventKind::MouseButtonPress {
                    x: pos.x,
                    y: pos.y,
                    button,
                })
            }
            EventKind::MouseButtonDblClick { x, y, button } => {
                let win_pos = Point::new(*x, *y);
                let button = *button;
                let path = hit_path(root, win_pos);
                self.note_press(button, path.last());
                if let Some(target) = path.last() {
                    // The OS reports the second press of a double click as a double click only
                    // (Qt does not deliver that press either), so it must also give focus.
                    if button == 1 {
                        self.focus_manager.handle_mouse_click(root, &target.widget);
                    }
                }
                deliver_double_click(&path, button)
            }
            EventKind::ContextMenu {
                x,
                y,
                global_x,
                global_y,
                reason,
            } => {
                let win_pos = Point::new(*x, *y);
                let (global_x, global_y, reason) = (*global_x, *global_y, *reason);
                let path = hit_path(root, win_pos);
                deliver_with_propagation(&path, |pos| EventKind::ContextMenu {
                    x: pos.x,
                    y: pos.y,
                    global_x,
                    global_y,
                    reason,
                })
            }
            EventKind::HoverMove {
                pos,
                old_pos,
                modifiers,
            } => {
                let win_pos = Point::new(pos.x.round() as i32, pos.y.round() as i32);
                if let Some((target, local_pos)) = hit_test(root, win_pos) {
                    let mut local_event = Event::new_spontaneous(EventKind::HoverMove {
                        pos: EventPointPos::new(local_pos.x as f32, local_pos.y as f32),
                        old_pos: *old_pos,
                        modifiers: *modifiers,
                    });
                    return target.borrow_mut().event(&mut local_event);
                }
                false
            }
            EventKind::HoverEnter {
                pos,
                old_pos,
                modifiers,
            } => {
                let win_pos = Point::new(pos.x.round() as i32, pos.y.round() as i32);
                if let Some((target, local_pos)) = hit_test(root, win_pos) {
                    let mut local_event = Event::new_spontaneous(EventKind::HoverEnter {
                        pos: EventPointPos::new(local_pos.x as f32, local_pos.y as f32),
                        old_pos: *old_pos,
                        modifiers: *modifiers,
                    });
                    return target.borrow_mut().event(&mut local_event);
                }
                false
            }
            EventKind::HoverLeave { old_pos, modifiers } => {
                if let Some((_, old_widget)) = &self.last_hovered {
                    let mut local_event = Event::new_spontaneous(EventKind::HoverLeave {
                        old_pos: *old_pos,
                        modifiers: *modifiers,
                    });
                    return old_widget.borrow_mut().event(&mut local_event);
                }
                false
            }
            EventKind::MouseButtonRelease { x, y, button } => {
                let win_pos = Point::new(*x, *y);
                self.held_buttons &= !button_bit(*button);
                // The grab ends with the last button released (qapplication.cpp:2322-2323).
                let grab = if self.held_buttons == 0 {
                    self.press_grab.take()
                } else {
                    self.press_grab.clone()
                };
                if let Some(grabber_id) = self.popup_manager.mouse_grabber() {
                    if let Some(grabber) = find_widget_by_id(root, grabber_id) {
                        let g = grabber.borrow().geometry();
                        let mut local_event =
                            Event::new_spontaneous(EventKind::MouseButtonRelease {
                                x: win_pos.x - g.x,
                                y: win_pos.y - g.y,
                                button: *button,
                            });
                        return grabber.borrow_mut().event(&mut local_event);
                    }
                }
                let button = *button;
                let path = match &grab {
                    Some((_, g)) => path_to(g, win_pos),
                    None => hit_path(root, win_pos),
                };
                deliver_with_propagation(&path, |pos| EventKind::MouseButtonRelease {
                    x: pos.x,
                    y: pos.y,
                    button,
                })
            }
            EventKind::Wheel {
                x,
                y,
                pixel_delta_x,
                pixel_delta_y,
                angle_delta_x,
                angle_delta_y,
                modifiers,
            } => {
                let win_pos = Point::new(*x, *y);
                let (pixel_delta_x, pixel_delta_y) = (*pixel_delta_x, *pixel_delta_y);
                let (angle_delta_x, angle_delta_y, modifiers) =
                    (*angle_delta_x, *angle_delta_y, *modifiers);
                let path = hit_path(root, win_pos);
                deliver_with_propagation(&path, |pos| EventKind::Wheel {
                    x: pos.x,
                    y: pos.y,
                    pixel_delta_x,
                    pixel_delta_y,
                    angle_delta_x,
                    angle_delta_y,
                    modifiers,
                })
            }
            EventKind::Resize {
                width,
                height,
                old_width,
                old_height,
            } => {
                root.borrow().set_geometry(Rect::new(0, 0, *width, *height));
                let mut resize_event = Event::new_spontaneous(EventKind::Resize {
                    width: *width,
                    height: *height,
                    old_width: *old_width,
                    old_height: *old_height,
                });
                root.borrow_mut().event(&mut resize_event)
            }
            EventKind::FocusIn { reason } => {
                if let Some(focused_id) = self.focus_manager.focused_widget_id() {
                    if let Some(focused_widget) = find_widget_by_id(root, focused_id) {
                        focused_widget.borrow_mut().event(event);
                    }
                } else if *reason == FocusReason::ActiveWindow {
                    self.focus_manager.focus_next(root);
                }
                root.borrow_mut().event(event)
            }
            EventKind::FocusOut { .. } => {
                if let Some(focused_id) = self.focus_manager.focused_widget_id() {
                    if let Some(focused_widget) = find_widget_by_id(root, focused_id) {
                        focused_widget.borrow_mut().event(event);
                    }
                }
                root.borrow_mut().event(event)
            }
            EventKind::KeyPress { key, modifiers, .. } => {
                // `QWidget::event` (qwidget.cpp, KeyPress): Tab, Shift+Tab and Backtab move the focus
                // before `keyPressEvent`, unless Ctrl or Alt is held. 0x09 is the Win32 VK_TAB.
                use crate::action::keys;
                let mods = keys::normalize_modifiers(*modifiers);
                if mods & (keys::MOD_CTRL | keys::MOD_ALT) == 0 {
                    let tab = *key == 0x09 || *key == keys::TAB;
                    let backtab = *key == keys::BACKTAB || (tab && mods & keys::MOD_SHIFT != 0);
                    // A focus widget can keep the key for itself (`QWidget::event` is
                    // overridable in Qt).
                    let kept = (tab || backtab)
                        && self
                            .focus_manager
                            .focused_widget_id()
                            .and_then(|id| find_widget_by_id(root, id))
                            .is_some_and(|w| {
                                w.try_borrow()
                                    .is_ok_and(|w| w.wants_tab_key(*key, *modifiers))
                            });
                    if !kept {
                        if backtab {
                            if self.focus_manager.focus_previous(root) {
                                return true;
                            }
                        } else if tab && self.focus_manager.focus_next(root) {
                            return true;
                        }
                    }
                }

                // Route key event to focused widget first
                if let Some(focused_id) = self.focus_manager.focused_widget_id() {
                    if let Some(focused_widget) = find_widget_by_id(root, focused_id) {
                        if focused_widget.borrow_mut().event(event) {
                            return true;
                        }
                    }
                }
                root.borrow_mut().event(event)
            }
            EventKind::KeyRelease { .. } => {
                if let Some(focused_id) = self.focus_manager.focused_widget_id() {
                    if let Some(focused_widget) = find_widget_by_id(root, focused_id) {
                        if focused_widget.borrow_mut().event(event) {
                            return true;
                        }
                    }
                }
                root.borrow_mut().event(event)
            }
            EventKind::InputMethod { .. } => {
                if let Some(focused_id) = self.focus_manager.focused_widget_id() {
                    if let Some(focused_widget) = find_widget_by_id(root, focused_id) {
                        if focused_widget.borrow_mut().event(event) {
                            return true;
                        }
                    }
                }
                root.borrow_mut().event(event)
            }
            EventKind::DpiChanged { dpi_x, .. } => {
                let new_dpr = ferrokit_gui::round_scale_factor(*dpi_x as f64 / 96.0) as f32;
                let old_dpr = self.last_dpr;
                self.last_dpr = new_dpr;
                crate::window::propagate_dpi_change_recursive(root, old_dpr, new_dpr);
                true
            }
            _ => false,
        }
    }

    /// `qt_button_down` bookkeeping for a press (qwidgetwindow.cpp:666-668): the grab is taken on
    /// the initial press, when no other button is down. Pressing a button that is already marked
    /// held means its release went missing, so that press takes the grab again.
    fn note_press(&mut self, button: u32, target: Option<&HitStep>) {
        let bit = button_bit(button);
        if self.held_buttons == 0 || self.held_buttons & bit != 0 {
            self.press_grab = target.map(|t| (t.widget.borrow().id(), t.widget.clone()));
        }
        self.held_buttons |= bit;
    }

    /// Enter/Leave as `QApplicationPrivate::dispatchEnterLeave` (qapplication.cpp:2037-2123):
    /// Leave innermost-first over the old chain, up to but excluding the common ancestor; Enter
    /// outermost-first from below the common ancestor down to `target`.
    fn update_hover(&mut self, target: &WidgetRef, win_pos: Point) {
        let target_id = target.borrow().id();
        if matches!(&self.last_hovered, Some((id, _)) if *id == target_id) {
            return;
        }
        let new_chain = ancestors(target);
        let old_chain = self
            .last_hovered
            .take()
            .map(|(_, w)| ancestors(&w))
            .unwrap_or_default();
        if !old_chain.is_empty() {
            crate::tooltip::leave();
        }
        let new_ids: Vec<ObjectId> = new_chain.iter().map(|w| w.borrow().id()).collect();
        let old_ids: Vec<ObjectId> = old_chain.iter().map(|w| w.borrow().id()).collect();
        let common = new_ids.iter().position(|id| old_ids.contains(id));
        let enter_end = common.unwrap_or(new_ids.len());
        let leave_end = match common {
            Some(i) => old_ids
                .iter()
                .position(|id| *id == new_ids[i])
                .unwrap_or(old_ids.len()),
            None => old_ids.len(),
        };
        for w in &old_chain[..leave_end] {
            let mut ev = Event::new_spontaneous(EventKind::Leave);
            let _ = w.borrow_mut().event(&mut ev);
        }
        for i in (0..enter_end).rev() {
            let p = local_pos(&new_chain[i..], win_pos);
            let mut ev = Event::new_spontaneous(EventKind::Enter { x: p.x, y: p.y });
            let _ = new_chain[i].borrow_mut().event(&mut ev);
        }
        self.last_hovered = Some((target_id, target.clone()));
    }
}

/// One widget on the path from the window's root widget down to the widget under the cursor.
pub struct HitStep {
    pub widget: WidgetRef,
    /// The cursor position in this widget's own coordinates.
    pub pos: Point,
    /// `QWidget::isEnabled`: false when this widget or any ancestor is disabled.
    pub enabled: bool,
}

/// The widgets under `pos` (window coordinates), root first, innermost widget last. Empty when
/// `pos` is outside a visible root.
pub fn hit_path(root: &WidgetRef, pos: Point) -> Vec<HitStep> {
    let mut path = Vec::new();
    {
        let r = root.borrow();
        let g = r.geometry();
        if !r.is_visible() || !Rect::new(0, 0, g.width, g.height).contains(pos) {
            return path;
        }
        path.push(HitStep {
            widget: root.clone(),
            pos,
            enabled: r.is_enabled(),
        });
    }
    loop {
        let candidates = {
            let cur = path.last().expect("path starts with the root");
            let w = cur.widget.borrow();
            // Children are clipped to the clip rect (a scroll area's viewport): a point outside
            // it belongs to the widget itself, e.g. to its scroll bars.
            if w.children_clip_rect().is_some_and(|clip| !clip.contains(cur.pos)) {
                None
            } else {
                Some((w.children(), cur.pos, cur.enabled))
            }
        };
        let Some((children, pos, enabled)) = candidates else {
            return path;
        };
        let mut next = None;
        for child in children.into_iter().rev() {
            let (visible, geom, child_enabled, transparent) = {
                let b = child.borrow();
                let base = b.widget_base();
                (
                    b.is_visible(),
                    b.geometry(),
                    b.is_enabled(),
                    base.is_transparent_for_mouse_events(),
                )
            };
            if visible && !transparent && geom.contains(pos) {
                next = Some(HitStep {
                    pos: Point::new(pos.x - geom.x, pos.y - geom.y),
                    enabled: enabled && child_enabled,
                    widget: child,
                });
                break;
            }
        }
        match next {
            Some(step) => path.push(step),
            None => return path,
        }
    }
}

pub fn hit_test(root: &WidgetRef, local_pos: Point) -> Option<(WidgetRef, Point)> {
    hit_path(root, local_pos).pop().map(|s| (s.widget, s.pos))
}

/// `w` and its ancestors up to the root, innermost first.
fn ancestors(w: &WidgetRef) -> Vec<WidgetRef> {
    let mut out = vec![w.clone()];
    loop {
        let parent = {
            let cur = out.last().expect("chain is never empty");
            let weak = cur.borrow().parent_widget();
            weak.and_then(|p| p.upgrade())
        };
        match parent {
            Some(p) => out.push(p),
            None => return out,
        }
    }
}

/// `win` (window coordinates) in the coordinates of `chain[0]`: subtract the origin of every
/// widget below the root, as `hit_path` does.
fn local_pos(chain: &[WidgetRef], win: Point) -> Point {
    let mut p = win;
    for w in &chain[..chain.len() - 1] {
        let g = w.borrow().geometry();
        p = Point::new(p.x - g.x, p.y - g.y);
    }
    p
}

/// Root-first path from the root down to `w`, shaped like `hit_path`'s result, for delivery to a grab.
fn path_to(w: &WidgetRef, win: Point) -> Vec<HitStep> {
    let chain = ancestors(w);
    (0..chain.len())
        .rev()
        .map(|i| HitStep {
            widget: chain[i].clone(),
            pos: local_pos(&chain[i..], win),
            enabled: chain[i].borrow().is_enabled(),
        })
        .collect()
}

/// Bit for a mouse button code (1 = left, 2 = right, 3 = middle); 0 and codes past 32 have none.
fn button_bit(button: u32) -> u32 {
    1u32.checked_shl(button.wrapping_sub(1)).unwrap_or(0)
}

/// `QApplication::notify` for mouse, wheel and context-menu events (qapplication.cpp:2689-2762):
/// the innermost widget gets the event first; while it is not handled and accepted, the event goes
/// to the parent (in the parent's coordinates), up to and including the window's root widget.
/// A disabled widget does not handle mouse events (`QWidget::event`, qwidget.cpp:8978-8998), so
/// they pass through it. Returns whether some widget handled and accepted the event.
fn deliver_with_propagation(path: &[HitStep], make_kind: impl Fn(Point) -> EventKind) -> bool {
    for step in path.iter().rev() {
        if !step.enabled {
            continue;
        }
        let mut ev = Event::new_spontaneous(make_kind(step.pos));
        let handled = step.widget.borrow_mut().event(&mut ev);
        if handled && ev.is_accepted() {
            return true;
        }
        if step
            .widget
            .borrow()
            .widget_base()
            .has_no_mouse_propagation()
        {
            return false;
        }
    }
    false
}

/// The buttonless MouseMove walk of `QApplication::notify` (qapplication.cpp:2738-2766), innermost
/// first. A widget without mouse tracking is filter-only and ends the walk: `res = true` with the
/// still-accepted copy (2746-2758). A tracking widget that does not accept the move (the default
/// `QWidget::mouseMoveEvent` ignores it, qwidget.cpp:9567-9570; widget.rs `EmptyWidget` returns false)
/// passes it to its parent. The walk stops at the window root, which is `path[0]` (`isWindow`,
/// 2759). `WA_NoMousePropagation` has no ferrokit equivalent. Disabled widgets are skipped after the
/// tracking check, as in `deliver_with_propagation`.
fn deliver_tracking_move(path: &[HitStep]) -> bool {
    for step in path.iter().rev() {
        if !step.widget.borrow().widget_base().has_mouse_tracking() {
            return true;
        }
        if !step.enabled {
            continue;
        }
        let mut ev = Event::new_spontaneous(EventKind::MouseMove {
            x: step.pos.x,
            y: step.pos.y,
        });
        let handled = step.widget.borrow_mut().event(&mut ev);
        if handled && ev.is_accepted() {
            return true;
        }
        if step
            .widget
            .borrow()
            .widget_base()
            .has_no_mouse_propagation()
        {
            return false;
        }
    }
    false
}

/// A double click propagates like `deliver_with_propagation`, except that a widget which does not
/// handle `MouseButtonDblClick` is given the press instead: the default
/// `QWidget::mouseDoubleClickEvent` calls `mousePressEvent` (qwidget.cpp:9636). The OS reports the
/// second press of a double click only as the double click, so without this a button would lose
/// the second click.
fn deliver_double_click(path: &[HitStep], button: u32) -> bool {
    for step in path.iter().rev() {
        if !step.enabled {
            continue;
        }
        let mut dbl = Event::new_spontaneous(EventKind::MouseButtonDblClick {
            x: step.pos.x,
            y: step.pos.y,
            button,
        });
        let handled = step.widget.borrow_mut().event(&mut dbl);
        if handled {
            if dbl.is_accepted() {
                return true;
            }
            continue;
        }
        let mut press = Event::new_spontaneous(EventKind::MouseButtonPress {
            x: step.pos.x,
            y: step.pos.y,
            button,
        });
        if step.widget.borrow_mut().event(&mut press) && press.is_accepted() {
            return true;
        }
        if step
            .widget
            .borrow()
            .widget_base()
            .has_no_mouse_propagation()
        {
            return false;
        }
    }
    false
}

pub fn dispatch_event_to_tree(root: &WidgetRef, event: &mut Event) -> bool {
    let mut dispatcher = EventTreeDispatcher::new();
    dispatcher.dispatch_event(root, event)
}
