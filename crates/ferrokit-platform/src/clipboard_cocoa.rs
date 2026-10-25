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
#[derive(Clone)]
pub struct CocoaClipboard {
    state: Arc<ChangeState>,
}

impl CocoaClipboard {
    /// The system general pasteboard; every instance shares one `dataChanged` signal.
    #[cfg(target_os = "macos")]
    pub fn new() -> Self {
        static SHARED: LazyLock<Arc<ChangeState>> =
            LazyLock::new(|| ChangeState::new(Arc::new(native::NsPasteboard::new())));
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

    /// `QClipboard::dataChanged`. Emitted by [`poll_changes`](Self::poll_changes) and after every
    /// write made through this clipboard.
    pub fn data_changed(&self) -> &Signal<()> {
        &self.state.data_changed
    }

    /// `-[NSPasteboard changeCount]`.
    pub fn change_count(&self) -> isize {
        self.state.backend.change_count()
    }

    /// Emits [`data_changed`](Self::data_changed) when another application (or this one) changed
    /// the pasteboard since the last check; returns whether it did. AppKit has no change
    /// notification, so the owner of the event loop calls this (Qt checks on application
    /// activation).
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
            // SAFETY: push/pop are plain libobjc calls; the guard pops exactly what it pushed,
            // on the same thread (`Pool` is `!Send` through its raw pointer).
            Self(unsafe { objc_autoreleasePoolPush() })
        }
    }

    impl Drop for Pool {
        fn drop(&mut self) {
            // SAFETY: `self.0` came from `objc_autoreleasePoolPush` on this thread and pools are
            // dropped in reverse order of creation.
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
            // SAFETY: `nsstring_from_str` returns an owned (+1) string, released exactly once
            // here; sending `release` to nil is a no-op.
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
        // SAFETY: `UTF8String` is a NUL-terminated buffer valid while `string` lives; it is
        // copied before the caller's autorelease pool drains.
        Some(CStr::from_ptr(utf8).to_string_lossy().into_owned())
    }

    /// `[NSPasteboard generalPasteboard]` as a [`PasteboardBackend`].
    ///
    /// `NSPasteboard` is not documented as thread safe and the trait is `Send + Sync`, so every
    /// operation takes `lock`.
    pub struct NsPasteboard {
        lock: Mutex<()>,
    }

    impl NsPasteboard {
        pub fn new() -> Self {
            Self {
                lock: Mutex::new(()),
            }
        }

        /// The general pasteboard singleton (autoreleased, so only valid inside the caller's
        /// [`Pool`]).
        fn general() -> Id {
            // SAFETY: class method with no arguments on `NSPasteboard`, returning an object.
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
            // SAFETY: `changeCount` takes no argument and returns `NSInteger`, the register-sized
            // integer `send_usize_return` reads (same ABI as `NSUInteger`).
            let count = unsafe {
                ObjcMsg::send_usize_return(Self::general(), Sel::register("changeCount"))
            };
            count as isize
        }

        fn clear_contents(&self) -> isize {
            let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
            let _pool = Pool::new();
            // SAFETY: `clearContents` takes no argument and returns `NSInteger`; `send_0` reads
            // the return register as a pointer-sized value.
            unsafe { ObjcMsg::send_0(Self::general(), Sel::register("clearContents")).0 as isize }
        }

        fn types(&self) -> Vec<String> {
            let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
            let _pool = Pool::new();
            let mut out = Vec::new();
            // SAFETY: `types` returns an autoreleased `NSArray<NSString *>` (nil when empty);
            // `count` and `objectAtIndex:` stay within its bounds, the strings are copied before
            // `_pool` drains, and nothing is retained or released here.
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
            // SAFETY: `dataForType:` takes one `NSString` and returns an autoreleased `NSData` or
            // nil. `bytes` points at `length` bytes owned by that `NSData`, which outlives the
            // copy because `_pool` drains after this block.
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
            // SAFETY: `+[NSData dataWithBytes:length:]` copies `data.len()` bytes from a valid
            // slice and returns an autoreleased object, so it needs no release; the transmuted
            // signatures match `dataWithBytes:length:` (`id, SEL, const void *, NSUInteger -> id`)
            // and `setData:forType:` (`id, SEL, id, id -> BOOL`), which the pasteboard
            // copies/retains.
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
