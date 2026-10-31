// SPDX-License-Identifier: LGPL-3.0-only
//! The macOS clipboard: `[NSPasteboard generalPasteboard]` behind [`PasteboardBackend`].
//!
//! [`CocoaClipboard`] holds the Qt semantics (MIME <-> pasteboard type mapping, clear-then-write,
//! `changeCount` based change detection); the backend is only the five `NSPasteboard` calls it
//! needs. `NsPasteboard` (macOS only) sends them through `ObjcMsg`; tests plug in a fake backend,
//! so the call sequence is checked on any OS.

use crate::clipboard::{MimeData, PlatformClipboard};
use ferrokit_core::signal::Signal;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::Arc;
#[cfg(target_os = "macos")]
use std::sync::LazyLock;

/// The `NSPasteboard` operations the clipboard uses. Types are pasteboard type strings (UTIs such
/// as `public.utf8-plain-text`).
pub trait PasteboardBackend: Send + Sync {
    /// `-[NSPasteboard changeCount]`: grows whenever the pasteboard owner changes.
    fn change_count(&self) -> isize;
    /// `-[NSPasteboard clearContents]`: drops the contents and makes the caller the owner.
    /// Returns the new change count.
    fn clear_contents(&self) -> isize;
    /// `-[NSPasteboard types]`, in the owner's preference order.
    fn types(&self) -> Vec<String>;
    /// `-[NSPasteboard dataForType:]`; `None` when the type is absent.
    fn data_for_type(&self, ty: &str) -> Option<Vec<u8>>;
    /// `-[NSPasteboard setData:forType:]`; `false` when AppKit refuses.
    fn set_data_for_type(&self, data: &[u8], ty: &str) -> bool;
}

/// Prefix Qt gives pasteboard types for MIME types without a system type
/// (`QMacPasteboardMimeAny`, qmacmime.mm): `com.trolltech.anymime.` + the MIME type with `/`
/// replaced by `--`.
const ANY_MIME_PREFIX: &str = "com.trolltech.anymime.";

/// MIME type <-> system pasteboard type (`NSPasteboardTypeString`, `NSPasteboardTypeHTML`,
/// `NSPasteboardTypePNG`, `NSPasteboardTypeTIFF` and the JPEG UTI).
const SYSTEM_TYPES: [(&str, &str); 5] = [
    ("text/plain", "public.utf8-plain-text"),
    ("text/html", "public.html"),
    ("image/png", "public.png"),
    ("image/tiff", "public.tiff"),
    ("image/jpeg", "public.jpeg"),
];

/// Pasteboard types read as `text/uri-list`, best first (`NSPasteboardTypeFileURL`,
/// `NSPasteboardTypeURL`); the data is the URL as UTF-8 text.
const URL_TYPES: [&str; 2] = ["public.file-url", "public.url"];

/// Legacy `NSStringPboardType` ('NSStringPboardType', historically 'NXStringPboardType').
const LEGACY_STRING_TYPES: [&str; 2] = ["NSStringPboardType", "NXStringPboardType"];

/// UTF-16 text (`public.utf16-plain-text`; native byte order on macOS, i.e. little endian unless
/// a byte order mark says otherwise) and legacy string types, as UTF-8 text. Legacy data that is
/// not valid UTF-8 (a system script encoding) is skipped.
fn fallback_text(backend: &dyn PasteboardBackend) -> Option<String> {
    if let Some(bytes) = backend.data_for_type("public.utf16-plain-text") {
        let big_endian = bytes.starts_with(&[0xFE, 0xFF]);
        let body = if big_endian || bytes.starts_with(&[0xFF, 0xFE]) {
            &bytes[2..]
        } else {
            &bytes[..]
        };
        let units: Vec<u16> = body
            .chunks(2)
            .filter(|pair| pair.len() == 2)
            .map(|pair| {
                let pair = [pair[0], pair[1]];
                if big_endian {
                    u16::from_be_bytes(pair)
                } else {
                    u16::from_le_bytes(pair)
                }
            })
            .collect();
        if let Ok(text) = String::from_utf16(&units) {
            return Some(text);
        }
    }
    LEGACY_STRING_TYPES
        .iter()
        .find_map(|ty| backend.data_for_type(ty))
        .and_then(|bytes| String::from_utf8(bytes).ok())
}

/// The pasteboard type a MIME type is stored under.
pub fn pasteboard_type_for_mime(mime: &str) -> String {
    SYSTEM_TYPES
        .iter()
        .find(|(m, _)| *m == mime)
        .map(|(_, ty)| (*ty).to_owned())
        .unwrap_or_else(|| format!("{ANY_MIME_PREFIX}{}", mime.replace('/', "--")))
}

/// The MIME type a pasteboard type carries; `None` for types ferrokit does not expose.
pub fn mime_for_pasteboard_type(ty: &str) -> Option<String> {
    if let Some((mime, _)) = SYSTEM_TYPES.iter().find(|(_, t)| *t == ty) {
        return Some((*mime).to_owned());
    }
    ty.strip_prefix(ANY_MIME_PREFIX)
        .filter(|rest| !rest.is_empty())
        .map(|rest| rest.replace("--", "/"))
}

/// Change-count bookkeeping shared by every [`CocoaClipboard`] on the same pasteboard.
struct ChangeState {
    backend: Arc<dyn PasteboardBackend>,
    last_seen: AtomicIsize,
    data_changed: Signal<()>,
}

impl ChangeState {
    fn new(backend: Arc<dyn PasteboardBackend>) -> Arc<Self> {
        let last_seen = AtomicIsize::new(backend.change_count());
        Arc::new(Self {
            backend,
            last_seen,
            data_changed: Signal::new(),
        })
    }

    fn poll(&self) -> bool {
        let current = self.backend.change_count();
        if self.last_seen.swap(current, Ordering::AcqRel) == current {
            return false;
        }
        self.data_changed.emit(&());
        true
    }
}

// The macOS-only shared instance lives in a `static`, so the state must be `Send + Sync`; check it
// on every OS.
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<ChangeState>();
};

/// `QCocoaClipboard`: the general pasteboard as a [`PlatformClipboard`].
///
/// AppKit has no pasteboard change notification, so outside changes are detected by comparing
/// `changeCount` in [`poll_changes`](Self::poll_changes) only.
#[derive(Clone)]
pub struct CocoaClipboard {
    state: Arc<ChangeState>,
}

#[cfg(target_os = "macos")]
impl Default for CocoaClipboard {
    fn default() -> Self {
        Self::new()
    }
}

impl CocoaClipboard {
    /// The system general pasteboard; every instance shares one `dataChanged` signal.
    #[cfg(target_os = "macos")]
    pub fn new() -> Self {
        static SHARED: LazyLock<Arc<ChangeState>> =
            LazyLock::new(|| ChangeState::new(Arc::new(native::NsPasteboard::default())));
        Self {
            state: Arc::clone(&SHARED),
        }
    }

    /// A clipboard on `backend` with its own change tracking.
    pub fn with_backend(backend: Arc<dyn PasteboardBackend>) -> Self {
        Self {
            state: ChangeState::new(backend),
        }
    }

    /// `QClipboard::dataChanged`. Emitted after every write or clear made through this clipboard
    /// and, for changes made by other applications, only when
    /// [`poll_changes`](Self::poll_changes) runs: on macOS the application delegate calls it from
    /// `applicationDidBecomeActive:` (as Qt does), so a copy made in another application is
    /// reported when the user switches back, not while this application stays in the background.
    pub fn data_changed(&self) -> &Signal<()> {
        &self.state.data_changed
    }

    /// `-[NSPasteboard changeCount]`.
    pub fn change_count(&self) -> isize {
        self.state.backend.change_count()
    }

    /// Emits [`data_changed`](Self::data_changed) when the pasteboard changed since the last
    /// check; returns whether it did. Called by the application delegate when the application
    /// becomes active; call it from a timer to report outside changes sooner.
    pub fn poll_changes(&self) -> bool {
        self.state.poll()
    }
}

impl PlatformClipboard for CocoaClipboard {
    fn set_mime_data(&self, data: &MimeData) -> Result<(), &'static str> {
        let backend = &self.state.backend;
        // `clearContents` first: it makes this process the owner, and `setData:forType:` is only
        // allowed by the owner.
        backend.clear_contents();
        let mut result = Ok(());
        for mime in data.formats() {
            let bytes = data.data(mime).unwrap_or_default();
            if !backend.set_data_for_type(bytes, &pasteboard_type_for_mime(mime)) {
                result = Err("NSPasteboard setData:forType: failed");
                break;
            }
        }
        self.state.poll();
        result
    }

    fn mime_data(&self) -> Result<MimeData, &'static str> {
        let backend = &self.state.backend;
        let mut mime_data = MimeData::new();
        for ty in backend.types() {
            let Some(mime) = mime_for_pasteboard_type(&ty) else {
                continue;
            };
            if mime_data.has_format(&mime) {
                continue;
            }
            if let Some(bytes) = backend.data_for_type(&ty) {
                mime_data.set_data(mime, bytes);
            }
        }
        // Types other applications put on the pasteboard that have a MIME counterpart but are
        // not stored by `set_mime_data`: used only when no better type supplied the MIME type.
        if !mime_data.has_format("text/plain") {
            if let Some(text) = fallback_text(backend.as_ref()) {
                mime_data.set_text(&text);
            }
        }
        if !mime_data.has_format("text/uri-list") {
            if let Some(url) = URL_TYPES.iter().find_map(|ty| backend.data_for_type(ty)) {
                mime_data.set_data("text/uri-list", url);
            }
        }
        Ok(mime_data)
    }

    fn clear(&self) -> Result<(), &'static str> {
        self.state.backend.clear_contents();
        self.state.poll();
        Ok(())
    }
}

#[cfg(target_os = "macos")]
mod native {
    use super::PasteboardBackend;
    use crate::objc_runtime::{
        nsstring_from_str, objc_get_class, Id, NSUInteger, ObjcMsg, Sel, BOOL,
    };
    use std::ffi::{c_void, CStr};
    use std::sync::Mutex;

    extern "C" {
        fn objc_msgSend();
        fn objc_autoreleasePoolPush() -> *mut c_void;
        fn objc_autoreleasePoolPop(pool: *mut c_void);
    }

    /// An autorelease pool for one pasteboard operation: `NSPasteboard` returns autoreleased
    /// `NSData`/`NSString`/`NSArray`, and the calling thread may not have a pool of its own.
    struct Pool(*mut c_void);

    impl Pool {
        fn new() -> Self {
            // SAFETY: `objc_autoreleasePoolPush` takes no arguments and returns the opaque token
            // that `objc_autoreleasePoolPop` expects; it is declared with that signature in the
            // `extern` block above and is available from any thread per the libobjc
            // documentation [inferred]. The token is stored in the returned guard, whose `Drop`
            // pops it exactly once, on the same thread (`Pool` holds a raw pointer, so it is
            // `!Send`).
            Self(unsafe { objc_autoreleasePoolPush() })
        }
    }

    impl Drop for Pool {
        fn drop(&mut self) {
            // SAFETY: `self.0` is the token this guard's `new` got from `objc_autoreleasePoolPush`
            // on this thread and is popped only here. Pools are popped in reverse creation order
            // because each guard is a local that drops before the guard declared before it (the
            // only nesting in this file is a single `Pool` per backend call).
            unsafe { objc_autoreleasePoolPop(self.0) };
        }
    }

    /// A +1 `NSString` released on drop.
    struct OwnedString(Id);

    impl OwnedString {
        fn new(text: &str) -> Self {
            Self(nsstring_from_str(text))
        }
    }

    impl Drop for OwnedString {
        fn drop(&mut self) {
            // SAFETY: `self.0` is the +1 string `nsstring_from_str` returned, released exactly
            // once here (`OwnedString` is neither `Clone` nor `Copy`); `release` takes no
            // arguments and sending it to nil is a no-op.
            unsafe { ObjcMsg::send_0(self.0, Sel::register("release")) };
        }
    }

    /// Copies an `NSString` (autoreleased or borrowed) into a Rust string.
    ///
    /// # Safety
    /// `string` is nil or a live `NSString`.
    unsafe fn rust_string(string: Id) -> Option<String> {
        let utf8 =
            ObjcMsg::send_0(string, Sel::register("UTF8String")).0 as *const std::ffi::c_char;
        if utf8.is_null() {
            return None;
        }
        // SAFETY: the caller guarantees `string` is nil (then `send_0` above returns nil and we
        // returned `None`) or a live NSString; `UTF8String` returns a NUL-terminated buffer
        // owned by `string` that stays valid while the caller's autorelease pool has not drained
        // (per the NSString documentation), and it is copied here before returning.
        Some(CStr::from_ptr(utf8).to_string_lossy().into_owned())
    }

    /// `[NSPasteboard generalPasteboard]` as a [`PasteboardBackend`].
    ///
    /// `NSPasteboard` is not documented as thread safe and the trait is `Send + Sync`, so every
    /// operation takes `lock`.
    #[derive(Default)]
    pub struct NsPasteboard {
        lock: Mutex<()>,
    }

    impl NsPasteboard {
        /// The general pasteboard singleton (autoreleased, so only valid inside the caller's
        /// [`Pool`]).
        fn general() -> Id {
            // SAFETY: `generalPasteboard` is a no-argument class method of `NSPasteboard`
            // returning an autoreleased object; it is only used inside the caller's `Pool`, which
            // every call site of `general()` holds, and the sends are serialized by `self.lock`.
            unsafe {
                ObjcMsg::send_class_0(
                    objc_get_class("NSPasteboard"),
                    Sel::register("generalPasteboard"),
                )
            }
        }
    }

    impl PasteboardBackend for NsPasteboard {
        fn change_count(&self) -> isize {
            let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
            let _pool = Pool::new();
            // SAFETY: `Self::general()` is the live pasteboard (inside `_pool`, under `_guard`);
            // `changeCount` takes no arguments and returns NSInteger, which `send_usize_return`
            // reads from the same integer return register (same ABI as NSUInteger) and the value
            // is reinterpreted as `isize`.
            let count = unsafe {
                ObjcMsg::send_usize_return(Self::general(), Sel::register("changeCount"))
            };
            count as isize
        }

        fn clear_contents(&self) -> isize {
            let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
            let _pool = Pool::new();
            // SAFETY: `Self::general()` is the live pasteboard (inside `_pool`, under `_guard`);
            // `clearContents` takes no arguments and returns NSInteger, which `send_0` reads from
            // the same pointer-sized integer return register and the value is cast to `isize`
            // without ever being dereferenced.
            unsafe { ObjcMsg::send_0(Self::general(), Sel::register("clearContents")).0 as isize }
        }

        fn types(&self) -> Vec<String> {
            let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
            let _pool = Pool::new();
            let mut out = Vec::new();
            // SAFETY: `Self::general()` is the live pasteboard (inside `_pool`, under `_guard`);
            // `types` returns an autoreleased NSArray of NSString or nil (a nil array makes `count`
            // return 0 and the loop does not run). `objectAtIndex:` takes one NSUInteger and is
            // called only for `index < count`, so it does not raise. Every item stays valid
            // until `_pool` drains, after this block, and `rust_string` copies it; nothing is
            // retained or released here.
            unsafe {
                let array = ObjcMsg::send_0(Self::general(), Sel::register("types"));
                let count = ObjcMsg::send_usize_return(array, Sel::register("count"));
                for index in 0..count {
                    let item =
                        ObjcMsg::send_id_with_usize(array, Sel::register("objectAtIndex:"), index);
                    if let Some(text) = rust_string(item) {
                        out.push(text);
                    }
                }
            }
            out
        }

        fn data_for_type(&self, ty: &str) -> Option<Vec<u8>> {
            let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
            let _pool = Pool::new();
            let ns_type = OwnedString::new(ty);
            // SAFETY: `dataForType:` takes one NSString (`ns_type`, the live +1 string created
            // above and released after this block) and returns an autoreleased NSData or nil, which
            // is checked. `length` and `bytes` take no arguments, so `bytes` points at `len`
            // bytes owned by that NSData, which stays valid until `_pool` drains (after this
            // block), and `from_raw_parts` is only called for `len > 0` with a non-null pointer.
            // The slice is read only, and copied by `to_vec` before the block ends.
            unsafe {
                let data =
                    ObjcMsg::send_id(Self::general(), Sel::register("dataForType:"), ns_type.0);
                if data.is_nil() {
                    return None;
                }
                let len = ObjcMsg::send_usize_return(data, Sel::register("length"));
                if len == 0 {
                    return Some(Vec::new());
                }
                let bytes = ObjcMsg::send_0(data, Sel::register("bytes")).0 as *const u8;
                if bytes.is_null() {
                    return None;
                }
                Some(std::slice::from_raw_parts(bytes, len).to_vec())
            }
        }

        fn set_data_for_type(&self, data: &[u8], ty: &str) -> bool {
            let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
            let _pool = Pool::new();
            let ns_type = OwnedString::new(ty);
            // SAFETY: the first transmute reinterprets `objc_msgSend` as the signature of
            // `+[NSData dataWithBytes:length:]` (receiver, selector, `const void *`, NSUInteger ->
            // id); `data.as_ptr()`/`data.len()` describe a live slice that the call copies, and
            // the result is autoreleased (valid until `_pool` drains, after this block) or nil,
            // which is checked. The second transmute reinterprets it as `-setData:forType:`
            // (receiver, selector, id, id -> BOOL); `pasteboard` is the live, non-nil general
            // pasteboard (inside `_pool`, under `_guard`), `ns_data` the non-nil NSData and
            // `ns_type.0` the live NSString, all valid for the call, which retains or copies
            // them. Both signatures are as the AppKit/Foundation documentation gives them
            // [inferred].
            unsafe {
                let make: extern "C" fn(
                    *mut c_void,
                    *const c_void,
                    *const u8,
                    NSUInteger,
                ) -> *mut c_void = std::mem::transmute(objc_msgSend as *const ());
                let ns_data = Id(make(
                    objc_get_class("NSData").0,
                    Sel::register("dataWithBytes:length:").0,
                    data.as_ptr(),
                    data.len(),
                ));
                if ns_data.is_nil() {
                    return false;
                }
                let set: extern "C" fn(
                    *mut c_void,
                    *const c_void,
                    *mut c_void,
                    *mut c_void,
                ) -> BOOL = std::mem::transmute(objc_msgSend as *const ());
                let pasteboard = Self::general();
                if pasteboard.is_nil() {
                    return false;
                }
                set(
                    pasteboard.0,
                    Sel::register("setData:forType:").0,
                    ns_data.0,
                    ns_type.0 .0,
                ) != 0
            }
        }
    }
}
