// SPDX-License-Identifier: LGPL-3.0-only
//! A popup menu's loop is a nested event loop: `QMenu::exec` runs a `QEventLoop`
//! (qmenu.cpp:2691-2697), so events posted while the menu is open — an application's hotkey and theme
//! `run_on_main_thread` calls, repaint requests — are delivered before `exec` returns. An
//! application can open its menu from `Timer::single_shot(0)`, i.e. from inside the posted-event pump.
#![cfg(windows)]

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::event_loop::{post_event_to_thread, EventLoop};
use ferrokit_core::object::{ObjectId, ThreadId};
use ferrokit_core::timer::Timer;
use ferrokit_gui::geometry::primitives::Point;
use ferrokit_widgets::{Action, Menu};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    PeekMessageW, PostThreadMessageW, MSG, PM_REMOVE, WM_CANCELMODE,
};

fn meta_call(f: impl FnOnce() + Send + 'static) -> Event {
    Event::new(EventKind::MetaCall(Box::new(move |_| f())))
}

fn cancel_menu(thread_id: u32) {
    unsafe { PostThreadMessageW(thread_id, WM_CANCELMODE, 0, 0) };
}

/// A popup menu holds the mouse capture, so on a shared desktop any real click (or Escape) made
/// anywhere while it is open dismisses it, as it would in Qt. A dismissal like that is not what this
/// test is about, but it ends the scenario before the posted event can be delivered; the scenario
/// is run again. The posted event being lost while the menu is open is a failure.
const MAX_ATTEMPTS: usize = 8;

/// The menu stays open at most this long without the test closing it: the posted event that
/// closes it was never delivered.
const GIVE_UP_AFTER: Duration = Duration::from_secs(3);

enum Attempt {
    /// The timer and the posted event ran inside the menu's loop, in that order, before it closed.
    Delivered,
    /// Something other than this test closed the menu before the posted event arrived.
    DismissedExternally(Vec<&'static str>),
}

fn run_scenario(event_loop: &EventLoop) -> Attempt {
    let main_thread = ThreadId::current();
    let native_thread = unsafe { GetCurrentThreadId() };
    let log: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
    let menu_closed = Arc::new(AtomicBool::new(false));
    let gave_up = Arc::new(AtomicBool::new(false));
    let mut helpers = Vec::new();

    // Without delivery inside the menu nothing would close it; give up after a while.
    {
        let menu_closed = Arc::clone(&menu_closed);
        let gave_up = Arc::clone(&gave_up);
        helpers.push(thread::spawn(move || {
            let deadline = Instant::now() + GIVE_UP_AFTER;
            while Instant::now() < deadline {
                if menu_closed.load(Ordering::SeqCst) {
                    return;
                }
                thread::sleep(Duration::from_millis(5));
            }
            gave_up.store(true, Ordering::SeqCst);
            cancel_menu(native_thread);
        }));
    }

    // A global hotkey: another thread posts to the main thread while the menu is open, as soon as
    // the timer has run inside the menu's loop (so the order is observable).
    {
        let log = Arc::clone(&log);
        let menu_closed = Arc::clone(&menu_closed);
        helpers.push(thread::spawn(move || {
            let deadline = Instant::now() + GIVE_UP_AFTER;
            while Instant::now() < deadline
                && !menu_closed.load(Ordering::SeqCst)
                && !log.lock().unwrap().contains(&"timer")
            {
                thread::sleep(Duration::from_millis(2));
            }
            post_event_to_thread(
                main_thread,
                ObjectId(0),
                meta_call(move || {
                    // Delivered after the menu was dismissed: it was not posted "while open".
                    if menu_closed.load(Ordering::SeqCst) {
                        log.lock().unwrap().push("posted after close");
                        return;
                    }
                    log.lock().unwrap().push("posted");
                    cancel_menu(unsafe { GetCurrentThreadId() });
                }),
            );
        }));
    }

    {
        let log = Arc::clone(&log);
        let menu_closed = Arc::clone(&menu_closed);
        Timer::single_shot(0, move || {
            let log_timer = Arc::clone(&log);
            Timer::single_shot(50, move || log_timer.lock().unwrap().push("timer"));

            let mut menu = Menu::new("PostedEventsTest");
            menu.add_action(Action::new_ref("First"));
            assert!(menu.exec_popup(Point::new(100, 100)).is_none());
            menu_closed.store(true, Ordering::SeqCst);
            log.lock().unwrap().push("menu closed");
        });
    }
    event_loop.send_posted_events();
    // Whatever the menu left behind runs now.
    event_loop.send_posted_events();

    for helper in helpers {
        helper.join().unwrap();
    }
    // The give-up cancel, or one that outlived its menu, must not close the next attempt's menu.
    drain_cancel_messages();

    let log = log.lock().unwrap().clone();
    if log == ["timer", "posted", "menu closed"] {
        return Attempt::Delivered;
    }
    assert!(
        !gave_up.load(Ordering::SeqCst),
        "the event posted while the menu was open was never delivered to the thread: {log:?}"
    );
    assert!(
        !log.contains(&"posted"),
        "the posted event ran out of order or at the wrong time: {log:?}"
    );
    Attempt::DismissedExternally(log)
}

/// Removes the `WM_CANCELMODE`s still queued for this thread.
fn drain_cancel_messages() {
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        while PeekMessageW(
            &mut msg,
            std::ptr::null_mut(),
            WM_CANCELMODE,
            WM_CANCELMODE,
            PM_REMOVE,
        ) != 0
        {}
    }
}

#[test]
fn events_posted_while_a_menu_is_open_are_delivered_before_it_closes() {
    let event_loop = EventLoop::new();
    let mut dismissals = Vec::new();
    for _ in 0..MAX_ATTEMPTS {
        match run_scenario(&event_loop) {
            Attempt::Delivered => return,
            Attempt::DismissedExternally(log) => {
                eprintln!(
                    "the menu was dismissed by outside input before the posted event: {log:?}"
                );
                dismissals.push(log);
            }
        }
    }
    panic!(
        "the menu was dismissed by outside input (a click or Escape on the desktop) in all \
         {MAX_ATTEMPTS} attempts, so the scenario never ran: {dismissals:?}"
    );
}
