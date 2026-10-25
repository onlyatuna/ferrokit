// SPDX-License-Identifier: LGPL-3.0-only
use std::collections::HashMap;
use std::ffi::c_void;
#[cfg(target_os = "macos")]
use std::ffi::{c_char, CString};
use std::sync::{LazyLock, Mutex};

pub mod appkit;

#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Id(pub *mut c_void);

// Deliberately neither `Send` nor `Sync`: an `Id` is a raw pointer to an Objective-C object, and
// AppKit objects may only be messaged from the main thread. Types holding one inherit that.

impl Id {
    pub const NIL: Self = Self(std::ptr::null_mut());

    #[inline]
    pub fn is_nil(&self) -> bool {
        self.0.is_null()
    }

    #[inline]
    pub fn as_ptr(&self) -> *mut c_void {
        self.0
    }
}

/// Objective-C class pointer (`Class`)
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Class(pub *mut c_void);

impl Class {
    pub const NIL: Self = Self(std::ptr::null_mut());

    #[inline]
    pub fn is_nil(&self) -> bool {
        self.0.is_null()
    }

    #[inline]
    pub fn get(name: &str) -> Option<Self> {
        let cls = objc_get_class(name);
        if cls.is_nil() {
            None
        } else {
            Some(cls)
        }
    }
}

/// Objective-C selector pointer (`Sel`)
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Sel(pub *const c_void);

impl Sel {
    pub const NIL: Self = Self(std::ptr::null());

    #[inline]
    pub fn is_nil(&self) -> bool {
        self.0.is_null()
    }

    #[inline]
    pub fn register(name: &str) -> Self {
        sel_register_name(name)
    }
}

/// Objective-C boolean type (`BOOL`: YES = 1, NO = 0)
pub type BOOL = i8;
pub const YES: BOOL = 1;
pub const NO: BOOL = 0;

pub type CGFloat = f64;
pub type NSInteger = isize;
pub type NSUInteger = usize;

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CGPoint {
    pub x: CGFloat,
    pub y: CGFloat,
}

impl CGPoint {
    pub const ZERO: Self = Self { x: 0.0, y: 0.0 };
    pub fn new(x: CGFloat, y: CGFloat) -> Self {
        Self { x, y }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CGSize {
    pub width: CGFloat,
    pub height: CGFloat,
}

impl CGSize {
    pub const ZERO: Self = Self {
        width: 0.0,
        height: 0.0,
    };
    pub fn new(width: CGFloat, height: CGFloat) -> Self {
        Self { width, height }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CGRect {
    pub origin: CGPoint,
    pub size: CGSize,
}

impl CGRect {
    pub const ZERO: Self = Self {
        origin: CGPoint::ZERO,
        size: CGSize::ZERO,
    };
    pub fn new(x: CGFloat, y: CGFloat, width: CGFloat, height: CGFloat) -> Self {
        Self {
            origin: CGPoint::new(x, y),
            size: CGSize::new(width, height),
        }
    }
}

// ---------------------------------------------------------------------------
// AppKit constant definitions (aligned with macOS SDK)
// ---------------------------------------------------------------------------

pub const NS_VARIABLE_STATUS_ITEM_LENGTH: CGFloat = -1.0;
pub const NS_SQUARE_STATUS_ITEM_LENGTH: CGFloat = -2.0;

pub const NS_CONTROL_STATE_VALUE_OFF: NSInteger = 0;
pub const NS_CONTROL_STATE_VALUE_ON: NSInteger = 1;
pub const NS_CONTROL_STATE_VALUE_MIXED: NSInteger = -1;

pub const NS_WINDOW_STYLE_MASK_BORDERLESS: NSUInteger = 0;
pub const NS_WINDOW_STYLE_MASK_TITLED: NSUInteger = 1 << 0;
pub const NS_WINDOW_STYLE_MASK_CLOSABLE: NSUInteger = 1 << 1;
pub const NS_WINDOW_STYLE_MASK_MINIATURIZABLE: NSUInteger = 1 << 2;
pub const NS_WINDOW_STYLE_MASK_RESIZABLE: NSUInteger = 1 << 3;
pub const NS_WINDOW_STYLE_MASK_FULL_SIZE_CONTENT_VIEW: NSUInteger = 1 << 15;

pub const NS_BACKING_STORE_BUFFERED: NSUInteger = 2;

// NSWindowLevel values used by QCocoaWindow::windowLevel (qcocoawindow.mm:548-564).
pub const NS_NORMAL_WINDOW_LEVEL: NSInteger = 0; // kCGNormalWindowLevel
pub const NS_FLOATING_WINDOW_LEVEL: NSInteger = 3; // kCGFloatingWindowLevel
pub const NS_MODAL_PANEL_WINDOW_LEVEL: NSInteger = 8; // kCGModalPanelWindowLevel
pub const NS_SCREEN_SAVER_WINDOW_LEVEL: NSInteger = 1000; // kCGScreenSaverWindowLevel
pub const NS_STATUS_WINDOW_LEVEL: NSInteger = 25; // System status bar window level

// ---------------------------------------------------------------------------
// Runtime and message dispatch (native vs mock)
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
mod native_bindings {
    use super::*;

    #[link(name = "objc")]
    #[link(name = "AppKit", kind = "framework")]
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        pub fn objc_getClass(name: *const c_char) -> *mut c_void;
        pub fn sel_registerName(name: *const c_char) -> *const c_void;
        pub fn objc_msgSend();
        pub fn objc_allocateClassPair(
            superclass: *mut c_void,
            name: *const c_char,
            extra_bytes: usize,
        ) -> *mut c_void;
        pub fn objc_registerClassPair(class: *mut c_void);
        pub fn class_getInstanceMethod(class: *mut c_void, sel: *const c_void) -> *mut c_void;
        pub fn method_getTypeEncoding(method: *mut c_void) -> *const c_char;
        pub fn class_addMethod(
            class: *mut c_void,
            sel: *const c_void,
            imp: *const c_void,
            types: *const c_char,
        ) -> BOOL;
    }
}

/// The content view class: an `NSView` subclass named `QNSView` whose `-isFlipped` returns YES,
/// like Qt's (qnsview_drawing.mm:67-70), registered with the runtime on first use.
#[cfg(target_os = "macos")]
pub fn qnsview_class() -> Class {
    extern "C" fn is_flipped(_this: *mut c_void, _cmd: *const c_void) -> BOOL {
        YES
    }
    static CLASS: std::sync::LazyLock<usize> = std::sync::LazyLock::new(|| unsafe {
        let existing = objc_get_class("QNSView");
        if !existing.is_nil() {
            return existing.0 as usize;
        }
        let ns_view = objc_get_class("NSView");
        let name = CString::new("QNSView").unwrap();
        let class = native_bindings::objc_allocateClassPair(ns_view.0, name.as_ptr(), 0);
        let sel = Sel::register("isFlipped");
        let inherited = native_bindings::class_getInstanceMethod(ns_view.0, sel.0);
        native_bindings::class_addMethod(
            class,
            sel.0,
            is_flipped as *const c_void,
            native_bindings::method_getTypeEncoding(inherited),
        );
        appkit::install_view_methods(class);
        native_bindings::objc_registerClassPair(class);
        class as usize
    });
    Class(*CLASS as *mut c_void)
}

/// `[[NSProcessInfo processInfo] isOperatingSystemAtLeastVersion:{major, 0, 0}]`.
#[cfg(target_os = "macos")]
fn is_macos_at_least(major: NSInteger) -> bool {
    #[repr(C)]
    struct NSOperatingSystemVersion {
        major: NSInteger,
        minor: NSInteger,
        patch: NSInteger,
    }
    let info = unsafe {
        ObjcMsg::send_class_0(
            objc_get_class("NSProcessInfo"),
            Sel::register("processInfo"),
        )
    };
    let sel = Sel::register("isOperatingSystemAtLeastVersion:");
    let version = NSOperatingSystemVersion {
        major,
        minor: 0,
        patch: 0,
    };
    let res: BOOL = unsafe {
        let msg_send: unsafe extern "C" fn(
            *mut c_void,
            *const c_void,
            NSOperatingSystemVersion,
        ) -> BOOL = std::mem::transmute(native_bindings::objc_msgSend as *const ());
        msg_send(info.0, sel.0, version)
    };
    res == YES
}

/// The application delegate, an `NSObject` subclass named `QCocoaApplicationDelegate` created
/// once and never released (Qt's `sharedDelegate`). Its `-applicationDidFinishLaunching:` brings
/// the application to the front like Qt's (qcocoaapplicationdelegate.mm:157-194): it activates
/// the application if it is not active, and on macOS 14 and later activates it again with all
/// its windows. Neither happens when `QT_MAC_DISABLE_FOREGROUND_APPLICATION_TRANSFORM` is set.
#[cfg(target_os = "macos")]
pub fn qcocoa_application_delegate() -> Id {
    extern "C" fn did_finish_launching(
        _this: *mut c_void,
        _cmd: *const c_void,
        _notification: *mut c_void,
    ) {
        if std::env::var_os("QT_MAC_DISABLE_FOREGROUND_APPLICATION_TRANSFORM")
            .is_some_and(|value| !value.is_empty())
        {
            return;
        }
        let current = unsafe {
            ObjcMsg::send_class_0(
                objc_get_class("NSRunningApplication"),
                Sel::register("currentApplication"),
            )
        };
        if !unsafe { ObjcMsg::send_bool_return(current, Sel::register("isActive")) } {
            let app = unsafe {
                ObjcMsg::send_class_0(
                    objc_get_class("NSApplication"),
                    Sel::register("sharedApplication"),
                )
            };
            unsafe { ObjcMsg::send_bool(app, Sel::register("activateIgnoringOtherApps:"), true) };
        }
        if is_macos_at_least(14) {
            // NSApplicationActivateAllWindows
            unsafe { ObjcMsg::send_int(current, Sel::register("activateWithOptions:"), 1) };
        }
    }
    static DELEGATE: std::sync::LazyLock<usize> = std::sync::LazyLock::new(|| unsafe {
        let mut class = objc_get_class("QCocoaApplicationDelegate");
        if class.is_nil() {
            let name = CString::new("QCocoaApplicationDelegate").unwrap();
            let raw = native_bindings::objc_allocateClassPair(
                objc_get_class("NSObject").0,
                name.as_ptr(),
                0,
            );
            native_bindings::class_addMethod(
                raw,
                Sel::register("applicationDidFinishLaunching:").0,
                did_finish_launching as *const c_void,
                c"v@:@".as_ptr(),
            );
            native_bindings::objc_registerClassPair(raw);
            class = Class(raw);
        }
        let delegate = ObjcMsg::send_0(
            ObjcMsg::send_class_0(class, Sel::register("alloc")),
            Sel::register("init"),
        );
        delegate.0 as usize
    });
    Id(*DELEGATE as *mut c_void)
}

/// Creates an NSString object from a UTF-8 string on macOS
#[cfg(target_os = "macos")]
pub fn nsstring_from_str(s: &str) -> Id {
    let cls = objc_get_class("NSString");
    let sel_alloc = Sel::register("alloc");
    let sel_init = Sel::register("initWithBytes:length:encoding:");
    let str_obj = unsafe { ObjcMsg::send_class_0(cls, sel_alloc) };
    unsafe {
        let msg_send: extern "C" fn(
            *mut c_void,
            *const c_void,
            *const u8,
            NSUInteger,
            NSUInteger,
        ) -> *mut c_void = std::mem::transmute(native_bindings::objc_msgSend as *const ());
        Id(msg_send(
            str_obj.0,
            sel_init.0,
            s.as_ptr(),
            s.len(),
            4, /* NSUTF8StringEncoding */
        ))
    }
}

/// Retrieves class pointer
pub fn objc_get_class(name: &str) -> Class {
    #[cfg(target_os = "macos")]
    {
        let c_str = CString::new(name).unwrap();
        Class(unsafe { native_bindings::objc_getClass(c_str.as_ptr()) })
    }
    #[cfg(not(target_os = "macos"))]
    {
        MockObjcRuntime::instance().get_class(name)
    }
}

/// Registers or retrieves selector
pub fn sel_register_name(name: &str) -> Sel {
    #[cfg(target_os = "macos")]
    {
        let c_str = CString::new(name).unwrap();
        Sel(unsafe { native_bindings::sel_registerName(c_str.as_ptr()) })
    }
    #[cfg(not(target_os = "macos"))]
    {
        MockObjcRuntime::instance().register_sel(name)
    }
}

// ---------------------------------------------------------------------------
// High-fidelity mock Objective-C runtime for cross-platform simulation (MockObjcRuntime)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct MockObjectData {
    pub class_name: String,
    pub title: String,
    pub frame: CGRect,
    pub level: NSInteger,
    pub ignores_mouse_events: bool,
    pub is_flipped: bool,
    pub is_visible: bool,
    /// `-[NSWindow isKeyWindow]`: set by `makeKeyAndOrderFront:`, cleared by `orderOut:`.
    pub is_key: bool,
    pub state: NSInteger,
    pub enabled: bool,
    pub children: Vec<Id>,
    pub parent: Id,
    pub menu: Id,
    pub button: Id,
    pub contents: Id,
    pub visible_frame: CGRect,
    pub scale_factor: f64,
    pub material: NSInteger,
    pub blending_mode: NSInteger,
    /// `-[NSControl target]` / `-[NSMenuItem target]`, set by `setTarget:`.
    pub target: Id,
    /// `-[NSControl action]` / `-[NSMenuItem action]`, set by `setAction:`.
    pub action: Sel,
    /// Pixel bytes of an `NSBitmapImageRep`, set by `ObjcMsg::send_bitmap_rep_init`.
    pub pixels: Vec<u8>,
    /// Owners of the object; `alloc` starts at one, containers add one per held child.
    pub retain_count: usize,
    /// `-[NSWindow releasedWhenClosed]`: AppKit's default is YES, so `close` releases the window.
    pub released_when_closed: bool,
    /// `-[NSWindow styleMask]`, set by `initWithContentRect:styleMask:backing:defer:`.
    pub style_mask: NSUInteger,
}

// SAFETY: in the mock runtime an `Id` is an integer handle into `MockObjcRuntime::objects`, never
// a dereferenced pointer, so moving `MockObjectData` between threads cannot touch foreign state.
// This lets the mock registry live in a `static` behind a `Mutex`; real `Id`s stay `!Send`.
unsafe impl Send for MockObjectData {}

impl Default for MockObjectData {
    fn default() -> Self {
        Self {
            class_name: String::new(),
            title: String::new(),
            frame: CGRect::ZERO,
            level: 0,
            ignores_mouse_events: false,
            is_flipped: false,
            is_visible: false,
            is_key: false,
            state: NS_CONTROL_STATE_VALUE_OFF,
            enabled: true,
            children: Vec::new(),
            parent: Id::NIL,
            menu: Id::NIL,
            button: Id::NIL,
            contents: Id::NIL,
            visible_frame: CGRect::ZERO,
            scale_factor: 1.0,
            material: 0,
            blending_mode: 0,
            target: Id::NIL,
            action: Sel::NIL,
            pixels: Vec::new(),
            retain_count: 1,
            released_when_closed: true,
            style_mask: 0,
        }
    }
}

pub struct MockObjcRuntime {
    classes: Mutex<HashMap<String, usize>>,
    selectors: Mutex<HashMap<String, usize>>,
    objects: Mutex<HashMap<usize, MockObjectData>>,
    next_id: Mutex<usize>,
    /// Ids sent `release` after they were deallocated (ids are never reused).
    over_released: Mutex<Vec<usize>>,
}

impl Default for MockObjcRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl MockObjcRuntime {
    pub fn new() -> Self {
        let mut classes = HashMap::new();
        let class_names = [
            "NSApplication",
            "NSStatusBar",
            "NSStatusItem",
            "NSStatusBarButton",
            "NSMenu",
            "NSMenuItem",
            "NSWindow",
            "NSView",
            "QNSView",
            "NSEvent",
            "CALayer",
            "NSScreen",
            "NSVisualEffectView",
            "NSArray",
        ];
        for (i, name) in class_names.iter().enumerate() {
            classes.insert(name.to_string(), i + 1);
        }

        Self {
            classes: Mutex::new(classes),
            selectors: Mutex::new(HashMap::new()),
            objects: Mutex::new(HashMap::new()),
            next_id: Mutex::new(100),
            over_released: Mutex::new(Vec::new()),
        }
    }

    pub fn instance() -> &'static Self {
        static INSTANCE: LazyLock<MockObjcRuntime> = LazyLock::new(MockObjcRuntime::new);
        &INSTANCE
    }

    pub fn get_class(&self, name: &str) -> Class {
        let mut lock = self.classes.lock().unwrap();
        let next = lock.len() + 1;
        let id = lock.entry(name.to_string()).or_insert(next);
        Class(*id as *mut c_void)
    }

    pub fn register_sel(&self, name: &str) -> Sel {
        let mut lock = self.selectors.lock().unwrap();
        let next = lock.len() + 1;
        let id = lock.entry(name.to_string()).or_insert(next);
        Sel(*id as *const c_void)
    }

    pub fn allocate_object(&self, class_name: &str) -> Id {
        let mut id_lock = self.next_id.lock().unwrap();
        let id = *id_lock;
        *id_lock += 1;

        let mut obj = MockObjectData {
            class_name: class_name.to_string(),
            ..Default::default()
        };

        if class_name == "NSStatusItem" {
            // Automatically create NSStatusBarButton for NSStatusItem
            let button_id = *id_lock;
            *id_lock += 1;
            let button = MockObjectData {
                class_name: "NSStatusBarButton".to_string(),
                parent: Id(id as *mut c_void),
                ..Default::default()
            };
            self.objects.lock().unwrap().insert(button_id, button);
            obj.button = Id(button_id as *mut c_void);
        }

        if class_name == "QNSView" {
            // Align with Qt QNSView (qnsview_drawing.mm:67-70): the view reports itself as flipped.
            obj.is_flipped = true;
        }

        if class_name == "NSScreen" {
            obj.frame = CGRect::new(0.0, 0.0, 1920.0, 1080.0);
            obj.visible_frame = CGRect::new(0.0, 25.0, 1920.0, 1055.0);
            obj.scale_factor = 2.0;
            obj.title = "CocoaPrimaryScreen".to_string();
        }

        self.objects.lock().unwrap().insert(id, obj);
        Id(id as *mut c_void)
    }

    pub fn get_object_data(&self, id: Id) -> Option<MockObjectData> {
        self.objects.lock().unwrap().get(&(id.0 as usize)).cloned()
    }

    pub fn update_object<F: FnOnce(&mut MockObjectData)>(&self, id: Id, f: F) {
        if let Some(obj) = self.objects.lock().unwrap().get_mut(&(id.0 as usize)) {
            f(obj);
        }
    }

    pub fn remove_object(&self, id: Id) {
        self.objects.lock().unwrap().remove(&(id.0 as usize));
    }

    /// `-retain`: another owner holds `id`.
    pub fn retain(&self, id: Id) {
        self.update_object(id, |data| data.retain_count += 1);
    }

    /// `-release`: drops one owner; at zero the object is deallocated and releases what it
    /// holds (subviews, window content view, image representations, ...).
    pub fn release(&self, id: Id) {
        let mut pending = vec![id];
        while let Some(id) = pending.pop() {
            let mut objects = self.objects.lock().unwrap();
            let key = id.0 as usize;
            let Some(obj) = objects.get_mut(&key) else {
                drop(objects);
                // `allocate_object` locks `next_id` before `objects`; never the reverse.
                if key < *self.next_id.lock().unwrap() {
                    self.over_released.lock().unwrap().push(key);
                }
                continue;
            };
            obj.retain_count = obj.retain_count.saturating_sub(1);
            if obj.retain_count == 0 {
                if let Some(dead) = objects.remove(&key) {
                    pending.extend(dead.children);
                }
            }
        }
    }

    /// Whether `id` was sent `release` after it had already been deallocated.
    pub fn was_over_released(&self, id: Id) -> bool {
        self.over_released
            .lock()
            .unwrap()
            .contains(&(id.0 as usize))
    }

    /// `-[NSWindow close]`: orders the window out and, if `releasedWhenClosed`, releases it.
    #[cfg(not(target_os = "macos"))]
    fn close_window(&self, id: Id) {
        let Some(data) = self.get_object_data(id) else {
            return;
        };
        self.update_object(id, |d| {
            d.is_visible = false;
            d.is_key = false;
        });
        if data.released_when_closed {
            self.release(id);
        }
    }
}

// ---------------------------------------------------------------------------
// Message send wrapper (msg_send mock and native dispatch)
// ---------------------------------------------------------------------------

/// Typed `objc_msgSend` wrappers (native on macOS, a recording mock elsewhere).
///
/// Each function fixes one C signature, named by the function, and forwards to `objc_msgSend`
/// with it. Nothing checks that `sel` actually has that signature, so every function is `unsafe`.
///
/// # Safety (all `send_*` functions)
///
/// - `receiver` is nil (the send is then a no-op returning nil/zero/false) or a live Objective-C
///   object; for `send_class_0` it is a class.
/// - The receiver's class implements `sel` with exactly the argument and return types of the
///   function called (`send_int` for `setLevel:`, `send_id` for `addSubview:`, ...).
/// - The caller is on the thread the receiver belongs to (the main thread for AppKit objects) and
///   owns the reference counts: results of `alloc`/`new`/`copy` are +1 and must be balanced.
pub struct ObjcMsg;

/// Name the mock runtime registered for `sel` (the reverse of `sel_register_name`).
#[cfg(not(target_os = "macos"))]
fn mock_selector_name(sel: Sel) -> String {
    MockObjcRuntime::instance()
        .selectors
        .lock()
        .unwrap()
        .iter()
        .find(|(_, &v)| v == (sel.0 as usize))
        .map(|(k, _)| k.clone())
        .unwrap_or_default()
}

#[cfg(target_os = "macos")]
#[inline]
unsafe fn msg_send_fn<F: Copy>() -> F {
    debug_assert_eq!(std::mem::size_of::<F>(), std::mem::size_of::<*const ()>());
    std::mem::transmute_copy(&(native_bindings::objc_msgSend as *const ()))
}

impl ObjcMsg {
    /// `-[receiver sel]` returning an object.
    ///
    /// # Safety
    /// See [`ObjcMsg`].
    pub unsafe fn send_0(receiver: Id, sel: Sel) -> Id {
        if receiver.is_nil() {
            return Id::NIL;
        }

        #[cfg(target_os = "macos")]
        {
            let msg_send: extern "C" fn(*mut c_void, *const c_void) -> *mut c_void = msg_send_fn();
            Id(msg_send(receiver.0, sel.0))
        }

        #[cfg(not(target_os = "macos"))]
        {
            let runtime = MockObjcRuntime::instance();
            let sel_name = mock_selector_name(sel);
            if sel_name == "button" {
                if let Some(data) = runtime.get_object_data(receiver) {
                    return data.button;
                }
            } else if sel_name == "menu" {
                if let Some(data) = runtime.get_object_data(receiver) {
                    return data.menu;
                }
            } else if sel_name == "release" {
                runtime.release(receiver);
                return Id::NIL;
            } else if sel_name == "close" {
                runtime.close_window(receiver);
                return Id::NIL;
            } else if sel_name == "removeFromSuperview" {
                // A view without a superview has nothing to remove (and no superview retain).
                if let Some(parent) = runtime
                    .get_object_data(receiver)
                    .map(|d| d.parent)
                    .filter(|p| !p.is_nil())
                {
                    runtime.update_object(parent, |p| p.children.retain(|c| *c != receiver));
                    runtime.update_object(receiver, |d| d.parent = Id::NIL);
                    // The superview's retain is gone.
                    runtime.release(receiver);
                }
                return Id::NIL;
            }
            receiver
        }
    }

    /// `+[class sel]` returning an object (`alloc`, `systemStatusBar`, ...).
    ///
    /// # Safety
    /// See [`ObjcMsg`].
    pub unsafe fn send_class_0(class: Class, sel: Sel) -> Id {
        if class.is_nil() {
            return Id::NIL;
        }

        #[cfg(target_os = "macos")]
        {
            let msg_send: extern "C" fn(*mut c_void, *const c_void) -> *mut c_void = msg_send_fn();
            Id(msg_send(class.0, sel.0))
        }

        #[cfg(not(target_os = "macos"))]
        {
            let runtime = MockObjcRuntime::instance();
            let class_name = runtime
                .classes
                .lock()
                .unwrap()
                .iter()
                .find(|(_, &v)| v == (class.0 as usize))
                .map(|(k, _)| k.clone())
                .unwrap_or_default();
            let sel_name = mock_selector_name(sel);

            if sel_name == "systemStatusBar" {
                runtime.allocate_object("NSStatusBar")
            } else if sel_name == "sharedApplication" {
                runtime.allocate_object("NSApplication")
            } else if sel_name == "mainScreen" {
                runtime.allocate_object("NSScreen")
            } else if sel_name == "screens" {
                let array_id = runtime.allocate_object("NSArray");
                let screen_id = runtime.allocate_object("NSScreen");
                runtime.update_object(array_id, |data| {
                    data.children.push(screen_id);
                });
                array_id
            } else if sel_name == "alloc" {
                runtime.allocate_object(&class_name)
            } else {
                Id::NIL
            }
        }
    }

    /// `-[receiver sel:NSString]` with `text` as a temporary `NSString` (released after the send,
    /// so `sel` must copy or retain it: `setTitle:`, `initWithTitle:`, `setToolTip:`, ...).
    ///
    /// # Safety
    /// See [`ObjcMsg`].
    #[allow(unused_variables)]
    pub unsafe fn send_str(receiver: Id, sel: Sel, text: &str) -> Id {
        if receiver.is_nil() {
            return Id::NIL;
        }

        #[cfg(not(target_os = "macos"))]
        {
            let runtime = MockObjcRuntime::instance();
            runtime.update_object(receiver, |data| {
                data.title = text.to_string();
            });
            receiver
        }

        #[cfg(target_os = "macos")]
        {
            let ns_text = nsstring_from_str(text);
            let msg_send: extern "C" fn(*mut c_void, *const c_void, *mut c_void) -> *mut c_void =
                msg_send_fn();
            let result = Id(msg_send(receiver.0, sel.0, ns_text.0));
            Self::send_0(ns_text, Sel::register("release"));
            result
        }
    }

    /// `-[NSMenuItem initWithTitle:action:keyEquivalent:]` on an allocated item.
    ///
    /// # Safety
    /// See [`ObjcMsg`]; `receiver` is an allocated, not yet initialised `NSMenuItem`.
    #[allow(unused_variables)]
    pub unsafe fn send_menu_item_init(
        receiver: Id,
        title: &str,
        action: Sel,
        key_equivalent: &str,
    ) -> Id {
        if receiver.is_nil() {
            return Id::NIL;
        }

        #[cfg(not(target_os = "macos"))]
        {
            let runtime = MockObjcRuntime::instance();
            runtime.update_object(receiver, |data| {
                data.title = title.to_string();
                data.action = action;
            });
            receiver
        }

        #[cfg(target_os = "macos")]
        {
            let ns_title = nsstring_from_str(title);
            let ns_key = nsstring_from_str(key_equivalent);
            let msg_send: extern "C" fn(
                *mut c_void,
                *const c_void,
                *mut c_void,
                *const c_void,
                *mut c_void,
            ) -> *mut c_void = msg_send_fn();
            let result = Id(msg_send(
                receiver.0,
                Sel::register("initWithTitle:action:keyEquivalent:").0,
                ns_title.0,
                action.0,
                ns_key.0,
            ));
            Self::send_0(ns_title, Sel::register("release"));
            Self::send_0(ns_key, Sel::register("release"));
            result
        }
    }

    /// `-[receiver sel:id]` (`setMenu:`, `addItem:`, `addSubview:`, `setTarget:`, ...).
    ///
    /// # Safety
    /// See [`ObjcMsg`].
    pub unsafe fn send_id(receiver: Id, sel: Sel, arg: Id) -> Id {
        if receiver.is_nil() {
            return Id::NIL;
        }

        #[cfg(not(target_os = "macos"))]
        {
            let runtime = MockObjcRuntime::instance();
            let sel_name = mock_selector_name(sel);

            if sel_name == "setMenu:" {
                runtime.update_object(receiver, |data| {
                    data.menu = arg;
                });
            } else if sel_name == "setTarget:" {
                runtime.update_object(receiver, |data| {
                    data.target = arg;
                });
            } else if sel_name == "makeKeyAndOrderFront:" {
                runtime.update_object(receiver, |data| {
                    data.is_visible = true;
                    data.is_key = true;
                });
            } else if sel_name == "orderFront:" {
                runtime.update_object(receiver, |data| {
                    data.is_visible = true;
                });
            } else if sel_name == "performClose:" {
                // AppKit only closes (and otherwise beeps) when the window has a close button.
                let closable = runtime
                    .get_object_data(receiver)
                    .is_some_and(|d| d.style_mask & NS_WINDOW_STYLE_MASK_CLOSABLE != 0);
                if closable {
                    runtime.close_window(receiver);
                }
            } else if sel_name == "orderOut:" {
                runtime.update_object(receiver, |data| {
                    data.is_visible = false;
                    data.is_key = false;
                });
            } else if sel_name == "setContents:" {
                runtime.update_object(receiver, |data| {
                    data.contents = arg;
                });
            } else if sel_name == "addItem:"
                || sel_name == "addSubview:"
                || sel_name == "setContentView:"
            {
                runtime.update_object(receiver, |data| {
                    data.children.push(arg);
                });
                runtime.update_object(arg, |data| {
                    data.parent = receiver;
                });
                // Containers retain what they hold.
                runtime.retain(arg);
            } else if sel_name == "removeStatusItem:" {
                runtime.remove_object(arg);
            } else if sel_name == "setImage:" {
                let old = runtime
                    .get_object_data(receiver)
                    .map(|d| d.contents)
                    .unwrap_or(Id::NIL);
                runtime.update_object(receiver, |data| {
                    data.contents = arg;
                });
                runtime.retain(arg);
                runtime.release(old);
            } else if sel_name == "addRepresentation:" {
                runtime.update_object(receiver, |data| {
                    data.children.push(arg);
                });
                runtime.retain(arg);
            }
            receiver
        }

        #[cfg(target_os = "macos")]
        {
            let msg_send: extern "C" fn(*mut c_void, *const c_void, *mut c_void) -> *mut c_void =
                msg_send_fn();
            Id(msg_send(receiver.0, sel.0, arg.0))
        }
    }

    /// `-[receiver sel:SEL]` (`setAction:`).
    ///
    /// # Safety
    /// See [`ObjcMsg`].
    pub unsafe fn send_action(receiver: Id, sel: Sel, action: Sel) -> Id {
        if receiver.is_nil() {
            return Id::NIL;
        }

        #[cfg(not(target_os = "macos"))]
        {
            if mock_selector_name(sel) == "setAction:" {
                MockObjcRuntime::instance().update_object(receiver, |data| {
                    data.action = action;
                });
            }
            receiver
        }

        #[cfg(target_os = "macos")]
        {
            let msg_send: extern "C" fn(*mut c_void, *const c_void, *const c_void) -> *mut c_void =
                msg_send_fn();
            Id(msg_send(receiver.0, sel.0, action.0))
        }
    }

    /// `-[receiver sel:CGFloat]` (`statusItemWithLength:`, `setZPosition:`).
    ///
    /// # Safety
    /// See [`ObjcMsg`].
    pub unsafe fn send_length(receiver: Id, sel: Sel, length: CGFloat) -> Id {
        if receiver.is_nil() {
            return Id::NIL;
        }

        #[cfg(not(target_os = "macos"))]
        {
            let _ = (sel, length);
            MockObjcRuntime::instance().allocate_object("NSStatusItem")
        }

        #[cfg(target_os = "macos")]
        {
            let msg_send: extern "C" fn(*mut c_void, *const c_void, CGFloat) -> *mut c_void =
                msg_send_fn();
            Id(msg_send(receiver.0, sel.0, length))
        }
    }

    /// `-[receiver sel:NSInteger]` (`setLevel:`, `setState:`, ...).
    ///
    /// # Safety
    /// See [`ObjcMsg`].
    pub unsafe fn send_int(receiver: Id, sel: Sel, val: NSInteger) -> Id {
        if receiver.is_nil() {
            return Id::NIL;
        }

        #[cfg(not(target_os = "macos"))]
        {
            let runtime = MockObjcRuntime::instance();
            let sel_name = mock_selector_name(sel);

            if sel_name == "setLevel:" {
                runtime.update_object(receiver, |data| {
                    data.level = val;
                });
            } else if sel_name == "setState:" {
                runtime.update_object(receiver, |data| {
                    data.state = val;
                });
            } else if sel_name == "setMaterial:" {
                runtime.update_object(receiver, |data| {
                    data.material = val;
                });
            } else if sel_name == "setBlendingMode:" {
                runtime.update_object(receiver, |data| {
                    data.blending_mode = val;
                });
            }
            receiver
        }

        #[cfg(target_os = "macos")]
        {
            let msg_send: extern "C" fn(*mut c_void, *const c_void, NSInteger) -> *mut c_void =
                msg_send_fn();
            Id(msg_send(receiver.0, sel.0, val))
        }
    }

    /// `-[receiver sel:BOOL]` (`setIgnoresMouseEvents:`, `setEnabled:`, ...).
    ///
    /// # Safety
    /// See [`ObjcMsg`].
    pub unsafe fn send_bool(receiver: Id, sel: Sel, val: bool) -> Id {
        if receiver.is_nil() {
            return Id::NIL;
        }

        #[cfg(not(target_os = "macos"))]
        {
            let runtime = MockObjcRuntime::instance();
            let sel_name = mock_selector_name(sel);

            if sel_name == "setIgnoresMouseEvents:" {
                runtime.update_object(receiver, |data| {
                    data.ignores_mouse_events = val;
                });
            } else if sel_name == "setReleasedWhenClosed:" {
                runtime.update_object(receiver, |data| {
                    data.released_when_closed = val;
                });
            } else if sel_name == "setEnabled:" {
                runtime.update_object(receiver, |data| {
                    data.enabled = val;
                });
            }
            receiver
        }

        #[cfg(target_os = "macos")]
        {
            let bool_val: BOOL = if val { YES } else { NO };
            let msg_send: extern "C" fn(*mut c_void, *const c_void, BOOL) -> *mut c_void =
                msg_send_fn();
            Id(msg_send(receiver.0, sel.0, bool_val))
        }
    }

    /// `-[receiver sel:CGSize]` (`setMinSize:`, `initWithSize:`).
    ///
    /// # Safety
    /// See [`ObjcMsg`].
    pub unsafe fn send_size(receiver: Id, sel: Sel, size: CGSize) -> Id {
        if receiver.is_nil() {
            return Id::NIL;
        }

        #[cfg(target_os = "macos")]
        {
            let msg_send: extern "C" fn(*mut c_void, *const c_void, CGSize) -> *mut c_void =
                msg_send_fn();
            Id(msg_send(receiver.0, sel.0, size))
        }

        #[cfg(not(target_os = "macos"))]
        {
            let _ = (sel, size);
            receiver
        }
    }

    /// `-[receiver sel:CGRect]` (`initWithFrame:`).
    ///
    /// # Safety
    /// See [`ObjcMsg`].
    #[allow(unused_variables)]
    pub unsafe fn send_rect(receiver: Id, sel: Sel, rect: CGRect) -> Id {
        if receiver.is_nil() {
            return Id::NIL;
        }

        #[cfg(not(target_os = "macos"))]
        {
            MockObjcRuntime::instance().update_object(receiver, |data| {
                data.frame = rect;
            });
            receiver
        }

        #[cfg(target_os = "macos")]
        {
            let msg_send: extern "C" fn(*mut c_void, *const c_void, CGRect) -> *mut c_void =
                msg_send_fn();
            Id(msg_send(receiver.0, sel.0, rect))
        }
    }

    /// `-[receiver sel:CGRect display:BOOL]` (`setFrame:display:`).
    ///
    /// # Safety
    /// See [`ObjcMsg`].
    #[allow(unused_variables)]
    pub unsafe fn send_rect_display(receiver: Id, sel: Sel, rect: CGRect, display: bool) -> Id {
        if receiver.is_nil() {
            return Id::NIL;
        }

        #[cfg(not(target_os = "macos"))]
        {
            MockObjcRuntime::instance().update_object(receiver, |data| {
                data.frame = rect;
            });
            receiver
        }

        #[cfg(target_os = "macos")]
        {
            let msg_send: extern "C" fn(*mut c_void, *const c_void, CGRect, BOOL) -> *mut c_void =
                msg_send_fn();
            Id(msg_send(
                receiver.0,
                sel.0,
                rect,
                if display { YES } else { NO },
            ))
        }
    }

    /// `-[NSWindow initWithContentRect:styleMask:backing:defer:]` on an allocated window.
    ///
    /// # Safety
    /// See [`ObjcMsg`]; `sel` is `initWithContentRect:styleMask:backing:defer:`.
    #[allow(unused_variables)]
    pub unsafe fn send_window_init(
        receiver: Id,
        sel: Sel,
        rect: CGRect,
        style_mask: NSUInteger,
        backing: NSUInteger,
        defer_flag: bool,
    ) -> Id {
        if receiver.is_nil() {
            return Id::NIL;
        }

        #[cfg(not(target_os = "macos"))]
        {
            let runtime = MockObjcRuntime::instance();
            runtime.update_object(receiver, |data| {
                data.frame = rect;
                data.style_mask = style_mask;
            });
            receiver
        }

        #[cfg(target_os = "macos")]
        {
            let msg_send: extern "C" fn(
                *mut c_void,
                *const c_void,
                CGRect,
                NSUInteger,
                NSUInteger,
                BOOL,
            ) -> *mut c_void = msg_send_fn();
            Id(msg_send(
                receiver.0,
                sel.0,
                rect,
                style_mask,
                backing,
                if defer_flag { YES } else { NO },
            ))
        }
    }

    /// `-[receiver sel]` returning `BOOL` (`isFlipped`, `isActive`, ...).
    ///
    /// # Safety
    /// See [`ObjcMsg`].
    pub unsafe fn send_bool_return(receiver: Id, sel: Sel) -> bool {
        if receiver.is_nil() {
            return false;
        }

        #[cfg(not(target_os = "macos"))]
        {
            let runtime = MockObjcRuntime::instance();
            let sel_name = mock_selector_name(sel);

            if sel_name == "isFlipped" {
                if let Some(obj) = runtime.get_object_data(receiver) {
                    return obj.is_flipped;
                }
            } else if sel_name == "isKeyWindow" {
                if let Some(obj) = runtime.get_object_data(receiver) {
                    return obj.is_key;
                }
            }
            false
        }

        #[cfg(target_os = "macos")]
        {
            let msg_send: extern "C" fn(*mut c_void, *const c_void) -> BOOL = msg_send_fn();
            msg_send(receiver.0, sel.0) == YES
        }
    }

    /// `-[receiver sel:id]` returning `BOOL` (`isEqualToString:`).
    ///
    /// # Safety
    /// See [`ObjcMsg`].
    pub unsafe fn send_id_bool_return(receiver: Id, sel: Sel, arg: Id) -> bool {
        if receiver.is_nil() {
            return false;
        }

        #[cfg(not(target_os = "macos"))]
        {
            let _ = (sel, arg);
            false
        }

        #[cfg(target_os = "macos")]
        {
            let msg_send: extern "C" fn(*mut c_void, *const c_void, *mut c_void) -> BOOL =
                msg_send_fn();
            msg_send(receiver.0, sel.0, arg.0) == YES
        }
    }

    /// `-[receiver sel]` returning a `CGRect` (`frame`, `visibleFrame`).
    ///
    /// # Safety
    /// See [`ObjcMsg`].
    pub unsafe fn send_rect_return(receiver: Id, sel: Sel) -> CGRect {
        if receiver.is_nil() {
            return CGRect::ZERO;
        }

        #[cfg(not(target_os = "macos"))]
        {
            let runtime = MockObjcRuntime::instance();
            let sel_name = mock_selector_name(sel);

            if sel_name == "visibleFrame" {
                runtime
                    .get_object_data(receiver)
                    .map(|d| d.visible_frame)
                    .unwrap_or(CGRect::new(0.0, 25.0, 1920.0, 1055.0))
            } else {
                runtime
                    .get_object_data(receiver)
                    .map(|d| d.frame)
                    .unwrap_or(CGRect::new(0.0, 0.0, 1920.0, 1080.0))
            }
        }

        #[cfg(target_os = "macos")]
        {
            #[cfg(target_arch = "aarch64")]
            {
                let msg_send: extern "C" fn(*mut c_void, *const c_void) -> CGRect = msg_send_fn();
                msg_send(receiver.0, sel.0)
            }
            #[cfg(target_arch = "x86_64")]
            {
                extern "C" {
                    fn objc_msgSend_stret(
                        stret_addr: *mut CGRect,
                        self_ptr: *mut c_void,
                        op: *const c_void,
                    );
                }
                let mut result = std::mem::MaybeUninit::<CGRect>::uninit();
                objc_msgSend_stret(result.as_mut_ptr(), receiver.0, sel.0);
                result.assume_init()
            }
        }
    }

    /// `-[receiver sel]` returning a `double` (`backingScaleFactor`).
    ///
    /// # Safety
    /// See [`ObjcMsg`].
    pub unsafe fn send_f64_return(receiver: Id, sel: Sel) -> f64 {
        if receiver.is_nil() {
            return 1.0;
        }

        #[cfg(not(target_os = "macos"))]
        {
            let _ = sel;
            MockObjcRuntime::instance()
                .get_object_data(receiver)
                .map(|d| d.scale_factor)
                .unwrap_or(2.0)
        }

        #[cfg(target_os = "macos")]
        {
            let msg_send: extern "C" fn(*mut c_void, *const c_void) -> f64 = msg_send_fn();
            msg_send(receiver.0, sel.0)
        }
    }

    /// `-[receiver sel]` returning an `NSUInteger` (`[NSArray count]`).
    ///
    /// # Safety
    /// See [`ObjcMsg`].
    pub unsafe fn send_usize_return(receiver: Id, sel: Sel) -> usize {
        if receiver.is_nil() {
            return 0;
        }

        #[cfg(not(target_os = "macos"))]
        {
            let _ = sel;
            MockObjcRuntime::instance()
                .get_object_data(receiver)
                .map(|d| d.children.len())
                .unwrap_or(0)
        }

        #[cfg(target_os = "macos")]
        {
            let msg_send: extern "C" fn(*mut c_void, *const c_void) -> NSUInteger = msg_send_fn();
            msg_send(receiver.0, sel.0)
        }
    }

    /// `-[receiver sel:NSUInteger]` returning an object (`[NSArray objectAtIndex:]`).
    ///
    /// # Safety
    /// See [`ObjcMsg`].
    pub unsafe fn send_id_with_usize(receiver: Id, sel: Sel, idx: usize) -> Id {
        if receiver.is_nil() {
            return Id::NIL;
        }

        #[cfg(not(target_os = "macos"))]
        {
            let _ = sel;
            MockObjcRuntime::instance()
                .get_object_data(receiver)
                .and_then(|d| d.children.get(idx).copied())
                .unwrap_or(Id::NIL)
        }

        #[cfg(target_os = "macos")]
        {
            let msg_send: extern "C" fn(*mut c_void, *const c_void, NSUInteger) -> *mut c_void =
                msg_send_fn();
            Id(msg_send(receiver.0, sel.0, idx))
        }
    }

    /// `[[NSBitmapImageRep alloc] initWithBitmapDataPlanes:NULL pixelsWide:w pixelsHigh:h
    /// bitsPerSample:8 samplesPerPixel:4 hasAlpha:YES isPlanar:NO colorSpaceName:NSDeviceRGBColorSpace
    /// bytesPerRow:w*4 bitsPerPixel:32]`, then copies `rgba` (premultiplied RGBA8, `w * h * 4`
    /// bytes, the layout AppKit's default bitmap format expects) into its pixel buffer.
    ///
    /// `receiver` is the `alloc`ed (+1) `NSBitmapImageRep`; the result is that +1 object, or nil
    /// when the size is zero, `rgba` is too short, or AppKit fails. On nil the receiver is
    /// already released, so the caller must not release it again.
    ///
    /// # Safety
    /// See [`ObjcMsg`].
    pub unsafe fn send_bitmap_rep_init(
        receiver: Id,
        width: usize,
        height: usize,
        rgba: &[u8],
    ) -> Id {
        if receiver.is_nil() {
            return Id::NIL;
        }
        let len = width
            .checked_mul(height)
            .and_then(|n| n.checked_mul(4))
            .filter(|&n| n != 0 && rgba.len() >= n);
        let Some(len) = len else {
            // Consume the +1 `alloc`: an uninitialized object must still be released.
            Self::send_0(receiver, Sel::register("release"));
            return Id::NIL;
        };

        #[cfg(not(target_os = "macos"))]
        {
            MockObjcRuntime::instance().update_object(receiver, |data| {
                data.pixels = rgba[..len].to_vec();
                data.frame = CGRect::new(0.0, 0.0, width as f64, height as f64);
            });
            receiver
        }

        #[cfg(target_os = "macos")]
        {
            let color_space = nsstring_from_str("NSDeviceRGBColorSpace");
            let msg_send: extern "C" fn(
                *mut c_void,
                *const c_void,
                *mut *mut u8,
                NSInteger,
                NSInteger,
                NSInteger,
                NSInteger,
                BOOL,
                BOOL,
                *mut c_void,
                NSInteger,
                NSInteger,
            ) -> *mut c_void = msg_send_fn();
            let rep = Id(msg_send(
                receiver.0,
                Sel::register(
                    "initWithBitmapDataPlanes:pixelsWide:pixelsHigh:bitsPerSample:samplesPerPixel:hasAlpha:isPlanar:colorSpaceName:bytesPerRow:bitsPerPixel:",
                )
                .0,
                std::ptr::null_mut(),
                width as NSInteger,
                height as NSInteger,
                8,
                4,
                YES,
                NO,
                color_space.0,
                (width * 4) as NSInteger,
                32,
            ));
            Self::send_0(color_space, Sel::register("release"));
            if rep.is_nil() {
                return Id::NIL;
            }
            let data = Self::send_0(rep, Sel::register("bitmapData")).0 as *mut u8;
            if data.is_null() {
                Self::send_0(rep, Sel::register("release"));
                return Id::NIL;
            }
            // SAFETY: `bitmapData` points at `bytesPerRow * height == len` bytes owned by `rep`,
            // and `rgba` holds at least `len` bytes (checked above); the buffers are distinct.
            std::ptr::copy_nonoverlapping(rgba.as_ptr(), data, len);
            rep
        }
    }
}

// ---------------------------------------------------------------------------
// Action targets: the Objective-C object AppKit controls and menu items call back into
// ---------------------------------------------------------------------------

/// Selector every [`ActionTarget`] implements: `-ferrokitPerformAction:`.
pub const ACTION_TARGET_SELECTOR: &str = "ferrokitPerformAction:";

type ActionCallback = std::rc::Rc<dyn Fn()>;

thread_local! {
    /// Callbacks by target object address. AppKit delivers actions on the main thread, the thread
    /// that created the target (`ActionTarget` is `!Send`).
    static ACTION_CALLBACKS: std::cell::RefCell<HashMap<usize, ActionCallback>> =
        std::cell::RefCell::new(HashMap::new());
}

/// The class behind [`ActionTarget`]: an `NSObject` subclass named `FerrokitActionTarget` whose
/// `-ferrokitPerformAction:` runs the target's callback. Registered once, on first use; nil if
/// the runtime refuses the class.
#[cfg(target_os = "macos")]
fn action_target_class() -> Class {
    extern "C" fn perform_action(this: *mut c_void, _cmd: *const c_void, _sender: *mut c_void) {
        // A panic must not unwind into AppKit.
        let _ = std::panic::catch_unwind(|| ActionTarget::perform(Id(this)));
    }
    static CLASS: std::sync::LazyLock<usize> = std::sync::LazyLock::new(|| unsafe {
        let existing = objc_get_class("FerrokitActionTarget");
        if !existing.is_nil() {
            return existing.0 as usize;
        }
        let class = native_bindings::objc_allocateClassPair(
            objc_get_class("NSObject").0,
            c"FerrokitActionTarget".as_ptr(),
            0,
        );
        if class.is_null() {
            return 0;
        }
        native_bindings::class_addMethod(
            class,
            Sel::register(ACTION_TARGET_SELECTOR).0,
            perform_action as *const c_void,
            c"v@:@".as_ptr(),
        );
        native_bindings::objc_registerClassPair(class);
        class as usize
    });
    Class(*CLASS as *mut c_void)
}

/// An Objective-C object that runs a Rust closure when AppKit sends it
/// [`ACTION_TARGET_SELECTOR`]. Controls and menu items hold their target weakly, so the owner
/// keeps the `ActionTarget` alive for as long as the control can fire (and clears the control's
/// target before dropping it).
pub struct ActionTarget {
    id: Id,
}

impl ActionTarget {
    /// Creates the target object. Errors when the runtime cannot provide the class.
    pub fn new(callback: impl Fn() + 'static) -> Result<Self, &'static str> {
        #[cfg(target_os = "macos")]
        let id = {
            let class = action_target_class();
            if class.is_nil() {
                return Err("Cannot register the action target class");
            }
            // SAFETY: `alloc`/`init` take no arguments and `class` is a registered NSObject
            // subclass; the +1 reference is released in `Drop`.
            unsafe {
                ObjcMsg::send_0(
                    ObjcMsg::send_class_0(class, Sel::register("alloc")),
                    Sel::register("init"),
                )
            }
        };
        #[cfg(not(target_os = "macos"))]
        let id = MockObjcRuntime::instance().allocate_object("FerrokitActionTarget");
        if id.is_nil() {
            return Err("Cannot allocate the action target");
        }
        ACTION_CALLBACKS.with(|callbacks| {
            callbacks
                .borrow_mut()
                .insert(id.0 as usize, std::rc::Rc::new(callback));
        });
        Ok(Self { id })
    }

    /// The object to pass to `setTarget:`.
    #[inline]
    pub fn id(&self) -> Id {
        self.id
    }

    /// The selector to pass to `setAction:`.
    #[inline]
    pub fn action() -> Sel {
        Sel::register(ACTION_TARGET_SELECTOR)
    }

    /// What `-ferrokitPerformAction:` does: runs the callback registered for `target` on this
    /// thread. Returns whether there was one. Tests use it to stand in for a click.
    pub fn perform(target: Id) -> bool {
        let callback = ACTION_CALLBACKS
            .with(|callbacks| callbacks.borrow().get(&(target.0 as usize)).cloned());
        match callback {
            Some(callback) => {
                callback();
                true
            }
            None => false,
        }
    }
}

impl Drop for ActionTarget {
    fn drop(&mut self) {
        ACTION_CALLBACKS.with(|callbacks| {
            callbacks.borrow_mut().remove(&(self.id.0 as usize));
        });
        #[cfg(target_os = "macos")]
        // SAFETY: balances the +1 from `alloc`/`init` in `new`.
        unsafe {
            ObjcMsg::send_0(self.id, Sel::register("release"));
        }
        #[cfg(not(target_os = "macos"))]
        MockObjcRuntime::instance().remove_object(self.id);
    }
}
