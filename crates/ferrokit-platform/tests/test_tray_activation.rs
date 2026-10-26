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
    let mut other_interface = host_call("GetAll", 50);
    other_interface.interface = Some("org.freedesktop.DBus.Properties".to_string());
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
