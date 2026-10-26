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

    /// Uses `connection` instead of connecting to the session bus on first use.
    pub fn set_connection(&mut self, connection: DbusConnection) {
        *self.connection.lock().unwrap() = Some(connection);
    }

    /// Watches the bus socket; `None` when there is no bus connection or it has no descriptor.
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
            .and_then(|c| c.bind_event_dispatcher(dispatcher))
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

    /// Sends the registration to the StatusNotifierWatcher on the session bus. Without a
    /// reachable bus the icon cannot appear, as with `QSystemTrayIcon::show` when no tray
    /// exists: this is not an error, but `is_registered()` stays false.
    ///
    /// With a bus, `RequestName` and `RegisterStatusNotifierItem` are only queued: the replies
    /// (and the `NameOwnerChanged` broadcasts) arrive through `DbusConnection::read_incoming`,
    /// which nothing drives outside tests yet, so the registration can never be confirmed.
    /// `show` therefore returns `Err` and `is_registered()` stays false, rather than claiming
    /// a registration it cannot see.
    fn show(&mut self) -> Result<(), &'static str> {
        self.status = "Active".to_string();

        let mut guard = self.connection.lock().unwrap();
        if guard.is_none() {
            *guard = DbusConnection::connect_session_bus().ok();
        }

        self.is_registered = false;
        let Some(conn) = guard.as_mut() else {
            return Ok(());
        };
        let service_name = format!("org.kde.StatusNotifierItem-{}-1", self.id);
        let requested = conn.request_name(&service_name);
        let queued = requested.is_ok()
            && conn
                .register_status_notifier_item("/StatusNotifierItem")
                .is_ok();

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
        if queued {
            Err("The StatusNotifierWatcher registration was sent but its reply is never read, so it is unconfirmed")
        } else {
            Err("The StatusNotifierWatcher registration could not be sent")
        }
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
            // org.freedesktop.Notifications.Notify: susssasa{sv}i
            notify_call.append_string(&self.id);
            notify_call.append_u32(0);
            notify_call.append_string(app_icon);
            notify_call.append_string(title);
            notify_call.append_string(msg);
            notify_call.append_empty_string_array();
            notify_call.append_empty_dict_sv();
            notify_call.append_i32(i32::try_from(dur).unwrap_or(i32::MAX));
            let _ = conn.send_message(notify_call);
        }
        Ok(())
    }

    fn supports_messages(&self) -> bool {
        true
    }
}
