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
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetForegroundWindow, GetMessageW, GetWindowThreadProcessId, PeekMessageW,
    PostThreadMessageW, SetTimer, SetWindowsHookExW, UnhookWindowsHookEx, HC_ACTION,
    KBDLLHOOKSTRUCT, MSG, PM_REMOVE, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_CANCELMODE, WM_KEYDOWN,
    WM_LBUTTONDOWN, WM_MBUTTONDOWN, WM_QUIT, WM_RBUTTONDOWN, WM_SYSKEYDOWN, WM_TIMER,
    WM_XBUTTONDOWN,
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

/// Positive evidence that something outside this test acted on the desktop: what a low-level mouse
/// and keyboard hook saw, and changes of the foreground to another program's window. The popup
/// menu closes on exactly those, so a close without any of them is not "outside input" and is not
/// retried. What happened shortly before the attempt began counts too: a click or a foreground
/// change already on its way is delivered to the menu that opens right after it.
mod outside {
    use super::*;
    use std::sync::atomic::AtomicU64;
    use std::sync::{mpsc, LazyLock};

    const GRACE: Duration = Duration::from_millis(500);
    const MAX_NOTES: usize = 64;

    /// When each thing was seen, in milliseconds since the first call to `now_ms`.
    static START: LazyLock<Instant> = LazyLock::new(Instant::now);
    static EVIDENCE: Mutex<Vec<(u64, String)>> = Mutex::new(Vec::new());
    static ATTEMPT_STARTED: AtomicU64 = AtomicU64::new(0);
    /// The last foreground window of another process, so that the foreground returning to it
    /// from a window of this process is not a change.
    static LAST_FOREIGN_FOREGROUND: AtomicIsize = AtomicIsize::new(0);

    fn now_ms() -> u64 {
        START.elapsed().as_millis() as u64
    }

    fn note(what: String) {
        let mut evidence = EVIDENCE.lock().unwrap_or_else(|e| e.into_inner());
        if evidence.len() == MAX_NOTES {
            evidence.remove(0);
        }
        evidence.push((now_ms(), what));
    }

    /// The foreground window if it belongs to another process.
    fn foreign_foreground() -> Option<(isize, u32)> {
        let fg = unsafe { GetForegroundWindow() } as isize;
        if fg == 0 {
            return None;
        }
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(fg as _, &mut pid) };
        (pid != unsafe { GetCurrentProcessId() }).then_some((fg, pid))
    }

    fn note_foreign_foreground() {
        if let Some((fg, pid)) = foreign_foreground() {
            if LAST_FOREIGN_FOREGROUND.swap(fg, Ordering::SeqCst) != fg {
                note(format!(
                    "the foreground changed to window {fg:#x} of process {pid}"
                ));
            }
        }
    }

    unsafe extern "system" fn mouse_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code == HC_ACTION as i32
            && matches!(
                wparam as u32,
                WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN
            )
        {
            note(format!("a mouse button went down (message {wparam:#x})"));
        }
        unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
    }

    unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code == HC_ACTION as i32 && matches!(wparam as u32, WM_KEYDOWN | WM_SYSKEYDOWN) {
            let vk = unsafe { (*(lparam as *const KBDLLHOOKSTRUCT)).vkCode };
            note(format!("a key went down (virtual key {vk:#x})"));
        }
        unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
    }

    /// Watches for outside input on a thread of its own, so it sees input while this thread is
    /// inside the menu's loop.
    pub struct Watch {
        thread_id: u32,
        thread: Option<thread::JoinHandle<()>>,
    }

    impl Watch {
        pub fn start() -> Watch {
            now_ms();
            if let Some((fg, _)) = foreign_foreground() {
                LAST_FOREIGN_FOREGROUND.store(fg, Ordering::SeqCst);
            }
            let (ready, installed) = mpsc::channel();
            let thread = thread::spawn(move || unsafe {
                let mouse =
                    SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), std::ptr::null_mut(), 0);
                let keyboard =
                    SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), std::ptr::null_mut(), 0);
                // Creates this thread's message queue before the id is published.
                SetTimer(std::ptr::null_mut(), 0, 2, None);
                ready
                    .send((
                        GetCurrentThreadId(),
                        !mouse.is_null() && !keyboard.is_null(),
                    ))
                    .unwrap();
                let mut msg: MSG = std::mem::zeroed();
                while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                    if msg.message == WM_TIMER {
                        note_foreign_foreground();
                    }
                }
                for hook in [mouse, keyboard] {
                    if !hook.is_null() {
                        UnhookWindowsHookEx(hook);
                    }
                }
            });
            let (thread_id, hooked) = installed.recv().unwrap();
            if !hooked {
                // Without the hooks no input can be told apart: an early close then fails.
                eprintln!("could not install the low-level input hooks");
            }
            Watch {
                thread_id,
                thread: Some(thread),
            }
        }

        pub fn begin_attempt(&self) {
            ATTEMPT_STARTED.store(now_ms(), Ordering::SeqCst);
        }

        /// What was seen from just before the attempt began until now; empty if nothing outside
        /// acted.
        pub fn evidence(&self) -> Vec<String> {
            note_foreign_foreground();
            let since = ATTEMPT_STARTED
                .load(Ordering::SeqCst)
                .saturating_sub(GRACE.as_millis() as u64);
            EVIDENCE
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .filter(|(at, _)| *at >= since)
                .map(|(at, what)| format!("{what} at {at} ms"))
                .collect()
        }
    }

    impl Drop for Watch {
        fn drop(&mut self) {
            unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0) };
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }
}

enum Attempt {
    /// The timer and the posted event ran inside the menu's loop, in that order, before it closed.
    Delivered,
    /// The menu closed before the posted event arrived, and outside input was seen meanwhile.
    DismissedExternally {
        log: Vec<&'static str>,
        evidence: Vec<String>,
    },
}

fn run_scenario(event_loop: &EventLoop, watch: &outside::Watch) -> Attempt {
    watch.begin_attempt();
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
    let evidence = watch.evidence();
    assert!(
        !evidence.is_empty(),
        "the menu closed before the posted event arrived, with no outside input seen: {log:?}"
    );
    Attempt::DismissedExternally { log, evidence }
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
    let watch = outside::Watch::start();
    let mut dismissals = Vec::new();
    for _ in 0..MAX_ATTEMPTS {
        match run_scenario(&event_loop, &watch) {
            Attempt::Delivered => return,
            Attempt::DismissedExternally { log, evidence } => {
                eprintln!(
                    "the menu was dismissed before the posted event: {log:?}, seen: {evidence:?}"
                );
                dismissals.push((log, evidence));
            }
        }
    }
    panic!(
        "the menu was dismissed by outside input in all {MAX_ATTEMPTS} attempts, so the scenario \
         never ran: {dismissals:?}"
    );
}
