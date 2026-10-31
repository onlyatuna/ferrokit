// SPDX-License-Identifier: LGPL-3.0-only
#![cfg(windows)]

use ferrokit_gui::geometry::primitives::Point;
use ferrokit_widgets::{Action, Menu, Widget};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowThreadProcessId, PostMessageW, WM_ACTIVATEAPP, WM_CANCELMODE,
    WM_CAPTURECHANGED, WM_KILLFOCUS,
};

static POPUP_TEST_LOCK: Mutex<()> = Mutex::new(());

unsafe extern "system" fn find_process_window(hwnd: HWND, lparam: isize) -> i32 {
    let mut pid = 0;
    GetWindowThreadProcessId(hwnd, &mut pid);
    if pid == std::process::id() {
        let out = lparam as *mut HWND;
        *out = hwnd;
        return 0; // stop enumeration
    }
    1 // continue
}

fn get_popup_hwnd() -> HWND {
    let mut found = 0 as HWND;
    for _ in 0..50 {
        thread::sleep(Duration::from_millis(10));
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

#[test]
fn test_popup_exits_on_kill_focus() {
    let _guard = POPUP_TEST_LOCK.lock();
    let mut menu = Menu::new("TestMenu");
    let act = Action::new_ref("Action 1");
    menu.add_action(act);

    let finished = Arc::new(AtomicBool::new(false));
    let finished_clone = Arc::clone(&finished);

    let bg = thread::spawn(move || {
        let hwnd = get_popup_hwnd();
        assert_ne!(hwnd, 0 as HWND, "Popup window should be found");
        unsafe {
            PostMessageW(hwnd, WM_KILLFOCUS, 0, 0);
        }
    });

    let chosen = Menu::exec_popup(
        &std::rc::Rc::new(std::cell::RefCell::new(menu)),
        Point::new(100, 100),
    );
    finished_clone.store(true, Ordering::SeqCst);
    assert!(chosen.is_none());

    bg.join().expect("Background thread failed");
    assert!(finished.load(Ordering::SeqCst));
}

#[test]
fn test_popup_exits_on_activateapp_deactivated() {
    let _guard = POPUP_TEST_LOCK.lock();
    let mut menu = Menu::new("TestMenu");
    let act = Action::new_ref("Action 1");
    menu.add_action(act);

    let bg = thread::spawn(move || {
        let hwnd = get_popup_hwnd();
        assert_ne!(hwnd, 0 as HWND, "Popup window should be found");
        unsafe {
            // wParam = 0 (FALSE / Deactivated)
            PostMessageW(hwnd, WM_ACTIVATEAPP, 0, 0);
        }
    });

    let chosen = Menu::exec_popup(
        &std::rc::Rc::new(std::cell::RefCell::new(menu)),
        Point::new(100, 100),
    );
    assert!(chosen.is_none());

    bg.join().expect("Background thread failed");
}

#[test]
fn test_popup_exits_on_capture_changed() {
    let _guard = POPUP_TEST_LOCK.lock();
    let mut menu = Menu::new("TestMenu");
    let act = Action::new_ref("Action 1");
    menu.add_action(act);

    let bg = thread::spawn(move || {
        let hwnd = get_popup_hwnd();
        assert_ne!(hwnd, 0 as HWND, "Popup window should be found");
        unsafe {
            // lParam = 0 (another window took capture)
            PostMessageW(hwnd, WM_CAPTURECHANGED, 0, 0);
        }
    });

    let chosen = Menu::exec_popup(
        &std::rc::Rc::new(std::cell::RefCell::new(menu)),
        Point::new(100, 100),
    );
    assert!(chosen.is_none());

    bg.join().expect("Background thread failed");
}

#[test]
fn test_popup_exits_on_cancel_mode() {
    let _guard = POPUP_TEST_LOCK.lock();
    let mut menu = Menu::new("TestMenu");
    let act = Action::new_ref("Action 1");
    menu.add_action(act);

    let bg = thread::spawn(move || {
        let hwnd = get_popup_hwnd();
        assert_ne!(hwnd, 0 as HWND, "Popup window should be found");
        unsafe {
            PostMessageW(hwnd, WM_CANCELMODE, 0, 0);
        }
    });

    let chosen = Menu::exec_popup(
        &std::rc::Rc::new(std::cell::RefCell::new(menu)),
        Point::new(100, 100),
    );
    assert!(chosen.is_none());

    bg.join().expect("Background thread failed");
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
