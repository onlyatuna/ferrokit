// SPDX-License-Identifier: LGPL-3.0-only
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CursorShape {
    Arrow,
    UpArrow,
    Cross,
    Wait,
    IBeam,
    SizeVer,
    SizeHor,
    SizeBDiag,
    SizeFDiag,
    SizeAll,
    Blank,
    PointingHand,
    Busy,
}

/// `QWindowsCursor::size` (qwindowscursor.cpp:675-692): the cursor's size in native pixels on a
/// screen whose device pixel ratio is `screen_dpr`, given the system cursor width
/// (`SM_CXCURSOR`), the accessibility value `HKCU\Control Panel\Cursors\CursorBaseSize` and the
/// device pixel ratio of the primary screen. `logicalDpi / logicalBaseDpi` is `dpr` (96 DPI base).
pub fn windows_cursor_size(
    system_cursor_width: i32,
    cursor_base_size: Option<u32>,
    screen_dpr: f64,
    primary_dpr: f64,
) -> i32 {
    // `screenCursorSize` (qwindowscursor.cpp:162-179): another screen's DPI rescales the
    // primary screen's size (`QSizeF::toSize` rounds, `qFuzzyCompare` is relative 1e-12).
    let fuzzy_equal =
        (screen_dpr - primary_dpr).abs() * 1e12 <= screen_dpr.abs().min(primary_dpr.abs());
    let width = if fuzzy_equal {
        system_cursor_width
    } else {
        (system_cursor_width as f64 * screen_dpr / primary_dpr).round() as i32
    };
    let base_size = width / 2;
    let Some(setting) = cursor_base_size else {
        return base_size / 2;
    };
    if base_size == 0 {
        return 0;
    }
    // The registry value is DPI-independent; 32..256 maps to steps 0..14 of `base_size / 2` px.
    let scaled = (setting as f64 * screen_dpr) as i32;
    let steps = (scaled - 2 * base_size) / base_size;
    base_size + steps * (base_size / 2)
}

pub trait PlatformCursor: Send + Sync {
    fn change_cursor(&mut self, shape: CursorShape);
    /// `QPlatformCursor::size` (qplatformcursor.cpp:100-103): the cursor's size in native pixels
    /// on a screen with `screen_dpr` while the primary screen has `primary_dpr`. The default is
    /// 16x16; the Windows plugin overrides it with [`windows_cursor_size`].
    fn size(&self, _screen_dpr: f64, _primary_dpr: f64) -> (i32, i32) {
        (16, 16)
    }
}

#[cfg(windows)]
pub mod win32_cursor {
    use super::*;
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ, REG_DWORD,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, LoadCursorW, SetCursor, HCURSOR, IDC_APPSTARTING, IDC_ARROW, IDC_CROSS,
        IDC_HAND, IDC_IBEAM, IDC_NO, IDC_SIZEALL, IDC_SIZENESW, IDC_SIZENS, IDC_SIZENWSE,
        IDC_SIZEWE, IDC_UPARROW, IDC_WAIT, SM_CXCURSOR,
    };
    static CURRENT_GLOBAL_SHAPE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

    pub struct Win32Cursor {
        current_shape: CursorShape,
    }
    impl Default for Win32Cursor {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Win32Cursor {
        pub fn new() -> Self {
            Self {
                current_shape: CursorShape::Arrow,
            }
        }

        pub fn current_shape(&self) -> CursorShape {
            self.current_shape
        }

        pub fn current_global_shape() -> CursorShape {
            match CURRENT_GLOBAL_SHAPE.load(std::sync::atomic::Ordering::Relaxed) {
                1 => CursorShape::UpArrow,
                2 => CursorShape::Cross,
                3 => CursorShape::Wait,
                4 => CursorShape::IBeam,
                5 => CursorShape::SizeVer,
                6 => CursorShape::SizeHor,
                7 => CursorShape::SizeBDiag,
                8 => CursorShape::SizeFDiag,
                9 => CursorShape::SizeAll,
                10 => CursorShape::Blank,
                11 => CursorShape::PointingHand,
                12 => CursorShape::Busy,
                _ => CursorShape::Arrow,
            }
        }

        pub fn set_shape(shape: CursorShape) {
            let code = match shape {
                CursorShape::Arrow => 0,
                CursorShape::UpArrow => 1,
                CursorShape::Cross => 2,
                CursorShape::Wait => 3,
                CursorShape::IBeam => 4,
                CursorShape::SizeVer => 5,
                CursorShape::SizeHor => 6,
                CursorShape::SizeBDiag => 7,
                CursorShape::SizeFDiag => 8,
                CursorShape::SizeAll => 9,
                CursorShape::Blank => 10,
                CursorShape::PointingHand => 11,
                CursorShape::Busy => 12,
            };
            CURRENT_GLOBAL_SHAPE.store(code, std::sync::atomic::Ordering::Relaxed);
            let idc = match shape {
                CursorShape::Arrow => IDC_ARROW,
                CursorShape::UpArrow => IDC_UPARROW,
                CursorShape::Cross => IDC_CROSS,
                CursorShape::Wait => IDC_WAIT,
                CursorShape::IBeam => IDC_IBEAM,
                CursorShape::SizeVer => IDC_SIZENS,
                CursorShape::SizeHor => IDC_SIZEWE,
                CursorShape::SizeBDiag => IDC_SIZENESW,
                CursorShape::SizeFDiag => IDC_SIZENWSE,
                CursorShape::SizeAll => IDC_SIZEALL,
                CursorShape::Blank => IDC_NO,
                CursorShape::PointingHand => IDC_HAND,
                CursorShape::Busy => IDC_APPSTARTING,
            };

            unsafe {
                let hcursor: HCURSOR = LoadCursorW(std::ptr::null_mut(), idc);
                if !hcursor.is_null() {
                    SetCursor(hcursor);
                }
            }
        }
    }

    impl PlatformCursor for Win32Cursor {
        fn change_cursor(&mut self, shape: CursorShape) {
            self.current_shape = shape;
            Self::set_shape(shape);
        }

        fn size(&self, screen_dpr: f64, primary_dpr: f64) -> (i32, i32) {
            let width = unsafe { GetSystemMetrics(SM_CXCURSOR) };
            let s = windows_cursor_size(width, cursor_base_size(), screen_dpr, primary_dpr);
            (s, s)
        }
    }

    /// `HKCU\Control Panel\Cursors\CursorBaseSize` (`QWinRegistryKey::value<DWORD>`), if set.
    fn cursor_base_size() -> Option<u32> {
        let subkey: Vec<u16> = "Control Panel\\Cursors"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let value_name: Vec<u16> = "CursorBaseSize"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        unsafe {
            let mut hkey: HKEY = std::ptr::null_mut();
            if RegOpenKeyExW(HKEY_CURRENT_USER, subkey.as_ptr(), 0, KEY_READ, &mut hkey) != 0 {
                return None;
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
            (status == 0 && val_type == REG_DWORD).then_some(data)
        }
    }
}

#[cfg(windows)]
pub use win32_cursor::Win32Cursor;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenericCursor {
    current_shape: CursorShape,
}

impl Default for GenericCursor {
    fn default() -> Self {
        Self {
            current_shape: CursorShape::Arrow,
        }
    }
}

impl GenericCursor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn current_shape(&self) -> CursorShape {
        self.current_shape
    }
}

impl PlatformCursor for GenericCursor {
    fn change_cursor(&mut self, shape: CursorShape) {
        self.current_shape = shape;
    }
}

pub mod cocoa_cursor {
    use super::*;
    use crate::objc_runtime::{Class, ObjcMsg, Sel};

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct CocoaCursor {
        current_shape: CursorShape,
    }

    impl Default for CocoaCursor {
        fn default() -> Self {
            Self::new()
        }
    }

    impl CocoaCursor {
        pub fn new() -> Self {
            Self {
                current_shape: CursorShape::Arrow,
            }
        }

        pub fn current_shape(&self) -> CursorShape {
            self.current_shape
        }

        pub fn set_shape(shape: CursorShape) {
            let cursor_class = Class::get("NSCursor").unwrap_or(Class::NIL);
            let sel_name = match shape {
                CursorShape::Arrow => "arrowCursor",
                CursorShape::PointingHand => "pointingHandCursor",
                CursorShape::IBeam => "IBeamCursor",
                CursorShape::SizeHor => "resizeLeftRightCursor",
                CursorShape::SizeVer => "resizeUpDownCursor",
                CursorShape::Cross => "crosshairCursor",
                _ => "arrowCursor",
            };
            let cursor = ObjcMsg::send_class_0(cursor_class, Sel::register(sel_name));
            if !cursor.is_nil() {
                ObjcMsg::send_0(cursor, Sel::register("set"));
            }
        }
    }

    impl PlatformCursor for CocoaCursor {
        fn change_cursor(&mut self, shape: CursorShape) {
            self.current_shape = shape;
            Self::set_shape(shape);
        }
    }
}

pub use cocoa_cursor::CocoaCursor;

pub mod unix_cursor {
    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct UnixCursor {
        current_shape: CursorShape,
    }

    impl Default for UnixCursor {
        fn default() -> Self {
            Self::new()
        }
    }

    impl UnixCursor {
        pub fn new() -> Self {
            Self {
                current_shape: CursorShape::Arrow,
            }
        }

        pub fn current_shape(&self) -> CursorShape {
            self.current_shape
        }

        pub fn cursor_name(shape: CursorShape) -> &'static str {
            match shape {
                CursorShape::Arrow => "default",
                CursorShape::PointingHand => "pointer",
                CursorShape::IBeam => "text",
                CursorShape::SizeHor => "ew-resize",
                CursorShape::SizeVer => "ns-resize",
                CursorShape::SizeFDiag => "nwse-resize",
                CursorShape::SizeBDiag => "nesw-resize",
                CursorShape::SizeAll => "move",
                CursorShape::Cross => "crosshair",
                CursorShape::Wait => "wait",
                CursorShape::Busy => "progress",
                _ => "default",
            }
        }
    }

    impl PlatformCursor for UnixCursor {
        fn change_cursor(&mut self, shape: CursorShape) {
            self.current_shape = shape;
        }
    }
}

pub use unix_cursor::UnixCursor;
