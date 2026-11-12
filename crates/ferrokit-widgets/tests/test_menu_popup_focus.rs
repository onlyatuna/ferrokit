// SPDX-License-Identifier: LGPL-3.0-only
#![cfg(windows)]

use ferrokit_gui::geometry::primitives::Point;
use ferrokit_widgets::{Action, Menu, Widget};
use parking_lot::Mutex;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowThreadProcessId, SendMessageW, WM_KILLFOCUS, WM_SETFOCUS,
};

static POPUP_TEST_LOCK: Mutex<()> = Mutex::new(());

unsafe extern "system" fn find_process_window(hwnd: HWND, lparam: isize) -> i32 {
    // SAFETY: lparam is a valid pointer to HWND passed from EnumWindows; hwnd is provided by system.
    unsafe {
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid == std::process::id() {
            let out = lparam as *mut HWND;
            *out = hwnd;
            return 0; // stop enumeration
        }
        1 // continue
    }
}

fn get_popup_hwnd() -> HWND {
    let mut found = 0 as HWND;
    // The first popup of a process can take long (font and window set-up).
    for _ in 0..1000 {
        thread::sleep(Duration::from_millis(10));
        // SAFETY: `EnumWindows` calls `find_process_window` synchronously, before it returns. The
        // `lparam` passed is the address of the live local `found`, whose type (`HWND`) is what
        // the callback casts it back to and writes; `found` is not otherwise accessed during the
        // call.
        unsafe {
            EnumWindows(Some(find_process_window), &mut found as *mut _ as isize);
        }
        if found != 0 as HWND {
            return found;
        }
    }
    found
}

/// `Menu::popup` needs a shared menu (`aboutToShow` runs with it free); these tests drive an
/// owned one, so hand it over for the popup and take it back.
fn popped(menu: Menu, pos: Point) -> Menu {
    let shared = std::rc::Rc::new(std::cell::RefCell::new(menu));
    Menu::popup(&shared, pos);
    match std::rc::Rc::try_unwrap(shared) {
        Ok(cell) => cell.into_inner(),
        Err(_) => unreachable!("the popup keeps no handle to its menu"),
    }
}

/// Losing the focus reaches the popup as a *sent* message, which `GetMessageW` dispatches to the
/// window procedure without returning it to the loop: the window's deactivation is what ends the
/// loop. `SendMessageW` from another thread is the same kind of delivery as the system's.
#[test]
fn test_popup_exits_on_kill_focus() {
    let _guard = POPUP_TEST_LOCK.lock();
    let mut menu = Menu::new("TestMenu");
    let act = Action::new_ref("Action 1");
    menu.add_action(act);

    // SAFETY: `GetCurrentThreadId` takes no arguments and cannot fail.
    let ui_thread = unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() };
    let bg = thread::spawn(move || {
        let hwnd = get_popup_hwnd();
        if hwnd == 0 as HWND {
            // Close the menu the way Escape does, so that the failure is a failure, not a hang.
            // SAFETY: `PostThreadMessageW` takes integers only; a post to a thread that is gone
            // only fails.
            unsafe {
                windows_sys::Win32::UI::WindowsAndMessaging::PostThreadMessageW(
                    ui_thread,
                    windows_sys::Win32::UI::WindowsAndMessaging::WM_KEYDOWN,
                    windows_sys::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE as usize,
                    0,
                );
            }
            panic!("Popup window should be found");
        }
        // SAFETY: `SendMessageW` takes a handle and integers, no pointer; `WM_SETFOCUS` and
        // `WM_KILLFOCUS` take `wParam` = 0 (no other window involved). `hwnd` is a window of this
        // process found by `get_popup_hwnd` (asserted non-null); the call returns once the
        // popup's thread has handled the message in its loop, or at once if the window has been
        // destroyed meanwhile.
        unsafe {
            SendMessageW(hwnd, WM_SETFOCUS, 0, 0);
            SendMessageW(hwnd, WM_KILLFOCUS, 0, 0);
        }
    });

    let chosen = Menu::exec_popup(
        &std::rc::Rc::new(std::cell::RefCell::new(menu)),
        Point::new(100, 100),
    );
    assert!(chosen.is_none());

    bg.join().expect("Background thread failed");
}

/// `QCoreApplication::exit` ends every running `QEventLoop` of the thread, the one `QMenu::exec`
/// runs included (qcoreapplication.cpp:1564-1568). An exit raised while the popup's loop runs
/// (here from a timer, which that loop dispatches) must end it without any input.
#[test]
fn test_popup_exits_when_the_application_exits() {
    use ferrokit_core::application::CoreApplication;
    use ferrokit_core::timer::Timer;
    use std::time::Instant;
    use windows_sys::Win32::System::Threading::GetCurrentThreadId;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE;
    use windows_sys::Win32::UI::WindowsAndMessaging::{PostThreadMessageW, WM_KEYDOWN};

    let _guard = POPUP_TEST_LOCK.lock();
    CoreApplication::reset_for_test();
    let mut app = CoreApplication::new(vec!["popup_exit".to_string()]);
    // SAFETY: `GetCurrentThreadId` takes no arguments and cannot fail.
    let ui_thread = unsafe { GetCurrentThreadId() };

    // Without the fix nothing closes the menu: Escape does, after a while, so that the test fails
    // instead of hanging.
    let escaped = Arc::new(AtomicBool::new(false));
    let finished = Arc::new(AtomicBool::new(false));
    let watchdog = {
        let (escaped, finished) = (Arc::clone(&escaped), Arc::clone(&finished));
        thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(4);
            while Instant::now() < deadline {
                if finished.load(Ordering::SeqCst) {
                    return;
                }
                thread::sleep(Duration::from_millis(10));
            }
            escaped.store(true, Ordering::SeqCst);
            // SAFETY: `PostThreadMessageW` takes integers only (the thread id, `WM_KEYDOWN`,
            // `VK_ESCAPE`, zero), no pointer; a post to a thread that is gone only fails.
            unsafe { PostThreadMessageW(ui_thread, WM_KEYDOWN, VK_ESCAPE as usize, 0) };
        })
    };

    let popup_returned = Arc::new(AtomicBool::new(false));
    let returned = Arc::clone(&popup_returned);
    Timer::single_shot(0, move || {
        Timer::single_shot(100, || CoreApplication::exit(7));
        let mut menu = Menu::new("TestMenu");
        menu.add_action(Action::new_ref("Action 1"));
        let chosen = Menu::exec_popup(
            &std::rc::Rc::new(std::cell::RefCell::new(menu)),
            Point::new(100, 100),
        );
        assert!(chosen.is_none());
        returned.store(true, Ordering::SeqCst);
    });
    let code = app.exec();
    finished.store(true, Ordering::SeqCst);
    watchdog.join().expect("watchdog thread");

    assert!(popup_returned.load(Ordering::SeqCst));
    assert!(
        !escaped.load(Ordering::SeqCst),
        "the exit did not end the popup: only the watchdog's Escape did"
    );
    assert_eq!(code, 7);
}

#[test]
fn test_submenu_flips_left_when_near_right_screen_edge() {
    let mut menu = Menu::new("RootMenu");
    let mut sub = Menu::new("SubMenu");
    sub.add_action(Action::new_ref("SubItem 1"));
    let act = Action::new_ref("HasSub");
    act.borrow_mut()
        .set_menu(Some(std::rc::Rc::new(std::cell::RefCell::new(sub))));
    menu.add_action(act);

    // Root menu placed near right edge of 1920 screen (e.g. at x = 1700)
    // popup_bounds is available geometry relative to root_origin
    let s_geom = ferrokit_gui::geometry::primitives::Rect::new(0, 0, 1920, 1040);
    let root_origin = Point::new(1700, 500);
    menu.set_popup_bounds(Some(s_geom.translated(-root_origin.x, -root_origin.y)));
    let mut menu = popped(menu, Point::new(0, 0));

    // Open submenu at index 0
    menu.handle_mouse_move_at(Point::new(10, 10));
    assert!(menu.open_submenu().is_some());
    let sub_ref = menu.open_submenu().unwrap();
    let sub_geom = sub_ref.borrow().geometry();

    // Since 1700 + root_w + sub_w > 1920, the submenu must flip to the LEFT of the root menu (negative x)
    assert!(
        sub_geom.x < 0,
        "Submenu must open to the left (negative x) when near right screen edge, got x={}",
        sub_geom.x
    );

    // The covered_rect() must include the left-expanded bounds
    let covered = menu.covered_rect();
    assert!(
        covered.x < 0,
        "Covered rect x must be negative to cover left submenu"
    );
}

#[test]
fn test_window_present_custom_at_updates_geometry_and_backing_store() {
    use ferrokit_gui::geometry::primitives::Rect;
    use ferrokit_gui::paint::PaintDevice;
    use ferrokit_platform::WindowFlags;
    use ferrokit_widgets::window::Window;
    let mut win = Window::new(
        "TestPopup",
        Rect::new(100, 100, 200, 300),
        WindowFlags::FRAMELESS | WindowFlags::TOOL | WindowFlags::LAYERED,
    )
    .expect("Window created");

    assert_eq!(win.geometry(), Rect::new(100, 100, 200, 300));

    // Present custom at a new rect (simulating left-expanded submenu)
    let new_rect = Rect::new(50, 100, 250, 300);
    win.present_custom_at(new_rect, |_painter| {});

    assert_eq!(win.geometry(), new_rect);
    assert_eq!(win.backing_store().width(), 250.0);
    assert_eq!(win.backing_store().height(), 300.0);
}
