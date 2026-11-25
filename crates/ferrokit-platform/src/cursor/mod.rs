// SPDX-License-Identifier: LGPL-3.0-only
pub mod bitmap;
pub mod macos;

/// `Qt::CursorShape` without `BitmapCursor` (a bitmap cursor is not supported). `Busy` is
/// `Qt::BusyCursor`; the variants follow the order of the Qt enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
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
    SplitV,
    SplitH,
    PointingHand,
    Forbidden,
    WhatsThis,
    Busy,
    OpenHand,
    ClosedHand,
    DragCopy,
    DragMove,
    DragLink,
}

impl CursorShape {
    /// Every shape, in declaration order (`self as usize` indexes it).
    pub const ALL: [CursorShape; 22] = [
        CursorShape::Arrow,
        CursorShape::UpArrow,
        CursorShape::Cross,
        CursorShape::Wait,
        CursorShape::IBeam,
        CursorShape::SizeVer,
        CursorShape::SizeHor,
        CursorShape::SizeBDiag,
        CursorShape::SizeFDiag,
        CursorShape::SizeAll,
        CursorShape::Blank,
        CursorShape::SplitV,
        CursorShape::SplitH,
        CursorShape::PointingHand,
        CursorShape::Forbidden,
        CursorShape::WhatsThis,
        CursorShape::Busy,
        CursorShape::OpenHand,
        CursorShape::ClosedHand,
        CursorShape::DragCopy,
        CursorShape::DragMove,
        CursorShape::DragLink,
    ];
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
    /// Makes `shape` the application-wide cursor and shows it.
    fn change_cursor(&mut self, shape: CursorShape);
    /// `QPlatformCursor::size` (qplatformcursor.cpp:100-103): the cursor's size in native pixels
    /// on a screen with `screen_dpr` while the primary screen has `primary_dpr`. The default is
    /// 16x16; the Windows plugin overrides it with [`windows_cursor_size`].
    fn size(&self, _screen_dpr: f64, _primary_dpr: f64) -> (i32, i32) {
        (16, 16)
    }
    /// `QPlatformCursor::pos`: the pointer position in native desktop pixels, queried from the
    /// window system; `None` when the backend cannot query it (`QCursor::pos` then reports the
    /// position of the last mouse event, `QGuiApplicationPrivate::lastCursorPosition`).
    fn pos(&self) -> Option<ferrokit_gui::geometry::primitives::Point> {
        None
    }
    /// `QPlatformCursor::setPos`: moves the pointer to a native desktop position. A backend that
    /// cannot do it does nothing, which `QCursor::setPos` documents as allowed.
    fn set_pos(&mut self, _pos: ferrokit_gui::geometry::primitives::Point) {}
    /// `QPlatformCursor::setOverrideCursor`: shows `shape` on every window of the application
    /// until [`clear_override_cursor`](Self::clear_override_cursor). Returns whether the backend
    /// has override cursors; when it does not, the caller applies the shape to each window, the
    /// fallback of `QGuiApplication::setOverrideCursor` (qguiapplication.cpp:4449-4457).
    fn set_override_cursor(&mut self, _shape: CursorShape) -> bool {
        false
    }
    /// `QPlatformCursor::clearOverrideCursor`.
    fn clear_override_cursor(&mut self) {}
}

/// Sentinel stored in atomic cursor-shape slots when no override cursor is active.
const NO_SHAPE: u8 = u8::MAX;

fn shape_from_u8(raw: u8) -> Option<CursorShape> {
    CursorShape::ALL.get(raw as usize).copied()
}

/// Sets the cursor shape on the active platform integration (`PlatformCursor::change_cursor`).
///
/// Prefer `Window::set_cursor` when a `&mut Window` is available so per-window cursor tracking
/// stays in sync; use this cross-platform helper from global handlers or mouse callbacks without
/// a window reference instead of calling backend types like `Win32Cursor::set_shape` directly.
pub fn set_cursor_shape(shape: CursorShape) {
    crate::integration::platform().cursor().change_cursor(shape);
}

#[cfg(windows)]
pub mod win32_cursor {
    use super::*;
    use crate::win32_handles::{OwnedBitmap, create_dib_section};
    use ferrokit_gui::geometry::primitives::Point;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicIsize, AtomicU8, Ordering};
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::Graphics::Gdi::CreateBitmap;
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_READ, REG_DWORD, RegCloseKey, RegOpenKeyExW, RegQueryValueExW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateIconIndirect, GetCursorPos, GetSystemMetrics, HCURSOR, ICONINFO, IDC_APPSTARTING,
        IDC_ARROW, IDC_CROSS, IDC_HAND, IDC_HELP, IDC_IBEAM, IDC_NO, IDC_SIZEALL, IDC_SIZENESW,
        IDC_SIZENS, IDC_SIZENWSE, IDC_SIZEWE, IDC_UPARROW, IDC_WAIT, LoadCursorW, SM_CXCURSOR,
        SetCursor, SetCursorPos,
    };

    static CURRENT_GLOBAL_SHAPE: AtomicU8 = AtomicU8::new(0);
    /// The application override cursor (`QWindowsCursor::m_overrideCursor`).
    static OVERRIDE_SHAPE: AtomicU8 = AtomicU8::new(NO_SHAPE);
    /// The cursor that was showing when the override began (`QWindowsCursor::m_overriddenCursor`),
    /// put back when the override ends; 0 while none.
    static OVERRIDDEN_CURSOR: AtomicIsize = AtomicIsize::new(0);

    /// Cursors built from [`custom_cursor_image`] as (shape, system cursor width, `HCURSOR`),
    /// created once and never destroyed, like Qt's `m_standardCursorCache`. Process-wide: a cursor
    /// is a USER object, not owned by a thread, and "handles to user objects are public to all
    /// processes" (learn.microsoft.com/windows/win32/sysinfo/user-objects), so any thread may show
    /// a cursor another thread made. A handful of entries (seven shapes per cursor width), so a
    /// linear search. A handle of 0 records a failed creation, which is not retried for that
    /// width (`WM_SETCURSOR` would otherwise rebuild the bitmaps under the lock on every move).
    static CUSTOM_CURSORS: Mutex<Vec<(u8, i32, isize)>> = Mutex::new(Vec::new());

    pub struct Win32Cursor {
        current_shape: CursorShape,
    }
    impl Default for Win32Cursor {
        fn default() -> Self {
            Self::new()
        }
    }

    /// `createPixmapCursor` (qwindowscursor.cpp:69-97): a 32-bit premultiplied colour bitmap with
    /// an AND mask, as `CreateIconIndirect` takes it, for the image's hot spot.
    fn create_custom_cursor(shape: CursorShape, system_cursor_width: i32) -> HCURSOR {
        let Some(image) = crate::cursor::bitmap::custom_cursor_image(shape, system_cursor_width)
        else {
            return std::ptr::null_mut();
        };
        let (width, height) = (image.width as i32, image.height as i32);
        // The colour and mask bitmaps are owned by guards, so they are deleted on every path out
        // (`CreateIconIndirect` copies them), including an unwind.
        let Some((color, bits)) = create_dib_section(
            std::ptr::null_mut(),
            image.width as u32,
            image.height as u32,
        ) else {
            return std::ptr::null_mut();
        };
        // SAFETY: `bits` is the non-null pixel pointer `create_dib_section` returned for a 32-bit
        // `image.width` x `image.height` DIB section kept alive by `color`, i.e. `width * height * 4`
        // writable bytes, and `image.bgra` is exactly that long (allocated `w * h * 4` in
        // `cursor_bitmap`); the two buffers cannot overlap (GDI-owned memory vs a `Vec`).
        unsafe {
            std::ptr::copy_nonoverlapping(image.bgra.as_ptr(), bits, image.bgra.len());
        }
        // The AND mask: a set bit lets the screen show through.
        let stride = (image.width.div_ceil(16)) * 2;
        let mut and_mask = vec![0u8; stride * image.height];
        for y in 0..image.height {
            for x in 0..image.width {
                if image.pixel(x, y)[3] == 0 {
                    and_mask[y * stride + x / 8] |= 0x80 >> (x % 8);
                }
            }
        }
        // SAFETY: `and_mask` outlives the call and holds `height` rows of `stride` bytes (a WORD
        // multiple), exactly the 1-bit layout CreateBitmap reads for a `width` x `height` bitmap.
        let mask = OwnedBitmap::from_raw(unsafe {
            CreateBitmap(width, height, 1, 1, and_mask.as_ptr() as *const _)
        });
        let icon = ICONINFO {
            fIcon: 0,
            xHotspot: image.hot_x.max(0) as u32,
            yHotspot: image.hot_y.max(0) as u32,
            hbmMask: mask
                .as_ref()
                .map_or(std::ptr::null_mut(), OwnedBitmap::as_raw),
            hbmColor: color.as_raw(),
        };
        // SAFETY: `icon` is a live, fully initialised ICONINFO; `hbmColor`/`hbmMask` are bitmaps
        // owned by the guards `color`/`mask`, alive past the call (CreateIconIndirect copies them);
        // a null mask, if CreateBitmap failed, is simply rejected by the call.
        unsafe { CreateIconIndirect(&icon) }
    }

    /// The system's handle for `shape` (`QWindowsCursor::createCursorFromShape`); null for
    /// `Blank`, which hides the cursor.
    fn handle_for(shape: CursorShape) -> HCURSOR {
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
            CursorShape::Forbidden => IDC_NO,
            CursorShape::WhatsThis => IDC_HELP,
            CursorShape::PointingHand => IDC_HAND,
            CursorShape::Busy => IDC_APPSTARTING,
            CursorShape::Blank => return std::ptr::null_mut(),
            CursorShape::SplitV
            | CursorShape::SplitH
            | CursorShape::OpenHand
            | CursorShape::ClosedHand
            | CursorShape::DragCopy
            | CursorShape::DragMove
            | CursorShape::DragLink => {
                // SAFETY: GetSystemMetrics takes only an integer index and has no preconditions.
                let width = unsafe { GetSystemMetrics(SM_CXCURSOR) };
                let key = (shape as u8, width);
                // Held while a missing cursor is made, so two threads never both create (and one
                // leak) the same cursor. The list is only pushed to after the attempt, so a panic
                // while the lock is held leaves it consistent and poisoning is ignored.
                let mut cache = CUSTOM_CURSORS.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(&(_, _, handle)) = cache.iter().find(|&&(s, w, _)| (s, w) == key) {
                    return handle as HCURSOR;
                }
                let handle = create_custom_cursor(shape, width);
                cache.push((key.0, key.1, handle as isize));
                return handle;
            }
        };
        // SAFETY: a null instance with an `IDC_*` id (chosen by the match above) is the documented
        // way to load a shared system cursor, which belongs to the system and needs no destruction.
        unsafe { LoadCursorW(std::ptr::null_mut(), idc) }
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
            shape_from_u8(CURRENT_GLOBAL_SHAPE.load(Ordering::Relaxed))
                .unwrap_or(CursorShape::Arrow)
        }

        /// Makes `shape` the application-wide cursor and shows it.
        pub fn set_shape(shape: CursorShape) {
            CURRENT_GLOBAL_SHAPE.store(shape as u8, Ordering::Relaxed);
            Self::apply_shape(shape);
        }

        /// The application override cursor, if one is set (`QWindowsCursor::hasOverrideCursor`).
        pub fn override_shape() -> Option<CursorShape> {
            shape_from_u8(OVERRIDE_SHAPE.load(Ordering::Relaxed))
        }

        /// `QWindowsCursor::setOverrideCursor` (qwindowscursor.cpp:626-639): shows `shape` and
        /// remembers the cursor that was showing, once.
        pub fn set_override(shape: CursorShape) {
            OVERRIDE_SHAPE.store(shape as u8, Ordering::Relaxed);
            let previous = Self::set_handle(shape);
            if OVERRIDDEN_CURSOR.load(Ordering::Relaxed) == 0 && !previous.is_null() {
                OVERRIDDEN_CURSOR.store(previous as isize, Ordering::Relaxed);
            }
        }

        /// `QWindowsCursor::clearOverrideCursor` (qwindowscursor.cpp:641-650): puts the
        /// remembered cursor back. A window re-applies its own cursor on its next
        /// `WM_SETCURSOR`.
        pub fn clear_override() {
            OVERRIDE_SHAPE.store(NO_SHAPE, Ordering::Relaxed);
            let previous = OVERRIDDEN_CURSOR.swap(0, Ordering::Relaxed);
            if previous != 0 {
                // SAFETY: `previous` is the non-zero `HCURSOR` that `SetCursor` returned in
                // `set_override`; it is a system cursor or one of the cached custom cursors, neither
                // of which is ever destroyed, so it is still valid. SetCursor only takes the handle.
                unsafe { SetCursor(previous as HCURSOR) };
            }
        }

        /// The native cursor handle of `shape` (an `HCURSOR`; 0 for `Blank`, which has none).
        pub fn native_handle(shape: CursorShape) -> isize {
            handle_for(shape) as isize
        }

        /// Shows `shape` and returns the cursor that was showing.
        fn set_handle(shape: CursorShape) -> HCURSOR {
            let handle = handle_for(shape);
            if handle.is_null() && shape != CursorShape::Blank {
                return std::ptr::null_mut();
            }
            // SAFETY: a null handle removes the cursor image (`Qt::BlankCursor`); any other
            // handle is a shared system cursor or one of ours that is never destroyed.
            unsafe { SetCursor(handle) }
        }

        /// Shows `shape` on the screen without changing the application-wide cursor
        /// (a per-window cursor is applied from that window's `WM_SETCURSOR`).
        /// `Blank` hides the cursor (`Qt::BlankCursor`).
        pub fn apply_shape(shape: CursorShape) {
            let _ = Self::set_handle(shape);
        }
    }

    impl PlatformCursor for Win32Cursor {
        fn change_cursor(&mut self, shape: CursorShape) {
            self.current_shape = shape;
            Self::set_shape(shape);
        }

        fn size(&self, screen_dpr: f64, primary_dpr: f64) -> (i32, i32) {
            // SAFETY: GetSystemMetrics takes only an integer index and has no preconditions.
            let width = unsafe { GetSystemMetrics(SM_CXCURSOR) };
            let s = windows_cursor_size(width, cursor_base_size(), screen_dpr, primary_dpr);
            (s, s)
        }

        fn pos(&self) -> Option<Point> {
            let mut pt = POINT { x: 0, y: 0 };
            // SAFETY: `&mut pt` points at the live local `pt`, a POINT GetCursorPos fills in.
            (unsafe { GetCursorPos(&mut pt) } != 0).then(|| Point::new(pt.x, pt.y))
        }

        fn set_pos(&mut self, pos: Point) {
            // SAFETY: SetCursorPos takes two integers and has no pointer or handle arguments;
            // out-of-range coordinates are clamped by the system.
            unsafe { SetCursorPos(pos.x, pos.y) };
        }

        fn set_override_cursor(&mut self, shape: CursorShape) -> bool {
            Self::set_override(shape);
            true
        }

        fn clear_override_cursor(&mut self) {
            Self::clear_override();
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
        // SAFETY: `subkey` and `value_name` are NUL-terminated UTF-16 buffers (terminators chained
        // above) alive for the whole block. `hkey` is a live out slot, used only after
        // RegOpenKeyExW succeeded (status 0) and closed exactly once below. `data` is a `u32`
        // and `data_size` its byte size (4), so RegQueryValueExW writes at most the 4 bytes of
        // `data`; a value of another type/size is rejected by the `REG_DWORD` check.
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
    use crate::objc_runtime::{Class, Id, ObjcMsg, Sel, appkit};
    use std::ffi::c_void;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

    static CURRENT_GLOBAL_SHAPE: AtomicU8 = AtomicU8::new(0);
    /// The application override cursor (`QCocoaCursor` override), `NO_SHAPE` when inactive.
    static OVERRIDE_SHAPE: AtomicU8 = AtomicU8::new(NO_SHAPE);
    /// Retained `NSCursor` that was showing when `set_override` began, restored and released on
    /// `clear_override`; 0 while none.
    static OVERRIDDEN_CURSOR: AtomicUsize = AtomicUsize::new(0);
    /// Immortal `+1` custom `NSCursor` instances (`Id.0 as usize`) built from
    /// [`crate::cursor::bitmap::cocoa_custom_cursor_image`], indexed by `CursorShape as usize`.
    /// `None` means not yet constructed; `Some(0)` caches a creation failure so repeated cursor
    /// updates never rebuild bitmaps on every mouse move.
    static CUSTOM_CURSORS: Mutex<[Option<usize>; CursorShape::ALL.len()]> =
        Mutex::new([None; CursorShape::ALL.len()]);

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

        /// The application-wide shape last passed to [`change_cursor`](PlatformCursor::change_cursor)
        /// or [`set_shape`](Self::set_shape).
        pub fn current_global_shape() -> CursorShape {
            shape_from_u8(CURRENT_GLOBAL_SHAPE.load(Ordering::Relaxed))
                .unwrap_or(CursorShape::Arrow)
        }

        /// The public `NSCursor` class method for `shape` (`QCocoaCursor::createCursorFromShape`,
        /// qcocoacursor.mm:103-146), or `None` when `shape` uses a custom `NSImage` cursor from
        /// [`crate::cursor::bitmap::cocoa_custom_cursor_image`].
        pub fn native_selector(shape: CursorShape) -> Option<&'static str> {
            match shape {
                CursorShape::Arrow => Some("arrowCursor"),
                CursorShape::PointingHand => Some("pointingHandCursor"),
                CursorShape::IBeam => Some("IBeamCursor"),
                CursorShape::SizeHor | CursorShape::SplitH => Some("resizeLeftRightCursor"),
                CursorShape::SizeVer | CursorShape::SplitV => Some("resizeUpDownCursor"),
                CursorShape::Cross | CursorShape::DragMove => Some("crosshairCursor"),
                CursorShape::Forbidden => Some("operationNotAllowedCursor"),
                CursorShape::OpenHand => Some("openHandCursor"),
                CursorShape::ClosedHand => Some("closedHandCursor"),
                CursorShape::DragCopy => Some("dragCopyCursor"),
                CursorShape::DragLink => Some("dragLinkCursor"),
                CursorShape::SizeBDiag
                | CursorShape::SizeFDiag
                | CursorShape::SizeAll
                | CursorShape::UpArrow
                | CursorShape::Wait
                | CursorShape::Busy
                | CursorShape::WhatsThis
                | CursorShape::Blank => None,
            }
        }

        /// The `NSCursor` class method that returns the standard cursor for `shape`, falling back
        /// to `"arrowCursor"` for shapes built as custom `NSCursor` images; prefer
        /// [`native_cursor`](Self::native_cursor) when applying cursors.
        pub fn ns_cursor_selector(shape: CursorShape) -> &'static str {
            Self::native_selector(shape).unwrap_or("arrowCursor")
        }

        /// Returns the `NSCursor` for any [`CursorShape`] — either an immortal AppKit singleton or
        /// a process-wide cached custom `NSCursor` built from `cocoa_custom_cursor_image`.
        ///
        /// Returns `Id::NIL` if called off the AppKit main thread on macOS.
        pub fn native_cursor(shape: CursorShape) -> Id {
            if !appkit::is_main_thread() {
                return Id::NIL;
            }
            if let Some(selector) = Self::native_selector(shape) {
                // SAFETY: `selector` is a standard `NSCursor` class method returning an immortal
                // AppKit singleton, and we verified we are on AppKit's main thread above.
                return unsafe { appkit::standard_cursor(selector) };
            }

            let idx = shape as usize;
            let mut cache = CUSTOM_CURSORS.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(cached) = cache[idx] {
                return Id(cached as *mut c_void);
            }
            let cursor = crate::cursor::bitmap::cocoa_custom_cursor_image(shape)
                // SAFETY: `custom_cursor_from_image` is called on the AppKit main thread (checked
                // above) and returns a +1 `NSCursor` (or nil), which stays cached permanently.
                .map(|image| unsafe { appkit::custom_cursor_from_image(&image) })
                .unwrap_or(Id::NIL);
            cache[idx] = Some(cursor.0 as usize);
            cursor
        }

        fn apply_cursor_id(cursor: Id) {
            if !cursor.is_nil() {
                // SAFETY: `cursor` is non-nil (an immortal AppKit singleton or a cached custom
                // `NSCursor` from `CUSTOM_CURSORS`); `set` takes no arguments and returns void,
                // and callers already verified `appkit::is_main_thread()`.
                unsafe { ObjcMsg::send_0(cursor, Sel::register("set")) };
            }
        }

        pub fn set_shape(shape: CursorShape) {
            CURRENT_GLOBAL_SHAPE.store(shape as u8, Ordering::Relaxed);
            if Self::override_shape().is_none() && appkit::is_main_thread() {
                Self::apply_cursor_id(Self::native_cursor(shape));
            }
        }

        /// The application override cursor, if one is active (`QCocoaCursor` override).
        pub fn override_shape() -> Option<CursorShape> {
            shape_from_u8(OVERRIDE_SHAPE.load(Ordering::Relaxed))
        }

        /// Shows `shape` as an application-wide override cursor across all Cocoa windows until
        /// [`clear_override`](Self::clear_override), remembering the cursor that was showing once.
        pub fn set_override(shape: CursorShape) {
            if OVERRIDDEN_CURSOR.load(Ordering::Relaxed) == 0 && appkit::is_main_thread() {
                let cursor_class = Class::get("NSCursor").unwrap_or(Class::NIL);
                // SAFETY: `currentCursor` is a class method of `NSCursor` taking no arguments
                // returning an `NSCursor` object (nil class returns nil); called on the AppKit main
                // thread (checked above).
                let previous =
                    unsafe { ObjcMsg::send_class_0(cursor_class, Sel::register("currentCursor")) };
                if !previous.is_nil() {
                    // SAFETY: `previous` is a non-nil `NSCursor` returned by `currentCursor`;
                    // retaining gives `OVERRIDDEN_CURSOR` its own reference balanced in
                    // `clear_override` (or released immediately below if another call won the CAS).
                    unsafe { appkit::retain(previous) };
                    if OVERRIDDEN_CURSOR
                        .compare_exchange(
                            0,
                            previous.0 as usize,
                            Ordering::AcqRel,
                            Ordering::Relaxed,
                        )
                        .is_err()
                    {
                        // SAFETY: this thread lost the CAS race above, so nobody owns this extra
                        // `+1` retain except us; release it once.
                        unsafe { appkit::release(previous) };
                    }
                }
            }
            OVERRIDE_SHAPE.store(shape as u8, Ordering::Relaxed);
            if appkit::is_main_thread() {
                Self::apply_cursor_id(Self::native_cursor(shape));
                crate::window::macos_input::invalidate_all_cursor_rects();
            }
        }

        /// Ends an active override cursor and restores the cursor that was showing beforehand, then
        /// asks Cocoa windows to rebuild their cursor rects.
        pub fn clear_override() {
            OVERRIDE_SHAPE.store(NO_SHAPE, Ordering::Relaxed);
            if appkit::is_main_thread() {
                let previous = OVERRIDDEN_CURSOR.swap(0, Ordering::AcqRel);
                if previous != 0 {
                    let prev_id = Id(previous as *mut c_void);
                    Self::apply_cursor_id(prev_id);
                    // SAFETY: `prev_id` carries the `+1` retain stored by `set_override` and was
                    // taken out of `OVERRIDDEN_CURSOR` atomically via `swap` above, so releasing it
                    // after `[prev_id set]` balances that retain exactly once.
                    unsafe { appkit::release(prev_id) };
                } else {
                    Self::apply_cursor_id(Self::native_cursor(Self::current_global_shape()));
                }
                crate::window::macos_input::invalidate_all_cursor_rects();
            }
        }
    }

    impl PlatformCursor for CocoaCursor {
        fn change_cursor(&mut self, shape: CursorShape) {
            self.current_shape = shape;
            Self::set_shape(shape);
        }

        fn pos(&self) -> Option<ferrokit_gui::geometry::primitives::Point> {
            crate::cursor::macos::cursor_pos(&crate::cursor::macos::NativeCursorServices)
        }

        fn set_pos(&mut self, pos: ferrokit_gui::geometry::primitives::Point) {
            crate::cursor::macos::set_cursor_pos(&crate::cursor::macos::NativeCursorServices, pos);
        }

        fn set_override_cursor(&mut self, shape: CursorShape) -> bool {
            Self::set_override(shape);
            true
        }

        fn clear_override_cursor(&mut self) {
            Self::clear_override();
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
                CursorShape::SplitV => "row-resize",
                CursorShape::SplitH => "col-resize",
                CursorShape::Forbidden => "not-allowed",
                CursorShape::WhatsThis => "help",
                CursorShape::OpenHand => "grab",
                CursorShape::ClosedHand => "grabbing",
                CursorShape::DragCopy => "copy",
                CursorShape::DragMove => "move",
                CursorShape::DragLink => "alias",
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
