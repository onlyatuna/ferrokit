// SPDX-License-Identifier: LGPL-3.0-only
use ferrokit_core::signal::{ConnectionId, Signal};
use ferrokit_gui::application::ColorSchemeSource;
use std::sync::{Arc, Mutex};

pub use ferrokit_gui::application::ColorScheme;

/// The colour scheme a platform theme reports: the system's scheme and, next to it, the one the
/// application asked for (`QPlatformTheme::requestColorScheme`). The effective scheme is the
/// request unless that is [`ColorScheme::Unknown`], as in `QWindowsTheme::effectiveColorScheme`
/// (qwindowstheme.cpp:655-667) and `QGnomeThemePrivate::colorScheme` (qgnometheme.cpp:56-78).
///
/// `changed` fires when the *effective* scheme changes, after the lock is released, so a slot can
/// read the theme back.
pub(crate) struct SchemeCore {
    /// `(system, requested)`.
    schemes: Mutex<(ColorScheme, ColorScheme)>,
    changed: Signal<ColorScheme>,
}

impl SchemeCore {
    pub(crate) fn new(system: ColorScheme) -> Self {
        Self {
            schemes: Mutex::new((system, ColorScheme::Unknown)),
            changed: Signal::new(),
        }
    }

    pub(crate) fn effective(&self) -> ColorScheme {
        let (system, requested) = *self.schemes.lock().unwrap_or_else(|e| e.into_inner());
        Self::resolve(system, requested)
    }

    fn resolve(system: ColorScheme, requested: ColorScheme) -> ColorScheme {
        if requested != ColorScheme::Unknown {
            requested
        } else {
            system
        }
    }

    pub(crate) fn signal(&self) -> &Signal<ColorScheme> {
        &self.changed
    }

    fn update(&self, f: impl FnOnce(&mut (ColorScheme, ColorScheme))) {
        let after = {
            let mut guard = self.schemes.lock().unwrap_or_else(|e| e.into_inner());
            let before = Self::resolve(guard.0, guard.1);
            f(&mut guard);
            let after = Self::resolve(guard.0, guard.1);
            (before != after).then_some(after)
        };
        if let Some(scheme) = after {
            self.changed.emit(&scheme);
        }
    }

    /// The operating system's scheme moved.
    pub(crate) fn set_system(&self, scheme: ColorScheme) {
        self.update(|s| s.0 = scheme);
    }

    /// `requestColorScheme`: override the system scheme, or follow it again with `Unknown`.
    pub(crate) fn set_requested(&self, scheme: ColorScheme) {
        self.update(|s| s.1 = scheme);
    }
}

/// The widget style Qt would pick on this platform. Style-sheet-driven widgets still draw
/// some parts (menu arrows, check marks) through it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeStyle {
    /// `windows11` (Windows 11): Fluent icon font glyphs.
    Windows11,
    /// `windowsvista` (earlier Windows).
    WindowsVista,
    /// `macos`.
    Macintosh,
    /// `Fusion`, the style on Linux and elsewhere.
    Fusion,
}

pub trait PlatformTheme: Send + Sync {
    /// Which style Qt would use on this platform.
    fn native_style(&self) -> NativeStyle {
        NativeStyle::Fusion
    }
    /// `QPlatformTheme::colorScheme`: the effective scheme, an application override included.
    fn color_scheme(&self) -> ColorScheme;
    /// Emitted with the new effective scheme whenever it changes, on the thread that noticed the
    /// change (a window procedure, a D-Bus listener thread, the main thread for AppKit). It is
    /// the platform half of `QStyleHints::colorSchemeChanged`; applications connect to
    /// `GuiApplication::style_hints().color_scheme_changed()` instead, which is emitted on the
    /// main thread.
    fn theme_changed(&self) -> &Signal<ColorScheme>;
    /// Re-reads what the system exposes (called when the system says its settings changed).
    fn refresh(&self);
    /// `QPlatformTheme::requestColorScheme`: overrides the system scheme with `scheme`, or follows
    /// the system again with [`ColorScheme::Unknown`]. Like the base class in Qt
    /// (qplatformtheme.cpp:480-487) the default ignores the request; every theme of this crate
    /// honours it.
    fn request_color_scheme(&self, scheme: ColorScheme) {
        let _ = scheme;
    }
    /// Family of the system menu font (`QPlatformTheme::MenuFont`), which Qt uses as the base
    /// font of every `QMenu`. `None` when the platform has no such query.
    fn menu_font_family(&self) -> Option<String> {
        None
    }
    /// How the system smooths text, which decides whether Qt draws glyphs as LCD (ClearType)
    /// masks. Grey-scale (`TextSmoothing::OFF`) where Qt has no sub-pixel path.
    fn text_smoothing(&self) -> ferrokit_gui::text::smoothing::TextSmoothing {
        ferrokit_gui::text::smoothing::TextSmoothing::OFF
    }
    /// `QPlatformTheme::motionPreference`.
    fn motion_preference(&self) -> ferrokit_gui::application::MotionPreference {
        ferrokit_gui::application::MotionPreference::NoPreference
    }
}

#[cfg(windows)]
pub mod win32_theme {
    use super::*;
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ, REG_DWORD,
    };

    pub struct Win32Theme {
        pub(super) core: SchemeCore,
    }

    impl Default for Win32Theme {
        fn default() -> Self {
            let theme = Self {
                core: SchemeCore::new(Self::query_color_scheme()),
            };
            // Qt reads the font smoothing state once, when the first painter starts
            // (`QRasterPaintEngine::clearTypeFontsEnabled`, a function-local static).
            ferrokit_gui::text::smoothing::set_text_smoothing(theme.text_smoothing());
            ferrokit_gui::application::set_system_motion_preference(theme.motion_preference());
            theme
        }
    }

    impl Win32Theme {
        /// Windows build number (`CurrentBuildNumber`).
        fn build_number() -> Option<u32> {
            use windows_sys::Win32::System::Registry::{HKEY_LOCAL_MACHINE, REG_SZ};
            let subkey: Vec<u16> = "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion"
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            let value_name: Vec<u16> = "CurrentBuildNumber"
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            // SAFETY: plain registry reads into stack buffers whose size is passed along.
            unsafe {
                let mut hkey: HKEY = std::ptr::null_mut();
                if RegOpenKeyExW(HKEY_LOCAL_MACHINE, subkey.as_ptr(), 0, KEY_READ, &mut hkey) != 0 {
                    return None;
                }
                let mut val_type: u32 = 0;
                let mut buf = [0u16; 16];
                let mut size = std::mem::size_of_val(&buf) as u32;
                let status = RegQueryValueExW(
                    hkey,
                    value_name.as_ptr(),
                    std::ptr::null_mut(),
                    &mut val_type,
                    buf.as_mut_ptr() as *mut u8,
                    &mut size,
                );
                RegCloseKey(hkey);
                if status != 0 || val_type != REG_SZ {
                    return None;
                }
                let len = (size as usize / 2).min(buf.len());
                String::from_utf16_lossy(&buf[..len])
                    .trim_end_matches('\0')
                    .trim()
                    .parse()
                    .ok()
            }
        }

        pub fn new() -> Self {
            Self::default()
        }

        pub fn query_color_scheme() -> ColorScheme {
            let subkey: Vec<u16> =
                "Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"
                    .encode_utf16()
                    .chain(std::iter::once(0))
                    .collect();
            let value_name: Vec<u16> = "AppsUseLightTheme"
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();

            unsafe {
                let mut hkey: HKEY = std::ptr::null_mut();
                if RegOpenKeyExW(HKEY_CURRENT_USER, subkey.as_ptr(), 0, KEY_READ, &mut hkey) != 0 {
                    return ColorScheme::Unknown;
                }

                let mut val_type: u32 = 0;
                let mut data: u32 = 0;
                let mut data_size: u32 = std::mem::size_of::<u32>() as u32;

                let status = RegQueryValueExW(
                    hkey,
                    value_name.as_ptr(),
                    std::ptr::null_mut(),
                    &mut val_type,
                    &mut data as *mut u32 as *mut u8,
                    &mut data_size,
                );

                RegCloseKey(hkey);

                if status == 0 && val_type == REG_DWORD {
                    if data == 0 {
                        ColorScheme::Dark
                    } else {
                        ColorScheme::Light
                    }
                } else {
                    ColorScheme::Unknown
                }
            }
        }
    }

    impl PlatformTheme for Win32Theme {
        /// Qt picks `windows11` from build 22000 on and `windowsvista` before
        /// (`QWindowsIntegration`), whose menus draw differently.
        fn native_style(&self) -> NativeStyle {
            static STYLE: std::sync::LazyLock<NativeStyle> =
                std::sync::LazyLock::new(|| match Win32Theme::build_number() {
                    Some(build) if build >= 22000 => NativeStyle::Windows11,
                    _ => NativeStyle::WindowsVista,
                });
            *STYLE
        }

        fn color_scheme(&self) -> ColorScheme {
            self.core.effective()
        }

        fn theme_changed(&self) -> &Signal<ColorScheme> {
            self.core.signal()
        }

        fn menu_font_family(&self) -> Option<String> {
            use windows_sys::Win32::UI::WindowsAndMessaging::{
                SystemParametersInfoW, NONCLIENTMETRICSW, SPI_GETNONCLIENTMETRICS,
            };
            // SAFETY: `NONCLIENTMETRICSW` is plain data; `cbSize` is set as the API requires.
            unsafe {
                let mut ncm: NONCLIENTMETRICSW = std::mem::zeroed();
                ncm.cbSize = std::mem::size_of::<NONCLIENTMETRICSW>() as u32;
                if SystemParametersInfoW(
                    SPI_GETNONCLIENTMETRICS,
                    ncm.cbSize,
                    &mut ncm as *mut _ as *mut _,
                    0,
                ) == 0
                {
                    return None;
                }
                let face = &ncm.lfMenuFont.lfFaceName;
                let len = face.iter().position(|&c| c == 0).unwrap_or(face.len());
                let name = String::from_utf16_lossy(&face[..len]);
                (!name.is_empty()).then_some(name)
            }
        }

        /// `SPI_GETCLIENTAREAANIMATION` off means reduced motion; a failed query counts as off
        /// (`QWindowsTheme::motionPreference`, `booleanSystemParametersInfo(.., false)`).
        fn motion_preference(&self) -> ferrokit_gui::application::MotionPreference {
            use ferrokit_gui::application::MotionPreference;
            use windows_sys::Win32::UI::WindowsAndMessaging::SystemParametersInfoW;
            const SPI_GETCLIENTAREAANIMATION: u32 = 0x1042;
            let mut on: i32 = 0;
            // SAFETY: the query writes one `BOOL` through the pointer.
            let ok = unsafe {
                SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, &mut on as *mut _ as *mut _, 0)
            };
            if ok != 0 && on != 0 {
                MotionPreference::NoPreference
            } else {
                MotionPreference::ReducedMotion
            }
        }

        /// `SPI_GETFONTSMOOTHINGTYPE` is ClearType (`winClearTypeFontsEnabled`) and the
        /// gamma is `SPI_GETFONTSMOOTHINGCONTRAST / 1000`, 1.4 when out of 1..=5
        /// (`QWindowsFontDatabase::fontSmoothingGamma`).
        fn text_smoothing(&self) -> ferrokit_gui::text::smoothing::TextSmoothing {
            use windows_sys::Win32::UI::WindowsAndMessaging::SystemParametersInfoW;
            const SPI_GETFONTSMOOTHINGTYPE: u32 = 0x200A;
            const SPI_GETFONTSMOOTHINGCONTRAST: u32 = 0x200C;
            const FE_FONTSMOOTHINGCLEARTYPE: u32 = 0x0002;
            let mut kind: u32 = 0;
            let mut contrast: u32 = 0;
            // SAFETY: both queries write one `UINT` through the pointer.
            let (kind_ok, contrast_ok) = unsafe {
                (
                    SystemParametersInfoW(
                        SPI_GETFONTSMOOTHINGTYPE,
                        0,
                        &mut kind as *mut _ as *mut _,
                        0,
                    ),
                    SystemParametersInfoW(
                        SPI_GETFONTSMOOTHINGCONTRAST,
                        0,
                        &mut contrast as *mut _ as *mut _,
                        0,
                    ),
                )
            };
            // `QWindowsFontDatabase::fontSmoothingGamma` starts from 1 when the query fails.
            let mut gamma = if contrast_ok != 0 {
                contrast as f32 / 1000.0
            } else {
                1.0
            };
            if !(1.0..=5.0).contains(&gamma) {
                gamma = 1.4;
            }
            ferrokit_gui::text::smoothing::TextSmoothing {
                cleartype: kind_ok != 0 && kind == FE_FONTSMOOTHINGCLEARTYPE,
                gamma,
            }
        }

        /// Called when Windows says its settings changed (`WM_SETTINGCHANGE`, `WM_THEMECHANGED`):
        /// re-reads `AppsUseLightTheme`, as `QWindowsTheme::handleThemeChange` re-queries the
        /// registry (qwindowstheme.cpp:681-705). `theme_changed` fires only if the effective
        /// scheme moved.
        fn refresh(&self) {
            ferrokit_gui::application::set_system_motion_preference(self.motion_preference());
            self.core.set_system(Self::query_color_scheme());
        }

        /// `QWindowsTheme::requestColorScheme` (qwindowstheme.cpp:669-673): the request replaces
        /// the registry value until it is reset with [`ColorScheme::Unknown`].
        fn request_color_scheme(&self, scheme: ColorScheme) {
            self.core.set_requested(scheme);
        }
    }
}

#[cfg(windows)]
pub use win32_theme::Win32Theme;

pub struct GenericTheme {
    core: SchemeCore,
}

impl Default for GenericTheme {
    fn default() -> Self {
        Self::new(ColorScheme::Light)
    }
}

impl GenericTheme {
    pub fn new(initial: ColorScheme) -> Self {
        Self {
            core: SchemeCore::new(initial),
        }
    }

    /// Plays the operating system changing its scheme to `scheme`.
    pub fn set_color_scheme(&self, scheme: ColorScheme) {
        self.core.set_system(scheme);
    }
}

impl PlatformTheme for GenericTheme {
    fn color_scheme(&self) -> ColorScheme {
        self.core.effective()
    }

    fn theme_changed(&self) -> &Signal<ColorScheme> {
        self.core.signal()
    }

    fn refresh(&self) {}

    fn request_color_scheme(&self, scheme: ColorScheme) {
        self.core.set_requested(scheme);
    }
}

pub mod cocoa_theme {
    use super::*;

    /// The colour scheme on macOS, as `QCocoaTheme` has it: the effective appearance of
    /// `NSApp`, observed so that a change of the system appearance (or of `NSApp.appearance`)
    /// reaches `theme_changed`.
    ///
    /// **Not run on macOS by the author** (written on Windows): the observer registration and the
    /// appearance override are type-checked for `aarch64-apple-darwin` only.
    pub struct CocoaTheme {
        core: Arc<SchemeCore>,
    }

    impl Default for CocoaTheme {
        fn default() -> Self {
            let core = Arc::new(SchemeCore::new(Self::query_color_scheme()));
            Self::observe_appearance(&core);
            Self { core }
        }
    }

    impl CocoaTheme {
        pub fn new() -> Self {
            Self::default()
        }

        /// `QCocoaTheme::QCocoaTheme` (qcocoatheme.mm:217-230): key-value observation of
        /// `NSApp.effectiveAppearance` (`m_appearanceObserver`) and of
        /// `NSSystemColorsDidChangeNotification` (`m_systemColorObserver`); either re-reads the
        /// appearance (`handleSystemThemeChange` -> `updateColorScheme`, qcocoatheme.mm:245-262).
        /// Both are delivered on the main thread, which is where the appearance may be read.
        ///
        /// The observer object is never removed: the theme lives as long as the process.
        #[cfg(target_os = "macos")]
        fn observe_appearance(core: &Arc<SchemeCore>) {
            use crate::objc_runtime::{nsstring_from_str, ActionTarget, Class, ObjcMsg, Sel};
            let on_change = Arc::clone(core);
            let Ok(target) = ActionTarget::new(move || {
                on_change.set_system(Self::query_color_scheme());
            }) else {
                return;
            };
            let key_path = nsstring_from_str("effectiveAppearance");
            let notification = nsstring_from_str("NSSystemColorsDidChangeNotification");
            // SAFETY: `NSApp` and the default notification center are live objects; `target`
            // implements both `-observeValueForKeyPath:ofObject:change:context:` and
            // `-ferrokitPerformAction:`; the strings are released after the calls (both
            // registries copy or retain them); the target is leaked on purpose below.
            unsafe {
                let nsapp = ObjcMsg::send_class_0(
                    Class::get("NSApplication").unwrap_or(Class::NIL),
                    Sel::register("sharedApplication"),
                );
                ObjcMsg::send_add_key_path_observer(nsapp, target.id(), key_path, 0);
                let center = ObjcMsg::send_class_0(
                    Class::get("NSNotificationCenter").unwrap_or(Class::NIL),
                    Sel::register("defaultCenter"),
                );
                ObjcMsg::send_add_notification_observer(
                    center,
                    target.id(),
                    ActionTarget::action(),
                    notification,
                    crate::objc_runtime::Id::NIL,
                );
                ObjcMsg::send_0(key_path, Sel::register("release"));
                ObjcMsg::send_0(notification, Sel::register("release"));
            }
            // The registrations hold the target without retaining it, and its callback lives in
            // a thread-local keyed by the object: keep both for the life of the process.
            std::mem::forget(target);
        }

        /// The mock runtime posts no notifications, so there is nothing to observe.
        #[cfg(not(target_os = "macos"))]
        fn observe_appearance(_core: &Arc<SchemeCore>) {}

        /// `QCocoaTheme::updateColorScheme` (qcocoatheme.mm:505-511): Dark only when the
        /// effective appearance's best match among Aqua and DarkAqua is DarkAqua; Light otherwise.
        #[cfg(target_os = "macos")]
        pub fn query_color_scheme() -> ColorScheme {
            use crate::objc_runtime::{nsstring_from_str, Class, Id, ObjcMsg, Sel};
            let nsapp_class = Class::get("NSApplication").unwrap_or(Class::NIL);
            let nsapp =
                unsafe { ObjcMsg::send_class_0(nsapp_class, Sel::register("sharedApplication")) };
            let appearance =
                unsafe { ObjcMsg::send_0(nsapp, Sel::register("effectiveAppearance")) };

            let aqua = nsstring_from_str("NSAppearanceNameAqua");
            let dark_aqua = nsstring_from_str("NSAppearanceNameDarkAqua");
            let array_class = Id(Class::get("NSArray").unwrap_or(Class::NIL).0);
            let names =
                unsafe { ObjcMsg::send_id(array_class, Sel::register("arrayWithObject:"), aqua) };
            let names = unsafe {
                ObjcMsg::send_id(names, Sel::register("arrayByAddingObject:"), dark_aqua)
            };
            let best = unsafe {
                ObjcMsg::send_id(
                    appearance,
                    Sel::register("bestMatchFromAppearancesWithNames:"),
                    names,
                )
            };
            let dark = unsafe {
                ObjcMsg::send_id_bool_return(best, Sel::register("isEqualToString:"), dark_aqua)
            };
            unsafe { ObjcMsg::send_0(aqua, Sel::register("release")) };
            unsafe { ObjcMsg::send_0(dark_aqua, Sel::register("release")) };
            if dark {
                ColorScheme::Dark
            } else {
                ColorScheme::Light
            }
        }

        /// The mock runtime has no appearance; like Qt's fallback this is Light.
        #[cfg(not(target_os = "macos"))]
        pub fn query_color_scheme() -> ColorScheme {
            ColorScheme::Light
        }

        /// Plays the appearance changing to `scheme`.
        pub fn set_color_scheme(&self, scheme: ColorScheme) {
            self.core.set_system(scheme);
        }
    }

    impl PlatformTheme for CocoaTheme {
        fn native_style(&self) -> NativeStyle {
            NativeStyle::Macintosh
        }

        fn color_scheme(&self) -> ColorScheme {
            self.core.effective()
        }

        fn theme_changed(&self) -> &Signal<ColorScheme> {
            self.core.signal()
        }

        fn refresh(&self) {
            self.core.set_system(Self::query_color_scheme());
        }

        /// `QCocoaTheme::requestColorScheme` (qcocoatheme.mm:478-496): sets `NSApp.appearance` to
        /// Aqua or DarkAqua, or to nil (follow the system) for [`ColorScheme::Unknown`]. The
        /// appearance observer then reports the change, so the effective scheme is always what
        /// AppKit says. Must run on the main thread.
        ///
        /// Without AppKit (the mock runtime) the request is kept next to the system scheme, as the
        /// other themes do.
        #[cfg(target_os = "macos")]
        fn request_color_scheme(&self, scheme: ColorScheme) {
            use crate::objc_runtime::{nsstring_from_str, Class, Id, ObjcMsg, Sel};
            let name = match scheme {
                ColorScheme::Dark => Some("NSAppearanceNameDarkAqua"),
                ColorScheme::Light => Some("NSAppearanceNameAqua"),
                ColorScheme::Unknown => None,
            };
            // SAFETY: `NSAppearance` and `NSApp` are live; `-appearanceNamed:` takes an NSString
            // and `-setAppearance:` an NSAppearance or nil; the string is released after use.
            unsafe {
                let appearance = match name {
                    Some(name) => {
                        let string = nsstring_from_str(name);
                        let appearance = ObjcMsg::send_id(
                            Id(Class::get("NSAppearance").unwrap_or(Class::NIL).0),
                            Sel::register("appearanceNamed:"),
                            string,
                        );
                        ObjcMsg::send_0(string, Sel::register("release"));
                        appearance
                    }
                    None => Id::NIL,
                };
                let nsapp = ObjcMsg::send_class_0(
                    Class::get("NSApplication").unwrap_or(Class::NIL),
                    Sel::register("sharedApplication"),
                );
                // Qt always sets it, even to the current appearance: otherwise the request would
                // not stick across system theme changes.
                ObjcMsg::send_id(nsapp, Sel::register("setAppearance:"), appearance);
            }
            // KVO has normally reported the change already; this makes it certain.
            self.core.set_system(Self::query_color_scheme());
        }

        #[cfg(not(target_os = "macos"))]
        fn request_color_scheme(&self, scheme: ColorScheme) {
            self.core.set_requested(scheme);
        }
    }
}

pub use cocoa_theme::CocoaTheme;

pub mod unix_theme {
    use super::*;
    use std::sync::atomic::{AtomicU8, Ordering};

    /// What the theme tracks, shared with the portal listener thread.
    struct UnixInner {
        core: SchemeCore,
        /// The scheme the portal reported last (`Unknown` until it reports one).
        portal: AtomicU8,
    }

    impl UnixInner {
        /// The system scheme: the portal's when it has an opinion, else what the GTK theme says
        /// (`QGnomeThemePrivate::colorScheme`, qgnometheme.cpp:56-78).
        fn recompute(&self) {
            let portal = ColorScheme::from(self.portal.load(Ordering::Acquire));
            let system = if portal != ColorScheme::Unknown {
                portal
            } else {
                UnixTheme::query_color_scheme()
            };
            self.core.set_system(system);
        }
    }

    /// The colour scheme on Linux and the other Unixes.
    ///
    /// Follows the XDG desktop portal's `org.freedesktop.appearance` `color-scheme` over D-Bus
    /// (GNOME, KDE and others), including its `SettingChanged` signal, from a listener thread.
    /// Without a portal (or when it has no preference) the GTK theme decides: `GTK_THEME`, then
    /// `gtk-application-prefer-dark-theme` / `gtk-theme-name` of `gtk-3.0/settings.ini`, read at
    /// construction and by [`refresh`](PlatformTheme::refresh); the fallback is not watched.
    /// With neither the scheme is [`ColorScheme::Unknown`], as in Qt.
    ///
    /// `theme_changed` is emitted on the listener thread; `StyleHints::color_scheme_changed`
    /// moves it to the main thread.
    ///
    /// **Not run on Linux by the author** (written on Windows): the portal protocol is tested
    /// against a scripted bus, the socket wait and thread are type-checked for
    /// `x86_64-unknown-linux-gnu` only.
    pub struct UnixTheme {
        inner: Arc<UnixInner>,
    }

    impl Default for UnixTheme {
        fn default() -> Self {
            let theme = Self::without_portal();
            theme.listen_to_portal();
            theme
        }
    }

    impl UnixTheme {
        pub fn new() -> Self {
            Self::default()
        }

        /// A theme that never talks to D-Bus.
        pub(super) fn without_portal() -> Self {
            Self {
                inner: Arc::new(UnixInner {
                    core: SchemeCore::new(Self::query_color_scheme()),
                    portal: AtomicU8::new(ColorScheme::Unknown as u8),
                }),
            }
        }

        /// Connects to the portal (if there is a session bus) and waits briefly for its answer,
        /// so the first scheme read is the portal's. A portal that stays silent costs this wait
        /// once, at start-up; its answer is then applied when it comes.
        #[cfg(all(target_os = "linux", not(miri)))]
        fn listen_to_portal(&self) {
            use crate::theme_portal::{spawn_listener, PortalEvent};
            let for_alive = Arc::downgrade(&self.inner);
            let for_events = Arc::downgrade(&self.inner);
            spawn_listener(
                std::time::Duration::from_millis(300),
                move || for_alive.strong_count() > 0,
                move |event| {
                    let Some(inner) = for_events.upgrade() else {
                        return;
                    };
                    match event {
                        PortalEvent::Initial(Some(scheme)) | PortalEvent::Changed(scheme) => {
                            inner.portal.store(scheme as u8, Ordering::Release);
                        }
                        PortalEvent::Initial(None) => {}
                    }
                    inner.recompute();
                },
            );
        }

        /// Miri cannot open a bus socket, and other systems have no portal.
        #[cfg(not(all(target_os = "linux", not(miri))))]
        fn listen_to_portal(&self) {}

        /// The scheme the GTK theme settings say, when no portal answers: `GTK_THEME`, then
        /// `gtk-3.0/settings.ini`. [`ColorScheme::Unknown`] when neither says.
        pub fn query_color_scheme() -> ColorScheme {
            if let Ok(name) = std::env::var("GTK_THEME") {
                let scheme = scheme_from_theme_name(&name);
                if scheme != ColorScheme::Unknown {
                    return scheme;
                }
            }
            // Miri cannot open files while isolated, so the settings file does not exist there.
            #[cfg(miri)]
            {
                ColorScheme::Unknown
            }
            #[cfg(not(miri))]
            {
                gtk_settings_ini_path()
                    .and_then(|path| std::fs::read_to_string(path).ok())
                    .map_or(ColorScheme::Unknown, |text| scheme_from_settings_ini(&text))
            }
        }

        /// Plays the portal reporting `scheme` (`Unknown` for "no preference").
        pub fn set_color_scheme(&self, scheme: ColorScheme) {
            self.inner.portal.store(scheme as u8, Ordering::Release);
            self.inner.recompute();
        }
    }

    /// `QGnomeThemePrivate::colorScheme`'s theme-name test (qgnometheme.cpp:68-74): a name with
    /// "light" in it is Light, else with "dark" Dark, case-insensitively.
    pub(super) fn scheme_from_theme_name(name: &str) -> ColorScheme {
        let name = name.to_lowercase();
        if name.contains("light") {
            ColorScheme::Light
        } else if name.contains("dark") {
            ColorScheme::Dark
        } else {
            ColorScheme::Unknown
        }
    }

    /// The GTK settings file: `$XDG_CONFIG_HOME/gtk-3.0/settings.ini`, else
    /// `~/.config/gtk-3.0/settings.ini`.
    #[cfg(not(miri))]
    fn gtk_settings_ini_path() -> Option<std::path::PathBuf> {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|dir| !dir.is_empty())
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .filter(|dir| !dir.is_empty())
                    .map(|home| std::path::Path::new(&home).join(".config"))
            })?;
        Some(config.join("gtk-3.0").join("settings.ini"))
    }

    /// What a `gtk-3.0/settings.ini` says: `gtk-application-prefer-dark-theme=true` (or `1`) is
    /// Dark; else the theme name decides ([`scheme_from_theme_name`]).
    pub(super) fn scheme_from_settings_ini(text: &str) -> ColorScheme {
        let mut theme_name = ColorScheme::Unknown;
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            match key.trim() {
                "gtk-application-prefer-dark-theme"
                    if value == "1" || value.eq_ignore_ascii_case("true") =>
                {
                    return ColorScheme::Dark;
                }
                "gtk-theme-name" => theme_name = scheme_from_theme_name(value),
                _ => {}
            }
        }
        theme_name
    }

    impl PlatformTheme for UnixTheme {
        fn color_scheme(&self) -> ColorScheme {
            self.inner.core.effective()
        }

        fn theme_changed(&self) -> &Signal<ColorScheme> {
            self.inner.core.signal()
        }

        fn refresh(&self) {
            self.inner.recompute();
        }

        /// `QGnomeTheme::requestColorScheme` (qgnometheme.cpp:175-183): remembers the request; the
        /// effective scheme is the request unless it is [`ColorScheme::Unknown`].
        fn request_color_scheme(&self, scheme: ColorScheme) {
            self.inner.core.set_requested(scheme);
        }
    }
}

pub use unix_theme::UnixTheme;

/// Connects `theme` to `QStyleHints`: [`GuiApplication::style_hints`](ferrokit_gui::application::GuiApplication::style_hints)
/// then reads, overrides and signals the colour scheme through it. `platform()` does this for the
/// default integration and `set_platform_integration` for a replacement, so applications do not
/// call it; it is public for tests that drive a mock theme. A later call replaces the earlier
/// theme (its `theme_changed` is disconnected).
///
/// The theme's `theme_changed` may fire on any thread; the style hints re-read the scheme and
/// emit `color_scheme_changed` on the main thread (`QGuiApplicationPrivate::handleThemeChanged`,
/// qguiapplication.cpp:2854-2856).
pub fn install_style_hints_theme(theme: Arc<dyn PlatformTheme>) {
    struct ThemeSource(Arc<dyn PlatformTheme>);

    impl ColorSchemeSource for ThemeSource {
        fn color_scheme(&self) -> ColorScheme {
            self.0.color_scheme()
        }

        fn request_color_scheme(&self, scheme: ColorScheme) {
            self.0.request_color_scheme(scheme);
        }
    }

    static BRIDGE: Mutex<Option<(Arc<dyn PlatformTheme>, ConnectionId)>> = Mutex::new(None);
    {
        let mut bridge = BRIDGE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((previous, connection)) = bridge.take() {
            previous.theme_changed().disconnect(connection);
        }
        let connection = theme
            .theme_changed()
            .connect(|_| ferrokit_gui::application::handle_theme_changed());
        *bridge = Some((Arc::clone(&theme), connection));
    }
    // Outside the lock: this reads the scheme and may run `color_scheme_changed` slots.
    ferrokit_gui::application::set_color_scheme_source(Arc::new(ThemeSource(theme)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn test_generic_theme_color_scheme_and_signals() {
        let theme = GenericTheme::new(ColorScheme::Light);
        assert_eq!(theme.color_scheme(), ColorScheme::Light);

        let change_count = Arc::new(AtomicUsize::new(0));
        let last_scheme = Arc::new(std::sync::Mutex::new(ColorScheme::Unknown));
        let c_cnt = Arc::clone(&change_count);
        let l_sch = Arc::clone(&last_scheme);

        theme.theme_changed().connect(move |s| {
            c_cnt.fetch_add(1, Ordering::SeqCst);
            *l_sch.lock().unwrap() = *s;
        });

        // Changing to same scheme does not emit
        theme.set_color_scheme(ColorScheme::Light);
        assert_eq!(change_count.load(Ordering::SeqCst), 0);

        // Changing to Dark emits
        theme.set_color_scheme(ColorScheme::Dark);
        assert_eq!(theme.color_scheme(), ColorScheme::Dark);
        assert_eq!(change_count.load(Ordering::SeqCst), 1);
        assert_eq!(*last_scheme.lock().unwrap(), ColorScheme::Dark);
    }

    use std::sync::Mutex as StdMutex;

    /// Records what `theme_changed` emitted.
    fn record(theme: &dyn PlatformTheme) -> Arc<StdMutex<Vec<ColorScheme>>> {
        let seen = Arc::new(StdMutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        theme
            .theme_changed()
            .connect(move |scheme| sink.lock().unwrap().push(*scheme));
        seen
    }

    #[test]
    fn a_request_overrides_the_system_scheme_until_it_is_reset() {
        let theme = GenericTheme::new(ColorScheme::Light);
        let seen = record(&theme);

        theme.request_color_scheme(ColorScheme::Dark);
        assert_eq!(theme.color_scheme(), ColorScheme::Dark);
        // The system moves under the override: the effective scheme does not, so nothing is emitted.
        theme.set_color_scheme(ColorScheme::Dark);
        theme.set_color_scheme(ColorScheme::Light);
        assert_eq!(theme.color_scheme(), ColorScheme::Dark);
        assert_eq!(*seen.lock().unwrap(), [ColorScheme::Dark]);

        // Unknown follows the system again, whose current scheme is Light.
        theme.request_color_scheme(ColorScheme::Unknown);
        assert_eq!(theme.color_scheme(), ColorScheme::Light);
        assert_eq!(
            *seen.lock().unwrap(),
            [ColorScheme::Dark, ColorScheme::Light]
        );
    }

    #[test]
    fn a_request_for_the_scheme_in_effect_emits_nothing() {
        let theme = GenericTheme::new(ColorScheme::Dark);
        let seen = record(&theme);
        theme.request_color_scheme(ColorScheme::Dark);
        theme.request_color_scheme(ColorScheme::Unknown);
        assert!(seen.lock().unwrap().is_empty());
    }

    #[test]
    fn the_default_theme_ignores_requests() {
        struct Fixed(Signal<ColorScheme>);
        impl PlatformTheme for Fixed {
            fn color_scheme(&self) -> ColorScheme {
                ColorScheme::Light
            }
            fn theme_changed(&self) -> &Signal<ColorScheme> {
                &self.0
            }
            fn refresh(&self) {}
        }
        let theme = Fixed(Signal::new());
        theme.request_color_scheme(ColorScheme::Dark);
        assert_eq!(theme.color_scheme(), ColorScheme::Light);
    }

    #[test]
    fn unix_theme_names_decide_like_qgnometheme() {
        use unix_theme::{scheme_from_settings_ini, scheme_from_theme_name};
        assert_eq!(scheme_from_theme_name("Adwaita"), ColorScheme::Unknown);
        assert_eq!(scheme_from_theme_name("Adwaita:dark"), ColorScheme::Dark);
        assert_eq!(scheme_from_theme_name("Breeze-Light"), ColorScheme::Light);
        assert_eq!(
            scheme_from_theme_name("HighContrastLight-dark"),
            ColorScheme::Light
        );

        let ini = "[Settings]\ngtk-theme-name = Adwaita-dark\n";
        assert_eq!(scheme_from_settings_ini(ini), ColorScheme::Dark);
        let ini = "[Settings]\ngtk-theme-name=Adwaita\ngtk-application-prefer-dark-theme=1\n";
        assert_eq!(scheme_from_settings_ini(ini), ColorScheme::Dark);
        let ini = "[Settings]\ngtk-application-prefer-dark-theme=false\ngtk-theme-name=Breeze\n";
        assert_eq!(scheme_from_settings_ini(ini), ColorScheme::Unknown);
        assert_eq!(scheme_from_settings_ini(""), ColorScheme::Unknown);
    }

    #[test]
    fn unix_theme_follows_the_portal_and_falls_back_on_no_preference() {
        let theme = UnixTheme::without_portal();
        let fallback = UnixTheme::query_color_scheme();
        let seen = record(&theme);

        theme.set_color_scheme(ColorScheme::Dark);
        assert_eq!(theme.color_scheme(), ColorScheme::Dark);
        theme.set_color_scheme(ColorScheme::Light);
        assert_eq!(theme.color_scheme(), ColorScheme::Light);
        // The portal has no preference: the GTK theme settings decide.
        theme.set_color_scheme(ColorScheme::Unknown);
        assert_eq!(theme.color_scheme(), fallback);

        // Whatever the machine's GTK settings are, the log is consistent with the effective scheme.
        assert_eq!(
            seen.lock().unwrap().last().copied(),
            Some(theme.color_scheme())
        );
    }

    #[test]
    fn unix_theme_override_beats_the_portal() {
        let theme = UnixTheme::without_portal();
        theme.set_color_scheme(ColorScheme::Light);
        let seen = record(&theme);
        theme.request_color_scheme(ColorScheme::Dark);
        theme.set_color_scheme(ColorScheme::Dark);
        theme.set_color_scheme(ColorScheme::Light);
        assert_eq!(theme.color_scheme(), ColorScheme::Dark);
        theme.request_color_scheme(ColorScheme::Unknown);
        assert_eq!(theme.color_scheme(), ColorScheme::Light);
        assert_eq!(
            *seen.lock().unwrap(),
            [ColorScheme::Dark, ColorScheme::Light]
        );
    }

    /// The mock runtime has no AppKit: the request is kept next to the system scheme.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn cocoa_theme_override_without_appkit() {
        let theme = CocoaTheme::new();
        assert_eq!(theme.color_scheme(), ColorScheme::Light);
        let seen = record(&theme);
        theme.request_color_scheme(ColorScheme::Dark);
        assert_eq!(theme.color_scheme(), ColorScheme::Dark);
        theme.refresh();
        assert_eq!(theme.color_scheme(), ColorScheme::Dark);
        theme.request_color_scheme(ColorScheme::Unknown);
        assert_eq!(theme.color_scheme(), ColorScheme::Light);
        assert_eq!(
            *seen.lock().unwrap(),
            [ColorScheme::Dark, ColorScheme::Light]
        );
    }

    /// `refresh` is what the window procedure runs on `WM_SETTINGCHANGE` / `WM_THEMECHANGED`: it
    /// re-reads `AppsUseLightTheme`, and `theme_changed` fires once, only if the scheme moved.
    #[cfg(windows)]
    #[test]
    fn win32_refresh_emits_once_when_the_registry_value_moved() {
        let theme = Win32Theme::new();
        let registry = Win32Theme::query_color_scheme();
        let seen = record(&theme);

        // Make the cached scheme stale, as if the value had changed since it was read.
        let stale = if registry == ColorScheme::Dark {
            ColorScheme::Light
        } else {
            ColorScheme::Dark
        };
        theme.core.set_system(stale);
        assert_eq!(*seen.lock().unwrap(), [stale]);

        theme.refresh();
        assert_eq!(theme.color_scheme(), registry);
        assert_eq!(*seen.lock().unwrap(), [stale, registry]);
        // The next notification finds nothing new.
        theme.refresh();
        assert_eq!(seen.lock().unwrap().len(), 2);
    }

    #[cfg(windows)]
    #[test]
    fn win32_request_overrides_the_registry() {
        let theme = Win32Theme::new();
        let registry = Win32Theme::query_color_scheme();
        let other = if registry == ColorScheme::Dark {
            ColorScheme::Light
        } else {
            ColorScheme::Dark
        };
        theme.request_color_scheme(other);
        assert_eq!(theme.color_scheme(), other);
        theme.refresh();
        assert_eq!(
            theme.color_scheme(),
            other,
            "the override survives a settings change"
        );
        theme.request_color_scheme(ColorScheme::Unknown);
        assert_eq!(theme.color_scheme(), registry);
    }

    #[cfg(windows)]
    #[test]
    fn test_win32_theme_query_and_refresh() {
        let theme = Win32Theme::new();
        let scheme = theme.color_scheme();
        assert!(matches!(
            scheme,
            ColorScheme::Dark | ColorScheme::Light | ColorScheme::Unknown
        ));
        // refresh without OS setting change does not crash or emit spurious signals
        theme.refresh();
        assert_eq!(theme.color_scheme(), scheme);
    }
}
