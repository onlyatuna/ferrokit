// SPDX-License-Identifier: LGPL-3.0-only
//! Tests ferrokit-platform core abstractions and Win32 implementation (PlatformWindow, PlatformTrayIcon, PlatformScreen, PlatformCursor, PlatformHotkeyManager, PlatformTheme)

use ferrokit_gui::geometry::primitives::Rect;
use ferrokit_gui::paint::Pixmap;
use ferrokit_gui::tiny_skia::Color;
use ferrokit_platform::*;
#[test]
#[cfg(windows)]
fn test_platform_screen_primary_and_multi_screens() {
    let screen = Win32Screen::primary();
    let geom = screen.geometry();
    let avail = screen.available_geometry();
    let dpr = screen.device_pixel_ratio();

    assert!(geom.width > 0);
    assert!(geom.height > 0);
    assert!(avail.width > 0);
    assert!(avail.height > 0);
    assert!(!screen.name().is_empty());
    assert!(screen.is_primary());
    // Available geometry must be smaller than or equal to physical full screen size
    assert!(avail.width <= geom.width);
    assert!(avail.height <= geom.height);
    assert!(dpr >= 1.0);

    // Verify multi-screen enumeration via Win32Screen::all_screens() and platform().screens()
    let all_screens = Win32Screen::all_screens();
    assert!(
        !all_screens.is_empty(),
        "system contains at least one primary screen"
    );
    assert!(
        all_screens[0].is_primary(),
        "first screen must be primary per Qt convention"
    );

    let p = platform();
    let p_screens = p.screens();
    assert_eq!(p_screens.len(), all_screens.len());

    // Hit test via screen_at
    let center_pt = ferrokit_gui::geometry::primitives::Point::new(
        geom.x + geom.width / 2,
        geom.y + geom.height / 2,
    );
    let hit_screen = p
        .screen_at(center_pt)
        .expect("center point must hit a screen");
    assert_eq!(hit_screen.geometry(), geom);
    // Verify screen_changed signal connection
    let signal_received = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = std::sync::Arc::clone(&signal_received);
    let _conn = p.screen_changed().connect(move |()| {
        flag.store(true, std::sync::atomic::Ordering::SeqCst);
    });
    p.screen_changed().emit(&());
}

#[test]
#[cfg(windows)]
fn test_platform_cursor_shapes() {
    let mut cursor = Win32Cursor::new();
    assert_eq!(cursor.current_shape(), CursorShape::Arrow);

    cursor.change_cursor(CursorShape::PointingHand);
    assert_eq!(cursor.current_shape(), CursorShape::PointingHand);

    cursor.change_cursor(CursorShape::SizeHor);
    assert_eq!(cursor.current_shape(), CursorShape::SizeHor);

    cursor.change_cursor(CursorShape::SizeVer);
    assert_eq!(cursor.current_shape(), CursorShape::SizeVer);
}

#[test]
#[cfg(windows)]
fn test_platform_theme_detection_and_notification() {
    let theme = Win32Theme::new();
    let scheme = theme.color_scheme();
    // Should detect dark, light, or unknown without crashing
    assert!(matches!(
        scheme,
        ColorScheme::Dark | ColorScheme::Light | ColorScheme::Unknown
    ));

    // Verify theme_changed signal subscription
    let received_scheme = std::sync::Arc::new(std::sync::Mutex::new(None));
    let sink = std::sync::Arc::clone(&received_scheme);
    let _conn = theme.theme_changed().connect(move |s| {
        *sink.lock().unwrap() = Some(*s);
    });

    // Simulate direct signal emission
    theme.theme_changed().emit(&ColorScheme::Dark);
    assert_eq!(*received_scheme.lock().unwrap(), Some(ColorScheme::Dark));

    // Verify platform().theme() returns shared instance
    let p_theme = platform().theme();
    assert_eq!(p_theme.color_scheme(), Win32Theme::query_color_scheme());
}

#[test]
#[cfg(windows)]
fn test_platform_window_abstraction_trait() {
    let rect = Rect::new(100, 100, 300, 200);
    let mut native_win = NativeWindow::new(
        "Trait Window",
        rect,
        WindowFlags::FRAMELESS | WindowFlags::LAYERED,
    )
    .expect("failed to create native window");

    // Operate via PlatformWindow trait
    let pwin: &mut dyn PlatformWindow = &mut native_win;
    assert_eq!(pwin.geometry(), rect);

    let new_rect = Rect::new(150, 150, 400, 250);
    pwin.set_geometry(new_rect);
    assert_eq!(pwin.geometry(), new_rect);

    pwin.set_stays_on_top(true);
    pwin.set_click_through(true);
    pwin.show();
    pwin.hide();

    let mut pixmap = Pixmap::new(400, 250).expect("failed to create pixmap");
    pixmap.fill(Color::from_rgba8(30, 60, 90, 200));
    assert!(pwin.present(&mut pixmap, 0.95).is_ok());
}

#[test]
#[cfg(windows)]
fn test_platform_tray_abstraction_trait() {
    let mut pixmap = Pixmap::new(16, 16).expect("failed to create pixmap");
    pixmap.fill(Color::from_rgba8(255, 100, 0, 255));
    let hicon = TrayIcon::create_hicon_from_pixmap(&pixmap).expect("failed to create HICON");

    let mut tray = TrayIcon::new("Trait Tray", hicon).expect("failed to create TrayIcon");
    let tray_hwnd = tray.hwnd();
    let mut win_menu = Box::new(Win32Menu::new(tray_hwnd));
    win_menu.add_action(1, "Open");

    // Operate via PlatformTrayIcon trait
    let ptray: &mut dyn PlatformTrayIcon = &mut *tray;
    assert!(ptray.show().is_ok());
    assert!(ptray.set_tooltip("Updated Trait Tooltip").is_ok());
    ptray.set_menu(win_menu);
    let mut new_pixmap = Pixmap::new(16, 16).expect("failed to create new pixmap");
    new_pixmap.fill(Color::from_rgba8(0, 200, 100, 255));
    assert!(ptray.set_icon(&new_pixmap).is_ok());

    assert!(ptray.hide().is_ok());
    TrayIcon::destroy_hicon(hicon);
}

#[test]
#[cfg(windows)]
fn test_platform_hotkey_manager() {
    let rect = Rect::new(0, 0, 10, 10);
    let win = NativeWindow::new("Hotkey Window", rect, WindowFlags::NORMAL)
        .expect("failed to create window");
    let mut manager = Win32HotkeyManager::new(win.hwnd());

    // Attempt to register F12 or any hotkey (graceful fallback if occupied)
    let res = manager.register_hotkey(999, HotkeyModifiers::CONTROL | HotkeyModifiers::ALT, 0x7B);
    if res.is_ok() {
        assert!(manager.registered_ids().contains(&999));
        assert!(manager.unregister_hotkey(999).is_ok());
        assert!(!manager.registered_ids().contains(&999));
    }
}
// macOS: `platform()` is the Cocoa integration, whose AppKit objects belong to the main thread;
// examples/appkit_main_thread.rs runs this there (`integration_factory`).
#[cfg(not(target_os = "macos"))]
#[test]
fn test_platform_integration_factory() {
    let p = platform();

    // 1. Retrieve screen and theme via factory
    let screen = p.primary_screen();
    assert!(screen.geometry().width > 0);
    assert!(screen.available_geometry().height > 0);

    let theme = p.theme();
    assert!(matches!(
        theme.color_scheme(),
        ColorScheme::Dark | ColorScheme::Light | ColorScheme::Unknown
    ));

    // 2. Operate clipboard via factory
    let cb = p.clipboard();
    let sample = "Factory Clipboard Text 🚀";
    assert!(cb.set_text(sample).is_ok());
    assert_eq!(cb.text().unwrap(), sample);

    // 3. Create platform window via factory
    let rect = Rect::new(50, 50, 200, 150);
    let win = p
        .create_window(
            "Factory Window",
            rect,
            WindowFlags::FRAMELESS | WindowFlags::LAYERED,
        )
        .expect("factory window creation failed");
    assert_eq!(win.geometry(), rect);
    win.show();
    win.hide();

    // 4. Create platform tray icon via factory
    let mut pixmap = Pixmap::new(16, 16).expect("failed to create pixmap");
    pixmap.fill(Color::from_rgba8(20, 120, 220, 255));
    let mut tray = p
        .create_tray_icon("Factory Tray", &pixmap)
        .expect("factory tray icon creation failed");
    assert!(tray.show().is_ok());
    assert!(tray.set_tooltip("Updated Factory Tray").is_ok());
    assert!(tray.hide().is_ok());
}
#[test]
#[cfg(windows)]
fn test_window_system_events_dispatch_pipeline() {
    use std::sync::{Arc, Mutex};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SendMessageW, WM_CLOSE, WM_KEYDOWN, WM_KEYUP, WM_KILLFOCUS, WM_LBUTTONDOWN, WM_LBUTTONUP,
        WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_SETFOCUS, WM_SIZE,
    };

    let p = Win32PlatformIntegration::default();
    let rect = Rect::new(100, 100, 400, 300);
    let mut win = p
        .create_window("Event Test Window", rect, WindowFlags::NORMAL)
        .expect("failed to create native window");

    let received_events = Arc::new(Mutex::new(Vec::<WindowSystemEvent>::new()));
    let events_sink = Arc::clone(&received_events);

    let handler = ClosureWindowEventHandler::new(move |event| {
        events_sink.lock().unwrap().push(event);
    });

    win.set_event_handler(Box::new(handler));

    let hwnd = win.native_handle() as windows_sys::Win32::Foundation::HWND;

    unsafe {
        // 1. Simulate mouse move (x: 50, y: 80)
        let mouse_pos = (50 & 0xffff) | ((80 & 0xffff) << 16);
        SendMessageW(hwnd, WM_MOUSEMOVE, 0, mouse_pos);

        // 2. Simulate mouse click and release
        SendMessageW(hwnd, WM_LBUTTONDOWN, 0, mouse_pos);
        SendMessageW(hwnd, WM_LBUTTONUP, 0, mouse_pos);

        // 3. Simulate window resize (width 450, height 350)
        let size_param = (450 & 0xffff) | ((350 & 0xffff) << 16);
        SendMessageW(hwnd, WM_SIZE, 0, size_param);

        // 4. Simulate focus change
        SendMessageW(hwnd, WM_SETFOCUS, 0, 0);
        SendMessageW(hwnd, WM_KILLFOCUS, 0, 0);

        // 5. Simulate key press (VK_ESCAPE = 0x1B)
        SendMessageW(hwnd, WM_KEYDOWN, 0x1B, 0);
        SendMessageW(hwnd, WM_KEYUP, 0x1B, 0);

        // 6. Simulate mouse wheel scroll (scroll up 120 units)
        let wheel_wparam = (120i16 as u16 as usize) << 16;
        SendMessageW(hwnd, WM_MOUSEWHEEL, wheel_wparam, 0);

        // 7. Simulate close request
        SendMessageW(hwnd, WM_CLOSE, 0, 0);
    }
    let events = received_events.lock().unwrap().clone();
    assert!(
        !events.is_empty(),
        "should receive at least one dispatched window event"
    );

    let has_mouse_move = events.iter().any(
        |e| matches!(e, WindowSystemEvent::MouseMove { pos, .. } if pos.x == 50 && pos.y == 80),
    );
    assert!(has_mouse_move, "should capture MouseMove event");

    let has_mouse_press = events.iter().any(|e| {
        matches!(
            e,
            WindowSystemEvent::MousePress {
                button: MouseButton::Left,
                ..
            }
        )
    });
    assert!(
        has_mouse_press,
        "should capture MousePress left button event"
    );

    let has_mouse_release = events.iter().any(|e| {
        matches!(
            e,
            WindowSystemEvent::MouseRelease {
                button: MouseButton::Left,
                ..
            }
        )
    });
    assert!(
        has_mouse_release,
        "should capture MouseRelease left button event"
    );

    let has_resize = events.iter().any(|e| matches!(e, WindowSystemEvent::Resize { size } if size.width == 450 && size.height == 350));
    assert!(has_resize, "should capture Resize event");

    let has_focus_in = events
        .iter()
        .any(|e| matches!(e, WindowSystemEvent::FocusIn));
    assert!(has_focus_in, "should capture FocusIn event");

    let has_focus_out = events
        .iter()
        .any(|e| matches!(e, WindowSystemEvent::FocusOut));
    assert!(has_focus_out, "should capture FocusOut event");

    let has_key_press = events
        .iter()
        .any(|e| matches!(e, WindowSystemEvent::KeyPress { key: 0x1B, .. }));
    assert!(has_key_press, "should capture KeyPress event");

    let has_key_release = events
        .iter()
        .any(|e| matches!(e, WindowSystemEvent::KeyRelease { key: 0x1B, .. }));
    assert!(has_key_release, "should capture KeyRelease event");

    let has_wheel = events
        .iter()
        .any(|e| matches!(e, WindowSystemEvent::Wheel { delta, .. } if delta.y == 120));
    assert!(has_wheel, "should capture Wheel scroll event");

    let has_close = events
        .iter()
        .any(|e| matches!(e, WindowSystemEvent::CloseRequest));
    assert!(has_close, "should capture CloseRequest event");
}
#[test]
fn test_cross_platform_tray_icon_implementations() {
    ferrokit_core::object::ThreadContext::init_current(true, None);
    use ferrokit_platform::tray::{DbusStatusNotifierItem, PlatformTrayIcon};
    let mut pixmap = Pixmap::new(24, 24).expect("failed to create pixmap");
    // Cyan RGBA: [0, 200, 255, 255]
    pixmap.fill(Color::from_rgba8(0, 200, 255, 255));

    // 1. Verify Linux D-Bus StatusNotifierItem (SNI)
    let mut dbus_tray: Box<dyn PlatformTrayIcon> =
        Box::new(DbusStatusNotifierItem::new("hud-monitor", "HUD Monitor"));

    assert!(dbus_tray.set_icon(&pixmap).is_ok());
    assert!(dbus_tray.set_tooltip("CPU 45% | RAM 62%").is_ok());
    assert!(dbus_tray.show().is_ok());
    assert!(dbus_tray.hide().is_ok());

    // Verify D-Bus a(iiay) format conversion: width 24, height 24, channel order ARGB [180, 0, 200, 255]
    let downcasted = DbusStatusNotifierItem::new("test", "Test");
    let mut test_tray = downcasted;
    test_tray.set_icon(&pixmap).unwrap();
    let img = &test_tray.icon_pixmap()[0];
    assert_eq!(img.width, 24);
    assert_eq!(img.height, 24);
    assert_eq!(img.data[0], 255); // Alpha
    assert_eq!(img.data[1], 0); // Red
    assert_eq!(img.data[2], 200); // Green
    assert_eq!(img.data[3], 255); // Blue

    // 2. Verify macOS Cocoa NSStatusBar / NSStatusItem against the mock runtime. On macOS the
    // real AppKit version runs on the main thread: examples/appkit_main_thread.rs
    // (`status_item_lifecycle`).
    #[cfg(not(target_os = "macos"))]
    {
        use ferrokit_platform::tray::CocoaStatusItem;
        let mut cocoa_tray: Box<dyn PlatformTrayIcon> = Box::new(CocoaStatusItem::new(101));
        assert!(cocoa_tray.set_icon(&pixmap).is_ok());
        assert!(cocoa_tray.set_tooltip("Claude HUD").is_ok());
        assert!(cocoa_tray.show().is_ok());
        assert!(cocoa_tray.hide().is_ok());
    }
}
#[test]
fn test_cross_platform_menu_implementations() {
    use ferrokit_platform::menu::{DBusMenu, DBusMenuPropValue, PlatformMenu};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    // 1. Verify Linux D-Bus Menu (com.canonical.dbusmenu)
    let mut dbus_menu = DBusMenu::new();
    let initial_rev = dbus_menu.revision();

    let action_item = dbus_menu.add_action(101, "Toggle HUD");
    let check_item = dbus_menu.add_checkable(102, "Click Through", true);
    dbus_menu.add_separator();

    assert!(dbus_menu.revision() > initial_rev);

    let clicked_flag = Arc::new(AtomicBool::new(false));
    let flag_clone = Arc::clone(&clicked_flag);
    action_item.activated().connect(move |()| {
        flag_clone.store(true, Ordering::SeqCst);
    });

    // Verify D-Bus GetLayout serialization tree
    let layout = dbus_menu.get_layout(0, -1);
    assert_eq!(layout.id, 0);
    assert_eq!(layout.children.len(), 3);

    // Item 1: Normal action
    assert_eq!(
        layout.children[0].properties.get("label"),
        Some(&DBusMenuPropValue::String("Toggle HUD".to_string()))
    );

    // Item 2: Checkable (toggle-type: checkmark, toggle-state: 1)
    assert_eq!(
        layout.children[1].properties.get("toggle-type"),
        Some(&DBusMenuPropValue::String("checkmark".to_string()))
    );
    assert_eq!(
        layout.children[1].properties.get("toggle-state"),
        Some(&DBusMenuPropValue::Int(1))
    );

    // Item 3: Separator (type: separator)
    assert_eq!(
        layout.children[2].properties.get("type"),
        Some(&DBusMenuPropValue::String("separator".to_string()))
    );

    // Simulate desktop panel remote click Event(101, "clicked")
    let handled = dbus_menu.handle_event(101, "clicked");
    assert!(handled);
    assert!(
        clicked_flag.load(Ordering::SeqCst),
        "click should trigger activated signal"
    );

    // Simulate desktop panel click on checkable item Event(102, "clicked") -> automatically toggle checked state
    assert!(check_item.is_checked());
    let handled_check = dbus_menu.handle_event(102, "clicked");
    assert!(handled_check);
    assert!(
        !check_item.is_checked(),
        "clicking checkable item should automatically toggle checked state"
    );

    // 2. Verify macOS Cocoa NSMenu / NSMenuItem against the mock runtime. On macOS the real
    // AppKit version runs on the main thread: examples/appkit_main_thread.rs (`menu_native`).
    #[cfg(not(target_os = "macos"))]
    {
        use ferrokit_gui::geometry::Point;
        use ferrokit_platform::menu::CocoaMenu;
        let mut cocoa_menu = CocoaMenu::new();
        let cocoa_action = cocoa_menu.add_action(201, "Preferences");
        let cocoa_check = cocoa_menu.add_checkable(202, "Always on Top", false);
        cocoa_menu.add_separator();

        let cocoa_clicked = Arc::new(AtomicBool::new(false));
        let cocoa_flag = Arc::clone(&cocoa_clicked);
        cocoa_action.activated().connect(move |()| {
            cocoa_flag.store(true, Ordering::SeqCst);
        });

        // Verify popup status and coordinates
        cocoa_menu.show_popup(Point::new(120, 240));
        assert!(cocoa_menu.is_popped_up());
        assert_eq!(cocoa_menu.popup_pos(), Some(Point::new(120, 240)));

        // Trigger click
        assert!(cocoa_menu.trigger_item(201));
        assert!(cocoa_clicked.load(Ordering::SeqCst));

        // Trigger checkable toggle
        assert!(!cocoa_check.is_checked());
        assert!(cocoa_menu.trigger_item(202));
        assert!(cocoa_check.is_checked());

        cocoa_menu.dismiss();
        assert!(!cocoa_menu.is_popped_up());
    }
}

// A real click reaches Rust only if the control's target implements the action selector. The mock
// runtime records `setTarget:`/`setAction:`; `ActionTarget::perform` is what the native
// `-ferrokitPerformAction:` does. On macOS the same path runs against AppKit:
// examples/appkit_main_thread.rs.
#[cfg(not(target_os = "macos"))]
#[test]
fn test_cocoa_status_item_and_menu_clicks_reach_rust_callbacks() {
    use ferrokit_core::object::ThreadContext;
    use ferrokit_platform::{ActionTarget, CocoaStatusItem, MockObjcRuntime, ObjcMsg, Sel};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    ThreadContext::init_current(true, None);
    let mock = MockObjcRuntime::instance();

    // Status item: the button's target is a dedicated action target, not the status item itself.
    let mut status_item = CocoaStatusItem::new(7);
    let clicks = Arc::new(AtomicUsize::new(0));
    let sink = Arc::clone(&clicks);
    status_item.set_on_click(move || {
        sink.fetch_add(1, Ordering::SeqCst);
    });
    let button =
        unsafe { ObjcMsg::send_0(status_item.native_status_item(), Sel::register("button")) };
    let wired = mock.get_object_data(button).expect("button exists");
    assert!(!wired.target.is_nil());
    assert_ne!(wired.target, status_item.native_status_item());
    assert_eq!(wired.action, ActionTarget::action());
    assert_eq!(
        mock.get_object_data(wired.target).unwrap().class_name,
        "FerrokitActionTarget"
    );
    assert!(ActionTarget::perform(wired.target));
    assert_eq!(clicks.load(Ordering::SeqCst), 1);
    drop(status_item);
    assert!(
        !ActionTarget::perform(wired.target),
        "a dropped status item leaves no live target"
    );

    // Menu items: leaf items get target + action, submenu parents and separators do not.
    let mut menu = CocoaMenu::new();
    let plain = menu.add_action(1, "Open");
    let check = menu.add_checkable(2, "Pinned", false);
    menu.add_separator();
    menu.add_submenu("More", Box::new(CocoaMenu::new()));
    let activated = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&activated);
    plain.activated().connect(move |()| {
        counter.fetch_add(1, Ordering::SeqCst);
    });

    let items = menu.items();
    let plain_native = mock.get_object_data(items[0].native_item()).unwrap();
    assert!(!plain_native.target.is_nil());
    assert_eq!(plain_native.action, ActionTarget::action());
    assert!(ActionTarget::perform(plain_native.target));
    assert_eq!(activated.load(Ordering::SeqCst), 1);

    let check_native = mock.get_object_data(items[1].native_item()).unwrap();
    assert!(ActionTarget::perform(check_native.target));
    assert!(check.is_checked(), "a click toggles the checkable item");

    // The mock hands out no object for `+[NSMenuItem separatorItem]`; a real one gets no target.
    assert!(items[2].is_separator());
    let submenu_parent = mock.get_object_data(items[3].native_item()).unwrap();
    assert!(submenu_parent.target.is_nil());
    assert!(submenu_parent.action.is_nil());

    drop(menu);
    drop(items);
    drop(plain);
    drop(check);
    assert!(
        !ActionTarget::perform(plain_native.target),
        "dropping the items releases their targets"
    );
}

// macOS: real NSScreen/NSCursor/NSApplication through `platform()`; examples/appkit_main_thread.rs
// runs them on the main thread (`screen_metrics`, `cursor_shape`, `theme_color_scheme`).
#[cfg(not(target_os = "macos"))]
#[test]
fn test_platform_singleton_does_not_panic() {
    use ferrokit_platform::{platform, CursorShape};

    let p = platform();
    let primary = p.primary_screen();
    assert!(!primary.name().is_empty());
    assert!(primary.geometry().width > 0);
    assert!(primary.geometry().height > 0);
    assert!(primary.device_pixel_ratio() >= 1.0);

    let screens = p.screens();
    assert!(!screens.is_empty());

    let mut cursor = p.cursor();
    cursor.change_cursor(CursorShape::PointingHand);

    let theme = p.theme();
    let _ = theme.color_scheme();
}

#[test]
fn test_generic_platform_integration_full_lifecycle() {
    use ferrokit_gui::geometry::primitives::Rect;
    use ferrokit_gui::paint::Pixmap;
    use ferrokit_gui::tiny_skia::Color;
    use ferrokit_platform::{GenericPlatformIntegration, PlatformIntegration, WindowFlags};

    let integration = GenericPlatformIntegration::default();
    let mut win = integration
        .create_window(
            "Generic Title",
            Rect::new(50, 50, 400, 300),
            WindowFlags::NORMAL,
        )
        .expect("failed to create generic window");

    win.show();
    assert_eq!(win.geometry(), Rect::new(50, 50, 400, 300));
    win.set_geometry(Rect::new(100, 100, 500, 400));
    assert_eq!(win.geometry(), Rect::new(100, 100, 500, 400));

    let mut pixmap = Pixmap::new(200, 200).expect("failed to create pixmap");
    pixmap.fill(Color::from_rgba8(255, 0, 0, 255));
    assert!(win.present(&mut pixmap, 0.9).is_ok());
    win.hide();

    let mut tray = integration
        .create_tray_icon("Generic Tray", &pixmap)
        .expect("failed to create generic tray icon");
    assert!(tray.show().is_ok());
    assert!(tray.set_tooltip("Updated Generic Tooltip").is_ok());
    assert!(tray.hide().is_ok());

    let clipboard = integration.clipboard();
    assert!(clipboard.set_text("FerroKit Generic Clipboard").is_ok());
    assert_eq!(clipboard.text().unwrap(), "FerroKit Generic Clipboard");
    assert!(clipboard.clear().is_ok());
    assert_eq!(clipboard.text().unwrap(), "");

    let mut hotkey_mgr = integration
        .create_hotkey_manager()
        .expect("failed to create hotkey manager");
    assert!(hotkey_mgr
        .register_hotkey(1, ferrokit_platform::HotkeyModifiers::ALT, 0x41)
        .is_err());
    assert!(hotkey_mgr.unregister_hotkey(1).is_err());
}

#[test]
fn test_unix_platform_integration_dbus_tray_and_window() {
    use ferrokit_gui::geometry::primitives::Rect;
    use ferrokit_gui::paint::Pixmap;
    use ferrokit_platform::{PlatformIntegration, UnixPlatformIntegration, WindowFlags};

    let unix_integration = UnixPlatformIntegration::default();
    let win = unix_integration
        .create_window("Linux App", Rect::new(0, 0, 800, 600), WindowFlags::NORMAL)
        .expect("failed to create Linux window");
    assert_eq!(win.geometry().width, 800);

    let pixmap = Pixmap::new(32, 32).expect("failed to create pixmap");
    let mut tray = unix_integration
        .create_tray_icon("Linux Tray Icon", &pixmap)
        .expect("failed to create Linux D-Bus tray icon");
    assert!(tray.show().is_ok());
    assert!(tray.hide().is_ok());

    let primary = unix_integration.primary_screen();
    assert!(primary.is_primary());
    assert_eq!(primary.device_pixel_ratio(), 1.0);
}

// Mock-runtime test (the 2.0 ratio is the mock screen's). On macOS: examples/appkit_main_thread.rs
// (`integration_factory`, `screen_metrics`) on the main thread.
#[cfg(not(target_os = "macos"))]
#[test]
fn test_cocoa_platform_integration_status_item_and_retina() {
    ferrokit_core::object::ThreadContext::init_current(true, None);
    use ferrokit_gui::geometry::primitives::Rect;
    use ferrokit_gui::paint::Pixmap;
    use ferrokit_platform::{CocoaPlatformIntegration, PlatformIntegration, WindowFlags};
    let cocoa_integration = CocoaPlatformIntegration::default();
    let win = cocoa_integration
        .create_window("macOS App", Rect::new(0, 0, 1024, 768), WindowFlags::NORMAL)
        .expect("failed to create macOS window");
    assert_eq!(win.geometry().width, 1024);

    let pixmap = Pixmap::new(32, 32).expect("failed to create pixmap");
    let mut tray = cocoa_integration
        .create_tray_icon("macOS Status Item", &pixmap)
        .expect("failed to create macOS Cocoa status bar icon");
    assert!(tray.show().is_ok());
    assert!(tray.hide().is_ok());

    let primary = cocoa_integration.primary_screen();
    assert!(primary.is_primary());
    assert_eq!(primary.device_pixel_ratio(), 2.0); // Retina DPR 2.0
}

#[test]
fn test_set_platform_integration_custom_override() {
    use ferrokit_platform::{platform, set_platform_integration, GenericPlatformIntegration};
    use std::sync::Arc;

    let prev = platform();
    let custom_generic: Arc<dyn PlatformIntegration> =
        Arc::new(GenericPlatformIntegration::default());
    set_platform_integration(Arc::clone(&custom_generic));

    let current = platform();
    let screen = current.primary_screen();
    assert_eq!(screen.name(), "DefaultScreen");
    set_platform_integration(prev);
}

#[test]
fn test_linux_x11_native_window_lifecycle_and_events() {
    use ferrokit_gui::geometry::primitives::Rect;
    use ferrokit_gui::paint::Pixmap;
    use ferrokit_gui::tiny_skia::Color;
    use ferrokit_platform::{
        ClosureWindowEventHandler, PlatformWindow, WindowFlags, WindowSystemEvent, X11Event,
        X11NativeWindow,
    };
    use std::sync::{Arc, Mutex};

    let mut win = X11NativeWindow::new_offscreen(
        "X11 HUD Window",
        Rect::new(100, 200, 600, 400),
        WindowFlags::FRAMELESS | WindowFlags::STAYS_ON_TOP | WindowFlags::CLICK_THROUGH,
    )
    .expect("failed to create X11 native window");

    assert!(win.xid() > 0);
    assert_eq!(win.geometry(), Rect::new(100, 200, 600, 400));
    assert_eq!(win.native_handle(), win.xid() as isize);

    win.show();
    win.set_geometry(Rect::new(150, 250, 700, 500));
    assert_eq!(win.geometry(), Rect::new(150, 250, 700, 500));

    let mut pixmap = Pixmap::new(700, 500).expect("failed to create pixmap");
    pixmap.fill(Color::from_rgba8(20, 40, 60, 255));
    assert!(win.present(&mut pixmap, 0.95).is_ok());

    // Verify X11 event translation and dispatch pipeline
    let received = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = Arc::clone(&received);
    win.set_event_handler(Box::new(ClosureWindowEventHandler::new(
        move |event| match event {
            WindowSystemEvent::Resize { .. } => sink.lock().unwrap().push("Resize".to_string()),
            WindowSystemEvent::MousePress { .. } => {
                sink.lock().unwrap().push("MousePress".to_string())
            }
            WindowSystemEvent::MouseMove { .. } => {
                sink.lock().unwrap().push("MouseMove".to_string())
            }
            WindowSystemEvent::CloseRequest => {
                sink.lock().unwrap().push("CloseRequest".to_string())
            }
            _ => {}
        },
    )));

    // Simulate X11 protocol event injection
    assert!(win.dispatch_x11_event(X11Event::Expose {
        rect: Rect::new(0, 0, 700, 500)
    }));
    assert!(win.dispatch_x11_event(X11Event::MotionNotify {
        x: 50,
        y: 80,
        modifiers: 0
    }));
    assert!(win.dispatch_x11_event(X11Event::ButtonPress {
        button: 1,
        x: 50,
        y: 80,
        modifiers: 0
    }));
    assert!(win.dispatch_x11_event(X11Event::ClientMessage {
        atom: "WM_DELETE_WINDOW"
    }));

    let events = received.lock().unwrap().clone();
    assert_eq!(
        events,
        vec!["Resize", "MouseMove", "MousePress", "CloseRequest"]
    );

    win.hide();
}

#[test]
fn test_linux_wayland_native_window_layer_shell_and_events() {
    use ferrokit_gui::geometry::primitives::Rect;
    use ferrokit_gui::paint::Pixmap;
    use ferrokit_gui::tiny_skia::Color;
    use ferrokit_platform::{
        ClosureWindowEventHandler, PlatformWindow, WaylandEvent, WaylandNativeWindow, WindowFlags,
        WindowSystemEvent,
    };
    use std::sync::{Arc, Mutex};

    let mut win = WaylandNativeWindow::new_offscreen(
        "Wayland HUD Window",
        Rect::new(0, 0, 800, 600),
        WindowFlags::FRAMELESS | WindowFlags::STAYS_ON_TOP,
    )
    .expect("failed to create Wayland native window");

    assert!(win.surface_id() > 0);
    assert!(
        win.is_layer_shell(),
        "HUD window should automatically enable zwlr_layer_shell_v1 protocol"
    );
    assert_eq!(win.geometry(), Rect::new(0, 0, 800, 600));

    win.show();
    let mut pixmap = Pixmap::new(800, 600).expect("failed to create pixmap");
    pixmap.fill(Color::from_rgba8(10, 80, 150, 220));
    assert!(win.present(&mut pixmap, 0.85).is_ok());

    // Verify Wayland event translation and dispatch pipeline
    let received = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = Arc::clone(&received);
    win.set_event_handler(Box::new(ClosureWindowEventHandler::new(
        move |event| match event {
            WindowSystemEvent::Resize { .. } => sink.lock().unwrap().push("Resize".to_string()),
            WindowSystemEvent::MouseMove { .. } => {
                sink.lock().unwrap().push("MouseMove".to_string())
            }
            WindowSystemEvent::MousePress { .. } => {
                sink.lock().unwrap().push("MousePress".to_string())
            }
            WindowSystemEvent::CloseRequest => {
                sink.lock().unwrap().push("CloseRequest".to_string())
            }
            _ => {}
        },
    )));

    assert!(win.dispatch_wayland_event(WaylandEvent::PointerMotion {
        surface_x: 120,
        surface_y: 60,
        modifiers: 0
    }));
    assert!(win.dispatch_wayland_event(WaylandEvent::PointerButton {
        button: 0x110,
        state: 1,
        modifiers: 0
    }));
    assert!(win.dispatch_wayland_event(WaylandEvent::Configure {
        width: 1024,
        height: 768
    }));
    assert!(win.dispatch_wayland_event(WaylandEvent::CloseRequest));

    let events = received.lock().unwrap().clone();
    assert_eq!(
        events,
        vec!["MouseMove", "MousePress", "Resize", "CloseRequest"]
    );

    win.hide();
}

#[test]
fn test_linux_dbus_wire_protocol_serialization_and_deserialization() {
    use ferrokit_platform::{DbusMessage, DBUS_MESSAGE_TYPE_METHOD_CALL};

    let mut msg = DbusMessage::method_call(
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "RequestName",
        1,
    );
    msg.append_string("org.kde.StatusNotifierItem-1001-1");

    let bytes = msg.to_bytes();
    assert!(bytes.len() >= 16);
    assert_eq!(bytes[0], b'l'); // Little-endian
    assert_eq!(bytes[1], DBUS_MESSAGE_TYPE_METHOD_CALL);
    // 8-byte alignment
    assert_eq!(bytes.len() % 8, (msg.body.len()) % 8);

    let parsed = DbusMessage::from_bytes(&bytes).expect("failed to parse D-Bus message bytes");
    assert_eq!(parsed.msg_type, DBUS_MESSAGE_TYPE_METHOD_CALL);
    assert_eq!(parsed.serial, 1);
}

#[test]
fn test_linux_dbus_connection_lifecycle_and_registration() {
    use ferrokit_platform::tray::dbus_connection::scripted::ScriptedDbusTransport;
    use ferrokit_platform::tray::dbus_connection::DBUS_MESSAGE_TYPE_METHOD_RETURN;
    use ferrokit_platform::DbusConnection;

    let (transport, bus) = ScriptedDbusTransport::new(":1.42");
    let mut conn = DbusConnection::from_transport(Box::new(transport), "scripted:", 1000)
        .expect("failed to handshake with the scripted bus");
    // The unique name is the one the bus put in the Hello reply, not a guess.
    assert_eq!(conn.unique_name(), ":1.42");
    assert_eq!(
        conn.socket_fd(),
        -1,
        "the scripted transport owns no descriptor"
    );

    assert!(conn
        .request_name("org.kde.StatusNotifierItem-TestApp-1")
        .unwrap());
    assert!(conn
        .register_status_notifier_item("/StatusNotifierItem")
        .unwrap());

    assert!(conn.has_sent_member("Hello"));
    assert!(conn.has_sent_member("RequestName"));
    assert!(conn.has_sent_member("RegisterStatusNotifierItem"));
    assert!(conn.outbox_count() >= 3);

    let members: Vec<_> = bus
        .received()
        .iter()
        .map(|m| m.member.clone().unwrap())
        .collect();
    assert_eq!(
        members,
        ["Hello", "RequestName", "RegisterStatusNotifierItem"]
    );
    // The RequestName reply (u32 1) arrives as a framed message with its header fields decoded.
    let replies = conn.read_incoming().unwrap();
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].msg_type, DBUS_MESSAGE_TYPE_METHOD_RETURN);
    assert_eq!(replies[0].signature, "u");
    assert_eq!(replies[0].reply_serial, Some(2));
}

/// Serves one client on `listener` with the in-memory bus until it disconnects.
#[cfg(target_os = "linux")]
fn serve_scripted_bus(listener: std::os::unix::net::UnixListener) -> std::thread::JoinHandle<()> {
    use ferrokit_platform::tray::dbus_connection::scripted::ScriptedDbusTransport;
    use std::io::{ErrorKind, Read, Write};
    std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().expect("accept");
        let (mut daemon, _bus) = ScriptedDbusTransport::new(":1.99");
        let mut buf = [0u8; 4096];
        loop {
            let n = sock.read(&mut buf).expect("read from client");
            if n == 0 {
                return;
            }
            daemon.write_all(&buf[..n]).unwrap();
            loop {
                match daemon.read(&mut buf) {
                    Ok(n) => sock.write_all(&buf[..n]).expect("write to client"),
                    Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                    Err(e) => panic!("{e}"),
                }
            }
        }
    })
}

#[cfg(target_os = "linux")]
#[test]
fn test_linux_dbus_connects_over_path_and_abstract_unix_sockets() {
    use ferrokit_core::event_loop::UnixEventDispatcher;
    use ferrokit_platform::DbusConnection;
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::net::{SocketAddr, UnixListener};

    let dir = std::env::temp_dir().join(format!("ferrokit-dbus-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("bus");
    let _ = std::fs::remove_file(&path);
    let server = serve_scripted_bus(UnixListener::bind(&path).unwrap());
    let conn = DbusConnection::connect_to_address(&format!(
        "unix:path=/nonexistent/bus;unix:path={}",
        path.display()
    ))
    .expect("connect over a path socket (first candidate is unreachable)");
    assert_eq!(conn.unique_name(), ":1.99");
    assert!(conn.socket_fd() >= 0);
    let mut dispatcher = UnixEventDispatcher::new();
    assert!(conn.bind_event_dispatcher(&mut dispatcher).is_some());
    drop(conn);
    server.join().unwrap();
    let _ = std::fs::remove_dir_all(&dir);

    let name = format!("ferrokit-dbus-test-{}", std::process::id());
    let addr = SocketAddr::from_abstract_name(name.as_bytes()).unwrap();
    let server = serve_scripted_bus(UnixListener::bind_addr(&addr).unwrap());
    let conn = DbusConnection::connect_to_address(&format!("unix:abstract={}", name))
        .expect("connect over an abstract socket");
    assert_eq!(conn.unique_name(), ":1.99");
    drop(conn);
    server.join().unwrap();

    assert!(DbusConnection::connect_to_address("unix:path=/nonexistent/bus").is_err());
    assert!(DbusConnection::connect_to_address("tcp:host=localhost,port=1").is_err());
}

#[cfg(not(target_os = "linux"))]
#[test]
fn test_dbus_session_bus_is_unavailable_off_linux() {
    assert!(ferrokit_platform::DbusConnection::connect_session_bus().is_err());
}

#[test]
fn test_linux_dbus_status_notifier_item_with_socket_notifier() {
    use ferrokit_core::event_loop::UnixEventDispatcher;
    use ferrokit_gui::paint::Pixmap;
    use ferrokit_gui::tiny_skia::Color;
    use ferrokit_platform::tray::dbus_connection::scripted::ScriptedDbusTransport;
    use ferrokit_platform::{DbusConnection, DbusStatusNotifierItem, PlatformTrayIcon};

    let mut tray = DbusStatusNotifierItem::new("hud_monitor", "HUD System Monitor");
    assert!(!tray.is_registered());
    assert!(!tray.is_connected());

    let (transport, bus) = ScriptedDbusTransport::new(":1.7");
    tray.set_connection(
        DbusConnection::from_transport(Box::new(transport), "scripted:", 1000).unwrap(),
    );

    let mut pixmap = Pixmap::new(24, 24).expect("failed to create pixmap");
    pixmap.fill(Color::from_rgba8(40, 160, 220, 255));
    assert!(tray.set_icon(&pixmap).is_ok());
    assert!(tray.set_tooltip("Running Smoothly").is_ok());

    // show() registers with the StatusNotifierWatcher
    assert!(tray.show().is_ok());
    assert!(tray.is_registered());
    assert!(tray.is_connected());
    assert_eq!(tray.status(), "Active");
    let members: Vec<_> = bus
        .received()
        .iter()
        .filter_map(|m| m.member.clone())
        .collect();
    assert!(members.contains(&"RequestName".to_string()));
    assert!(members.contains(&"RegisterStatusNotifierItem".to_string()));
    assert!(members.contains(&"AddMatch".to_string()));

    // Notify carries the complete `susssasa{sv}i` signature with aligned arguments.
    assert!(tray
        .show_message("Title", "Body", TrayMessageIcon::Information, 3000)
        .is_ok());
    let notify = bus
        .received()
        .into_iter()
        .find(|m| m.member.as_deref() == Some("Notify"))
        .expect("Notify was sent");
    assert_eq!(notify.signature, "susssasa{sv}i");
    assert!(notify.body.len().is_multiple_of(4));

    // The scripted transport has no OS descriptor, so there is nothing to register.
    let mut dispatcher = UnixEventDispatcher::new();
    assert!(tray.bind_event_dispatcher(&mut dispatcher).is_none());

    assert!(tray.hide().is_ok());
    assert_eq!(tray.status(), "Passive");
}

#[test]
fn test_dbus_tray_without_a_bus_is_not_registered() {
    use ferrokit_platform::{DbusStatusNotifierItem, PlatformTrayIcon};

    // No D-Bus session bus on the host (Windows/macOS, or Linux CI without a session):
    // show() must not claim a registration that never happened.
    if ferrokit_platform::DbusConnection::connect_session_bus().is_ok() {
        return;
    }
    let mut tray = DbusStatusNotifierItem::new("no_bus", "No Bus");
    assert!(tray.show().is_ok());
    assert!(!tray.is_registered());
    assert!(!tray.is_connected());
}

// Mock-runtime test. On macOS: examples/appkit_main_thread.rs (`window_title_and_style`,
// `window_level`, `window_show_hide`, `window_event_bridge`) on the main thread.
#[cfg(not(target_os = "macos"))]
#[test]
fn test_macos_objc_runtime_and_cocoa_window_lifecycle() {
    use ferrokit_core::object::ThreadContext;
    use ferrokit_gui::geometry::primitives::Rect;
    use ferrokit_platform::{
        ClosureWindowEventHandler, CocoaNativeEvent, CocoaNativeWindow, MockObjcRuntime,
        PlatformWindow, WindowFlags, WindowSystemEvent, NS_MODAL_PANEL_WINDOW_LEVEL,
    };
    use std::sync::{Arc, Mutex};

    ThreadContext::init_current(true, None);

    let mut win = CocoaNativeWindow::new(
        "macOS HUD Panel",
        Rect::new(100, 100, 600, 400),
        WindowFlags::FRAMELESS | WindowFlags::STAYS_ON_TOP | WindowFlags::CLICK_THROUGH,
    )
    .expect("failed to create macOS Cocoa native window");

    assert_eq!(win.title(), "macOS HUD Panel");
    assert_eq!(win.geometry(), Rect::new(100, 100, 600, 400));
    assert!(!win.is_visible());

    // Verify MockObjcRuntime has created NSWindow and NSView via Objective-C messaging
    let runtime = MockObjcRuntime::instance();
    let window_data = runtime
        .get_object_data(win.ns_window())
        .expect("failed to get NSWindow mock data");
    assert_eq!(window_data.class_name, "NSWindow");
    assert_eq!(window_data.title, "macOS HUD Panel");
    assert_eq!(window_data.level, NS_MODAL_PANEL_WINDOW_LEVEL);
    assert!(window_data.ignores_mouse_events);

    win.show();
    assert!(win.is_visible());

    // Verify AppKit NSEvent translation pipeline (dispatch_cocoa_event)
    let received = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = Arc::clone(&received);
    win.set_event_handler(Box::new(ClosureWindowEventHandler::new(
        move |event| match event {
            WindowSystemEvent::MousePress { pos, .. } => {
                sink.lock()
                    .unwrap()
                    .push(format!("MousePress({},{})", pos.x, pos.y));
            }
            WindowSystemEvent::MouseMove { pos, .. } => {
                sink.lock()
                    .unwrap()
                    .push(format!("MouseMove({},{})", pos.x, pos.y));
            }
            WindowSystemEvent::Wheel { delta, .. } => {
                sink.lock().unwrap().push(format!("Wheel({})", delta.y));
            }
            WindowSystemEvent::Resize { size } => {
                sink.lock()
                    .unwrap()
                    .push(format!("Resize({},{})", size.width, size.height));
            }
            WindowSystemEvent::CloseRequest => {
                sink.lock().unwrap().push("CloseRequest".to_string());
            }
            _ => {}
        },
    )));

    assert!(win.dispatch_cocoa_event(CocoaNativeEvent::MouseDown {
        x: 40.0,
        y: 60.0,
        button: 0,
        modifiers: 0,
    }));
    assert!(win.dispatch_cocoa_event(CocoaNativeEvent::MouseMoved {
        x: 80.0,
        y: 120.0,
        modifiers: 0,
    }));
    assert!(win.dispatch_cocoa_event(CocoaNativeEvent::ScrollWheel {
        x: 80.0,
        y: 120.0,
        delta_x: 0.0,
        delta_y: -15.0,
    }));
    assert!(win.dispatch_cocoa_event(CocoaNativeEvent::WindowResized {
        width: 800.0,
        height: 500.0,
    }));
    assert!(win.dispatch_cocoa_event(CocoaNativeEvent::WindowCloseRequested));

    let events = received.lock().unwrap().clone();
    assert_eq!(
        events,
        vec![
            "MousePress(40,60)",
            "MouseMove(80,120)",
            "Wheel(-15)",
            "Resize(800,500)",
            "CloseRequest"
        ]
    );

    win.hide();
    assert!(!win.is_visible());
}

// Mock-runtime test. On macOS: examples/appkit_main_thread.rs (`status_item_lifecycle`,
// `status_item_menu`) on the main thread.
#[cfg(not(target_os = "macos"))]
#[test]
fn test_macos_cocoa_status_item_and_menu_objc_integration() {
    use ferrokit_core::object::ThreadContext;
    use ferrokit_gui::paint::Pixmap;
    use ferrokit_gui::tiny_skia::Color;
    use ferrokit_platform::menu::{CocoaMenu, PlatformMenu};
    use ferrokit_platform::tray::{CocoaStatusItem, PlatformTrayIcon};
    use ferrokit_platform::MockObjcRuntime;

    ThreadContext::init_current(true, None);

    let mut item = CocoaStatusItem::new(1001);
    assert_eq!(item.item_id(), 1001);
    assert!(!item.native_status_item().is_nil());

    let mut pixmap = Pixmap::new(18, 18).expect("failed to create pixmap");
    pixmap.fill(Color::from_rgba8(255, 255, 255, 255));
    assert!(item.set_icon(&pixmap).is_ok());
    assert!(item.set_tooltip("Claude HUD macOS").is_ok());

    // Verify AppKit NSMenu and NSMenuItem hierarchy
    let mut menu = Box::new(CocoaMenu::new());
    let action1 = menu.add_action(1, "Open Monitor");
    let action2 = menu.add_checkable(2, "Click Through", false);
    menu.add_separator();
    let _action3 = menu.add_action(3, "Quit");

    assert_eq!(action1.text(), "Open Monitor");
    assert!(!action2.is_checked());
    action2.activated(); // Register
    menu.trigger_item(2);
    assert!(action2.is_checked());

    // Bind native menu to status bar item
    item.set_menu(menu);
    assert!(item.show().is_ok());
    assert!(item.is_visible());

    let runtime = MockObjcRuntime::instance();
    let status_data = runtime
        .get_object_data(item.native_status_item())
        .expect("failed to get NSStatusItem data");
    assert_eq!(status_data.class_name, "NSStatusItem");
    assert!(!status_data.button.is_nil());
    assert!(!status_data.menu.is_nil());

    assert!(item.hide().is_ok());
}

#[test]
fn test_cross_platform_input_event_bridge_queuing_and_polling() {
    use ferrokit_core::object::ThreadContext;
    use ferrokit_gui::geometry::primitives::Rect;
    use ferrokit_platform::{
        ClosureWindowEventHandler, GenericWindow, PlatformWindow, WaylandEvent,
        WaylandNativeWindow, WindowFlags, WindowSystemEvent, X11Event, X11NativeWindow,
    };
    use std::sync::{Arc, Mutex};

    ThreadContext::init_current(true, None);

    // 1. Test Linux X11 window input event bridge (queue_x11_event -> poll_events -> WindowSystemEvent)
    let mut x11_win = X11NativeWindow::new_offscreen(
        "X11 Bridge Window",
        Rect::new(0, 0, 800, 600),
        WindowFlags::NORMAL,
    )
    .unwrap();

    let x11_received = Arc::new(Mutex::new(Vec::<String>::new()));
    let x11_sink = Arc::clone(&x11_received);
    x11_win.set_event_handler(Box::new(ClosureWindowEventHandler::new(
        move |event| match event {
            WindowSystemEvent::MousePress { button, .. } => {
                x11_sink
                    .lock()
                    .unwrap()
                    .push(format!("X11:MousePress({:?})", button));
            }
            WindowSystemEvent::KeyPress { key, .. } => {
                x11_sink
                    .lock()
                    .unwrap()
                    .push(format!("X11:KeyPress({})", key));
            }
            _ => {}
        },
    )));

    x11_win.queue_x11_event(X11Event::ButtonPress {
        button: 1,
        x: 100,
        y: 100,
        modifiers: 0,
    });
    x11_win.queue_x11_event(X11Event::KeyPress {
        keycode: 65,
        modifiers: 0,
    });
    assert_eq!(x11_win.poll_events(), 2);
    assert_eq!(x11_win.poll_events(), 0); // Queue drained
    assert_eq!(
        *x11_received.lock().unwrap(),
        vec!["X11:MousePress(Left)", "X11:KeyPress(32)"] // 65 (X11 Space) -> 32 (Qt::Key_Space)
    );

    // 2. Test Linux Wayland window input event bridge (queue_wayland_event -> poll_events)
    let mut wayland_win = WaylandNativeWindow::new_offscreen(
        "Wayland Bridge Window",
        Rect::new(0, 0, 1024, 768),
        WindowFlags::NORMAL,
    )
    .unwrap();

    let wl_received = Arc::new(Mutex::new(Vec::<String>::new()));
    let wl_sink = Arc::clone(&wl_received);
    wayland_win.set_event_handler(Box::new(ClosureWindowEventHandler::new(move |event| {
        if let WindowSystemEvent::MouseMove { pos, .. } = event {
            wl_sink
                .lock()
                .unwrap()
                .push(format!("Wayland:MouseMove({},{})", pos.x, pos.y));
        }
    })));

    wayland_win.queue_wayland_event(WaylandEvent::PointerMotion {
        surface_x: 250,
        surface_y: 350,
        modifiers: 0,
    });
    assert_eq!(wayland_win.poll_events(), 1);
    assert_eq!(
        *wl_received.lock().unwrap(),
        vec!["Wayland:MouseMove(250,350)"]
    );

    // 3. Test macOS Cocoa window input event bridge (queue_cocoa_event -> poll_events) against
    // the mock runtime. On macOS: examples/appkit_main_thread.rs (`window_event_bridge`).
    #[cfg(not(target_os = "macos"))]
    {
        use ferrokit_platform::{CocoaNativeEvent, CocoaNativeWindow};
        let mut cocoa_win = CocoaNativeWindow::new(
            "Cocoa Bridge Window",
            Rect::new(0, 0, 500, 400),
            WindowFlags::NORMAL,
        )
        .unwrap();

        let cocoa_received = Arc::new(Mutex::new(Vec::<String>::new()));
        let cocoa_sink = Arc::clone(&cocoa_received);
        cocoa_win.set_event_handler(Box::new(ClosureWindowEventHandler::new(move |event| {
            if let WindowSystemEvent::MouseRelease { button, .. } = event {
                cocoa_sink
                    .lock()
                    .unwrap()
                    .push(format!("Cocoa:MouseRelease({:?})", button));
            }
        })));

        cocoa_win.queue_cocoa_event(CocoaNativeEvent::MouseUp {
            x: 50.0,
            y: 80.0,
            button: 0,
            modifiers: 0,
        });
        assert_eq!(cocoa_win.poll_events(), 1);
        assert_eq!(
            *cocoa_received.lock().unwrap(),
            vec!["Cocoa:MouseRelease(Left)"]
        );
    }

    // 4. Test Generic window input event queue
    let mut generic_win = GenericWindow::new(
        "Generic Bridge Window",
        Rect::new(0, 0, 300, 300),
        WindowFlags::NORMAL,
    );
    let generic_received = Arc::new(Mutex::new(Vec::<String>::new()));
    let generic_sink = Arc::clone(&generic_received);
    generic_win.set_event_handler(Box::new(ClosureWindowEventHandler::new(move |event| {
        if let WindowSystemEvent::CloseRequest = event {
            generic_sink
                .lock()
                .unwrap()
                .push("Generic:CloseRequest".to_string());
        }
    })));

    generic_win.queue_event(WindowSystemEvent::CloseRequest);
    assert_eq!(generic_win.poll_events(), 1);
    assert_eq!(
        *generic_received.lock().unwrap(),
        vec!["Generic:CloseRequest"]
    );
}

// Mock-runtime test: Qt's QNSView returns YES from -isFlipped (qnsview_drawing.mm:67-70). On macOS
// the real view is checked on the main thread: examples/appkit_main_thread.rs (`window_flipped`).
#[cfg(not(target_os = "macos"))]
#[test]
fn test_macos_content_view_is_flipped() {
    use ferrokit_core::object::ThreadContext;
    use ferrokit_gui::geometry::primitives::Rect;
    use ferrokit_platform::{CocoaNativeWindow, WindowFlags};

    ThreadContext::init_current(true, None);

    let cocoa_win = CocoaNativeWindow::new(
        "Flipped View Test",
        Rect::new(100, 200, 640, 480),
        WindowFlags::NORMAL,
    )
    .expect("failed to create Cocoa native window");

    assert!(
        cocoa_win.is_flipped(),
        "QNSView must return isFlipped == true, flipping Cocoa coordinates with top-left as origin (Y downward)"
    );
}

#[test]
fn test_qt_mac_flip_point_and_rect_are_reversible() {
    use ferrokit_gui::geometry::primitives::{Point, Rect};
    use ferrokit_platform::{qt_mac_flip_point, qt_mac_flip_rect};

    // Verify screen geometry transformation functions (aligned with Qt qt_mac_flip / QCocoaScreen::mapToNative / mapFromNative)
    let screen_height = 1080;
    let qt_pt = Point::new(150, 200);
    let cocoa_pt = qt_mac_flip_point(qt_pt, screen_height);
    assert_eq!(cocoa_pt, Point::new(150, 880));
    // Bidirectional reversibility
    assert_eq!(qt_mac_flip_point(cocoa_pt, screen_height), qt_pt);

    let qt_rect = Rect::new(50, 100, 400, 300);
    let cocoa_rect = qt_mac_flip_rect(qt_rect, screen_height);
    // Cocoa Cartesian bottom-left origin: y = 1080 - (100 + 300) = 680
    assert_eq!(cocoa_rect, Rect::new(50, 680, 400, 300));
    // Bidirectional reversibility
    assert_eq!(qt_mac_flip_rect(cocoa_rect, screen_height), qt_rect);
}

#[test]
fn test_wayland_wheel_delta_scale() {
    use ferrokit_gui::geometry::primitives::Rect;
    use ferrokit_platform::{
        ClosureWindowEventHandler, PlatformWindow, WaylandEvent, WaylandNativeWindow, WheelDelta,
        WindowFlags, WindowSystemEvent,
    };
    use std::sync::{Arc, Mutex};

    // Verify Wayland wheel delta conversion (aligned with Qt qwaylandinputdevice.cpp: WheelDelta::vertical(-value * 12))
    let mut wayland_win = WaylandNativeWindow::new_offscreen(
        "Wayland Wheel Test",
        Rect::new(0, 0, 800, 600),
        WindowFlags::NORMAL,
    )
    .expect("failed to create Wayland window");

    let captured_wheel = Arc::new(Mutex::new(Vec::<WheelDelta>::new()));
    let wheel_sink = Arc::clone(&captured_wheel);
    wayland_win.set_event_handler(Box::new(ClosureWindowEventHandler::new(move |event| {
        if let WindowSystemEvent::Wheel { delta, .. } = event {
            wheel_sink.lock().unwrap().push(delta);
        }
    })));

    // Wheel scrolls forward one step (Wayland accumulated displacement value = -10)
    wayland_win.queue_wayland_event(WaylandEvent::PointerAxis {
        value: -10,
        modifiers: 0,
    });
    // Wheel scrolls backward one step (Wayland accumulated displacement value = 10)
    wayland_win.queue_wayland_event(WaylandEvent::PointerAxis {
        value: 10,
        modifiers: 0,
    });
    assert_eq!(wayland_win.poll_events(), 2);

    let deltas = captured_wheel.lock().unwrap().clone();
    assert_eq!(deltas.len(), 2);
    // -(-10) * 12 = 120 (Standard Windows/Qt forward step)
    assert_eq!(deltas[0], WheelDelta::vertical(120));
    // -(10) * 12 = -120 (Standard Windows/Qt backward step)
    assert_eq!(deltas[1], WheelDelta::vertical(-120));
}
#[test]
fn test_platform_parity_gaps_verification() {
    use ferrokit_core::event_loop::{EpollReactor, SocketEvent, SocketNotifier};
    use ferrokit_platform::tray::dbus_connection::DbusConnection;
    use ferrokit_platform::window::WindowFlags;
    use ferrokit_platform::window_wayland::WaylandNativeWindow;
    use ferrokit_platform::window_x11::X11NativeWindow;

    // 1. Verify ObjcMsg message dispatch and property configuration against the mock runtime.
    // On macOS a real NSWindow belongs to the main thread: examples/appkit_main_thread.rs
    // (`window_title_and_style`, `window_level`).
    #[cfg(not(target_os = "macos"))]
    {
        use ferrokit_platform::objc_runtime::{CGRect, Class, MockObjcRuntime, ObjcMsg, Sel};
        let win_cls = Class::get("NSWindow").unwrap_or(Class::NIL);
        let win_alloc = unsafe { ObjcMsg::send_class_0(win_cls, Sel::register("alloc")) };
        assert!(!win_alloc.is_nil());

        let cg_rect = CGRect::new(10.0, 20.0, 300.0, 200.0);
        let win = unsafe {
            ObjcMsg::send_window_init(
                win_alloc,
                Sel::register("initWithContentRect:styleMask:backing:defer:"),
                cg_rect,
                0,
                2,
                false,
            )
        };
        assert!(!win.is_nil());

        unsafe { ObjcMsg::send_str(win, Sel::register("setTitle:"), "Parity Window") };
        unsafe { ObjcMsg::send_int(win, Sel::register("setLevel:"), 3) };
        unsafe { ObjcMsg::send_bool(win, Sel::register("setIgnoresMouseEvents:"), true) };

        let view_cls = Class::get("QNSView").unwrap_or(Class::NIL);
        let view_alloc = unsafe { ObjcMsg::send_class_0(view_cls, Sel::register("alloc")) };
        let view =
            unsafe { ObjcMsg::send_rect(view_alloc, Sel::register("initWithFrame:"), cg_rect) };
        unsafe { ObjcMsg::send_id(win, Sel::register("setContentView:"), view) };

        let data = MockObjcRuntime::instance()
            .get_object_data(win)
            .expect("window data should exist");
        assert_eq!(data.title, "Parity Window");
        assert_eq!(data.level, 3);
        assert!(data.ignores_mouse_events);
        assert_eq!(data.children.len(), 1);
        assert_eq!(data.children[0], view);
    }

    // 2. Verify D-Bus transport framing against the in-memory bus (the real socket is covered by
    // test_linux_dbus_connects_over_path_and_abstract_unix_sockets).
    {
        use ferrokit_platform::tray::dbus_connection::scripted::ScriptedDbusTransport;
        let (transport, _bus) = ScriptedDbusTransport::new(":1.5");
        let dbus_conn = DbusConnection::from_transport(Box::new(transport), "scripted:", 1000)
            .expect("failed to handshake with the scripted bus");
        assert_eq!(dbus_conn.unique_name(), ":1.5");
        assert!(dbus_conn.has_sent_member("Hello"));
    }

    // 3. Offscreen X11 / Wayland windows own no socket, so nothing can be registered with an
    // event loop under a fabricated descriptor.
    let x11_win = X11NativeWindow::new_offscreen(
        "X11 Parity",
        Rect::new(0, 0, 200, 100),
        WindowFlags::empty(),
    )
    .expect("failed to create offscreen X11 window");
    assert_eq!(x11_win.connection_fd(), -1);
    assert!(!x11_win.is_live_display());

    let wayland_win = WaylandNativeWindow::new_offscreen(
        "Wayland Parity",
        Rect::new(0, 0, 200, 100),
        WindowFlags::empty(),
    )
    .expect("failed to create offscreen Wayland window");
    assert_eq!(wayland_win.connection_fd(), -1);
    assert!(!wayland_win.is_live_compositor());

    // The live constructors succeed exactly when the display socket exists.
    #[cfg(not(target_os = "linux"))]
    {
        assert!(X11NativeWindow::new("x", Rect::new(0, 0, 10, 10), WindowFlags::empty()).is_err());
        assert!(
            WaylandNativeWindow::new("w", Rect::new(0, 0, 10, 10), WindowFlags::empty()).is_err()
        );
    }
    #[cfg(target_os = "linux")]
    {
        let x11_num = std::env::var("DISPLAY")
            .ok()
            .and_then(|d| {
                d.split(':')
                    .nth(1)
                    .and_then(|s| s.split('.').next())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "0".to_string());
        let has_x11_socket = std::path::Path::new(&format!("/tmp/.X11-unix/X{}", x11_num)).exists();
        let live = X11NativeWindow::new("x", Rect::new(0, 0, 10, 10), WindowFlags::empty());
        assert_eq!(live.is_ok(), has_x11_socket);
        if let Ok(live) = live {
            assert!(live.is_live_display());
            assert!(live.connection_fd() >= 0);
        }

        let socket_name =
            std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "wayland-0".to_string());
        let xdg = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| {
            use std::os::unix::fs::MetadataExt;
            let uid = std::fs::metadata("/proc/self")
                .map(|m| m.uid())
                .unwrap_or(0);
            format!("/run/user/{}", uid)
        });
        let has_wayland_socket = std::path::Path::new(&xdg).join(&socket_name).exists();
        let live = WaylandNativeWindow::new("w", Rect::new(0, 0, 10, 10), WindowFlags::empty());
        assert_eq!(live.is_ok(), has_wayland_socket);
        if let Ok(live) = live {
            assert!(live.is_live_compositor());
            assert!(live.connection_fd() >= 0);
        }
    }
    // 4. Verify macOS CocoaLayerSurface CALayer double buffer presentation against the mock
    // runtime (layer id 1234 is not a real object). On macOS: examples/appkit_main_thread.rs
    // (`window_present`) presents to a real window on the main thread.
    #[cfg(not(target_os = "macos"))]
    {
        use ferrokit_gui::paint::Pixmap;
        use ferrokit_platform::surface::macos::CocoaLayerSurface;
        use ferrokit_platform::surface::PlatformSurface;
        let mut surface =
            CocoaLayerSurface::new(1234, 100, 100).expect("failed to create CocoaLayerSurface");
        let mut pixmap = Pixmap::new(100, 100).expect("failed to create pixmap");
        pixmap.fill(Color::from_rgba8(255, 0, 0, 255));
        surface
            .present(&mut pixmap, 0.85)
            .expect("present should succeed to CALayer");
        assert_eq!(surface.pixel_buffer().len(), 100 * 100 * 4);

        surface
            .present_dirty(&mut pixmap, 0.90, Rect::new(10, 10, 50, 50))
            .expect("present_dirty should succeed");
    }

    // 5. Verify Unix EpollReactor core syscalls and reactor operation
    let reactor = EpollReactor::new();
    let notifier = std::sync::Arc::new(SocketNotifier::new(42, SocketEvent::Read));
    reactor.register_socket_notifier(&notifier);
    reactor.trigger_socket_event(42, SocketEvent::Read);

    let (was_awoken, timers, sockets) =
        reactor.epoll_wait(Some(std::time::Duration::from_millis(10)));
    assert!(!was_awoken);
    assert!(timers.is_empty());
    assert_eq!(sockets.len(), 1);
    assert_eq!(sockets[0], (42, SocketEvent::Read));

    reactor.unregister_socket_notifier(&notifier);
}
#[test]
fn test_x11_keymapper_wayland_buffer_release_and_cocoa_multi_monitor() {
    use ferrokit_gui::geometry::primitives::{Point, Rect};
    use ferrokit_gui::paint::Pixmap;
    use ferrokit_platform::window::WindowFlags;
    use ferrokit_platform::window_cocoa::{
        qt_mac_flip_global_point, qt_mac_flip_global_rect, qt_mac_primary_screen_height,
    };
    use ferrokit_platform::window_system_interface::{WindowSystemEvent, WindowSystemEventHandler};
    use ferrokit_platform::window_wayland::{WaylandEvent, WaylandNativeWindow};
    use ferrokit_platform::window_x11::{qt_key, x11_keycode_to_qt_key, X11Event, X11NativeWindow};

    // 1. Verify X11 hardware keycode to Qt::Key translation
    assert_eq!(x11_keycode_to_qt_key(9), qt_key::KEY_ESCAPE);
    assert_eq!(x11_keycode_to_qt_key(22), qt_key::KEY_BACKSPACE);
    assert_eq!(x11_keycode_to_qt_key(23), qt_key::KEY_TAB);
    assert_eq!(x11_keycode_to_qt_key(36), qt_key::KEY_RETURN);
    assert_eq!(x11_keycode_to_qt_key(65), qt_key::KEY_SPACE);
    assert_eq!(x11_keycode_to_qt_key(111), qt_key::KEY_UP);
    assert_eq!(x11_keycode_to_qt_key(116), qt_key::KEY_DOWN);
    assert_eq!(x11_keycode_to_qt_key(24), 0x51); // 'Q'

    // Verify X11NativeWindow automatically translates hardware keycode to Qt::Key during event dispatch
    let mut x11_win =
        X11NativeWindow::new_offscreen("Key Test", Rect::new(0, 0, 100, 100), WindowFlags::empty())
            .unwrap();
    let captured_keys = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let keys_clone = captured_keys.clone();

    struct KeyHandler(std::sync::Arc<std::sync::Mutex<Vec<u32>>>);
    impl WindowSystemEventHandler for KeyHandler {
        fn handle_window_event(&mut self, event: WindowSystemEvent) {
            if let WindowSystemEvent::KeyPress { key, .. } = event {
                self.0.lock().unwrap().push(key);
            }
        }
    }

    x11_win.set_event_handler(Box::new(KeyHandler(keys_clone)));
    x11_win.queue_x11_event(X11Event::KeyPress {
        keycode: 9,
        modifiers: 0,
    }); // Esc
    x11_win.queue_x11_event(X11Event::KeyPress {
        keycode: 36,
        modifiers: 0,
    }); // Enter
    x11_win.queue_x11_event(X11Event::KeyPress {
        keycode: 65,
        modifiers: 0,
    }); // Space
    assert_eq!(x11_win.poll_events(), 3);

    let keys = captured_keys.lock().unwrap().clone();
    assert_eq!(
        keys,
        vec![qt_key::KEY_ESCAPE, qt_key::KEY_RETURN, qt_key::KEY_SPACE]
    );

    // 2. Verify Wayland wl_buffer.release busy and unlock lifecycle
    let mut wayland_win = WaylandNativeWindow::new_offscreen(
        "Buffer Test",
        Rect::new(0, 0, 200, 150),
        WindowFlags::empty(),
    )
    .unwrap();
    let mut pixmap = Pixmap::new(200, 150).unwrap();
    wayland_win.present(&mut pixmap, 1.0).unwrap();

    // Buffer is in busy state after present
    assert!(wayland_win.is_buffer_busy());
    // Simulate compositor sending wl_buffer.release (WaylandEvent::BufferRelease)
    wayland_win.queue_wayland_event(WaylandEvent::BufferRelease);
    assert_eq!(wayland_win.poll_events(), 1);

    // Unlocked to free state after receiving release event
    assert!(!wayland_win.is_buffer_busy());
    // 3. Verify macOS primary screen height origin multi-screen coordinate conversion
    let primary_height = qt_mac_primary_screen_height();
    assert!(primary_height > 0);

    let original_pt = Point::new(100, 200);
    let flipped_pt = qt_mac_flip_global_point(original_pt);
    assert_eq!(flipped_pt, Point::new(100, primary_height - 200));

    let original_rect = Rect::new(50, 100, 400, 300);
    let flipped_rect = qt_mac_flip_global_rect(original_rect);
    assert_eq!(flipped_rect.x, 50);
    assert_eq!(flipped_rect.y, primary_height - 400);
    assert_eq!(flipped_rect.width, 400);
    assert_eq!(flipped_rect.height, 300);
}
