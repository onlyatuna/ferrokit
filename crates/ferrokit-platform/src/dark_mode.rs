// SPDX-License-Identifier: LGPL-3.0-only
//! Dark mode of the native Windows chrome: the window frames and the system's own menus.
//!
//! # What Qt does (`qtbase` tag `v6.11.2`, `src/plugins/platforms/windows`)
//!
//! * **Window frames.** `QWindowsWindow::setDarkBorderToWindow` (qwindowswindow.cpp:3857-3866)
//!   sets `DWMWA_USE_IMMERSIVE_DARK_MODE` (20) and, if that fails, the pre-20H1 value 19.
//!   It runs when a window is created while the scheme is dark (qwindowswindow.cpp:971-972), on
//!   `QEvent::ApplicationPaletteChange` (qwindowswindow.cpp:2912-2914) and, for every window, when
//!   the effective colour scheme changes (`QWindowsTheme::handleThemeChange`,
//!   qwindowstheme.cpp:681-705). `QWindowsWindow::setDarkBorder` (qwindowswindow.cpp:3868-3874)
//!   first asks `shouldApplyDarkFrame` (qwindowswindow.cpp:863-878): top-level, not
//!   `Qt::FramelessWindowHint`, not opted out with `darkmode=0`, and a palette whose text is
//!   lighter than its window.
//! * **Colour scheme.** `QWindowsTheme::effectiveColorScheme` (qwindowstheme.cpp:655-667): an
//!   application override (`requestColorScheme`), else the registry value, but `Light` when the
//!   style part of dark mode is switched off.
//! * **The `-platform windows:darkmode=N` option** (qwindowsintegration.cpp:178-186, default
//!   `DarkModeWindowFrames | DarkModeStyle`): `0` neither, `1` frames only, `2` frames and style
//!   (`QNativeInterface::Private::QWindowsApplication::DarkModeHandlingFlag`).
//! * **uxtheme: nothing.** Qt 6.11.2 never calls `SetPreferredAppMode`, `AllowDarkModeForWindow`,
//!   `FlushMenuThemes` or `SetWindowTheme(.., "DarkMode_Explorer", ..)` (a search of `src/` finds
//!   `SetWindowTheme` only in qwindowsvistastyle.cpp:274, for `L"explorer"`, and qwizard_win.cpp).
//!   Qt draws its menus, scroll bars and controls itself from a dark palette, so it never needs
//!   the system to theme them. Native `HMENU` pop-ups (`menus=native`, `QWindowsMenu`) get no dark
//!   treatment in Qt.
//!
//! # What this module adds
//!
//! The frames and the colour scheme follow Qt as above. ferrokit also opens real `HMENU` pop-ups
//! (`Win32Menu`, the tray icon's menu), which Windows themes per process through
//! undocumented `uxtheme.dll` exports. They are what a hand-written `enable_win32_dark_mode`
//! in an application used to reach, so the app-level mode is driven from the same scheme
//! ([`scheme_changed`], run for every change of the effective scheme, a
//! `StyleHints::set_color_scheme` override included):
//!
//! | ordinal | export | used |
//! |---|---|---|
//! | 135 | `SetPreferredAppMode(int)` from build 18362, `AllowDarkModeForApp(BOOL)` before | the app mode |
//! | 136 | `FlushMenuThemes()` | after a mode change, so open menu themes are rebuilt |
//! | 133 | `AllowDarkModeForWindow(HWND, BOOL)` | each top-level window, next to its frame attribute |
//!
//! The exports are looked up by ordinal at run time and only on builds from 17763 (1809), the
//! first with them; below that, or when the build number cannot be read, nothing is resolved or
//! called (the ordinals mean other functions on older builds). An export that is missing is
//! skipped, never an error. `SetWindowTheme(.., "DarkMode_Explorer", ..)` is not called: it only
//! matters for native child controls and ferrokit windows have none.
//!
//! # Other platforms
//!
//! macOS: nothing to do here. `QStyleHints::setColorScheme` reaches `NSApp.appearance`
//! ([`crate::theme::CocoaTheme`], as `QCocoaTheme::requestColorScheme`, qcocoatheme.mm:478-496) and
//! AppKit gives `NSMenu`s, the menu bar and status item menus that appearance unless they set their
//! own; neither Qt (`QCocoaMenu`) nor `cocoa_menu.rs` does. Linux: no native chrome to theme (the
//! portal scheme only feeds [`crate::theme::UnixTheme`]); these functions do nothing there.

use crate::theme::ColorScheme;
use std::sync::atomic::{AtomicU8, Ordering};

bitflags::bitflags! {
    /// `QNativeInterface::Private::QWindowsApplication::DarkModeHandlingFlag`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct DarkModeHandling: u8 {
        /// `DarkModeWindowFrames`: top-level frames follow a dark scheme.
        const WINDOW_FRAMES = 0x1;
        /// `DarkModeStyle`: the system's dark scheme is reported (otherwise it reads as `Light`
        /// and only an application override can make it dark).
        const STYLE = 0x2;
    }
}

impl Default for DarkModeHandling {
    /// Qt's default, `darkmode=2`.
    fn default() -> Self {
        Self::WINDOW_FRAMES | Self::STYLE
    }
}

impl DarkModeHandling {
    /// One `-platform windows:<param>` option (`parseOptions`, qwindowsintegration.cpp:178-186):
    /// `darkmode=0` clears both flags, `darkmode=1` sets the frames and clears the style,
    /// `darkmode=2` sets both. `None` for any other parameter.
    pub fn with_platform_option(self, param: &str) -> Option<Self> {
        match param {
            "darkmode=0" => Some(Self::empty()),
            "darkmode=1" => Some((self | Self::WINDOW_FRAMES) - Self::STYLE),
            "darkmode=2" => Some(self | Self::WINDOW_FRAMES | Self::STYLE),
            _ => None,
        }
    }
}

static HANDLING: AtomicU8 = AtomicU8::new(0x3);

/// `QWindowsApplication::darkModeHandling`.
pub fn dark_mode_handling() -> DarkModeHandling {
    DarkModeHandling::from_bits_truncate(HANDLING.load(Ordering::Relaxed))
}

/// `QWindowsApplication::setDarkModeHandling`. Qt sets it once, while the integration is built;
/// here a later call also re-reads the system scheme and re-applies it to every open window (on
/// Windows; the setting is stored and otherwise ignored on other platforms).
pub fn set_dark_mode_handling(handling: DarkModeHandling) {
    HANDLING.store(handling.bits(), Ordering::Relaxed);
    #[cfg(windows)]
    {
        let theme = crate::integration::platform().theme();
        theme.refresh();
        scheme_changed(theme.color_scheme());
    }
}

/// Whether a window's frame should be dark under `scheme`: `Some(dark)` to set it, `None` to
/// leave the frame alone because the application set it itself (`set_backdrop`).
/// `shouldApplyDarkFrame` (qwindowswindow.cpp:863-878) without the palette test: ferrokit has no
/// palette change notification to re-run it on, so the frame follows the scheme.
#[cfg(any(windows, test))]
fn frame_should_be_dark(
    handling: DarkModeHandling,
    scheme: ColorScheme,
    frameless: bool,
    set_by_application: bool,
) -> Option<bool> {
    if set_by_application {
        return None;
    }
    Some(
        scheme == ColorScheme::Dark
            && !frameless
            && handling.contains(DarkModeHandling::WINDOW_FRAMES),
    )
}

/// First build with the undocumented dark mode exports (Windows 10 1809).
#[cfg(any(windows, test))]
const FIRST_BUILD_WITH_DARK_MODE: u32 = 17763;
/// First build where ordinal 135 is `SetPreferredAppMode(int)` instead of
/// `AllowDarkModeForApp(BOOL)` (Windows 10 1903).
#[cfg(any(windows, test))]
const FIRST_BUILD_WITH_PREFERRED_APP_MODE: u32 = 18362;

/// `PreferredAppMode` of uxtheme.
#[cfg(any(windows, test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
enum PreferredAppMode {
    Default = 0,
    ForceDark = 2,
    ForceLight = 3,
}

/// The call that sets the app-level mode for a scheme on a given build.
#[cfg(any(windows, test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppModePlan {
    /// The build is unknown or too old for the exports.
    Unsupported,
    /// Ordinal 135 as `AllowDarkModeForApp(BOOL)` (builds 17763..18362).
    AllowDarkForApp(bool),
    /// Ordinal 135 as `SetPreferredAppMode(int)` (build 18362 and later).
    Preferred(PreferredAppMode),
}

#[cfg(any(windows, test))]
fn plan_app_mode(build: Option<u32>, scheme: ColorScheme) -> AppModePlan {
    match build {
        Some(b) if b >= FIRST_BUILD_WITH_PREFERRED_APP_MODE => {
            AppModePlan::Preferred(match scheme {
                ColorScheme::Dark => PreferredAppMode::ForceDark,
                ColorScheme::Light => PreferredAppMode::ForceLight,
                ColorScheme::Unknown => PreferredAppMode::Default,
            })
        }
        Some(b) if b >= FIRST_BUILD_WITH_DARK_MODE => {
            AppModePlan::AllowDarkForApp(scheme == ColorScheme::Dark)
        }
        _ => AppModePlan::Unsupported,
    }
}

/// The top-level windows that exist, with what decides their frame.
#[cfg(any(windows, test))]
#[derive(Default)]
struct WindowRegistry {
    windows: std::collections::HashMap<isize, WindowEntry>,
}

#[cfg(any(windows, test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WindowEntry {
    frameless: bool,
    /// `set_backdrop` set the frame attribute from its own `dark_mode`, which wins over the scheme.
    set_by_application: bool,
}

#[cfg(any(windows, test))]
impl WindowRegistry {
    fn insert(&mut self, hwnd: isize, frameless: bool) {
        self.windows.insert(
            hwnd,
            WindowEntry {
                frameless,
                set_by_application: false,
            },
        );
    }

    fn remove(&mut self, hwnd: isize) {
        self.windows.remove(&hwnd);
    }

    fn mark_set_by_application(&mut self, hwnd: isize) {
        if let Some(entry) = self.windows.get_mut(&hwnd) {
            entry.set_by_application = true;
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    fn entry(&self, hwnd: isize) -> Option<WindowEntry> {
        self.windows.get(&hwnd).copied()
    }

    fn snapshot(&self) -> Vec<(isize, WindowEntry)> {
        self.windows.iter().map(|(h, e)| (*h, *e)).collect()
    }
}

#[cfg(windows)]
pub(crate) use win::{
    set_dark_border_to_window, set_frame_by_application, window_created, window_destroyed,
};

/// The effective colour scheme changed (or was just installed): move the app-level mode of the
/// native menus and the frame of every window. Called by the platform theme bridge
/// ([`crate::theme::install_style_hints_theme`]) on whichever thread noticed the change. No
/// effect outside Windows.
pub(crate) fn scheme_changed(scheme: ColorScheme) {
    #[cfg(windows)]
    win::apply_scheme(scheme);
    #[cfg(not(windows))]
    let _ = scheme;
}

#[cfg(windows)]
mod win {
    use super::*;
    use std::sync::{LazyLock, Mutex};
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};
    use windows_sys::Win32::System::LibraryLoader::{
        GetModuleHandleW, GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32,
    };

    /// `DWMWA_USE_IMMERSIVE_DARK_MODE` before Windows 10 20H1 (build 18985).
    const DWMWA_USE_IMMERSIVE_DARK_MODE_BEFORE_20H1: u32 = 19;

    const ORDINAL_ALLOW_DARK_MODE_FOR_WINDOW: u16 = 133;
    const ORDINAL_APP_MODE: u16 = 135;
    const ORDINAL_FLUSH_MENU_THEMES: u16 = 136;

    type AllowDarkModeForWindow = unsafe extern "system" fn(HWND, i32) -> i32;
    type AllowDarkModeForApp = unsafe extern "system" fn(i32) -> i32;
    type SetPreferredAppMode = unsafe extern "system" fn(i32) -> i32;
    type FlushMenuThemes = unsafe extern "system" fn();

    /// The uxtheme exports, resolved by ordinal. A field is `None` when the build is too old
    /// (nothing is resolved then) or when the export is not there.
    pub(super) struct UxTheme {
        build: Option<u32>,
        allow_dark_mode_for_window: Option<AllowDarkModeForWindow>,
        allow_dark_mode_for_app: Option<AllowDarkModeForApp>,
        set_preferred_app_mode: Option<SetPreferredAppMode>,
        flush_menu_themes: Option<FlushMenuThemes>,
        /// The plan last applied, so a repeated scheme does not flush the menu themes again.
        applied: Mutex<Option<AppModePlan>>,
    }

    impl UxTheme {
        /// `resolve(ordinal)` returns the address of the export, `0` when there is none. It is
        /// not called at all when `build` is below [`FIRST_BUILD_WITH_DARK_MODE`] or unknown.
        pub(super) fn resolve(build: Option<u32>, mut resolve: impl FnMut(u16) -> usize) -> Self {
            let mut fns = Self {
                build,
                allow_dark_mode_for_window: None,
                allow_dark_mode_for_app: None,
                set_preferred_app_mode: None,
                flush_menu_themes: None,
                applied: Mutex::new(None),
            };
            if plan_app_mode(build, ColorScheme::Dark) == AppModePlan::Unsupported {
                return fns;
            }
            let mut lookup = |ordinal| match resolve(ordinal) {
                0 => None,
                addr => Some(addr),
            };
            // SAFETY: each address is the export the ordinal names on this build range (the
            // signatures in the table of the module documentation); the function pointer types
            // have the size of `usize`, and the build gate above is what makes the ordinals mean
            // these functions.
            unsafe {
                fns.allow_dark_mode_for_window = lookup(ORDINAL_ALLOW_DARK_MODE_FOR_WINDOW)
                    .map(|a| std::mem::transmute::<usize, AllowDarkModeForWindow>(a));
                let app_mode = lookup(ORDINAL_APP_MODE);
                if build.is_some_and(|b| b >= FIRST_BUILD_WITH_PREFERRED_APP_MODE) {
                    fns.set_preferred_app_mode =
                        app_mode.map(|a| std::mem::transmute::<usize, SetPreferredAppMode>(a));
                } else {
                    fns.allow_dark_mode_for_app =
                        app_mode.map(|a| std::mem::transmute::<usize, AllowDarkModeForApp>(a));
                }
                fns.flush_menu_themes = lookup(ORDINAL_FLUSH_MENU_THEMES)
                    .map(|a| std::mem::transmute::<usize, FlushMenuThemes>(a));
            }
            fns
        }

        /// Resolves the real exports of the system's `uxtheme.dll`.
        fn load() -> Self {
            let build = crate::theme::win32_theme::Win32Theme::build_number();
            Self::resolve(build, |ordinal| {
                let name: Vec<u16> = "uxtheme.dll"
                    .encode_utf16()
                    .chain(std::iter::once(0))
                    .collect();
                // SAFETY: `name` is NUL-terminated; a failed load yields null, handled below.
                // The module stays loaded for the life of the process.
                let module = unsafe {
                    let loaded = GetModuleHandleW(name.as_ptr());
                    if loaded.is_null() {
                        LoadLibraryExW(
                            name.as_ptr(),
                            std::ptr::null_mut(),
                            LOAD_LIBRARY_SEARCH_SYSTEM32,
                        )
                    } else {
                        loaded
                    }
                };
                if module.is_null() {
                    return 0;
                }
                // SAFETY: a `PCSTR` below 0x10000 is an ordinal, which `GetProcAddress` documents.
                unsafe { GetProcAddress(module, ordinal as usize as *const u8) }
                    .map_or(0, |f| f as usize)
            })
        }

        /// Sets the app-level mode for `scheme` and flushes the menu themes. Returns whether
        /// the mode call was made (not when unsupported, missing, or already in that state).
        pub(super) fn apply_app_scheme(&self, scheme: ColorScheme) -> bool {
            let plan = plan_app_mode(self.build, scheme);
            let mut applied = self.applied.lock().unwrap_or_else(|e| e.into_inner());
            if *applied == Some(plan) {
                return false;
            }
            // SAFETY: the pointers were resolved for this build by `resolve`.
            unsafe {
                match plan {
                    AppModePlan::Unsupported => return false,
                    AppModePlan::AllowDarkForApp(on) => match self.allow_dark_mode_for_app {
                        Some(f) => {
                            f(on as i32);
                        }
                        None => return false,
                    },
                    AppModePlan::Preferred(mode) => match self.set_preferred_app_mode {
                        Some(f) => {
                            f(mode as i32);
                        }
                        None => return false,
                    },
                }
                if let Some(flush) = self.flush_menu_themes {
                    flush();
                }
            }
            *applied = Some(plan);
            true
        }

        /// `AllowDarkModeForWindow`; whether it was called.
        pub(super) fn allow_window(&self, hwnd: HWND, allow: bool) -> bool {
            match self.allow_dark_mode_for_window {
                Some(f) if !hwnd.is_null() => {
                    // SAFETY: resolved for this build; `hwnd` is a window handle.
                    unsafe { f(hwnd, allow as i32) };
                    true
                }
                _ => false,
            }
        }
    }

    static UXTHEME: LazyLock<UxTheme> = LazyLock::new(UxTheme::load);
    static WINDOWS: Mutex<Option<WindowRegistry>> = Mutex::new(None);

    fn with_registry<R>(f: impl FnOnce(&mut WindowRegistry) -> R) -> R {
        let mut guard = WINDOWS.lock().unwrap_or_else(|e| e.into_inner());
        f(guard.get_or_insert_with(WindowRegistry::default))
    }

    /// `QWindowsWindow::setDarkBorderToWindow` (qwindowswindow.cpp:3857-3866): attribute 20, and
    /// 19 when the build does not know it. Returns whether either was accepted.
    pub(crate) fn set_dark_border_to_window(hwnd: HWND, dark: bool) -> bool {
        if hwnd.is_null() {
            return false;
        }
        let value: i32 = dark as i32;
        let size = std::mem::size_of::<i32>() as u32;
        // SAFETY: `value` outlives the calls and `size` is its size.
        unsafe {
            [
                DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
                DWMWA_USE_IMMERSIVE_DARK_MODE_BEFORE_20H1,
            ]
            .into_iter()
            .any(|attribute| {
                DwmSetWindowAttribute(hwnd, attribute, &value as *const i32 as *const _, size) >= 0
            })
        }
    }

    fn apply_to_window(hwnd: isize, entry: WindowEntry, scheme: ColorScheme) {
        let Some(dark) = frame_should_be_dark(
            super::dark_mode_handling(),
            scheme,
            entry.frameless,
            entry.set_by_application,
        ) else {
            return;
        };
        UXTHEME.allow_window(hwnd as HWND, dark);
        set_dark_border_to_window(hwnd as HWND, dark);
    }

    /// A top-level window exists: a dark scheme darkens its frame from the start
    /// (qwindowswindow.cpp:971-972). `frameless` windows draw no frame of their own to darken.
    pub(crate) fn window_created(hwnd: HWND, frameless: bool) {
        let key = hwnd as isize;
        with_registry(|r| r.insert(key, frameless));
        let scheme = crate::integration::platform().theme().color_scheme();
        if scheme == ColorScheme::Dark {
            apply_to_window(
                key,
                WindowEntry {
                    frameless,
                    set_by_application: false,
                },
                scheme,
            );
        }
    }

    pub(crate) fn window_destroyed(hwnd: HWND) {
        with_registry(|r| r.remove(hwnd as isize));
    }

    /// `set_backdrop` chose the frame attribute from its own `dark_mode`; the scheme leaves it.
    pub(crate) fn set_frame_by_application(hwnd: HWND) {
        with_registry(|r| r.mark_set_by_application(hwnd as isize));
    }

    pub(super) fn apply_scheme(scheme: ColorScheme) {
        UXTHEME.apply_app_scheme(scheme);
        // The lock is not held across the DWM calls.
        for (hwnd, entry) in with_registry(|r| r.snapshot()) {
            apply_to_window(hwnd, entry, scheme);
        }
    }

    #[cfg(test)]
    pub(super) fn registered(hwnd: HWND) -> Option<WindowEntry> {
        with_registry(|r| r.entry(hwnd as isize))
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};

        #[test]
        fn nothing_is_resolved_below_the_first_supported_build() {
            for build in [None, Some(0), Some(10240), Some(17762)] {
                let asked = AtomicUsize::new(0);
                let fns = UxTheme::resolve(build, |_| {
                    asked.fetch_add(1, Ordering::SeqCst);
                    0x1000
                });
                assert_eq!(asked.load(Ordering::SeqCst), 0, "{build:?}");
                assert!(!fns.apply_app_scheme(ColorScheme::Dark), "{build:?}");
                assert!(!fns.allow_window(0x10 as HWND, true), "{build:?}");
            }
        }

        #[test]
        fn the_three_ordinals_are_asked_for_on_a_supported_build() {
            for build in [17763, 18361, 18362, 19045, 26100] {
                let mut asked = Vec::new();
                UxTheme::resolve(Some(build), |ordinal| {
                    asked.push(ordinal);
                    0
                });
                asked.sort_unstable();
                assert_eq!(asked, [133, 135, 136], "build {build}");
            }
        }

        #[test]
        fn absent_exports_are_tolerated() {
            let fns = UxTheme::resolve(Some(22631), |_| 0);
            assert!(!fns.apply_app_scheme(ColorScheme::Dark));
            assert!(!fns.apply_app_scheme(ColorScheme::Light));
            assert!(!fns.allow_window(0x10 as HWND, true));
            // A failed attempt is not remembered as applied.
            assert_eq!(*fns.applied.lock().unwrap(), None);
        }

        // Each test owns its recording statics: tests run in parallel.
        static MODE_CALLS: AtomicUsize = AtomicUsize::new(0);
        static MODE_LAST: AtomicI32 = AtomicI32::new(i32::MIN);
        unsafe extern "system" fn mode_stub(mode: i32) -> i32 {
            MODE_CALLS.fetch_add(1, Ordering::SeqCst);
            MODE_LAST.store(mode, Ordering::SeqCst);
            1
        }
        static ALLOW_CALLS: AtomicUsize = AtomicUsize::new(0);
        static ALLOW_LAST: AtomicI32 = AtomicI32::new(i32::MIN);
        unsafe extern "system" fn allow_stub(_hwnd: HWND, allow: i32) -> i32 {
            ALLOW_CALLS.fetch_add(1, Ordering::SeqCst);
            ALLOW_LAST.store(allow, Ordering::SeqCst);
            1
        }
        static FLUSH_CALLS: AtomicUsize = AtomicUsize::new(0);
        unsafe extern "system" fn flush_stub() {
            FLUSH_CALLS.fetch_add(1, Ordering::SeqCst);
        }

        #[test]
        fn preferred_app_mode_follows_the_scheme_and_flushes_once_per_change() {
            let fns = UxTheme::resolve(Some(22631), |ordinal| match ordinal {
                133 => allow_stub as *const () as usize,
                135 => mode_stub as *const () as usize,
                136 => flush_stub as *const () as usize,
                _ => 0,
            });
            assert!(fns.apply_app_scheme(ColorScheme::Dark));
            assert_eq!(MODE_LAST.load(Ordering::SeqCst), 2); // ForceDark
            assert_eq!(FLUSH_CALLS.load(Ordering::SeqCst), 1);
            // The same scheme again changes nothing.
            assert!(!fns.apply_app_scheme(ColorScheme::Dark));
            assert_eq!(MODE_CALLS.load(Ordering::SeqCst), 1);
            assert_eq!(FLUSH_CALLS.load(Ordering::SeqCst), 1);
            assert!(fns.apply_app_scheme(ColorScheme::Light));
            assert_eq!(MODE_LAST.load(Ordering::SeqCst), 3); // ForceLight
            assert!(fns.apply_app_scheme(ColorScheme::Unknown));
            assert_eq!(MODE_LAST.load(Ordering::SeqCst), 0); // Default
            assert_eq!(MODE_CALLS.load(Ordering::SeqCst), 3);
            assert_eq!(FLUSH_CALLS.load(Ordering::SeqCst), 3);

            assert!(fns.allow_window(0x10 as HWND, true));
            assert_eq!(ALLOW_LAST.load(Ordering::SeqCst), 1);
            assert!(!fns.allow_window(std::ptr::null_mut(), true));
            assert_eq!(ALLOW_CALLS.load(Ordering::SeqCst), 1);
        }

        static APP_CALLS: AtomicUsize = AtomicUsize::new(0);
        static APP_LAST: AtomicI32 = AtomicI32::new(i32::MIN);
        unsafe extern "system" fn app_stub(on: i32) -> i32 {
            APP_CALLS.fetch_add(1, Ordering::SeqCst);
            APP_LAST.store(on, Ordering::SeqCst);
            1
        }

        #[test]
        fn the_1809_build_uses_allow_dark_mode_for_app_and_a_missing_flush_is_fine() {
            let fns = UxTheme::resolve(Some(17763), |ordinal| match ordinal {
                135 => app_stub as *const () as usize,
                _ => 0,
            });
            assert!(fns.set_preferred_app_mode.is_none());
            assert!(fns.apply_app_scheme(ColorScheme::Dark));
            assert_eq!(APP_LAST.load(Ordering::SeqCst), 1);
            assert!(fns.apply_app_scheme(ColorScheme::Light));
            assert_eq!(APP_LAST.load(Ordering::SeqCst), 0);
            assert_eq!(APP_CALLS.load(Ordering::SeqCst), 2);
        }

        #[test]
        fn the_real_exports_resolve_on_this_machine_without_being_called() {
            let build = crate::theme::win32_theme::Win32Theme::build_number();
            let fns = UxTheme::load();
            assert_eq!(fns.build, build);
            if build.is_some_and(|b| b >= FIRST_BUILD_WITH_PREFERRED_APP_MODE) {
                assert!(fns.allow_dark_mode_for_window.is_some());
                assert!(fns.set_preferred_app_mode.is_some());
                assert!(fns.flush_menu_themes.is_some());
            }
        }

        #[test]
        fn a_null_window_gets_no_frame_attribute() {
            assert!(!set_dark_border_to_window(std::ptr::null_mut(), true));
        }

        #[test]
        fn a_window_is_tracked_from_creation_to_destruction() {
            // A made-up handle: the DWM calls on it fail, which is tolerated.
            let hwnd = 0x7fff_f00d as HWND;
            with_registry(|r| r.insert(hwnd as isize, false));
            assert_eq!(
                registered(hwnd),
                Some(WindowEntry {
                    frameless: false,
                    set_by_application: false
                })
            );
            set_frame_by_application(hwnd);
            assert!(registered(hwnd).unwrap().set_by_application);
            window_destroyed(hwnd);
            assert_eq!(registered(hwnd), None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_qts_darkmode_2() {
        assert_eq!(
            DarkModeHandling::default(),
            DarkModeHandling::WINDOW_FRAMES | DarkModeHandling::STYLE
        );
    }

    #[test]
    fn platform_options_set_the_flags_like_qt() {
        let all = DarkModeHandling::default();
        assert_eq!(
            all.with_platform_option("darkmode=0"),
            Some(DarkModeHandling::empty())
        );
        assert_eq!(
            all.with_platform_option("darkmode=1"),
            Some(DarkModeHandling::WINDOW_FRAMES)
        );
        assert_eq!(all.with_platform_option("darkmode=2"), Some(all));
        // `setFlag` leaves the other flag alone, so options accumulate.
        assert_eq!(
            DarkModeHandling::empty().with_platform_option("darkmode=2"),
            Some(all)
        );
        assert_eq!(
            DarkModeHandling::STYLE.with_platform_option("darkmode=1"),
            Some(DarkModeHandling::WINDOW_FRAMES)
        );
        assert_eq!(all.with_platform_option("darkmode=3"), None);
        assert_eq!(all.with_platform_option("fontengine=gdi"), None);
    }

    #[test]
    fn app_mode_is_gated_by_the_build_number() {
        for scheme in [ColorScheme::Dark, ColorScheme::Light, ColorScheme::Unknown] {
            assert_eq!(plan_app_mode(None, scheme), AppModePlan::Unsupported);
            assert_eq!(plan_app_mode(Some(10240), scheme), AppModePlan::Unsupported);
            assert_eq!(plan_app_mode(Some(17762), scheme), AppModePlan::Unsupported);
        }
        // 1809: only AllowDarkModeForApp(bool) exists.
        assert_eq!(
            plan_app_mode(Some(17763), ColorScheme::Dark),
            AppModePlan::AllowDarkForApp(true)
        );
        assert_eq!(
            plan_app_mode(Some(18361), ColorScheme::Light),
            AppModePlan::AllowDarkForApp(false)
        );
        assert_eq!(
            plan_app_mode(Some(18361), ColorScheme::Unknown),
            AppModePlan::AllowDarkForApp(false)
        );
        // 1903 and later: SetPreferredAppMode.
        for build in [18362, 19045, 22000, 26100] {
            assert_eq!(
                plan_app_mode(Some(build), ColorScheme::Dark),
                AppModePlan::Preferred(PreferredAppMode::ForceDark)
            );
            assert_eq!(
                plan_app_mode(Some(build), ColorScheme::Light),
                AppModePlan::Preferred(PreferredAppMode::ForceLight)
            );
            assert_eq!(
                plan_app_mode(Some(build), ColorScheme::Unknown),
                AppModePlan::Preferred(PreferredAppMode::Default)
            );
        }
    }

    #[test]
    fn frames_follow_the_scheme_unless_opted_out_frameless_or_set_by_the_application() {
        let all = DarkModeHandling::default();
        assert_eq!(
            frame_should_be_dark(all, ColorScheme::Dark, false, false),
            Some(true)
        );
        assert_eq!(
            frame_should_be_dark(all, ColorScheme::Light, false, false),
            Some(false)
        );
        assert_eq!(
            frame_should_be_dark(all, ColorScheme::Unknown, false, false),
            Some(false)
        );
        // `Qt::FramelessWindowHint`.
        assert_eq!(
            frame_should_be_dark(all, ColorScheme::Dark, true, false),
            Some(false)
        );
        // `darkmode=0` and `darkmode=1` differ in the style only; frames need `WINDOW_FRAMES`.
        assert_eq!(
            frame_should_be_dark(DarkModeHandling::empty(), ColorScheme::Dark, false, false),
            Some(false)
        );
        assert_eq!(
            frame_should_be_dark(DarkModeHandling::STYLE, ColorScheme::Dark, false, false),
            Some(false)
        );
        assert_eq!(
            frame_should_be_dark(
                DarkModeHandling::WINDOW_FRAMES,
                ColorScheme::Dark,
                false,
                false
            ),
            Some(true)
        );
        // The application's own `set_backdrop` dark flag is left alone.
        assert_eq!(
            frame_should_be_dark(all, ColorScheme::Dark, false, true),
            None
        );
    }

    #[test]
    fn the_registry_tracks_windows() {
        let mut r = WindowRegistry::default();
        r.insert(1, false);
        r.insert(2, true);
        r.mark_set_by_application(1);
        r.mark_set_by_application(99); // unknown window: ignored
        assert_eq!(
            r.entry(1),
            Some(WindowEntry {
                frameless: false,
                set_by_application: true
            })
        );
        assert_eq!(
            r.entry(2),
            Some(WindowEntry {
                frameless: true,
                set_by_application: false
            })
        );
        assert_eq!(r.snapshot().len(), 2);
        r.remove(1);
        assert_eq!(r.entry(1), None);
        // A new window with a reused handle starts clean.
        r.insert(1, false);
        assert!(!r.entry(1).unwrap().set_by_application);
    }
}
