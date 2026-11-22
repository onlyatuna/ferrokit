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

#[test]
fn test_system_tray_set_template_smoke() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _app = setup_app();

    let icon = create_test_icon();
    let mut tray = SystemTrayIcon::from_icon(&icon).expect("from_icon");
    tray.set_template(false);
    tray.set_template(true);
}

/// Non-Windows modal `Menu::exec_popup` tests.
///
/// Not on macOS libtest: `setup_app()` installs `DispatcherKind::Gui` (`CocoaEventDispatcher`),
/// and pumping AppKit's event loop from a libtest worker thread traps on AppKit's main-thread
/// requirement (same reason as `test_application_layers::dropping_a_window_while_running_does_not_quit`).
#[cfg(not(any(windows, target_os = "macos")))]
mod modal_exec_popup {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use ferrokit_core::application::CoreApplication;
    use ferrokit_core::event::MouseButtons;
    use ferrokit_core::signal::Signal;
    use ferrokit_core::timer::Timer;
    use ferrokit_gui::geometry::Region;
    use ferrokit_gui::geometry::primitives::{Point, Rect};
    use ferrokit_gui::image::Icon;
    use ferrokit_gui::paint::Pixmap;
    use ferrokit_platform::backdrop::BackdropType;
    use ferrokit_platform::clipboard::PlatformClipboard;
    use ferrokit_platform::cursor::PlatformCursor;
    use ferrokit_platform::hotkey::PlatformHotkeyManager;
    use ferrokit_platform::integration::{PlatformIntegration, platform, set_platform_integration};
    use ferrokit_platform::platform_tray::PlatformTrayIcon;
    use ferrokit_platform::platform_window::PlatformWindow;
    use ferrokit_platform::screen::PlatformScreen;
    use ferrokit_platform::theme::PlatformTheme;
    use ferrokit_platform::window::WindowFlags;
    use ferrokit_platform::window_system_interface::{
        KeyboardModifiers, MouseButton, WindowSystemEvent, WindowSystemEventHandler, event_time_ms,
    };
    use ferrokit_widgets::Widget;
    use ferrokit_widgets::menu::Menu;

    const KEY_ESCAPE: u32 = 0x0100_0000;
    const KEY_RETURN: u32 = 0x0100_0004;
    const KEY_DOWN: u32 = 0x0100_0015;

    use super::{TEST_LOCK, setup_app};

    thread_local! {
        static MODAL_HANDLER: RefCell<Option<Box<dyn WindowSystemEventHandler>>> =
            const { RefCell::new(None) };
        static MODAL_GEOMETRY: Cell<Rect> = const { Cell::new(Rect::new(0, 0, 0, 0)) };
    }

    fn reset_modal_window_state(rect: Rect) {
        MODAL_HANDLER.with(|slot| *slot.borrow_mut() = None);
        MODAL_GEOMETRY.with(|g| g.set(rect));
    }

    fn deliver_modal_event(event: WindowSystemEvent) {
        let handler = MODAL_HANDLER.with(|slot| slot.borrow_mut().take());
        if let Some(mut h) = handler {
            h.handle_window_event(event);
            MODAL_HANDLER.with(|slot| {
                if slot.borrow().is_none() {
                    *slot.borrow_mut() = Some(h);
                }
            });
        }
    }

    fn modal_geometry() -> Rect {
        MODAL_GEOMETRY.with(Cell::get)
    }

    struct PlatformGuard {
        previous: Arc<dyn PlatformIntegration>,
    }

    impl PlatformGuard {
        fn install() -> Self {
            let previous = platform();
            set_platform_integration(Arc::new(FakeModalPlatform {
                inner: Arc::clone(&previous),
            }));
            Self { previous }
        }
    }

    impl Drop for PlatformGuard {
        fn drop(&mut self) {
            MODAL_HANDLER.with(|slot| *slot.borrow_mut() = None);
            set_platform_integration(Arc::clone(&self.previous));
        }
    }

    struct FakeScreen;

    impl PlatformScreen for FakeScreen {
        fn name(&self) -> String {
            "FakeModalScreen".to_string()
        }

        fn geometry(&self) -> Rect {
            Rect::new(0, 0, 1920, 1080)
        }

        fn available_geometry(&self) -> Rect {
            Rect::new(0, 0, 1920, 1040)
        }

        fn is_primary(&self) -> bool {
            true
        }

        fn device_pixel_ratio(&self) -> f32 {
            1.0
        }
    }

    struct FakeModalWindow {
        geometry: Rect,
        target_pos: Option<Point>,
        active: Cell<bool>,
        opacity: f32,
        min_size: (i32, i32),
        max_size: (i32, i32),
        flags: WindowFlags,
    }

    impl FakeModalWindow {
        fn new(rect: Rect, flags: WindowFlags) -> Self {
            reset_modal_window_state(rect);
            Self {
                geometry: rect,
                target_pos: None,
                active: Cell::new(false),
                opacity: 1.0,
                min_size: (0, 0),
                max_size: (i32::MAX, i32::MAX),
                flags,
            }
        }
    }

    impl Drop for FakeModalWindow {
        fn drop(&mut self) {
            MODAL_HANDLER.with(|slot| *slot.borrow_mut() = None);
        }
    }

    impl PlatformWindow for FakeModalWindow {
        fn show(&self) {
            self.active.set(true);
        }

        fn hide(&self) {
            self.active.set(false);
        }

        fn is_active(&self) -> bool {
            self.active.get()
        }

        fn request_activate_window(&self) {
            self.active.set(true);
        }

        fn geometry(&self) -> Rect {
            self.geometry
        }

        fn set_geometry(&mut self, rect: Rect) {
            self.geometry = rect;
            MODAL_GEOMETRY.with(|g| g.set(rect));
        }

        fn set_stays_on_top(&mut self, _enabled: bool) {}

        fn set_click_through(&mut self, _enabled: bool) {}

        fn flags(&self) -> WindowFlags {
            self.flags
        }

        fn set_opacity(&mut self, opacity: f32) {
            self.opacity = opacity;
        }

        fn opacity(&self) -> f32 {
            self.opacity
        }

        fn set_minimum_size(&mut self, min_w: i32, min_h: i32) {
            self.min_size = (min_w, min_h);
        }

        fn minimum_size(&self) -> (i32, i32) {
            self.min_size
        }

        fn set_maximum_size(&mut self, max_w: i32, max_h: i32) {
            self.max_size = (max_w, max_h);
        }

        fn maximum_size(&self) -> (i32, i32) {
            self.max_size
        }

        fn device_pixel_ratio(&self) -> f32 {
            1.0
        }

        fn present(&mut self, _pixmap: &mut Pixmap, _opacity: f32) -> Result<(), &'static str> {
            Ok(())
        }

        fn present_region(&mut self, pixmap: &Pixmap, _dirty: &Region) -> Result<(), &'static str> {
            if let Some(pos) = self.target_pos.take() {
                self.geometry = Rect::new(
                    pos.x,
                    pos.y,
                    pixmap.logical_width().round() as i32,
                    pixmap.logical_height().round() as i32,
                );
                MODAL_GEOMETRY.with(|g| g.set(self.geometry));
            }
            Ok(())
        }

        fn set_target_pos(&mut self, pos: Option<Point>) {
            self.target_pos = pos;
        }

        fn set_event_handler(&mut self, handler: Box<dyn WindowSystemEventHandler>) {
            MODAL_HANDLER.with(|slot| *slot.borrow_mut() = Some(handler));
        }

        fn native_handle(&self) -> isize {
            0
        }

        fn set_backdrop(&mut self, _backdrop: BackdropType, _dark_mode: bool) -> bool {
            false
        }

        fn supports_modal_event_loop(&self) -> bool {
            true
        }
    }

    struct FakeModalPlatform {
        inner: Arc<dyn PlatformIntegration>,
    }

    impl PlatformIntegration for FakeModalPlatform {
        fn create_window(
            &self,
            _title: &str,
            rect: Rect,
            flags: WindowFlags,
        ) -> Result<Box<dyn PlatformWindow>, &'static str> {
            Ok(Box::new(FakeModalWindow::new(rect, flags)))
        }

        fn create_tray_icon(
            &self,
            tooltip: &str,
            icon: &Icon,
        ) -> Result<Box<dyn PlatformTrayIcon>, &'static str> {
            self.inner.create_tray_icon(tooltip, icon)
        }

        fn primary_screen(&self) -> Box<dyn PlatformScreen> {
            Box::new(FakeScreen)
        }

        fn screens(&self) -> Vec<Box<dyn PlatformScreen>> {
            vec![Box::new(FakeScreen)]
        }

        fn screen_at(&self, pos: Point) -> Option<Box<dyn PlatformScreen>> {
            if FakeScreen.geometry().contains(pos) {
                Some(Box::new(FakeScreen))
            } else {
                None
            }
        }

        fn screen_changed(&self) -> &Signal<()> {
            self.inner.screen_changed()
        }

        fn theme(&self) -> Arc<dyn PlatformTheme> {
            self.inner.theme()
        }

        fn clipboard(&self) -> Box<dyn PlatformClipboard> {
            self.inner.clipboard()
        }

        fn create_hotkey_manager(&self) -> Result<Box<dyn PlatformHotkeyManager>, &'static str> {
            self.inner.create_hotkey_manager()
        }

        fn cursor(&self) -> Box<dyn PlatformCursor> {
            self.inner.cursor()
        }
    }

    #[test]
    fn exec_popup_modal_keyboard_selects_and_triggers_in_qt_order() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _app = setup_app();
        let _platform = PlatformGuard::install();

        let menu = Menu::new_ref("modal");
        let check = menu.borrow_mut().add_new_action("Toggle");
        check.borrow_mut().set_checkable(true);

        let log = Arc::new(Mutex::new(Vec::<&'static str>::new()));
        {
            let l = Arc::clone(&log);
            menu.borrow().about_to_show.connect(move |_| {
                l.lock().unwrap().push("about_to_show");
            });
            let l = Arc::clone(&log);
            menu.borrow().about_to_hide.connect(move |_| {
                l.lock().unwrap().push("about_to_hide");
            });
            let l = Arc::clone(&log);
            check.borrow().triggered.connect(move |_| {
                l.lock().unwrap().push("action.triggered");
            });
            let l = Arc::clone(&log);
            menu.borrow().triggered.connect(move |_| {
                l.lock().unwrap().push("menu.triggered");
            });
        }

        Timer::single_shot(10, || {
            deliver_modal_event(WindowSystemEvent::KeyPress {
                key: KEY_DOWN,
                modifiers: KeyboardModifiers::default(),
                is_repeat: false,
            });
            deliver_modal_event(WindowSystemEvent::KeyPress {
                key: KEY_RETURN,
                modifiers: KeyboardModifiers::default(),
                is_repeat: false,
            });
        });

        let chosen = Menu::exec_popup(&menu, Point::new(50, 50)).expect("chosen action");
        assert!(Rc::ptr_eq(&chosen, &check));
        assert!(check.borrow().is_checked());
        assert_eq!(
            *log.lock().unwrap(),
            vec![
                "about_to_show",
                "about_to_hide",
                "action.triggered",
                "menu.triggered",
            ]
        );
    }

    #[test]
    fn exec_popup_modal_mouse_submenu_and_left_flip() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _app = setup_app();
        let _platform = PlatformGuard::install();

        let menu = Menu::new_ref("root");
        let sub = menu.borrow_mut().add_menu("More");
        let sub_act = sub.borrow_mut().add_new_action("Nested Item");
        let sub_trigger_action = {
            let m = menu.borrow();
            m.actions()[0].clone()
        };

        let menu_hover = Rc::clone(&menu);
        let initial_rect: Rc<Cell<Rect>> = Rc::new(Cell::new(Rect::default()));
        let initial_for_hover = Rc::clone(&initial_rect);
        let sub_trigger_hover = Rc::clone(&sub_trigger_action);

        Timer::single_shot_local(10, move || {
            let init = modal_geometry();
            initial_for_hover.set(init);
            let item0 = menu_hover
                .borrow()
                .action_geometry(&sub_trigger_hover)
                .expect("item0 geometry");
            let hover_pos = Point::new(item0.x + item0.width / 2, item0.y + item0.height / 2);
            deliver_modal_event(WindowSystemEvent::MouseMove {
                pos: hover_pos,
                global_pos: Point::new(init.x + hover_pos.x, init.y + hover_pos.y),
                buttons: MouseButtons::NO_BUTTON,
            });
        });

        let menu_click = Rc::clone(&menu);
        let initial_for_click = Rc::clone(&initial_rect);
        let sub_act_click = Rc::clone(&sub_act);
        let left_flipped = Rc::new(Cell::new(false));
        let left_flipped_flag = Rc::clone(&left_flipped);

        Timer::single_shot_local(30, move || {
            let init = initial_for_click.get();
            let geom = modal_geometry();
            assert!(
                geom.x < init.x && geom.width > init.width,
                "submenu near right edge flips left: init={init:?}, expanded={geom:?}"
            );
            left_flipped_flag.set(true);

            let open_sub = menu_click
                .borrow()
                .open_submenu()
                .expect("open left-flipped submenu");
            let sub_geom = open_sub.borrow().geometry();
            let sub_item = open_sub
                .borrow()
                .action_geometry(&sub_act_click)
                .expect("submenu item geometry");
            let win_offset = Point::new(geom.x - init.x, geom.y - init.y);
            let local_in_win = Point::new(
                sub_geom.x + sub_item.x + sub_item.width / 2 - win_offset.x,
                sub_geom.y + sub_item.y + sub_item.height / 2 - win_offset.y,
            );
            let global_pos = Point::new(geom.x + local_in_win.x, geom.y + local_in_win.y);

            deliver_modal_event(WindowSystemEvent::MouseMove {
                pos: local_in_win,
                global_pos,
                buttons: MouseButtons::NO_BUTTON,
            });
            deliver_modal_event(WindowSystemEvent::MousePress {
                pos: local_in_win,
                global_pos,
                button: MouseButton::Left,
                modifiers: KeyboardModifiers::default(),
                timestamp: event_time_ms(),
            });
            deliver_modal_event(WindowSystemEvent::MouseRelease {
                pos: local_in_win,
                global_pos,
                button: MouseButton::Left,
                modifiers: KeyboardModifiers::default(),
            });
        });

        let chosen = Menu::exec_popup(&menu, Point::new(1900, 100)).expect("submenu action");
        assert!(left_flipped.get());
        assert!(Rc::ptr_eq(&chosen, &sub_act));
    }

    #[test]
    fn exec_popup_modal_dismisses_on_escape_outside_click_and_focus_out() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _app = setup_app();
        let _platform = PlatformGuard::install();

        let menu = Menu::new_ref("dismiss");
        menu.borrow_mut().add_new_action("One");
        let hides = Arc::new(Mutex::new(0usize));
        {
            let h = Arc::clone(&hides);
            menu.borrow().about_to_hide.connect(move |_| {
                *h.lock().unwrap() += 1;
            });
        }

        // (a) Escape key
        Timer::single_shot(10, || {
            deliver_modal_event(WindowSystemEvent::KeyPress {
                key: KEY_ESCAPE,
                modifiers: KeyboardModifiers::default(),
                is_repeat: false,
            });
        });
        assert!(Menu::exec_popup(&menu, Point::new(100, 100)).is_none());
        assert_eq!(*hides.lock().unwrap(), 1);

        // (b) Mouse press outside window / root widget bounds
        Timer::single_shot(10, || {
            deliver_modal_event(WindowSystemEvent::MousePress {
                pos: Point::new(-20, -20),
                global_pos: Point::new(80, 80),
                button: MouseButton::Left,
                modifiers: KeyboardModifiers::default(),
                timestamp: event_time_ms(),
            });
        });
        assert!(Menu::exec_popup(&menu, Point::new(100, 100)).is_none());
        assert_eq!(*hides.lock().unwrap(), 2);

        // (c) FocusOut
        Timer::single_shot(10, || {
            deliver_modal_event(WindowSystemEvent::FocusOut);
        });
        assert!(Menu::exec_popup(&menu, Point::new(100, 100)).is_none());
        assert_eq!(*hides.lock().unwrap(), 3);
    }

    #[test]
    fn exec_popup_modal_exits_when_application_exits() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut app = setup_app();
        let _platform = PlatformGuard::install();

        let menu = Menu::new_ref("exit_test");
        menu.borrow_mut().add_new_action("Action");

        let hides = Arc::new(AtomicUsize::new(0));
        let hides_sig = Arc::clone(&hides);
        menu.borrow().about_to_hide.connect(move |_| {
            hides_sig.fetch_add(1, Ordering::SeqCst);
        });

        let popup_ran = Rc::new(Cell::new(false));
        let popup_none = Rc::new(Cell::new(false));
        let popup_ran_timer = Rc::clone(&popup_ran);
        let popup_none_timer = Rc::clone(&popup_none);
        let menu_timer = Rc::clone(&menu);

        Timer::single_shot_local(5, move || {
            let res = Menu::exec_popup(&menu_timer, Point::new(40, 40));
            popup_none_timer.set(res.is_none());
            popup_ran_timer.set(true);
        });

        Timer::single_shot(20, || {
            CoreApplication::exit(7);
        });

        let exit_code = app.exec();
        assert_eq!(exit_code, 7);
        assert!(popup_ran.get());
        assert!(popup_none.get());
        assert_eq!(hides.load(Ordering::SeqCst), 1);
    }
}
