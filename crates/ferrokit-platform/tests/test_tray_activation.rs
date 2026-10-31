// SPDX-License-Identifier: LGPL-3.0-only
//! `QSystemTrayIcon::activated(ActivationReason)` on the back ends that are not Windows: the
//! StatusNotifierItem calls of a Linux desktop (`QStatusNotifierItemAdaptor`) against the
//! in-memory bus, and the AppKit status item (`QCocoaSystemTrayIcon::emitActivated`) against the
//! mock Objective-C runtime. Neither runs against a real desktop here: the Linux test uses a
//! scripted bus and the macOS one fake AppKit objects, so a real StatusNotifierHost or a real
//! status bar is still unverified.

use std::cell::RefCell;
use std::rc::Rc;

use ferrokit_platform::tray::dbus_connection::scripted::ScriptedDbusTransport;
use ferrokit_platform::tray::dbus_connection::DBUS_MESSAGE_TYPE_METHOD_RETURN;
use ferrokit_platform::tray::PlatformTrayIcon;
use ferrokit_platform::{DbusConnection, DbusMessage, DbusStatusNotifierItem, TrayActivation};

type Seen = Rc<RefCell<Vec<TrayActivation>>>;

fn item_with_bus() -> (
    DbusStatusNotifierItem,
    ferrokit_platform::tray::dbus_connection::scripted::ScriptedDbusBus,
    Seen,
) {
    let (transport, bus) = ScriptedDbusTransport::new(":1.42");
    let conn = DbusConnection::from_transport(Box::new(transport), "scripted:", 1000)
        .expect("handshake with the scripted bus");
    let mut item = DbusStatusNotifierItem::new("ferrokit.app", "HUD");
    item.set_connection(conn);
    let seen: Seen = Rc::default();
    let sink = Rc::clone(&seen);
    item.set_activation_handler(Box::new(move |reason| sink.borrow_mut().push(reason)));
    (item, bus, seen)
}

fn host_call(member: &str, serial: u32) -> DbusMessage {
    let mut call = DbusMessage::method_call(
        "org.kde.StatusNotifierItem-ferrokit.app-1",
        "/StatusNotifierItem",
        "org.kde.StatusNotifierItem",
        member,
        serial,
    );
    call.sender = Some(":1.99".to_string());
    call.append_i32(10);
    call.append_i32(20);
    call
}

#[test]
fn status_notifier_methods_map_to_qt_activation_reasons() {
    let (mut item, bus, seen) = item_with_bus();
    // QStatusNotifierItemAdaptor: Activate -> Trigger, ContextMenu -> Context,
    // SecondaryActivate -> MiddleClick; Scroll is "unsupported" and reports nothing.
    for (serial, member) in [
        (10, "Activate"),
        (11, "ContextMenu"),
        (12, "SecondaryActivate"),
        (13, "Scroll"),
        (14, "Activate"),
    ] {
        bus.push_to_client(&host_call(member, serial).to_bytes());
    }
    item.process_events();
    assert_eq!(
        *seen.borrow(),
        [
            TrayActivation::Trigger,
            TrayActivation::Context,
            TrayActivation::MiddleClick,
            TrayActivation::Trigger,
        ]
    );
    // Every call is answered, to its sender, with a method return for its serial.
    let replies: Vec<_> = bus
        .received()
        .into_iter()
        .filter(|m| m.msg_type == DBUS_MESSAGE_TYPE_METHOD_RETURN)
        .collect();
    let serials: Vec<_> = replies.iter().map(|m| m.reply_serial).collect();
    assert_eq!(serials, [Some(10), Some(11), Some(12), Some(13), Some(14)]);
    assert!(replies
        .iter()
        .all(|m| m.destination.as_deref() == Some(":1.99")));
}

#[test]
fn calls_for_other_objects_and_members_are_ignored() {
    let (mut item, bus, seen) = item_with_bus();
    let mut wrong_path = host_call("Activate", 20);
    wrong_path.path = Some("/Other".to_string());
    let mut wrong_interface = host_call("Activate", 21);
    wrong_interface.interface = Some("org.example.Other".to_string());
    for msg in [wrong_path, wrong_interface, host_call("NoSuchMethod", 22)] {
        bus.push_to_client(&msg.to_bytes());
    }
    item.process_events();
    assert!(seen.borrow().is_empty());
    assert!(!bus
        .received()
        .iter()
        .any(|m| m.msg_type == DBUS_MESSAGE_TYPE_METHOD_RETURN));
}

#[test]
fn no_reply_expected_calls_are_not_answered() {
    let (mut item, bus, seen) = item_with_bus();
    let mut call = host_call("Activate", 30);
    call.flags = 0x1;
    bus.push_to_client(&call.to_bytes());
    item.process_events();
    assert_eq!(*seen.borrow(), [TrayActivation::Trigger]);
    assert!(!bus
        .received()
        .iter()
        .any(|m| m.msg_type == DBUS_MESSAGE_TYPE_METHOD_RETURN));
}

#[test]
fn process_events_without_a_bus_or_handler_is_harmless() {
    let mut item = DbusStatusNotifierItem::new("ferrokit.app", "HUD");
    item.process_events();
    item.activate(0, 0);
    item.context_menu(0, 0);
    item.secondary_activate(0, 0);
}

#[test]
fn activation_reasons_carry_qts_values() {
    // qsystemtrayicon.h: Unknown, Context, DoubleClick, Trigger, MiddleClick.
    assert_eq!(TrayActivation::Unknown as i32, 0);
    assert_eq!(TrayActivation::Context as i32, 1);
    assert_eq!(TrayActivation::DoubleClick as i32, 2);
    assert_eq!(TrayActivation::Trigger as i32, 3);
    assert_eq!(TrayActivation::MiddleClick as i32, 4);
}

/// `forward_activations` is what the non-Windows `TrayIcon` uses to feed `on_activated`; it is
/// platform neutral, so it runs here too. (`TrayIcon::with_backend` itself is not compiled on
/// Windows, where `TrayIcon` is the Win32 one.)
#[test]
fn backend_activations_are_forwarded_to_a_signal() {
    use ferrokit_core::signal::Signal;
    use ferrokit_platform::platform_tray::forward_activations;

    let (mut item, bus, _seen) = item_with_bus();
    let signal = Signal::new();
    let reasons = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = std::sync::Arc::clone(&reasons);
    signal.connect(move |reason: &TrayActivation| sink.lock().unwrap().push(*reason));
    forward_activations(&mut item, &signal);
    bus.push_to_client(&host_call("SecondaryActivate", 40).to_bytes());
    item.process_events();
    assert_eq!(*reasons.lock().unwrap(), [TrayActivation::MiddleClick]);
}

#[cfg(not(windows))]
#[test]
fn tray_icon_forwards_backend_activations_to_its_signal() {
    use ferrokit_platform::TrayIcon;

    let (item, bus, _seen) = item_with_bus();
    let mut tray = TrayIcon::with_backend(Box::new(item));
    let reasons = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = std::sync::Arc::clone(&reasons);
    tray.on_activated
        .connect(move |reason| sink.lock().unwrap().push(*reason));
    bus.push_to_client(&host_call("SecondaryActivate", 40).to_bytes());
    tray.process_events();
    assert_eq!(*reasons.lock().unwrap(), [TrayActivation::MiddleClick]);
}

#[test]
fn unknown_calls_to_the_item_get_an_unknown_method_error() {
    use ferrokit_platform::tray::dbus_connection::DBUS_MESSAGE_TYPE_ERROR;

    let (mut item, bus, seen) = item_with_bus();
    let mut other_interface = host_call("Introspect", 50);
    other_interface.interface = Some("org.freedesktop.DBus.Introspectable".to_string());
    bus.push_to_client(&host_call("NoSuchMethod", 51).to_bytes());
    bus.push_to_client(&other_interface.to_bytes());
    item.process_events();
    assert!(seen.borrow().is_empty());
    let errors: Vec<_> = bus
        .received()
        .into_iter()
        .filter(|m| m.msg_type == DBUS_MESSAGE_TYPE_ERROR)
        .collect();
    assert_eq!(
        errors.iter().map(|m| m.reply_serial).collect::<Vec<_>>(),
        [Some(51), Some(50)]
    );
    for error in &errors {
        assert_eq!(
            error.error_name.as_deref(),
            Some("org.freedesktop.DBus.Error.UnknownMethod")
        );
        assert_eq!(error.destination.as_deref(), Some(":1.99"));
    }
}

// --- org.freedesktop.DBus.Properties on /StatusNotifierItem -------------------------------

const ITEM: &str = "org.kde.StatusNotifierItem";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";

/// A decoded variant value of the StatusNotifierItem properties.
#[derive(Debug, PartialEq)]
enum Value {
    Str(String),
    Path(String),
    U32(u32),
    Bool(bool),
    Pixmaps(Vec<(i32, i32, Vec<u8>)>),
    ToolTip(String, Vec<(i32, i32, Vec<u8>)>, String, String),
}

/// Little-endian D-Bus decoder over a message body (alignment relative to its first byte).
struct Dec<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Dec<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }
    fn align(&mut self, to: usize) {
        let padded = self.pos.next_multiple_of(to);
        assert!(
            self.buf[self.pos..padded].iter().all(|&b| b == 0),
            "padding must be zero"
        );
        self.pos = padded;
    }
    fn u32(&mut self) -> u32 {
        self.align(4);
        let v = u32::from_le_bytes(self.buf[self.pos..self.pos + 4].try_into().unwrap());
        self.pos += 4;
        v
    }
    fn str(&mut self) -> String {
        let len = self.u32() as usize;
        let s = String::from_utf8(self.buf[self.pos..self.pos + len].to_vec()).unwrap();
        self.pos += len;
        assert_eq!(self.buf[self.pos], 0, "strings are NUL-terminated");
        self.pos += 1;
        s
    }
    fn sig(&mut self) -> String {
        let len = self.buf[self.pos] as usize;
        let s = String::from_utf8(self.buf[self.pos + 1..self.pos + 1 + len].to_vec()).unwrap();
        assert_eq!(self.buf[self.pos + 1 + len], 0);
        self.pos += len + 2;
        s
    }
    fn pixmaps(&mut self) -> Vec<(i32, i32, Vec<u8>)> {
        let len = self.u32() as usize;
        self.align(8);
        let end = self.pos + len;
        let mut out = Vec::new();
        while self.pos < end {
            self.align(8);
            let w = self.u32() as i32;
            let h = self.u32() as i32;
            let n = self.u32() as usize;
            out.push((w, h, self.buf[self.pos..self.pos + n].to_vec()));
            self.pos += n;
        }
        assert_eq!(self.pos, end, "array length covers exactly its elements");
        out
    }
    fn variant(&mut self) -> Value {
        match self.sig().as_str() {
            "s" => Value::Str(self.str()),
            "o" => Value::Path(self.str()),
            "u" => Value::U32(self.u32()),
            "b" => Value::Bool(self.u32() != 0),
            "a(iiay)" => Value::Pixmaps(self.pixmaps()),
            "(sa(iiay)ss)" => {
                self.align(8);
                let icon = self.str();
                let pixmaps = self.pixmaps();
                let title = self.str();
                let description = self.str();
                Value::ToolTip(icon, pixmaps, title, description)
            }
            other => panic!("unexpected variant type {other}"),
        }
    }
    fn dict(&mut self) -> Vec<(String, Value)> {
        let len = self.u32() as usize;
        self.align(8);
        let end = self.pos + len;
        let mut out = Vec::new();
        while self.pos < end {
            self.align(8);
            let key = self.str();
            out.push((key, self.variant()));
        }
        assert_eq!(self.pos, end);
        out
    }
}

fn properties_call(member: &str, serial: u32, args: &[&str]) -> DbusMessage {
    let mut call = host_call(member, serial);
    call.interface = Some(PROPERTIES.to_string());
    call.signature.clear();
    call.body.clear();
    for arg in args {
        call.append_string(arg);
    }
    call
}

fn reply_to(
    bus: &ferrokit_platform::tray::dbus_connection::scripted::ScriptedDbusBus,
    serial: u32,
) -> DbusMessage {
    bus.received()
        .into_iter()
        .find(|m| m.reply_serial == Some(serial))
        .expect("the item answered")
}

fn pixmap_item() -> (
    DbusStatusNotifierItem,
    ferrokit_platform::tray::dbus_connection::scripted::ScriptedDbusBus,
) {
    use ferrokit_gui::paint::Pixmap;
    use ferrokit_gui::tiny_skia::Color;

    let (mut item, bus, _seen) = item_with_bus();
    // An icon with a small and a medium size, so that it is sent as it is (no size is added).
    let mut icon = ferrokit_gui::image::Icon::new();
    for side in [2, 24] {
        let mut pixmap = Pixmap::new(side, side).unwrap();
        pixmap.fill(Color::from_rgba8(10, 20, 30, 255));
        icon.add_pixmap(
            &pixmap,
            ferrokit_gui::image::IconMode::Normal,
            ferrokit_gui::image::IconState::Off,
        );
    }
    item.set_icon(&icon).unwrap();
    item.set_tooltip("CPU 45%").unwrap();
    (item, bus)
}

#[test]
fn get_status_replies_with_a_variant_string_byte_exactly() {
    let (mut item, bus, _seen) = item_with_bus();
    item.show().unwrap();
    bus.push_to_client(&properties_call("Get", 80, &[ITEM, "Status"]).to_bytes());
    item.process_events();
    let reply = reply_to(&bus, 80);
    assert_eq!(reply.msg_type, DBUS_MESSAGE_TYPE_METHOD_RETURN);
    assert_eq!(reply.destination.as_deref(), Some(":1.99"));
    assert_eq!(reply.signature, "v");
    // Variant signature "s", one pad byte up to the 4-aligned length, then the NUL-terminated text.
    assert_eq!(
        reply.body,
        [1, b's', 0, 0, 6, 0, 0, 0, b'A', b'c', b't', b'i', b'v', b'e', 0]
    );
}

#[test]
fn get_reports_each_property_with_its_type() {
    let (mut item, bus) = pixmap_item();
    let expected = [
        ("Category", Value::Str("ApplicationStatus".into())),
        ("Id", Value::Str("ferrokit.app".into())),
        ("Title", Value::Str("HUD".into())),
        ("Status", Value::Str("Passive".into())),
        ("WindowId", Value::U32(0)),
        ("IconThemeName", Value::Str(String::new())),
        ("OverlayIconName", Value::Str(String::new())),
        ("OverlayIconPixmap", Value::Pixmaps(Vec::new())),
        ("AttentionIconName", Value::Str(String::new())),
        ("AttentionIconPixmap", Value::Pixmaps(Vec::new())),
        ("AttentionMovieName", Value::Str(String::new())),
        (
            "ToolTip",
            Value::ToolTip(String::new(), Vec::new(), "CPU 45%".into(), String::new()),
        ),
        ("ItemIsMenu", Value::Bool(false)),
        ("Menu", Value::Path("/NO_DBUSMENU".into())),
    ];
    for (i, (name, _)) in expected.iter().enumerate() {
        bus.push_to_client(&properties_call("Get", 100 + i as u32, &[ITEM, name]).to_bytes());
    }
    item.process_events();
    for (i, (name, value)) in expected.iter().enumerate() {
        let reply = reply_to(&bus, 100 + i as u32);
        assert_eq!(reply.signature, "v", "{name}");
        let mut dec = Dec::new(&reply.body);
        assert_eq!(&dec.variant(), value, "{name}");
        assert_eq!(dec.pos, reply.body.len(), "{name}: no trailing bytes");
    }
}

#[test]
fn icon_pixmap_is_width_height_and_argb_bytes() {
    let (mut item, bus) = pixmap_item();
    bus.push_to_client(&properties_call("Get", 90, &[ITEM, "IconPixmap"]).to_bytes());
    item.process_events();
    let reply = reply_to(&bus, 90);
    let mut expected = vec![7, b'a', b'(', b'i', b'i', b'a', b'y', b')', 0];
    expected.extend_from_slice(&[0; 3]); // up to the 4-aligned array length
                                         // Two elements of 4 + 4 + 4 + bytes each; the second starts 8-aligned, after 4 padding bytes.
    let first = 12 + 2 * 2 * 4;
    let second = 12 + 24 * 24 * 4;
    expected.extend_from_slice(&((first + 4 + second) as u32).to_le_bytes());
    // 12 (sig 9 + pad 3) + 4 = 16: already 8-aligned, so the struct starts right here.
    for (side, pad) in [(2i32, 4usize), (24, 0)] {
        expected.extend_from_slice(&side.to_le_bytes());
        expected.extend_from_slice(&side.to_le_bytes());
        expected.extend_from_slice(&((side * side * 4) as u32).to_le_bytes());
        for _ in 0..side * side {
            expected.extend_from_slice(&[255, 10, 20, 30]);
        }
        expected.extend(std::iter::repeat_n(0, pad));
    }
    assert_eq!(reply.body, expected);
}

#[test]
fn empty_pixmap_arrays_still_pad_to_the_struct_alignment() {
    let (mut item, bus, _seen) = item_with_bus();
    bus.push_to_client(&properties_call("Get", 91, &[ITEM, "IconPixmap"]).to_bytes());
    item.process_events();
    let reply = reply_to(&bus, 91);
    // sig (9) + pad (3) + length (4) = 16, already aligned; nothing follows.
    assert_eq!(reply.body.len(), 16);
    assert_eq!(&reply.body[12..], [0, 0, 0, 0]);
    // Inside a dict entry the array data would start 8-aligned too: ToolTip exercises that.
    bus.push_to_client(&properties_call("Get", 92, &[ITEM, "ToolTip"]).to_bytes());
    item.process_events();
    let tip = reply_to(&bus, 92);
    let mut dec = Dec::new(&tip.body);
    assert_eq!(
        dec.variant(),
        Value::ToolTip(String::new(), Vec::new(), String::new(), String::new())
    );
}

#[test]
fn get_all_lists_every_property() {
    let (mut item, bus) = pixmap_item();
    bus.push_to_client(&properties_call("GetAll", 110, &[ITEM]).to_bytes());
    item.process_events();
    let reply = reply_to(&bus, 110);
    assert_eq!(reply.msg_type, DBUS_MESSAGE_TYPE_METHOD_RETURN);
    assert_eq!(reply.signature, "a{sv}");
    let mut dec = Dec::new(&reply.body);
    let entries = dec.dict();
    assert_eq!(dec.pos, reply.body.len());
    let names: Vec<_> = entries.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(names, DbusStatusNotifierItem::property_names());
    for name in [
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
    ] {
        assert!(names.contains(&name), "{name} is listed");
    }
    let get = |name: &str| &entries.iter().find(|(k, _)| k == name).unwrap().1;
    assert_eq!(
        get("IconPixmap"),
        &Value::Pixmaps(vec![
            (2, 2, [255, 10, 20, 30].repeat(2 * 2)),
            (24, 24, [255, 10, 20, 30].repeat(24 * 24)),
        ])
    );
    assert_eq!(get("Status"), &Value::Str("Passive".into()));
    assert_eq!(get("Menu"), &Value::Path("/NO_DBUSMENU".into()));
}

#[test]
fn property_reads_that_cannot_be_answered_are_errors() {
    use ferrokit_platform::tray::dbus_connection::DBUS_MESSAGE_TYPE_ERROR;

    let (mut item, bus, _seen) = item_with_bus();
    bus.push_to_client(&properties_call("Get", 120, &[ITEM, "NoSuchProperty"]).to_bytes());
    bus.push_to_client(&properties_call("Get", 121, &["org.example.Other", "Status"]).to_bytes());
    bus.push_to_client(&properties_call("GetAll", 122, &["org.example.Other"]).to_bytes());
    bus.push_to_client(&properties_call("Get", 123, &[ITEM]).to_bytes());
    let mut set = properties_call("Set", 124, &[ITEM, "Status"]);
    set.append_variant_u32(1);
    bus.push_to_client(&set.to_bytes());
    item.process_events();
    for serial in 120..=123 {
        let reply = reply_to(&bus, serial);
        assert_eq!(reply.msg_type, DBUS_MESSAGE_TYPE_ERROR, "{serial}");
        assert_eq!(
            reply.error_name.as_deref(),
            Some("org.freedesktop.DBus.Error.InvalidArgs"),
            "{serial}"
        );
    }
    let set_reply = reply_to(&bus, 124);
    assert_eq!(set_reply.msg_type, DBUS_MESSAGE_TYPE_ERROR);
    assert_eq!(
        set_reply.error_name.as_deref(),
        Some("org.freedesktop.DBus.Error.PropertyReadOnly")
    );
}

// --- registration with the StatusNotifierWatcher ------------------------------------------

fn register_serials(
    bus: &ferrokit_platform::tray::dbus_connection::scripted::ScriptedDbusBus,
) -> Vec<u32> {
    bus.received()
        .into_iter()
        .filter(|m| m.member.as_deref() == Some("RegisterStatusNotifierItem"))
        .map(|m| m.serial)
        .collect()
}

fn name_owner_changed(sender: &str, name: &str, old: &str, new: &str) -> DbusMessage {
    let mut signal = DbusMessage::signal(
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "NameOwnerChanged",
        200,
    );
    signal.sender = Some(sender.to_string());
    signal.append_string(name);
    signal.append_string(old);
    signal.append_string(new);
    signal
}

#[test]
fn registration_is_confirmed_by_the_watchers_reply() {
    let (mut item, bus, _seen) = item_with_bus();
    assert!(item.show().is_ok());
    // The RequestName reply arrives first; it does not register the item.
    item.process_events();
    assert!(!item.is_registered());
    let serial = register_serials(&bus)[0];
    // A reply to some other call does not confirm it.
    bus.push_to_client(&DbusMessage::method_return(serial + 1000, ":1.42", 3).to_bytes());
    item.process_events();
    assert!(!item.is_registered());
    bus.push_to_client(&DbusMessage::method_return(serial, ":1.42", 4).to_bytes());
    item.process_events();
    assert!(item.is_registered());
}

#[test]
fn registration_is_rejected_when_there_is_no_watcher() {
    let (mut item, bus, _seen) = item_with_bus();
    item.show().unwrap();
    let serial = register_serials(&bus)[0];
    bus.push_to_client(
        &DbusMessage::error(
            serial,
            ":1.42",
            3,
            "org.freedesktop.DBus.Error.ServiceUnknown",
            "The name org.kde.StatusNotifierWatcher was not provided by any .service files",
        )
        .to_bytes(),
    );
    item.process_events();
    assert!(!item.is_registered());
    // The rejection is final: a later stray return for the same serial changes nothing.
    bus.push_to_client(&DbusMessage::method_return(serial, ":1.42", 4).to_bytes());
    item.process_events();
    assert!(!item.is_registered());
}

#[test]
fn a_new_watcher_owner_registers_the_item_again() {
    let (mut item, bus, _seen) = item_with_bus();
    item.show().unwrap();
    let first = register_serials(&bus)[0];
    bus.push_to_client(&DbusMessage::method_return(first, ":1.42", 3).to_bytes());
    item.process_events();
    assert!(item.is_registered());

    // Not from the bus daemon, or about another name: ignored.
    bus.push_to_client(
        &name_owner_changed(":1.5", "org.kde.StatusNotifierWatcher", "", ":1.6").to_bytes(),
    );
    bus.push_to_client(
        &name_owner_changed("org.freedesktop.DBus", "org.other", "", ":1.6").to_bytes(),
    );
    item.process_events();
    assert!(item.is_registered());
    assert_eq!(register_serials(&bus).len(), 1);

    // The watcher goes away: no longer registered.
    bus.push_to_client(
        &name_owner_changed(
            "org.freedesktop.DBus",
            "org.kde.StatusNotifierWatcher",
            ":1.3",
            "",
        )
        .to_bytes(),
    );
    item.process_events();
    assert!(!item.is_registered());
    assert_eq!(register_serials(&bus).len(), 1);

    // A new one takes the name: the item registers again, confirmed by its own reply.
    bus.push_to_client(
        &name_owner_changed(
            "org.freedesktop.DBus",
            "org.kde.StatusNotifierWatcher",
            "",
            ":1.77",
        )
        .to_bytes(),
    );
    item.process_events();
    let serials = register_serials(&bus);
    assert_eq!(serials.len(), 2);
    assert!(!item.is_registered());
    // The old serial's reply no longer counts.
    bus.push_to_client(&DbusMessage::method_return(first, ":1.42", 5).to_bytes());
    item.process_events();
    assert!(!item.is_registered());
    bus.push_to_client(&DbusMessage::method_return(serials[1], ":1.42", 6).to_bytes());
    item.process_events();
    assert!(item.is_registered());
}

#[test]
fn calls_without_an_interface_dispatch_by_member_name() {
    let (mut item, bus, seen) = item_with_bus();
    let mut call = host_call("Activate", 60);
    call.interface = None;
    bus.push_to_client(&call.to_bytes());
    let mut unknown = host_call("Bogus", 61);
    unknown.interface = None;
    bus.push_to_client(&unknown.to_bytes());
    item.process_events();
    assert_eq!(*seen.borrow(), [TrayActivation::Trigger]);
    let kinds: Vec<_> = bus
        .received()
        .into_iter()
        .filter(|m| m.reply_serial.is_some())
        .map(|m| (m.reply_serial.unwrap(), m.msg_type))
        .collect();
    use ferrokit_platform::tray::dbus_connection::DBUS_MESSAGE_TYPE_ERROR;
    assert_eq!(
        kinds,
        [
            (60, DBUS_MESSAGE_TYPE_METHOD_RETURN),
            (61, DBUS_MESSAGE_TYPE_ERROR)
        ]
    );
}

#[test]
fn a_failed_connection_disables_the_notifier_and_keeps_the_descriptor_owner() {
    use ferrokit_core::event_loop::SocketEvent;

    let (mut item, bus, seen) = item_with_bus();
    let notifier = std::sync::Arc::new(ferrokit_core::event_loop::SocketNotifier::new(
        7,
        SocketEvent::Read,
    ));
    // Stand-in for what `bind_event_dispatcher` stores: the item must switch it off when the
    // connection fails, before anything could poll a closed descriptor.
    item.set_socket_notifier(std::sync::Arc::clone(&notifier));
    bus.close_after_queued();
    bus.push_to_client(&host_call("Activate", 70).to_bytes());
    item.process_events();
    // The call that arrived before the close is still delivered.
    assert_eq!(*seen.borrow(), [TrayActivation::Trigger]);
    // Drain until the connection reports the failure.
    for _ in 0..3 {
        item.process_events();
    }
    assert!(
        !notifier.is_enabled(),
        "the notifier is disabled once the bus is gone"
    );
    assert!(!item.is_registered());
    assert!(
        item.is_connected(),
        "the closed connection is kept, so its descriptor outlives the notifier"
    );
}

#[cfg(not(target_os = "macos"))]
mod cocoa {
    use super::*;
    use ferrokit_core::object::ThreadContext;
    use ferrokit_platform::menu::CocoaMenu;
    use ferrokit_platform::objc_runtime::appkit::MockValue;
    use ferrokit_platform::{ActionTarget, CocoaStatusItem, Id, MockObjcRuntime};

    /// `NSEventMaskLeftMouseDown | NSEventMaskRightMouseDown | NSEventMaskOtherMouseDown`.
    const ACTIVATION_MASK: i64 = (1 << 1) | (1 << 3) | (1 << 25);

    fn mouse_event(kind: i64, clicks: i64, button: i64) -> Id {
        MockObjcRuntime::instance().object_with(
            "NSEvent",
            &[
                ("type", MockValue::Int(kind)),
                ("clickCount", MockValue::Int(clicks)),
                ("buttonNumber", MockValue::Int(button)),
            ],
        )
    }

    fn handler(item: &mut CocoaStatusItem) -> Seen {
        let seen: Seen = Rc::default();
        let sink = Rc::clone(&seen);
        item.set_activation_handler(Box::new(move |reason| sink.borrow_mut().push(reason)));
        seen
    }

    fn button_of(item: &CocoaStatusItem) -> Id {
        MockObjcRuntime::instance()
            .get_object_data(item.native_status_item())
            .expect("status item")
            .button
    }

    fn menu_of(item: &CocoaStatusItem) -> Id {
        MockObjcRuntime::instance()
            .get_object_data(item.native_status_item())
            .expect("status item")
            .menu
    }

    /// The observers added for `menu`: the target ids of each `addObserver:...:object:menu`.
    fn menu_observers(menu: Id) -> Vec<Id> {
        MockObjcRuntime::instance()
            .calls()
            .into_iter()
            .filter(|call| call.selector == "addObserver:selector:name:object:")
            .filter(|call| call.args.last() == Some(&MockValue::Id(menu)))
            .map(|call| {
                assert_eq!(
                    call.args[2],
                    MockValue::Str("NSMenuDidBeginTrackingNotification".to_string())
                );
                match call.args[0] {
                    MockValue::Id(target) => target,
                    ref other => panic!("observer is not an object: {other:?}"),
                }
            })
            .collect()
    }

    /// The targets passed to `removeObserver:`.
    fn removed_observers() -> Vec<Id> {
        MockObjcRuntime::instance()
            .calls()
            .into_iter()
            .filter(|call| call.selector == "removeObserver:")
            .filter_map(|call| match call.args[0] {
                MockValue::Id(target) => Some(target),
                _ => None,
            })
            .collect()
    }

    /// One test: `[NSApp currentEvent]` is process-wide state in the mock.
    #[test]
    fn button_click_and_menu_tracking_report_the_current_events_reason() {
        ThreadContext::init_current(true, None);
        let runtime = MockObjcRuntime::instance();

        let mut item = CocoaStatusItem::new(1);
        let seen = handler(&mut item);
        let button = button_of(&item);
        let data = runtime.get_object_data(button).expect("button");
        assert!(
            !data.target.is_nil(),
            "the button sends its action to the delegate"
        );
        assert_eq!(data.action, ActionTarget::action());
        // [button sendActionOn:LeftMouseDown | RightMouseDown | OtherMouseDown]
        let masks: Vec<_> = runtime
            .calls_for(button)
            .into_iter()
            .filter(|call| call.selector == "sendActionOn:")
            .map(|call| call.args[0].clone())
            .collect();
        assert_eq!(masks, [MockValue::Int(ACTIVATION_MASK)]);

        let click = |event: Id| {
            runtime.set_current_event(event);
            assert!(ActionTarget::perform(data.target));
        };
        click(mouse_event(1, 1, 0)); // left mouse down
        click(mouse_event(3, 1, 1)); // right mouse down
        click(mouse_event(3, 1, 2)); // right mouse down with button number 2: still Right
        click(mouse_event(25, 1, 2)); // other mouse down, button 2
        click(mouse_event(1, 2, 0)); // second click of a double click
        runtime.set_current_event(Id::NIL);
        assert!(ActionTarget::perform(data.target)); // no event: Qt reads click count 0
        assert_eq!(
            *seen.borrow(),
            [
                TrayActivation::Trigger,
                TrayActivation::Context,
                TrayActivation::Context,
                TrayActivation::MiddleClick,
                TrayActivation::DoubleClick,
                TrayActivation::Trigger,
            ]
        );

        // A status item with a menu opens it instead of sending the button action; the
        // menu's tracking notification reports the activation.
        item.set_menu(Box::new(CocoaMenu::new()));
        let menu_id = menu_of(&item);
        let observers = menu_observers(menu_id);
        assert_eq!(
            observers.len(),
            1,
            "the handler was set before the menu: observed once"
        );
        let live = observers[0];
        assert!(item.is_observing_menu_tracking());
        seen.borrow_mut().clear();
        runtime.set_current_event(mouse_event(1, 1, 0));
        assert!(ActionTarget::perform(live));
        runtime.set_current_event(Id::NIL);
        assert_eq!(*seen.borrow(), [TrayActivation::Trigger]);

        // Replacing the menu removes the old observer first and observes the new menu.
        item.set_menu(Box::new(CocoaMenu::new()));
        assert_eq!(
            removed_observers().iter().filter(|&&t| t == live).count(),
            1
        );
        assert!(!ActionTarget::perform(live), "the old observer is released");
        let new_menu = menu_of(&item);
        assert_ne!(new_menu, menu_id);
        let new_observers = menu_observers(new_menu);
        assert_eq!(new_observers.len(), 1);
        let live2 = new_observers[0];
        assert!(item.is_observing_menu_tracking());

        drop(item);
        assert!(
            removed_observers().contains(&live2),
            "dropping the status item removes its observer from the notification centre"
        );
        assert!(!ActionTarget::perform(live2));
    }

    #[test]
    fn handler_set_after_the_menu_observes_it() {
        ThreadContext::init_current(true, None);
        let runtime = MockObjcRuntime::instance();
        let mut item = CocoaStatusItem::new(3);
        item.set_menu(Box::new(CocoaMenu::new()));
        let menu_id = menu_of(&item);
        assert!(menu_observers(menu_id).is_empty(), "no handler yet");
        let seen = handler(&mut item);
        let observers = menu_observers(menu_id);
        assert_eq!(observers.len(), 1);
        assert!(item.is_observing_menu_tracking());
        runtime.set_current_event(mouse_event(3, 1, 1));
        assert!(ActionTarget::perform(observers[0]));
        runtime.set_current_event(Id::NIL);
        assert_eq!(*seen.borrow(), [TrayActivation::Context]);
        // Setting the handler again keeps a single live observer.
        let _second = handler(&mut item);
        assert_eq!(
            removed_observers()
                .iter()
                .filter(|&&t| t == observers[0])
                .count(),
            1
        );
        assert_eq!(menu_observers(menu_id).len(), 2);
    }

    #[test]
    fn menu_without_a_handler_is_not_observed() {
        ThreadContext::init_current(true, None);
        let mut item = CocoaStatusItem::new(2);
        item.set_menu(Box::new(CocoaMenu::new()));
        let menu_id = menu_of(&item);
        assert!(menu_observers(menu_id).is_empty());
        assert!(!item.is_observing_menu_tracking());
    }

    /// `set_on_click` and the activation handler share the button's target: neither replaces
    /// the other, in either order.
    #[test]
    fn click_callback_and_activation_handler_coexist() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        ThreadContext::init_current(true, None);
        let runtime = MockObjcRuntime::instance();
        for handler_first in [true, false] {
            let mut item = CocoaStatusItem::new(4);
            let clicks = Arc::new(AtomicUsize::new(0));
            let counter = Arc::clone(&clicks);
            let on_click = move || {
                counter.fetch_add(1, Ordering::SeqCst);
            };
            let seen: Seen = Rc::default();
            let sink = Rc::clone(&seen);
            let activation: Box<dyn Fn(TrayActivation)> =
                Box::new(move |reason| sink.borrow_mut().push(reason));
            if handler_first {
                item.set_activation_handler(activation);
                item.set_on_click(on_click);
            } else {
                item.set_on_click(on_click);
                item.set_activation_handler(activation);
            }
            let target = runtime
                .get_object_data(button_of(&item))
                .expect("button")
                .target;
            runtime.set_current_event(mouse_event(1, 1, 0));
            assert!(ActionTarget::perform(target));
            runtime.set_current_event(Id::NIL);
            assert_eq!(
                clicks.load(Ordering::SeqCst),
                1,
                "handler_first={handler_first}"
            );
            assert_eq!(
                *seen.borrow(),
                [TrayActivation::Trigger],
                "handler_first={handler_first}"
            );
            item.trigger_click();
            assert_eq!(clicks.load(Ordering::SeqCst), 2);
        }
    }
}
