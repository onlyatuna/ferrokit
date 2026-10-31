// SPDX-License-Identifier: LGPL-3.0-only
//! Native macOS dialogs: `NSOpenPanel`/`NSSavePanel` and `NSAlert`, run modally.
//!
//! The dialog logic ([`CocoaDialogs`]) is written against the small [`DialogRuntime`] message
//! interface instead of calling Objective-C directly, so the exact message sequence (which
//! selectors, which arguments, which objects are released) is unit-tested on every OS with a
//! recording implementation. `NativeRuntime` (macOS only, crate-private) forwards the interface
//! to the Objective-C runtime through [`ObjcMsg`].
//!
//! Ownership rules the logic follows: every object it creates is either autoreleased by its
//! factory (`openPanel`, `savePanel`, `array`, `ns_string`) and lives until the enclosing
//! autorelease pool is popped, or is `alloc`ed/`init`ed (`NSAlert`) and explicitly released. The
//! whole operation runs inside one autorelease pool, which is popped even if a panic unwinds.
//! Everything is main-thread only (AppKit constraint); other threads panic before any message is
//! sent, like [`ferrokit_core::object::ThreadContext::assert_main_thread`].
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use std::path::{Path, PathBuf};

use crate::dialogs::{
    DialogIcon, FileFilter, MessageBoxButtons, MessageBoxResult, PlatformDialogs,
};
use crate::objc_runtime::{Id, NSInteger};

/// `NSModalResponseOK`: the user confirmed an `NSOpenPanel`/`NSSavePanel`.
pub const NS_MODAL_RESPONSE_OK: NSInteger = 1;
/// `NSAlertFirstButtonReturn`; later buttons count up from it.
pub const NS_ALERT_FIRST_BUTTON_RETURN: NSInteger = 1000;
/// `NSAlertStyleWarning` (the default style).
pub const NS_ALERT_STYLE_WARNING: NSInteger = 0;
/// `NSAlertStyleInformational`.
pub const NS_ALERT_STYLE_INFORMATIONAL: NSInteger = 1;
/// `NSAlertStyleCritical`.
pub const NS_ALERT_STYLE_CRITICAL: NSInteger = 2;

/// Messages the dialog logic sends. Selector names are the Objective-C ones (`"setMessage:"`);
/// receivers are objects, or class names for the `class_*` methods.
///
/// Implementations must be callable from any thread only up to [`require_main_thread`], which
/// panics off the main thread; the other methods may assume the main thread.
///
/// [`require_main_thread`]: DialogRuntime::require_main_thread
pub trait DialogRuntime {
    /// Panics with a message naming `operation` unless called on the main thread.
    fn require_main_thread(&self, operation: &str);
    /// Creates an `NSAutoreleasePool` (+1, owned by the caller until [`pool_pop`]).
    ///
    /// [`pool_pop`]: DialogRuntime::pool_pop
    fn pool_push(&self) -> Id;
    /// Drains and releases a pool made by [`pool_push`](DialogRuntime::pool_push).
    fn pool_pop(&self, pool: Id);
    /// `[NSApplication sharedApplication]`: connects the process to the window server and
    /// returns the application object (owned by AppKit).
    fn shared_application(&self) -> Id;
    /// `[Class sel]` returning an autoreleased object (`openPanel`, `savePanel`, `array`).
    fn class_msg(&self, class: &str, sel: &str) -> Id;
    /// `[Class sel:arg]` returning an autoreleased object (`fileURLWithPath:`).
    fn class_msg_id(&self, class: &str, sel: &str, arg: Id) -> Id;
    /// `[[Class alloc] init]`: a +1 object the caller must [`release`](DialogRuntime::release).
    fn alloc_init(&self, class: &str) -> Id;
    /// `[recv sel]` returning an object that `recv` (or the pool) owns.
    fn msg(&self, recv: Id, sel: &str) -> Id;
    /// `[recv sel:arg]` returning an object that `recv` (or the pool) owns.
    fn msg_id(&self, recv: Id, sel: &str, arg: Id) -> Id;
    /// `[recv sel:BOOL]`.
    fn msg_bool(&self, recv: Id, sel: &str, value: bool);
    /// `[recv sel:NSInteger]`.
    fn msg_int(&self, recv: Id, sel: &str, value: NSInteger);
    /// `[recv sel]` returning an `NSInteger` (`runModal`).
    fn msg_int_return(&self, recv: Id, sel: &str) -> NSInteger;
    /// An autoreleased `NSString` holding `text`.
    fn ns_string(&self, text: &str) -> Id;
    /// A copy of an `NSString`'s contents; `None` for nil.
    fn string_value(&self, string: Id) -> Option<String>;
    /// An autoreleased `NSArray` of `items`.
    fn array(&self, items: &[Id]) -> Id;
    /// `[array count]`.
    fn array_len(&self, array: Id) -> usize;
    /// `[array objectAtIndex:index]`; `index < array_len`.
    fn array_at(&self, array: Id, index: usize) -> Id;
    /// Sends `release` to an object the caller owns (+1).
    fn release(&self, object: Id);
}

/// Pops the autorelease pool on every exit path, including unwinding.
struct Pool<'a, R: DialogRuntime> {
    runtime: &'a R,
    pool: Id,
}

impl<'a, R: DialogRuntime> Pool<'a, R> {
    fn new(runtime: &'a R) -> Self {
        Self {
            runtime,
            pool: runtime.pool_push(),
        }
    }
}

impl<R: DialogRuntime> Drop for Pool<'_, R> {
    fn drop(&mut self) {
        self.runtime.pool_pop(self.pool);
    }
}

/// File extensions (without `*.`) an `NSSavePanel` should allow for `filter`; `None` when the
/// filter admits every file (`*`, `*.*`, empty) or has no extension-shaped pattern.
pub fn allowed_extensions(filter: Option<&FileFilter>) -> Option<Vec<String>> {
    let filter = filter?;
    let mut extensions: Vec<String> = Vec::new();
    for pattern in &filter.patterns {
        let pattern = pattern.trim();
        if pattern.is_empty() || pattern == "*" || pattern == "*.*" {
            return None;
        }
        let extension = pattern.rsplit('.').next().unwrap_or(pattern);
        let usable = !extension.is_empty()
            && !extension.contains(['*', '?', '[', ']', '/', '\\'])
            && (pattern.contains('.') || !pattern.contains(['*', '?']));
        if usable && !extensions.iter().any(|e| e == extension) {
            extensions.push(extension.to_string());
        }
    }
    (!extensions.is_empty()).then_some(extensions)
}

/// Splits `initial_path` into the directory a panel opens in and an optional file name.
/// An existing directory (or a path ending in a separator) is a directory; anything else is
/// `dir/name`, where a missing parent means "no directory".
fn split_initial_path(initial_path: &str) -> (Option<PathBuf>, Option<String>) {
    if initial_path.is_empty() {
        return (None, None);
    }
    let path = Path::new(initial_path);
    if path.is_dir() || initial_path.ends_with(['/', '\\']) {
        return (Some(path.to_path_buf()), None);
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty());
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf);
    (dir, name)
}

/// Releases an object the caller owns (+1) on every exit path, including unwinding.
struct Owned<'a, R: DialogRuntime> {
    runtime: &'a R,
    object: Id,
}

impl<R: DialogRuntime> Drop for Owned<'_, R> {
    fn drop(&mut self) {
        self.runtime.release(self.object);
    }
}

/// `NSApplicationActivationPolicyRegular`.
pub const NS_APPLICATION_ACTIVATION_POLICY_REGULAR: NSInteger = 0;
/// `NSApplicationActivationPolicyProhibited`: the policy of a bare (unbundled) process.
pub const NS_APPLICATION_ACTIVATION_POLICY_PROHIBITED: NSInteger = 2;

/// Which panel [`CocoaDialogs::run_panel`] shows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PanelKind {
    OpenFile { multiple: bool },
    SaveFile,
    Folder,
}

/// Qt-style static dialogs on AppKit, generic over how Objective-C is reached.
///
/// Every [`PlatformDialogs`] method must be called on the main thread (AppKit constraint) and
/// panics before sending any message otherwise. Each call activates the application first.
#[derive(Debug, Default)]
pub struct CocoaDialogs<R> {
    runtime: R,
}

impl<R> CocoaDialogs<R> {
    pub const fn new(runtime: R) -> Self {
        Self { runtime }
    }

    /// The message interface this instance sends through.
    pub const fn runtime(&self) -> &R {
        &self.runtime
    }
}

impl<R: DialogRuntime> CocoaDialogs<R> {
    /// Makes the application able to show a key window and brings it forward, so a dialog
    /// opened from a bare process (or behind other apps) can take focus. A process that already
    /// has a policy of its own (a bundled app: regular or accessory) keeps it; only a
    /// prohibited one becomes regular.
    fn prepare_application(&self) {
        let rt = &self.runtime;
        let app = rt.shared_application();
        if rt.msg_int_return(app, "activationPolicy") == NS_APPLICATION_ACTIVATION_POLICY_PROHIBITED
        {
            rt.msg_int(
                app,
                "setActivationPolicy:",
                NS_APPLICATION_ACTIVATION_POLICY_REGULAR,
            );
        }
        rt.msg_bool(app, "activateIgnoringOtherApps:", true);
    }

    fn nsstring(&self, text: &str) -> Id {
        self.runtime.ns_string(text)
    }

    fn file_url(&self, path: &Path) -> Id {
        let path = self.nsstring(&path.to_string_lossy());
        self.runtime.class_msg_id("NSURL", "fileURLWithPath:", path)
    }

    /// Path of an `NSURL` (`[url path]`); `None` for nil or a non-file URL without a path.
    fn url_path(&self, url: Id) -> Option<PathBuf> {
        let path = self.runtime.msg(url, "path");
        self.runtime.string_value(path).map(PathBuf::from)
    }

    /// Configures and runs one panel; returns the chosen paths (empty when cancelled).
    fn run_panel(
        &self,
        kind: PanelKind,
        title: &str,
        initial_path: &str,
        filter: Option<&FileFilter>,
    ) -> Vec<PathBuf> {
        let rt = &self.runtime;
        rt.require_main_thread("CocoaDialogs panel");
        let _pool = Pool::new(rt);
        self.prepare_application();

        let panel = match kind {
            PanelKind::SaveFile => rt.class_msg("NSSavePanel", "savePanel"),
            _ => rt.class_msg("NSOpenPanel", "openPanel"),
        };
        if panel.is_nil() {
            return Vec::new();
        }
        // `title` is not shown by modern panels; `message` is the line above the file list.
        if !title.is_empty() {
            rt.msg_id(panel, "setMessage:", self.nsstring(title));
        }
        match kind {
            PanelKind::OpenFile { multiple } => {
                rt.msg_bool(panel, "setCanChooseFiles:", true);
                rt.msg_bool(panel, "setCanChooseDirectories:", false);
                rt.msg_bool(panel, "setAllowsMultipleSelection:", multiple);
            }
            PanelKind::Folder => {
                rt.msg_bool(panel, "setCanChooseFiles:", false);
                rt.msg_bool(panel, "setCanChooseDirectories:", true);
                rt.msg_bool(panel, "setAllowsMultipleSelection:", false);
                rt.msg_bool(panel, "setCanCreateDirectories:", true);
            }
            PanelKind::SaveFile => rt.msg_bool(panel, "setCanCreateDirectories:", true),
        }

        if kind != PanelKind::Folder {
            if let Some(extensions) = allowed_extensions(filter) {
                let strings: Vec<Id> = extensions.iter().map(|e| self.nsstring(e)).collect();
                rt.msg_id(panel, "setAllowedFileTypes:", rt.array(&strings));
            }
        }

        let (directory, file_name) = split_initial_path(initial_path);
        if let Some(directory) = directory {
            rt.msg_id(panel, "setDirectoryURL:", self.file_url(&directory));
        }
        if let (PanelKind::SaveFile, Some(name)) = (kind, file_name) {
            rt.msg_id(panel, "setNameFieldStringValue:", self.nsstring(&name));
        }

        if rt.msg_int_return(panel, "runModal") != NS_MODAL_RESPONSE_OK {
            return Vec::new();
        }
        match kind {
            PanelKind::OpenFile { multiple: true } => {
                let urls = rt.msg(panel, "URLs");
                (0..rt.array_len(urls))
                    .filter_map(|i| self.url_path(rt.array_at(urls, i)))
                    .collect()
            }
            _ => {
                let url = rt.msg(panel, "URL");
                self.url_path(url).into_iter().collect()
            }
        }
    }

    /// Button titles and the result each maps to, in `NSAlert` button order (first = default).
    fn alert_buttons(buttons: MessageBoxButtons) -> &'static [(&'static str, MessageBoxResult)] {
        match buttons {
            MessageBoxButtons::Ok => &[("OK", MessageBoxResult::Ok)],
            MessageBoxButtons::OkCancel => &[
                ("OK", MessageBoxResult::Ok),
                ("Cancel", MessageBoxResult::Cancel),
            ],
            MessageBoxButtons::YesNo => {
                &[("Yes", MessageBoxResult::Yes), ("No", MessageBoxResult::No)]
            }
        }
    }

    fn alert_style(icon: DialogIcon) -> NSInteger {
        match icon {
            DialogIcon::Warning => NS_ALERT_STYLE_WARNING,
            DialogIcon::Error => NS_ALERT_STYLE_CRITICAL,
            // AppKit has no question style; Qt shows the application icon for both.
            DialogIcon::Info | DialogIcon::Question => NS_ALERT_STYLE_INFORMATIONAL,
        }
    }
}

impl<R: DialogRuntime + Send + Sync> PlatformDialogs for CocoaDialogs<R> {
    fn message_box(
        &self,
        title: &str,
        message: &str,
        icon: DialogIcon,
        buttons: MessageBoxButtons,
    ) -> MessageBoxResult {
        let rt = &self.runtime;
        rt.require_main_thread("CocoaDialogs::message_box");
        let _pool = Pool::new(rt);
        self.prepare_application();

        let table = Self::alert_buttons(buttons);
        // The result when the alert is dismissed without a button (stopped or aborted).
        let dismissed = table[table.len() - 1].1;
        let alert = rt.alloc_init("NSAlert");
        if alert.is_nil() {
            return dismissed;
        }
        // Declared after the pool, so it is released before the pool is popped.
        let alert = Owned {
            runtime: rt,
            object: alert,
        };
        let alert = alert.object;
        rt.msg_int(alert, "setAlertStyle:", Self::alert_style(icon));
        // AppKit shows an empty headline as a generic "Alert"; with no title the body becomes
        // the headline instead.
        if !title.is_empty() {
            rt.msg_id(alert, "setMessageText:", self.nsstring(title));
            rt.msg_id(alert, "setInformativeText:", self.nsstring(message));
        } else if !message.is_empty() {
            rt.msg_id(alert, "setMessageText:", self.nsstring(message));
        }
        for (title, result) in table {
            let button = rt.msg_id(alert, "addButtonWithTitle:", self.nsstring(title));
            if *result == MessageBoxResult::No {
                // Escape answers "No", as in QMessageBox.
                rt.msg_id(button, "setKeyEquivalent:", self.nsstring("\u{1b}"));
            }
        }
        let response = rt.msg_int_return(alert, "runModal");

        usize::try_from(response - NS_ALERT_FIRST_BUTTON_RETURN)
            .ok()
            .and_then(|index| table.get(index))
            .map_or(dismissed, |(_, result)| *result)
    }

    fn open_file(
        &self,
        title: &str,
        initial_path: &str,
        filter: Option<&FileFilter>,
    ) -> Option<PathBuf> {
        self.run_panel(
            PanelKind::OpenFile { multiple: false },
            title,
            initial_path,
            filter,
        )
        .into_iter()
        .next()
    }

    fn open_files(
        &self,
        title: &str,
        initial_path: &str,
        filter: Option<&FileFilter>,
    ) -> Option<Vec<PathBuf>> {
        let paths = self.run_panel(
            PanelKind::OpenFile { multiple: true },
            title,
            initial_path,
            filter,
        );
        (!paths.is_empty()).then_some(paths)
    }

    fn save_file(
        &self,
        title: &str,
        initial_path: &str,
        filter: Option<&FileFilter>,
    ) -> Option<PathBuf> {
        self.run_panel(PanelKind::SaveFile, title, initial_path, filter)
            .into_iter()
            .next()
    }

    fn select_folder(&self, title: &str, initial_path: &str) -> Option<PathBuf> {
        self.run_panel(PanelKind::Folder, title, initial_path, None)
            .into_iter()
            .next()
    }
}

// Crate-private: its methods message whatever `Id` they are given, which is sound only for the
// objects `CocoaDialogs` creates and passes, so no code outside the crate may call them.
#[cfg(target_os = "macos")]
pub(crate) use native::NativeRuntime;

#[cfg(target_os = "macos")]
mod native {
    use std::ffi::{c_char, CStr};

    use super::DialogRuntime;
    use crate::objc_runtime::{nsstring_from_str, objc_get_class, Id, NSInteger, ObjcMsg, Sel};

    /// [`DialogRuntime`] over the real Objective-C runtime.
    #[derive(Debug, Default, Clone, Copy)]
    pub(crate) struct NativeRuntime;

    fn class_id(name: &str) -> Id {
        Id(objc_get_class(name).0)
    }

    impl DialogRuntime for NativeRuntime {
        fn require_main_thread(&self, operation: &str) {
            // SAFETY: `NSThread` is a Foundation class; `+isMainThread` takes no arguments and
            // returns BOOL, matching `send_bool_return`, and is documented as callable from any
            // thread [inferred: Foundation documentation]. This is the one call made before the
            // main-thread check, so nothing here may assume the main thread.
            let is_main = unsafe {
                ObjcMsg::send_bool_return(class_id("NSThread"), Sel::register("isMainThread"))
            };
            if !is_main {
                panic!(
                    "ferrokit: `{operation}` must be called on the main thread (AppKit constraint)"
                );
            }
        }

        fn pool_push(&self) -> Id {
            // SAFETY: `NSAutoreleasePool` is a Foundation class; `alloc` and `init` take no
            // arguments and return objects, matching `send_class_0` / `send_0`, with `init`
            // sent to the unconsumed `alloc` result. The +1 pool is balanced by `drain` in
            // `pool_pop`, which `Pool::drop` calls on the same thread after
            // `require_main_thread` has passed.
            unsafe {
                let pool = ObjcMsg::send_class_0(
                    objc_get_class("NSAutoreleasePool"),
                    Sel::register("alloc"),
                );
                ObjcMsg::send_0(pool, Sel::register("init"))
            }
        }

        fn pool_pop(&self, pool: Id) {
            // SAFETY: `pool` was returned by `pool_push` and `Pool`'s `Drop` pops it exactly once,
            // on the thread that pushed it (the guard is a stack local); `drain` takes no
            // arguments and releases the pool (nil would be a no-op).
            unsafe { ObjcMsg::send_0(pool, Sel::register("drain")) };
        }

        fn shared_application(&self) -> Id {
            // SAFETY: `sharedApplication` is a no-argument class method of `NSApplication`
            // returning an object that AppKit owns for the life of the process (no +1 to
            // balance); main thread already verified by `require_main_thread`.
            unsafe {
                ObjcMsg::send_class_0(
                    objc_get_class("NSApplication"),
                    Sel::register("sharedApplication"),
                )
            }
        }

        fn class_msg(&self, class: &str, sel: &str) -> Id {
            // SAFETY: the callers pass only `openPanel`, `savePanel` and `array`
            // (`CocoaDialogs::run_panel`), which are no-argument class methods returning an
            // autoreleased object that lives until the enclosing `Pool` pops; an unknown class
            // name gives a nil class and a no-op send.
            unsafe { ObjcMsg::send_class_0(objc_get_class(class), Sel::register(sel)) }
        }

        fn class_msg_id(&self, class: &str, sel: &str, arg: Id) -> Id {
            // SAFETY: the only caller passes `fileURLWithPath:` (`CocoaDialogs::url`), which takes
            // one NSString and returns an autoreleased NSURL; `arg` is the autoreleased NSString
            // from `ns_string`, live until the pool pops, which is after this call.
            unsafe { ObjcMsg::send_id(class_id(class), Sel::register(sel), arg) }
        }

        fn alloc_init(&self, class: &str) -> Id {
            // SAFETY: `alloc` and `init` take no arguments and return objects; `init` is sent to
            // the unconsumed `alloc` result. The result is +1 and is released through `release`
            // by the caller (`Owned`), which is why this is not autoreleased.
            unsafe {
                let object = ObjcMsg::send_class_0(objc_get_class(class), Sel::register("alloc"));
                ObjcMsg::send_0(object, Sel::register("init"))
            }
        }

        fn msg(&self, recv: Id, sel: &str) -> Id {
            // SAFETY: the callers pass argument-less selectors that return an object (`URLs`,
            // `URL`, `path`, `window`, ...); `recv` is an object the logic created or got from an
            // earlier call inside the same pool (so live), or nil (a no-op send); main thread
            // already verified.
            unsafe { ObjcMsg::send_0(recv, Sel::register(sel)) }
        }

        fn msg_id(&self, recv: Id, sel: &str, arg: Id) -> Id {
            // SAFETY: the callers pass selectors taking exactly one object (`setMessageText:`,
            // `setDirectoryURL:`, ...); `recv` is live as in `msg`, and `arg` is an object built
            // inside the same pool (or nil), so it outlives this call.
            unsafe { ObjcMsg::send_id(recv, Sel::register(sel), arg) }
        }

        fn msg_bool(&self, recv: Id, sel: &str, value: bool) {
            // SAFETY: the callers pass selectors taking exactly one BOOL (`setCanChooseFiles:`,
            // ...); `recv` is the live panel/application as in `msg`.
            unsafe { ObjcMsg::send_bool(recv, Sel::register(sel), value) };
        }

        fn msg_int(&self, recv: Id, sel: &str, value: NSInteger) {
            // SAFETY: the callers pass selectors taking exactly one NSInteger (`setAlertStyle:`,
            // `setActivationPolicy:`); `recv` is the live alert/application as in `msg`.
            unsafe { ObjcMsg::send_int(recv, Sel::register(sel), value) };
        }

        fn msg_int_return(&self, recv: Id, sel: &str) -> NSInteger {
            // SAFETY: the callers pass argument-less selectors returning NSInteger
            // (`activationPolicy`, `runModal`); the NSUInteger sender is read through the same
            // 64-bit integer return register and `as NSInteger` reinterprets the bits
            // [inferred: x86-64 and arm64 return both in the integer register]; `recv` is the
            // live panel/alert/application as in `msg`.
            unsafe { ObjcMsg::send_usize_return(recv, Sel::register(sel)) as NSInteger }
        }

        fn ns_string(&self, text: &str) -> Id {
            let owned = nsstring_from_str(text);
            // SAFETY: `owned` is the +1 NSString just created by `nsstring_from_str`, so it is a
            // live receiver for `autorelease` (no arguments, returns the object), which hands that
            // one reference to the enclosing pool; no other release happens.
            unsafe { ObjcMsg::send_0(owned, Sel::register("autorelease")) }
        }

        fn string_value(&self, string: Id) -> Option<String> {
            if string.is_nil() {
                return None;
            }
            // SAFETY: `string` was checked non-nil; the callers pass NSStrings (paths from `[url
            // path]`). `UTF8String` takes no arguments and returns a `const char *`, read here
            // through the integer-return sender [inferred: pointer returns use the same register
            // as NSUInteger]; the buffer is NUL-terminated, owned by `string` and valid until the
            // pool pops (per the NSString documentation), and it is copied before returning.
            unsafe {
                let ptr = ObjcMsg::send_usize_return(string, Sel::register("UTF8String"))
                    as *const c_char;
                if ptr.is_null() {
                    return None;
                }
                Some(CStr::from_ptr(ptr).to_string_lossy().into_owned())
            }
        }

        fn array(&self, items: &[Id]) -> Id {
            // SAFETY: `+[NSMutableArray array]` returns an autoreleased array that lives until the
            // pool pops; `addObject:` takes one non-nil object and retains it. `items` are the
            // autoreleased strings/URLs from this operation, live until the pool pops, so the
            // array never holds a freed element (a nil item would raise; the callers pass only
            // non-nil objects).
            unsafe {
                let array =
                    ObjcMsg::send_class_0(objc_get_class("NSMutableArray"), Sel::register("array"));
                for item in items {
                    ObjcMsg::send_id(array, Sel::register("addObject:"), *item);
                }
                array
            }
        }

        fn array_len(&self, array: Id) -> usize {
            // SAFETY: `array` is the autoreleased array returned by `URLs` (or nil, giving 0);
            // `count` takes no arguments and returns NSUInteger, matching `send_usize_return`.
            unsafe { ObjcMsg::send_usize_return(array, Sel::register("count")) }
        }

        fn array_at(&self, array: Id, index: usize) -> Id {
            // SAFETY: `array` is the live array from `URLs`, and `run_panel` calls this only for
            // `i in 0..array_len(urls)`, so `index < count` and `objectAtIndex:` does not raise;
            // it takes one NSUInteger, matching `send_id_with_usize`.
            unsafe { ObjcMsg::send_id_with_usize(array, Sel::register("objectAtIndex:"), index) }
        }

        fn release(&self, object: Id) {
            // SAFETY: `object` is the +1 alert from `alloc_init`, released exactly once by its
            // `Owned` guard, which is dropped before the pool pops; `release` takes no arguments.
            unsafe { ObjcMsg::send_0(object, Sel::register("release")) };
        }
    }
}
