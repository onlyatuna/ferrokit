// SPDX-License-Identifier: LGPL-3.0-only
use ferrokit_gui::geometry::primitives::{Point, Rect};
use ferrokit_platform::backdrop::BackdropType;
use ferrokit_platform::drag_drop::{DropAction, DropEvent};
use ferrokit_platform::ime::CompositionContext;
use ferrokit_platform::window_system_interface::{WindowSystemEvent, WindowSystemEventHandler};
use ferrokit_platform::{PlatformWindow, WindowFlags};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[test]
fn test_backdrop_type_variants() {
    let none = BackdropType::None;
    let mica = BackdropType::Mica;
    let mica_alt = BackdropType::MicaAlt;
    let acrylic = BackdropType::Acrylic;
    let blur_behind = BackdropType::BlurBehind;

    assert_ne!(none, mica);
    assert_ne!(mica, mica_alt);
    assert_ne!(acrylic, blur_behind);

    // Calling with a null/dummy handle should safely return false without crashing
    #[cfg(windows)]
    {
        use ferrokit_platform::backdrop::set_window_backdrop;
        let null_hwnd = std::ptr::null_mut();
        assert!(!set_window_backdrop(null_hwnd, BackdropType::Mica, true));
        assert!(!set_window_backdrop(
            null_hwnd,
            BackdropType::Acrylic,
            false
        ));
    }
}

#[test]
fn test_ime_composition_context() {
    let mut ctx = CompositionContext::default();
    assert!(!ctx.is_composing);
    assert_eq!(ctx.composition_string, "");
    assert_eq!(ctx.cursor_position, 0);

    ctx.is_composing = true;
    ctx.composition_string = "nihao".to_string();
    ctx.cursor_position = 5;

    assert!(ctx.is_composing);
    assert_eq!(ctx.composition_string, "nihao");
    assert_eq!(ctx.cursor_position, 5);
}

#[test]
fn test_drag_drop_actions_and_events() {
    assert_eq!(DropAction::Ignore as u32, 0);
    assert_eq!(DropAction::Copy as u32, 1);
    assert_eq!(DropAction::Move as u32, 2);
    assert_eq!(DropAction::Link as u32, 4);

    let enter_ev = DropEvent::Enter {
        pos: Point::new(100, 200),
        formats: vec!["text/uri-list".to_string()],
        effect: 1,
    };

    match enter_ev {
        DropEvent::Enter {
            pos,
            formats,
            effect,
        } => {
            assert_eq!(pos, Point::new(100, 200));
            assert_eq!(formats, vec!["text/uri-list".to_string()]);
            assert_eq!(effect, 1);
        }
        _ => panic!("Expected DropEvent::Enter"),
    }
}

#[test]
fn test_dpi_change_and_ime_events_in_window_system() {
    struct MockEventHandler {
        received_dpi: Arc<AtomicBool>,
        received_ime: Arc<AtomicBool>,
        received_drop: Arc<AtomicBool>,
    }

    impl WindowSystemEventHandler for MockEventHandler {
        fn handle_window_event(&mut self, event: WindowSystemEvent) {
            match event {
                WindowSystemEvent::DpiChanged { dpi_x, dpi_y } => {
                    if dpi_x == 192 && dpi_y == 192 {
                        self.received_dpi.store(true, Ordering::SeqCst);
                    }
                }
                WindowSystemEvent::InputMethod {
                    commit_string,
                    preedit_string,
                    ..
                } => {
                    if commit_string == "你好" || preedit_string == "nihao" {
                        self.received_ime.store(true, Ordering::SeqCst);
                    }
                }
                WindowSystemEvent::Drop { pos, formats, .. }
                    if pos == Point::new(50, 50) && formats.contains(&"text/plain".to_string()) =>
                {
                    self.received_drop.store(true, Ordering::SeqCst);
                }
                _ => {}
            }
        }
    }

    let dpi_flag = Arc::new(AtomicBool::new(false));
    let ime_flag = Arc::new(AtomicBool::new(false));
    let drop_flag = Arc::new(AtomicBool::new(false));

    let mut handler = MockEventHandler {
        received_dpi: dpi_flag.clone(),
        received_ime: ime_flag.clone(),
        received_drop: drop_flag.clone(),
    };

    // Dispatch DPI Changed
    handler.handle_window_event(WindowSystemEvent::DpiChanged {
        dpi_x: 192,
        dpi_y: 192,
    });
    assert!(dpi_flag.load(Ordering::SeqCst));

    // Dispatch IME event
    handler.handle_window_event(WindowSystemEvent::InputMethod {
        commit_string: "你好".to_string(),
        preedit_string: "".to_string(),
        cursor_position: 2,
    });
    assert!(ime_flag.load(Ordering::SeqCst));

    // Dispatch Drop event
    handler.handle_window_event(WindowSystemEvent::Drop {
        pos: Point::new(50, 50),
        formats: vec!["text/plain".to_string()],
        data: vec![("text/plain".to_string(), b"hello".to_vec())],
        drop_action: 1,
    });
    assert!(drop_flag.load(Ordering::SeqCst));
}

#[test]
#[cfg(windows)]
fn test_platform_window_advanced_features_support() {
    let mut win = ferrokit_platform::window::NativeWindow::new(
        "TestAdvancedFeatures",
        Rect::new(100, 100, 400, 300),
        WindowFlags::FRAMELESS,
    )
    .expect("Window creation should succeed");

    // Test backdrop call
    let _ = win.set_backdrop(BackdropType::Mica, false);

    // Test IME microfocus positioning
    win.set_ime_focus(Point::new(120, 80));

    // Test drop target toggle
    let _ = win.enable_drop_target(true);
    let _ = win.enable_drop_target(false);
}

#[test]
fn test_cross_platform_screens_parity() {
    use ferrokit_platform::screen::{PlatformScreen, X11Screen};

    // Test Cocoa screen against the mock runtime (its 1920x1080 at 2.0 is the mock's fallback).
    // On macOS: examples/appkit_main_thread.rs (`screen_metrics`) on the main thread.
    #[cfg(not(target_os = "macos"))]
    {
        use ferrokit_platform::screen::CocoaScreen;
        let cocoa_primary = CocoaScreen::primary();
        assert!(cocoa_primary.is_primary());
        assert_eq!(cocoa_primary.geometry().width, 1920);
        assert_eq!(cocoa_primary.geometry().height, 1080);
        assert!(cocoa_primary.available_geometry().height <= cocoa_primary.geometry().height);
        assert_eq!(cocoa_primary.device_pixel_ratio(), 2.0);

        let cocoa_screens = CocoaScreen::screens();
        assert!(!cocoa_screens.is_empty());

        let at_origin = CocoaScreen::screen_at(Point::new(100, 100));
        assert!(at_origin.is_some());
    }

    // Test X11 screen
    let x11_primary = X11Screen::primary();
    assert!(x11_primary.is_primary());
    assert_eq!(x11_primary.geometry().width, 1920);
    assert_eq!(x11_primary.geometry().height, 1080);
    assert_eq!(x11_primary.device_pixel_ratio(), 1.0);

    let x11_screens = X11Screen::screens();
    assert_eq!(x11_screens.len(), 1);

    let x11_at = X11Screen::screen_at(Point::new(500, 500));
    assert!(x11_at.is_some());
}

#[test]
fn test_cross_platform_backdrop_and_click_through() {
    use ferrokit_platform::window_wayland::WaylandNativeWindow;
    use ferrokit_platform::window_x11::X11NativeWindow;

    let rect = Rect::new(50, 50, 600, 400);

    // Cocoa window against the mock runtime. On macOS: examples/appkit_main_thread.rs
    // (`backdrop_on`, `backdrop_off`, `window_click_through`) on the main thread.
    #[cfg(not(target_os = "macos"))]
    {
        use ferrokit_platform::window_cocoa::CocoaNativeWindow;
        // Qt: native windows belong to the thread that constructed the application
        // (qcoreapplication.cpp:475, 531-537), so the application comes first on this thread.
        let _app = ferrokit_gui::application::GuiApplication::new(vec![
            "test_platform_modern_features".to_string(),
        ]);
        let mut cocoa_win = CocoaNativeWindow::new("CocoaHUD", rect, WindowFlags::FRAMELESS)
            .expect("Cocoa window creation should succeed");
        assert!(cocoa_win.set_backdrop(BackdropType::Acrylic, true));
        assert!(cocoa_win.set_backdrop(BackdropType::None, false));
        cocoa_win.set_click_through(true);
        cocoa_win.set_click_through(false);
    }

    // X11 window
    let mut x11_win = X11NativeWindow::new_offscreen("X11HUD", rect, WindowFlags::FRAMELESS)
        .expect("X11 window creation should succeed");
    assert!(x11_win.set_backdrop(BackdropType::Acrylic, true));
    assert!(x11_win.set_backdrop(BackdropType::None, false));
    x11_win.set_click_through(true);
    x11_win.set_click_through(false);

    // Wayland window
    let mut wayland_win =
        WaylandNativeWindow::new_offscreen("WaylandHUD", rect, WindowFlags::FRAMELESS)
            .expect("Wayland window creation should succeed");
    assert!(wayland_win.set_backdrop(BackdropType::Acrylic, true));
    assert!(wayland_win.set_backdrop(BackdropType::None, false));
    wayland_win.set_click_through(true);
    wayland_win.set_click_through(false);
}

#[test]
fn test_cross_platform_theme_and_dynamic_detection() {
    use ferrokit_platform::theme::{ColorScheme, PlatformTheme, UnixTheme};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    // CocoaTheme against the mock runtime. On macOS (NSApplication's appearance):
    // examples/appkit_main_thread.rs (`theme_color_scheme`) on the main thread.
    #[cfg(not(target_os = "macos"))]
    {
        use ferrokit_platform::theme::CocoaTheme;
        let cocoa_theme = CocoaTheme::new();
        assert_ne!(cocoa_theme.color_scheme(), ColorScheme::Unknown);
        let cocoa_count = Arc::new(AtomicUsize::new(0));
        let c_cnt = cocoa_count.clone();
        cocoa_theme.theme_changed().connect(move |_| {
            c_cnt.fetch_add(1, Ordering::SeqCst);
        });
        cocoa_theme.set_color_scheme(ColorScheme::Light);
        assert_eq!(cocoa_theme.color_scheme(), ColorScheme::Light);
        cocoa_theme.set_color_scheme(ColorScheme::Dark);
        assert_eq!(cocoa_theme.color_scheme(), ColorScheme::Dark);
        assert!(cocoa_count.load(Ordering::SeqCst) >= 1);
    }

    // UnixTheme
    // Unknown is a valid answer: with no portal and no GTK settings (a CI runner) Qt's GNOME
    // theme does not know either.
    let unix_theme = UnixTheme::new();
    assert!(matches!(
        unix_theme.color_scheme(),
        ColorScheme::Light | ColorScheme::Dark | ColorScheme::Unknown
    ));
    let unix_count = Arc::new(AtomicUsize::new(0));
    let u_cnt = unix_count.clone();
    unix_theme.theme_changed().connect(move |_| {
        u_cnt.fetch_add(1, Ordering::SeqCst);
    });
    unix_theme.set_color_scheme(ColorScheme::Light);
    assert_eq!(unix_theme.color_scheme(), ColorScheme::Light);
    unix_theme.set_color_scheme(ColorScheme::Dark);
    assert_eq!(unix_theme.color_scheme(), ColorScheme::Dark);
    assert!(unix_count.load(Ordering::SeqCst) >= 1);
}

#[test]
fn test_cross_platform_cursor_shapes() {
    use ferrokit_platform::cursor::{CursorShape, PlatformCursor, UnixCursor};

    // CocoaCursor against the mock runtime. On macOS (NSCursor): examples/appkit_main_thread.rs
    // (`cursor_shape`) on the main thread.
    #[cfg(not(target_os = "macos"))]
    {
        use ferrokit_platform::cursor::CocoaCursor;
        let mut cocoa_cursor = CocoaCursor::new();
        assert_eq!(cocoa_cursor.current_shape(), CursorShape::Arrow);
        cocoa_cursor.change_cursor(CursorShape::SizeHor);
        assert_eq!(cocoa_cursor.current_shape(), CursorShape::SizeHor);
        cocoa_cursor.change_cursor(CursorShape::SizeVer);
        assert_eq!(cocoa_cursor.current_shape(), CursorShape::SizeVer);
        cocoa_cursor.change_cursor(CursorShape::PointingHand);
        assert_eq!(cocoa_cursor.current_shape(), CursorShape::PointingHand);
    }

    let mut unix_cursor = UnixCursor::new();
    assert_eq!(unix_cursor.current_shape(), CursorShape::Arrow);
    unix_cursor.change_cursor(CursorShape::SizeHor);
    assert_eq!(unix_cursor.current_shape(), CursorShape::SizeHor);
    assert_eq!(UnixCursor::cursor_name(CursorShape::SizeHor), "ew-resize");
    assert_eq!(UnixCursor::cursor_name(CursorShape::SizeVer), "ns-resize");
    assert_eq!(
        UnixCursor::cursor_name(CursorShape::SizeFDiag),
        "nwse-resize"
    );
    assert_eq!(
        UnixCursor::cursor_name(CursorShape::SizeBDiag),
        "nesw-resize"
    );
    assert_eq!(
        UnixCursor::cursor_name(CursorShape::PointingHand),
        "pointer"
    );
    assert_eq!(UnixCursor::cursor_name(CursorShape::Arrow), "default");
}

#[test]
fn test_cross_platform_hotkeys() {
    use ferrokit_platform::hotkey::{HotkeyModifiers, PlatformHotkeyManager, UnixHotkeyManager};
    use ferrokit_platform::integration::{PlatformIntegration, UnixPlatformIntegration};

    // Cocoa hotkeys: off macOS the manager has no OS back end and every registration fails. On
    // macOS it registers real Carbon hotkeys (the mock-backed lifecycle is in
    // tests/test_cocoa_hotkey.rs).
    #[cfg(not(target_os = "macos"))]
    {
        use ferrokit_platform::hotkey::CocoaHotkeyManager;
        use ferrokit_platform::integration::CocoaPlatformIntegration;

        let cocoa_integration = CocoaPlatformIntegration::default();
        let mut cocoa_mgr = cocoa_integration
            .create_hotkey_manager()
            .expect("Cocoa hotkey manager");
        assert!(cocoa_mgr
            .register_hotkey(9527, HotkeyModifiers::ALT, 8)
            .is_err());
        assert!(cocoa_mgr
            .register_hotkey(9528, HotkeyModifiers::ALT | HotkeyModifiers::SHIFT, 8)
            .is_err());
        assert!(cocoa_mgr.unregister_hotkey(9527).is_err());
        assert!(cocoa_mgr.unregister_hotkey(9528).is_err());

        let mut standalone_cocoa = CocoaHotkeyManager::new();
        assert!(standalone_cocoa
            .register_hotkey(1, HotkeyModifiers::ALT, 8)
            .is_err());
        assert_eq!(standalone_cocoa.registered_ids().len(), 0);
    }

    // Unix hotkeys
    let unix_integration = UnixPlatformIntegration::default();
    let mut unix_mgr = unix_integration
        .create_hotkey_manager()
        .expect("Unix hotkey manager");
    assert!(unix_mgr
        .register_hotkey(9527, HotkeyModifiers::ALT, 67)
        .is_err());
    assert!(unix_mgr
        .register_hotkey(9528, HotkeyModifiers::ALT | HotkeyModifiers::SHIFT, 67)
        .is_err());
    assert!(unix_mgr.unregister_hotkey(9527).is_err());
    assert!(unix_mgr.unregister_hotkey(9528).is_err());

    let mut standalone_unix = UnixHotkeyManager::new();
    assert!(standalone_unix
        .register_hotkey(2, HotkeyModifiers::ALT, 67)
        .is_err());
    assert_eq!(standalone_unix.registered_ids().len(), 0);
}

#[test]
fn test_platform_window_move_resize_opacity_minsize() {
    ferrokit_core::object::ThreadContext::init_current(true, None);
    use ferrokit_platform::{
        GenericWindow, PlatformWindow, WaylandNativeWindow, WindowEdges, X11NativeWindow,
    };

    // WindowEdges bitflags tests
    assert_eq!(WindowEdges::LEFT.bits(), 0x01);
    assert_eq!(WindowEdges::TOP.bits(), 0x02);
    assert_eq!(WindowEdges::RIGHT.bits(), 0x04);
    assert_eq!(WindowEdges::BOTTOM.bits(), 0x08);
    assert_eq!(WindowEdges::TOP_LEFT, WindowEdges::TOP | WindowEdges::LEFT);
    assert_eq!(
        WindowEdges::BOTTOM_RIGHT,
        WindowEdges::BOTTOM | WindowEdges::RIGHT
    );

    // GenericWindow tests
    let mut gw = GenericWindow::new(
        "Generic Test",
        Rect::new(0, 0, 800, 600),
        WindowFlags::empty(),
    );
    assert_eq!(gw.minimum_size(), (0, 0));
    gw.set_minimum_size(300, 200);
    assert_eq!(gw.minimum_size(), (300, 200));
    assert_eq!(gw.opacity(), 1.0);
    gw.set_opacity(0.85);
    assert!((gw.opacity() - 0.85).abs() < 1e-4);
    gw.set_opacity(1.5);
    assert_eq!(gw.opacity(), 1.0);
    gw.set_opacity(-0.5);
    assert_eq!(gw.opacity(), 0.0);
    assert!(gw.start_system_move());
    assert!(gw.start_system_resize(WindowEdges::BOTTOM_RIGHT));

    // CocoaNativeWindow against the mock runtime. On macOS: examples/appkit_main_thread.rs
    // (`window_opacity_min_size_move`) on the main thread.
    #[cfg(not(target_os = "macos"))]
    if let Ok(mut cocoa_win) = ferrokit_platform::CocoaNativeWindow::new(
        "Cocoa Test",
        Rect::new(0, 0, 640, 480),
        WindowFlags::empty(),
    ) {
        assert_eq!(cocoa_win.minimum_size(), (0, 0));
        cocoa_win.set_minimum_size(200, 150);
        assert_eq!(cocoa_win.minimum_size(), (200, 150));
        cocoa_win.set_opacity(0.75);
        assert!((cocoa_win.opacity() - 0.75).abs() < 1e-4);
        // No mouse button is pressed (Qt: qcocoawindow.mm:366-370) and Cocoa has no system
        // resize (qplatformwindow.cpp:495-498).
        assert!(!cocoa_win.start_system_move());
        assert!(!cocoa_win.start_system_resize(WindowEdges::RIGHT));
    }

    // X11NativeWindow tests
    let mut x11_win =
        X11NativeWindow::new_offscreen("X11 Test", Rect::new(0, 0, 640, 480), WindowFlags::empty())
            .unwrap();
    assert_eq!(x11_win.minimum_size(), (0, 0));
    x11_win.set_minimum_size(100, 100);
    assert_eq!(x11_win.minimum_size(), (100, 100));
    x11_win.set_opacity(0.5);
    assert!((x11_win.opacity() - 0.5).abs() < 1e-4);
    assert!(x11_win.start_system_move());
    assert!(x11_win.start_system_resize(WindowEdges::BOTTOM));

    // WaylandNativeWindow tests
    let mut wl_win = WaylandNativeWindow::new_offscreen(
        "Wayland Test",
        Rect::new(0, 0, 640, 480),
        WindowFlags::empty(),
    )
    .unwrap();
    assert_eq!(wl_win.minimum_size(), (0, 0));
    wl_win.set_minimum_size(120, 80);
    assert_eq!(wl_win.minimum_size(), (120, 80));
    wl_win.set_opacity(0.9);
    assert!((wl_win.opacity() - 0.9).abs() < 1e-4);
    assert!(wl_win.start_system_move());
    assert!(wl_win.start_system_resize(WindowEdges::TOP_RIGHT));

    #[cfg(windows)]
    {
        use ferrokit_platform::NativeWindow;
        if let Ok(mut win) =
            NativeWindow::new("Win Test", Rect::new(0, 0, 400, 300), WindowFlags::empty())
        {
            assert_eq!(win.minimum_size(), (0, 0));
            win.set_minimum_size(250, 150);
            assert_eq!(win.minimum_size(), (250, 150));
            win.set_opacity(0.8);
            assert!((win.opacity() - 0.8).abs() < 1e-4);
            assert!(win.start_system_move());
            assert!(win.start_system_resize(WindowEdges::BOTTOM_RIGHT));
        }
    }
}

#[test]
fn test_platform_tray_messages() {
    ferrokit_core::object::ThreadContext::init_current(true, None);
    use ferrokit_gui::paint::Pixmap;
    use ferrokit_platform::{
        DbusStatusNotifierItem, GenericTrayIcon, PlatformTrayIcon, TrayMessageIcon,
    };

    // 1. Verify TrayMessageIcon variants
    assert_eq!(TrayMessageIcon::NoIcon as u32, 0);
    assert_eq!(TrayMessageIcon::Information as u32, 1);
    assert_eq!(TrayMessageIcon::Warning as u32, 2);
    assert_eq!(TrayMessageIcon::Critical as u32, 3);

    // 2. GenericTrayIcon
    let pm = Pixmap::new(16, 16).unwrap();
    let mut generic_tray =
        GenericTrayIcon::new("Generic Tray", &ferrokit_gui::image::Icon::from_pixmap(&pm));
    assert!(generic_tray.supports_messages());
    assert_eq!(generic_tray.last_message(), None);
    assert!(generic_tray
        .show_message("Title", "Body", TrayMessageIcon::Information, 5000)
        .is_ok());
    assert_eq!(
        generic_tray.last_message(),
        Some(("Title", "Body", TrayMessageIcon::Information, 5000))
    );

    // 3. CocoaStatusItem against the mock runtime. On macOS: examples/appkit_main_thread.rs
    // (`status_item_message`) on the main thread.
    #[cfg(not(target_os = "macos"))]
    {
        let mut cocoa_tray = ferrokit_platform::CocoaStatusItem::new(101);
        assert!(cocoa_tray.supports_messages());
        assert_eq!(cocoa_tray.last_message(), None);
        assert!(cocoa_tray
            .show_message("ClaudeHUD", "Quota reached", TrayMessageIcon::Warning, 8000)
            .is_ok());
        assert_eq!(
            cocoa_tray.last_message(),
            Some(("ClaudeHUD", "Quota reached", TrayMessageIcon::Warning, 8000))
        );
    }

    // 4. DbusStatusNotifierItem
    let mut dbus_tray = DbusStatusNotifierItem::new("claude-hud", "ClaudeHUD");
    assert!(dbus_tray.supports_messages());
    assert_eq!(dbus_tray.last_message(), None);
    assert!(dbus_tray
        .show_message("Alert", "Memory trimmed", TrayMessageIcon::Critical, 0)
        .is_ok());
    assert_eq!(
        dbus_tray.last_message(),
        Some(("Alert", "Memory trimmed", TrayMessageIcon::Critical, 10000))
    );

    // 5. Windows TrayIcon
    #[cfg(windows)]
    {
        use ferrokit_platform::tray_icon::TrayIcon;
        let hicon = std::ptr::null_mut();
        if let Ok(win_tray) = TrayIcon::new("WinTray", hicon) {
            assert!(win_tray.supports_messages());
            let clicked_signal = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let flag = clicked_signal.clone();
            let _conn = win_tray.on_message_clicked.connect(move |()| {
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
            });
            win_tray.on_message_clicked.emit(&());
            assert!(clicked_signal.load(std::sync::atomic::Ordering::SeqCst));
        }
    }
}

#[test]
fn test_desktop_services() {
    use ferrokit_platform::{open_file, open_url, set_url_handler};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    // 1. Desktop services: custom URL handler interception
    let intercepted = Arc::new(AtomicBool::new(false));
    let flag = intercepted.clone();
    set_url_handler(Some(Box::new(move |url: &str| {
        if url == "https://claude.ai" {
            flag.store(true, Ordering::SeqCst);
            true
        } else {
            false
        }
    })));

    assert!(open_url("https://claude.ai"));
    assert!(intercepted.load(Ordering::SeqCst));
    assert!(!open_url("")); // Empty url returns false

    // Reset handler
    set_url_handler(None);

    // 2. Desktop services: open_file non-existent
    assert!(!open_file(
        "C:/non_existent_folder_xyz_12345/non_existent.txt"
    ));
}

#[test]
fn test_power_events_and_screen_clamping() {
    ferrokit_core::object::ThreadContext::init_current(true, None);
    use ferrokit_gui::geometry::primitives::Rect;
    use ferrokit_platform::{
        clamp_window_rect_to_screens, ensure_within_screens, GenericScreen, PlatformScreen,
        PowerEvent, WindowSystemEvent,
    };

    // 1. PowerEvent variants
    let suspend = PowerEvent::Suspend;
    let resume = PowerEvent::Resume;
    assert_ne!(suspend, resume);

    let power_ev = WindowSystemEvent::Power { event: resume };
    match power_ev {
        WindowSystemEvent::Power { event } => assert_eq!(event, PowerEvent::Resume),
        _ => panic!("Expected Power event"),
    }

    // 2. ensure_within_screens testing
    let screen1 = GenericScreen::new(
        "Screen1",
        Rect::new(0, 0, 1920, 1080),
        Rect::new(0, 0, 1920, 1040),
        true,
        1.0,
    );
    let screen2 = GenericScreen::new(
        "Screen2",
        Rect::new(1920, 0, 1920, 1080),
        Rect::new(1920, 0, 1920, 1040),
        false,
        1.0,
    );
    let screens: Vec<&dyn PlatformScreen> = vec![&screen1, &screen2];

    // Case A: rect comfortably within screen1
    let normal_rect = Rect::new(100, 100, 400, 300);
    let clamped_normal = ensure_within_screens(normal_rect, &screens, 32);
    assert_eq!(clamped_normal, normal_rect);

    // Case B: rect overflowing right side of screen1 into screen2
    let overflow_rect = Rect::new(1800, 100, 400, 300);
    let clamped_b = ensure_within_screens(overflow_rect, &screens, 32);
    assert!(clamped_b.x >= 1920 && clamped_b.x + clamped_b.width <= 3840);

    // Case C: rect completely off-screen (e.g. disconnected monitor at 5000, 5000)
    let disconnected_rect = Rect::new(5000, 5000, 400, 300);
    let recovered = ensure_within_screens(disconnected_rect, &screens, 32);
    // Must be safely relocated inside primary screen (screen1: 0..1920, 0..1040)
    assert!(recovered.x >= 0 && recovered.x + recovered.width <= 1920);
    assert!(recovered.y >= 0 && recovered.y + recovered.height <= 1040);
    assert_eq!(recovered.width, 400);
    assert_eq!(recovered.height, 300);

    // 3. clamp_window_rect_to_screens live check
    let live_clamped = clamp_window_rect_to_screens(Rect::new(100, 100, 300, 200));
    assert!(live_clamped.width > 0 && live_clamped.height > 0);

    // 4. Windows TrayIcon on_power_event check
    #[cfg(windows)]
    {
        use ferrokit_platform::tray_icon::TrayIcon;
        let hicon = std::ptr::null_mut();
        if let Ok(win_tray) = TrayIcon::new("PowerTray", hicon) {
            let received = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let flag = received.clone();
            let _conn = win_tray.on_power_event.connect(move |ev| {
                if *ev == PowerEvent::Resume {
                    flag.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            });
            win_tray.on_power_event.emit(&PowerEvent::Resume);
            assert!(received.load(std::sync::atomic::Ordering::SeqCst));
        }
    }
}

#[test]
fn test_event_delivery_policies() {
    #[cfg(windows)]
    {
        use ferrokit_platform::integration::platform;
        use ferrokit_platform::window_system_interface::{
            ClosureWindowEventHandler, Delivery, WindowSystemEvent,
        };
        use ferrokit_platform::{
            dispatch_window_system_event, flush_window_system_events, handle_geometry_change,
        };
        use std::sync::{Arc, Mutex};

        let p = platform();
        let mut win = p
            .create_window(
                "Delivery Test Window",
                Rect::new(50, 50, 300, 200),
                WindowFlags::NORMAL,
            )
            .expect("create window");
        let hwnd = win.native_handle() as windows_sys::Win32::Foundation::HWND;

        let events = Arc::new(Mutex::new(Vec::<WindowSystemEvent>::new()));
        let events_clone = Arc::clone(&events);
        win.set_event_handler(Box::new(ClosureWindowEventHandler::new(move |ev| {
            events_clone.lock().unwrap().push(ev);
        })));

        // 1. Synchronous delivery: dispatched immediately
        dispatch_window_system_event(Delivery::Synchronous, hwnd, WindowSystemEvent::FocusIn);
        assert_eq!(events.lock().unwrap().len(), 1);
        assert!(matches!(
            events.lock().unwrap()[0],
            WindowSystemEvent::FocusIn
        ));

        // 2. Asynchronous delivery: queued in WINDOW_SYSTEM_EVENT_QUEUE until flush
        dispatch_window_system_event(Delivery::Asynchronous, hwnd, WindowSystemEvent::FocusOut);
        // Not delivered immediately
        assert_eq!(events.lock().unwrap().len(), 1);
        // Flush queue
        let flushed = flush_window_system_events();
        assert!(flushed);
        assert_eq!(events.lock().unwrap().len(), 2);
        assert!(matches!(
            events.lock().unwrap()[1],
            WindowSystemEvent::FocusOut
        ));

        // 3. handle_geometry_change with Delivery::Default on window thread: delivers immediately
        handle_geometry_change(Delivery::Default, hwnd, Rect::new(60, 70, 400, 300));
        let current_events = events.lock().unwrap().clone();
        assert!(current_events.iter().any(|e| matches!(e, WindowSystemEvent::GeometryChange { geometry } if geometry.width == 400 && geometry.height == 300)));
        assert!(current_events.iter().any(|e| matches!(e, WindowSystemEvent::Resize { size } if size.width == 400 && size.height == 300)));

        // 4. Background thread with Delivery::Default: queues asynchronously
        let events_len_before = events.lock().unwrap().len();
        let hwnd_isize = hwnd as isize;
        let t = std::thread::spawn(move || {
            let bg_hwnd = hwnd_isize as windows_sys::Win32::Foundation::HWND;
            dispatch_window_system_event(
                Delivery::Default,
                bg_hwnd,
                WindowSystemEvent::CloseRequest,
            );
        });
        t.join().unwrap();
        // Background thread queued the event
        assert_eq!(events.lock().unwrap().len(), events_len_before);
        // Flush delivers it
        let flushed_bg = flush_window_system_events();
        assert!(flushed_bg);
        assert_eq!(events.lock().unwrap().len(), events_len_before + 1);
        assert!(matches!(
            events.lock().unwrap().last().unwrap(),
            WindowSystemEvent::CloseRequest
        ));
    }
}

#[test]
fn test_surface_presenter_dc_and_layered_alignment() {
    #[cfg(windows)]
    {
        use ferrokit_gui::geometry::primitives::Rect;
        use ferrokit_gui::geometry::Region;
        use ferrokit_gui::paint::Pixmap;
        use ferrokit_gui::tiny_skia::Color;
        use ferrokit_platform::presenter::{
            SurfacePresenter, Win32DcPresenter, Win32LayeredPresenter,
        };
        use ferrokit_platform::window::{NativeWindow, WindowFlags};
        use ferrokit_platform::PlatformWindow;
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetWindowLongPtrW, GWL_EXSTYLE, WS_EX_LAYERED,
        };

        // 1. Test Standard/DC Presenter:
        // Window MUST NOT have WS_EX_LAYERED, and presents via GDI BitBlt
        let mut normal_win = NativeWindow::new(
            "DC Presenter Test",
            Rect::new(50, 50, 200, 150),
            WindowFlags::NORMAL,
        )
        .expect("create normal native window");

        let hwnd_normal = normal_win.hwnd();
        // SAFETY: `hwnd_normal` is the live window of `normal_win`; `GetWindowLongPtrW` only reads.
        let ex_style_normal = unsafe { GetWindowLongPtrW(hwnd_normal, GWL_EXSTYLE) };
        assert_eq!(
            (ex_style_normal as u32 & WS_EX_LAYERED),
            0,
            "Standard window must not have WS_EX_LAYERED before present"
        );

        let mut pixmap = Pixmap::new(200, 150).expect("allocate pixmap");
        pixmap.fill(Color::from_rgba8(255, 0, 0, 255));

        // Present to normal window via PlatformWindow::present_region
        let dirty_region = Region::from_coords(10, 10, 80, 60);
        let res = normal_win.present_region(&pixmap, &dirty_region);
        assert!(res.is_ok(), "normal_win.present_region failed: {:?}", res);

        // SAFETY: `hwnd_normal` is the live window of `normal_win` (still alive); read only.
        let ex_style_after = unsafe { GetWindowLongPtrW(hwnd_normal, GWL_EXSTYLE) };
        assert_eq!(
            (ex_style_after as u32 & WS_EX_LAYERED),
            0,
            "Standard window must NOT be forced into WS_EX_LAYERED by DC presentation!"
        );

        // 2. Direct Win32DcPresenter test
        let mut dc_presenter =
            Win32DcPresenter::new(hwnd_normal, 200, 150).expect("create Win32DcPresenter");
        let dc_res = dc_presenter.present(&pixmap, &dirty_region);
        assert!(
            dc_res.is_ok(),
            "Win32DcPresenter.present failed: {:?}",
            dc_res
        );

        // 3. Test Layered Presenter:
        // Window HAS WS_EX_LAYERED and uses UpdateLayeredWindowIndirect
        let mut layered_win = NativeWindow::new(
            "Layered Presenter Test",
            Rect::new(100, 100, 200, 150),
            WindowFlags::LAYERED | WindowFlags::FRAMELESS,
        )
        .expect("create layered native window");

        let hwnd_layered = layered_win.hwnd();
        // SAFETY: `hwnd_layered` is the live window of `layered_win`; `GetWindowLongPtrW` only reads.
        let ex_style_layered = unsafe { GetWindowLongPtrW(hwnd_layered, GWL_EXSTYLE) };
        assert_ne!(
            (ex_style_layered as u32 & WS_EX_LAYERED),
            0,
            "Layered window must have WS_EX_LAYERED"
        );

        let res_layered = layered_win.present_region(&pixmap, &dirty_region);
        assert!(
            res_layered.is_ok(),
            "layered_win.present_region failed: {:?}",
            res_layered
        );

        // 4. Direct Win32LayeredPresenter test with dirty region (dirty-rect update)
        let mut layered_presenter = Win32LayeredPresenter::new(hwnd_layered, 200, 150, 0.85)
            .expect("create Win32LayeredPresenter");
        layered_presenter.set_opacity(0.9);
        let lay_res = layered_presenter.present(&pixmap, &dirty_region);
        assert!(
            lay_res.is_ok(),
            "Win32LayeredPresenter.present with dirty region failed: {:?}",
            lay_res
        );

        // 5. Test resize transition (reallocates/re-bounds)
        let mut resized_pixmap = Pixmap::new(240, 180).expect("create resized pixmap");
        resized_pixmap.fill(Color::from_rgba8(50, 120, 220, 200));
        let full_resized_region = Region::from_coords(0, 0, 240, 180);

        let dc_resize_res = dc_presenter.present(&resized_pixmap, &full_resized_region);
        assert!(
            dc_resize_res.is_ok(),
            "Win32DcPresenter resize presentation failed: {:?}",
            dc_resize_res
        );

        let lay_resize_res = layered_presenter.present(&resized_pixmap, &full_resized_region);
        assert!(
            lay_resize_res.is_ok(),
            "Win32LayeredPresenter resize presentation failed: {:?}",
            lay_resize_res
        );
    }
}

#[test]
fn test_platform_tray_message_with_custom_icon() {
    ferrokit_core::object::ThreadContext::init_current(true, None);
    use ferrokit_gui::image::Icon;
    use ferrokit_gui::paint::Pixmap;
    use ferrokit_platform::{
        DbusStatusNotifierItem, GenericTrayIcon, PlatformTrayIcon, TrayMessageIcon,
    };

    let pm = Pixmap::new(16, 16).unwrap();
    let icon = Icon::from_pixmap(&pm);

    // The generic back end keeps the custom icon and shows no standard icon.
    let mut generic = GenericTrayIcon::new("Generic", &icon);
    assert_eq!(
        generic.icon().available_sizes(
            ferrokit_gui::image::IconMode::Normal,
            ferrokit_gui::image::IconState::Off
        ),
        icon.available_sizes(
            ferrokit_gui::image::IconMode::Normal,
            ferrokit_gui::image::IconState::Off
        )
    );
    generic.set_icon(&Icon::new()).unwrap();
    assert!(generic.icon().is_null());
    generic.set_icon(&icon).unwrap();
    assert!(generic.last_message_icon().is_none());
    generic.show_message_with_icon("T", "B", &icon, 0).unwrap();
    assert_eq!(
        generic.last_message(),
        Some(("T", "B", TrayMessageIcon::NoIcon, 10000))
    );
    assert!(generic.last_message_icon().is_some());
    // A later standard-icon message drops it again, and a null icon never counts as custom.
    generic
        .show_message("T2", "B2", TrayMessageIcon::Warning, 1000)
        .unwrap();
    assert!(generic.last_message_icon().is_none());
    generic
        .show_message_with_icon("T3", "B3", &Icon::new(), 1000)
        .unwrap();
    assert!(generic.last_message_icon().is_none());

    // Back ends that cannot show an arbitrary image fall back to NoIcon.
    let mut dbus = DbusStatusNotifierItem::new("claude-hud", "ClaudeHUD");
    dbus.show_message_with_icon("Alert", "Body", &icon, 2000)
        .unwrap();
    assert_eq!(
        dbus.last_message(),
        Some(("Alert", "Body", TrayMessageIcon::NoIcon, 2000))
    );
}

#[cfg(not(windows))]
#[test]
fn a_hidden_tray_icon_forwards_no_message() {
    // `QSystemTrayIcon::showMessage` (both overloads) does nothing while the icon is hidden.
    use ferrokit_gui::image::Icon;
    use ferrokit_platform::menu::PlatformMenu;
    use ferrokit_platform::tray_icon::TrayIcon;
    use ferrokit_platform::{PlatformTrayIcon, TrayMessageIcon};
    use std::cell::Cell;
    use std::rc::Rc;

    struct Spy(Rc<Cell<u32>>);
    impl PlatformTrayIcon for Spy {
        fn set_icon(&mut self, _: &Icon) -> Result<(), &'static str> {
            Ok(())
        }
        fn set_tooltip(&mut self, _: &str) -> Result<(), &'static str> {
            Ok(())
        }
        fn set_menu(&mut self, _: Box<dyn PlatformMenu>) {}
        fn show(&mut self) -> Result<(), &'static str> {
            Ok(())
        }
        fn hide(&mut self) -> Result<(), &'static str> {
            Ok(())
        }
        fn show_message(
            &mut self,
            _: &str,
            _: &str,
            _: TrayMessageIcon,
            _: u32,
        ) -> Result<(), &'static str> {
            self.0.set(self.0.get() + 1);
            Ok(())
        }
        fn show_message_with_icon(
            &mut self,
            _: &str,
            _: &str,
            _: &Icon,
            _: u32,
        ) -> Result<(), &'static str> {
            self.0.set(self.0.get() + 10);
            Ok(())
        }
    }

    let seen = Rc::new(Cell::new(0));
    let mut tray = TrayIcon::with_backend(Box::new(Spy(seen.clone())));
    let send = |tray: &mut TrayIcon| {
        tray.show_message("T", "B", TrayMessageIcon::Information, 0)
            .unwrap();
        tray.show_message_with_icon("T", "B", &Icon::new(), 0)
            .unwrap();
    };
    send(&mut tray);
    assert_eq!(seen.get(), 0, "hidden: nothing is forwarded");
    tray.show().unwrap();
    send(&mut tray);
    assert_eq!(seen.get(), 11, "shown: both are forwarded");
    tray.hide().unwrap();
    send(&mut tray);
    assert_eq!(seen.get(), 11, "hidden again: nothing more");
}
