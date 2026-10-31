// SPDX-License-Identifier: LGPL-3.0-only
use ferrokit_core::signal::Signal;
use ferrokit_gui::geometry::primitives::Point;
use std::cell::RefCell;
use std::sync::{Arc, Mutex};

use super::{PlatformMenu, PlatformMenuItem};
use crate::objc_runtime::{
    ActionTarget, Class, Id, ObjcMsg, Sel, NS_CONTROL_STATE_VALUE_OFF, NS_CONTROL_STATE_VALUE_ON,
};

pub struct CocoaMenuItem {
    id: u32,
    ns_item: Id,
    text: Mutex<String>,
    separator: bool,
    checkable: bool,
    checked: Mutex<bool>,
    enabled: Mutex<bool>,
    activated: Signal<()>,
    submenu: Mutex<Option<Box<dyn PlatformMenu>>>,
    /// The object AppKit sends clicks to (`NSMenuItem.target` is weak, so it lives here).
    target: RefCell<Option<ActionTarget>>,
}

// Deliberately neither `Send` nor `Sync`: wraps an `NSMenuItem` (`Id`), which AppKit only allows
// to be used from the main thread.

impl CocoaMenuItem {
    pub fn new_action(id: u32, text: &str) -> Self {
        let item_class = Class::get("NSMenuItem").unwrap_or(Class::NIL);
        // SAFETY: `alloc` takes no arguments and returns an object, matching `send_class_0`
        // (a nil class from `unwrap_or` makes the send a no-op returning nil).
        let ns_item_alloc = unsafe { ObjcMsg::send_class_0(item_class, Sel::register("alloc")) };
        // SAFETY: `send_menu_item_init` sends `initWithTitle:action:keyEquivalent:` (NSString,
        // selector, NSString) to the unconsumed `alloc` result above; `ActionTarget::action()` is
        // a registered selector. The +1 is never released, so `ns_item` is live for the life of
        // this value (and beyond, inside its menu).
        let ns_item = unsafe {
            ObjcMsg::send_menu_item_init(ns_item_alloc, text, ActionTarget::action(), "")
        };

        Self {
            id,
            ns_item,
            text: Mutex::new(text.to_string()),
            separator: false,
            checkable: false,
            checked: Mutex::new(false),
            enabled: Mutex::new(true),
            activated: Signal::new(),
            submenu: Mutex::new(None),
            target: RefCell::new(None),
        }
    }

    pub fn new_checkable(id: u32, text: &str, checked: bool) -> Self {
        let item_class = Class::get("NSMenuItem").unwrap_or(Class::NIL);
        // SAFETY: `alloc` takes no arguments and returns an object, matching `send_class_0`
        // (a nil class from `unwrap_or` makes the send a no-op returning nil).
        let ns_item_alloc = unsafe { ObjcMsg::send_class_0(item_class, Sel::register("alloc")) };
        // SAFETY: `send_menu_item_init` sends `initWithTitle:action:keyEquivalent:` (NSString,
        // selector, NSString) to the unconsumed `alloc` result above; `ActionTarget::action()` is
        // a registered selector. The +1 is never released, so `ns_item` is live for the life of
        // this value (and beyond, inside its menu).
        let ns_item = unsafe {
            ObjcMsg::send_menu_item_init(ns_item_alloc, text, ActionTarget::action(), "")
        };

        let initial_state = if checked {
            NS_CONTROL_STATE_VALUE_ON
        } else {
            NS_CONTROL_STATE_VALUE_OFF
        };
        // SAFETY: `ns_item` is the item initialised above (nil is a no-op); `setState:` takes one
        // NSInteger (an `NSControlStateValue`).
        unsafe { ObjcMsg::send_int(ns_item, Sel::register("setState:"), initial_state) };

        Self {
            id,
            ns_item,
            text: Mutex::new(text.to_string()),
            separator: false,
            checkable: true,
            checked: Mutex::new(checked),
            enabled: Mutex::new(true),
            activated: Signal::new(),
            submenu: Mutex::new(None),
            target: RefCell::new(None),
        }
    }

    pub fn new_separator() -> Self {
        let item_class = Class::get("NSMenuItem").unwrap_or(Class::NIL);
        // SAFETY: `separatorItem` is a no-argument class method of `NSMenuItem` returning a shared
        // object (a nil class from `unwrap_or` makes the send a no-op returning nil).
        let ns_item = unsafe { ObjcMsg::send_class_0(item_class, Sel::register("separatorItem")) };

        Self {
            id: 0,
            ns_item,
            text: Mutex::new(String::new()),
            separator: true,
            checkable: false,
            checked: Mutex::new(false),
            enabled: Mutex::new(false),
            activated: Signal::new(),
            submenu: Mutex::new(None),
            target: RefCell::new(None),
        }
    }

    /// Routes AppKit clicks on this item to [`trigger`](Self::trigger), as a click on a Qt menu
    /// item reaches `QCocoaMenuItem`'s action. The target holds the item weakly, so the item's
    /// owner (the menu) stays the only strong reference.
    pub fn attach_target(self: &Arc<Self>) {
        let item = Arc::downgrade(self);
        let Ok(target) = ActionTarget::new(move || {
            if let Some(item) = item.upgrade() {
                item.trigger();
            }
        }) else {
            return;
        };
        // SAFETY: `self.ns_item` is the item created in `new_*` and never released, so it is
        // live; `target.id()` is the live ActionTarget, which is stored in `self.target` right
        // below and cleared from the item in `Drop` before it is released, so the item's weak
        // target never dangles. `setTarget:` takes one object.
        unsafe { ObjcMsg::send_id(self.ns_item, Sel::register("setTarget:"), target.id()) };
        *self.target.borrow_mut() = Some(target);
    }

    #[inline]
    pub fn native_item(&self) -> Id {
        self.ns_item
    }

    pub fn ns_control_state(&self) -> isize {
        if *self.checked.lock().unwrap() {
            NS_CONTROL_STATE_VALUE_ON
        } else {
            NS_CONTROL_STATE_VALUE_OFF
        }
    }

    pub fn trigger(&self) {
        if *self.enabled.lock().unwrap() {
            if self.checkable {
                let mut chk = self.checked.lock().unwrap();
                *chk = !*chk;
                let state = if *chk {
                    NS_CONTROL_STATE_VALUE_ON
                } else {
                    NS_CONTROL_STATE_VALUE_OFF
                };
                // SAFETY: `self.ns_item` is live (never released); `setState:` takes one NSInteger.
                unsafe { ObjcMsg::send_int(self.ns_item, Sel::register("setState:"), state) };
            }
            self.activated.emit(&());
        }
    }

    pub fn set_submenu(&self, menu: Box<dyn PlatformMenu>) {
        let sub_id = Id(menu.native_handle() as *mut std::ffi::c_void);
        // SAFETY: `self.ns_item` is live (never released); `sub_id` is the NSMenu handle of
        // `menu`, which is stored in `self.submenu` right below and so outlives the item's use
        // of it; `setSubmenu:` takes one NSMenu.
        unsafe { ObjcMsg::send_id(self.ns_item, Sel::register("setSubmenu:"), sub_id) };
        *self.submenu.lock().unwrap() = Some(menu);
    }

    pub fn has_submenu(&self) -> bool {
        self.submenu.lock().unwrap().is_some()
    }
}

impl Drop for CocoaMenuItem {
    fn drop(&mut self) {
        // The item may outlive us inside its NSMenu; it must not keep a dangling target.
        if self.target.get_mut().is_some() {
            // SAFETY: `self.ns_item` is live (its `alloc` +1 is never released); `setTarget:` takes
            // one object, here nil, which clears the item's weak reference to `self.target`
            // before that ActionTarget is dropped with `self`.
            unsafe { ObjcMsg::send_id(self.ns_item, Sel::register("setTarget:"), Id::NIL) };
        }
    }
}

impl PlatformMenuItem for CocoaMenuItem {
    fn id(&self) -> u32 {
        self.id
    }

    fn text(&self) -> String {
        self.text.lock().unwrap().clone()
    }

    fn set_text(&mut self, text: &str) {
        *self.text.lock().unwrap() = text.to_string();
        // SAFETY: `self.ns_item` is live (never released); `setTitle:` takes one NSString, which
        // `send_str` builds from `text`.
        unsafe { ObjcMsg::send_str(self.ns_item, Sel::register("setTitle:"), text) };
    }

    fn is_separator(&self) -> bool {
        self.separator
    }

    fn is_checkable(&self) -> bool {
        self.checkable
    }

    fn is_checked(&self) -> bool {
        *self.checked.lock().unwrap()
    }

    fn set_checked(&mut self, checked: bool) {
        *self.checked.lock().unwrap() = checked;
        let state = if checked {
            NS_CONTROL_STATE_VALUE_ON
        } else {
            NS_CONTROL_STATE_VALUE_OFF
        };
        // SAFETY: `self.ns_item` is live (never released); `setState:` takes one NSInteger.
        unsafe { ObjcMsg::send_int(self.ns_item, Sel::register("setState:"), state) };
    }

    fn is_enabled(&self) -> bool {
        *self.enabled.lock().unwrap()
    }

    fn set_enabled(&mut self, enabled: bool) {
        *self.enabled.lock().unwrap() = enabled;
        // SAFETY: `self.ns_item` is live (never released); `setEnabled:` takes one BOOL.
        unsafe { ObjcMsg::send_bool(self.ns_item, Sel::register("setEnabled:"), enabled) };
    }

    fn activated(&self) -> &Signal<()> {
        &self.activated
    }
}

pub struct CocoaMenu {
    ns_menu: Id,
    title: Mutex<String>,
    items: Mutex<Vec<Arc<CocoaMenuItem>>>,
    is_popped_up: Mutex<bool>,
    popup_pos: Mutex<Option<Point>>,
}

// Deliberately neither `Send` nor `Sync`: wraps an `NSMenu` (`Id`), which AppKit only allows to be
// used from the main thread.

impl CocoaMenu {
    pub fn new() -> Self {
        let menu_class = Class::get("NSMenu").unwrap_or(Class::NIL);
        // SAFETY: `alloc` takes no arguments and returns an object, matching `send_class_0`
        // (a nil class from `unwrap_or` makes the send a no-op returning nil).
        let ns_menu_alloc = unsafe { ObjcMsg::send_class_0(menu_class, Sel::register("alloc")) };
        // SAFETY: `initWithTitle:` takes one NSString, which `send_str` builds from ""; the
        // receiver is the unconsumed `alloc` result above. The +1 is never released, so
        // `ns_menu` is live for the life of this value.
        let ns_menu =
            unsafe { ObjcMsg::send_str(ns_menu_alloc, Sel::register("initWithTitle:"), "") };

        Self {
            ns_menu,
            title: Mutex::new(String::new()),
            items: Mutex::new(Vec::new()),
            is_popped_up: Mutex::new(false),
            popup_pos: Mutex::new(None),
        }
    }

    pub fn title(&self) -> String {
        self.title.lock().unwrap().clone()
    }

    pub fn set_title(&self, title: &str) {
        *self.title.lock().unwrap() = title.to_string();
        // SAFETY: `self.ns_menu` is live (never released); `setTitle:` takes one NSString, which
        // `send_str` builds from `title`.
        unsafe { ObjcMsg::send_str(self.ns_menu, Sel::register("setTitle:"), title) };
    }

    pub fn items(&self) -> Vec<Arc<CocoaMenuItem>> {
        self.items.lock().unwrap().clone()
    }

    pub fn is_popped_up(&self) -> bool {
        *self.is_popped_up.lock().unwrap()
    }

    pub fn popup_pos(&self) -> Option<Point> {
        *self.popup_pos.lock().unwrap()
    }

    pub fn trigger_item(&self, id: u32) -> bool {
        // Release the lock before emitting: a slot may call back into this menu.
        let item = self
            .items
            .lock()
            .unwrap()
            .iter()
            .find(|item| item.id == id)
            .cloned();
        match item {
            Some(item) => {
                item.trigger();
                true
            }
            None => false,
        }
    }
}

impl Default for CocoaMenu {
    fn default() -> Self {
        Self::new()
    }
}

// The `PlatformMenuItem` trait hands out `Arc`s; the items are only used from the GUI thread.
#[allow(clippy::arc_with_non_send_sync)]
impl PlatformMenu for CocoaMenu {
    fn add_action(&mut self, id: u32, text: &str) -> Arc<dyn PlatformMenuItem> {
        let item = Arc::new(CocoaMenuItem::new_action(id, text));
        item.attach_target();
        // SAFETY: `self.ns_menu` is live (never released) and `item.native_item()` is the live
        // NSMenuItem just created (kept alive by `item` in `self.items` and by its own +1);
        // `addItem:` takes one NSMenuItem and retains it.
        unsafe { ObjcMsg::send_id(self.ns_menu, Sel::register("addItem:"), item.native_item()) };
        self.items.lock().unwrap().push(item.clone());
        item
    }

    fn add_checkable(&mut self, id: u32, text: &str, checked: bool) -> Arc<dyn PlatformMenuItem> {
        let item = Arc::new(CocoaMenuItem::new_checkable(id, text, checked));
        item.attach_target();
        // SAFETY: `self.ns_menu` is live (never released) and `item.native_item()` is the live
        // NSMenuItem just created (kept alive by `item` in `self.items` and by its own +1);
        // `addItem:` takes one NSMenuItem and retains it.
        unsafe { ObjcMsg::send_id(self.ns_menu, Sel::register("addItem:"), item.native_item()) };
        self.items.lock().unwrap().push(item.clone());
        item
    }

    fn add_separator(&mut self) {
        let item = Arc::new(CocoaMenuItem::new_separator());
        // SAFETY: `self.ns_menu` is live (never released) and `item.native_item()` is the live
        // separator NSMenuItem just obtained; `addItem:` takes one NSMenuItem.
        unsafe { ObjcMsg::send_id(self.ns_menu, Sel::register("addItem:"), item.native_item()) };
        self.items.lock().unwrap().push(item);
    }

    fn add_submenu(&mut self, text: &str, submenu: Box<dyn PlatformMenu>) {
        let item =
            CocoaMenuItem::new_action((self.items.lock().unwrap().len() + 1000) as u32, text);
        item.set_submenu(submenu);
        // A submenu parent has no action of its own: with an action but no target AppKit would
        // disable it.
        // SAFETY: `item.native_item()` is the live NSMenuItem created just above; `setAction:`
        // takes one selector, here nil, matching `send_action`.
        unsafe { ObjcMsg::send_action(item.native_item(), Sel::register("setAction:"), Sel::NIL) };
        let arc_item = Arc::new(item);
        // SAFETY: `self.ns_menu` is live (never released) and `arc_item.native_item()` is the
        // live NSMenuItem created above, kept alive by `arc_item`; `addItem:` takes one NSMenuItem
        // and retains it.
        unsafe {
            ObjcMsg::send_id(
                self.ns_menu,
                Sel::register("addItem:"),
                arc_item.native_item(),
            )
        };
        self.items.lock().unwrap().push(arc_item);
    }

    fn show_popup(&self, screen_pos: Point) {
        *self.is_popped_up.lock().unwrap() = true;
        *self.popup_pos.lock().unwrap() = Some(screen_pos);
    }

    fn dismiss(&self) {
        *self.is_popped_up.lock().unwrap() = false;
        *self.popup_pos.lock().unwrap() = None;
    }

    fn native_handle(&self) -> isize {
        self.ns_menu.0 as isize
    }
}
