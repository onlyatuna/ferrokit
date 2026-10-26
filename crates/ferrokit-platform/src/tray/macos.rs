// SPDX-License-Identifier: LGPL-3.0-only
use ferrokit_core::object::ThreadContext;
use ferrokit_gui::paint::Pixmap;

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
    on_click: Option<Arc<dyn Fn() + Send + Sync>>,
    /// The object the button sends clicks to (`NSControl.target` is weak, so it lives here).
    click_target: Option<ActionTarget>,
    activation_handler: Option<Rc<dyn Fn(TrayActivation)>>,
    /// The observer of the menu's `NSMenuDidBeginTrackingNotification` (weak in AppKit, so it
    /// lives here).
    menu_observer: Option<ActionTarget>,
}

// Deliberately neither `Send` nor `Sync`: wraps an `NSStatusItem` (`Id`), which AppKit only allows
// to be used from the main thread (`new` asserts it).

impl CocoaStatusItem {
    pub fn new(item_id: usize) -> Self {
        ThreadContext::assert_main_thread("CocoaStatusItem::new");

        let status_bar_class = Class::get("NSStatusBar").unwrap_or(Class::NIL);
        let status_bar =
            unsafe { ObjcMsg::send_class_0(status_bar_class, Sel::register("systemStatusBar")) };

        // Qt: statusItemWithLength:NSSquareStatusItemLength (qcocoasystemtrayicon.mm:37). Qt
        // switches to NSVariableStatusItemLength only for icons backed by a native NSImage
        // (QAppleIconEngine, :88-98); pixmap icons keep the square length.
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
            on_click: None,
            click_target: None,
            activation_handler: None,
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
        let button = unsafe { ObjcMsg::send_0(self.status_item, Sel::register("button")) };
        if !button.is_nil() {
            let image = unsafe { ObjcMsg::send_0(button, Sel::register("image")) };
            if !image.is_nil() {
                unsafe { ObjcMsg::send_bool(image, Sel::register("setTemplate:"), is_template) };
            }
        }
    }

    /// Sets a callback invoked when the status bar item is clicked.
    pub fn set_on_click<F>(&mut self, callback: F)
    where
        F: Fn() + Send + Sync + 'static,
    {
        let callback: Arc<dyn Fn() + Send + Sync> = Arc::new(callback);
        self.on_click = Some(Arc::clone(&callback));
        let button = unsafe { ObjcMsg::send_0(self.status_item, Sel::register("button")) };
        if button.is_nil() {
            return;
        }
        let Ok(target) = ActionTarget::new(move || callback()) else {
            return;
        };
        // [button setTarget:target] & [button setAction:@selector(ferrokitPerformAction:)]: the
        // target runs the callback when AppKit delivers the click.
        unsafe { ObjcMsg::send_id(button, Sel::register("setTarget:"), target.id()) };
        unsafe {
            ObjcMsg::send_action(button, Sel::register("setAction:"), ActionTarget::action())
        };
        // A previous target is released only now that the button points at the new one.
        self.click_target = Some(target);
    }

    /// Triggers the click action handler.
    pub fn trigger_click(&self) {
        if let Some(cb) = &self.on_click {
            cb();
        }
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
        let (Some(handler), Some(menu)) = (&self.activation_handler, &self.menu) else {
            return;
        };
        let menu_id = Id(menu.native_handle() as *mut std::ffi::c_void);
        let handler = Rc::clone(handler);
        let Ok(target) =
            ActionTarget::new(move || handler(activation_reason_for_event(current_event())))
        else {
            return;
        };
        let center_class = Class::get("NSNotificationCenter").unwrap_or(Class::NIL);
        // SAFETY: `+defaultCenter` returns the shared centre; the observer is removed in
        // `remove_menu_observer` before `target` is released.
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
        // SAFETY: `[center removeObserver:target]` before the target is released by drop.
        unsafe {
            let center = ObjcMsg::send_class_0(center_class, Sel::register("defaultCenter"));
            ObjcMsg::send_id(center, Sel::register("removeObserver:"), target.id());
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
    fn set_icon(&mut self, pixmap: &Pixmap) -> Result<(), &'static str> {
        self.icon_width = pixmap.physical_width();
        self.icon_height = pixmap.physical_height();
        self.icon_bytes = pixmap.data().to_vec();

        // Configure icon on status item button
        let button = unsafe { ObjcMsg::send_0(self.status_item, Sel::register("button")) };
        if !button.is_nil() {
            let image_class = Class::get("NSImage").unwrap_or(Class::NIL);
            let image_alloc = unsafe { ObjcMsg::send_class_0(image_class, Sel::register("alloc")) };
            let cg_size =
                crate::objc_runtime::CGSize::new(self.icon_width as f64, self.icon_height as f64);
            let image =
                unsafe { ObjcMsg::send_size(image_alloc, Sel::register("initWithSize:"), cg_size) };
            if !image.is_nil() {
                // Give the image its pixels: an NSBitmapImageRep holding the pixmap's
                // premultiplied RGBA bytes (without a representation the button draws nothing).
                let rep_class = Class::get("NSBitmapImageRep").unwrap_or(Class::NIL);
                let rep_alloc =
                    unsafe { ObjcMsg::send_class_0(rep_class, Sel::register("alloc")) };
                let rep = unsafe {
                    ObjcMsg::send_bitmap_rep_init(
                        rep_alloc,
                        self.icon_width as usize,
                        self.icon_height as usize,
                        &self.icon_bytes,
                    )
                };
                if rep.is_nil() {
                    unsafe { ObjcMsg::send_0(image, Sel::register("release")) };
                    return Err("Cannot create NSBitmapImageRep from icon pixmap");
                }
                unsafe { ObjcMsg::send_id(image, Sel::register("addRepresentation:"), rep) };
                // The image retains the representation.
                unsafe { ObjcMsg::send_0(rep, Sel::register("release")) };
                unsafe {
                    ObjcMsg::send_bool(image, Sel::register("setTemplate:"), self.is_template)
                };
                // The button retains the image (and releases the previous one); drop our +1 from
                // `alloc`.
                unsafe { ObjcMsg::send_id(button, Sel::register("setImage:"), image) };
                unsafe { ObjcMsg::send_0(image, Sel::register("release")) };
            }
        }
        Ok(())
    }
    fn set_tooltip(&mut self, tooltip: &str) -> Result<(), &'static str> {
        self.tooltip = tooltip.to_string();

        // Qt: m_statusItem.button.toolTip (qcocoasystemtrayicon.mm:195-200); the button title
        // is never set, so no text appears in the menu bar.
        let button = unsafe { ObjcMsg::send_0(self.status_item, Sel::register("button")) };
        if !button.is_nil() {
            unsafe { ObjcMsg::send_str(button, Sel::register("setToolTip:"), tooltip) };
        }
        Ok(())
    }

    fn set_menu(&mut self, menu: Box<dyn PlatformMenu>) {
        // Qt removes the observer of the previous menu before replacing it
        // (QCocoaSystemTrayIcon::updateMenu).
        self.remove_menu_observer();
        // [status_item setMenu:ns_menu]
        let menu_id = Id(menu.native_handle() as *mut std::ffi::c_void);
        unsafe { ObjcMsg::send_id(self.status_item, Sel::register("setMenu:"), menu_id) };
        self.menu = Some(menu);
        self.observe_menu_tracking();
    }

    /// Mirrors `QCocoaSystemTrayIcon::init`/`updateMenu`/`emitActivated` (qcocoasystemtrayicon.mm):
    /// the status item button sends its action on left, right and other mouse down, and a status
    /// item that has a menu reports through the menu's `NSMenuDidBeginTrackingNotification`
    /// instead; either way the reason comes from `[NSApp currentEvent]`
    /// ([`activation_reason_for_event`]).
    ///
    /// **Not run on macOS.** It is type-checked for `aarch64-apple-darwin` and tested against
    /// the mock Objective-C runtime; no real status bar item has been clicked.
    fn set_activation_handler(&mut self, handler: Box<dyn Fn(TrayActivation)>) {
        let handler: Rc<dyn Fn(TrayActivation)> = Rc::from(handler);
        self.activation_handler = Some(Rc::clone(&handler));
        let button = unsafe { ObjcMsg::send_0(self.status_item, Sel::register("button")) };
        if !button.is_nil() {
            let click_handler = Rc::clone(&handler);
            if let Ok(target) =
                ActionTarget::new(move || click_handler(activation_reason_for_event(current_event())))
            {
                // button.target = delegate; button.action = @selector(statusItemClicked);
                // [button sendActionOn:LeftMouseDown | RightMouseDown | OtherMouseDown]
                unsafe { ObjcMsg::send_id(button, Sel::register("setTarget:"), target.id()) };
                unsafe {
                    ObjcMsg::send_action(
                        button,
                        Sel::register("setAction:"),
                        ActionTarget::action(),
                    )
                };
                unsafe {
                    ObjcMsg::send_int(
                        button,
                        Sel::register("sendActionOn:"),
                        NS_EVENT_MASK_ACTIVATION,
                    )
                };
                self.click_target = Some(target);
            }
        }
        self.observe_menu_tracking();
    }

    fn show(&mut self) -> Result<(), &'static str> {
        self.is_visible = true;
        // Set status item visible
        unsafe { ObjcMsg::send_bool(self.status_item, Sel::register("setVisible:"), true) };
        Ok(())
    }

    fn hide(&mut self) -> Result<(), &'static str> {
        self.is_visible = false;
        // Set status item hidden
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
        let center = unsafe {
            ObjcMsg::send_class_0(center_class, Sel::register("defaultUserNotificationCenter"))
        };
        let notif_class = Class::get("NSUserNotification").unwrap_or(Class::NIL);
        let notif_alloc = unsafe { ObjcMsg::send_class_0(notif_class, Sel::register("alloc")) };
        let notif = unsafe { ObjcMsg::send_0(notif_alloc, Sel::register("init")) };
        if !notif.is_nil() {
            unsafe { ObjcMsg::send_str(notif, Sel::register("setTitle:"), title) };
            unsafe { ObjcMsg::send_str(notif, Sel::register("setInformativeText:"), msg) };
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
            let button = unsafe { ObjcMsg::send_0(self.status_item, Sel::register("button")) };
            unsafe { ObjcMsg::send_id(button, Sel::register("setTarget:"), Id::NIL) };
        }
        // [[NSStatusBar systemStatusBar] removeStatusItem:status_item]
        if !self.status_item.is_nil() {
            let status_bar_class = Class::get("NSStatusBar").unwrap_or(Class::NIL);
            let status_bar = unsafe {
                ObjcMsg::send_class_0(status_bar_class, Sel::register("systemStatusBar"))
            };
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

/// `NSEventMaskLeftMouseDown | NSEventMaskRightMouseDown | NSEventMaskOtherMouseDown`, the
/// events Qt asks the status item button to send its action on
/// (`qcocoasystemtrayicon.mm`, `QCocoaSystemTrayIcon::init`).
const NS_EVENT_MASK_ACTIVATION: isize =
    (1 << NS_EVENT_LEFT_MOUSE_DOWN) | (1 << NS_EVENT_RIGHT_MOUSE_DOWN) | (1 << NS_EVENT_OTHER_MOUSE_DOWN);

/// The notification `NSMenu` posts when it starts tracking; Qt observes it on a status item's
/// menu because a status item with a menu never sends its button action.
const NS_MENU_DID_BEGIN_TRACKING: &str = "NSMenuDidBeginTrackingNotification";

/// `QCocoaSystemTrayIcon::emitActivated`'s classification of the event that activated the
/// status item: click count 2 is a double click, otherwise `cocoaButton2QtButton(event)`
/// decides: a right mouse event or button number 1 is `Qt::RightButton`, button number 2 is
/// `Qt::MiddleButton`, everything else `Trigger`.
pub fn activation_reason(event_type: isize, click_count: isize, button_number: isize) -> TrayActivation {
    if click_count == 2 {
        TrayActivation::DoubleClick
    } else if button_number == 2 {
        TrayActivation::MiddleClick
    } else if event_type == NS_EVENT_RIGHT_MOUSE_DOWN
        || event_type == NS_EVENT_RIGHT_MOUSE_UP
        || button_number == 1
    {
        TrayActivation::Context
    } else {
        TrayActivation::Trigger
    }
}

/// [`activation_reason`] for the `NSEvent` `event` (`[NSApp currentEvent]`). A nil event reads
/// as click count 0 and button 0, so it is a `Trigger`, exactly as Qt's messages to nil are.
/// Qt reads `clickCount` and `buttonNumber` without looking at the event type, which raises for
/// a non-mouse event; here such an event is `Unknown` instead.
pub fn activation_reason_for_event(event: Id) -> TrayActivation {
    if event.is_nil() {
        return TrayActivation::Trigger;
    }
    // SAFETY: `event` is a live NSEvent; `type` is defined for every NSEvent, and
    // `clickCount`/`buttonNumber` only for the mouse types checked before reading them.
    let event_type = unsafe { ObjcMsg::send_isize_return(event, Sel::register("type")) };
    let is_mouse = matches!(
        event_type,
        NS_EVENT_LEFT_MOUSE_DOWN
            | NS_EVENT_LEFT_MOUSE_UP
            | NS_EVENT_RIGHT_MOUSE_DOWN
            | NS_EVENT_RIGHT_MOUSE_UP
            | NS_EVENT_OTHER_MOUSE_DOWN
            | NS_EVENT_OTHER_MOUSE_UP
    );
    if !is_mouse {
        return TrayActivation::Unknown;
    }
    let click_count = unsafe { ObjcMsg::send_isize_return(event, Sel::register("clickCount")) };
    let button_number = unsafe { ObjcMsg::send_isize_return(event, Sel::register("buttonNumber")) };
    activation_reason(event_type, click_count, button_number)
}

/// `[NSApp currentEvent]`.
fn current_event() -> Id {
    let app_class = Class::get("NSApplication").unwrap_or(Class::NIL);
    // SAFETY: `+sharedApplication` and `-currentEvent` take no arguments and return objects.
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
        assert_eq!(activation_reason(NS_EVENT_OTHER_MOUSE_DOWN, 1, 2), MiddleClick);
        // Extra buttons (3 and up) are plain triggers, as `cocoaButton2QtButton` maps them to
        // buttons Qt does not single out.
        assert_eq!(activation_reason(NS_EVENT_OTHER_MOUSE_DOWN, 1, 3), Trigger);
    }

    #[test]
    fn click_count_two_is_a_double_click_whatever_the_button() {
        assert_eq!(activation_reason(NS_EVENT_LEFT_MOUSE_DOWN, 2, 0), DoubleClick);
        assert_eq!(activation_reason(NS_EVENT_RIGHT_MOUSE_DOWN, 2, 1), DoubleClick);
        assert_eq!(activation_reason(NS_EVENT_OTHER_MOUSE_DOWN, 2, 2), DoubleClick);
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
    }
}
