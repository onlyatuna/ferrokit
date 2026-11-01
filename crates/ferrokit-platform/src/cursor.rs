// SPDX-License-Identifier: LGPL-3.0-only
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

    /// `OVERRIDE_SHAPE` value while no override cursor is set.
    const NO_SHAPE: u8 = u8::MAX;

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
        let Some(image) = crate::cursor_bitmap::custom_cursor_image(shape, system_cursor_width)
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
            CursorShape::ALL
                .get(CURRENT_GLOBAL_SHAPE.load(Ordering::Relaxed) as usize)
                .copied()
                .unwrap_or(CursorShape::Arrow)
        }

        /// Makes `shape` the application-wide cursor and shows it.
        pub fn set_shape(shape: CursorShape) {
            CURRENT_GLOBAL_SHAPE.store(shape as u8, Ordering::Relaxed);
            Self::apply_shape(shape);
        }

        /// The application override cursor, if one is set (`QWindowsCursor::hasOverrideCursor`).
        pub fn override_shape() -> Option<CursorShape> {
            CursorShape::ALL
                .get(OVERRIDE_SHAPE.load(Ordering::Relaxed) as usize)
                .copied()
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

        /// The `NSCursor` class method that returns the cursor for `shape`
        /// (`QCocoaCursor::createCursorFromShape`, qcocoacursor.mm:103-146).
        pub fn ns_cursor_selector(shape: CursorShape) -> &'static str {
            match shape {
                CursorShape::Arrow => "arrowCursor",
                CursorShape::PointingHand => "pointingHandCursor",
                CursorShape::IBeam => "IBeamCursor",
                CursorShape::SizeHor => "resizeLeftRightCursor",
                CursorShape::SizeVer => "resizeUpDownCursor",
                CursorShape::Cross => "crosshairCursor",
                CursorShape::Forbidden => "operationNotAllowedCursor",
                CursorShape::SplitV => "resizeUpDownCursor",
                CursorShape::SplitH => "resizeLeftRightCursor",
                CursorShape::OpenHand => "openHandCursor",
                CursorShape::ClosedHand => "closedHandCursor",
                CursorShape::DragMove => "crosshairCursor",
                CursorShape::DragCopy => "dragCopyCursor",
                CursorShape::DragLink => "dragLinkCursor",
                _ => "arrowCursor",
            }
        }

        pub fn set_shape(shape: CursorShape) {
            let cursor_class = Class::get("NSCursor").unwrap_or(Class::NIL);
            let sel_name = Self::ns_cursor_selector(shape);
            // SAFETY: `send_class_0` fits a class method with no arguments returning an object, and
            // every selector from `ns_cursor_selector` is such an `NSCursor` class method; a nil
            // class (failed lookup) makes the send a no-op returning nil. Main-thread use is the
            // caller's contract [inferred: not checked here].
            let cursor = unsafe { ObjcMsg::send_class_0(cursor_class, Sel::register(sel_name)) };
            if !cursor.is_nil() {
                // SAFETY: `cursor` is non-nil (checked above), a shared `NSCursor` returned by the
                // class method; `set` takes no arguments and returns void, matching `send_0`.
                unsafe { ObjcMsg::send_0(cursor, Sel::register("set")) };
            }
        }
    }

    impl PlatformCursor for CocoaCursor {
        fn change_cursor(&mut self, shape: CursorShape) {
            self.current_shape = shape;
            Self::set_shape(shape);
        }

        fn pos(&self) -> Option<ferrokit_gui::geometry::primitives::Point> {
            crate::cursor_cocoa::cursor_pos(&crate::cursor_cocoa::NativeCursorServices)
        }

        fn set_pos(&mut self, pos: ferrokit_gui::geometry::primitives::Point) {
            crate::cursor_cocoa::set_cursor_pos(&crate::cursor_cocoa::NativeCursorServices, pos);
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
