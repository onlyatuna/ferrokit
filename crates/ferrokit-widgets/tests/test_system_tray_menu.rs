// SPDX-License-Identifier: LGPL-3.0-only
//! Tests for `SystemTrayIcon` (`QSystemTrayIcon`) context menu integration.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

#[cfg(not(windows))]
use ferrokit_gui::geometry::primitives::Point;
use ferrokit_gui::image::Icon;
use ferrokit_gui::paint::Pixmap;
use ferrokit_gui::tiny_skia::Color;
use ferrokit_widgets::application::Application;
use ferrokit_widgets::menu::{Menu, MenuRef};
#[cfg(windows)]
use ferrokit_widgets::system_tray::ActivationReason;
use ferrokit_widgets::system_tray::{MessageIcon, QSystemTrayIcon, SystemTrayIcon};
#[cfg(windows)]
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
#[cfg(windows)]
use windows_sys::Win32::UI::WindowsAndMessaging::{
    MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW, SendMessageW, WM_CONTEXTMENU, WM_KEYDOWN,
    WM_LBUTTONDBLCLK, WM_MBUTTONUP,
};

static TEST_LOCK: Mutex<()> = Mutex::new(());

fn setup_app() -> Application {
    Application::reset_for_test();
    Application::new(vec!["system_tray_test".to_string()])
}

fn create_test_icon() -> Icon {
    let mut pixmap = Pixmap::new(16, 16).expect("pixmap allocation");
    pixmap.fill(Color::from_rgba8(255, 0, 0, 255));
    Icon::from_pixmap(&pixmap)
}

#[cfg(windows)]
fn ensure_queue() {
    // SAFETY: MSG struct is all-integer/pointer fields valid with zeroed memory.
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    // SAFETY: PeekMessageW with PM_NOREMOVE and null HWND safely creates the thread message queue.
    unsafe { PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_NOREMOVE) };
}

#[cfg(windows)]
const VK_DOWN: usize = 0x28;
#[cfg(windows)]
const VK_RETURN: usize = 0x0D;
#[cfg(windows)]
const VK_ESCAPE: usize = 0x1B;

#[cfg(windows)]
fn post_key(vk: usize) {
    ensure_queue();
    // SAFETY: Posting WM_KEYDOWN with valid VK code to current thread message queue.
    unsafe { PostThreadMessageW(GetCurrentThreadId(), WM_KEYDOWN, vk, 0) };
}

#[cfg(windows)]
fn send_tray_event(hwnd: windows_sys::Win32::Foundation::HWND, event: u32) {
    // SAFETY: Sending WM_TRAY_CALLBACK to the tray HWND owned by this thread.
    unsafe {
        SendMessageW(
            hwnd,
            ferrokit_platform::tray_icon::WM_TRAY_CALLBACK,
            0,
            event as isize,
        );
    }
}
macro_rules! assert_not_send {
    ($($t:ty),+ $(,)?) => {$(
        const _: fn() = || {
            trait AmbiguousIfSend<A> {
                fn some_item() {}
            }
            impl<T: ?Sized> AmbiguousIfSend<()> for T {}
            struct Invalid;
            impl<T: ?Sized + Send> AmbiguousIfSend<Invalid> for T {}
            let _ = <$t as AmbiguousIfSend<_>>::some_item;
        };
    )+};
}

macro_rules! assert_not_sync {
    ($($t:ty),+ $(,)?) => {$(
        const _: fn() = || {
            trait AmbiguousIfSync<A> {
                fn some_item() {}
            }
            impl<T: ?Sized> AmbiguousIfSync<()> for T {}
            struct Invalid;
            impl<T: ?Sized + Sync> AmbiguousIfSync<Invalid> for T {}
            let _ = <$t as AmbiguousIfSync<_>>::some_item;
        };
    )+};
}

assert_not_send!(SystemTrayIcon);
assert_not_sync!(SystemTrayIcon);

#[test]
fn test_system_tray_main_thread_constraint() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _app = setup_app();

    // Succeeds on main thread
    let tray = SystemTrayIcon::new();
    assert!(tray.is_ok(), "SystemTrayIcon::new succeeds on main thread");

    // Panics on a worker thread
    let worker = std::thread::spawn(|| {
        let result = std::panic::catch_unwind(|| {
            let _ = SystemTrayIcon::new();
        });
        result.is_err()
    });
    let panicked = worker.join().expect("thread join");
    assert!(
        panicked,
        "SystemTrayIcon::new on non-main thread must panic"
    );
}

#[test]
fn test_system_tray_properties_and_qt_parity() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _app = setup_app();

    let mut tray: QSystemTrayIcon = SystemTrayIcon::new().expect("tray creation");

    // Tooltip
    assert_eq!(tray.tooltip(), "");
    assert!(tray.set_tooltip("Application Status").is_ok());
    assert_eq!(tray.tooltip(), "Application Status");

    // Icon
    let icon = create_test_icon();
    assert!(tray.set_icon(&icon).is_ok());
    assert!(tray.icon().is_some());

    // System tray availability and messages
    assert!(SystemTrayIcon::is_system_tray_available());
    let _ = tray.supports_messages();

    // Visibility
    assert!(!tray.is_visible());
    assert!(tray.show().is_ok());
    assert!(tray.is_visible());
    assert!(tray.hide().is_ok());
    assert!(!tray.is_visible());

    // Message API parity
    let _ = tray.show_message("Title", "Body", MessageIcon::Information, 5000);
    let _ = tray.show_message_with_icon("Title", "Body", &icon, 5000);
}

#[test]
fn test_system_tray_activation_and_message_clicked_signals() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _app = setup_app();

    let tray = SystemTrayIcon::new().expect("tray");
    let activated_reasons = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&activated_reasons);
    tray.activated.connect(move |reason| {
        sink.lock().unwrap().push(*reason);
    });

    let message_clicked = Arc::new(AtomicBool::new(false));
    let clicked_flag = Arc::clone(&message_clicked);
    tray.message_clicked.connect(move |()| {
        clicked_flag.store(true, Ordering::SeqCst);
    });

    #[cfg(windows)]
    {
        let hwnd = tray.hwnd();
        send_tray_event(hwnd, windows_sys::Win32::UI::Shell::NIN_SELECT);
        send_tray_event(hwnd, WM_LBUTTONDBLCLK);
        send_tray_event(hwnd, WM_MBUTTONUP);
        send_tray_event(hwnd, 0x0405); // NIN_BALLOONUSERCLICK

        let reasons = activated_reasons.lock().unwrap().clone();
        assert_eq!(
            reasons,
            vec![
                ActivationReason::Trigger,
                ActivationReason::DoubleClick,
                ActivationReason::MiddleClick
            ]
        );
        assert!(message_clicked.load(Ordering::SeqCst));
    }
}

#[test]
fn test_system_tray_context_menu_set_get_clear() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _app = setup_app();

    let mut tray = SystemTrayIcon::new().expect("tray");
    assert!(tray.context_menu().is_none());

    let menu1 = Menu::new_ref("Menu 1");
    tray.set_context_menu(menu1.clone());
    assert!(tray.context_menu().is_some());
    assert!(std::rc::Rc::ptr_eq(&tray.context_menu().unwrap(), &menu1));

    // Clear context menu
    tray.clear_context_menu();
    assert!(tray.context_menu().is_none());
}

#[test]
fn test_system_tray_context_menu_exec_popup_on_request() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _app = setup_app();

    let mut tray = SystemTrayIcon::new().expect("tray");
    let menu = Menu::new_ref("TrayMenu");
    let _action = menu.borrow_mut().add_new_action("Exit");

    let about_to_show_count = Arc::new(AtomicUsize::new(0));
    let show_sink = Arc::clone(&about_to_show_count);
    menu.borrow().about_to_show.connect(move |_| {
        show_sink.fetch_add(1, Ordering::SeqCst);
    });

    let about_to_hide_count = Arc::new(AtomicUsize::new(0));
    let hide_sink = Arc::clone(&about_to_hide_count);
    menu.borrow().about_to_hide.connect(move |_| {
        hide_sink.fetch_add(1, Ordering::SeqCst);
    });

    tray.set_context_menu(menu);

    #[cfg(windows)]
    {
        // Post Escape to dismiss the popup immediately once entered
        post_key(VK_ESCAPE);
        send_tray_event(tray.hwnd(), WM_CONTEXTMENU);

        assert_eq!(
            about_to_show_count.load(Ordering::SeqCst),
            1,
            "about_to_show must have been emitted on context menu popup"
        );
        assert_eq!(
            about_to_hide_count.load(Ordering::SeqCst),
            1,
            "about_to_hide must have been emitted on popup close"
        );
    }

    #[cfg(not(windows))]
    {
        // Non-windows direct emission of context menu signal
        tray.platform_tray()
            .on_context_menu_requested
            .emit(&Point::new(50, 50));
        assert_eq!(about_to_show_count.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn test_system_tray_context_menu_action_trigger() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _app = setup_app();

    let mut tray = SystemTrayIcon::new().expect("tray");
    let menu = Menu::new_ref("TrayMenu");
    let action = menu.borrow_mut().add_new_action("Item 1");

    let action_triggered = Arc::new(AtomicBool::new(false));
    let sink = Arc::clone(&action_triggered);
    action.borrow().triggered.connect(move |_| {
        sink.store(true, Ordering::SeqCst);
    });

    tray.set_context_menu(menu);

    #[cfg(windows)]
    {
        // Select first item and press return
        post_key(VK_DOWN);
        post_key(VK_RETURN);
        send_tray_event(tray.hwnd(), WM_CONTEXTMENU);

        assert!(
            action_triggered.load(Ordering::SeqCst),
            "chosen action triggered signal must have fired"
        );
    }
}

#[test]
fn test_system_tray_context_menu_replace_disconnects_old() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _app = setup_app();

    let mut tray = SystemTrayIcon::new().expect("tray");

    let menu1 = Menu::new_ref("Menu 1");
    let menu1_shown = Arc::new(AtomicUsize::new(0));
    let m1_sink = Arc::clone(&menu1_shown);
    menu1.borrow().about_to_show.connect(move |_| {
        m1_sink.fetch_add(1, Ordering::SeqCst);
    });

    let menu2 = Menu::new_ref("Menu 2");
    let menu2_shown = Arc::new(AtomicUsize::new(0));
    let m2_sink = Arc::clone(&menu2_shown);
    menu2.borrow().about_to_show.connect(move |_| {
        m2_sink.fetch_add(1, Ordering::SeqCst);
    });

    tray.set_context_menu(menu1);
    tray.set_context_menu(menu2);

    #[cfg(windows)]
    {
        post_key(VK_ESCAPE);
        send_tray_event(tray.hwnd(), WM_CONTEXTMENU);

        assert_eq!(
            menu1_shown.load(Ordering::SeqCst),
            0,
            "old menu must not be shown"
        );
        assert_eq!(
            menu2_shown.load(Ordering::SeqCst),
            1,
            "new menu must be shown"
        );
    }
}

thread_local! {
    static SHARED_MENU_FOR_TEST: RefCell<Option<MenuRef>> = const { RefCell::new(None) };
}

#[test]
fn test_system_tray_short_borrow_rule() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _app = setup_app();

    let mut tray = SystemTrayIcon::new().expect("tray");
    let menu = Menu::new_ref("DynamicMenu");

    SHARED_MENU_FOR_TEST.with(|s| *s.borrow_mut() = Some(menu.clone()));

    // Verify inside `about_to_show` that menu can be mutably borrowed to dynamically add actions
    menu.borrow().about_to_show.connect(|_| {
        SHARED_MENU_FOR_TEST.with(|s| {
            if let Some(m) = s.borrow().as_ref() {
                // Short-borrow rule: menu is not borrowed during about_to_show emission,
                // so slot can safely mutate it dynamically!
                let mut guard = m.borrow_mut();
                guard.add_new_action("Dynamic Action");
            }
        });
    });
    tray.set_context_menu(menu.clone());

    #[cfg(windows)]
    {
        post_key(VK_ESCAPE);
        send_tray_event(tray.hwnd(), WM_CONTEXTMENU);

        assert_eq!(
            menu.borrow().actions().len(),
            1,
            "dynamic action added during about_to_show without borrow panic"
        );
    }

    SHARED_MENU_FOR_TEST.with(|s| *s.borrow_mut() = None);
}

#[test]
fn test_system_tray_from_icon_and_with_tray() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _app = setup_app();

    let icon = create_test_icon();
    let tray = SystemTrayIcon::from_icon(&icon).expect("from_icon");
    assert!(tray.icon().is_some());

    // with_tray
    let platform_tray =
        ferrokit_platform::tray_icon::TrayIcon::from_icon("Custom", &icon).expect("platform tray");
    let custom_tray = SystemTrayIcon::with_tray(platform_tray).expect("with_tray");
    assert_eq!(custom_tray.tooltip(), "Custom");
}

#[test]
fn test_system_tray_drop_with_menu_cleans_up_safely() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _app = setup_app();

    let menu = Menu::new_ref("Menu");
    menu.borrow_mut().add_new_action("Action");

    {
        let mut tray = SystemTrayIcon::new().expect("tray");
        tray.set_context_menu(menu.clone());
        assert!(tray.context_menu().is_some());
        // tray drops here while menu connection is active
    }

    // Menu is still alive and usable
    assert_eq!(menu.borrow().actions().len(), 1);
}

#[test]
fn test_system_tray_reentrant_context_menu_does_not_panic() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _app = setup_app();

    let mut tray = SystemTrayIcon::new().expect("tray");
    let menu = Menu::new_ref("Menu");
    menu.borrow_mut().add_new_action("Item");
    tray.set_context_menu(menu);

    #[cfg(windows)]
    {
        ensure_queue();
        // Post second context menu request followed by escape key
        // The second request arrives inside the modal loop while menu.borrow_mut() is active.
        // SAFETY: PostMessageW safely posts WM_TRAY_CALLBACK with valid HWND to current thread.
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW(
                tray.hwnd(),
                ferrokit_platform::tray_icon::WM_TRAY_CALLBACK,
                0,
                WM_CONTEXTMENU as isize,
            );
        }
        post_key(VK_ESCAPE);

        // First request enters exec_popup modal loop
        send_tray_event(tray.hwnd(), WM_CONTEXTMENU);
    }
    #[cfg(not(windows))]
    {
        tray.platform_tray()
            .on_context_menu_requested
            .emit(&Point::new(50, 50));
        // Non-windows simulated dispatch
        tray.platform_tray()
            .on_context_menu_requested
            .emit(&Point::new(50, 50));
    }
}
