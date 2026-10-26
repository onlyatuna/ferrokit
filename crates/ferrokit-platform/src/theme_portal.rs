// SPDX-License-Identifier: LGPL-3.0-only
//! The `org.freedesktop.appearance` `color-scheme` setting of the XDG desktop portal, the source
//! of the colour scheme on Linux (GNOME, KDE, and every desktop with a portal backend).
//!
//! Mirrors Qt 6.11 (`qtbase` tag `v6.11.2`):
//! - `QGnomePortalInterface` (qgnomeportalinterface.cpp) reads the setting once at start-up and
//!   then follows the `SettingChanged` signal (`QDBusListener`, qdbuslistener.cpp:
//!   `org.freedesktop.portal.Settings` on `/org/freedesktop/portal/desktop`, no sender filter);
//! - `QDBusSettings::XdgSettings::convertColorScheme` (qdbussettings.cpp:24-35) maps the value:
//!   `0` no preference to `Unknown`, `1` prefer dark to `Dark`, `2` prefer light to `Light`;
//! - `QGnomeThemePrivate::colorScheme` (qgnometheme.cpp:56-78) falls back to what the GTK theme
//!   name says when the portal reports `Unknown` (here: [`crate::theme::UnixTheme`]'s fallback).
//!
//! Qt reads the value with `ReadAll` for the appearance namespace; this reads the one key with
//! `Read`, which the same interface has offered since its first version.
//!
//! [`PortalSession`] is the protocol, on any [`DbusConnection`]; it is exercised on every
//! platform against the scripted bus. [`spawn_listener`] (Linux) runs it on a thread that waits
//! on the bus socket. It could not be run against a real portal in the environment this was
//! written in.

use crate::theme::ColorScheme;
use crate::tray::dbus_connection::{
    DbusConnection, DbusMessage, Reader, DBUS_MESSAGE_TYPE_ERROR, DBUS_MESSAGE_TYPE_METHOD_RETURN,
    DBUS_MESSAGE_TYPE_SIGNAL,
};

pub(crate) const PORTAL_SERVICE: &str = "org.freedesktop.portal.Desktop";
pub(crate) const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
pub(crate) const SETTINGS_INTERFACE: &str = "org.freedesktop.portal.Settings";
pub(crate) const APPEARANCE_NAMESPACE: &str = "org.freedesktop.appearance";
pub(crate) const COLOR_SCHEME_KEY: &str = "color-scheme";

/// `QDBusSettings::XdgSettings::convertColorScheme`.
pub(crate) fn convert_color_scheme(value: u32) -> ColorScheme {
    match value {
        1 => ColorScheme::Dark,
        2 => ColorScheme::Light,
        _ => ColorScheme::Unknown,
    }
}

/// What the portal told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PortalEvent {
    /// The answer to the start-up `Read`: the scheme, or `None` when the portal has no such
    /// setting (no portal, an old one, an error reply).
    Initial(Option<ColorScheme>),
    /// A `SettingChanged` for the colour scheme.
    Changed(ColorScheme),
}

/// A `v` that holds a `u` (the portal's value), possibly wrapped in further variants: some
/// portal versions answer `Read` with a variant inside the reply's variant.
fn read_variant_u32(reader: &mut Reader<'_>) -> Option<u32> {
    // A reply cannot meaningfully nest deeper than a couple of levels; bound the loop anyway.
    for _ in 0..4 {
        match reader.signature().ok()?.as_str() {
            "v" => continue,
            "u" => return reader.u32().ok(),
            _ => return None,
        }
    }
    None
}

/// The protocol: subscribe to `SettingChanged`, `Read` the current value, and turn the bus's
/// messages into [`PortalEvent`]s.
pub(crate) struct PortalSession {
    conn: DbusConnection,
    read_serial: u32,
    /// The answer to the `Read` has arrived (or failed).
    initial_done: bool,
}

impl PortalSession {
    /// Subscribes and sends the `Read`; the answer comes through [`poll`](Self::poll).
    pub(crate) fn start(mut conn: DbusConnection) -> Result<Self, &'static str> {
        // As `QDBusListener`: the match names path, interface and member, not the sender.
        conn.add_match(&format!(
            "type='signal',path='{PORTAL_PATH}',interface='{SETTINGS_INTERFACE}',member='SettingChanged'"
        ))?;
        let read_serial = conn.next_serial();
        let mut read = DbusMessage::method_call(
            PORTAL_SERVICE,
            PORTAL_PATH,
            SETTINGS_INTERFACE,
            "Read",
            read_serial,
        );
        read.append_string(APPEARANCE_NAMESPACE);
        read.append_string(COLOR_SCHEME_KEY);
        conn.send_message(read)?;
        Ok(Self {
            conn,
            read_serial,
            initial_done: false,
        })
    }

    /// The bus socket, for waiting until [`poll`](Self::poll) has something to read.
    #[cfg(all(target_os = "linux", not(miri)))]
    pub(crate) fn socket_fd(&self) -> ferrokit_core::event_loop::SocketDescriptor {
        self.conn.socket_fd()
    }

    /// Reads what the bus has sent (never blocks) and returns the colour-scheme events in it, in
    /// order. An error means the connection is gone.
    pub(crate) fn poll(&mut self) -> Result<Vec<PortalEvent>, &'static str> {
        let mut events = Vec::new();
        for message in self.conn.read_incoming()? {
            if let Some(event) = self.event_of(&message) {
                events.push(event);
            }
        }
        Ok(events)
    }

    fn event_of(&mut self, message: &DbusMessage) -> Option<PortalEvent> {
        match message.msg_type {
            DBUS_MESSAGE_TYPE_METHOD_RETURN | DBUS_MESSAGE_TYPE_ERROR
                if message.reply_serial == Some(self.read_serial) && !self.initial_done =>
            {
                self.initial_done = true;
                let scheme = (message.msg_type == DBUS_MESSAGE_TYPE_METHOD_RETURN
                    && message.signature == "v")
                    .then(|| read_variant_u32(&mut message.body_reader()))
                    .flatten()
                    .map(convert_color_scheme);
                Some(PortalEvent::Initial(scheme))
            }
            DBUS_MESSAGE_TYPE_SIGNAL
                if message.interface.as_deref() == Some(SETTINGS_INTERFACE)
                    && message.member.as_deref() == Some("SettingChanged")
                    && message.signature == "ssv" =>
            {
                let mut reader = message.body_reader();
                let namespace = reader.string().ok()?;
                let key = reader.string().ok()?;
                if namespace != APPEARANCE_NAMESPACE || key != COLOR_SCHEME_KEY {
                    return None;
                }
                read_variant_u32(&mut reader).map(|v| PortalEvent::Changed(convert_color_scheme(v)))
            }
            _ => None,
        }
    }
}

/// Runs a thread that connects to the session bus, subscribes, and feeds the portal's
/// colour-scheme events to `on_event` until `alive` returns `false` or the bus goes away. Nothing
/// blocks the caller (not even connecting): the returned receiver gets one message when the answer
/// to the start-up `Read` has been handed to `on_event`, and disconnects without one if there is no
/// session bus or the bus goes away first. A caller that wants the first scheme to be the portal's
/// can wait on it; one that does not never blocks. `None` when the thread could not be started.
///
/// `on_event` runs on the listener thread.
#[cfg(all(target_os = "linux", not(miri)))]
pub(crate) fn spawn_listener(
    alive: impl Fn() -> bool + Send + 'static,
    on_event: impl Fn(PortalEvent) + Send + 'static,
) -> Option<std::sync::mpsc::Receiver<()>> {
    use std::time::Duration;

    let (initial_tx, initial_rx) = std::sync::mpsc::channel::<()>();
    std::thread::Builder::new()
        .name("ferrokit-portal-theme".to_string())
        .spawn(move || {
            let Ok(conn) = DbusConnection::connect_session_bus() else {
                return;
            };
            let Ok(mut session) = PortalSession::start(conn) else {
                return;
            };
            while alive() {
                let Ok(events) = session.poll() else {
                    break;
                };
                for event in events {
                    let is_initial = matches!(event, PortalEvent::Initial(_));
                    on_event(event);
                    if is_initial {
                        let _ = initial_tx.send(());
                    }
                }
                // Wake at least once a second to notice that the theme is gone.
                wait_readable(session.socket_fd(), Duration::from_secs(1));
            }
        })
        .ok()?;
    Some(initial_rx)
}

/// `poll(2)` on one descriptor for readability; `true` when it is readable (or in error or hang
/// up, which the next read reports), `false` on timeout or when interrupted.
#[cfg(all(target_os = "linux", not(miri)))]
fn wait_readable(
    fd: ferrokit_core::event_loop::SocketDescriptor,
    timeout: std::time::Duration,
) -> bool {
    #[repr(C)]
    struct PollFd {
        fd: i32,
        events: i16,
        revents: i16,
    }
    extern "C" {
        fn poll(fds: *mut PollFd, nfds: std::ffi::c_ulong, timeout: i32) -> i32;
    }
    const POLLIN: i16 = 0x001;
    if fd < 0 {
        std::thread::sleep(timeout);
        return false;
    }
    let mut pfd = PollFd {
        fd,
        events: POLLIN,
        revents: 0,
    };
    let millis = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
    // SAFETY: `pfd` is one valid `pollfd` and `nfds` is 1; `poll` writes only `revents`.
    let ready = unsafe { poll(&mut pfd, 1, millis) };
    ready > 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tray::dbus_connection::scripted::{ScriptedDbusBus, ScriptedDbusTransport};
    use crate::tray::dbus_connection::DBUS_MESSAGE_TYPE_METHOD_CALL;

    fn session() -> (PortalSession, ScriptedDbusBus) {
        let (transport, bus) = ScriptedDbusTransport::new(":1.7");
        let conn = DbusConnection::from_transport(Box::new(transport), "scripted:", 1000)
            .expect("handshake with the scripted bus");
        (PortalSession::start(conn).expect("start"), bus)
    }

    fn read_reply(serial: u32, scheme: u32) -> Vec<u8> {
        let mut reply = DbusMessage::method_return(serial, ":1.7", 20);
        reply.append_variant_u32(scheme);
        reply.to_bytes()
    }

    fn setting_changed(namespace: &str, key: &str, scheme: u32) -> Vec<u8> {
        let mut signal = DbusMessage::signal(PORTAL_PATH, SETTINGS_INTERFACE, "SettingChanged", 21);
        signal.append_string(namespace);
        signal.append_string(key);
        signal.append_variant_u32(scheme);
        signal.to_bytes()
    }

    #[test]
    fn values_map_like_qt() {
        assert_eq!(convert_color_scheme(0), ColorScheme::Unknown);
        assert_eq!(convert_color_scheme(1), ColorScheme::Dark);
        assert_eq!(convert_color_scheme(2), ColorScheme::Light);
        assert_eq!(convert_color_scheme(99), ColorScheme::Unknown);
    }

    #[test]
    fn start_subscribes_and_reads_the_setting() {
        let (_session, bus) = session();
        let calls: Vec<DbusMessage> = bus
            .received()
            .into_iter()
            .filter(|m| m.msg_type == DBUS_MESSAGE_TYPE_METHOD_CALL)
            .collect();
        let members: Vec<_> = calls.iter().filter_map(|m| m.member.as_deref()).collect();
        assert_eq!(members, ["Hello", "AddMatch", "Read"]);
        let add_match = &calls[1];
        assert_eq!(
            add_match.body_string().unwrap(),
            "type='signal',path='/org/freedesktop/portal/desktop',\
             interface='org.freedesktop.portal.Settings',member='SettingChanged'"
        );
        let read = &calls[2];
        assert_eq!(read.destination.as_deref(), Some(PORTAL_SERVICE));
        assert_eq!(read.path.as_deref(), Some(PORTAL_PATH));
        assert_eq!(read.interface.as_deref(), Some(SETTINGS_INTERFACE));
        assert_eq!(read.signature, "ss");
        let mut reader = read.body_reader();
        assert_eq!(reader.string().unwrap(), "org.freedesktop.appearance");
        assert_eq!(reader.string().unwrap(), "color-scheme");
    }

    #[test]
    fn the_read_reply_is_the_initial_scheme() {
        let (mut session, bus) = session();
        bus.push_to_client(&read_reply(session.read_serial, 1));
        assert_eq!(
            session.poll().unwrap(),
            [PortalEvent::Initial(Some(ColorScheme::Dark))]
        );
        // Only the first answer to the Read is the initial one.
        bus.push_to_client(&read_reply(session.read_serial, 2));
        assert!(session.poll().unwrap().is_empty());
    }

    #[test]
    fn a_doubly_wrapped_read_reply_is_unwrapped() {
        let (mut session, bus) = session();
        let mut reply = DbusMessage::method_return(session.read_serial, ":1.7", 20);
        // v holding v holding u: signature "v", then "v", then "u" and the value.
        reply.signature.push('v');
        reply
            .body
            .extend_from_slice(&[1, b'v', 0, 1, b'u', 0, 0, 0]);
        reply.body.extend_from_slice(&2u32.to_le_bytes());
        bus.push_to_client(&reply.to_bytes());
        assert_eq!(
            session.poll().unwrap(),
            [PortalEvent::Initial(Some(ColorScheme::Light))]
        );
    }

    #[test]
    fn an_error_reply_means_no_portal_setting() {
        let (mut session, bus) = session();
        let mut error = DbusMessage::method_return(session.read_serial, ":1.7", 20);
        error.msg_type = DBUS_MESSAGE_TYPE_ERROR;
        bus.push_to_client(&error.to_bytes());
        assert_eq!(session.poll().unwrap(), [PortalEvent::Initial(None)]);
    }

    #[test]
    fn setting_changed_for_the_color_scheme_is_reported() {
        let (mut session, bus) = session();
        bus.push_to_client(&setting_changed(
            "org.freedesktop.appearance",
            "color-scheme",
            1,
        ));
        bus.push_to_client(&setting_changed(
            "org.freedesktop.appearance",
            "color-scheme",
            2,
        ));
        bus.push_to_client(&setting_changed(
            "org.freedesktop.appearance",
            "color-scheme",
            0,
        ));
        assert_eq!(
            session.poll().unwrap(),
            [
                PortalEvent::Changed(ColorScheme::Dark),
                PortalEvent::Changed(ColorScheme::Light),
                PortalEvent::Changed(ColorScheme::Unknown),
            ]
        );
    }

    #[test]
    fn other_settings_are_ignored() {
        let (mut session, bus) = session();
        bus.push_to_client(&setting_changed(
            "org.freedesktop.appearance",
            "accent-color",
            1,
        ));
        bus.push_to_client(&setting_changed(
            "org.gnome.desktop.interface",
            "color-scheme",
            1,
        ));
        let mut other = DbusMessage::signal(PORTAL_PATH, "org.example.Other", "SettingChanged", 22);
        other.append_string("org.freedesktop.appearance");
        other.append_string("color-scheme");
        other.append_variant_u32(1);
        bus.push_to_client(&other.to_bytes());
        assert!(session.poll().unwrap().is_empty());
    }

    #[test]
    fn a_closed_bus_is_an_error() {
        let (mut session, bus) = session();
        bus.close_after_queued();
        assert!(session.poll().is_err());
    }
}
