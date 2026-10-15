// SPDX-License-Identifier: LGPL-3.0-only
use super::dbus_connection::{DbusConnection, DbusMessage};
use crate::menu::PlatformMenu;
use crate::platform_tray::PlatformTrayIcon;
use ferrokit_core::event_loop::{EventDispatcher, SocketNotifier};
use ferrokit_gui::paint::Pixmap;
use std::sync::Mutex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbusImage {
    pub width: i32,
    pub height: i32,
    pub data: Vec<u8>,
}

impl DbusImage {
    pub fn from_pixmap(pixmap: &Pixmap) -> Self {
        let width = pixmap.physical_width() as i32;
        let height = pixmap.physical_height() as i32;
        let src_rgba = pixmap.data();

        let mut data = Vec::with_capacity((width * height * 4) as usize);
        for chunk in src_rgba.as_chunks::<4>().0 {
            let r = chunk[0];
            let g = chunk[1];
            let b = chunk[2];
            let a = chunk[3];
            data.push(a);
            data.push(r);
            data.push(g);
            data.push(b);
        }

        Self {
            width,
            height,
            data,
        }
    }
}

pub struct DbusStatusNotifierItem {
    id: String,
    category: String,
    title: String,
    tooltip: String,
    status: String,
    icon_pixmap: Vec<DbusImage>,
    menu_path: String,
    menu: Option<Box<dyn PlatformMenu>>,
    is_registered: bool,
    connection: Mutex<Option<DbusConnection>>,
    last_message: Option<(String, String, crate::platform_tray::TrayMessageIcon, u32)>,
}

unsafe impl Send for DbusStatusNotifierItem {}
unsafe impl Sync for DbusStatusNotifierItem {}

impl DbusStatusNotifierItem {
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            category: "ApplicationStatus".to_string(),
            title: title.into(),
            tooltip: String::new(),
            status: "Passive".to_string(),
            icon_pixmap: Vec::new(),
            menu_path: "/MenuBar".to_string(),
            menu: None,
            is_registered: false,
            connection: Mutex::new(None),
            last_message: None,
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn category(&self) -> &str {
        &self.category
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn tooltip(&self) -> &str {
        &self.tooltip
    }

    pub fn status(&self) -> &str {
        &self.status
    }

    pub fn icon_pixmap(&self) -> &[DbusImage] {
        &self.icon_pixmap
    }

    pub fn menu_path(&self) -> &str {
        &self.menu_path
    }

    pub fn is_registered(&self) -> bool {
        self.is_registered
    }

    pub fn is_connected(&self) -> bool {
        self.connection.lock().unwrap().is_some()
    }

    pub fn bind_event_dispatcher(
        &mut self,
        dispatcher: &mut dyn EventDispatcher,
    ) -> Option<std::sync::Arc<SocketNotifier>> {
        let mut conn_guard = self.connection.lock().unwrap();
        if conn_guard.is_none() {
            *conn_guard = DbusConnection::connect_session_bus().ok();
        }
        conn_guard
            .as_ref()
            .map(|c| c.bind_event_dispatcher(dispatcher))
    }

    pub fn activate(&mut self, _x: i32, _y: i32) {}

    pub fn context_menu(&mut self, _x: i32, _y: i32) {}

    pub fn last_message(&self) -> Option<(&str, &str, crate::platform_tray::TrayMessageIcon, u32)> {
        self.last_message
            .as_ref()
            .map(|(t, m, i, d)| (t.as_str(), m.as_str(), *i, *d))
    }
    /// Handles NameOwnerChanged D-Bus broadcast to automatically re-register
    /// if the system status notifier host (KDE Plasma, Waybar, etc.) restarts.
    pub fn handle_name_owner_changed(&mut self, name: &str, _old_owner: &str, new_owner: &str) {
        if name == "org.kde.StatusNotifierWatcher" && !new_owner.is_empty() {
            let mut guard = self.connection.lock().unwrap();
            if let Some(conn) = guard.as_mut() {
                let service_name = format!("org.kde.StatusNotifierItem-{}-1", self.id);
                let _ = conn.request_name(&service_name);
                let _ = conn.register_status_notifier_item("/StatusNotifierItem");

                let serial = conn.next_serial();
                let mut sig = DbusMessage::signal(
                    "/StatusNotifierItem",
                    "org.kde.StatusNotifierItem",
                    "NewStatus",
                    serial,
                );
                sig.append_string(&self.status);
                let _ = conn.send_message(sig);
            }
        }
    }
}

impl PlatformTrayIcon for DbusStatusNotifierItem {
    fn set_icon(&mut self, pixmap: &Pixmap) -> Result<(), &'static str> {
        let dbus_img = DbusImage::from_pixmap(pixmap);
        self.icon_pixmap = vec![dbus_img];

        // Emit NewIcon signal on Session Bus
        let mut guard = self.connection.lock().unwrap();
        if let Some(conn) = guard.as_mut() {
            let serial = conn.next_serial();
            let sig = DbusMessage::signal(
                "/StatusNotifierItem",
                "org.kde.StatusNotifierItem",
                "NewIcon",
                serial,
            );
            let _ = conn.send_message(sig);
        }
        Ok(())
    }

    fn set_tooltip(&mut self, tooltip: &str) -> Result<(), &'static str> {
        self.tooltip = tooltip.to_string();

        // Emit NewTitle signal on Session Bus
        let mut guard = self.connection.lock().unwrap();
        if let Some(conn) = guard.as_mut() {
            let serial = conn.next_serial();
            let mut sig = DbusMessage::signal(
                "/StatusNotifierItem",
                "org.kde.StatusNotifierItem",
                "NewTitle",
                serial,
            );
            sig.append_string(tooltip);
            let _ = conn.send_message(sig);
        }
        Ok(())
    }

    fn set_menu(&mut self, menu: Box<dyn PlatformMenu>) {
        self.menu = Some(menu);
    }

    fn show(&mut self) -> Result<(), &'static str> {
        self.status = "Active".to_string();

        // Register with StatusNotifierWatcher on Session Bus
        let mut guard = self.connection.lock().unwrap();
        if guard.is_none() {
            *guard = DbusConnection::connect_session_bus().ok();
        }

        if let Some(conn) = guard.as_mut() {
            let service_name = format!("org.kde.StatusNotifierItem-{}-1", self.id);
            let _ = conn.request_name(&service_name);
            let _ = conn.register_status_notifier_item("/StatusNotifierItem");

            // Emit NewStatus("Active") signal
            let serial = conn.next_serial();
            let mut sig = DbusMessage::signal(
                "/StatusNotifierItem",
                "org.kde.StatusNotifierItem",
                "NewStatus",
                serial,
            );
            sig.append_string("Active");
            let _ = conn.send_message(sig);

            // Subscribe to NameOwnerChanged signals to detect panel restarts
            let _ = conn.add_match("type='signal',interface='org.freedesktop.DBus',member='NameOwnerChanged',arg0='org.kde.StatusNotifierWatcher'");
        }
        self.is_registered = true;
        Ok(())
    }

    fn hide(&mut self) -> Result<(), &'static str> {
        self.status = "Passive".to_string();

        let mut guard = self.connection.lock().unwrap();
        if let Some(conn) = guard.as_mut() {
            let serial = conn.next_serial();
            let mut sig = DbusMessage::signal(
                "/StatusNotifierItem",
                "org.kde.StatusNotifierItem",
                "NewStatus",
                serial,
            );
            sig.append_string("Passive");
            let _ = conn.send_message(sig);
        }
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

        let app_icon = match icon {
            crate::platform_tray::TrayMessageIcon::Information => "dialog-information",
            crate::platform_tray::TrayMessageIcon::Warning => "dialog-warning",
            crate::platform_tray::TrayMessageIcon::Critical => "dialog-error",
            crate::platform_tray::TrayMessageIcon::NoIcon => "",
        };

        let mut guard = self.connection.lock().unwrap();
        if guard.is_none() {
            *guard = DbusConnection::connect_session_bus().ok();
        }

        if let Some(conn) = guard.as_mut() {
            let serial = conn.next_serial();
            let mut notify_call = DbusMessage::method_call(
                "org.freedesktop.Notifications",
                "/org/freedesktop/Notifications",
                "org.freedesktop.Notifications",
                "Notify",
                serial,
            );
            notify_call.append_string(&self.id);
            notify_call.append_u32(0);
            notify_call.append_string(app_icon);
            notify_call.append_string(title);
            notify_call.append_string(msg);
            notify_call.append_i32(dur as i32);
            let _ = conn.send_message(notify_call);
        }
        Ok(())
    }

    fn supports_messages(&self) -> bool {
        true
    }
}
