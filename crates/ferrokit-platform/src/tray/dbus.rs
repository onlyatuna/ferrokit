// SPDX-License-Identifier: LGPL-3.0-only
use super::dbus_connection::{
    DbusConnection, DbusMessage, DbusValue, DBUS_MESSAGE_TYPE_ERROR, DBUS_MESSAGE_TYPE_METHOD_CALL,
    DBUS_MESSAGE_TYPE_METHOD_RETURN, DBUS_MESSAGE_TYPE_SIGNAL,
};
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
    /// Serial of the `RegisterStatusNotifierItem` call whose reply has not arrived yet.
    pending_registration: Option<u32>,
    connection: Mutex<Option<DbusConnection>>,
    last_message: Option<(String, String, crate::platform_tray::TrayMessageIcon, u32)>,
    activation_handler: Option<Box<dyn Fn(TrayActivation)>>,
    /// The notifier `bind_event_dispatcher` registered for the bus socket.
    notifier: Option<std::sync::Arc<SocketNotifier>>,
}

/// What a method call to the item is answered with.
enum Reply {
    Empty,
    /// A reply whose signature and body are those of this message.
    Body(DbusMessage),
    Error(&'static str, String),
}

const ITEM_INTERFACE: &str = "org.kde.StatusNotifierItem";
const PROPERTIES_INTERFACE: &str = "org.freedesktop.DBus.Properties";
const WATCHER_NAME: &str = "org.kde.StatusNotifierWatcher";
/// `Menu` when the item has no dbusmenu, as Qt reports it without a menu.
const NO_DBUSMENU: &str = "/NO_DBUSMENU";
const INVALID_ARGS: &str = "org.freedesktop.DBus.Error.InvalidArgs";

impl DbusStatusNotifierItem {
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            category: "ApplicationStatus".to_string(),
            title: title.into(),
            tooltip: String::new(),
            status: "Passive".to_string(),
            icon_pixmap: Vec::new(),
            menu_path: NO_DBUSMENU.to_string(),
            pending_registration: None,
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

    /// Dispatches one method call received from the bus. A call to `/StatusNotifierItem` is
    /// dispatched by member name when its interface is `org.kde.StatusNotifierItem` or absent
    /// (QtDBus does the same): `Activate`, `ContextMenu` and `SecondaryActivate` report their
    /// reason, `Scroll` and `ProvideXdgActivationToken` are accepted and ignored (Qt:
    /// "unsupported" / only stores the token in the environment), and the caller gets an empty
    /// reply.
    ///
    /// `org.freedesktop.DBus.Properties` is served for `org.kde.StatusNotifierItem` (see
    /// [`property_names`](Self::property_names)): `Get(ss)` and `GetAll(s)` reply with the
    /// values, an unknown property or interface gets `org.freedesktop.DBus.Error.InvalidArgs`,
    /// and `Set` gets `org.freedesktop.DBus.Error.PropertyReadOnly` (the properties are
    /// read-only, as in Qt's adaptor).
    ///
    /// Any other call to the item (another member or interface) gets an
    /// `org.freedesktop.DBus.Error.UnknownMethod` error, so the host does not wait for its
    /// timeout. Replies are skipped when the caller set `NO_REPLY_EXPECTED`. Messages for any
    /// other object are left alone. Returns whether the message was a call to the item.
    pub fn handle_message(&mut self, msg: &DbusMessage) -> bool {
        if msg.msg_type != DBUS_MESSAGE_TYPE_METHOD_CALL
            || msg.path.as_deref() != Some("/StatusNotifierItem")
        {
            return false;
        }
        let member = msg.member.as_deref();
        let reply = if msg.interface.as_deref() == Some(PROPERTIES_INTERFACE)
            && matches!(member, Some("Get" | "GetAll" | "Set"))
        {
            self.properties_reply(msg)
        } else {
            let interface_matches = msg
                .interface
                .as_deref()
                .is_none_or(|interface| interface == ITEM_INTERFACE);
            let known = interface_matches
                && match member {
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
            if known {
                Reply::Empty
            } else {
                Reply::Error(
                    "org.freedesktop.DBus.Error.UnknownMethod",
                    format!(
                        "No such method {}.{}",
                        msg.interface.as_deref().unwrap_or("(any)"),
                        msg.member.as_deref().unwrap_or("")
                    ),
                )
            }
        };
        const NO_REPLY_EXPECTED: u8 = 0x1;
        if msg.flags & NO_REPLY_EXPECTED == 0 {
            if let Some(conn) = self.connection.lock().unwrap().as_mut() {
                let serial = conn.next_serial();
                let destination = msg.sender.as_deref().unwrap_or("");
                let reply = match reply {
                    Reply::Empty => DbusMessage::method_return(msg.serial, destination, serial),
                    Reply::Body(body) => {
                        let mut reply = DbusMessage::method_return(msg.serial, destination, serial);
                        reply.signature = body.signature;
                        reply.body = body.body;
                        reply
                    }
                    Reply::Error(name, description) => {
                        DbusMessage::error(msg.serial, destination, serial, name, &description)
                    }
                };
                let _ = conn.send_message(reply);
            }
        }
        true
    }

    /// The `org.kde.StatusNotifierItem` properties served through
    /// `org.freedesktop.DBus.Properties`, in the order `GetAll` lists them.
    pub fn property_names() -> &'static [&'static str] {
        &[
            "Category",
            "Id",
            "Title",
            "Status",
            "WindowId",
            "IconThemeName",
            "IconPixmap",
            "OverlayIconName",
            "OverlayIconPixmap",
            "AttentionIconName",
            "AttentionIconPixmap",
            "AttentionMovieName",
            "ToolTip",
            "ItemIsMenu",
            "Menu",
        ]
    }

    /// One property as `QStatusNotifierItemAdaptor` reports it. There are no overlay or
    /// attention icons, no icon theme name and no dbusmenu (`Menu` is `/NO_DBUSMENU`); the
    /// `ToolTip` title is the tooltip text.
    fn property(&self, name: &str) -> Option<DbusValue<'_>> {
        const NO_PIXMAPS: &[DbusImage] = &[];
        Some(match name {
            "Category" => DbusValue::Str(&self.category),
            "Id" => DbusValue::Str(&self.id),
            "Title" => DbusValue::Str(&self.title),
            "Status" => DbusValue::Str(&self.status),
            "WindowId" => DbusValue::U32(0),
            "IconThemeName" | "OverlayIconName" | "AttentionIconName" | "AttentionMovieName" => {
                DbusValue::Str("")
            }
            "IconPixmap" => DbusValue::Pixmaps(&self.icon_pixmap),
            "OverlayIconPixmap" | "AttentionIconPixmap" => DbusValue::Pixmaps(NO_PIXMAPS),
            "ToolTip" => DbusValue::ToolTip {
                icon_name: "",
                title: &self.tooltip,
                description: "",
            },
            "ItemIsMenu" => DbusValue::Bool(false),
            "Menu" => DbusValue::ObjectPath(&self.menu_path),
            _ => return None,
        })
    }

    fn properties_reply(&self, msg: &DbusMessage) -> Reply {
        let invalid = |description: String| Reply::Error(INVALID_ARGS, description);
        let member = msg.member.as_deref().unwrap_or("");
        if member == "Set" {
            return Reply::Error(
                "org.freedesktop.DBus.Error.PropertyReadOnly",
                "StatusNotifierItem properties are read-only".to_string(),
            );
        }
        let args = msg.body_strings().unwrap_or_default();
        let expected = if member == "Get" { 2 } else { 1 };
        if args.len() != expected || msg.signature.len() != expected {
            return invalid(format!("{member} expects {expected} string argument(s)"));
        }
        if args[0] != ITEM_INTERFACE {
            return invalid(format!("No such interface {}", args[0]));
        }
        let mut body = DbusMessage::method_return(0, "", 0);
        if member == "Get" {
            let Some(value) = self.property(&args[1]) else {
                return invalid(format!("No such property {}", args[1]));
            };
            body.append_variant(&value);
        } else {
            let entries: Vec<_> = Self::property_names()
                .iter()
                .filter_map(|&name| Some((name, self.property(name)?)))
                .collect();
            body.append_dict_sv(&entries);
        }
        Reply::Body(body)
    }

    /// Handles a message that is not a call to the item: the reply to the
    /// `RegisterStatusNotifierItem` call (`METHOD_RETURN` confirms the registration, an
    /// `ERROR` such as `ServiceUnknown` when no watcher runs rejects it) and the bus's
    /// `NameOwnerChanged` signal, which goes to
    /// [`handle_name_owner_changed`](Self::handle_name_owner_changed).
    fn handle_bus_message(&mut self, msg: &DbusMessage) {
        match msg.msg_type {
            DBUS_MESSAGE_TYPE_METHOD_RETURN | DBUS_MESSAGE_TYPE_ERROR
                if msg.reply_serial.is_some() && msg.reply_serial == self.pending_registration =>
            {
                self.pending_registration = None;
                self.is_registered = msg.msg_type == DBUS_MESSAGE_TYPE_METHOD_RETURN;
            }
            DBUS_MESSAGE_TYPE_SIGNAL
                if msg.sender.as_deref() == Some("org.freedesktop.DBus")
                    && msg.interface.as_deref() == Some("org.freedesktop.DBus")
                    && msg.member.as_deref() == Some("NameOwnerChanged") =>
            {
                if let Some([name, old_owner, new_owner]) = msg.body_strings().as_deref() {
                    self.handle_name_owner_changed(name, old_owner, new_owner);
                }
            }
            _ => {}
        }
    }

    pub fn last_message(&self) -> Option<(&str, &str, crate::platform_tray::TrayMessageIcon, u32)> {
        self.last_message
            .as_ref()
            .map(|(t, m, i, d)| (t.as_str(), m.as_str(), *i, *d))
    }
    /// Handles the `NameOwnerChanged` broadcast for the StatusNotifierWatcher: when the host
    /// (KDE Plasma, Waybar, etc.) goes away the item is no longer registered, and when a new
    /// owner appears the item registers again. The confirmation arrives through
    /// [`process_events`](PlatformTrayIcon::process_events) like the first one; the bus name
    /// requested by `show` is still held, so only the registration is repeated.
    pub fn handle_name_owner_changed(&mut self, name: &str, _old_owner: &str, new_owner: &str) {
        if name != WATCHER_NAME {
            return;
        }
        self.is_registered = false;
        self.pending_registration = None;
        if new_owner.is_empty() {
            return;
        }
        let mut guard = self.connection.lock().unwrap();
        if let Some(conn) = guard.as_mut() {
            self.pending_registration = conn
                .register_status_notifier_item("/StatusNotifierItem")
                .ok();

            let serial = conn.next_serial();
            let mut sig =
                DbusMessage::signal("/StatusNotifierItem", ITEM_INTERFACE, "NewStatus", serial);
            sig.append_string(&self.status);
            let _ = conn.send_message(sig);
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
    /// [`DbusStatusNotifierItem::handle_message`] for the method-to-reason mapping and the
    /// `org.freedesktop.DBus.Properties` reads that are answered. The reasons
    /// arrive when [`PlatformTrayIcon::process_events`] runs, which something must drive from
    /// the bus socket's readability ([`DbusStatusNotifierItem::bind_event_dispatcher`]).
    ///
    /// **Not run on Linux.** It is type-checked for `x86_64-unknown-linux-gnu` and tested
    /// against the in-memory bus; no real StatusNotifierHost has called the item or read its
    /// properties, so how a real host displays the icon is unverified.
    fn set_activation_handler(&mut self, handler: Box<dyn Fn(TrayActivation)>) {
        self.activation_handler = Some(handler);
    }

    /// Reads the messages that arrived on the bus socket and handles each: method calls to the
    /// item by [`handle_message`], the registration reply (which sets
    /// [`is_registered`](DbusStatusNotifierItem::is_registered)) and `NameOwnerChanged`
    /// broadcasts. When the connection has failed (the peer closed it, or the stream is
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
                    self.pending_registration = None;
                    if let Some(notifier) = &self.notifier {
                        notifier.set_enabled(false);
                    }
                    return;
                }
            }
        };
        for message in &incoming {
            if !self.handle_message(message) {
                self.handle_bus_message(message);
            }
        }
    }

    fn bind_event_dispatcher(
        &mut self,
        dispatcher: &mut dyn EventDispatcher,
    ) -> Result<Option<std::sync::Arc<SocketNotifier>>, SocketNotifierError> {
        DbusStatusNotifierItem::bind_event_dispatcher(self, dispatcher)
    }

    /// Queues the registration with the StatusNotifierWatcher on the session bus and returns
    /// without waiting for the answer, like `QSystemTrayIcon::show` with an asynchronous
    /// registration. Without a reachable bus the icon cannot appear, as with
    /// `QSystemTrayIcon::show` when no tray exists: this is not an error, but `is_registered()`
    /// stays false. `Err` means the requests could not be queued on the connection.
    ///
    /// With a bus, `RequestName` and `RegisterStatusNotifierItem` are queued and
    /// `is_registered()` stays false until [`process_events`](PlatformTrayIcon::process_events)
    /// reads the watcher's reply: a method return confirms the registration, an error (for
    /// example `ServiceUnknown` when no watcher runs) leaves it unregistered, and a watcher
    /// that appears later re-registers the item through `NameOwnerChanged`.
    fn show(&mut self) -> Result<(), &'static str> {
        self.status = "Active".to_string();

        let mut guard = self.connection.lock().unwrap();
        if guard.is_none() {
            *guard = DbusConnection::connect_session_bus().ok();
        }

        self.is_registered = false;
        self.pending_registration = None;
        let Some(conn) = guard.as_mut() else {
            return Ok(());
        };

        // Subscribe to NameOwnerChanged signals to detect panel restarts
        let _ = conn.add_match("type='signal',interface='org.freedesktop.DBus',member='NameOwnerChanged',arg0='org.kde.StatusNotifierWatcher'");
        let service_name = format!("org.kde.StatusNotifierItem-{}-1", self.id);
        let registration = conn
            .request_name(&service_name)
            .and_then(|_| conn.register_status_notifier_item("/StatusNotifierItem"));

        // Emit NewStatus("Active") signal
        let serial = conn.next_serial();
        let mut sig =
            DbusMessage::signal("/StatusNotifierItem", ITEM_INTERFACE, "NewStatus", serial);
        sig.append_string("Active");
        let _ = conn.send_message(sig);

        match registration {
            Ok(serial) => {
                self.pending_registration = Some(serial);
                Ok(())
            }
            Err(_) => Err("The StatusNotifierWatcher registration could not be sent"),
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
