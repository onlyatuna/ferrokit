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
use windows_sys::Win32::UI::WindowsAndMessaging::{PostThreadMessageW, WM_CANCELMODE};

fn meta_call(f: impl FnOnce() + Send + 'static) -> Event {
    Event::new(EventKind::MetaCall(Box::new(move |_| f())))
}

fn cancel_menu(thread_id: u32) {
    unsafe { PostThreadMessageW(thread_id, WM_CANCELMODE, 0, 0) };
}

#[test]
fn events_posted_while_a_menu_is_open_are_delivered_before_it_closes() {
    let event_loop = EventLoop::new();
    let main_thread = ThreadId::current();
    let native_thread = unsafe { GetCurrentThreadId() };
    let log = Arc::new(Mutex::new(Vec::new()));
    let menu_closed = Arc::new(AtomicBool::new(false));

    // Without delivery inside the menu nothing would close it; give up after a while.
    {
        let menu_closed = Arc::clone(&menu_closed);
        thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline {
                if menu_closed.load(Ordering::SeqCst) {
                    return;
                }
                thread::sleep(Duration::from_millis(20));
            }
            cancel_menu(native_thread);
        });
    }

    {
        let log = Arc::clone(&log);
        let menu_closed = Arc::clone(&menu_closed);
        Timer::single_shot(0, move || {
            let log_timer = Arc::clone(&log);
            Timer::single_shot(50, move || log_timer.lock().unwrap().push("timer"));

            // A global hotkey: another thread posts to the main thread while the menu is open.
            let log_posted = Arc::clone(&log);
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(300));
                post_event_to_thread(
                    main_thread,
                    ObjectId(0),
                    meta_call(move || {
                        log_posted.lock().unwrap().push("posted");
                        cancel_menu(unsafe { GetCurrentThreadId() });
                    }),
                );
            });

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
    assert_eq!(*log.lock().unwrap(), vec!["timer", "posted", "menu closed"]);
}
