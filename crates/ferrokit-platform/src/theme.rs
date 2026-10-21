// SPDX-License-Identifier: LGPL-3.0-only
use ferrokit_core::signal::Signal;
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ColorScheme {
    Unknown = 0,
    Light = 1,
    Dark = 2,
}

impl From<u8> for ColorScheme {
    fn from(val: u8) -> Self {
        match val {
            1 => ColorScheme::Light,
            2 => ColorScheme::Dark,
            _ => ColorScheme::Unknown,
        }
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
    fn color_scheme(&self) -> ColorScheme;
    fn theme_changed(&self) -> &Signal<ColorScheme>;
    fn refresh(&self);
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
}

#[cfg(windows)]
pub mod win32_theme {
    use super::*;
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ, REG_DWORD,
    };

    pub struct Win32Theme {
        cached_scheme: AtomicU8,
        theme_changed_signal: Signal<ColorScheme>,
    }

    impl Default for Win32Theme {
        fn default() -> Self {
            let initial = Self::query_color_scheme();
            let theme = Self {
                cached_scheme: AtomicU8::new(initial as u8),
                theme_changed_signal: Signal::new(),
            };
            // Qt reads the font smoothing state once, when the first painter starts
            // (`QRasterPaintEngine::clearTypeFontsEnabled`, a function-local static).
            ferrokit_gui::text::smoothing::set_text_smoothing(theme.text_smoothing());
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
            ColorScheme::from(self.cached_scheme.load(Ordering::Acquire))
        }

        fn theme_changed(&self) -> &Signal<ColorScheme> {
            &self.theme_changed_signal
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

        fn refresh(&self) {
            let current = Self::query_color_scheme();
            let previous =
                ColorScheme::from(self.cached_scheme.swap(current as u8, Ordering::AcqRel));
            if current != previous {
                self.theme_changed_signal.emit(&current);
            }
        }
    }
}

#[cfg(windows)]
pub use win32_theme::Win32Theme;

pub struct GenericTheme {
    cached_scheme: AtomicU8,
    theme_changed_signal: Signal<ColorScheme>,
}

impl Default for GenericTheme {
    fn default() -> Self {
        Self::new(ColorScheme::Light)
    }
}

impl GenericTheme {
    pub fn new(initial: ColorScheme) -> Self {
        Self {
            cached_scheme: AtomicU8::new(initial as u8),
            theme_changed_signal: Signal::new(),
        }
    }

    pub fn set_color_scheme(&self, scheme: ColorScheme) {
        let previous = ColorScheme::from(self.cached_scheme.swap(scheme as u8, Ordering::AcqRel));
        if scheme != previous {
            self.theme_changed_signal.emit(&scheme);
        }
    }
}

impl PlatformTheme for GenericTheme {
    fn color_scheme(&self) -> ColorScheme {
        ColorScheme::from(self.cached_scheme.load(Ordering::Acquire))
    }

    fn theme_changed(&self) -> &Signal<ColorScheme> {
        &self.theme_changed_signal
    }

    fn refresh(&self) {}
}

pub mod cocoa_theme {
    use super::*;

    pub struct CocoaTheme {
        cached_scheme: AtomicU8,
        theme_changed_signal: Signal<ColorScheme>,
    }

    impl Default for CocoaTheme {
        fn default() -> Self {
            let initial = Self::query_color_scheme();
            Self {
                cached_scheme: AtomicU8::new(initial as u8),
                theme_changed_signal: Signal::new(),
            }
        }
    }

    impl CocoaTheme {
        pub fn new() -> Self {
            Self::default()
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

        pub fn set_color_scheme(&self, scheme: ColorScheme) {
            let previous =
                ColorScheme::from(self.cached_scheme.swap(scheme as u8, Ordering::AcqRel));
            if scheme != previous {
                self.theme_changed_signal.emit(&scheme);
            }
        }
    }

    impl PlatformTheme for CocoaTheme {
        fn native_style(&self) -> NativeStyle {
            NativeStyle::Macintosh
        }

        fn color_scheme(&self) -> ColorScheme {
            ColorScheme::from(self.cached_scheme.load(Ordering::Acquire))
        }

        fn theme_changed(&self) -> &Signal<ColorScheme> {
            &self.theme_changed_signal
        }

        fn refresh(&self) {
            let current = Self::query_color_scheme();
            let previous =
                ColorScheme::from(self.cached_scheme.swap(current as u8, Ordering::AcqRel));
            if current != previous {
                self.theme_changed_signal.emit(&current);
            }
        }
    }
}

pub use cocoa_theme::CocoaTheme;

pub mod unix_theme {
    use super::*;

    pub struct UnixTheme {
        cached_scheme: AtomicU8,
        theme_changed_signal: Signal<ColorScheme>,
    }

    impl Default for UnixTheme {
        fn default() -> Self {
            let initial = Self::query_color_scheme();
            Self {
                cached_scheme: AtomicU8::new(initial as u8),
                theme_changed_signal: Signal::new(),
            }
        }
    }

    impl UnixTheme {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn query_color_scheme() -> ColorScheme {
            if let Ok(theme) = std::env::var("GTK_THEME") {
                if theme.to_lowercase().contains("dark") {
                    return ColorScheme::Dark;
                } else if theme.to_lowercase().contains("light") {
                    return ColorScheme::Light;
                }
            }
            ColorScheme::Dark
        }

        pub fn set_color_scheme(&self, scheme: ColorScheme) {
            let previous =
                ColorScheme::from(self.cached_scheme.swap(scheme as u8, Ordering::AcqRel));
            if scheme != previous {
                self.theme_changed_signal.emit(&scheme);
            }
        }
    }

    impl PlatformTheme for UnixTheme {
        fn color_scheme(&self) -> ColorScheme {
            ColorScheme::from(self.cached_scheme.load(Ordering::Acquire))
        }

        fn theme_changed(&self) -> &Signal<ColorScheme> {
            &self.theme_changed_signal
        }

        fn refresh(&self) {
            let current = Self::query_color_scheme();
            let previous =
                ColorScheme::from(self.cached_scheme.swap(current as u8, Ordering::AcqRel));
            if current != previous {
                self.theme_changed_signal.emit(&current);
            }
        }
    }
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
pub use unix_theme::UnixTheme;
