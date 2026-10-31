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
                core: SchemeCore::new(Self::system_color_scheme()),
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
        pub(crate) fn build_number() -> Option<u32> {
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

        /// What the system contributes to the effective scheme: the registry value, but
        /// `Light` when the style part of dark mode is switched off
        /// (`QWindowsTheme::effectiveColorScheme`, qwindowstheme.cpp:655-667;
        /// `-platform windows:darkmode=0|1`, see [`crate::dark_mode`]).
        fn system_color_scheme() -> ColorScheme {
            let style = crate::dark_mode::DarkModeHandling::STYLE;
            if !crate::dark_mode::dark_mode_handling().contains(style) {
                return ColorScheme::Light;
            }
            Self::query_color_scheme()
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
                SystemParametersInfoW(
                    SPI_GETCLIENTAREAANIMATION,
                    0,
                    &mut on as *mut _ as *mut _,
                    0,
                )
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
            self.core.set_system(Self::system_color_scheme());
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
    /// AppKit rules apply as in Qt: the appearance is read, and the observers are registered, on
    /// the main thread (the application's: ferrokit's main thread, which need not be the OS main
    /// thread; with no application yet, the OS main thread). A theme constructed elsewhere
    /// registers them from the application's main thread when it gets to run an event (and
    /// without an application, on a thread that is not the OS main thread, not at all: it then
    /// updates only through [`refresh`](PlatformTheme::refresh), and says so on stderr). The
    /// observers are removed when the theme is dropped.
    ///
    /// Like the rest of this crate's AppKit code (`query_color_scheme`) the theme uses
    /// `+[NSApplication sharedApplication]`, which creates `NSApp` if it does not exist yet; Qt
    /// reads the `NSApp` its integration has already made. Only a program that wants a custom
    /// `NSApplication` subclass has to create it before the first `platform()` call.
    ///
    /// **Not run on macOS by the author** (written on Windows): the observer registration and the
    /// appearance override are type-checked for `aarch64-apple-darwin` only.
    pub struct CocoaTheme {
        core: Arc<SchemeCore>,
        /// The registered observers; shared with a deferred registration (see above).
        #[allow(dead_code)] // held for its `Drop`, which removes the observers
        observer: Arc<Mutex<Option<AppearanceObserver>>>,
    }

    /// The KVO and notification observers of one theme (`m_appearanceObserver` and
    /// `m_systemColorObserver` of `QCocoaTheme`). Removed on drop.
    #[cfg(target_os = "macos")]
    struct AppearanceObserver {
        target: crate::objc_runtime::ActionTarget,
        nsapp: crate::objc_runtime::Id,
        center: crate::objc_runtime::Id,
        /// `@"effectiveAppearance"`, +1.
        key_path: crate::objc_runtime::Id,
    }

    // SAFETY: the `Id`s are only messaged to remove the observers, which AppKit allows from any
    // thread for key-value and notification observers; `NSApp`, the notification center and the
    // key path outlive the observer (the first two are process-lifetime singletons).
    #[cfg(target_os = "macos")]
    unsafe impl Send for AppearanceObserver {}

    #[cfg(target_os = "macos")]
    impl Drop for AppearanceObserver {
        fn drop(&mut self) {
            use crate::objc_runtime::{ObjcMsg, Sel};
            // SAFETY: undoes the registrations made in `register`, with the same objects.
            unsafe {
                ObjcMsg::send_remove_key_path_observer(self.nsapp, self.target.id(), self.key_path);
                ObjcMsg::send_remove_notification_observer(self.center, self.target.id());
                ObjcMsg::send_0(self.key_path, Sel::register("release"));
            }
        }
    }

    /// Nothing is observed without AppKit.
    #[cfg(not(target_os = "macos"))]
    struct AppearanceObserver;

    /// What [`CocoaTheme::observe_plan`] decides for a theme being created.
    #[cfg(any(target_os = "macos", test))]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum ObservePlan {
        /// Register here and now.
        Now,
        /// Register from this thread (the application's main thread).
        Post(ferrokit_core::object::ThreadId),
        /// No thread to register from: the theme follows changes only through `refresh`.
        Never,
    }

    impl Default for CocoaTheme {
        fn default() -> Self {
            let core = Arc::new(SchemeCore::new(Self::query_color_scheme()));
            let observer = Arc::new(Mutex::new(None));
            Self::observe_appearance(&core, &observer);
            Self { core, observer }
        }
    }

    impl CocoaTheme {
        pub fn new() -> Self {
            Self::default()
        }

        /// Where the observers are registered, given the application's main thread (when it has
        /// named one), the calling thread and whether that is the OS main thread
        /// (`pthread_main_np`). Ferrokit's main thread is the application's, and need not be the
        /// OS one (a test, or a program that runs its application on a spawned thread), so it
        /// decides when there is one; the OS main thread is only the answer with no application.
        #[cfg(any(target_os = "macos", test))]
        fn observe_plan(
            app_main: Option<ferrokit_core::object::ThreadId>,
            current: ferrokit_core::object::ThreadId,
            os_main: bool,
        ) -> ObservePlan {
            match app_main {
                Some(main) if main == current => ObservePlan::Now,
                Some(main) => ObservePlan::Post(main),
                None if os_main => ObservePlan::Now,
                None => ObservePlan::Never,
            }
        }

        /// `QCocoaTheme::QCocoaTheme` (qcocoatheme.mm:217-230): key-value observation of
        /// `NSApp.effectiveAppearance` and of `NSSystemColorsDidChangeNotification`; either
        /// re-reads the appearance (`handleSystemThemeChange` -> `updateColorScheme`,
        /// qcocoatheme.mm:245-262). On the application's main thread this registers at once;
        /// elsewhere the registration is posted to it. See [`observe_plan`](Self::observe_plan).
        #[cfg(target_os = "macos")]
        fn observe_appearance(
            core: &Arc<SchemeCore>,
            slot: &Arc<Mutex<Option<AppearanceObserver>>>,
        ) {
            extern "C" {
                fn pthread_main_np() -> i32;
            }
            use ferrokit_core::event::{Event, EventKind};
            use ferrokit_core::object::{ObjectId, ThreadContext, ThreadId};
            // SAFETY: `pthread_main_np` takes no arguments and only reads the calling thread.
            let os_main = unsafe { pthread_main_np() } != 0;
            match Self::observe_plan(
                ThreadContext::main_thread_id(),
                ThreadId::current(),
                os_main,
            ) {
                ObservePlan::Now => {
                    *slot.lock().unwrap_or_else(|e| e.into_inner()) = Self::register(core);
                }
                ObservePlan::Post(main) => {
                    let core = Arc::clone(core);
                    let slot = Arc::clone(slot);
                    ferrokit_core::event_loop::post_event_to_thread(
                        main,
                        ObjectId(0),
                        Event::new(EventKind::MetaCallNoTarget(Box::new(move || {
                            // The theme is gone: nothing will ever drop what this would register.
                            if Arc::strong_count(&slot) > 1 {
                                *slot.lock().unwrap_or_else(|e| e.into_inner()) =
                                    Self::register(&core);
                            }
                        }))),
                    );
                }
                ObservePlan::Never => eprintln!(
                    "ferrokit: the macOS theme was created off the main thread with no \
                     application to run on; it will not follow appearance changes"
                ),
            }
        }

        /// Registers the observers; the main thread only.
        #[cfg(target_os = "macos")]
        fn register(core: &Arc<SchemeCore>) -> Option<AppearanceObserver> {
            use crate::objc_runtime::{nsstring_from_str, ActionTarget, Class, Id, ObjcMsg, Sel};
            // Weak: the observers must not keep the theme's state alive.
            let weak = Arc::downgrade(core);
            let target = ActionTarget::new_shared(move || {
                if let Some(core) = weak.upgrade() {
                    core.set_system(Self::query_color_scheme());
                }
            })
            .ok()?;
            let key_path = nsstring_from_str("effectiveAppearance");
            let notification = nsstring_from_str("NSSystemColorsDidChangeNotification");
            // SAFETY: `NSApp` and the default notification center are live objects; `target`
            // implements both `-observeValueForKeyPath:ofObject:change:context:` and
            // `-ferrokitPerformAction:`; the registries do not retain the observer, which
            // `AppearanceObserver::drop` unregisters before the target is released; the
            // notification name is released here (the center copies it), the key path in `drop`.
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
                    Id::NIL,
                );
                ObjcMsg::send_0(notification, Sel::register("release"));
                Some(AppearanceObserver {
                    target,
                    nsapp,
                    center,
                    key_path,
                })
            }
        }

        /// Without AppKit (the mock runtime) there is nothing to observe.
        #[cfg(not(target_os = "macos"))]
        fn observe_appearance(
            _core: &Arc<SchemeCore>,
            _slot: &Arc<Mutex<Option<AppearanceObserver>>>,
        ) {
        }

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

    #[cfg(test)]
    mod tests {
        use super::*;
        use ferrokit_core::object::ThreadId;

        fn other_thread_id() -> ThreadId {
            std::thread::spawn(ThreadId::current).join().unwrap()
        }

        #[test]
        fn the_application_main_thread_registers_at_once_even_off_the_os_main_thread() {
            let me = ThreadId::current();
            // A ferrokit main thread that `pthread_main_np` does not call the main thread.
            assert_eq!(
                CocoaTheme::observe_plan(Some(me), me, false),
                ObservePlan::Now
            );
            assert_eq!(
                CocoaTheme::observe_plan(Some(me), me, true),
                ObservePlan::Now
            );
        }

        #[test]
        fn another_thread_posts_to_the_application_main_thread() {
            let me = ThreadId::current();
            let main = other_thread_id();
            assert_eq!(
                CocoaTheme::observe_plan(Some(main), me, false),
                ObservePlan::Post(main)
            );
            // Even the OS main thread posts when the application lives elsewhere.
            assert_eq!(
                CocoaTheme::observe_plan(Some(main), me, true),
                ObservePlan::Post(main)
            );
        }

        #[test]
        fn without_an_application_only_the_os_main_thread_registers() {
            let me = ThreadId::current();
            assert_eq!(CocoaTheme::observe_plan(None, me, true), ObservePlan::Now);
            assert_eq!(
                CocoaTheme::observe_plan(None, me, false),
                ObservePlan::Never
            );
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

        /// Applies what the portal told, then recomputes (which emits `theme_changed` when the
        /// effective scheme moved).
        #[cfg(any(all(target_os = "linux", not(miri)), test))]
        fn apply(&self, event: crate::theme_portal::PortalEvent) {
            use crate::theme_portal::PortalEvent;
            match event {
                PortalEvent::Initial(Some(scheme)) | PortalEvent::Changed(scheme) => {
                    self.portal.store(scheme as u8, Ordering::Release);
                }
                PortalEvent::Initial(None) => {}
            }
            self.recompute();
        }
    }

    thread_local! {
        static ON_LISTENER_THREAD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    /// Marks the calling thread as a portal listener thread (see [`on_listener_thread`]).
    #[cfg(any(all(target_os = "linux", not(miri)), test))]
    pub(crate) fn mark_listener_thread() {
        ON_LISTENER_THREAD.with(|flag| flag.set(true));
    }

    /// Whether the caller is a portal listener thread: a slot of `theme_changed` may read the
    /// theme from there.
    fn on_listener_thread() -> bool {
        ON_LISTENER_THREAD.with(|flag| flag.get())
    }

    /// The colour scheme on Linux and the other Unixes.
    ///
    /// Follows the XDG desktop portal's `org.freedesktop.appearance` `color-scheme` over D-Bus
    /// (GNOME, KDE and others), including its `SettingChanged` signal, from a listener thread:
    /// `QGnomePortalInterface` / `QDBusListener` (qgnomeportalinterface.cpp, qdbuslistener.cpp).
    /// When the portal has no preference (or there is none) the fallback decides: the theme name
    /// of `GTK_THEME` (Qt's `QGnomeThemePrivate::colorScheme` tests the GTK theme name for
    /// "light" / "dark"); with neither the scheme is [`ColorScheme::Unknown`], as in Qt.
    ///
    /// **Extension, not in Qt:** the fallback also reads `gtk-3.0/settings.ini`
    /// (`gtk-application-prefer-dark-theme`, which wins when set, and `gtk-theme-name`). It is
    /// read at construction and by [`refresh`](PlatformTheme::refresh), not watched.
    ///
    /// Construction does not wait for the portal. The first [`color_scheme`](PlatformTheme::color_scheme)
    /// read waits up to 300 ms for the portal's answer to the start-up query (nothing when there is
    /// no session bus or the portal answers at once), so the first scheme read is the portal's in
    /// the usual case. An answer that comes later is applied then; it changes the effective scheme,
    /// and so emits `theme_changed`, only if it differs from what was read before.
    ///
    /// `theme_changed` is emitted on the listener thread; `StyleHints::color_scheme_changed`
    /// moves it to the main thread.
    ///
    /// **Not run on Linux by the author** (written on Windows): the portal protocol is tested
    /// against a scripted bus, the socket wait and thread are type-checked for
    /// `x86_64-unknown-linux-gnu` only.
    pub struct UnixTheme {
        inner: Arc<UnixInner>,
        /// Signalled by the listener when the start-up query has been answered; taken (and
        /// waited on, once) by the first scheme read.
        initial_answer: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
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
                initial_answer: Mutex::new(None),
            }
        }

        /// Connects to the portal (if there is a session bus) and starts following it.
        #[cfg(all(target_os = "linux", not(miri)))]
        fn listen_to_portal(&self) {
            use crate::theme_portal::spawn_listener;
            let for_alive = Arc::downgrade(&self.inner);
            let for_events = Arc::downgrade(&self.inner);
            let initial = spawn_listener(
                move || for_alive.strong_count() > 0,
                move |event| {
                    let Some(inner) = for_events.upgrade() else {
                        return;
                    };
                    // `apply` stores the scheme and recomputes before any slot of `theme_changed`
                    // runs, so a slot reading the theme from here sees the new value and, being on
                    // the listener thread, never waits for this thread's own answer.
                    inner.apply(event);
                },
            );
            *self
                .initial_answer
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = initial;
        }

        /// Miri cannot open a bus socket, and other systems have no portal.
        #[cfg(not(all(target_os = "linux", not(miri))))]
        fn listen_to_portal(&self) {}

        /// Waits (once, at most 300 ms) for the portal's answer to the start-up query. Never on
        /// the listener thread itself: a `theme_changed` slot (for instance
        /// `handle_theme_changed` with no application yet, which re-reads the scheme in place)
        /// runs there while the answer is still being handed over, and would wait out the
        /// timeout for its own answer.
        fn wait_for_initial_answer(&self) {
            if on_listener_thread() {
                return;
            }
            let pending = self
                .initial_answer
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take();
            if let Some(answered) = pending {
                let _ = answered.recv_timeout(std::time::Duration::from_millis(300));
            }
        }

        /// The fallback scheme, when no portal answers: the theme name in `GTK_THEME`
        /// (`QGnomeThemePrivate::colorScheme`'s name test), then, as an extension not in Qt,
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

    /// What a `gtk-3.0/settings.ini` says (an extension, Qt does not read it):
    /// `gtk-application-prefer-dark-theme=true` (or `1`) is Dark; else the theme name decides
    /// ([`scheme_from_theme_name`]).
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
            self.wait_for_initial_answer();
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

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::theme_portal::PortalEvent;
        use std::sync::mpsc::{channel, Sender};
        use std::time::{Duration, Instant};

        /// A generous upper bound for a read that finds its answer in: a missed wake-up would show
        /// as the 300 ms timeout, but loaded machines must not fail the test.
        const PROMPT: Duration = Duration::from_secs(2);

        /// A scheme that is neither `Unknown` nor what the fallback (GTK settings of the machine
        /// running the test) says, so the portal's answer is always a change.
        fn answer_differing_from_fallback() -> ColorScheme {
            if UnixTheme::query_color_scheme() == ColorScheme::Dark {
                ColorScheme::Light
            } else {
                ColorScheme::Dark
            }
        }

        fn record(theme: &UnixTheme) -> Arc<Mutex<Vec<ColorScheme>>> {
            let seen = Arc::new(Mutex::new(Vec::new()));
            let sink = Arc::clone(&seen);
            theme.theme_changed().connect(move |scheme| {
                sink.lock().unwrap_or_else(|e| e.into_inner()).push(*scheme)
            });
            seen
        }

        /// A theme whose start-up answer is delivered by a stand-in for `spawn_listener`'s
        /// thread: it marks itself a listener thread, applies each event it is sent, and after an
        /// `Initial` one signals the answer, in the order `spawn_listener` does.
        fn theme_with_listener() -> (
            Arc<UnixTheme>,
            Sender<PortalEvent>,
            std::thread::JoinHandle<()>,
        ) {
            let (events_tx, events_rx) = channel::<PortalEvent>();
            let (answer_tx, answer_rx) = channel::<()>();
            let theme = Arc::new(UnixTheme::without_portal());
            *theme.initial_answer.lock().unwrap() = Some(answer_rx);
            let inner = Arc::clone(&theme.inner);
            let listener = std::thread::spawn(move || {
                mark_listener_thread();
                for event in events_rx {
                    let is_initial = matches!(event, PortalEvent::Initial(_));
                    inner.apply(event);
                    if is_initial {
                        let _ = answer_tx.send(());
                    }
                }
            });
            (theme, events_tx, listener)
        }

        fn timed_read(theme: &UnixTheme) -> (ColorScheme, Duration) {
            let start = Instant::now();
            let scheme = theme.color_scheme();
            (scheme, start.elapsed())
        }

        #[test]
        fn an_answer_before_the_timeout_wakes_the_first_read_at_once() {
            let answer = answer_differing_from_fallback();
            let (theme, events, listener) = theme_with_listener();
            let sender = std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(30));
                events.send(PortalEvent::Initial(Some(answer))).unwrap();
                // Keep the listener (and its answer sender) alive: only the answer may wake it.
                std::thread::sleep(Duration::from_millis(600));
                drop(events);
            });
            let (scheme, took) = timed_read(&theme);
            assert_eq!(scheme, answer);
            assert!(
                took < PROMPT,
                "the read waited {took:?} for an answer already in"
            );
            sender.join().unwrap();
            listener.join().unwrap();
        }

        #[test]
        fn a_first_read_nobody_answers_times_out_once() {
            let (theme, events, listener) = theme_with_listener();
            let fallback = UnixTheme::query_color_scheme();
            let (scheme, took) = timed_read(&theme);
            assert_eq!(scheme, fallback);
            assert!(took >= Duration::from_millis(290), "gave up after {took:?}");
            assert!(took < Duration::from_secs(2), "waited {took:?}");
            // The wait happens once: later reads do not wait again.
            let (again, took) = timed_read(&theme);
            assert_eq!(again, fallback);
            assert!(took < PROMPT, "the second read waited {took:?}");
            drop(events);
            listener.join().unwrap();
        }

        #[test]
        fn a_theme_without_a_session_bus_does_not_wait() {
            // `spawn_listener`'s receiver disconnects without a message when there is no bus.
            let (answer_tx, answer_rx) = channel::<()>();
            drop(answer_tx);
            let theme = UnixTheme::without_portal();
            *theme.initial_answer.lock().unwrap() = Some(answer_rx);
            let (scheme, took) = timed_read(&theme);
            assert_eq!(scheme, UnixTheme::query_color_scheme());
            assert!(took < PROMPT, "waited {took:?} on a closed channel");
        }

        #[test]
        fn an_answer_after_the_timeout_is_emitted_as_a_change() {
            let answer = answer_differing_from_fallback();
            let (theme, events, listener) = theme_with_listener();
            let seen = record(&theme);
            let (first, _) = timed_read(&theme);
            assert_eq!(first, UnixTheme::query_color_scheme());
            assert!(seen.lock().unwrap().is_empty(), "nothing has changed yet");

            events.send(PortalEvent::Initial(Some(answer))).unwrap();
            drop(events);
            listener.join().unwrap();
            assert_eq!(*seen.lock().unwrap(), [answer]);
            let (now, took) = timed_read(&theme);
            assert_eq!(now, answer);
            assert!(took < PROMPT);
        }

        #[test]
        fn a_late_answer_without_a_preference_emits_nothing() {
            let (theme, events, listener) = theme_with_listener();
            let seen = record(&theme);
            let _ = timed_read(&theme);
            events.send(PortalEvent::Initial(None)).unwrap();
            drop(events);
            listener.join().unwrap();
            assert!(seen.lock().unwrap().is_empty());
        }

        #[test]
        fn an_answer_before_the_timeout_that_the_read_saw_is_not_emitted_again() {
            let answer = answer_differing_from_fallback();
            let (theme, events, listener) = theme_with_listener();
            let seen = record(&theme);
            events.send(PortalEvent::Initial(Some(answer))).unwrap();
            let (scheme, took) = timed_read(&theme);
            assert_eq!(scheme, answer);
            assert!(took < PROMPT);
            // The same value reported again, as the portal's `SettingChanged` may.
            events.send(PortalEvent::Changed(answer)).unwrap();
            events.send(PortalEvent::Changed(answer)).unwrap();
            drop(events);
            listener.join().unwrap();
            assert_eq!(*seen.lock().unwrap(), [answer], "one change, one emission");
        }

        #[test]
        fn a_slot_reading_the_theme_on_the_listener_thread_does_not_wait_for_its_own_answer() {
            // What `handle_theme_changed` does with no application: re-read the scheme in place,
            // on the thread that reported the change, before the answer has been signalled.
            let answer = answer_differing_from_fallback();
            let (theme, events, listener) = theme_with_listener();
            let reads: Arc<Mutex<Vec<(ColorScheme, Duration)>>> = Arc::default();
            let weak = Arc::downgrade(&theme);
            let log = Arc::clone(&reads);
            theme.theme_changed().connect(move |_| {
                if let Some(theme) = weak.upgrade() {
                    let read = timed_read(&theme);
                    log.lock().unwrap_or_else(|e| e.into_inner()).push(read);
                }
            });
            events.send(PortalEvent::Initial(Some(answer))).unwrap();
            // The listener stays alive (so the answer channel is not disconnected) while the read
            // is checked.
            let deadline = Instant::now() + Duration::from_secs(5);
            while reads.lock().unwrap().is_empty() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            let recorded = reads.lock().unwrap().clone();
            assert_eq!(recorded.len(), 1);
            let (scheme, took) = recorded[0];
            // A read that waited would have taken the one wait (and its receiver) for itself,
            // whatever the clock says.
            assert!(
                theme.initial_answer.lock().unwrap().is_some(),
                "the slot on the listener thread used up the wait"
            );
            assert_eq!(scheme, answer, "the slot already sees the new scheme");
            assert!(
                took < Duration::from_secs(2),
                "the listener stalled for {took:?}"
            );
            // The slot did not use up the one wait: the first outside read still gets the answer
            // at once.
            let (scheme, took) = timed_read(&theme);
            assert_eq!(scheme, answer);
            assert!(took < PROMPT, "the outside read waited {took:?}");
            drop(events);
            listener.join().unwrap();
        }

        #[test]
        fn other_threads_are_not_listener_threads() {
            assert!(!std::thread::spawn(on_listener_thread).join().unwrap());
            assert!(std::thread::spawn(|| {
                mark_listener_thread();
                on_listener_thread()
            })
            .join()
            .unwrap());
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
    // Before the connection below, so a notification never reads the previous theme.
    #[cfg(windows)]
    crate::dark_mode::set_theme(Arc::clone(&theme));
    {
        let mut bridge = BRIDGE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((previous, connection)) = bridge.take() {
            previous.theme_changed().disconnect(connection);
        }
        let connection = theme.theme_changed().connect(|_| {
            // Qt moves the window frames before it signals the change
            // (`QWindowsTheme::handleThemeChange`, qwindowstheme.cpp:690-705). The pass reads the
            // scheme itself, so a late delivery of an older notification cannot apply a stale one.
            crate::dark_mode::scheme_changed();
            ferrokit_gui::application::handle_theme_changed();
        });
        *bridge = Some((Arc::clone(&theme), connection));
    }
    // The native menus need their app mode before the first one opens, and a window created
    // afterwards takes its frame from the scheme of this theme.
    crate::dark_mode::scheme_changed();
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
