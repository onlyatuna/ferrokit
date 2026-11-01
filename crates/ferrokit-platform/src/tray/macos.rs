// SPDX-License-Identifier: LGPL-3.0-only
use ferrokit_core::object::ThreadContext;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use crate::menu::PlatformMenu;
use crate::objc_runtime::{ActionTarget, Class, Id, ObjcMsg, Sel, NS_SQUARE_STATUS_ITEM_LENGTH};
use crate::platform_tray::PlatformTrayIcon;
use crate::tray_icon::TrayActivation;

pub struct CocoaStatusItem {
    item_id: usize,
    status_item: Id,
    tooltip: String,
    is_visible: bool,
    icon_width: u32,
    icon_height: u32,
    icon_bytes: Vec<u8>,
    is_template: bool,
    menu: Option<Box<dyn PlatformMenu>>,
    last_message: Option<(String, String, crate::platform_tray::TrayMessageIcon, u32)>,
    /// What the button's single target runs on a click; see `ensure_button_target`.
    handlers: Rc<RefCell<ButtonHandlers>>,
    /// The object the button sends clicks to (`NSControl.target` is weak, so it lives here).
    click_target: Option<ActionTarget>,
    /// The observer of the menu's `NSMenuDidBeginTrackingNotification` (weak in AppKit, so it
    /// lives here).
    menu_observer: Option<ActionTarget>,
}

#[derive(Default)]
struct ButtonHandlers {
    on_click: Option<Arc<dyn Fn() + Send + Sync>>,
    activation: Option<Rc<dyn Fn(TrayActivation)>>,
    context_menu: Option<Rc<dyn Fn(ferrokit_gui::geometry::primitives::Point)>>,
}
// Deliberately neither `Send` nor `Sync`: wraps an `NSStatusItem` (`Id`), which AppKit only allows
// to be used from the main thread (`new` asserts it).

impl CocoaStatusItem {
    pub fn new(item_id: usize) -> Self {
        ThreadContext::assert_main_thread("CocoaStatusItem::new");

        let status_bar_class = Class::get("NSStatusBar").unwrap_or(Class::NIL);
        // SAFETY: `systemStatusBar` is a no-argument class method of `NSStatusBar` returning an
        // object (a nil class from `unwrap_or` makes the send a no-op returning nil). `new`
        // asserted the main thread, which AppKit requires.
        let status_bar =
            unsafe { ObjcMsg::send_class_0(status_bar_class, Sel::register("systemStatusBar")) };

        // Qt: statusItemWithLength:NSSquareStatusItemLength (qcocoasystemtrayicon.mm:37). Qt
        // switches to NSVariableStatusItemLength only for icons backed by a native NSImage
        // (QAppleIconEngine, :88-98); pixmap icons keep the square length.
        // SAFETY: `status_bar` is the shared `NSStatusBar` (or nil, a no-op);
        // `statusItemWithLength:` takes one CGFloat, which `send_length` passes. The status bar
        // owns the item; it is removed again in `Drop`.
        let status_item = unsafe {
            ObjcMsg::send_length(
                status_bar,
                Sel::register("statusItemWithLength:"),
                NS_SQUARE_STATUS_ITEM_LENGTH,
            )
        };

        Self {
            item_id,
            status_item,
            tooltip: String::new(),
            is_visible: false,
            icon_width: 0,
            icon_height: 0,
            icon_bytes: Vec::new(),
            is_template: true,
            menu: None,
            last_message: None,
            handlers: Rc::default(),
            click_target: None,
            menu_observer: None,
        }
    }

    pub fn item_id(&self) -> usize {
        self.item_id
    }

    #[inline]
    pub fn native_status_item(&self) -> Id {
        self.status_item
    }

    pub fn is_template(&self) -> bool {
        self.is_template
    }

    pub fn set_template(&mut self, is_template: bool) {
        self.is_template = is_template;
        // SAFETY: `self.status_item` is the item created in `new` and removed only in `Drop`
        // (nil is a no-op); `button` takes no arguments and returns an object.
        let button = unsafe { ObjcMsg::send_0(self.status_item, Sel::register("button")) };
        if !button.is_nil() {
            // SAFETY: `button` was checked non-nil and belongs to the live status item; `image`
            // takes no arguments and returns an object.
            let image = unsafe { ObjcMsg::send_0(button, Sel::register("image")) };
            if !image.is_nil() {
                // SAFETY: `image` was checked non-nil and is the button's live NSImage;
                // `setTemplate:` takes one BOOL.
                unsafe { ObjcMsg::send_bool(image, Sel::register("setTemplate:"), is_template) };
            }
        }
    }

    /// Sets a callback invoked when the status bar item is clicked. It shares the button's
    /// target with the activation handler ([`PlatformTrayIcon::set_activation_handler`]): both
    /// run for each click, and setting either leaves the other in place.
    pub fn set_on_click<F>(&mut self, callback: F)
    where
        F: Fn() + Send + Sync + 'static,
    {
        self.handlers.borrow_mut().on_click = Some(Arc::new(callback));
        self.ensure_button_target();
    }

    /// Triggers the click action handler.
    pub fn trigger_click(&self) {
        let on_click = self.handlers.borrow().on_click.clone();
        if let Some(cb) = on_click {
            cb();
        }
    }

    /// Points the button at one target that runs both the click callback and the activation
    /// handler as they are set at the time of the click. Returns whether the button has such a
    /// target (false: no button, or the target class could not be created).
    fn ensure_button_target(&mut self) -> bool {
        if self.click_target.is_some() {
            return true;
        }
        // SAFETY: `self.status_item` is the item created in `new` and removed only in `Drop`
        // (nil is a no-op); `button` takes no arguments and returns an object.
        let button = unsafe { ObjcMsg::send_0(self.status_item, Sel::register("button")) };
        if button.is_nil() {
            return false;
        }
        let handlers = Rc::clone(&self.handlers);
        let Ok(target) = ActionTarget::new(move || {
            // Copied out so a callback may set handlers without a borrow outstanding.
            let (on_click, activation, context_menu) = {
                let h = handlers.borrow();
                (
                    h.on_click.clone(),
                    h.activation.clone(),
                    h.context_menu.clone(),
                )
            };
            if let Some(cb) = on_click {
                cb();
            }
            let reason = activation_reason_for_event(current_event());
            if let Some(handler) = activation {
                handler(reason);
            }
            if reason == TrayActivation::Context {
                if let Some(handler) = context_menu {
                    let pos =
                        crate::cursor_cocoa::cursor_pos(&crate::cursor_cocoa::NativeCursorServices)
                            .unwrap_or_default();
                    handler(pos);
                }
            }
        }) else {
            return false;
        };
        // [button setTarget:target] & [button setAction:@selector(ferrokitPerformAction:)]: the
        // target runs the callbacks when AppKit delivers the click.
        // SAFETY: `button` was checked non-nil and belongs to the live status item; `target.id()`
        // is the live ActionTarget object, which `self.click_target` keeps alive (below) until
        // `Drop` clears the button's target first, so the button's weak reference never dangles.
        // `setTarget:` takes one object.
        unsafe { ObjcMsg::send_id(button, Sel::register("setTarget:"), target.id()) };
        // SAFETY: as above for `button`; `setAction:` takes one selector, matching `send_action`,
        // and `ActionTarget::action()` is the selector the target class implements.
        unsafe {
            ObjcMsg::send_action(button, Sel::register("setAction:"), ActionTarget::action())
        };
        self.click_target = Some(target);
        true
    }

    /// Whether the status item's menu is being observed for `NSMenuDidBeginTrackingNotification`
    /// (it is once both a menu and an activation handler are set).
    pub fn is_observing_menu_tracking(&self) -> bool {
        self.menu_observer.is_some()
    }

    /// `-[QStatusItemDelegate statusItemMenuBeganTracking:]`: a status item that has a menu never
    /// sends its button action (the menu opens instead), so Qt reports the activation when the
    /// menu starts tracking (`QCocoaSystemTrayIcon::updateMenu`). Does nothing until a menu
    /// and an activation handler are both set.
    fn observe_menu_tracking(&mut self) {
        self.remove_menu_observer();
        if self.handlers.borrow().activation.is_none() {
            return;
        }
        let Some(menu) = &self.menu else {
            return;
        };
        // Qt observes only a status item that has an NSMenu; a menu that is not a `CocoaMenu`
        // has none (and nil would observe every menu of the application). `set_menu` already
        // logged such a menu as ignored (`ns_menu_of`); this logs what it costs here.
        let handle = menu.native_handle();
        let menu_id = crate::menu::cocoa_menu::known_ns_menu(handle);
        if menu_id.is_nil() {
            if handle != 0 {
                eprintln!(
                    "ferrokit: menu handle {handle:#x} is not a CocoaMenu; opening it does not \
                     report a tray activation"
                );
            }
            return;
        }
        let handlers = Rc::clone(&self.handlers);
        let Ok(target) = ActionTarget::new(move || {
            let activation = handlers.borrow().activation.clone();
            if let Some(handler) = activation {
                handler(activation_reason_for_event(current_event()));
            }
        }) else {
            return;
        };
        let center_class = Class::get("NSNotificationCenter").unwrap_or(Class::NIL);
        // SAFETY: `center` is the shared notification centre (a nil class is a no-op);
        // `addObserver:selector:name:object:` takes (observer, selector, NSString name, object),
        // matching `send_add_observer_logged`. `target` is a live ActionTarget implementing the
        // selector and `menu_id` is the live NSMenu of `self.menu` (`known_ns_menu`); the observer
        // is removed in `remove_menu_observer` before `target` is dropped, so the centre never
        // holds a freed observer. (The centre does not retain observers: documented AppKit
        // behaviour [inferred].)
        unsafe {
            let center = ObjcMsg::send_class_0(center_class, Sel::register("defaultCenter"));
            ObjcMsg::send_add_observer_logged(
                center,
                Sel::register("addObserver:selector:name:object:"),
                target.id(),
                ActionTarget::action(),
                NS_MENU_DID_BEGIN_TRACKING,
                menu_id,
            );
        }
        self.menu_observer = Some(target);
    }

    fn remove_menu_observer(&mut self) {
        let Some(target) = self.menu_observer.take() else {
            return;
        };
        let center_class = Class::get("NSNotificationCenter").unwrap_or(Class::NIL);
        // SAFETY: `center` is the shared notification centre (a nil class is a no-op);
        // `removeObserver:` takes one object, here the live `target`, which is dropped only after
        // this block (`drop(target)` below), so the observer is removed before it can be freed.
        unsafe {
            let center = ObjcMsg::send_class_0(center_class, Sel::register("defaultCenter"));
            ObjcMsg::send_id_logged(center, Sel::register("removeObserver:"), target.id());
        }
        drop(target);
    }

    pub fn tooltip(&self) -> &str {
        &self.tooltip
    }

    pub fn icon_size(&self) -> (u32, u32) {
        (self.icon_width, self.icon_height)
    }

    pub fn is_visible(&self) -> bool {
        self.is_visible
    }

    pub fn last_message(&self) -> Option<(&str, &str, crate::platform_tray::TrayMessageIcon, u32)> {
        self.last_message
            .as_ref()
            .map(|(t, m, i, d)| (t.as_str(), m.as_str(), *i, *d))
    }
}

impl PlatformTrayIcon for CocoaStatusItem {
    fn set_icon(&mut self, icon: &ferrokit_gui::image::Icon) -> Result<(), &'static str> {
        // The largest image; AppKit scales it to the status bar.
        use ferrokit_gui::image::{IconMode, IconState};
        let largest = icon
            .available_sizes(IconMode::Normal, IconState::Off)
            .pop()
            .ok_or("null icon")?;
        let pixmap = &icon.pixmap(largest, IconMode::Normal, IconState::Off);
        self.icon_width = pixmap.physical_width();
        self.icon_height = pixmap.physical_height();
        self.icon_bytes = pixmap.data().to_vec();

        // Configure icon on status item button
        // SAFETY: `self.status_item` is the item created in `new` and removed only in `Drop`
        // (nil is a no-op); `button` takes no arguments and returns an object.
        let button = unsafe { ObjcMsg::send_0(self.status_item, Sel::register("button")) };
        if !button.is_nil() {
            let image_class = Class::get("NSImage").unwrap_or(Class::NIL);
            // SAFETY: `alloc` takes no arguments and returns an object, matching `send_class_0`
            // (a nil class from `unwrap_or` is a no-op).
            let image_alloc = unsafe { ObjcMsg::send_class_0(image_class, Sel::register("alloc")) };
            let cg_size =
                crate::objc_runtime::CGSize::new(self.icon_width as f64, self.icon_height as f64);
            // SAFETY: `initWithSize:` takes one NSSize, matching `send_size`; the receiver is the
            // unconsumed `alloc` result above (nil is a no-op, checked next).
            let image =
                unsafe { ObjcMsg::send_size(image_alloc, Sel::register("initWithSize:"), cg_size) };
            if !image.is_nil() {
                // Give the image its pixels: an NSBitmapImageRep holding the pixmap's
                // premultiplied RGBA bytes (without a representation the button draws nothing).
                let rep_class = Class::get("NSBitmapImageRep").unwrap_or(Class::NIL);
                // SAFETY: `alloc` takes no arguments and returns an object, matching
                // `send_class_0` (a nil class from `unwrap_or` is a no-op).
                let rep_alloc = unsafe { ObjcMsg::send_class_0(rep_class, Sel::register("alloc")) };
                // SAFETY: `send_bitmap_rep_init` consumes the `alloc` result and a slice of the
                // icon bytes together with the width and height they were taken from
                // (`self.icon_bytes = pixmap.data()` with `pixmap`'s own physical width/height),
                // so the slice covers the declared pixel area; the bytes are RGBA8 premultiplied,
                // as the helper documents.
                let rep = unsafe {
                    ObjcMsg::send_bitmap_rep_init(
                        rep_alloc,
                        self.icon_width as usize,
                        self.icon_height as usize,
                        &self.icon_bytes,
                    )
                };
                if rep.is_nil() {
                    // SAFETY: `image` is the non-nil +1 NSImage allocated above and no one else
                    // owns it (it was not attached to the button), so this is its one release.
                    unsafe { ObjcMsg::send_0(image, Sel::register("release")) };
                    return Err("Cannot create NSBitmapImageRep from icon pixmap");
                }
                // SAFETY: `image` and `rep` are live non-nil objects created above;
                // `addRepresentation:` takes one NSImageRep.
                unsafe { ObjcMsg::send_id(image, Sel::register("addRepresentation:"), rep) };
                // The image retains the representation.
                // SAFETY: `rep` is the +1 representation created above; the image retained it in
                // `addRepresentation:`, so this releases only our own reference, once.
                unsafe { ObjcMsg::send_0(rep, Sel::register("release")) };
                // SAFETY: `image` is live (see above); `setTemplate:` takes one BOOL.
                unsafe {
                    ObjcMsg::send_bool(image, Sel::register("setTemplate:"), self.is_template)
                };
                // The button retains the image (and releases the previous one); drop our +1 from
                // `alloc`.
                // SAFETY: `button` and `image` are live; `setImage:` takes one NSImage and the
                // button retains it.
                unsafe { ObjcMsg::send_id(button, Sel::register("setImage:"), image) };
                // SAFETY: `image` is the +1 from `alloc`/`init`; the button retained its own
                // reference in `setImage:`, so this releases ours once.
                unsafe { ObjcMsg::send_0(image, Sel::register("release")) };
            }
        }
        Ok(())
    }
    fn set_tooltip(&mut self, tooltip: &str) -> Result<(), &'static str> {
        self.tooltip = tooltip.to_string();

        // Qt: m_statusItem.button.toolTip (qcocoasystemtrayicon.mm:195-200); the button title
        // is never set, so no text appears in the menu bar.
        // SAFETY: `self.status_item` is the item created in `new` and removed only in `Drop`
        // (nil is a no-op); `button` takes no arguments and returns an object.
        let button = unsafe { ObjcMsg::send_0(self.status_item, Sel::register("button")) };
        if !button.is_nil() {
            // SAFETY: `button` was checked non-nil and belongs to the live status item;
            // `setToolTip:` takes one NSString, which `send_str` builds from `tooltip`.
            unsafe { ObjcMsg::send_str(button, Sel::register("setToolTip:"), tooltip) };
        }
        Ok(())
    }

    fn set_menu(&mut self, menu: Box<dyn PlatformMenu>) {
        // Qt removes the observer of the previous menu before replacing it
        // (QCocoaSystemTrayIcon::updateMenu).
        self.remove_menu_observer();
        // [status_item setMenu:ns_menu]; a menu that is not a `CocoaMenu` has no NSMenu, so the
        // status item gets none.
        let menu_id = crate::menu::cocoa_menu::ns_menu_of(&*menu);
        // SAFETY: `self.status_item` is live (created in `new`, removed in `Drop`); `menu_id`
        // is nil or a live NSMenu that `CocoaMenu::new` created and never releases
        // (`ns_menu_of`); `setMenu:` takes one NSMenu or nil.
        unsafe { ObjcMsg::send_id(self.status_item, Sel::register("setMenu:"), menu_id) };
        self.menu = Some(menu);
        self.observe_menu_tracking();
    }

    /// Mirrors `QCocoaSystemTrayIcon::init`/`updateMenu`/`emitActivated` (qcocoasystemtrayicon.mm):
    /// the status item button sends its action on left, right and other mouse down, and a status
    /// item that has a menu reports through the menu's `NSMenuDidBeginTrackingNotification`
    /// instead; either way the reason comes from `[NSApp currentEvent]`
    /// (`activation_reason_for_event`).
    ///
    /// **Not run on macOS.** It is type-checked for `aarch64-apple-darwin` and tested against
    /// the mock Objective-C runtime; no real status bar item has been clicked.
    fn set_activation_handler(&mut self, handler: Box<dyn Fn(TrayActivation)>) {
        // The handler is only kept once the button has a target to deliver clicks to.
        if !self.ensure_button_target() {
            return;
        }
        self.handlers.borrow_mut().activation = Some(Rc::from(handler));
        // SAFETY: `self.status_item` is live (created in `new`, removed in `Drop`); `button`
        // takes no arguments and returns an object (non-nil here: `ensure_button_target`
        // returned true).
        let button = unsafe { ObjcMsg::send_0(self.status_item, Sel::register("button")) };
        // [button sendActionOn:LeftMouseDown | RightMouseDown | OtherMouseDown]
        // SAFETY: `button` is the live button of the status item; `sendActionOn:` takes one
        // NSEventMask (an unsigned 64-bit integer), which `send_int_logged` passes in the same
        // register [inferred: same size and class as NSInteger].
        unsafe {
            ObjcMsg::send_int_logged(
                button,
                Sel::register("sendActionOn:"),
                NS_EVENT_MASK_ACTIVATION,
            )
        };
        self.observe_menu_tracking();
    }

    fn set_context_menu_handler(
        &mut self,
        handler: Box<dyn Fn(ferrokit_gui::geometry::primitives::Point)>,
    ) {
        if !self.ensure_button_target() {
            return;
        }
        self.handlers.borrow_mut().context_menu = Some(Rc::from(handler));
        self.observe_menu_tracking();
    }
    fn show(&mut self) -> Result<(), &'static str> {
        self.is_visible = true;
        // Set status item visible
        // SAFETY: `self.status_item` is live (created in `new`, removed in `Drop`);
        // `setVisible:` takes one BOOL.
        unsafe { ObjcMsg::send_bool(self.status_item, Sel::register("setVisible:"), true) };
        Ok(())
    }

    fn hide(&mut self) -> Result<(), &'static str> {
        self.is_visible = false;
        // Set status item hidden
        // SAFETY: `self.status_item` is live (created in `new`, removed in `Drop`);
        // `setVisible:` takes one BOOL.
        unsafe { ObjcMsg::send_bool(self.status_item, Sel::register("setVisible:"), false) };
        Ok(())
    }

    fn show_message(
        &mut self,
        title: &str,
        msg: &str,
        icon: crate::platform_tray::TrayMessageIcon,
        duration_ms: u32,
    ) -> Result<(), &'static str> {
        let dur = if duration_ms == 0 { 10000 } else { duration_ms };
        self.last_message = Some((title.to_string(), msg.to_string(), icon, dur));

        let center_class = Class::get("NSUserNotificationCenter").unwrap_or(Class::NIL);
        // SAFETY: `defaultUserNotificationCenter` is a no-argument class method returning an
        // object (a nil class from `unwrap_or` is a no-op).
        let center = unsafe {
            ObjcMsg::send_class_0(center_class, Sel::register("defaultUserNotificationCenter"))
        };
        let notif_class = Class::get("NSUserNotification").unwrap_or(Class::NIL);
        // SAFETY: `alloc` takes no arguments and returns an object, matching `send_class_0`
        // (a nil class from `unwrap_or` is a no-op).
        let notif_alloc = unsafe { ObjcMsg::send_class_0(notif_class, Sel::register("alloc")) };
        // SAFETY: `init` takes no arguments; the receiver is the unconsumed `alloc` result above.
        let notif = unsafe { ObjcMsg::send_0(notif_alloc, Sel::register("init")) };
        if !notif.is_nil() {
            // SAFETY: `notif` is the non-nil NSUserNotification initialised above;
            // `setTitle:` takes one NSString, which `send_str` builds from `title`.
            unsafe { ObjcMsg::send_str(notif, Sel::register("setTitle:"), title) };
            // SAFETY: as above; `setInformativeText:` takes one NSString.
            unsafe { ObjcMsg::send_str(notif, Sel::register("setInformativeText:"), msg) };
            // SAFETY: `center` is the shared centre (nil is a no-op) and `notif` the live
            // notification; `deliverNotification:` takes one NSUserNotification and the centre
            // retains it [inferred], so our `alloc` +1 is deliberately left to it (not released
            // here), which is a leak of one object per message, not unsoundness.
            unsafe { ObjcMsg::send_id(center, Sel::register("deliverNotification:"), notif) };
        }
        Ok(())
    }

    fn supports_messages(&self) -> bool {
        true
    }
}

impl Drop for CocoaStatusItem {
    fn drop(&mut self) {
        self.remove_menu_observer();
        if self.click_target.is_some() {
            // SAFETY: `self.status_item` is still live (removed from the bar only below);
            // `button` takes no arguments and returns an object (nil is a no-op for the next send).
            let button = unsafe { ObjcMsg::send_0(self.status_item, Sel::register("button")) };
            // SAFETY: `button` is the status item's button or nil; `setTarget:` takes one
            // object, here nil, which clears the button's weak reference to `click_target` before
            // that is dropped with `self`.
            unsafe { ObjcMsg::send_id(button, Sel::register("setTarget:"), Id::NIL) };
        }
        // [[NSStatusBar systemStatusBar] removeStatusItem:status_item]
        if !self.status_item.is_nil() {
            let status_bar_class = Class::get("NSStatusBar").unwrap_or(Class::NIL);
            // SAFETY: `systemStatusBar` is a no-argument class method of `NSStatusBar` returning
            // an object (a nil class from `unwrap_or` is a no-op).
            let status_bar = unsafe {
                ObjcMsg::send_class_0(status_bar_class, Sel::register("systemStatusBar"))
            };
            // SAFETY: `status_bar` is the shared `NSStatusBar` (or nil, a no-op) and
            // `self.status_item` the non-nil item this value created in `new`; `removeStatusItem:`
            // takes one NSStatusItem and is sent once, in `drop`.
            unsafe {
                ObjcMsg::send_id(
                    status_bar,
                    Sel::register("removeStatusItem:"),
                    self.status_item,
                )
            };
        }
    }
}

/// `NSEventTypeLeftMouseDown`, `NSEventTypeLeftMouseUp`, ... the mouse event types whose
/// `clickCount` and `buttonNumber` AppKit allows to be read (other types raise).
const NS_EVENT_LEFT_MOUSE_DOWN: isize = 1;
const NS_EVENT_LEFT_MOUSE_UP: isize = 2;
const NS_EVENT_RIGHT_MOUSE_DOWN: isize = 3;
const NS_EVENT_RIGHT_MOUSE_UP: isize = 4;
const NS_EVENT_OTHER_MOUSE_DOWN: isize = 25;
const NS_EVENT_OTHER_MOUSE_UP: isize = 26;
const NS_EVENT_MOUSE_MOVED: isize = 5;
const NS_EVENT_LEFT_MOUSE_DRAGGED: isize = 6;
const NS_EVENT_RIGHT_MOUSE_DRAGGED: isize = 7;
const NS_EVENT_OTHER_MOUSE_DRAGGED: isize = 27;

/// `NSEventMaskLeftMouseDown | NSEventMaskRightMouseDown | NSEventMaskOtherMouseDown`, the
/// events Qt asks the status item button to send its action on
/// (`qcocoasystemtrayicon.mm`, `QCocoaSystemTrayIcon::init`).
const NS_EVENT_MASK_ACTIVATION: isize = (1 << NS_EVENT_LEFT_MOUSE_DOWN)
    | (1 << NS_EVENT_RIGHT_MOUSE_DOWN)
    | (1 << NS_EVENT_OTHER_MOUSE_DOWN);

/// The notification `NSMenu` posts when it starts tracking; Qt observes it on a status item's
/// menu because a status item with a menu never sends its button action.
const NS_MENU_DID_BEGIN_TRACKING: &str = "NSMenuDidBeginTrackingNotification";

/// `QCocoaSystemTrayIcon::emitActivated`'s classification of the event that activated the
/// status item: click count 2 is a double click, otherwise `cocoaButton2QtButton(event)`
/// decides. It gives no button for a mouse-move class event (moved, dragged), `Qt::RightButton`
/// for a right mouse down or up whatever the button number, and else the button number: 1 is
/// `Qt::RightButton`, 2 `Qt::MiddleButton`, anything else is neither. Middle is
/// `MiddleClick`, right `Context`, everything else `Trigger`.
pub fn activation_reason(
    event_type: isize,
    click_count: isize,
    button_number: isize,
) -> TrayActivation {
    if click_count == 2 {
        return TrayActivation::DoubleClick;
    }
    if is_mouse_move_event(event_type) {
        return TrayActivation::Trigger;
    }
    if event_type == NS_EVENT_RIGHT_MOUSE_DOWN || event_type == NS_EVENT_RIGHT_MOUSE_UP {
        return TrayActivation::Context;
    }
    match button_number {
        2 => TrayActivation::MiddleClick,
        1 => TrayActivation::Context,
        _ => TrayActivation::Trigger,
    }
}

/// [`activation_reason`] for the `NSEvent` `event` (`[NSApp currentEvent]`). A nil event reads
/// as click count 0 and button 0, so it is a `Trigger`, exactly as Qt's messages to nil are.
/// Qt reads `clickCount` and `buttonNumber` without looking at the event type, which raises for
/// a non-mouse event; here such an event is `Unknown` instead.
///
/// Private, and safe only because every caller passes `current_event()`: `event` must be nil
/// or a live `NSEvent`.
fn activation_reason_for_event(event: Id) -> TrayActivation {
    if event.is_nil() {
        return TrayActivation::Trigger;
    }
    // SAFETY: `event` is non-nil (checked above) and is `[NSApp currentEvent]`, which every
    // caller in this module passes (the mock tests pass mock events, never dereferenced), live
    // for this call. `type` is defined for every NSEvent and returns NSEventType (NSUInteger),
    // read here as an `isize` of the same size; `clickCount` and `buttonNumber` are only read
    // after `is_mouse_event` established a mouse type (AppKit raises for the others), per the
    // NSEvent documentation [inferred].
    let event_type = unsafe { ObjcMsg::send_isize_return(event, Sel::register("type")) };
    if !is_mouse_event(event_type) {
        return TrayActivation::Unknown;
    }
    // SAFETY: `event` is a live, non-nil NSEvent of a mouse type (checked above), for which
    // `clickCount` (NSInteger, no arguments) is defined [inferred: NSEvent documentation].
    let click_count = unsafe { ObjcMsg::send_isize_return(event, Sel::register("clickCount")) };
    // SAFETY: as above; `buttonNumber` (NSInteger, no arguments) is defined for mouse types.
    let button_number = unsafe { ObjcMsg::send_isize_return(event, Sel::register("buttonNumber")) };
    activation_reason(event_type, click_count, button_number)
}

/// The mouse event types `cocoaEvent2QtMouseEvent` knows: down, up, moved and dragged.
fn is_mouse_event(event_type: isize) -> bool {
    matches!(
        event_type,
        NS_EVENT_LEFT_MOUSE_DOWN
            | NS_EVENT_LEFT_MOUSE_UP
            | NS_EVENT_RIGHT_MOUSE_DOWN
            | NS_EVENT_RIGHT_MOUSE_UP
            | NS_EVENT_OTHER_MOUSE_DOWN
            | NS_EVENT_OTHER_MOUSE_UP
    ) || is_mouse_move_event(event_type)
}

/// `QEvent::MouseMove` in `cocoaEvent2QtMouseEvent`: moved and the three dragged types.
fn is_mouse_move_event(event_type: isize) -> bool {
    matches!(
        event_type,
        NS_EVENT_MOUSE_MOVED
            | NS_EVENT_LEFT_MOUSE_DRAGGED
            | NS_EVENT_RIGHT_MOUSE_DRAGGED
            | NS_EVENT_OTHER_MOUSE_DRAGGED
    )
}

/// `[NSApp currentEvent]`.
fn current_event() -> Id {
    let app_class = Class::get("NSApplication").unwrap_or(Class::NIL);
    // SAFETY: `+sharedApplication` and `-currentEvent` take no arguments and return objects;
    // a nil class or nil `app` makes the sends no-ops returning nil, and the returned event is
    // only used by callers for the duration of the AppKit callback that is running.
    unsafe {
        let app = ObjcMsg::send_class_0(app_class, Sel::register("sharedApplication"));
        ObjcMsg::send_0(app, Sel::register("currentEvent"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use TrayActivation::*;

    #[test]
    fn left_click_triggers() {
        assert_eq!(activation_reason(NS_EVENT_LEFT_MOUSE_DOWN, 1, 0), Trigger);
        assert_eq!(activation_reason(NS_EVENT_LEFT_MOUSE_UP, 1, 0), Trigger);
    }

    #[test]
    fn right_and_middle_buttons() {
        assert_eq!(activation_reason(NS_EVENT_RIGHT_MOUSE_DOWN, 1, 1), Context);
        // A right mouse event wins even when the (Wacom) button number says 0.
        assert_eq!(activation_reason(NS_EVENT_RIGHT_MOUSE_DOWN, 1, 0), Context);
        assert_eq!(
            activation_reason(NS_EVENT_OTHER_MOUSE_DOWN, 1, 2),
            MiddleClick
        );
        // Extra buttons (3 and up) are plain triggers, as `cocoaButton2QtButton` maps them to
        // buttons Qt does not single out.
        assert_eq!(activation_reason(NS_EVENT_OTHER_MOUSE_DOWN, 1, 3), Trigger);
        // A right mouse event is Right whatever the button number says.
        assert_eq!(activation_reason(NS_EVENT_RIGHT_MOUSE_DOWN, 1, 2), Context);
        assert_eq!(activation_reason(NS_EVENT_RIGHT_MOUSE_UP, 1, 2), Context);
        // Mouse-move class events have no button.
        assert_eq!(activation_reason(NS_EVENT_MOUSE_MOVED, 1, 2), Trigger);
        assert_eq!(
            activation_reason(NS_EVENT_OTHER_MOUSE_DRAGGED, 1, 1),
            Trigger
        );
    }

    #[test]
    fn click_count_two_is_a_double_click_whatever_the_button() {
        assert_eq!(
            activation_reason(NS_EVENT_LEFT_MOUSE_DOWN, 2, 0),
            DoubleClick
        );
        assert_eq!(
            activation_reason(NS_EVENT_RIGHT_MOUSE_DOWN, 2, 1),
            DoubleClick
        );
        assert_eq!(
            activation_reason(NS_EVENT_OTHER_MOUSE_DOWN, 2, 2),
            DoubleClick
        );
        // Only exactly 2, as Qt tests `clickCount == 2`.
        assert_eq!(activation_reason(NS_EVENT_LEFT_MOUSE_DOWN, 3, 0), Trigger);
    }

    #[cfg(not(target_os = "macos"))]
    mod mock_runtime {
        use super::*;
        use crate::objc_runtime::appkit::MockValue;
        use crate::objc_runtime::MockObjcRuntime;

        fn event(kind: isize, clicks: isize, button: isize) -> Id {
            MockObjcRuntime::instance().object_with(
                "NSEvent",
                &[
                    ("type", MockValue::Int(kind as i64)),
                    ("clickCount", MockValue::Int(clicks as i64)),
                    ("buttonNumber", MockValue::Int(button as i64)),
                ],
            )
        }

        #[test]
        fn reads_the_mouse_event_properties() {
            assert_eq!(activation_reason_for_event(event(1, 1, 0)), Trigger);
            assert_eq!(activation_reason_for_event(event(3, 1, 1)), Context);
            assert_eq!(activation_reason_for_event(event(25, 1, 2)), MiddleClick);
            assert_eq!(activation_reason_for_event(event(1, 2, 0)), DoubleClick);
        }

        #[test]
        fn nil_event_is_a_trigger_and_non_mouse_event_unknown() {
            assert_eq!(activation_reason_for_event(Id::NIL), Trigger);
            // NSEventTypeKeyDown (10) has no click count or button number.
            assert_eq!(activation_reason_for_event(event(10, 0, 0)), Unknown);
        }

        #[test]
        fn moves_and_drags_are_triggers_unless_double_clicked() {
            // cocoaButton2QtButton: a mouse-move class event has no button.
            for kind in [5, 6, 7, 27] {
                assert_eq!(activation_reason_for_event(event(kind, 1, 2)), Trigger);
                assert_eq!(activation_reason_for_event(event(kind, 1, 1)), Trigger);
                assert_eq!(activation_reason_for_event(event(kind, 2, 0)), DoubleClick);
            }
        }
    }
}
