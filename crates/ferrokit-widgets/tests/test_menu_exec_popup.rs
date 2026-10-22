// SPDX-License-Identifier: LGPL-3.0-only
//! `QMenu::exec` (here `exec_popup`) triggers the chosen action and returns it, and a `WM_QUIT`
//! that arrives while the menu is open is not lost (`QMenu::exec` runs a nested event loop that
//! re-posts a quit it consumed).
#![cfg(windows)]

use ferrokit_gui::geometry::primitives::Point;
use ferrokit_widgets::Menu;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetMessageW, PeekMessageW, PostQuitMessage, PostThreadMessageW, MSG, PM_NOREMOVE, WM_KEYDOWN,
};

/// `PostThreadMessageW` needs the thread's message queue to exist; a peek creates it.
fn ensure_queue() {
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    unsafe { PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_NOREMOVE) };
}

const VK_DOWN: usize = 0x28;
const VK_RETURN: usize = 0x0D;

fn post_key(vk: usize) {
    ensure_queue();
    unsafe { PostThreadMessageW(GetCurrentThreadId(), WM_KEYDOWN, vk, 0) };
}

#[test]
fn exec_popup_triggers_the_action_it_returns() {
    let menu = Menu::new_ref("m");
    let action = menu.borrow_mut().add_new_action("Go");
    action.borrow_mut().set_checkable(true);
    let triggered = Arc::new(AtomicUsize::new(0));
    let (a, m) = (Arc::clone(&triggered), Arc::clone(&triggered));
    action.borrow().triggered.connect(move |_| {
        a.fetch_add(1, Ordering::SeqCst);
    });
    menu.borrow().triggered.connect(move |_| {
        m.fetch_add(10, Ordering::SeqCst);
    });

    post_key(VK_DOWN);
    post_key(VK_RETURN);
    let chosen = Menu::exec_popup(&menu, Point::new(50, 50));

    let chosen = chosen.expect("Return chose the first item");
    assert!(std::rc::Rc::ptr_eq(&chosen, &action));
    assert_eq!(triggered.load(Ordering::SeqCst), 11, "action and menu `triggered` both fired");
    assert!(action.borrow().is_checked(), "a checkable action toggled");
}

#[test]
fn a_quit_posted_while_the_menu_is_open_survives_it() {
    let menu = Menu::new_ref("m");
    menu.borrow_mut().add_new_action("Go");
    ensure_queue();
    unsafe { PostQuitMessage(7) };

    let chosen = Menu::exec_popup(&menu, Point::new(50, 50));

    assert!(chosen.is_none());
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    let got = unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) };
    assert_eq!(got, 0, "the quit is still pending");
    assert_eq!(msg.wParam, 7);
}
