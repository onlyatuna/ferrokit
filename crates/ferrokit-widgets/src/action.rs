// SPDX-License-Identifier: LGPL-3.0-only
//! Actions, action groups and key sequences (`QAction`, `QActionGroup`, `QKeySequence`).

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use ferrokit_core::object::{ObjectData, ObjectId, QObject};
use ferrokit_core::signal::Signal;
use ferrokit_core::variant::Variant;
use ferrokit_gui::image::Icon;

use crate::menu::MenuRef;

/// Shared, reference-counted handle to an [`Action`].
///
/// Actions are shared between menus, menu bars and tool bars exactly like
/// `QAction*` pointers are shared in Qt.
pub type ActionRef = Rc<RefCell<Action>>;
/// Weak counterpart of [`ActionRef`].
pub type ActionWeak = Weak<RefCell<Action>>;

/// Key code and modifier helpers shared by the menu, tool bar and tab widgets.
///
/// Widgets in this crate receive either Qt key codes (`Qt::Key_*`) or legacy
#[allow(dead_code)]
pub(crate) mod keys {
    pub const ESCAPE: u32 = 0x0100_0000;
    pub const TAB: u32 = 0x0100_0001;
    pub const BACKTAB: u32 = 0x0100_0002;
    pub const BACKSPACE: u32 = 0x0100_0003;
    pub const RETURN: u32 = 0x0100_0004;
    pub const ENTER: u32 = 0x0100_0005;
    pub const INSERT: u32 = 0x0100_0006;
    pub const DELETE: u32 = 0x0100_0007;
    pub const HOME: u32 = 0x0100_0010;
    pub const END: u32 = 0x0100_0011;
    pub const LEFT: u32 = 0x0100_0012;
    pub const UP: u32 = 0x0100_0013;
    pub const RIGHT: u32 = 0x0100_0014;
    pub const DOWN: u32 = 0x0100_0015;
    pub const PAGE_UP: u32 = 0x0100_0016;
    pub const PAGE_DOWN: u32 = 0x0100_0017;
    pub const SHIFT: u32 = 0x0100_0020;
    pub const CONTROL: u32 = 0x0100_0021;
    #[allow(dead_code)]
    pub const META: u32 = 0x0100_0022;
    pub const ALT: u32 = 0x0100_0023;
    pub const F1: u32 = 0x0100_0030;
    pub const SPACE: u32 = 0x20;

    pub const MOD_SHIFT: u32 = 0x0200_0000;
    pub const MOD_CTRL: u32 = 0x0400_0000;
    pub const MOD_ALT: u32 = 0x0800_0000;
    pub const MOD_META: u32 = 0x1000_0000;
    pub const MOD_MASK: u32 = MOD_SHIFT | MOD_CTRL | MOD_ALT | MOD_META;

    /// Normalizes modifier bits to Qt `KeyboardModifier` flags, accepting the
    /// compact `KeyboardModifiers` bits of `ferrokit_core` (shift=1, ctrl=2, alt=4, meta=8).
    pub fn normalize_modifiers(modifiers: u32) -> u32 {
        let mut out = modifiers & MOD_MASK;
        if modifiers & 1 != 0 {
            out |= MOD_SHIFT;
        }
        if modifiers & 2 != 0 {
            out |= MOD_CTRL;
        }
        if modifiers & 4 != 0 {
            out |= MOD_ALT;
        }
        if modifiers & 8 != 0 {
            out |= MOD_META;
        }
        out
    }

    /// Returns the uppercase character typed by a normalized key, for mnemonic matching.
    pub fn key_char(key: u32) -> Option<char> {
        match key {
            0x30..=0x39 | 0x41..=0x5A => char::from_u32(key),
            _ => None,
        }
    }

    /// Returns the mnemonic character (the character after a single `&`) of `text`, uppercased.
    pub fn mnemonic(text: &str) -> Option<char> {
        let mut chars = text.chars();
        while let Some(c) = chars.next() {
            if c == '&' {
                match chars.next() {
                    Some('&') => continue,
                    Some(m) => return m.to_uppercase().next(),
                    None => return None,
                }
            }
        }
        None
    }

    /// Removes mnemonic markers: `&F` becomes `F` and `&&` becomes `&`.
    pub fn strip_mnemonic(text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut chars = text.chars();
        while let Some(c) = chars.next() {
            if c == '&' {
                if let Some(next) = chars.next() {
                    out.push(next);
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    /// Returns the byte offset (in the stripped text) of the mnemonic character, if any.
    pub fn mnemonic_offset(text: &str) -> Option<usize> {
        let mut stripped_len = 0usize;
        let mut chars = text.chars();
        while let Some(c) = chars.next() {
            if c == '&' {
                match chars.next() {
                    Some('&') => stripped_len += 1,
                    Some(_) => return Some(stripped_len),
                    None => return None,
                }
            } else {
                stripped_len += c.len_utf8();
            }
        }
        None
    }

    #[allow(dead_code)]
    pub fn has_ctrl(modifiers: u32) -> bool {
        modifiers & MOD_CTRL != 0
    }

    #[allow(dead_code)]
    pub fn has_shift(modifiers: u32) -> bool {
        modifiers & MOD_SHIFT != 0
    }

    #[allow(dead_code)]
    pub fn has_alt(modifiers: u32) -> bool {
        modifiers & MOD_ALT != 0
    }
}

pub use crate::key_sequence_edit::{KeySequence, QKeySequence};

/// Kind of activation delivered to an action (`QAction::ActionEvent`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionEvent {
    Trigger,
    Hover,
}

/// Exclusivity policy of an [`ActionGroup`] (`QActionGroup::ExclusionPolicy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExclusionPolicy {
    /// Actions are independent.
    None,
    /// Exactly one checkable action can be checked; the checked one cannot be unchecked by triggering it.
    #[default]
    Exclusive,
    /// At most one action can be checked; the checked one can be unchecked.
    ExclusiveOptional,
}

/// Interior state of an action group, shared with its member actions.
struct GroupState {
    actions: RefCell<Vec<(ObjectId, ActionWeak)>>,
    current: Cell<Option<ObjectId>>,
    exclusion: Cell<ExclusionPolicy>,
    enabled: Cell<bool>,
    visible: Cell<bool>,
    triggered: Signal<ActionRef>,
    hovered: Signal<ActionRef>,
}

impl GroupState {
    fn find(&self, id: ObjectId) -> Option<ActionRef> {
        let weak = self
            .actions
            .borrow()
            .iter()
            .find(|(aid, _)| *aid == id)
            .map(|(_, w)| w.clone());
        weak.and_then(|w| w.upgrade())
    }

    fn remove(&self, id: ObjectId) {
        self.actions.borrow_mut().retain(|(aid, _)| *aid != id);
        if self.current.get() == Some(id) {
            self.current.set(None);
        }
    }

    /// Mirrors `QActionGroup::_q_actionChanged`: keeps at most one checked action.
    ///
    /// The unchecked previous action's signals are returned, not emitted: the caller emits
    /// them once no action is borrowed.
    fn action_check_changed(&self, id: ObjectId, checked: bool) -> Option<CheckedNotice> {
        if self.exclusion.get() == ExclusionPolicy::None {
            return None;
        }
        if checked {
            let previous = self.current.replace(Some(id));
            let prev_id = previous.filter(|p| *p != id)?;
            let prev = self.find(prev_id)?;
            let mut prev = prev.try_borrow_mut().ok()?;
            prev.change_checked(false)
        } else {
            if self.current.get() == Some(id) {
                self.current.set(None);
            }
            None
        }
    }
}

/// Signals owed after a checked-state change, to emit with no action borrowed.
struct CheckedNotice {
    /// The exclusive group's previously checked action, unchecked by this change.
    unchecked: Option<Box<CheckedNotice>>,
    changed: Signal<()>,
    toggled: Signal<bool>,
    checked: bool,
}

impl CheckedNotice {
    fn emit(self) {
        if let Some(prev) = self.unchecked {
            prev.emit();
        }
        self.changed.emit(&());
        self.toggled.emit(&self.checked);
    }
}

/// An abstract user command that can be placed in menus and tool bars (`QAction`).
pub struct Action {
    object_data: ObjectData,
    text: String,
    icon_text: String,
    tool_tip: String,
    status_tip: String,
    icon: Icon,
    shortcut: KeySequence,
    checkable: bool,
    checked: bool,
    enabled: bool,
    visible: bool,
    separator: bool,
    data: Variant,
    menu: Option<MenuRef>,
    group: Option<Weak<GroupState>>,

    /// Emitted when the action is activated; carries the checked state.
    pub triggered: Signal<bool>,
    /// Emitted whenever the checked state changes.
    pub toggled: Signal<bool>,
    /// Emitted when any property of the action changes.
    pub changed: Signal<()>,
    /// Emitted when the user highlights the action.
    pub hovered: Signal<()>,
}

pub type QAction = Action;

impl Action {
    /// Creates a new action with the given text (mnemonics use `&`).
    pub fn new(text: impl Into<String>) -> Self {
        let object_data = ObjectData::new(ObjectId::next());
        let triggered = Signal::with_owner(&object_data);
        let toggled = Signal::with_owner(&object_data);
        let changed = Signal::with_owner(&object_data);
        let hovered = Signal::with_owner(&object_data);
        Self {
            object_data,
            text: text.into(),
            icon_text: String::new(),
            tool_tip: String::new(),
            status_tip: String::new(),
            icon: Icon::default(),
            shortcut: KeySequence::default(),
            checkable: false,
            checked: false,
            enabled: true,
            visible: true,
            separator: false,
            data: Variant::Invalid,
            menu: None,
            group: None,
            triggered,
            toggled,
            changed,
            hovered,
        }
    }

    /// Creates a new shared action.
    pub fn new_ref(text: impl Into<String>) -> ActionRef {
        Rc::new(RefCell::new(Self::new(text)))
    }

    /// Creates a new shared separator action.
    pub fn separator_ref() -> ActionRef {
        let mut action = Self::new("");
        action.separator = true;
        Rc::new(RefCell::new(action))
    }

    pub fn id(&self) -> ObjectId {
        self.object_data.id()
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn set_text(&mut self, text: impl Into<String>) {
        let text = text.into();
        if self.text != text {
            self.text = text;
            self.changed.emit(&());
        }
    }

    /// Text without mnemonic markers.
    pub fn display_text(&self) -> String {
        keys::strip_mnemonic(&self.text)
    }

    /// Uppercased mnemonic character of the text, if any.
    pub fn mnemonic(&self) -> Option<char> {
        keys::mnemonic(&self.text)
    }

    /// Short text used by tool buttons; falls back to the stripped text.
    pub fn icon_text(&self) -> String {
        if self.icon_text.is_empty() {
            self.display_text()
        } else {
            self.icon_text.clone()
        }
    }

    pub fn set_icon_text(&mut self, text: impl Into<String>) {
        self.icon_text = text.into();
        self.changed.emit(&());
    }

    /// Tool tip; falls back to the stripped text like Qt.
    pub fn tool_tip(&self) -> String {
        if self.tool_tip.is_empty() {
            self.display_text()
        } else {
            self.tool_tip.clone()
        }
    }

    pub fn set_tool_tip(&mut self, tip: impl Into<String>) {
        self.tool_tip = tip.into();
        self.changed.emit(&());
    }

    pub fn status_tip(&self) -> &str {
        &self.status_tip
    }

    pub fn set_status_tip(&mut self, tip: impl Into<String>) {
        self.status_tip = tip.into();
        self.changed.emit(&());
    }

    pub fn icon(&self) -> &Icon {
        &self.icon
    }

    pub fn set_icon(&mut self, icon: Icon) {
        self.icon = icon;
        self.changed.emit(&());
    }

    pub fn shortcut(&self) -> KeySequence {
        self.shortcut.clone()
    }

    pub fn set_shortcut(&mut self, shortcut: KeySequence) {
        if self.shortcut != shortcut {
            self.shortcut = shortcut;
            self.changed.emit(&());
        }
    }

    pub fn is_checkable(&self) -> bool {
        self.checkable
    }

    /// Makes the action checkable; unchecks it when it becomes non-checkable.
    pub fn set_checkable(&mut self, checkable: bool) {
        if self.checkable == checkable {
            return;
        }
        if !checkable && self.checked {
            self.set_checked(false);
        }
        self.checkable = checkable;
        self.changed.emit(&());
    }

    pub fn is_checked(&self) -> bool {
        self.checked
    }

    /// Sets the checked state (`QAction::setChecked`). Ignored for non-checkable actions.
    ///
    /// Inside an exclusive group, checking this action unchecks the previously checked one.
    ///
    /// The `changed` and `toggled` slots run before this returns, while the caller still holds
    /// the borrow of `self`; [`Action::trigger`] emits them after releasing it.
    pub fn set_checked(&mut self, checked: bool) {
        if let Some(notice) = self.change_checked(checked) {
            notice.emit();
        }
    }

    /// Applies a checked-state change and returns the signals it owes.
    fn change_checked(&mut self, checked: bool) -> Option<CheckedNotice> {
        if !self.checkable || self.checked == checked {
            return None;
        }
        self.checked = checked;
        let unchecked = self
            .group_state()
            .and_then(|group| group.action_check_changed(self.id(), checked))
            .map(Box::new);
        Some(CheckedNotice {
            unchecked,
            changed: self.changed.clone(),
            toggled: self.toggled.clone(),
            checked,
        })
    }

    /// Flips the checked state of a checkable action (`QAction::toggle`).
    pub fn toggle(&mut self) {
        let next = !self.checked;
        self.set_checked(next);
    }

    /// Effective enabled state: the action and its group must both be enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled && self.group_state().is_none_or(|g| g.enabled.get())
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        if self.enabled != enabled {
            self.enabled = enabled;
            self.changed.emit(&());
        }
    }

    /// Effective visibility: the action and its group must both be visible.
    pub fn is_visible(&self) -> bool {
        self.visible && self.group_state().is_none_or(|g| g.visible.get())
    }

    pub fn set_visible(&mut self, visible: bool) {
        if self.visible != visible {
            self.visible = visible;
            self.changed.emit(&());
        }
    }

    pub fn is_separator(&self) -> bool {
        self.separator
    }

    pub fn set_separator(&mut self, separator: bool) {
        if self.separator != separator {
            self.separator = separator;
            self.changed.emit(&());
        }
    }

    pub fn data(&self) -> &Variant {
        &self.data
    }

    pub fn set_data(&mut self, data: Variant) {
        self.data = data;
    }

    /// Sub-menu opened by this action, if any.
    pub fn menu(&self) -> Option<MenuRef> {
        self.menu.clone()
    }

    pub fn set_menu(&mut self, menu: Option<MenuRef>) {
        self.menu = menu;
        self.changed.emit(&());
    }

    /// Returns true if this action belongs to an action group that is still alive.
    pub fn has_action_group(&self) -> bool {
        self.group_state().is_some()
    }

    /// Returns true if this action is in a group with an exclusive policy (rendered as a radio item).
    pub fn is_exclusive_in_group(&self) -> bool {
        self.group_state()
            .is_some_and(|g| g.exclusion.get() != ExclusionPolicy::None)
    }

    fn group_state(&self) -> Option<Rc<GroupState>> {
        self.group.as_ref().and_then(|w| w.upgrade())
    }

    /// Triggers the action (`QAction::trigger`).
    pub fn trigger(action: &ActionRef) {
        Self::activate(action, ActionEvent::Trigger);
    }

    /// Emits the hover notifications of the action (`QAction::hover`).
    pub fn hover(action: &ActionRef) {
        Self::activate(action, ActionEvent::Hover);
    }

    /// Activates the action (`QAction::activate`).
    ///
    /// A trigger on a disabled action is ignored. A checkable action toggles its
    /// state, except the checked action of an exclusive group, which stays checked.
    /// Signals are emitted after the action borrow is released so slots may inspect it.
    pub fn activate(action: &ActionRef, event: ActionEvent) {
        match event {
            ActionEvent::Trigger => {
                let (notice, signal, checked, group) = {
                    let mut a = action.borrow_mut();
                    if !a.is_enabled() {
                        return;
                    }
                    let group = a.group_state();
                    let mut notice = None;
                    if a.checkable {
                        let locked = a.checked
                            && group.as_ref().is_some_and(|g| {
                                g.exclusion.get() == ExclusionPolicy::Exclusive
                                    && g.current.get() == Some(a.id())
                            });
                        if !locked {
                            let next = !a.checked;
                            notice = a.change_checked(next);
                        }
                    }
                    (notice, a.triggered.clone(), a.checked, group)
                };
                if let Some(notice) = notice {
                    notice.emit();
                }
                signal.emit(&checked);
                if let Some(group) = group {
                    group.triggered.emit(action);
                }
            }
            ActionEvent::Hover => {
                let (signal, group) = {
                    let a = action.borrow();
                    (a.hovered.clone(), a.group_state())
                };
                signal.emit(&());
                if let Some(group) = group {
                    group.hovered.emit(action);
                }
            }
        }
    }
}

impl QObject for Action {
    fn object_data(&self) -> &ObjectData {
        &self.object_data
    }

    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.object_data)
    }

    fn as_qobject_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_qobject_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

/// Groups actions together, optionally making them mutually exclusive (`QActionGroup`).
pub struct ActionGroup {
    object_data: ObjectData,
    state: Rc<GroupState>,
    /// Emitted with the triggered member action.
    pub triggered: Signal<ActionRef>,
    /// Emitted with the hovered member action.
    pub hovered: Signal<ActionRef>,
}

pub type QActionGroup = ActionGroup;

impl ActionGroup {
    /// Creates an exclusive action group.
    pub fn new() -> Self {
        let object_data = ObjectData::new(ObjectId::next());
        let triggered = Signal::with_owner(&object_data);
        let hovered = Signal::with_owner(&object_data);
        Self {
            object_data,
            state: Rc::new(GroupState {
                actions: RefCell::new(Vec::new()),
                current: Cell::new(None),
                exclusion: Cell::new(ExclusionPolicy::Exclusive),
                enabled: Cell::new(true),
                visible: Cell::new(true),
                triggered: triggered.clone(),
                hovered: hovered.clone(),
            }),
            triggered,
            hovered,
        }
    }

    /// Adds an action to the group, removing it from any previous group.
    pub fn add_action(&mut self, action: &ActionRef) -> ActionRef {
        let (id, checked, old_group) = {
            let a = action.borrow();
            (a.id(), a.checked, a.group_state())
        };
        if let Some(old) = old_group {
            if Rc::ptr_eq(&old, &self.state) {
                return action.clone();
            }
            old.remove(id);
        }
        action.borrow_mut().group = Some(Rc::downgrade(&self.state));
        {
            let mut members = self.state.actions.borrow_mut();
            // A member that was dropped without `remove_action` must not pile up.
            members.retain(|(_, w)| w.strong_count() > 0);
            members.push((id, Rc::downgrade(action)));
        }
        let notice = if checked {
            self.state.action_check_changed(id, true)
        } else {
            None
        };
        if let Some(notice) = notice {
            notice.emit();
        }
        action.borrow().changed.emit(&());
        action.clone()
    }

    /// Creates a new action with `text` and adds it to the group.
    pub fn add_new_action(&mut self, text: impl Into<String>) -> ActionRef {
        let action = Action::new_ref(text);
        self.add_action(&action)
    }

    /// Removes an action from the group.
    pub fn remove_action(&mut self, action: &ActionRef) {
        let id = action.borrow().id();
        let member = action
            .borrow()
            .group_state()
            .is_some_and(|g| Rc::ptr_eq(&g, &self.state));
        if member {
            self.state.remove(id);
            action.borrow_mut().group = None;
            action.borrow().changed.emit(&());
        }
    }

    /// Number of member entries held, dead ones included (for tests of the pruning).
    #[doc(hidden)]
    pub fn member_slots(&self) -> usize {
        self.state.actions.borrow().len()
    }

    /// Returns the live member actions in insertion order.
    pub fn actions(&self) -> Vec<ActionRef> {
        self.state
            .actions
            .borrow()
            .iter()
            .filter_map(|(_, w)| w.upgrade())
            .collect()
    }

    /// Returns the currently checked action of an exclusive group.
    pub fn checked_action(&self) -> Option<ActionRef> {
        self.state.current.get().and_then(|id| self.state.find(id))
    }

    pub fn exclusion_policy(&self) -> ExclusionPolicy {
        self.state.exclusion.get()
    }

    pub fn set_exclusion_policy(&mut self, policy: ExclusionPolicy) {
        self.state.exclusion.set(policy);
    }

    pub fn is_exclusive(&self) -> bool {
        self.state.exclusion.get() != ExclusionPolicy::None
    }

    pub fn set_exclusive(&mut self, exclusive: bool) {
        self.set_exclusion_policy(if exclusive {
            ExclusionPolicy::Exclusive
        } else {
            ExclusionPolicy::None
        });
    }

    pub fn is_enabled(&self) -> bool {
        self.state.enabled.get()
    }

    /// Enables or disables all member actions.
    pub fn set_enabled(&mut self, enabled: bool) {
        if self.state.enabled.replace(enabled) != enabled {
            self.notify_members();
        }
    }

    pub fn is_visible(&self) -> bool {
        self.state.visible.get()
    }

    /// Shows or hides all member actions.
    pub fn set_visible(&mut self, visible: bool) {
        if self.state.visible.replace(visible) != visible {
            self.notify_members();
        }
    }

    fn notify_members(&self) {
        for action in self.actions() {
            if let Ok(a) = action.try_borrow() {
                a.changed.emit(&());
            }
        }
    }
}

impl Default for ActionGroup {
    fn default() -> Self {
        Self::new()
    }
}

impl QObject for ActionGroup {
    fn object_data(&self) -> &ObjectData {
        &self.object_data
    }

    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.object_data)
    }

    fn as_qobject_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_qobject_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}
