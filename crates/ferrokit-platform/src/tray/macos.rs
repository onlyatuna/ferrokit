// SPDX-License-Identifier: LGPL-3.0-only
use ferrokit_core::object::ThreadContext;
use ferrokit_gui::paint::Pixmap;

use crate::menu::PlatformMenu;
use crate::objc_runtime::{Class, Id, ObjcMsg, Sel, NS_SQUARE_STATUS_ITEM_LENGTH};
use crate::platform_tray::PlatformTrayIcon;

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
    on_click: Option<Box<dyn Fn() + Send + Sync>>,
}

unsafe impl Send for CocoaStatusItem {}
unsafe impl Sync for CocoaStatusItem {}

impl CocoaStatusItem {
    pub fn new(item_id: usize) -> Self {
        ThreadContext::assert_main_thread("CocoaStatusItem::new");

        let status_bar_class = Class::get("NSStatusBar").unwrap_or(Class::NIL);
        let status_bar = ObjcMsg::send_class_0(status_bar_class, Sel::register("systemStatusBar"));

        // Qt: statusItemWithLength:NSSquareStatusItemLength (qcocoasystemtrayicon.mm:37). Qt
        // switches to NSVariableStatusItemLength only for icons backed by a native NSImage
        // (QAppleIconEngine, :88-98); pixmap icons keep the square length.
        let status_item = ObjcMsg::send_length(
            status_bar,
            Sel::register("statusItemWithLength:"),
            NS_SQUARE_STATUS_ITEM_LENGTH,
        );

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
        let button = ObjcMsg::send_0(self.status_item, Sel::register("button"));
        if !button.is_nil() {
            let image = ObjcMsg::send_0(button, Sel::register("image"));
            if !image.is_nil() {
                ObjcMsg::send_bool(image, Sel::register("setTemplate:"), is_template);
            }
        }
    }

    /// Sets a callback invoked when the status bar item is clicked.
    pub fn set_on_click<F>(&mut self, callback: F)
    where
        F: Fn() + Send + Sync + 'static,
    {
        self.on_click = Some(Box::new(callback));
        let button = ObjcMsg::send_0(self.status_item, Sel::register("button"));
        if !button.is_nil() {
            // In AppKit: [button setTarget:self] & [button setAction:@selector(onStatusItemClick:)]
            ObjcMsg::send_id(button, Sel::register("setTarget:"), self.status_item);
            ObjcMsg::send_id(
                button,
                Sel::register("setAction:"),
                Id(Sel::register("onStatusItemClick:").0 as *mut std::ffi::c_void),
            );
        }
    }

    /// Triggers the click action handler.
    pub fn trigger_click(&self) {
        if let Some(cb) = &self.on_click {
            cb();
        }
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
        let button = ObjcMsg::send_0(self.status_item, Sel::register("button"));
        if !button.is_nil() {
            let image_class = Class::get("NSImage").unwrap_or(Class::NIL);
            let image_alloc = ObjcMsg::send_class_0(image_class, Sel::register("alloc"));
            let cg_size = crate::objc_runtime::CGSize::new(
                self.icon_width as f64,
                self.icon_height as f64,
            );
            let image = ObjcMsg::send_size(image_alloc, Sel::register("initWithSize:"), cg_size);
            if !image.is_nil() {
                ObjcMsg::send_bool(image, Sel::register("setTemplate:"), self.is_template);
                ObjcMsg::send_id(button, Sel::register("setImage:"), image);
            }
        }
        Ok(())
    }
    fn set_tooltip(&mut self, tooltip: &str) -> Result<(), &'static str> {
        self.tooltip = tooltip.to_string();

        // Qt: m_statusItem.button.toolTip (qcocoasystemtrayicon.mm:195-200); the button title
        // is never set, so no text appears in the menu bar.
        let button = ObjcMsg::send_0(self.status_item, Sel::register("button"));
        if !button.is_nil() {
            ObjcMsg::send_str(button, Sel::register("setToolTip:"), tooltip);
        }
        Ok(())
    }

    fn set_menu(&mut self, menu: Box<dyn PlatformMenu>) {
        // [status_item setMenu:ns_menu]
        let menu_id = Id(menu.native_handle() as *mut std::ffi::c_void);
        ObjcMsg::send_id(self.status_item, Sel::register("setMenu:"), menu_id);
        self.menu = Some(menu);
    }

    fn show(&mut self) -> Result<(), &'static str> {
        self.is_visible = true;
        // Set status item visible
        ObjcMsg::send_bool(self.status_item, Sel::register("setVisible:"), true);
        Ok(())
    }

    fn hide(&mut self) -> Result<(), &'static str> {
        self.is_visible = false;
        // Set status item hidden
        ObjcMsg::send_bool(self.status_item, Sel::register("setVisible:"), false);
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
        let center =
            ObjcMsg::send_class_0(center_class, Sel::register("defaultUserNotificationCenter"));
        let notif_class = Class::get("NSUserNotification").unwrap_or(Class::NIL);
        let notif_alloc = ObjcMsg::send_class_0(notif_class, Sel::register("alloc"));
        let notif = ObjcMsg::send_0(notif_alloc, Sel::register("init"));
        if !notif.is_nil() {
            ObjcMsg::send_str(notif, Sel::register("setTitle:"), title);
            ObjcMsg::send_str(notif, Sel::register("setInformativeText:"), msg);
            ObjcMsg::send_id(center, Sel::register("deliverNotification:"), notif);
        }
        Ok(())
    }

    fn supports_messages(&self) -> bool {
        true
    }
}

impl Drop for CocoaStatusItem {
    fn drop(&mut self) {
        // [[NSStatusBar systemStatusBar] removeStatusItem:status_item]
        if !self.status_item.is_nil() {
            let status_bar_class = Class::get("NSStatusBar").unwrap_or(Class::NIL);
            let status_bar =
                ObjcMsg::send_class_0(status_bar_class, Sel::register("systemStatusBar"));
            ObjcMsg::send_id(
                status_bar,
                Sel::register("removeStatusItem:"),
                self.status_item,
            );
        }
    }
}
