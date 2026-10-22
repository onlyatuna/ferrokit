// SPDX-License-Identifier: LGPL-3.0-only
use crate::widget::{WidgetRef, WidgetWeak};
use ferrokit_core::event::{Event, EventKind, FocusReason};
use ferrokit_core::object::ObjectId;
use std::cell::{Cell, RefCell};
use crate::application::{Application, FocusTicket};
use std::rc::Rc;

/// Focus policy defining how a widget accepts keyboard focus (`Qt::FocusPolicy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FocusPolicy {
    /// Widget does not accept keyboard focus at all.
    #[default]
    NoFocus = 0,
    /// Widget accepts focus only via Tab key navigation.
    TabFocus = 1,
    /// Widget accepts focus only by clicking with mouse.
    ClickFocus = 2,
    /// Widget accepts focus by both Tab navigation and mouse click (`TabFocus | ClickFocus`).
    StrongFocus = 3,
    /// Widget accepts focus by Tab, mouse click, and mouse wheel.
    WheelFocus = 7,
}

impl FocusPolicy {
    /// Returns true if the policy permits Tab navigation focus.
    pub fn accepts_tab(&self) -> bool {
        matches!(self, Self::TabFocus | Self::StrongFocus | Self::WheelFocus)
    }

    /// Returns true if the policy permits mouse click focus.
    pub fn accepts_click(&self) -> bool {
        matches!(
            self,
            Self::ClickFocus | Self::StrongFocus | Self::WheelFocus
        )
    }
}

/// Recursively finds a widget by its ObjectId in the widget tree.
pub fn find_widget_by_id(root: &WidgetRef, id: ObjectId) -> Option<WidgetRef> {
    if root.borrow().id() == id {
        return Some(root.clone());
    }
    for child in root.borrow().children() {
        if let Some(found) = find_widget_by_id(&child, id) {
            return Some(found);
        }
    }
    None
}

/// Recursively collects all visible, enabled widgets accepting Tab focus in pre-order traversal.
pub fn collect_tab_focusable(root: &WidgetRef, out: &mut Vec<WidgetRef>) {
    collect_tab_chain(root, None, out).expect("a widget of the tree is mutably borrowed");
}

/// `collect_tab_focusable`, also keeping `from` (the focus widget, though disabled) in its place.
/// Fails when a widget of the tree is mutably borrowed.
fn collect_tab_chain(
    root: &WidgetRef,
    from: Option<ObjectId>,
    out: &mut Vec<WidgetRef>,
) -> Result<(), std::cell::BorrowError> {
    let borrow = root.try_borrow()?;
    let is_from = from == Some(borrow.id());
    if !is_from && (!borrow.is_visible() || !borrow.is_enabled()) {
        return Ok(());
    }
    if is_from || borrow.focus_policy().accepts_tab() {
        out.push(root.clone());
    }
    // A hidden `from` (the focus widget, or the ancestor being hidden) keeps its place, but its
    // subtree is not a candidate.
    if is_from && !borrow.is_visible() {
        return Ok(());
    }
    let children = borrow.children();
    drop(borrow);
    for child in children {
        collect_tab_chain(&child, from, out)?;
    }
    Ok(())
}

/// `QWidget::focusNextChild` from `from`: the widget after it in the Tab chain, wrapping around.
fn next_in_tab_chain(
    root: &WidgetRef,
    from: ObjectId,
) -> Result<Option<WidgetRef>, std::cell::BorrowError> {
    let mut chain = Vec::new();
    collect_tab_chain(root, Some(from), &mut chain)?;
    let Some(i) = chain.iter().position(|w| w.borrow().id() == from) else {
        return Ok(None);
    };
    Ok((chain.len() > 1).then(|| chain[(i + 1) % chain.len()].clone()))
}

fn send_focus_out(root: &WidgetRef, id: ObjectId, reason: FocusReason) {
    if let Some(old_widget) = find_widget_by_id(root, id) {
        let mut old = old_widget.borrow_mut();
        old.set_has_focus(false);
        let mut ev = Event::new_spontaneous(EventKind::FocusOut { reason });
        // `event()` routes FocusOut to `focus_out_event`; a widget whose `event` does not handle it
        // still gets the virtual call, so the handler runs once either way.
        if !old.event(&mut ev) {
            old.focus_out_event(reason);
        }
        old.update();
    }
}

/// A window's focus widget (`window()->focusWidget()`), shared with the widget that holds the
/// focus so that disabling it drops the focus at once (`WidgetBase::set_enabled`).
#[derive(Debug, Default)]
pub(crate) struct FocusState {
    focused: Cell<Option<ObjectId>>,
    /// Focus widgets disabled since the events were last sent, in order.
    lost: RefCell<Vec<LostFocus>>,
    /// The window the `FocusManager` works on, where a lost focus moves on.
    root: RefCell<WidgetWeak>,
}

/// A focus widget that was disabled; its `FocusOut` and the next widget's `FocusIn` are still to
/// come.
#[derive(Debug, Clone, Copy)]
struct LostFocus {
    id: ObjectId,
    /// Where `focusNextChild()` starts: `id`, or the hidden ancestor.
    anchor: ObjectId,
    next: NextFocus,
}

/// Where the focus of a disabled focus widget went.
#[derive(Debug, Clone, Copy)]
enum NextFocus {
    /// `clearFocus()`: no widget is next, or the parent is disabled.
    Cleared,
    /// `focusNextChild()` picked this widget.
    To(ObjectId),
    /// The window was borrowed, so the next widget is picked when the events are sent.
    Later,
}

impl Drop for FocusState {
    fn drop(&mut self) {
        crate::application::Application::focus_owner_dropped(self as *const Self as usize);
    }
}

impl FocusState {
    /// `id` is being disabled: if it is the focus widget, the focus moves on now, as
    /// `setEnabled_helper` does before it disables the children (qwidget.cpp:3439-3445):
    /// `focusNextChild()` (which can pick a child about to be disabled), or `clearFocus()` when the
    /// parent is disabled. Only the state changes here; `move_lost_focus` sends the events.
    /// Returns whether `id` had focus.
    pub(crate) fn lose(self: &Rc<Self>, id: ObjectId, parent_enabled: bool) -> bool {
        self.lose_via(id, id, parent_enabled)
    }

    /// `lose` for a focus widget `id` that stops being shown because its ancestor `anchor` (or
    /// itself) was hidden: `focusNextChild()` runs from `anchor` (`hide_helper`, qwidget.cpp:8276-8283).
    pub(crate) fn lose_via(self: &Rc<Self>, id: ObjectId, anchor: ObjectId, parent_enabled: bool) -> bool {
        if self.focused.get() != Some(id) {
            return false;
        }
        let root = parent_enabled
            .then(|| self.root.borrow().upgrade())
            .flatten();
        let next = match root.map(|root| next_in_tab_chain(&root, anchor)) {
            _ if !parent_enabled => NextFocus::Cleared,
            Some(Ok(Some(widget))) => match widget.try_borrow() {
                Ok(w) => {
                    *w.widget_base().focus_state.borrow_mut() = Rc::downgrade(self);
                    w.set_has_focus(true);
                    NextFocus::To(w.id())
                }
                Err(_) => NextFocus::Later,
            },
            Some(Ok(None)) => NextFocus::Cleared,
            Some(Err(_)) | None => NextFocus::Later,
        };
        self.focused.set(match next {
            NextFocus::To(next) => Some(next),
            _ => None,
        });
        self.lost.borrow_mut().push(LostFocus { id, anchor, next });
        true
    }
}

fn give_focus(state: &Rc<FocusState>, widget: &WidgetRef, reason: FocusReason) {
    state.focused.set(Some(widget.borrow().id()));
    *widget.borrow().widget_base().focus_state.borrow_mut() = Rc::downgrade(state);
    // `QApplicationPrivate::setFocusWidget`: the focus widget is current while `FocusIn` runs;
    // `focusChanged` follows it.
    let ticket = begin_publish(state, Some(widget));
    send_focus_in(widget, reason);
    Application::finish_focus_publish(ticket);
}

/// Records the window's focus widget with `Application` (see `Application::begin_focus_publish`).
fn begin_publish(state: &Rc<FocusState>, widget: Option<&WidgetRef>) -> FocusTicket {
    let window = widget
        .and_then(|w| w.try_borrow().ok().and_then(|w| w.window_id()))
        .or_else(|| {
            let root = state.root.borrow().upgrade()?;
            let id = root.try_borrow().ok()?.window_id();
            id
        });
    let new = widget.map(|w| (w.borrow().id(), Rc::downgrade(w)));
    Application::begin_focus_publish(Rc::as_ptr(state) as usize, window, new)
}

fn publish_focus(state: &Rc<FocusState>, widget: Option<&WidgetRef>) {
    Application::finish_focus_publish(begin_publish(state, widget));
}

fn send_focus_in(widget: &WidgetRef, reason: FocusReason) {
    let mut new = widget.borrow_mut();
    new.set_has_focus(true);
    let mut ev = Event::new_spontaneous(EventKind::FocusIn { reason });
    if !new.event(&mut ev) {
        new.focus_in_event(reason);
    }
    new.update();
}

/// Sends the events of the focus moves `FocusState::lose` made (qwidget.cpp:3442-3446): the
/// disabled widget's `FocusOut`, then the next widget's `FocusIn`. The widgets are borrowed while
/// they are disabled, so the events wait until here.
fn move_lost_focus(state: &Rc<FocusState>, root: &WidgetRef) {
    let lost = std::mem::take(&mut *state.lost.borrow_mut());
    for lost in lost {
        let next = match lost.next {
            NextFocus::Cleared => None,
            NextFocus::To(id) => find_widget_by_id(root, id),
            NextFocus::Later => next_in_tab_chain(root, lost.anchor).ok().flatten(),
        };
        let reason = if next.is_some() {
            FocusReason::Tab
        } else {
            FocusReason::Other
        };
        send_focus_out(root, lost.id, reason);
        match (next, lost.next) {
            (Some(next), NextFocus::Later) => give_focus(state, &next, reason),
            (Some(next), _) => {
                let ticket = begin_publish(state, Some(&next));
                send_focus_in(&next, reason);
                Application::finish_focus_publish(ticket);
            }
            (None, _) => publish_focus(state, None),
        }
    }
}

/// Whether no widget of the tree is borrowed, so the focus can move without a `BorrowMutError`.
fn tree_is_free(w: &WidgetRef) -> bool {
    if w.try_borrow_mut().is_err() {
        return false;
    }
    let children = w.borrow().children();
    children.iter().all(tree_is_free)
}

/// After the focus widget was disabled and released (`set_widget_enabled`): sends the events
/// and moves the focus on now, as `setEnabled` does before it returns. While a widget of the
/// window is still borrowed this would panic, so the move is left to
/// `FocusManager::process_pending` on the dispatcher's next event.
pub(crate) fn move_lost_focus_now(state: &Rc<FocusState>) {
    let root = state.root.borrow().upgrade();
    if let Some(root) = root.filter(tree_is_free) {
        move_lost_focus(state, &root);
    }
}

/// Window-level focus manager tracking the currently active focused widget
/// and managing Tab / Shift+Tab navigation order and click focus.
#[derive(Debug, Default)]
pub struct FocusManager {
    state: Rc<FocusState>,
}

impl FocusManager {
    /// Creates a new focus manager with no initially focused widget.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the ObjectId of the currently focused widget, if any.
    pub fn focused_widget_id(&self) -> Option<ObjectId> {
        self.state.focused.get()
    }

    /// Sets keyboard focus to the specified widget in the widget tree.
    ///
    /// Sends `FocusOut` to the previous focused widget and `FocusIn` to the new target.
    pub fn set_focus(
        &mut self,
        root: &WidgetRef,
        target_id: Option<ObjectId>,
        reason: FocusReason,
    ) -> bool {
        self.process_pending(root);
        if self.state.focused.get() == target_id {
            return false;
        }
        if let Some(old_id) = self.state.focused.take() {
            send_focus_out(root, old_id, reason);
        }
        let Some(new_id) = target_id else {
            publish_focus(&self.state, None);
            return true;
        };
        match find_widget_by_id(root, new_id) {
            Some(new_widget) => {
                give_focus(&self.state, &new_widget, reason);
                true
            }
            None => {
                publish_focus(&self.state, None);
                false
            }
        }
    }

    /// Sends the events and moves the focus for a focus widget disabled while the window was
    /// borrowed (see `move_lost_focus_now`), and records `root` as the window.
    pub fn process_pending(&mut self, root: &WidgetRef) {
        *self.state.root.borrow_mut() = Rc::downgrade(root);
        move_lost_focus(&self.state, root);
    }

    /// Clears keyboard focus from any currently focused widget.
    pub fn clear_focus(&mut self, root: &WidgetRef, reason: FocusReason) -> bool {
        self.set_focus(root, None, reason)
    }

    /// Navigates focus to the next focusable widget in the Tab order sequence.
    pub fn focus_next(&mut self, root: &WidgetRef) -> bool {
        let mut focusables = Vec::new();
        collect_tab_focusable(root, &mut focusables);
        if focusables.is_empty() {
            return false;
        }

        let next_id = match self.state.focused.get() {
            Some(curr_id) => {
                if let Some(idx) = focusables.iter().position(|w| w.borrow().id() == curr_id) {
                    let next_idx = (idx + 1) % focusables.len();
                    focusables[next_idx].borrow().id()
                } else {
                    focusables[0].borrow().id()
                }
            }
            None => focusables[0].borrow().id(),
        };

        self.set_focus(root, Some(next_id), FocusReason::Tab)
    }

    /// Navigates focus to the previous focusable widget in the Tab order sequence (Shift+Tab).
    pub fn focus_previous(&mut self, root: &WidgetRef) -> bool {
        let mut focusables = Vec::new();
        collect_tab_focusable(root, &mut focusables);
        if focusables.is_empty() {
            return false;
        }

        let prev_id = match self.state.focused.get() {
            Some(curr_id) => {
                if let Some(idx) = focusables.iter().position(|w| w.borrow().id() == curr_id) {
                    let prev_idx = (idx + focusables.len() - 1) % focusables.len();
                    focusables[prev_idx].borrow().id()
                } else {
                    focusables[focusables.len() - 1].borrow().id()
                }
            }
            None => focusables[focusables.len() - 1].borrow().id(),
        };

        self.set_focus(root, Some(prev_id), FocusReason::Backtab)
    }

    /// Handles mouse click on a hit widget, setting focus if click focus is accepted.
    pub fn handle_mouse_click(&mut self, root: &WidgetRef, hit_widget: &WidgetRef) -> bool {
        // `giveFocusAccordingToFocusPolicy` (qapplication.cpp): from the clicked widget up to its
        // window, the first enabled widget that accepts click focus takes it.
        let mut current = Some(Rc::clone(hit_widget));
        while let Some(widget) = current {
            let (id, enabled, accepts_click, parent) = {
                let w = widget.borrow();
                (w.id(), w.is_enabled(), w.focus_policy().accepts_click(), w.parent_widget())
            };
            if enabled && accepts_click {
                return self.set_focus(root, Some(id), FocusReason::Mouse);
            }
            current = parent.and_then(|p| p.upgrade());
        }
        false
    }
}
