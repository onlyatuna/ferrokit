// SPDX-License-Identifier: LGPL-3.0-only
use super::dbus_connection::{DbusConnection, DbusMessage, DBUS_MESSAGE_TYPE_METHOD_CALL};
use crate::menu::PlatformMenu;
use crate::platform_tray::PlatformTrayIcon;
use crate::tray_icon::TrayActivation;
use ferrokit_core::event_loop::{EventDispatcher, SocketNotifier, SocketNotifierError};
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
    activation_handler: Option<Box<dyn Fn(TrayActivation)>>,
    /// The notifier `bind_event_dispatcher` registered for the bus socket.
    notifier: Option<std::sync::Arc<SocketNotifier>>,
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
            activation_handler: None,
            notifier: None,
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

    /// Uses `connection` instead of connecting to the session bus on first use. A notifier
    /// bound to the previous connection is disabled first, since its descriptor goes away.
    pub fn set_connection(&mut self, connection: DbusConnection) {
        if let Some(notifier) = self.notifier.take() {
            notifier.set_enabled(false);
        }
        *self.connection.lock().unwrap() = Some(connection);
    }

    /// Watches the bus socket; `Ok(None)` when there is no bus connection or it has no
    /// descriptor, and [`SocketNotifierError::Unsupported`] when the dispatcher does not poll
    /// descriptors (see [`DbusConnection::bind_event_dispatcher`]). The item keeps the notifier
    /// so that it can disable it when the connection fails.
    pub fn bind_event_dispatcher(
        &mut self,
        dispatcher: &mut dyn EventDispatcher,
    ) -> Result<Option<std::sync::Arc<SocketNotifier>>, SocketNotifierError> {
        let mut conn_guard = self.connection.lock().unwrap();
        if conn_guard.is_none() {
            *conn_guard = DbusConnection::connect_session_bus().ok();
        }
        let notifier = match conn_guard.as_ref() {
            Some(connection) => connection.bind_event_dispatcher(dispatcher)?,
            None => None,
        };
        drop(conn_guard);
        self.notifier = notifier.clone();
        Ok(notifier)
    }

    /// Stands in for what [`bind_event_dispatcher`](Self::bind_event_dispatcher) stores, for tests
    /// of how the item treats the notifier when the connection fails.
    #[cfg(any(test, feature = "test-hooks"))]
    pub fn set_socket_notifier(&mut self, notifier: std::sync::Arc<SocketNotifier>) {
        self.notifier = Some(notifier);
    }

    /// What the host's `Activate(x, y)` call does: reports [`TrayActivation::Trigger`], as
    /// `QStatusNotifierItemAdaptor::Activate` does (the position is only logged by Qt).
    pub fn activate(&mut self, _x: i32, _y: i32) {
        self.emit_activated(TrayActivation::Trigger);
    }

    /// What the host's `ContextMenu(x, y)` call does: [`TrayActivation::Context`]
    /// (`QStatusNotifierItemAdaptor::ContextMenu`).
    pub fn context_menu(&mut self, _x: i32, _y: i32) {
        self.emit_activated(TrayActivation::Context);
    }

    /// What the host's `SecondaryActivate(x, y)` call does: [`TrayActivation::MiddleClick`]
    /// (`QStatusNotifierItemAdaptor::SecondaryActivate`).
    pub fn secondary_activate(&mut self, _x: i32, _y: i32) {
        self.emit_activated(TrayActivation::MiddleClick);
    }

    fn emit_activated(&self, reason: TrayActivation) {
        if let Some(handler) = &self.activation_handler {
            handler(reason);
        }
    }

    /// Dispatches one message received from the bus. A method call to `/StatusNotifierItem` is
    /// dispatched by member name when its interface is `org.kde.StatusNotifierItem` or absent
    /// (QtDBus does the same): `Activate`, `ContextMenu` and `SecondaryActivate` report their
    /// reason, `Scroll` and `ProvideXdgActivationToken` are accepted and ignored (Qt:
    /// "unsupported" / only stores the token in the environment), and the caller gets an empty
    /// reply. Any other call to the item (another member, or another interface such as
    /// `org.freedesktop.DBus.Properties`, which this crate does not implement) gets an
    /// `org.freedesktop.DBus.Error.UnknownMethod` error, so the host does not wait for its
    /// timeout. Replies are skipped when the caller set `NO_REPLY_EXPECTED`. Messages for any
    /// other object are left alone. Returns whether the message was a call to the item.
    pub fn handle_message(&mut self, msg: &DbusMessage) -> bool {
        if msg.msg_type != DBUS_MESSAGE_TYPE_METHOD_CALL
            || msg.path.as_deref() != Some("/StatusNotifierItem")
        {
            return false;
        }
        let interface_matches = msg
            .interface
            .as_deref()
            .is_none_or(|interface| interface == "org.kde.StatusNotifierItem");
        let known = interface_matches
            && match msg.member.as_deref() {
                Some("Activate") => {
                    self.activate(0, 0);
                    true
                }
                Some("ContextMenu") => {
                    self.context_menu(0, 0);
                    true
                }
                Some("SecondaryActivate") => {
                    self.secondary_activate(0, 0);
                    true
                }
                Some("Scroll") | Some("ProvideXdgActivationToken") => true,
                _ => false,
            };
        const NO_REPLY_EXPECTED: u8 = 0x1;
        if msg.flags & NO_REPLY_EXPECTED == 0 {
            if let Some(conn) = self.connection.lock().unwrap().as_mut() {
                let serial = conn.next_serial();
                let destination = msg.sender.as_deref().unwrap_or("");
                let reply = if known {
                    DbusMessage::method_return(msg.serial, destination, serial)
                } else {
                    DbusMessage::error(
                        msg.serial,
                        destination,
                        serial,
                        "org.freedesktop.DBus.Error.UnknownMethod",
                        &format!(
                            "No such method {}.{}",
                            msg.interface.as_deref().unwrap_or("(any)"),
                            msg.member.as_deref().unwrap_or("")
                        ),
                    )
                };
                let _ = conn.send_message(reply);
            }
        }
        true
    }

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

    /// Mirrors `QStatusNotifierItemAdaptor` (qstatusnotifieritemadaptor.cpp): see
    /// [`DbusStatusNotifierItem::handle_message`] for the method-to-reason mapping. The reasons
    /// arrive when [`PlatformTrayIcon::process_events`] runs, which something must drive from
    /// the bus socket's readability ([`DbusStatusNotifierItem::bind_event_dispatcher`]).
    ///
    /// **Not run on Linux.** It is type-checked for `x86_64-unknown-linux-gnu` and tested
    /// against the in-memory bus; no real StatusNotifierHost has called the item. The item
    /// answers `Activate`, `ContextMenu` and `SecondaryActivate` only: nothing in this crate
    /// answers the host's `org.freedesktop.DBus.Properties` reads, so a host that needs them
    /// to show the icon is a separate, existing gap.
    fn set_activation_handler(&mut self, handler: Box<dyn Fn(TrayActivation)>) {
        self.activation_handler = Some(handler);
    }

    /// Reads the messages that arrived on the bus socket and runs [`handle_message`]
    /// on each. When the connection has failed (the peer closed it, or the stream is
    /// corrupt), the item is no longer registered and the notifier from `bind_event_dispatcher`
    /// is disabled, so the dispatcher stops polling the descriptor. The closed connection is
    /// kept rather than dropped: its descriptor must stay valid for as long as the notifier is
    /// registered, and the item stays silent until it is rebuilt.
    ///
    /// [`handle_message`]: DbusStatusNotifierItem::handle_message
    fn process_events(&mut self) {
        let incoming = {
            let mut guard = self.connection.lock().unwrap();
            let Some(conn) = guard.as_mut() else {
                return;
            };
            match conn.read_incoming() {
                Ok(messages) => messages,
                Err(_) => {
                    drop(guard);
                    self.is_registered = false;
                    if let Some(notifier) = &self.notifier {
                        notifier.set_enabled(false);
                    }
                    return;
                }
            }
        };
        for message in &incoming {
            self.handle_message(message);
        }
    }

    fn bind_event_dispatcher(
        &mut self,
        dispatcher: &mut dyn EventDispatcher,
    ) -> Result<Option<std::sync::Arc<SocketNotifier>>, SocketNotifierError> {
        DbusStatusNotifierItem::bind_event_dispatcher(self, dispatcher)
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
