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

#[cfg(any(windows, test))]
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
        crate::integration::platform().theme().refresh();
        scheme_changed();
    }
}

/// Whether a window's frame should be dark under `scheme`: `Some(dark)` to set it, `None` to
/// leave the frame alone: the window is frameless (`shouldApplyDarkFrame`, qwindowswindow.cpp:
/// 863-878, skips those, so nothing at all is written for them, neither a light nor a dark
/// frame) or the application set the frame itself (`set_window_backdrop`). Otherwise the frame
/// follows the scheme (the palette test of `shouldApplyDarkFrame` is left out: ferrokit has no
/// palette change notification to re-run it on).
#[cfg(any(windows, test))]
fn frame_should_be_dark(
    handling: DarkModeHandling,
    scheme: ColorScheme,
    frameless: bool,
    set_by_application: bool,
) -> Option<bool> {
    if frameless || set_by_application {
        return None;
    }
    Some(scheme == ColorScheme::Dark && handling.contains(DarkModeHandling::WINDOW_FRAMES))
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

/// Runs a "pass" so that the last request wins and no pass ever runs inside another one.
///
/// A pass reads the *current* scheme and the current windows, so a request that arrives while a
/// pass runs (another thread, or the same thread re-entered from a message that a uxtheme call
/// sent to one of our windows) only marks the running pass dirty and returns; the running pass
/// then repeats, reading the state again. A stale scheme can therefore never be the last one
/// applied, and no lock is held while a pass runs.
#[cfg(any(windows, test))]
#[derive(Default)]
struct Coalescer {
    /// `(running, dirty)`.
    state: std::sync::Mutex<(bool, bool)>,
}

#[cfg(any(windows, test))]
impl Coalescer {
    /// Runs `pass` now, or arranges for the pass that is running to run once more.
    fn request(&self, mut pass: impl FnMut()) {
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.0 {
                state.1 = true;
                return;
            }
            *state = (true, false);
        }
        // A panicking pass must not leave the coalescer "running" for good.
        struct Reset<'a>(&'a Coalescer);
        impl Drop for Reset<'_> {
            fn drop(&mut self) {
                let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
                *state = (false, false);
            }
        }
        let reset = Reset(self);
        loop {
            pass();
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.1 {
                state.1 = false;
                continue;
            }
            state.0 = false;
            drop(state);
            std::mem::forget(reset);
            return;
        }
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
    /// Windows that draw no frame the scheme could darken. The scheme never writes anything for
    /// them (neither the app-level window mode nor the frame attribute): `shouldApplyDarkFrame`
    /// skips `Qt::FramelessWindowHint` windows. This is wider than Qt's test: ferrokit's
    /// `CUSTOM_FRAMELESS`, `LAYERED` and `TOOLTIP` windows count too, because their caption is
    /// drawn by the application or absent.
    frameless: bool,
    /// `set_window_backdrop` wrote the frame attribute from its own `dark_mode` (this value),
    /// which wins over the scheme until a `BackdropType::None` hands the frame back.
    app_dark: Option<bool>,
    /// The attribute last written successfully, `None` when unknown (then it is written again).
    applied_dark: Option<bool>,
}

#[cfg(any(windows, test))]
impl WindowRegistry {
    /// A new window has DWM's default, a light frame.
    fn insert(&mut self, hwnd: isize, frameless: bool) {
        self.windows.insert(
            hwnd,
            WindowEntry {
                frameless,
                app_dark: None,
                applied_dark: Some(false),
            },
        );
    }

    fn remove(&mut self, hwnd: isize) {
        self.windows.remove(&hwnd);
    }

    /// The application writes `dark` as the frame of `hwnd`; the scheme leaves it from now on.
    /// Whether `hwnd` is tracked.
    fn mark_set_by_application(&mut self, hwnd: isize, dark: bool) -> bool {
        match self.windows.get_mut(&hwnd) {
            Some(entry) => {
                entry.app_dark = Some(dark);
                entry.applied_dark = None;
                true
            }
            None => false,
        }
    }

    /// Gives the frame back to the scheme. Whether the scheme now owns the frame: `false` for an
    /// untracked window and for a frameless one, which the scheme never writes (the caller keeps
    /// writing its own value there).
    fn release_to_scheme(&mut self, hwnd: isize) -> bool {
        match self.windows.get_mut(&hwnd) {
            Some(entry) if !entry.frameless => {
                entry.app_dark = None;
                entry.applied_dark = None;
                true
            }
            _ => false,
        }
    }

    fn record_applied(&mut self, hwnd: isize, dark: bool) {
        if let Some(entry) = self.windows.get_mut(&hwnd) {
            entry.applied_dark = Some(dark);
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

/// One pass over the windows: writes the frame of every window whose wanted value differs from
/// what was last written. `set_frame(hwnd, dark)` reports whether the write was accepted; a
/// refused one is not recorded, so the next pass tries again. The registry lock is not held
/// while `set_frame` runs.
#[cfg(any(windows, test))]
fn apply_frames(
    registry: &std::sync::Mutex<Option<WindowRegistry>>,
    handling: DarkModeHandling,
    scheme: ColorScheme,
    mut set_frame: impl FnMut(isize, bool) -> bool,
) {
    let snapshot = registry
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(WindowRegistry::default)
        .snapshot();
    for (hwnd, entry) in snapshot {
        let Some(dark) =
            frame_should_be_dark(handling, scheme, entry.frameless, entry.app_dark.is_some())
        else {
            continue;
        };
        if entry.applied_dark == Some(dark) {
            continue;
        }
        if !set_frame(hwnd, dark) {
            continue;
        }
        // `set_window_backdrop` may have taken the frame over (marked it, then written its own
        // value) between the snapshot and the write above, and the write above may have landed
        // after the application's. No lock can be held across a write, so the application's value
        // is written again until the value last written is the one the application wants: the
        // application only ever writes `app_dark`, so whichever write comes last, it wins.
        let mut last = dark;
        loop {
            let mut guard = registry.lock().unwrap_or_else(|e| e.into_inner());
            let Some(r) = guard.as_mut() else { break };
            let Some(now) = r.entry(hwnd) else { break }; // destroyed meanwhile
            match now.app_dark {
                Some(want) if want == last => break,
                Some(want) => {
                    drop(guard);
                    set_frame(hwnd, want);
                    last = want;
                }
                None => {
                    // Still the window of the snapshot: remember what was written. A window that
                    // was handed back to the scheme meanwhile has its own pass coming.
                    if now == entry {
                        r.record_applied(hwnd, dark);
                    }
                    break;
                }
            }
        }
    }
}

#[cfg(windows)]
pub(crate) use win::{
    release_frame_to_scheme, set_dark_border_to_window, set_frame_by_application, set_theme,
    window_created, window_destroyed,
};

/// The effective colour scheme changed (or was just installed): move the app-level mode of the
/// native menus and the frame of every window to the scheme the theme reports *now*. Called by
/// the platform theme bridge ([`crate::theme::install_style_hints_theme`]) on whichever thread
/// noticed the change, and safe to call re-entrantly from a message handler. No effect outside
/// Windows.
pub(crate) fn scheme_changed() {
    #[cfg(windows)]
    win::apply_scheme();
}

#[cfg(windows)]
mod win {
    use super::*;
    use std::sync::{Arc, LazyLock, Mutex};
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
        /// The plan committed last, so a repeated scheme does not flush the menu themes again.
        /// Never held while an export runs: `FlushMenuThemes` sends messages to our windows, and
        /// their handlers may come back here.
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
            // SAFETY: `transmute::<usize, fn>` is sound only if each address is an export with the
            // signature of the target type. `resolve`'s contract (it is `pub(super)`; its callers are
            // `load`, which returns `GetProcAddress` results, and the tests, which return `extern
            // "system"` stubs of the matching signature) is that `resolver(ordinal)` is the export
            // that ordinal names, with the signatures in the module documentation table (133 =
            // `AllowDarkModeForWindow(HWND, BOOL)`, 135 = `SetPreferredAppMode(int)` from
            // `FIRST_BUILD_WITH_PREFERRED_APP_MODE` else `AllowDarkModeForApp(BOOL)`, 136 =
            // `FlushMenuThemes()`); the early return above keeps builds before 17763 (where the
            // ordinals mean other functions) from reaching here, and the branch on `build` picks the
            // 135 signature. A function pointer is `usize`-sized, and `lookup` maps 0 to `None`, so
            // no null address is transmuted. [inferred: the ordinal-to-signature mapping is from
            // undocumented exports and cannot be verified here.]
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
                // SAFETY: `name` is a live NUL-terminated UTF-16 vector for both calls. A null result
                // from `GetModuleHandleW` or `LoadLibraryExW` is handled right below (`return 0`), so
                // the handle is never used unchecked. `GetModuleHandleW` takes no reference, and the
                // `LoadLibraryExW` handle is never freed, so the module stays loaded for the process
                // (the module is therefore valid for every later `GetProcAddress`). `hFile` is null
                // as the API requires for the `LOAD_LIBRARY_SEARCH_SYSTEM32` flag.
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
                // SAFETY: `module` is the non-null, never-unloaded `uxtheme.dll` handle checked
                // above; `ordinal as usize` is at most 136 (< 0x10000), which `GetProcAddress`
                // documents as an ordinal rather than a pointer to a NUL-terminated name, so nothing
                // is dereferenced.
                unsafe { GetProcAddress(module, ordinal as usize as *const u8) }
                    .map_or(0, |f| f as usize)
            })
        }

        /// Sets the app-level mode for `scheme` and flushes the menu themes. Returns whether
        /// the mode call was made (not when unsupported, when the export is missing, or when the
        /// mode is already that one).
        ///
        /// The plan is compared and committed under the lock; the exports run after it is
        /// released. A plan that cannot be carried out (no export) is not committed, so it is
        /// not mistaken for done.
        pub(super) fn apply_app_scheme(&self, scheme: ColorScheme) -> bool {
            let plan = plan_app_mode(self.build, scheme);
            let call: Box<dyn Fn()> = match plan {
                AppModePlan::Unsupported => return false,
                AppModePlan::AllowDarkForApp(on) => match self.allow_dark_mode_for_app {
                    // SAFETY: `f` is `allow_dark_mode_for_app`, set by `resolve` only for builds
                    // before `FIRST_BUILD_WITH_PREFERRED_APP_MODE` (the plan above is
                    // `AllowDarkForApp` only on those builds) from ordinal 135, whose signature is
                    // `AllowDarkModeForApp(BOOL)`; it takes a plain integer, no pointers.
                    Some(f) => Box::new(move || unsafe {
                        f(on as i32);
                    }),
                    None => return false,
                },
                AppModePlan::Preferred(mode) => match self.set_preferred_app_mode {
                    // SAFETY: `f` is `set_preferred_app_mode`, set by `resolve` only for builds from
                    // `FIRST_BUILD_WITH_PREFERRED_APP_MODE` (the plan above is `Preferred` only on
                    // those builds) from ordinal 135, whose signature is `SetPreferredAppMode(int)`;
                    // it takes a plain integer, no pointers.
                    Some(f) => Box::new(move || unsafe {
                        f(mode as i32);
                    }),
                    None => return false,
                },
            };
            {
                let mut applied = self.applied.lock().unwrap_or_else(|e| e.into_inner());
                if *applied == Some(plan) {
                    return false;
                }
                *applied = Some(plan);
            }
            call();
            if let Some(flush) = self.flush_menu_themes {
                // SAFETY: `flush` is `flush_menu_themes`, set by `resolve` from ordinal 136
                // (`FlushMenuThemes()`, no arguments); no lock is held across the call, so the
                // window messages it sends cannot deadlock on `applied`.
                unsafe { flush() };
            }
            true
        }

        /// `AllowDarkModeForWindow`; whether it was called.
        pub(super) fn allow_window(&self, hwnd: HWND, allow: bool) -> bool {
            match self.allow_dark_mode_for_window {
                Some(f) if !hwnd.is_null() => {
                    // SAFETY: `f` is `allow_dark_mode_for_window`, set by `resolve` from ordinal 133
                    // (`AllowDarkModeForWindow(HWND, BOOL)`); `hwnd` is non-null (checked in the
                    // match guard) and comes from the window registry, and uxtheme validates the
                    // handle itself [inferred: undocumented export, so this is not checked against
                    // any documentation].
                    unsafe { f(hwnd, allow as i32) };
                    true
                }
                _ => false,
            }
        }
    }

    static UXTHEME: LazyLock<UxTheme> = LazyLock::new(UxTheme::load);
    static WINDOWS: Mutex<Option<WindowRegistry>> = Mutex::new(None);
    static PASSES: LazyLock<Coalescer> = LazyLock::new(Coalescer::default);
    /// The theme whose scheme is the effective one (`install_style_hints_theme`).
    static THEME: Mutex<Option<Arc<dyn crate::theme::PlatformTheme>>> = Mutex::new(None);

    fn with_registry<R>(f: impl FnOnce(&mut WindowRegistry) -> R) -> R {
        let mut guard = WINDOWS.lock().unwrap_or_else(|e| e.into_inner());
        f(guard.get_or_insert_with(WindowRegistry::default))
    }

    /// Remembers the theme whose scheme the windows and menus follow.
    pub(crate) fn set_theme(theme: Arc<dyn crate::theme::PlatformTheme>) {
        *THEME.lock().unwrap_or_else(|e| e.into_inner()) = Some(theme);
    }

    /// The scheme the theme reports now, `Unknown` before there is a theme.
    fn current_scheme() -> ColorScheme {
        let theme = THEME.lock().unwrap_or_else(|e| e.into_inner()).clone();
        theme.map_or(ColorScheme::Unknown, |t| t.color_scheme())
    }

    /// `QWindowsWindow::setDarkBorderToWindow` (qwindowswindow.cpp:3857-3866): attribute 20, and
    /// 19 when the build does not know it. Returns whether either was accepted.
    pub(crate) fn set_dark_border_to_window(hwnd: HWND, dark: bool) -> bool {
        if hwnd.is_null() {
            return false;
        }
        let value: i32 = dark as i32;
        let size = std::mem::size_of::<i32>() as u32;
        // SAFETY: `hwnd` is non-null (checked above) and `DwmSetWindowAttribute` validates it,
        // failing with an `HRESULT` for a stale handle; `pvAttribute` points to `value`, a live
        // `i32` (a `BOOL`, which both dark-mode attributes take per the DWM documentation), with
        // `size` its byte size 4, and it is only read; `value` outlives every call in the iterator.
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

    fn set_frame(hwnd: isize, dark: bool) -> bool {
        UXTHEME.allow_window(hwnd as HWND, dark);
        set_dark_border_to_window(hwnd as HWND, dark)
    }

    /// One pass: the app mode and every frame, for the scheme the theme reports at this moment.
    fn pass() {
        let scheme = current_scheme();
        UXTHEME.apply_app_scheme(scheme);
        apply_frames(&WINDOWS, super::dark_mode_handling(), scheme, set_frame);
    }

    pub(super) fn apply_scheme() {
        PASSES.request(pass);
    }

    /// A top-level window exists. It is registered, and then a pass runs (or the running one is
    /// asked to repeat), so that a scheme change racing with the creation cannot miss it: the
    /// pass reads the scheme after the window is in the registry. A dark scheme darkens the
    /// frame from the start (qwindowswindow.cpp:971-972); under a light one the pass finds a
    /// new window already light and writes nothing.
    ///
    /// `frameless` windows draw no frame the scheme could darken. That is wider than Qt's
    /// `Qt::FramelessWindowHint` test: ferrokit's `CUSTOM_FRAMELESS`, `LAYERED` and `TOOLTIP`
    /// windows are counted too (their caption is drawn by the application, or absent).
    pub(crate) fn window_created(hwnd: HWND, frameless: bool) {
        with_registry(|r| r.insert(hwnd as isize, frameless));
        // The theme may not be installed yet when the first window is made.
        let _ = crate::integration::platform();
        apply_scheme();
    }

    pub(crate) fn window_destroyed(hwnd: HWND) {
        with_registry(|r| r.remove(hwnd as isize));
    }

    /// `set_window_backdrop` writes `dark` as the frame attribute of `hwnd`; the scheme leaves
    /// the frame alone until [`release_frame_to_scheme`], and corrects a write of its own that
    /// raced with this one (see `apply_frames`). Call it *before* writing. Whether the window is
    /// tracked.
    pub(crate) fn set_frame_by_application(hwnd: HWND, dark: bool) -> bool {
        with_registry(|r| r.mark_set_by_application(hwnd as isize, dark))
    }

    /// Hands the frame back to the scheme and applies it now. Returns whether the scheme now
    /// owns the frame. `false` means the caller keeps writing the frame itself: the window is
    /// untracked, or frameless (the scheme never writes those, as Qt's `shouldApplyDarkFrame`
    /// skips frameless windows).
    pub(crate) fn release_frame_to_scheme(hwnd: HWND) -> bool {
        let owned = with_registry(|r| r.release_to_scheme(hwnd as isize));
        if owned {
            apply_scheme();
        }
        owned
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
        fn absent_exports_are_tolerated_and_not_remembered_as_applied() {
            let fns = UxTheme::resolve(Some(22631), |_| 0);
            assert!(!fns.apply_app_scheme(ColorScheme::Dark));
            assert!(!fns.apply_app_scheme(ColorScheme::Light));
            assert!(!fns.allow_window(0x10 as HWND, true));
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

        // The flush stub of this test comes back into `apply_app_scheme` on the same thread, as a
        // window procedure would when `FlushMenuThemes` sends it a message. With the lock held
        // across the export this deadlocks.
        static REENTRANT: std::sync::OnceLock<&'static UxTheme> = std::sync::OnceLock::new();
        static REENTRANT_FLUSHES: AtomicUsize = AtomicUsize::new(0);
        static REENTRANT_RESULT: AtomicI32 = AtomicI32::new(-1);
        unsafe extern "system" fn reentrant_mode_stub(_mode: i32) -> i32 {
            1
        }
        unsafe extern "system" fn reentrant_flush_stub() {
            if REENTRANT_FLUSHES.fetch_add(1, Ordering::SeqCst) == 0 {
                // The same plan is already committed: nothing to do, and no deadlock.
                let again = REENTRANT.get().unwrap().apply_app_scheme(ColorScheme::Dark);
                REENTRANT_RESULT.store(again as i32, Ordering::SeqCst);
            }
        }

        #[test]
        fn an_export_may_re_enter_apply_app_scheme_without_deadlocking() {
            let fns: &'static UxTheme = Box::leak(Box::new(UxTheme::resolve(
                Some(22631),
                |ordinal| match ordinal {
                    135 => reentrant_mode_stub as *const () as usize,
                    136 => reentrant_flush_stub as *const () as usize,
                    _ => 0,
                },
            )));
            REENTRANT.set(fns).ok().unwrap();
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                tx.send(fns.apply_app_scheme(ColorScheme::Dark)).ok();
            });
            // A deadlock would never answer; the timeout only bounds a failing run.
            let done = rx
                .recv_timeout(std::time::Duration::from_secs(20))
                .expect("apply_app_scheme deadlocked");
            assert!(done);
            assert_eq!(REENTRANT_FLUSHES.load(Ordering::SeqCst), 1);
            assert_eq!(
                REENTRANT_RESULT.load(Ordering::SeqCst),
                0,
                "re-entry is a no-op"
            );
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
                    app_dark: None,
                    applied_dark: Some(false),
                })
            );
            assert!(set_frame_by_application(hwnd, true));
            assert_eq!(registered(hwnd).unwrap().app_dark, Some(true));
            assert!(release_frame_to_scheme(hwnd));
            assert_eq!(registered(hwnd).unwrap().app_dark, None);
            window_destroyed(hwnd);
            assert_eq!(registered(hwnd), None);
            // Untracked windows are not ours to touch.
            assert!(!set_frame_by_application(hwnd, true));
            assert!(!release_frame_to_scheme(hwnd));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::sync::Mutex;

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
        let f = frame_should_be_dark;
        assert_eq!(f(all, ColorScheme::Dark, false, false), Some(true));
        assert_eq!(f(all, ColorScheme::Light, false, false), Some(false));
        assert_eq!(f(all, ColorScheme::Unknown, false, false), Some(false));
        // `Qt::FramelessWindowHint` (and ferrokit's wider notion): nothing is written at all,
        // for any scheme.
        assert_eq!(f(all, ColorScheme::Dark, true, false), None);
        assert_eq!(f(all, ColorScheme::Light, true, false), None);
        assert_eq!(f(all, ColorScheme::Unknown, true, false), None);
        // `darkmode=0` and `darkmode=1` differ in the style only; frames need `WINDOW_FRAMES`.
        let empty = DarkModeHandling::empty();
        assert_eq!(f(empty, ColorScheme::Dark, false, false), Some(false));
        assert_eq!(
            f(DarkModeHandling::STYLE, ColorScheme::Dark, false, false),
            Some(false)
        );
        assert_eq!(
            f(
                DarkModeHandling::WINDOW_FRAMES,
                ColorScheme::Dark,
                false,
                false
            ),
            Some(true)
        );
        // The application's own `set_window_backdrop` dark flag is left alone.
        assert_eq!(f(all, ColorScheme::Dark, false, true), None);
    }

    #[test]
    fn the_registry_tracks_windows() {
        let mut r = WindowRegistry::default();
        r.insert(1, false);
        r.insert(2, true);
        assert!(r.mark_set_by_application(1, true));
        assert!(!r.mark_set_by_application(99, true)); // unknown window: ignored
        assert_eq!(r.entry(1).unwrap().app_dark, Some(true));
        assert_eq!(r.entry(2).unwrap().app_dark, None);
        assert!(r.entry(2).unwrap().frameless);
        assert_eq!(r.snapshot().len(), 2);
        // `BackdropType::None` gives the frame back and forgets what was written.
        assert!(r.release_to_scheme(1));
        assert_eq!(r.entry(1).unwrap().applied_dark, None);
        assert_eq!(r.entry(1).unwrap().app_dark, None);
        assert!(!r.release_to_scheme(99));
        // A frameless window is not taken over by the scheme.
        assert!(!r.release_to_scheme(2));
        assert_eq!(r.entry(2).unwrap().applied_dark, Some(false));
        r.remove(1);
        assert_eq!(r.entry(1), None);
        // A new window with a reused handle starts clean and light.
        r.insert(1, false);
        let e = r.entry(1).unwrap();
        assert_eq!(e.app_dark, None);
        assert_eq!(e.applied_dark, Some(false));
    }

    fn registry(windows: &[(isize, bool)]) -> Mutex<Option<WindowRegistry>> {
        let mut r = WindowRegistry::default();
        for (hwnd, frameless) in windows {
            r.insert(*hwnd, *frameless);
        }
        Mutex::new(Some(r))
    }

    fn entry(reg: &Mutex<Option<WindowRegistry>>, hwnd: isize) -> WindowEntry {
        reg.lock().unwrap().as_ref().unwrap().entry(hwnd).unwrap()
    }

    #[test]
    fn a_pass_writes_only_the_frames_that_change() {
        let reg = registry(&[(1, false), (2, true)]);
        let all = DarkModeHandling::default();
        let writes = RefCell::new(Vec::new());
        let run = |scheme| {
            apply_frames(&reg, all, scheme, |h, d| {
                writes.borrow_mut().push((h, d));
                true
            });
        };
        // New windows are already light: nothing to write under Light.
        run(ColorScheme::Light);
        assert!(writes.borrow().is_empty());
        // Dark darkens the framed window; the frameless one stays light.
        run(ColorScheme::Dark);
        assert_eq!(*writes.borrow(), [(1, true)]);
        // Again: nothing.
        run(ColorScheme::Dark);
        assert_eq!(writes.borrow().len(), 1);
        run(ColorScheme::Light);
        assert_eq!(*writes.borrow(), [(1, true), (1, false)]);
    }

    #[test]
    fn a_refused_write_is_retried_by_the_next_pass() {
        let reg = registry(&[(1, false)]);
        let all = DarkModeHandling::default();
        apply_frames(&reg, all, ColorScheme::Dark, |_, _| false);
        assert_eq!(entry(&reg, 1).applied_dark, Some(false), "not recorded");
        let writes = Cell::new(0);
        apply_frames(&reg, all, ColorScheme::Dark, |_, d| {
            assert!(d);
            writes.set(writes.get() + 1);
            true
        });
        assert_eq!(writes.get(), 1);
        assert_eq!(entry(&reg, 1).applied_dark, Some(true));
    }

    #[test]
    fn a_window_owned_by_the_application_is_skipped_until_released() {
        let reg = registry(&[(1, false)]);
        let all = DarkModeHandling::default();
        reg.lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .mark_set_by_application(1, false);
        let writes = Cell::new(0);
        let run = |scheme| {
            apply_frames(&reg, all, scheme, |_, _| {
                writes.set(writes.get() + 1);
                true
            })
        };
        run(ColorScheme::Dark);
        run(ColorScheme::Light);
        assert_eq!(writes.get(), 0, "the scheme leaves an owned frame alone");
        // `BackdropType::None` hands the frame back: the scheme is applied again, even though it
        // equals what was there before the application took over.
        reg.lock().unwrap().as_mut().unwrap().release_to_scheme(1);
        run(ColorScheme::Light);
        assert_eq!(writes.get(), 1);
        assert_eq!(entry(&reg, 1).applied_dark, Some(false));
    }

    #[test]
    fn a_window_destroyed_during_a_write_is_not_resurrected() {
        let reg = registry(&[(1, false)]);
        apply_frames(
            &reg,
            DarkModeHandling::default(),
            ColorScheme::Dark,
            |h, _| {
                reg.lock().unwrap().as_mut().unwrap().remove(h);
                true
            },
        );
        assert_eq!(reg.lock().unwrap().as_ref().unwrap().entry(1), None);
    }

    /// A scheme write that lands while the application takes the frame over: the application's
    /// value is written again, so it is the last one whichever side wrote last.
    #[test]
    fn an_application_owned_frame_wins_over_a_pass_in_flight() {
        let reg = registry(&[(1, false)]);
        let writes = RefCell::new(Vec::new());
        apply_frames(
            &reg,
            DarkModeHandling::default(),
            ColorScheme::Dark,
            |h, d| {
                writes.borrow_mut().push((h, d));
                if writes.borrow().len() == 1 {
                    // `set_window_backdrop` on another thread: marked, written, all between the
                    // snapshot and the end of this write. It wants a light frame.
                    reg.lock()
                        .unwrap()
                        .as_mut()
                        .unwrap()
                        .mark_set_by_application(h, false);
                }
                true
            },
        );
        assert_eq!(*writes.borrow(), [(1, true), (1, false)]);
        let e = entry(&reg, 1);
        assert_eq!(e.app_dark, Some(false));
        assert_eq!(
            e.applied_dark, None,
            "an owned frame is not the scheme's record"
        );
    }

    #[test]
    fn the_application_changing_its_mind_during_the_correction_still_wins() {
        let reg = registry(&[(1, false)]);
        let writes = RefCell::new(Vec::new());
        apply_frames(
            &reg,
            DarkModeHandling::default(),
            ColorScheme::Dark,
            |h, d| {
                writes.borrow_mut().push((h, d));
                let n = writes.borrow().len();
                // Scheme write (1): the application takes over with light. Correction (2): it
                // changes to dark meanwhile. Correction (3) is then the same as its value.
                if n <= 2 {
                    reg.lock()
                        .unwrap()
                        .as_mut()
                        .unwrap()
                        .mark_set_by_application(h, n == 2);
                }
                true
            },
        );
        assert_eq!(*writes.borrow(), [(1, true), (1, false), (1, true)]);
        assert_eq!(entry(&reg, 1).app_dark, Some(true));
    }

    #[test]
    fn a_frame_handed_back_during_a_write_is_left_to_the_pass_that_follows() {
        let reg = registry(&[(1, false)]);
        apply_frames(
            &reg,
            DarkModeHandling::default(),
            ColorScheme::Dark,
            |h, _| {
                let mut guard = reg.lock().unwrap();
                let r = guard.as_mut().unwrap();
                r.mark_set_by_application(h, false);
                r.release_to_scheme(h);
                true
            },
        );
        let e = entry(&reg, 1);
        assert_eq!(e.app_dark, None);
        assert_eq!(e.applied_dark, None, "unknown: the next pass writes it");
    }

    #[test]
    fn the_scheme_never_writes_a_frameless_window() {
        // A HUD-like window: frameless, so no attribute is written at all, neither 0 nor 1, and
        // not for any scheme, handling or release.
        let reg = registry(&[(1, true)]);
        let writes = Cell::new(0);
        for handling in [
            DarkModeHandling::default(),
            DarkModeHandling::empty(),
            DarkModeHandling::WINDOW_FRAMES,
        ] {
            for scheme in [ColorScheme::Dark, ColorScheme::Light, ColorScheme::Unknown] {
                apply_frames(&reg, handling, scheme, |_, _| {
                    writes.set(writes.get() + 1);
                    true
                });
            }
        }
        {
            let mut guard = reg.lock().unwrap();
            let r = guard.as_mut().unwrap();
            // `BackdropType::None`: the scheme does not take a frameless window over, so the
            // caller keeps writing its own value ...
            assert!(!r.release_to_scheme(1));
            // ... and one that was marked stays untouched by the scheme as well.
            r.mark_set_by_application(1, true);
        }
        for scheme in [ColorScheme::Dark, ColorScheme::Light] {
            apply_frames(&reg, DarkModeHandling::default(), scheme, |_, _| {
                writes.set(writes.get() + 1);
                true
            });
        }
        assert_eq!(writes.get(), 0);
    }

    #[test]
    fn the_coalescer_runs_a_request_that_arrives_during_a_pass_after_it() {
        let c = Coalescer::default();
        let scheme = Cell::new(ColorScheme::Light);
        let applied = RefCell::new(Vec::new());
        let passes = Cell::new(0);
        c.request(|| {
            passes.set(passes.get() + 1);
            // The pass reads the scheme first, as `pass` does ...
            let read = scheme.get();
            // ... and the scheme moves, and says so, while it is still working (another thread,
            // or a message sent by an export, in the real code).
            if passes.get() == 1 {
                scheme.set(ColorScheme::Dark);
                c.request(|| unreachable!("a nested request must not run a pass"));
            }
            applied.borrow_mut().push(read);
        });
        assert_eq!(passes.get(), 2);
        assert_eq!(
            *applied.borrow(),
            [ColorScheme::Light, ColorScheme::Dark],
            "the last pass saw the last scheme"
        );
        // And it is idle again afterwards.
        c.request(|| passes.set(passes.get() + 1));
        assert_eq!(passes.get(), 3);
    }

    #[test]
    fn a_window_created_during_a_pass_is_seen_by_the_repeat() {
        // `window_created`: insert, then request. A pass that started before the insert took its
        // snapshot without the window; the request makes it repeat with a fresh snapshot.
        let reg = registry(&[]);
        let c = Coalescer::default();
        let scheme = Cell::new(ColorScheme::Dark);
        let writes = RefCell::new(Vec::new());
        let all = DarkModeHandling::default();
        let passes = Cell::new(0);
        c.request(|| {
            passes.set(passes.get() + 1);
            let s = scheme.get();
            if passes.get() == 1 {
                // Another thread creates a window after this pass's scheme read ...
                reg.lock().unwrap().as_mut().unwrap().insert(7, false);
                c.request(|| unreachable!());
            }
            apply_frames(&reg, all, s, |h, d| {
                writes.borrow_mut().push((h, d));
                true
            });
        });
        // ... the first pass may or may not have seen the window; the repeat surely did.
        assert_eq!(passes.get(), 2);
        assert_eq!(*writes.borrow().last().unwrap(), (7, true));
        assert_eq!(entry(&reg, 7).applied_dark, Some(true));
    }

    #[test]
    fn the_coalescer_survives_a_panicking_pass() {
        let c = Coalescer::default();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            c.request(|| panic!("pass failed"));
        }));
        assert!(r.is_err());
        let ran = Cell::new(false);
        c.request(|| ran.set(true));
        assert!(ran.get(), "not stuck in the running state");
    }

    #[test]
    fn concurrent_requests_end_on_the_latest_scheme() {
        use std::sync::atomic::{AtomicU8, Ordering};
        use std::sync::Arc;
        let c = Arc::new(Coalescer::default());
        let scheme = Arc::new(AtomicU8::new(ColorScheme::Light as u8));
        let last_applied = Arc::new(AtomicU8::new(ColorScheme::Unknown as u8));
        let workers: Vec<_> = (0..4)
            .map(|i| {
                let (c, scheme, last) = (c.clone(), scheme.clone(), last_applied.clone());
                std::thread::spawn(move || {
                    for n in 0..200 {
                        let s = if (n + i) % 2 == 0 {
                            ColorScheme::Dark
                        } else {
                            ColorScheme::Light
                        };
                        scheme.store(s as u8, Ordering::SeqCst);
                        c.request(|| {
                            // Reads the scheme at the time of the pass, like `pass`.
                            last.store(scheme.load(Ordering::SeqCst), Ordering::SeqCst);
                        });
                    }
                })
            })
            .collect();
        for w in workers {
            w.join().unwrap();
        }
        assert_eq!(
            last_applied.load(Ordering::SeqCst),
            scheme.load(Ordering::SeqCst),
            "the last applied scheme is the last one set"
        );
    }
}
