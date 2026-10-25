// SPDX-License-Identifier: LGPL-3.0-only
use ferrokit_core::signal::Signal;
use ferrokit_gui::geometry::primitives::{Point, Rect};
use ferrokit_gui::paint::Pixmap;
use std::sync::Arc;

use crate::clipboard::PlatformClipboard;
use crate::cursor::PlatformCursor;
use crate::hotkey::PlatformHotkeyManager;
use crate::platform_tray::PlatformTrayIcon;
use crate::platform_window::PlatformWindow;
use crate::screen::PlatformScreen;
use crate::theme::PlatformTheme;
use crate::window::WindowFlags;

pub trait PlatformIntegration: Send + Sync {
    fn create_window(
        &self,
        title: &str,
        rect: Rect,
        flags: WindowFlags,
    ) -> Result<Box<dyn PlatformWindow>, &'static str>;

    fn create_tray_icon(
        &self,
        tooltip: &str,
        pixmap: &Pixmap,
    ) -> Result<Box<dyn PlatformTrayIcon>, &'static str>;

    fn primary_screen(&self) -> Box<dyn PlatformScreen>;
    fn screens(&self) -> Vec<Box<dyn PlatformScreen>>;
    fn screen_at(&self, pos: Point) -> Option<Box<dyn PlatformScreen>>;
    fn screen_changed(&self) -> &Signal<()>;
    fn theme(&self) -> Arc<dyn PlatformTheme>;
    fn clipboard(&self) -> Box<dyn PlatformClipboard>;
    fn create_hotkey_manager(&self) -> Result<Box<dyn PlatformHotkeyManager>, &'static str>;
    fn cursor(&self) -> Box<dyn PlatformCursor>;
}

#[cfg(windows)]
pub mod win32 {
    use super::*;
    use crate::clipboard::Win32Clipboard;
    use crate::cursor::Win32Cursor;
    use crate::hotkey::Win32HotkeyManager;
    use crate::screen::Win32Screen;
    use crate::theme::Win32Theme;
    use crate::tray_icon::TrayIcon;
    use crate::window::NativeWindow;

    pub struct Win32PlatformIntegration {
        screen_changed_signal: Signal<()>,
        theme: Arc<Win32Theme>,
    }

    impl Default for Win32PlatformIntegration {
        fn default() -> Self {
            Self {
                screen_changed_signal: Signal::new(),
                theme: Arc::new(Win32Theme::new()),
            }
        }
    }

    impl PlatformIntegration for Win32PlatformIntegration {
        fn create_window(
            &self,
            title: &str,
            rect: Rect,
            flags: WindowFlags,
        ) -> Result<Box<dyn PlatformWindow>, &'static str> {
            let win = NativeWindow::new(title, rect, flags)?;
            Ok(Box::new(win))
        }

        fn create_tray_icon(
            &self,
            tooltip: &str,
            pixmap: &Pixmap,
        ) -> Result<Box<dyn PlatformTrayIcon>, &'static str> {
            // Keep the original allocation: the window-procedure router holds its address.
            // `from_pixmap` also makes the tray own (and later destroy) the icon.
            let tray = TrayIcon::from_pixmap(tooltip, pixmap)?;
            Ok(tray)
        }

        fn primary_screen(&self) -> Box<dyn PlatformScreen> {
            Box::new(Win32Screen::primary())
        }

        fn screens(&self) -> Vec<Box<dyn PlatformScreen>> {
            Win32Screen::all_screens()
                .into_iter()
                .map(|s| Box::new(s) as Box<dyn PlatformScreen>)
                .collect()
        }

        fn screen_at(&self, pos: Point) -> Option<Box<dyn PlatformScreen>> {
            Win32Screen::screen_at(pos).map(|s| Box::new(s) as Box<dyn PlatformScreen>)
        }

        fn screen_changed(&self) -> &Signal<()> {
            &self.screen_changed_signal
        }

        fn theme(&self) -> Arc<dyn PlatformTheme> {
            Arc::clone(&self.theme) as Arc<dyn PlatformTheme>
        }

        fn clipboard(&self) -> Box<dyn PlatformClipboard> {
            Box::new(Win32Clipboard)
        }

        fn create_hotkey_manager(&self) -> Result<Box<dyn PlatformHotkeyManager>, &'static str> {
            Ok(Box::new(Win32HotkeyManager::new(std::ptr::null_mut())))
        }

        fn cursor(&self) -> Box<dyn PlatformCursor> {
            Box::new(Win32Cursor::new())
        }
    }
}

pub mod generic {
    use super::*;
    use crate::clipboard::GenericClipboard;
    use crate::cursor::GenericCursor;
    use crate::hotkey::GenericHotkeyManager;
    use crate::platform_tray::GenericTrayIcon;
    use crate::platform_window::GenericWindow;
    use crate::screen::GenericScreen;
    use crate::theme::GenericTheme;

    pub struct GenericPlatformIntegration {
        screen_changed_signal: Signal<()>,
        theme: Arc<GenericTheme>,
    }

    impl Default for GenericPlatformIntegration {
        fn default() -> Self {
            Self {
                screen_changed_signal: Signal::new(),
                theme: Arc::new(GenericTheme::default()),
            }
        }
    }

    impl PlatformIntegration for GenericPlatformIntegration {
        fn create_window(
            &self,
            title: &str,
            rect: Rect,
            flags: WindowFlags,
        ) -> Result<Box<dyn PlatformWindow>, &'static str> {
            Ok(Box::new(GenericWindow::new(title, rect, flags)))
        }

        fn create_tray_icon(
            &self,
            tooltip: &str,
            pixmap: &Pixmap,
        ) -> Result<Box<dyn PlatformTrayIcon>, &'static str> {
            Ok(Box::new(GenericTrayIcon::new(tooltip, pixmap)))
        }

        fn primary_screen(&self) -> Box<dyn PlatformScreen> {
            Box::new(GenericScreen::default_primary())
        }

        fn screens(&self) -> Vec<Box<dyn PlatformScreen>> {
            vec![Box::new(GenericScreen::default_primary())]
        }

        fn screen_at(&self, pos: Point) -> Option<Box<dyn PlatformScreen>> {
            let primary = GenericScreen::default_primary();
            if primary.geometry().contains(pos) {
                Some(Box::new(primary))
            } else {
                None
            }
        }

        fn screen_changed(&self) -> &Signal<()> {
            &self.screen_changed_signal
        }

        fn theme(&self) -> Arc<dyn PlatformTheme> {
            Arc::clone(&self.theme) as Arc<dyn PlatformTheme>
        }

        fn clipboard(&self) -> Box<dyn PlatformClipboard> {
            Box::new(GenericClipboard::new())
        }

        fn create_hotkey_manager(&self) -> Result<Box<dyn PlatformHotkeyManager>, &'static str> {
            Ok(Box::new(GenericHotkeyManager::new()))
        }

        fn cursor(&self) -> Box<dyn PlatformCursor> {
            Box::new(GenericCursor::new())
        }
    }
}

pub mod unix {
    use super::*;
    use crate::clipboard::GenericClipboard;
    use crate::cursor::UnixCursor;
    use crate::hotkey::UnixHotkeyManager;
    use crate::platform_window::GenericWindow;
    use crate::theme::UnixTheme;
    use crate::tray::DbusStatusNotifierItem;
    use crate::window_wayland::{WaylandNativeWindow, WAYLAND_CONNECT_FAILED};
    use crate::window_x11::{X11NativeWindow, X11_CONNECT_FAILED};

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum DisplayServerKind {
        Wayland,
        X11,
        Generic,
    }

    pub struct UnixPlatformIntegration {
        screen_changed_signal: Signal<()>,
        theme: Arc<UnixTheme>,
    }

    impl UnixPlatformIntegration {
        /// The display server selected by the environment, or `Generic` when its socket is not
        /// reachable (stale `DISPLAY`, SSH forwarding, `WAYLAND_SOCKET` alone), so windows fall
        /// back to the offscreen window instead of failing.
        pub fn detect_display_server() -> DisplayServerKind {
            if (std::env::var("WAYLAND_DISPLAY").is_ok() || std::env::var("WAYLAND_SOCKET").is_ok())
                && WaylandNativeWindow::display_reachable()
            {
                return DisplayServerKind::Wayland;
            }
            if std::env::var("DISPLAY").is_ok() && X11NativeWindow::display_reachable() {
                return DisplayServerKind::X11;
            }
            DisplayServerKind::Generic
        }
    }

    impl Default for UnixPlatformIntegration {
        fn default() -> Self {
            Self {
                screen_changed_signal: Signal::new(),
                theme: Arc::new(UnixTheme::default()),
            }
        }
    }

    /// Opens a window for the detected `kind`. Detection and the native connect are separate
    /// steps, so the socket can vanish in between; a failed native connect then falls back to the
    /// offscreen window like an unreachable display does.
    pub(super) fn open_window(
        kind: DisplayServerKind,
        title: &str,
        rect: Rect,
        flags: WindowFlags,
        open_native: impl FnOnce(
            DisplayServerKind,
            &str,
            Rect,
            WindowFlags,
        ) -> Result<Box<dyn PlatformWindow>, &'static str>,
    ) -> Result<Box<dyn PlatformWindow>, &'static str> {
        if kind == DisplayServerKind::Generic {
            return Ok(Box::new(GenericWindow::new(title, rect, flags)));
        }
        match open_native(kind, title, rect, flags) {
            Err(e) if e == X11_CONNECT_FAILED || e == WAYLAND_CONNECT_FAILED => {
                Ok(Box::new(GenericWindow::new(title, rect, flags)))
            }
            other => other,
        }
    }

    impl PlatformIntegration for UnixPlatformIntegration {
        fn create_window(
            &self,
            title: &str,
            rect: Rect,
            flags: WindowFlags,
        ) -> Result<Box<dyn PlatformWindow>, &'static str> {
            open_window(
                Self::detect_display_server(),
                title,
                rect,
                flags,
                |kind, title, rect, flags| -> Result<Box<dyn PlatformWindow>, &'static str> {
                    match kind {
                        DisplayServerKind::Wayland => {
                            Ok(Box::new(WaylandNativeWindow::new(title, rect, flags)?))
                        }
                        DisplayServerKind::X11 => {
                            Ok(Box::new(X11NativeWindow::new(title, rect, flags)?))
                        }
                        DisplayServerKind::Generic => {
                            Ok(Box::new(GenericWindow::new(title, rect, flags)))
                        }
                    }
                },
            )
        }

        fn create_tray_icon(
            &self,
            tooltip: &str,
            pixmap: &Pixmap,
        ) -> Result<Box<dyn PlatformTrayIcon>, &'static str> {
            let mut item = DbusStatusNotifierItem::new("ferrokit.app", tooltip);
            let _ = item.set_icon(pixmap);
            let _ = item.set_tooltip(tooltip);
            Ok(Box::new(item))
        }

        fn primary_screen(&self) -> Box<dyn PlatformScreen> {
            Box::new(crate::screen::X11Screen::primary())
        }

        fn screens(&self) -> Vec<Box<dyn PlatformScreen>> {
            crate::screen::X11Screen::screens()
                .into_iter()
                .map(|s| Box::new(s) as Box<dyn PlatformScreen>)
                .collect()
        }

        fn screen_at(&self, pos: Point) -> Option<Box<dyn PlatformScreen>> {
            crate::screen::X11Screen::screen_at(pos).map(|s| Box::new(s) as Box<dyn PlatformScreen>)
        }

        fn screen_changed(&self) -> &Signal<()> {
            &self.screen_changed_signal
        }

        fn theme(&self) -> Arc<dyn PlatformTheme> {
            Arc::clone(&self.theme) as Arc<dyn PlatformTheme>
        }

        fn clipboard(&self) -> Box<dyn PlatformClipboard> {
            Box::new(GenericClipboard::new())
        }

        fn create_hotkey_manager(&self) -> Result<Box<dyn PlatformHotkeyManager>, &'static str> {
            Ok(Box::new(UnixHotkeyManager::new()))
        }

        fn cursor(&self) -> Box<dyn PlatformCursor> {
            Box::new(UnixCursor::new())
        }
    }
}

pub mod cocoa {
    use super::*;
    use crate::clipboard::GenericClipboard;
    use crate::cursor::CocoaCursor;
    use crate::hotkey::CocoaHotkeyManager;
    use crate::platform_tray::GenericTrayIcon;
    use crate::platform_window::GenericWindow;
    use crate::theme::CocoaTheme;
    use crate::tray::CocoaStatusItem;
    use crate::window_cocoa::CocoaNativeWindow;

    impl CocoaPlatformIntegration {
        pub fn is_headless() -> bool {
            std::env::var("CI").is_ok()
                || std::env::var("GITHUB_ACTIONS").is_ok()
                || std::env::var("QT_QPA_PLATFORM").as_deref() == Ok("offscreen")
                || std::env::var("FERROKIT_HEADLESS").is_ok()
        }

        /// Runs the `NSApplication` setup once. AppKit allows it only on the main thread, so a
        /// call from any other thread (the lazy `platform()` singleton built by a worker) does
        /// nothing and the setup is done by the first call made on the main thread, which window
        /// creation makes.
        #[cfg(target_os = "macos")]
        fn ensure_ns_application() {
            use std::sync::atomic::{AtomicBool, Ordering};
            static DONE: AtomicBool = AtomicBool::new(false);
            if Self::is_headless() || !ferrokit_core::object::ThreadContext::is_main_thread() {
                return;
            }
            if !DONE.swap(true, Ordering::AcqRel) {
                init_ns_application();
            }
        }
    }
    pub struct CocoaPlatformIntegration {
        screen_changed_signal: Signal<()>,
        theme: Arc<CocoaTheme>,
    }

    impl Default for CocoaPlatformIntegration {
        fn default() -> Self {
            #[cfg(target_os = "macos")]
            Self::ensure_ns_application();
            Self {
                screen_changed_signal: Signal::new(),
                theme: Arc::new(CocoaTheme::default()),
            }
        }
    }

    /// The `NSApplication` setup of Qt's `QCocoaIntegration` constructor
    /// (qcocoaintegration.mm:137-161): make the process a foreground application unless
    /// `QT_MAC_DISABLE_FOREGROUND_APPLICATION_TRANSFORM` is set, and install the application
    /// delegate, which activates the application once it has launched.
    #[cfg(target_os = "macos")]
    fn init_ns_application() {
        use crate::objc_runtime::{objc_get_class, qcocoa_application_delegate, ObjcMsg, Sel};
        let app = unsafe {
            ObjcMsg::send_class_0(
                objc_get_class("NSApplication"),
                Sel::register("sharedApplication"),
            )
        };
        if std::env::var_os("QT_MAC_DISABLE_FOREGROUND_APPLICATION_TRANSFORM")
            .is_none_or(|value| value.is_empty())
        {
            transform_process_to_foreground_application(app);
        }
        unsafe {
            ObjcMsg::send_id(
                app,
                Sel::register("setDelegate:"),
                qcocoa_application_delegate(),
            )
        };
    }

    /// `qt_mac_transformProccessToForegroundApplication` (qcocoahelpers.mm:138-180): the
    /// activation policy becomes `NSApplicationActivationPolicyRegular` unless the Info.plist sets
    /// `LSUIElement` or `LSBackgroundOnly` to a true value.
    #[cfg(target_os = "macos")]
    fn transform_process_to_foreground_application(app: crate::objc_runtime::Id) {
        use crate::objc_runtime::{ObjcMsg, Sel};
        let set = |key| info_plist_int(key).is_some_and(|value| value != 0);
        if !set("LSUIElement") && !set("LSBackgroundOnly") {
            // NSApplicationActivationPolicyRegular
            unsafe { ObjcMsg::send_int(app, Sel::register("setActivationPolicy:"), 0) };
        }
    }

    /// The main bundle's Info.plist value for `key` as an integer, read the way Qt does: a string
    /// is parsed as a number (0 if it is not one), a boolean is 0 or 1, a number is truncated to
    /// `int`. `None` if the key is absent or has another type.
    #[cfg(target_os = "macos")]
    fn info_plist_int(key: &str) -> Option<i64> {
        use crate::objc_runtime::{nsstring_from_str, ObjcMsg, Sel};
        use std::ffi::{c_char, c_void, CStr};

        #[link(name = "CoreFoundation", kind = "framework")]
        extern "C" {
            fn CFBundleGetMainBundle() -> *mut c_void;
            fn CFBundleGetValueForInfoDictionaryKey(
                bundle: *mut c_void,
                key: *const c_void,
            ) -> *const c_void;
            fn CFGetTypeID(object: *const c_void) -> usize;
            fn CFStringGetTypeID() -> usize;
            fn CFBooleanGetTypeID() -> usize;
            fn CFNumberGetTypeID() -> usize;
            fn CFBooleanGetValue(boolean: *const c_void) -> u8;
            fn CFNumberGetValue(number: *const c_void, kind: isize, value: *mut c_void) -> u8;
        }
        const K_CF_NUMBER_INT_TYPE: isize = 9;

        let ns_key = nsstring_from_str(key);
        let value =
            unsafe { CFBundleGetValueForInfoDictionaryKey(CFBundleGetMainBundle(), ns_key.0) };
        unsafe { ObjcMsg::send_0(ns_key, Sel::register("release")) };
        if value.is_null() {
            return None;
        }
        unsafe {
            let kind = CFGetTypeID(value);
            if kind == CFStringGetTypeID() {
                let utf8 = ObjcMsg::send_0(
                    crate::objc_runtime::Id(value as *mut c_void),
                    Sel::register("UTF8String"),
                )
                .0 as *const c_char;
                let text = if utf8.is_null() {
                    ""
                } else {
                    CStr::from_ptr(utf8).to_str().unwrap_or("")
                };
                Some(text.trim().parse::<i32>().map_or(0, i64::from))
            } else if kind == CFBooleanGetTypeID() {
                Some(i64::from(CFBooleanGetValue(value)))
            } else if kind == CFNumberGetTypeID() {
                let mut number: i32 = 0;
                CFNumberGetValue(
                    value,
                    K_CF_NUMBER_INT_TYPE,
                    (&mut number as *mut i32).cast(),
                );
                Some(i64::from(number))
            } else {
                None
            }
        }
    }

    impl PlatformIntegration for CocoaPlatformIntegration {
        fn create_window(
            &self,
            title: &str,
            rect: Rect,
            flags: WindowFlags,
        ) -> Result<Box<dyn PlatformWindow>, &'static str> {
            if Self::is_headless() {
                return Ok(Box::new(GenericWindow::new(title, rect, flags)));
            }
            #[cfg(target_os = "macos")]
            Self::ensure_ns_application();
            let win = CocoaNativeWindow::new(title, rect, flags)?;
            Ok(Box::new(win))
        }

        fn create_tray_icon(
            &self,
            tooltip: &str,
            pixmap: &Pixmap,
        ) -> Result<Box<dyn PlatformTrayIcon>, &'static str> {
            if Self::is_headless() {
                return Ok(Box::new(GenericTrayIcon::new(tooltip, pixmap)));
            }
            let mut item = CocoaStatusItem::new(1);
            let _ = item.set_icon(pixmap);
            let _ = item.set_tooltip(tooltip);
            Ok(Box::new(item))
        }

        fn primary_screen(&self) -> Box<dyn PlatformScreen> {
            Box::new(crate::screen::CocoaScreen::primary())
        }

        fn screens(&self) -> Vec<Box<dyn PlatformScreen>> {
            crate::screen::CocoaScreen::screens()
                .into_iter()
                .map(|s| Box::new(s) as Box<dyn PlatformScreen>)
                .collect()
        }

        fn screen_at(&self, pos: Point) -> Option<Box<dyn PlatformScreen>> {
            crate::screen::CocoaScreen::screen_at(pos)
                .map(|s| Box::new(s) as Box<dyn PlatformScreen>)
        }

        fn screen_changed(&self) -> &Signal<()> {
            &self.screen_changed_signal
        }

        fn theme(&self) -> Arc<dyn PlatformTheme> {
            Arc::clone(&self.theme) as Arc<dyn PlatformTheme>
        }

        fn clipboard(&self) -> Box<dyn PlatformClipboard> {
            Box::new(GenericClipboard::new())
        }

        fn create_hotkey_manager(&self) -> Result<Box<dyn PlatformHotkeyManager>, &'static str> {
            CocoaHotkeyManager::try_new().map(|m| Box::new(m) as Box<dyn PlatformHotkeyManager>)
        }

        fn cursor(&self) -> Box<dyn PlatformCursor> {
            Box::new(CocoaCursor::new())
        }
    }
}

pub use cocoa::CocoaPlatformIntegration;
pub use generic::GenericPlatformIntegration;
pub use unix::DisplayServerKind;
pub use unix::UnixPlatformIntegration;

#[cfg(windows)]
pub use win32::Win32PlatformIntegration;

static PLATFORM_INTEGRATION: std::sync::RwLock<Option<Arc<dyn PlatformIntegration>>> =
    std::sync::RwLock::new(None);

pub fn platform() -> Arc<dyn PlatformIntegration> {
    if let Some(integration) = &*PLATFORM_INTEGRATION.read().unwrap() {
        return Arc::clone(integration);
    }

    // Built without holding the lock: constructing an integration may call into the window
    // system, and must not block (or deadlock with) other users of `platform()`.
    #[cfg(windows)]
    let default_integration: Arc<dyn PlatformIntegration> =
        Arc::new(win32::Win32PlatformIntegration::default());

    #[cfg(target_os = "linux")]
    let default_integration: Arc<dyn PlatformIntegration> =
        Arc::new(unix::UnixPlatformIntegration::default());

    #[cfg(target_os = "macos")]
    let default_integration: Arc<dyn PlatformIntegration> =
        Arc::new(cocoa::CocoaPlatformIntegration::default());

    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    let default_integration: Arc<dyn PlatformIntegration> =
        Arc::new(generic::GenericPlatformIntegration::default());

    let mut writer = PLATFORM_INTEGRATION.write().unwrap();
    // A racing caller (or `set_platform_integration`) may have installed one meanwhile.
    Arc::clone(writer.get_or_insert(default_integration))
}

pub fn set_platform_integration(integration: Arc<dyn PlatformIntegration>) {
    let mut writer = PLATFORM_INTEGRATION.write().unwrap();
    *writer = Some(integration);
}

#[cfg(all(test, target_os = "linux"))]
mod unreachable_display_tests {
    use super::unix::{DisplayServerKind, UnixPlatformIntegration};
    use super::PlatformIntegration;
    use ferrokit_gui::geometry::primitives::Rect;
    use crate::window::WindowFlags;

    #[test]
    fn bogus_display_falls_back_to_a_working_window() {
        let saved = (
            std::env::var_os("DISPLAY"),
            std::env::var_os("WAYLAND_DISPLAY"),
            std::env::var_os("WAYLAND_SOCKET"),
        );
        std::env::set_var("DISPLAY", "localhost:7777.0");
        std::env::set_var("WAYLAND_DISPLAY", "ferrokit-bogus-wayland");
        std::env::set_var("WAYLAND_SOCKET", "99");
        let kind = UnixPlatformIntegration::detect_display_server();
        let win = UnixPlatformIntegration::default().create_window(
            "t",
            Rect::new(0, 0, 100, 100),
            WindowFlags::default(),
        );
        for (k, v) in [
            ("DISPLAY", saved.0),
            ("WAYLAND_DISPLAY", saved.1),
            ("WAYLAND_SOCKET", saved.2),
        ] {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
        assert_eq!(kind, DisplayServerKind::Generic);
        assert!(win.is_ok());
    }

    #[test]
    fn failed_native_connect_after_detection_falls_back_to_generic() {
        let win = super::unix::open_window(
            DisplayServerKind::X11,
            "t",
            Rect::new(0, 0, 100, 100),
            WindowFlags::default(),
            |_, _, _, _| Err(crate::window_x11::X11_CONNECT_FAILED),
        );
        assert!(win.is_ok());
    }

    #[test]
    fn other_native_errors_propagate() {
        let win = super::unix::open_window(
            DisplayServerKind::X11,
            "t",
            Rect::new(0, 0, 100, 100),
            WindowFlags::default(),
            |_, _, _, _| Err("shm setup failed"),
        );
        assert_eq!(win.err(), Some("shm setup failed"));
    }
}
