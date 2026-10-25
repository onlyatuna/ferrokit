// SPDX-License-Identifier: LGPL-3.0-only
//! Delivery of a global hotkey press by `CocoaHotkeyManager`: the OS callback (here the mock
//! backend's `press`) only queues the press, and the handler runs from the owner thread's event
//! loop. Registration bookkeeping and the key tables are unit-tested in `src/hotkey.rs`.

use ferrokit_core::application::CoreApplication;
use ferrokit_platform::hotkey::cocoa_hotkey::{MockHotkeyBackend, CARBON_OPTION_KEY};
use ferrokit_platform::hotkey::{CocoaHotkeyManager, HotkeyModifiers, PlatformHotkeyManager};
use std::cell::RefCell;
use std::rc::Rc;

const KEY_A: u32 = 0x41;
const KEY_B: u32 = 0x42;
const CARBON_A: u32 = 0;
const CARBON_B: u32 = 11;

#[test]
fn press_is_delivered_from_the_event_loop_and_stops_after_unregister() {
    let _app = CoreApplication::new(Vec::new());
    let mock = MockHotkeyBackend::new();
    let mgr = Rc::new(RefCell::new(CocoaHotkeyManager::with_backend(Box::new(
        mock.clone(),
    ))));
    let log = Rc::new(RefCell::new(Vec::<u32>::new()));
    {
        let log = Rc::clone(&log);
        mgr.borrow_mut()
            .set_triggered_handler(move |id| log.borrow_mut().push(id));
    }
    {
        let mut m = mgr.borrow_mut();
        m.register_hotkey(10, HotkeyModifiers::ALT, KEY_A).unwrap();
        m.register_hotkey(20, HotkeyModifiers::ALT, KEY_B).unwrap();
    }

    // The press is queued, not handled inside the OS callback.
    assert!(mock.press(CARBON_A, CARBON_OPTION_KEY));
    assert!(log.borrow().is_empty());
    CoreApplication::process_events(false);
    assert_eq!(*log.borrow(), [10]);

    // A press with no registered hotkey is not taken.
    assert!(!mock.press(CARBON_A, 0));
    CoreApplication::process_events(false);
    assert_eq!(*log.borrow(), [10]);

    // Presses queued before an unregister are dropped; the other hotkey is unaffected.
    assert!(mock.press(CARBON_A, CARBON_OPTION_KEY));
    assert!(mock.press(CARBON_B, CARBON_OPTION_KEY));
    mgr.borrow_mut().unregister_hotkey(10).unwrap();
    CoreApplication::process_events(false);
    assert_eq!(*log.borrow(), [10, 20]);
    assert!(!mock.press(CARBON_A, CARBON_OPTION_KEY));

    // A handler may unregister (and so is not running inside the OS callback).
    {
        let log = Rc::clone(&log);
        let mgr_in_handler = Rc::clone(&mgr);
        mgr.borrow_mut().set_triggered_handler(move |id| {
            log.borrow_mut().push(id + 1000);
            mgr_in_handler.borrow_mut().unregister_hotkey(id).unwrap();
        });
    }
    assert!(mock.press(CARBON_B, CARBON_OPTION_KEY));
    CoreApplication::process_events(false);
    assert_eq!(*log.borrow(), [10, 20, 1020]);
    assert!(mock.hotkeys().is_empty());
    assert!(mgr.borrow().registered_ids().is_empty());
}

#[test]
fn dropping_the_manager_drops_queued_presses_and_its_handler() {
    let _app = CoreApplication::new(Vec::new());
    let mock = MockHotkeyBackend::new();
    let mut mgr = CocoaHotkeyManager::with_backend(Box::new(mock.clone()));
    let log = Rc::new(RefCell::new(Vec::<u32>::new()));
    {
        let log = Rc::clone(&log);
        mgr.set_triggered_handler(move |id| log.borrow_mut().push(id));
    }
    mgr.register_hotkey(1, HotkeyModifiers::ALT, KEY_A).unwrap();
    assert!(mock.press(CARBON_A, CARBON_OPTION_KEY));
    drop(mgr);
    assert!(mock.hotkeys().is_empty());
    CoreApplication::process_events(false);
    assert!(log.borrow().is_empty());
    // The handler closure was released with the manager (only `log` here still holds it).
    assert_eq!(Rc::strong_count(&log), 1);
}

#[test]
fn press_without_a_handler_is_ignored() {
    let _app = CoreApplication::new(Vec::new());
    let mock = MockHotkeyBackend::new();
    let mut mgr = CocoaHotkeyManager::with_backend(Box::new(mock.clone()));
    mgr.register_hotkey(1, HotkeyModifiers::ALT, KEY_A).unwrap();
    assert!(mock.press(CARBON_A, CARBON_OPTION_KEY));
    CoreApplication::process_events(false);
    mgr.set_triggered_handler(|_| panic!("a press from before the handler existed"));
    mgr.clear_triggered_handler();
    assert!(mock.press(CARBON_A, CARBON_OPTION_KEY));
    CoreApplication::process_events(false);
}
