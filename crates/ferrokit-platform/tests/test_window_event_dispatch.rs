// SPDX-License-Identifier: LGPL-3.0-only
//! The per-thread window event dispatcher: re-entrancy, panic safety, queue ownership.
//! Handlers are keyed by HWND value only, so fake handles exercise it without real windows.
#![cfg(windows)]

use ferrokit_platform::window::{
    flush_window_system_events, post_window_system_event, send_window_system_event_immediately,
    set_window_event_handler, unregister_window_event_binding,
};
use ferrokit_platform::window_system_interface::{ClosureWindowEventHandler, WindowSystemEvent};
use std::cell::RefCell;
use std::rc::Rc;
use windows_sys::Win32::Foundation::HWND;

fn fake(n: usize) -> HWND {
    n as HWND
}

type Log = Rc<RefCell<Vec<(usize, &'static str)>>>;

fn name(ev: &WindowSystemEvent) -> &'static str {
    match ev {
        WindowSystemEvent::FocusIn => "in",
        WindowSystemEvent::FocusOut => "out",
        WindowSystemEvent::CloseRequest => "close",
        _ => "other",
    }
}

fn recorder(
    log: &Log,
    id: usize,
) -> Box<ClosureWindowEventHandler<impl FnMut(WindowSystemEvent) + use<>>> {
    let log = Rc::clone(log);
    Box::new(ClosureWindowEventHandler::new(move |ev| {
        log.borrow_mut().push((id, name(&ev)));
    }))
}

#[test]
fn a_panicking_handler_does_not_wedge_later_dispatch() {
    let log: Log = Default::default();
    set_window_event_handler(
        fake(0x1001),
        Box::new(ClosureWindowEventHandler::new(|_| panic!("handler raised"))),
    );
    set_window_event_handler(fake(0x1002), recorder(&log, 2));

    let r = std::panic::catch_unwind(|| {
        send_window_system_event_immediately(fake(0x1001), WindowSystemEvent::FocusIn)
    });
    assert!(r.is_err(), "the handler's panic propagates to the caller");

    send_window_system_event_immediately(fake(0x1002), WindowSystemEvent::FocusIn);
    assert_eq!(*log.borrow(), vec![(2, "in")]);
    unregister_window_event_binding(fake(0x1001));
    unregister_window_event_binding(fake(0x1002));
}

#[test]
fn a_handler_that_panicked_stays_registered() {
    let calls = Rc::new(RefCell::new(0));
    let c = Rc::clone(&calls);
    set_window_event_handler(
        fake(0x1011),
        Box::new(ClosureWindowEventHandler::new(move |_| {
            *c.borrow_mut() += 1;
            if *c.borrow() == 1 {
                panic!("first call raises");
            }
        })),
    );
    let _ = std::panic::catch_unwind(|| {
        send_window_system_event_immediately(fake(0x1011), WindowSystemEvent::FocusIn)
    });
    send_window_system_event_immediately(fake(0x1011), WindowSystemEvent::FocusIn);
    assert_eq!(*calls.borrow(), 2);
    unregister_window_event_binding(fake(0x1011));
}

#[test]
fn a_handler_may_register_another_window_handler() {
    let log: Log = Default::default();
    let l = Rc::clone(&log);
    set_window_event_handler(
        fake(0x1021),
        Box::new(ClosureWindowEventHandler::new(move |_| {
            // `Window::new` from inside an event handler (open a dialog on click).
            set_window_event_handler(fake(0x1022), recorder(&l, 22));
        })),
    );
    send_window_system_event_immediately(fake(0x1021), WindowSystemEvent::FocusIn);
    send_window_system_event_immediately(fake(0x1022), WindowSystemEvent::FocusIn);
    assert_eq!(*log.borrow(), vec![(22, "in")]);
    unregister_window_event_binding(fake(0x1021));
    unregister_window_event_binding(fake(0x1022));
}

#[test]
fn a_handler_may_unregister_its_own_window() {
    let log: Log = Default::default();
    let l = Rc::clone(&log);
    set_window_event_handler(
        fake(0x1031),
        Box::new(ClosureWindowEventHandler::new(move |ev| {
            l.borrow_mut().push((31, name(&ev)));
            // A close-request handler that destroys its window (WM_DESTROY unregisters).
            unregister_window_event_binding(fake(0x1031));
        })),
    );
    send_window_system_event_immediately(fake(0x1031), WindowSystemEvent::CloseRequest);
    // The window is gone: a later event finds no handler.
    send_window_system_event_immediately(fake(0x1031), WindowSystemEvent::FocusIn);
    assert_eq!(*log.borrow(), vec![(31, "close")]);
}

#[test]
fn a_handler_may_replace_its_own_handler() {
    let log: Log = Default::default();
    let l = Rc::clone(&log);
    set_window_event_handler(
        fake(0x1041),
        Box::new(ClosureWindowEventHandler::new(move |_| {
            set_window_event_handler(fake(0x1041), recorder(&l, 42));
        })),
    );
    send_window_system_event_immediately(fake(0x1041), WindowSystemEvent::FocusIn);
    send_window_system_event_immediately(fake(0x1041), WindowSystemEvent::FocusOut);
    assert_eq!(*log.borrow(), vec![(42, "out")]);
    unregister_window_event_binding(fake(0x1041));
}

#[test]
fn a_nested_event_for_the_same_window_runs_after_the_handler_returns() {
    let log: Log = Default::default();
    let l = Rc::clone(&log);
    set_window_event_handler(
        fake(0x1051),
        Box::new(ClosureWindowEventHandler::new(move |ev| {
            l.borrow_mut().push((51, name(&ev)));
            if matches!(ev, WindowSystemEvent::FocusIn) {
                send_window_system_event_immediately(fake(0x1051), WindowSystemEvent::FocusOut);
                l.borrow_mut().push((51, "in-done"));
            }
        })),
    );
    send_window_system_event_immediately(fake(0x1051), WindowSystemEvent::FocusIn);
    assert_eq!(
        *log.borrow(),
        vec![(51, "in"), (51, "in-done"), (51, "out")]
    );
    unregister_window_event_binding(fake(0x1051));
}

#[test]
fn a_nested_event_for_another_window_is_delivered_at_once() {
    // A modal loop started from a handler (QDialog::exec) must keep feeding the dialog.
    let log: Log = Default::default();
    set_window_event_handler(fake(0x1062), recorder(&log, 62));
    let l = Rc::clone(&log);
    set_window_event_handler(
        fake(0x1061),
        Box::new(ClosureWindowEventHandler::new(move |_| {
            send_window_system_event_immediately(fake(0x1062), WindowSystemEvent::FocusIn);
            l.borrow_mut().push((61, "after-nested"));
        })),
    );
    send_window_system_event_immediately(fake(0x1061), WindowSystemEvent::FocusIn);
    assert_eq!(*log.borrow(), vec![(62, "in"), (61, "after-nested")]);
    unregister_window_event_binding(fake(0x1061));
    unregister_window_event_binding(fake(0x1062));
}

#[test]
fn unregistering_a_window_purges_its_posted_events() {
    let log: Log = Default::default();
    set_window_event_handler(fake(0x1071), recorder(&log, 71));
    post_window_system_event(fake(0x1071), WindowSystemEvent::CloseRequest);
    unregister_window_event_binding(fake(0x1071));
    // A recycled handle must not receive the destroyed window's events.
    set_window_event_handler(fake(0x1071), recorder(&log, 72));
    flush_window_system_events();
    assert!(log.borrow().is_empty(), "got {:?}", log.borrow());
    unregister_window_event_binding(fake(0x1071));
}

#[test]
fn a_replaced_then_unregistered_handler_does_not_come_back() {
    let log: Log = Default::default();
    let l = Rc::clone(&log);
    set_window_event_handler(
        fake(0x1091),
        Box::new(ClosureWindowEventHandler::new(move |_| {
            l.borrow_mut().push((90, "old handler ran"));
            // Replace itself, then the window dies: neither handler may survive.
            set_window_event_handler(fake(0x1091), recorder(&l, 91));
            unregister_window_event_binding(fake(0x1091));
        })),
    );
    send_window_system_event_immediately(fake(0x1091), WindowSystemEvent::FocusIn);
    send_window_system_event_immediately(fake(0x1091), WindowSystemEvent::FocusOut);
    assert_eq!(
        *log.borrow(),
        vec![(90, "old handler ran")],
        "only the first event is handled"
    );
}

#[test]
fn flushing_drops_events_for_a_window_nobody_handles() {
    let log: Log = Default::default();
    post_window_system_event(fake(0x10A1), WindowSystemEvent::CloseRequest);
    assert!(!flush_window_system_events(), "nothing to deliver");
    // A handler registered later must not receive the orphaned event.
    set_window_event_handler(fake(0x10A1), recorder(&log, 0xA1));
    flush_window_system_events();
    assert!(log.borrow().is_empty(), "got {:?}", log.borrow());
    unregister_window_event_binding(fake(0x10A1));
}
