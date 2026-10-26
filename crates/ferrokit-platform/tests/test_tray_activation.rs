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

/// The non-Windows `TrayIcon` forwards the back end's activations to `on_activated`.
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

#[cfg(not(target_os = "macos"))]
mod cocoa {
    use super::*;
    use ferrokit_core::object::ThreadContext;
    use ferrokit_platform::menu::CocoaMenu;
    use ferrokit_platform::objc_runtime::appkit::MockValue;
    use ferrokit_platform::{ActionTarget, CocoaStatusItem, Id, MockObjcRuntime};

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

    /// One test: `[NSApp currentEvent]` is process-wide state in the mock.
    #[test]
    fn button_click_and_menu_tracking_report_the_current_events_reason() {
        ThreadContext::init_current(true, None);
        let runtime = MockObjcRuntime::instance();

        let mut item = CocoaStatusItem::new(1);
        let seen = handler(&mut item);
        let button = runtime
            .get_object_data(item.native_status_item())
            .expect("status item")
            .button;
        let data = runtime.get_object_data(button).expect("button");
        assert!(
            !data.target.is_nil(),
            "the button sends its action to the delegate"
        );
        assert_eq!(data.action, ActionTarget::action());

        let click = |event: Id| {
            runtime.set_current_event(event);
            assert!(ActionTarget::perform(data.target));
        };
        click(mouse_event(1, 1, 0)); // left mouse down
        click(mouse_event(3, 1, 1)); // right mouse down
        click(mouse_event(25, 1, 2)); // other mouse down, button 2
        click(mouse_event(1, 2, 0)); // second click of a double click
        runtime.set_current_event(Id::NIL);
        assert!(ActionTarget::perform(data.target)); // no event: Qt reads click count 0
        assert_eq!(
            *seen.borrow(),
            [
                TrayActivation::Trigger,
                TrayActivation::Context,
                TrayActivation::MiddleClick,
                TrayActivation::DoubleClick,
                TrayActivation::Trigger,
            ]
        );

        // A status item with a menu opens it instead of sending the button action; the
        // menu's tracking notification reports the activation.
        let menu = Box::new(CocoaMenu::new());
        item.set_menu(menu);
        let menu_id = runtime
            .get_object_data(item.native_status_item())
            .expect("status item")
            .menu;
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

        drop(item);
        assert!(
            !ActionTarget::perform(live),
            "dropping the status item releases its observer"
        );
    }

    #[test]
    fn menu_without_a_handler_is_not_observed() {
        ThreadContext::init_current(true, None);
        let runtime = MockObjcRuntime::instance();
        let mut item = CocoaStatusItem::new(2);
        item.set_menu(Box::new(CocoaMenu::new()));
        let menu_id = runtime
            .get_object_data(item.native_status_item())
            .expect("status item")
            .menu;
        assert!(menu_observers(menu_id).is_empty());
        assert!(!item.is_observing_menu_tracking());
    }
}
