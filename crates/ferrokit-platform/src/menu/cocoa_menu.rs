// SPDX-License-Identifier: LGPL-3.0-only
use ferrokit_core::signal::Signal;
use ferrokit_gui::geometry::primitives::Point;
use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::{Arc, LazyLock, Mutex};

use super::{PlatformMenu, PlatformMenuItem};
use crate::objc_runtime::{
    ActionTarget, CGPoint, Class, Id, NS_CONTROL_STATE_VALUE_OFF, NS_CONTROL_STATE_VALUE_ON,
    ObjcMsg, Sel,
};

fn should_invoke_native_popup() -> bool {
    #[cfg(target_os = "macos")]
    {
        if crate::integration::CocoaPlatformIntegration::is_headless() {
            return false;
        }
        // `popUpMenuPositioningItem:atLocation:inView:` runs a modal tracking loop; skip it when
        // no `NSApplication` run loop is active (e.g. `appkit_main_thread` checks).
        let app_cls = Class::get("NSApplication").unwrap_or(Class::NIL);
        // SAFETY: `sharedApplication` takes no arguments and returns `NSApplication *` or nil;
        // `isRunning` takes no arguments and returns `BOOL`.
        unsafe {
            let app = ObjcMsg::send_class_0(app_cls, Sel::register("sharedApplication"));
            ObjcMsg::send_bool_return(app, Sel::register("isRunning"))
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        true
    }
}

/// Addresses of the live `NSMenu`s [`CocoaMenu::new`] created (removed in [`CocoaMenu::drop`]
/// before the `NSMenu` is released).
static NS_MENUS: LazyLock<Mutex<HashSet<usize>>> = LazyLock::new(Mutex::default);

/// The `NSMenu` of `menu` if it is a [`CocoaMenu`], nil otherwise. Any code can implement
/// `PlatformMenu`, so a `native_handle` is only sent messages (or handed to AppKit) once it is
/// known to be one of the live menus above; anything else is treated as no menu, which is
/// also what the default `native_handle` of 0 means. A non-zero handle that is not one is
/// logged.
pub(crate) fn ns_menu_of(menu: &dyn PlatformMenu) -> Id {
    let handle = menu.native_handle();
    let ns_menu = known_ns_menu(handle);
    if ns_menu.is_nil() && handle != 0 {
        eprintln!("ferrokit: menu handle {handle:#x} is not a CocoaMenu; it is ignored");
    }
    ns_menu
}

/// [`ns_menu_of`] for a `native_handle`, without logging.
pub(crate) fn known_ns_menu(handle: isize) -> Id {
    if handle == 0 {
        return Id::NIL;
    }
    let known = NS_MENUS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains(&(handle as usize));
    if known {
        Id(handle as *mut std::ffi::c_void)
    } else {
        Id::NIL
    }
}

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
        // a registered selector. The +1 from `alloc`/`init` is balanced in `Drop`.
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
        // a registered selector. The +1 from `alloc`/`init` is balanced in `Drop`.
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
        // SAFETY: `self.ns_item` is the item created in `new_*` and released only in `Drop`, so
        // it is live; `target.id()` is the live ActionTarget, which is stored in `self.target`
        // right below and cleared from the item in `Drop` before it is released, so the item's
        // weak target never dangles. `setTarget:` takes one object.
        unsafe { ObjcMsg::send_id(self.ns_item, Sel::register("setTarget:"), target.id()) };
        *self.target.borrow_mut() = Some(target);
    }

    #[inline]
    pub fn native_item(&self) -> Id {
        self.ns_item
    }

    pub fn ns_control_state(&self) -> isize {
        if *ferrokit_core::poison::lock_unpoisoned(&self.checked) {
            NS_CONTROL_STATE_VALUE_ON
        } else {
            NS_CONTROL_STATE_VALUE_OFF
        }
    }

    pub fn trigger(&self) {
        if *ferrokit_core::poison::lock_unpoisoned(&self.enabled) {
            if self.checkable {
                let mut chk = ferrokit_core::poison::lock_unpoisoned(&self.checked);
                *chk = !*chk;
                let state = if *chk {
                    NS_CONTROL_STATE_VALUE_ON
                } else {
                    NS_CONTROL_STATE_VALUE_OFF
                };
                // SAFETY: `self.ns_item` is live (released only in `Drop`); `setState:` takes one NSInteger.
                unsafe { ObjcMsg::send_int(self.ns_item, Sel::register("setState:"), state) };
            }
            self.activated.emit(&());
        }
    }

    /// Makes `menu` the item's submenu. A `menu` that is not a [`CocoaMenu`] has no `NSMenu`
    /// AppKit could show, so the item gets none.
    pub fn set_submenu(&self, menu: Box<dyn PlatformMenu>) {
        let sub_id = ns_menu_of(&*menu);
        // SAFETY: `self.ns_item` is live (released only in `Drop`); `sub_id` is nil or a live
        // NSMenu (`ns_menu_of`, kept alive by `menu` in `self.submenu` and retained by
        // `setSubmenu:`); `setSubmenu:` takes one NSMenu or nil.
        unsafe { ObjcMsg::send_id(self.ns_item, Sel::register("setSubmenu:"), sub_id) };
        *ferrokit_core::poison::lock_unpoisoned(&self.submenu) = Some(menu);
    }

    pub fn has_submenu(&self) -> bool {
        ferrokit_core::poison::lock_unpoisoned(&self.submenu).is_some()
    }
}

impl Drop for CocoaMenuItem {
    fn drop(&mut self) {
        // The item may outlive us inside its NSMenu; it must not keep a dangling target.
        if self.target.get_mut().is_some() {
            // SAFETY: `self.ns_item` is still live (our +1 is released below); `setTarget:` takes
            // one object, here nil, which clears the item's weak reference to `self.target`
            // before that ActionTarget is dropped with `self`.
            unsafe { ObjcMsg::send_id(self.ns_item, Sel::register("setTarget:"), Id::NIL) };
        }
        // Clear the submenu before `self.submenu` drops so `NSMenuItem` releases its retain first.
        if self
            .submenu
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
        {
            // SAFETY: `self.ns_item` is still live; `setSubmenu:` takes one NSMenu or nil.
            unsafe { ObjcMsg::send_id(self.ns_item, Sel::register("setSubmenu:"), Id::NIL) };
        }
        // Separator items come from `+[NSMenuItem separatorItem]` (autoreleased/shared), not
        // `alloc`/`init`, so only non-separator items own a +1 reference to balance.
        if !self.separator && !self.ns_item.is_nil() {
            // SAFETY: `self.ns_item` was created with `alloc`/`initWithTitle:...` (+1) in
            // `new_action` or `new_checkable`; this `release` balances that +1 once in `Drop`.
            unsafe { ObjcMsg::send_0(self.ns_item, Sel::register("release")) };
        }
    }
}

impl PlatformMenuItem for CocoaMenuItem {
    fn id(&self) -> u32 {
        self.id
    }

    fn text(&self) -> String {
        ferrokit_core::poison::lock_unpoisoned(&self.text).clone()
    }

    fn set_text(&mut self, text: &str) {
        *ferrokit_core::poison::lock_unpoisoned(&self.text) = text.to_string();
        // SAFETY: `self.ns_item` is live (released only in `Drop`); `setTitle:` takes one
        // NSString, which `send_str` builds from `text`.
        unsafe { ObjcMsg::send_str(self.ns_item, Sel::register("setTitle:"), text) };
    }

    fn is_separator(&self) -> bool {
        self.separator
    }

    fn is_checkable(&self) -> bool {
        self.checkable
    }

    fn is_checked(&self) -> bool {
        *ferrokit_core::poison::lock_unpoisoned(&self.checked)
    }

    fn set_checked(&mut self, checked: bool) {
        *ferrokit_core::poison::lock_unpoisoned(&self.checked) = checked;
        let state = if checked {
            NS_CONTROL_STATE_VALUE_ON
        } else {
            NS_CONTROL_STATE_VALUE_OFF
        };
        // SAFETY: `self.ns_item` is live (released only in `Drop`); `setState:` takes one NSInteger.
        unsafe { ObjcMsg::send_int(self.ns_item, Sel::register("setState:"), state) };
    }

    fn is_enabled(&self) -> bool {
        *ferrokit_core::poison::lock_unpoisoned(&self.enabled)
    }

    fn set_enabled(&mut self, enabled: bool) {
        *ferrokit_core::poison::lock_unpoisoned(&self.enabled) = enabled;
        // SAFETY: `self.ns_item` is live (released only in `Drop`); `setEnabled:` takes one BOOL.
        unsafe { ObjcMsg::send_bool(self.ns_item, Sel::register("setEnabled:"), enabled) };
    }

    fn activated(&self) -> &Signal<()> {
        &self.activated
    }
}

/// An `NSMenu` and its items. The `NSMenu` (+1 from `new`) is registered in `NS_MENUS` while
/// live and removed from `NS_MENUS` before being released in `Drop`.
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
        // receiver is the unconsumed `alloc` result above. The +1 is balanced in `Drop`.
        let ns_menu =
            unsafe { ObjcMsg::send_str(ns_menu_alloc, Sel::register("initWithTitle:"), "") };
        if !ns_menu.is_nil() {
            // Disable AppKit's automatic target/responder validation so manual `set_enabled(false)`
            // on items is respected when the menu opens (`ActionTarget` does not implement
            // `NSMenuItemValidation`).
            // SAFETY: `ns_menu` is the live +1 NSMenu just initialised; `setAutoenablesItems:`
            // takes one BOOL.
            unsafe { ObjcMsg::send_bool(ns_menu, Sel::register("setAutoenablesItems:"), false) };
            NS_MENUS
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(ns_menu.0 as usize);
        }

        Self {
            ns_menu,
            title: Mutex::new(String::new()),
            items: Mutex::new(Vec::new()),
            is_popped_up: Mutex::new(false),
            popup_pos: Mutex::new(None),
        }
    }

    pub fn title(&self) -> String {
        ferrokit_core::poison::lock_unpoisoned(&self.title).clone()
    }

    pub fn set_title(&self, title: &str) {
        *ferrokit_core::poison::lock_unpoisoned(&self.title) = title.to_string();
        // SAFETY: `self.ns_menu` is live (released only in `Drop`); `setTitle:` takes one
        // NSString, which `send_str` builds from `title`.
        unsafe { ObjcMsg::send_str(self.ns_menu, Sel::register("setTitle:"), title) };
    }

    pub fn items(&self) -> Vec<Arc<CocoaMenuItem>> {
        ferrokit_core::poison::lock_unpoisoned(&self.items).clone()
    }

    pub fn is_popped_up(&self) -> bool {
        *ferrokit_core::poison::lock_unpoisoned(&self.is_popped_up)
    }

    pub fn popup_pos(&self) -> Option<Point> {
        *ferrokit_core::poison::lock_unpoisoned(&self.popup_pos)
    }

    pub fn trigger_item(&self, id: u32) -> bool {
        // Release the lock before emitting: a slot may call back into this menu.
        let item = ferrokit_core::poison::lock_unpoisoned(&self.items)
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

impl Drop for CocoaMenu {
    fn drop(&mut self) {
        if !self.ns_menu.is_nil() {
            NS_MENUS
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&(self.ns_menu.0 as usize));
            // SAFETY: `self.ns_menu` was created with `alloc`/`initWithTitle:` (+1) in `new` and
            // removed from `NS_MENUS` just above; this `release` balances that +1 once in `Drop`.
            unsafe { ObjcMsg::send_0(self.ns_menu, Sel::register("release")) };
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
        // SAFETY: `self.ns_menu` is live (released only in `Drop`) and `item.native_item()` is the
        // live NSMenuItem just created (kept alive by `item` in `self.items` and by its own +1);
        // `addItem:` takes one NSMenuItem and retains it.
        unsafe { ObjcMsg::send_id(self.ns_menu, Sel::register("addItem:"), item.native_item()) };
        ferrokit_core::poison::lock_unpoisoned(&self.items).push(item.clone());
        item
    }

    fn add_checkable(&mut self, id: u32, text: &str, checked: bool) -> Arc<dyn PlatformMenuItem> {
        let item = Arc::new(CocoaMenuItem::new_checkable(id, text, checked));
        item.attach_target();
        // SAFETY: `self.ns_menu` is live (released only in `Drop`) and `item.native_item()` is the
        // live NSMenuItem just created (kept alive by `item` in `self.items` and by its own +1);
        // `addItem:` takes one NSMenuItem and retains it.
        unsafe { ObjcMsg::send_id(self.ns_menu, Sel::register("addItem:"), item.native_item()) };
        ferrokit_core::poison::lock_unpoisoned(&self.items).push(item.clone());
        item
    }

    fn add_separator(&mut self) {
        let item = Arc::new(CocoaMenuItem::new_separator());
        // SAFETY: `self.ns_menu` is live (released only in `Drop`) and `item.native_item()` is the
        // live separator NSMenuItem just obtained; `addItem:` takes one NSMenuItem.
        unsafe { ObjcMsg::send_id(self.ns_menu, Sel::register("addItem:"), item.native_item()) };
        ferrokit_core::poison::lock_unpoisoned(&self.items).push(item);
    }

    fn add_submenu(&mut self, text: &str, submenu: Box<dyn PlatformMenu>) {
        let item = CocoaMenuItem::new_action(
            (ferrokit_core::poison::lock_unpoisoned(&self.items).len() + 1000) as u32,
            text,
        );
        item.set_submenu(submenu);
        // A submenu parent has no action of its own: with an action but no target AppKit would
        // disable it.
        // SAFETY: `item.native_item()` is the live NSMenuItem created just above; `setAction:`
        // takes one selector, here nil, matching `send_action`.
        unsafe { ObjcMsg::send_action(item.native_item(), Sel::register("setAction:"), Sel::NIL) };
        let arc_item = Arc::new(item);
        // SAFETY: `self.ns_menu` is live (released only in `Drop`) and `arc_item.native_item()` is
        // the live NSMenuItem created above, kept alive by `arc_item`; `addItem:` takes one
        // NSMenuItem and retains it.
        unsafe {
            ObjcMsg::send_id(
                self.ns_menu,
                Sel::register("addItem:"),
                arc_item.native_item(),
            )
        };
        ferrokit_core::poison::lock_unpoisoned(&self.items).push(arc_item);
    }

    fn show_popup(&self, screen_pos: Point) {
        *ferrokit_core::poison::lock_unpoisoned(&self.is_popped_up) = true;
        *ferrokit_core::poison::lock_unpoisoned(&self.popup_pos) = Some(screen_pos);
        if !self.ns_menu.is_nil() && should_invoke_native_popup() {
            let cocoa_pos = crate::window::macos::qt_mac_flip_global_point(screen_pos);
            // SAFETY: `self.ns_menu` is live (released only in `Drop`); `popUpMenuPositioningItem:atLocation:inView:`
            // takes `(NSMenuItem * = nil, NSPoint, NSView * = nil)` in Cocoa screen coordinates.
            unsafe {
                ObjcMsg::send_popup_menu(
                    self.ns_menu,
                    Id::NIL,
                    CGPoint::new(cocoa_pos.x as f64, cocoa_pos.y as f64),
                    Id::NIL,
                );
            }
        }
    }

    fn dismiss(&self) {
        *ferrokit_core::poison::lock_unpoisoned(&self.is_popped_up) = false;
        *ferrokit_core::poison::lock_unpoisoned(&self.popup_pos) = None;
        if !self.ns_menu.is_nil() {
            // SAFETY: `self.ns_menu` is live (released only in `Drop`); `cancelTracking` takes no arguments.
            unsafe { ObjcMsg::send_cancel_tracking(self.ns_menu) };
        }
    }

    fn find_item(&self, id: u32) -> Option<Arc<dyn PlatformMenuItem>> {
        for item in ferrokit_core::poison::lock_unpoisoned(&self.items).iter() {
            if item.id == id {
                return Some(item.clone() as Arc<dyn PlatformMenuItem>);
            }
            if let Some(ref sub) = *ferrokit_core::poison::lock_unpoisoned(&item.submenu)
                && let Some(found) = sub.find_item(id)
            {
                return Some(found);
            }
        }
        None
    }

    fn native_handle(&self) -> isize {
        self.ns_menu.0 as isize
    }
}

#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    use super::*;
    use crate::objc_runtime::MockObjcRuntime;
    use crate::objc_runtime::appkit::MockValue;

    #[test]
    fn show_popup_and_dismiss_dispatch_appkit_messages() {
        let runtime = MockObjcRuntime::instance();
        let menu = CocoaMenu::new();
        let ns_menu = Id(menu.native_handle() as *mut std::ffi::c_void);
        runtime.clear_calls();

        let screen_pos = Point::new(120, 240);
        menu.show_popup(screen_pos);
        assert!(menu.is_popped_up());
        assert_eq!(menu.popup_pos(), Some(screen_pos));

        let expected_cocoa = crate::window::macos::qt_mac_flip_global_point(screen_pos);
        let calls = runtime.calls_for(ns_menu);
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].selector,
            "popUpMenuPositioningItem:atLocation:inView:"
        );
        assert_eq!(
            calls[0].args,
            vec![
                MockValue::Id(Id::NIL),
                MockValue::Point(CGPoint::new(
                    expected_cocoa.x as f64,
                    expected_cocoa.y as f64
                )),
                MockValue::Id(Id::NIL),
            ]
        );

        menu.dismiss();
        assert!(!menu.is_popped_up());
        assert_eq!(menu.popup_pos(), None);
        let calls = runtime.calls_for(ns_menu);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1].selector, "cancelTracking");
        assert!(calls[1].args.is_empty());
    }
}
