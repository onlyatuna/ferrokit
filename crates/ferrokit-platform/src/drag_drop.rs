// SPDX-License-Identifier: LGPL-3.0-only
use ferrokit_gui::geometry::primitives::Point;
#[allow(unused_imports)]
use std::sync::atomic::{AtomicU32, Ordering};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DropAction {
    Ignore = 0,
    Copy = 1,
    Move = 2,
    Link = 4,
}

#[derive(Debug, Clone)]
pub enum DropEvent {
    Enter {
        pos: Point,
        formats: Vec<String>,
        effect: u32,
    },
    Over {
        pos: Point,
        effect: u32,
    },
    Leave,
    Drop {
        pos: Point,
        formats: Vec<String>,
        data: Vec<(String, Vec<u8>)>,
        effect: u32,
    },
}

/// Payload decoding shared by every backend that receives files or text.
pub mod payload {
    /// MIME type reported for a list of dropped files (`CF_HDROP`).
    pub const URI_LIST: &str = "text/uri-list";
    /// MIME type reported for dropped text (`CF_UNICODETEXT`).
    pub const PLAIN_TEXT: &str = "text/plain";

    /// The paths in a `DROPFILES` block (`CF_HDROP`): a header (`pFiles` offset, `POINT`, `fNC`,
    /// `fWide`) followed by NUL-separated, double-NUL-terminated names. Malformed blocks yield
    /// what could be read; nothing is read out of bounds.
    pub fn parse_hdrop(bytes: &[u8]) -> Vec<String> {
        const HEADER: usize = 20;
        if bytes.len() < HEADER {
            return Vec::new();
        }
        let offset = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
        let wide = i32::from_le_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]) != 0;
        let Some(list) = bytes.get(offset.max(HEADER)..) else {
            return Vec::new();
        };
        let mut paths = Vec::new();
        if wide {
            let units: Vec<u16> = list
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            for name in units.split(|&u| u == 0) {
                if name.is_empty() {
                    break;
                }
                paths.push(String::from_utf16_lossy(name));
            }
        } else {
            for name in list.split(|&b| b == 0) {
                if name.is_empty() {
                    break;
                }
                paths.push(String::from_utf8_lossy(name).into_owned());
            }
        }
        paths
    }

    /// `QUrl::fromLocalFile(path).toEncoded()` for a Windows path: `C:\a b` becomes
    /// `file:///C:/a%20b`, `\\server\share\f` becomes `file://server/share/f`.
    pub fn file_uri(path: &str) -> String {
        let normalized = path.replace('\\', "/");
        let (authority, rest) = match normalized.strip_prefix("//") {
            Some(unc) => match unc.split_once('/') {
                Some((host, rest)) => (host.to_string(), format!("/{rest}")),
                None => (unc.to_string(), String::new()),
            },
            None if normalized.starts_with('/') => (String::new(), normalized),
            None => (String::new(), format!("/{normalized}")),
        };
        let mut out = String::from("file://");
        out.push_str(&authority);
        for b in rest.bytes() {
            if b.is_ascii_alphanumeric() || b"-._~/:".contains(&b) {
                out.push(b as char);
            } else {
                out.push_str(&format!("%{b:02X}"));
            }
        }
        out
    }

    /// The `text/uri-list` payload (CRLF separated, as RFC 2483 and Qt use).
    pub fn uri_list(paths: &[String]) -> Vec<u8> {
        paths
            .iter()
            .map(|p| file_uri(p))
            .collect::<Vec<_>>()
            .join("\r\n")
            .into_bytes()
    }

    /// UTF-8 text from a `CF_UNICODETEXT` block (NUL terminated UTF-16).
    pub fn unicode_text(bytes: &[u8]) -> String {
        let units: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .take_while(|&u| u != 0)
            .collect();
        String::from_utf16_lossy(&units)
    }
}

#[cfg(windows)]
pub mod win32_ole {
    use super::*;
    use std::ffi::c_void;
    use windows_sys::Win32::Foundation::{HWND, POINT, POINTL, S_OK};
    use windows_sys::Win32::Graphics::Gdi::ScreenToClient;
    use windows_sys::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
    use windows_sys::Win32::System::Ole::{
        CF_HDROP, CF_UNICODETEXT, DROPEFFECT_COPY, DROPEFFECT_LINK, DROPEFFECT_MOVE,
        DROPEFFECT_NONE, RegisterDragDrop, RevokeDragDrop,
    };

    pub type HRESULT = i32;

    /// Drop targets created and not yet freed (leak detection in tests).
    #[cfg(any(test, feature = "test-hooks"))]
    pub static LIVE_DROP_TARGETS: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);

    const E_NOINTERFACE: HRESULT = 0x8000_4002_u32 as i32;
    const MK_SHIFT: u32 = 0x0004;
    const MK_CONTROL: u32 = 0x0008;
    const MK_ALT: u32 = 0x0020;
    /// `IID_IUnknown`
    pub const IID_IUNKNOWN: windows_sys::core::GUID =
        windows_sys::core::GUID::from_u128(0x00000000_0000_0000_c000_000000000046);
    /// `IID_IDropTarget`
    pub const IID_IDROPTARGET: windows_sys::core::GUID =
        windows_sys::core::GUID::from_u128(0x00000122_0000_0000_c000_000000000046);

    #[repr(C)]
    pub struct IDropTargetVtbl {
        pub query_interface: unsafe extern "system" fn(
            *mut c_void,
            *const windows_sys::core::GUID,
            *mut *mut c_void,
        ) -> HRESULT,
        pub add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
        pub release: unsafe extern "system" fn(*mut c_void) -> u32,
        pub drag_enter:
            unsafe extern "system" fn(*mut c_void, *mut c_void, u32, POINTL, *mut u32) -> HRESULT,
        pub drag_over: unsafe extern "system" fn(*mut c_void, u32, POINTL, *mut u32) -> HRESULT,
        pub drag_leave: unsafe extern "system" fn(*mut c_void) -> HRESULT,
        pub drop:
            unsafe extern "system" fn(*mut c_void, *mut c_void, u32, POINTL, *mut u32) -> HRESULT,
    }

    #[repr(C)]
    pub struct OleDropTarget {
        pub vtbl: *const IDropTargetVtbl,
        pub ref_count: AtomicU32,
        pub hwnd: HWND,
        /// Returns the `DROPEFFECT_*` the target accepts the event with (it may differ from the
        /// proposed `effect`); `DROPEFFECT_NONE` refuses. The result is limited to the effects the
        /// source allows.
        pub callback: Box<dyn Fn(DropEvent) -> u32 + Send + Sync>,
        /// Whether the drag currently over the window carries a format this target can decode.
        pub accepts: std::sync::atomic::AtomicBool,
    }

    impl OleDropTarget {
        pub fn new<F>(hwnd: HWND, callback: F) -> *mut Self
        where
            F: Fn(DropEvent) -> u32 + Send + Sync + 'static,
        {
            #[cfg(any(test, feature = "test-hooks"))]
            LIVE_DROP_TARGETS.fetch_add(1, Ordering::SeqCst);
            let target = Box::new(Self {
                vtbl: &OLE_DROP_TARGET_VTBL,
                ref_count: AtomicU32::new(1),
                hwnd,
                callback: Box::new(callback),
                accepts: std::sync::atomic::AtomicBool::new(false),
            });
            Box::into_raw(target)
        }
    }

    fn same_guid(a: &windows_sys::core::GUID, b: &windows_sys::core::GUID) -> bool {
        a.data1 == b.data1 && a.data2 == b.data2 && a.data3 == b.data3 && a.data4 == b.data4
    }

    /// `IUnknown::QueryInterface` of [`OleDropTarget`].
    ///
    /// # Safety
    /// `this` must be a live `OleDropTarget` the caller holds a reference to; `riid` must be null
    /// or point to a GUID and `ppv_object` null or point to a writable pointer slot, as the
    /// `QueryInterface` contract has it.
    unsafe extern "system" fn query_interface(
        this: *mut c_void,
        riid: *const windows_sys::core::GUID,
        ppv_object: *mut *mut c_void,
    ) -> HRESULT {
        // SAFETY: COM QueryInterface implementation; writes interface pointer if supported.
        unsafe {
            if ppv_object.is_null() {
                return E_NOINTERFACE;
            }
            // SAFETY: `ppv_object` is non-null (checked above); per the `QueryInterface` contract it
            // points to a writable pointer slot, valid for the duration of the call.
            *ppv_object = std::ptr::null_mut();
            if riid.is_null() {
                return E_NOINTERFACE;
            }
            // SAFETY: `riid` is non-null (checked above); per the `QueryInterface` contract it points
            // to a GUID valid for the duration of the call.
            let riid = &*riid;
            // Only the two interfaces this object implements: answering anything else would let OLE
            // call foreign vtable slots (IMarshal, IDropTargetHelper, ...) on an `IDropTarget` vtable.
            if !same_guid(riid, &IID_IUNKNOWN) && !same_guid(riid, &IID_IDROPTARGET) {
                return E_NOINTERFACE;
            }
            add_ref(this);
            *ppv_object = this;
            S_OK
        }
    }

    impl Drop for OleDropTarget {
        fn drop(&mut self) {
            #[cfg(any(test, feature = "test-hooks"))]
            LIVE_DROP_TARGETS.fetch_sub(1, Ordering::SeqCst);
        }
    }

    /// `IUnknown::AddRef` of [`OleDropTarget`].
    ///
    /// # Safety
    /// `this` must be a live `OleDropTarget` the caller holds a reference to.
    unsafe extern "system" fn add_ref(this: *mut c_void) -> u32 {
        // SAFETY: `this` is a live `OleDropTarget` while any reference is outstanding.
        unsafe {
            let target = this as *mut OleDropTarget;
            // SAFETY: `this` is a live `OleDropTarget` while any reference is outstanding; the caller
            // owns one, so the allocation has not been freed. Only the atomic `ref_count` is touched.
            (*target).ref_count.fetch_add(1, Ordering::SeqCst) + 1
        }
    }

    /// `IUnknown::Release` of [`OleDropTarget`]; frees the target with the last reference.
    ///
    /// # Safety
    /// `this` must be a live `OleDropTarget` and the caller must own the reference it gives up;
    /// it must not use `this` afterwards through that reference.
    unsafe extern "system" fn release(this: *mut c_void) -> u32 {
        // SAFETY: caller guarantees `this` is a live `OleDropTarget` and owns the reference given up.
        unsafe {
            let target = this as *mut OleDropTarget;
            // SAFETY: `this` is a live `OleDropTarget` while any reference is outstanding; the caller
            // (OLE or `DropTargetRegistration::drop`) owns one, so the allocation has not been freed.
            let count = (*target).ref_count.fetch_sub(1, Ordering::SeqCst) - 1;
            if count == 0 {
                // SAFETY: the count reached 0, so this was the last reference; `target` came from
                // `Box::into_raw` in `OleDropTarget::new`, and no one else can use it after this.
                let _ = Box::from_raw(target);
            }
            count
        }
    }
    #[repr(C)]
    struct FormatEtc {
        cf_format: u16,
        ptd: *mut c_void,
        dw_aspect: u32,
        lindex: i32,
        tymed: u32,
    }

    #[repr(C)]
    struct StgMedium {
        tymed: u32,
        h_global: *mut c_void,
        p_unk_for_release: *mut c_void,
    }

    #[repr(C)]
    pub struct IDataObjectVtbl {
        pub query_interface: usize,
        pub add_ref: usize,
        pub release: usize,
        pub get_data: unsafe extern "system" fn(*mut c_void, *const c_void, *mut c_void) -> HRESULT,
        pub get_data_here: usize,
        pub query_get_data: unsafe extern "system" fn(*mut c_void, *const c_void) -> HRESULT,
        pub get_canonical_format_etc: usize,
        pub set_data: usize,
        pub enum_format_etc: usize,
        pub d_advise: usize,
        pub d_unadvise: usize,
        pub enum_d_advise: usize,
    }

    const DVASPECT_CONTENT: u32 = 1;
    const TYMED_HGLOBAL: u32 = 1;

    #[link(name = "ole32")]
    unsafe extern "system" {
        fn ReleaseStgMedium(medium: *mut c_void);
    }

    fn format_etc(cf: u16) -> FormatEtc {
        FormatEtc {
            cf_format: cf,
            ptd: std::ptr::null_mut(),
            dw_aspect: DVASPECT_CONTENT,
            lindex: -1,
            tymed: TYMED_HGLOBAL,
        }
    }

    /// Whether `data_object` offers `cf` as global memory.
    ///
    /// # Safety
    /// `data_object` must be a live, non-null `IDataObject*`.
    unsafe fn has_format(data_object: *mut c_void, cf: u16) -> bool {
        unsafe {
            // SAFETY: `data_object` is a live `IDataObject*` (caller contract), whose first word is
            // its vtable pointer, so reading it as `*const *const IDataObjectVtbl` is a pointer read.
            let vtbl = *(data_object as *const *const IDataObjectVtbl);
            let fmt = format_etc(cf);
            ((*vtbl).query_get_data)(data_object, &fmt as *const _ as *const c_void) == S_OK
        }
    }

    /// The bytes of the `cf` global-memory block of `data_object`.
    ///
    /// # Safety
    /// `data_object` must be a live, non-null `IDataObject*`.
    unsafe fn read_format(data_object: *mut c_void, cf: u16) -> Option<Vec<u8>> {
        unsafe {
            // SAFETY: `data_object` is a live `IDataObject*` (caller contract), whose first word is
            // its vtable pointer, so reading it as `*const *const IDataObjectVtbl` is a pointer read.
            let vtbl = *(data_object as *const *const IDataObjectVtbl);
            let fmt = format_etc(cf);
            let mut medium = StgMedium {
                tymed: 0,
                h_global: std::ptr::null_mut(),
                p_unk_for_release: std::ptr::null_mut(),
            };
            let hr = ((*vtbl).get_data)(
                data_object,
                &fmt as *const _ as *const c_void,
                &mut medium as *mut _ as *mut c_void,
            );
            if hr != S_OK {
                return None;
            }
            let mut bytes = None;
            if medium.tymed == TYMED_HGLOBAL && !medium.h_global.is_null() {
                let size = GlobalSize(medium.h_global);
                let ptr = GlobalLock(medium.h_global) as *const u8;
                if !ptr.is_null() {
                    // SAFETY: `GlobalLock` succeeded (`ptr` non-null), so `ptr` addresses the locked
                    // block, which per the `GlobalSize` documentation is at least `size` bytes; the
                    // block stays locked (and owned by `medium`) until `GlobalUnlock` and
                    // `ReleaseStgMedium` below, after the bytes are copied out.
                    bytes = Some(std::slice::from_raw_parts(ptr, size).to_vec());
                    GlobalUnlock(medium.h_global);
                }
            }
            ReleaseStgMedium(&mut medium as *mut _ as *mut c_void);
            bytes
        }
    }

    /// The MIME types `data_object` can be decoded into.
    ///
    /// # Safety
    /// `data_object` must be a live `IDataObject`.
    pub unsafe fn data_object_formats(data_object: *mut c_void) -> Vec<String> {
        // SAFETY: caller guarantees data_object is null or a live IDataObject.
        unsafe {
            let mut formats = Vec::new();
            if data_object.is_null() {
                return formats;
            }
            if has_format(data_object, CF_HDROP) {
                formats.push(payload::URI_LIST.to_string());
            }
            if has_format(data_object, CF_UNICODETEXT) {
                formats.push(payload::PLAIN_TEXT.to_string());
            }
            formats
        }
    }

    /// The decoded payload of every format `data_object_formats` reports.
    ///
    /// # Safety
    /// `data_object` must be a live `IDataObject`.
    pub unsafe fn data_object_payload(data_object: *mut c_void) -> Vec<(String, Vec<u8>)> {
        // SAFETY: caller guarantees data_object is null or a live IDataObject.
        unsafe {
            let mut data = Vec::new();
            if data_object.is_null() {
                return data;
            }
            if let Some(bytes) = read_format(data_object, CF_HDROP) {
                data.push((
                    payload::URI_LIST.to_string(),
                    payload::uri_list(&payload::parse_hdrop(&bytes)),
                ));
            }
            if let Some(bytes) = read_format(data_object, CF_UNICODETEXT) {
                data.push((
                    payload::PLAIN_TEXT.to_string(),
                    payload::unicode_text(&bytes).into_bytes(),
                ));
            }
            data
        }
    }

    /// The effect a drag proposes (`QWindowsOleDropTarget::chosenEffect`): Ctrl+Shift or Alt
    /// chooses link, Shift chooses move, Ctrl chooses copy, and no modifier prefers copy, then
    /// move, then link; limited to what the source `allowed`. The caller writes the returned
    /// effect back to `*pdwEffect` only when the callback accepted the event, so `DROPEFFECT_MOVE`
    /// is never reported to the source unless a target accepted it.
    pub fn proposed_effect(key_state: u32, allowed: u32) -> u32 {
        let ctrl = key_state & MK_CONTROL != 0;
        let shift = key_state & MK_SHIFT != 0;
        let alt = key_state & MK_ALT != 0;
        let wanted = if (ctrl && shift) || alt {
            DROPEFFECT_LINK
        } else if shift {
            DROPEFFECT_MOVE
        } else if ctrl {
            DROPEFFECT_COPY
        } else {
            0
        };
        if wanted != 0 && allowed & wanted != 0 {
            return wanted;
        }
        [DROPEFFECT_COPY, DROPEFFECT_MOVE, DROPEFFECT_LINK]
            .into_iter()
            .find(|e| allowed & e != 0)
            .unwrap_or(DROPEFFECT_NONE)
    }

    /// The window-client logical position of a screen point of the drag.
    ///
    /// # Safety
    /// No obligation beyond passing a window handle: `hwnd` reaches only `ScreenToClient` and the
    /// DPI lookup, which validate it.
    unsafe fn logical_client_pos(hwnd: HWND, pt: POINTL) -> Point {
        // SAFETY: hwnd and pt are handled via ScreenToClient which validates the window handle.
        unsafe {
            let mut native = POINT { x: pt.x, y: pt.y };
            ScreenToClient(hwnd, &mut native);
            crate::high_dpi::from_native_point(
                Point::new(native.x, native.y),
                crate::window::get_window_dpr(hwnd),
            )
        }
    }

    /// The effect proposed for `*pdw_effect`'s allowed effects; none without decodable formats.
    ///
    /// # Safety
    /// `pdw_effect` must be null or point to a readable `u32` (the `IDropTarget` `pdwEffect`).
    unsafe fn allowed_effect(pdw_effect: *const u32, has_formats: bool, key_state: u32) -> u32 {
        // SAFETY: caller guarantees pdw_effect is null or points to readable u32.
        unsafe {
            if pdw_effect.is_null() || !has_formats {
                return DROPEFFECT_NONE;
            }
            // SAFETY: `pdw_effect` is non-null (checked above); per the `IDropTarget` documentation it
            // points to the DWORD of allowed effects, valid for the duration of the call.
            proposed_effect(key_state, *pdw_effect)
        }
    }

    /// Writes `effect` to `*pdw_effect` unless it is null.
    ///
    /// # Safety
    /// `pdw_effect` must be null or point to a writable `u32` (the `IDropTarget` `pdwEffect`).
    unsafe fn write_effect(pdw_effect: *mut u32, effect: u32) {
        // SAFETY: caller guarantees pdw_effect is null or points to writable u32.
        unsafe {
            if !pdw_effect.is_null() {
                // SAFETY: `pdw_effect` is non-null (checked above); per the `IDropTarget` documentation
                // it points to a writable DWORD, valid for the duration of the call.
                *pdw_effect = effect;
            }
        }
    }

    /// The single effect reported to the source: the callback's reply limited to what the source
    /// allowed (`allowed`, the incoming `*pdwEffect`), reduced to one `DROPEFFECT_*` bit (copy,
    /// then move, then link), and none without decodable formats.
    fn reported_effect(has_formats: bool, reply: u32, allowed: u32) -> u32 {
        if !has_formats {
            return DROPEFFECT_NONE;
        }
        let mask = reply & allowed;
        [DROPEFFECT_COPY, DROPEFFECT_MOVE, DROPEFFECT_LINK]
            .into_iter()
            .find(|e| mask & e != 0)
            .unwrap_or(DROPEFFECT_NONE)
    }

    /// The effects the source allows: `*pdw_effect`, none when it is null.
    ///
    /// # Safety
    /// `pdw_effect` must be null or point to a readable `u32` (the `IDropTarget` `pdwEffect`).
    unsafe fn incoming_effects(pdw_effect: *const u32) -> u32 {
        // SAFETY: caller guarantees pdw_effect is null or points to readable u32.
        unsafe {
            if pdw_effect.is_null() {
                DROPEFFECT_NONE
            } else {
                // SAFETY: `pdw_effect` is non-null (checked above); per the `IDropTarget` documentation
                // it points to the DWORD of allowed effects, valid for the duration of the call.
                *pdw_effect
            }
        }
    }

    /// `IDropTarget::DragEnter`.
    ///
    /// # Safety
    /// As OLE calls it: `this` must be a live `OleDropTarget` the caller holds a reference to,
    /// `p_data_obj` null or a live `IDataObject*`, and `pdw_effect` null or a valid `u32` for the
    /// call.
    unsafe extern "system" fn drag_enter(
        this: *mut c_void,
        p_data_obj: *mut c_void,
        grf_key_state: u32,
        pt: POINTL,
        pdw_effect: *mut u32,
    ) -> HRESULT {
        // SAFETY: `this` is our live `OleDropTarget` held by OLE's reference; p_data_obj is valid.
        let (target, formats) = unsafe {
            (
                &*(this as *mut OleDropTarget),
                data_object_formats(p_data_obj),
            )
        };
        let has_formats = !formats.is_empty();
        target.accepts.store(has_formats, Ordering::SeqCst);
        // SAFETY: pdw_effect is provided by OLE per IDropTarget contract; pt is handled via ScreenToClient.
        let (allowed, proposed, pos) = unsafe {
            (
                incoming_effects(pdw_effect),
                allowed_effect(pdw_effect, has_formats, grf_key_state),
                logical_client_pos(target.hwnd, pt),
            )
        };
        // Python/PySide6 contains a raising handler; unwinding out of an extern "system" fn aborts.
        let reply = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            (target.callback)(DropEvent::Enter {
                pos,
                formats,
                effect: proposed,
            })
        }))
        .unwrap_or(DROPEFFECT_NONE);
        // SAFETY: writes the agreed effect to pdw_effect.
        unsafe { write_effect(pdw_effect, reported_effect(has_formats, reply, allowed)) };
        S_OK
    }

    /// `IDropTarget::DragOver`.
    ///
    /// # Safety
    /// As OLE calls it: `this` must be a live `OleDropTarget` the caller holds a reference to,
    /// and `pdw_effect` null or a valid `u32` for the call.
    unsafe extern "system" fn drag_over(
        this: *mut c_void,
        grf_key_state: u32,
        pt: POINTL,
        pdw_effect: *mut u32,
    ) -> HRESULT {
        // SAFETY: `this` is our live `OleDropTarget` held by OLE's reference; only a shared reference is formed.
        let target = unsafe { &*(this as *mut OleDropTarget) };
        let has_formats = target.accepts.load(Ordering::SeqCst);
        // SAFETY: pdw_effect is provided by OLE per IDropTarget::DragOver contract; pt is handled via ScreenToClient.
        let (allowed, proposed, pos) = unsafe {
            (
                incoming_effects(pdw_effect),
                allowed_effect(pdw_effect, has_formats, grf_key_state),
                logical_client_pos(target.hwnd, pt),
            )
        };
        let reply = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            (target.callback)(DropEvent::Over {
                pos,
                effect: proposed,
            })
        }))
        .unwrap_or(DROPEFFECT_NONE);
        // SAFETY: writes the agreed effect to pdw_effect.
        unsafe { write_effect(pdw_effect, reported_effect(has_formats, reply, allowed)) };
        S_OK
    }

    /// `IDropTarget::DragLeave`.
    ///
    /// # Safety
    /// As OLE calls it: `this` must be a live `OleDropTarget` the caller holds a reference to.
    unsafe extern "system" fn drag_leave(this: *mut c_void) -> HRESULT {
        // SAFETY: `this` is our live `OleDropTarget` held by OLE's reference; only a shared reference is formed.
        let target = unsafe { &*(this as *mut OleDropTarget) };
        target.accepts.store(false, Ordering::SeqCst);
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            (target.callback)(DropEvent::Leave)
        }));
        S_OK
    }

    /// `IDropTarget::Drop`.
    ///
    /// # Safety
    /// As OLE calls it: `this` must be a live `OleDropTarget` the caller holds a reference to,
    /// `p_data_obj` null or a live `IDataObject*`, and `pdw_effect` null or a valid `u32` for the
    /// call.
    unsafe extern "system" fn drop(
        this: *mut c_void,
        p_data_obj: *mut c_void,
        grf_key_state: u32,
        pt: POINTL,
        pdw_effect: *mut u32,
    ) -> HRESULT {
        // SAFETY: `this` is the live OleDropTarget; p_data_obj is null or live IDataObject.
        let (target, formats, data) = unsafe {
            (
                &*(this as *mut OleDropTarget),
                data_object_formats(p_data_obj),
                data_object_payload(p_data_obj),
            )
        };
        target.accepts.store(false, Ordering::SeqCst);
        let has_formats = !formats.is_empty();
        // SAFETY: pdw_effect is provided by OLE per IDropTarget::Drop contract; pt is handled via ScreenToClient.
        let (allowed, proposed, pos) = unsafe {
            (
                incoming_effects(pdw_effect),
                allowed_effect(pdw_effect, has_formats, grf_key_state),
                logical_client_pos(target.hwnd, pt),
            )
        };
        let reply = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            (target.callback)(DropEvent::Drop {
                pos,
                formats,
                data,
                effect: proposed,
            })
        }))
        .unwrap_or(DROPEFFECT_NONE);
        // SAFETY: writes the agreed effect to pdw_effect.
        unsafe { write_effect(pdw_effect, reported_effect(has_formats, reply, allowed)) };
        S_OK
    }

    static OLE_DROP_TARGET_VTBL: IDropTargetVtbl = IDropTargetVtbl {
        query_interface,
        add_ref,
        release,
        drag_enter,
        drag_over,
        drag_leave,
        drop,
    };

    /// A drop target registered for a window with `RegisterDragDrop`. Dropping it revokes the
    /// registration (`RevokeDragDrop`, which releases OLE's reference) and releases this side's
    /// reference, so the target is freed once neither OLE nor a running drag holds it.
    ///
    /// OLE's reference can only be released through `RevokeDragDrop` on a live window: once the
    /// window is destroyed the call fails with `DRAGDROP_E_INVALIDHWND` and OLE keeps its
    /// reference for good. Ferrokit windows therefore also revoke in `WM_DESTROY`
    /// ([`revoke_on_destroy`]), as MFC's `COleDropTarget` revokes when its window is destroyed, so
    /// a window destroyed other than through `close()` (for example with its parent) does not leak
    /// the target.
    pub struct DropTargetRegistration {
        hwnd: HWND,
        target: *mut OleDropTarget,
    }

    /// The windows with a live registration made here, by handle, with the target registered for
    /// each. `revoke_on_destroy` takes a window out when it revokes, so a later
    /// `DropTargetRegistration` drop knows not to revoke again: by then the handle may name a new
    /// window whose own registration must stay. Process-wide (not per thread) because
    /// `register_drop_target` accepts any window handle.
    static REGISTERED: std::sync::Mutex<Vec<(usize, usize)>> = std::sync::Mutex::new(Vec::new());

    /// Takes `hwnd` out of [`REGISTERED`] when it is registered with `target` (or with any target,
    /// for `None`); returns whether it was.
    fn unregister(hwnd: HWND, target: Option<*mut OleDropTarget>) -> bool {
        let mut registered = REGISTERED.lock().unwrap_or_else(|e| e.into_inner());
        let found = registered.iter().position(|&(h, t)| {
            h == hwnd as usize && target.is_none_or(|target| t == target as usize)
        });
        found.map(|i| registered.swap_remove(i)).is_some()
    }

    impl Drop for DropTargetRegistration {
        fn drop(&mut self) {
            // Still registered: revoke now. Otherwise `revoke_on_destroy` revoked while the window
            // existed (OLE released its reference then), and the handle may already name another
            // window, whose registration a second `RevokeDragDrop` would take away.
            if unregister(self.hwnd, Some(self.target)) {
                // A failed revocation is deliberately not answered with a release: OLE then holds
                // no reference this side may give up. After a destruction without
                // `revoke_on_destroy` (a foreign window class) the reference belongs to OLE's lost
                // registration, and releasing it here would free the target while OLE still
                // points at it.
                // SAFETY: `RevokeDragDrop` takes any handle value and validates it.
                unsafe {
                    RevokeDragDrop(self.hwnd);
                }
            }
            // Only this side's own reference is released, which never frees memory OLE still uses.
            // SAFETY: `target` came from `OleDropTarget::new` in `register_drop_target`, which
            // handed its one reference to this registration; it is released exactly once, here.
            unsafe {
                release(self.target as *mut c_void);
            }
        }
    }

    /// Revokes `hwnd`'s drop-target registration, if it has one made here, while the window still
    /// exists. Called from `WM_DESTROY`; the later `DropTargetRegistration` drop then skips its
    /// own revocation.
    pub(crate) fn revoke_on_destroy(hwnd: HWND) {
        if unregister(hwnd, None) {
            // SAFETY: `RevokeDragDrop` takes any handle value and validates it; `hwnd` is the
            // window being destroyed, which still exists during `WM_DESTROY`.
            unsafe {
                RevokeDragDrop(hwnd);
            }
        }
    }

    // `hwnd` is only passed to OLE, which validates it (an invalid window fails the registration);
    // nothing here dereferences it.
    #[allow(clippy::not_unsafe_ptr_arg_deref)]
    pub fn register_drop_target<F>(
        hwnd: HWND,
        callback: F,
    ) -> Result<DropTargetRegistration, &'static str>
    where
        F: Fn(DropEvent) -> u32 + Send + Sync + 'static,
    {
        use windows_sys::Win32::System::Ole::OleInitialize;
        // SAFETY: `OleInitialize` takes a reserved null pointer, as documented. `target_ptr` comes
        // from `Box::into_raw` in `OleDropTarget::new` just above with `ref_count` 1 and is not shared
        // yet, and `target_ptr as *mut _` is an `IDropTarget*` because `vtbl` is the first `repr(C)`
        // field, pointing at `OLE_DROP_TARGET_VTBL`. On success OLE takes its own reference and ours
        // moves into the returned registration; on failure OLE took none, so the sole reference is
        // freed here with `Box::from_raw` and never used again. `hwnd` is validated by OLE.
        unsafe {
            OleInitialize(std::ptr::null_mut());
            let target_ptr = OleDropTarget::new(hwnd, callback);
            let hr = RegisterDragDrop(hwnd, target_ptr as *mut _);
            if hr == S_OK {
                // `RegisterDragDrop` succeeded, so no registration of this handle is live: an
                // entry left for it belongs to a destroyed window whose handle was reused.
                {
                    let mut registered = REGISTERED.lock().unwrap_or_else(|e| e.into_inner());
                    registered.retain(|&(h, _)| h != hwnd as usize);
                    registered.push((hwnd as usize, target_ptr as usize));
                }
                Ok(DropTargetRegistration {
                    hwnd,
                    target: target_ptr,
                })
            } else {
                let _ = Box::from_raw(target_ptr);
                Err("RegisterDragDrop failed")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::payload::*;

    #[test]
    fn malformed_drop_files_blocks_never_read_out_of_bounds() {
        assert!(parse_hdrop(&[]).is_empty());
        assert!(parse_hdrop(&[0; 19]).is_empty());
        // pFiles points past the end.
        let mut b = vec![0u8; 20];
        b[..4].copy_from_slice(&1000u32.to_le_bytes());
        assert!(parse_hdrop(&b).is_empty());
        // Unterminated wide name with an odd trailing byte is still bounded.
        let mut b = vec![0u8; 20];
        b[..4].copy_from_slice(&20u32.to_le_bytes());
        b[16] = 1;
        b.extend_from_slice(&[b'a', 0, b'b', 0, b'c']);
        assert_eq!(parse_hdrop(&b), vec!["ab".to_string()]);
    }

    #[test]
    fn ansi_drop_files_are_decoded() {
        let mut b = vec![0u8; 20];
        b[..4].copy_from_slice(&20u32.to_le_bytes());
        b.extend_from_slice(b"C:\\x.txt\0D:\\y.txt\0\0");
        assert_eq!(parse_hdrop(&b), vec!["C:\\x.txt", "D:\\y.txt"]);
    }

    #[test]
    fn file_uris_percent_encode_non_ascii_and_reserved_bytes() {
        assert_eq!(
            file_uri("C:\\a b\\é#.txt"),
            "file:///C:/a%20b/%C3%A9%23.txt"
        );
        assert_eq!(file_uri("\\\\srv\\share\\f"), "file://srv/share/f");
    }

    #[test]
    fn unicode_text_stops_at_the_terminator() {
        let bytes: Vec<u8> = "hi\0junk"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(unicode_text(&bytes), "hi");
    }
}
