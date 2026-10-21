// SPDX-License-Identifier: LGPL-3.0-only
use std::collections::HashMap;
use std::ffi::c_void;
#[cfg(target_os = "macos")]
use std::ffi::{c_char, CStr, CString};
use std::sync::{LazyLock, Mutex};

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
        pub fn sel_getName(sel: *const c_void) -> *const c_char;
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
    let info = ObjcMsg::send_class_0(
        objc_get_class("NSProcessInfo"),
        Sel::register("processInfo"),
    );
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
        let current = ObjcMsg::send_class_0(
            objc_get_class("NSRunningApplication"),
            Sel::register("currentApplication"),
        );
        if !ObjcMsg::send_bool_return(current, Sel::register("isActive")) {
            let app = ObjcMsg::send_class_0(
                objc_get_class("NSApplication"),
                Sel::register("sharedApplication"),
            );
            ObjcMsg::send_bool(app, Sel::register("activateIgnoringOtherApps:"), true);
        }
        if is_macos_at_least(14) {
            // NSApplicationActivateAllWindows
            ObjcMsg::send_int(current, Sel::register("activateWithOptions:"), 1);
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
    let str_obj = ObjcMsg::send_class_0(cls, sel_alloc);
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
        }
    }
}

pub struct MockObjcRuntime {
    classes: Mutex<HashMap<String, usize>>,
    selectors: Mutex<HashMap<String, usize>>,
    objects: Mutex<HashMap<usize, MockObjectData>>,
    next_id: Mutex<usize>,
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
}

// ---------------------------------------------------------------------------
// Message send wrapper (msg_send mock and native dispatch)
// ---------------------------------------------------------------------------

pub struct ObjcMsg;

impl ObjcMsg {
    /// Sends a zero-argument message
    pub fn send_0(receiver: Id, sel: Sel) -> Id {
        if receiver.is_nil() {
            return Id::NIL;
        }

        #[cfg(target_os = "macos")]
        unsafe {
            let msg_send: extern "C" fn(*mut c_void, *const c_void) -> *mut c_void =
                std::mem::transmute(native_bindings::objc_msgSend as *const ());
            Id(msg_send(receiver.0, sel.0))
        }

        #[cfg(not(target_os = "macos"))]
        {
            let runtime = MockObjcRuntime::instance();
            let sel_name = runtime
                .selectors
                .lock()
                .unwrap()
                .iter()
                .find(|(_, &v)| v == (sel.0 as usize))
                .map(|(k, _)| k.clone())
                .unwrap_or_default();

            if sel_name == "button" {
                if let Some(data) = runtime.get_object_data(receiver) {
                    return data.button;
                }
            } else if sel_name == "menu" {
                if let Some(data) = runtime.get_object_data(receiver) {
                    return data.menu;
                }
            }
            receiver
        }
    }

    /// Sends a class factory message (e.g., [NSMenu alloc], [NSStatusBar systemStatusBar])
    pub fn send_class_0(class: Class, sel: Sel) -> Id {
        if class.is_nil() {
            return Id::NIL;
        }

        #[cfg(target_os = "macos")]
        unsafe {
            let msg_send: extern "C" fn(*mut c_void, *const c_void) -> *mut c_void =
                std::mem::transmute(native_bindings::objc_msgSend as *const ());
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

            let sel_name = runtime
                .selectors
                .lock()
                .unwrap()
                .iter()
                .find(|(_, &v)| v == (sel.0 as usize))
                .map(|(k, _)| k.clone())
                .unwrap_or_default();

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

    /// Sends a message with 1 string argument (e.g., initWithTitle:, setTitle:)
    #[allow(unused_variables)]
    pub fn send_str(receiver: Id, sel: Sel, text: &str) -> Id {
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
        unsafe {
            let ns_title = nsstring_from_str(text);
            let sel_name_ptr = native_bindings::sel_getName(sel.0 as *const c_void);
            let sel_str = if !sel_name_ptr.is_null() {
                CStr::from_ptr(sel_name_ptr).to_str().unwrap_or_default()
            } else {
                ""
            };

            if sel_str == "initWithTitle:action:keyEquivalent:" {
                let empty_key = nsstring_from_str("");
                let msg_send: extern "C" fn(
                    *mut c_void,
                    *const c_void,
                    *mut c_void,
                    *const c_void,
                    *mut c_void,
                ) -> *mut c_void = std::mem::transmute(native_bindings::objc_msgSend as *const ());
                Id(msg_send(
                    receiver.0,
                    sel.0,
                    ns_title.0,
                    std::ptr::null(),
                    empty_key.0,
                ))
            } else {
                let msg_send: extern "C" fn(
                    *mut c_void,
                    *const c_void,
                    *mut c_void,
                ) -> *mut c_void = std::mem::transmute(native_bindings::objc_msgSend as *const ());
                Id(msg_send(receiver.0, sel.0, ns_title.0))
            }
        }
    }

    /// Sends a message with 1 object argument (e.g., setMenu:, addItem:, addSubview:, removeStatusItem:)
    pub fn send_id(receiver: Id, sel: Sel, arg: Id) -> Id {
        if receiver.is_nil() {
            return Id::NIL;
        }

        #[cfg(not(target_os = "macos"))]
        {
            let runtime = MockObjcRuntime::instance();
            let sel_name = runtime
                .selectors
                .lock()
                .unwrap()
                .iter()
                .find(|(_, &v)| v == (sel.0 as usize))
                .map(|(k, _)| k.clone())
                .unwrap_or_default();

            if sel_name == "setMenu:" {
                runtime.update_object(receiver, |data| {
                    data.menu = arg;
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
            } else if sel_name == "removeStatusItem:" {
                runtime.remove_object(arg);
            }
            receiver
        }

        #[cfg(target_os = "macos")]
        unsafe {
            let msg_send: extern "C" fn(*mut c_void, *const c_void, *mut c_void) -> *mut c_void =
                std::mem::transmute(native_bindings::objc_msgSend as *const ());
            Id(msg_send(receiver.0, sel.0, arg.0))
        }
    }

    /// Sends a message with 1 float/length argument (e.g., statusItemWithLength:)
    pub fn send_length(receiver: Id, sel: Sel, length: CGFloat) -> Id {
        if receiver.is_nil() {
            return Id::NIL;
        }

        #[cfg(not(target_os = "macos"))]
        {
            let _ = (sel, length);
            MockObjcRuntime::instance().allocate_object("NSStatusItem")
        }

        #[cfg(target_os = "macos")]
        unsafe {
            let msg_send: extern "C" fn(*mut c_void, *const c_void, CGFloat) -> *mut c_void =
                std::mem::transmute(native_bindings::objc_msgSend as *const ());
            Id(msg_send(receiver.0, sel.0, length))
        }
    }

    /// Sends a message with an integer argument (e.g., setLevel:, setState:)
    pub fn send_int(receiver: Id, sel: Sel, val: NSInteger) -> Id {
        if receiver.is_nil() {
            return Id::NIL;
        }

        #[cfg(not(target_os = "macos"))]
        {
            let runtime = MockObjcRuntime::instance();
            let sel_name = runtime
                .selectors
                .lock()
                .unwrap()
                .iter()
                .find(|(_, &v)| v == (sel.0 as usize))
                .map(|(k, _)| k.clone())
                .unwrap_or_default();

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
        unsafe {
            let msg_send: extern "C" fn(*mut c_void, *const c_void, NSInteger) -> *mut c_void =
                std::mem::transmute(native_bindings::objc_msgSend as *const ());
            Id(msg_send(receiver.0, sel.0, val))
        }
    }

    /// Sends a message with a boolean argument (e.g., setIgnoresMouseEvents:, setEnabled:)
    pub fn send_bool(receiver: Id, sel: Sel, val: bool) -> Id {
        if receiver.is_nil() {
            return Id::NIL;
        }

        #[cfg(not(target_os = "macos"))]
        {
            let runtime = MockObjcRuntime::instance();
            let sel_name = runtime
                .selectors
                .lock()
                .unwrap()
                .iter()
                .find(|(_, &v)| v == (sel.0 as usize))
                .map(|(k, _)| k.clone())
                .unwrap_or_default();

            if sel_name == "setIgnoresMouseEvents:" {
                runtime.update_object(receiver, |data| {
                    data.ignores_mouse_events = val;
                });
            } else if sel_name == "setEnabled:" {
                runtime.update_object(receiver, |data| {
                    data.enabled = val;
                });
            }
            receiver
        }

        #[cfg(target_os = "macos")]
        unsafe {
            let bool_val: BOOL = if val { YES } else { NO };
            let msg_send: extern "C" fn(*mut c_void, *const c_void, BOOL) -> *mut c_void =
                std::mem::transmute(native_bindings::objc_msgSend as *const ());
            Id(msg_send(receiver.0, sel.0, bool_val))
        }
    }

    /// Sends a message with a CGSize argument (e.g., [NSWindow setMinSize:])
    pub fn send_size(receiver: Id, sel: Sel, size: CGSize) -> Id {
        if receiver.is_nil() {
            return Id::NIL;
        }

        #[cfg(target_os = "macos")]
        unsafe {
            let msg_send: extern "C" fn(*mut c_void, *const c_void, CGSize) -> *mut c_void =
                std::mem::transmute(native_bindings::objc_msgSend as *const ());
            Id(msg_send(receiver.0, sel.0, size))
        }

        #[cfg(not(target_os = "macos"))]
        {
            let _ = (sel, size);
            receiver
        }
    }

    /// Sends window initialization message (initWithContentRect:styleMask:backing:defer:)
    #[allow(unused_variables)]
    pub fn send_window_init(
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
            });
            receiver
        }

        #[cfg(target_os = "macos")]
        unsafe {
            let sel_name_ptr = native_bindings::sel_getName(sel.0 as *const c_void);
            let sel_str = if !sel_name_ptr.is_null() {
                CStr::from_ptr(sel_name_ptr).to_str().unwrap_or_default()
            } else {
                ""
            };

            if sel_str.starts_with("initWithFrame:") {
                let msg_send: extern "C" fn(*mut c_void, *const c_void, CGRect) -> *mut c_void =
                    std::mem::transmute(native_bindings::objc_msgSend as *const ());
                Id(msg_send(receiver.0, sel.0, rect))
            } else if sel_str.starts_with("setFrame:display:") {
                let msg_send: extern "C" fn(
                    *mut c_void,
                    *const c_void,
                    CGRect,
                    BOOL,
                ) -> *mut c_void = std::mem::transmute(native_bindings::objc_msgSend as *const ());
                Id(msg_send(receiver.0, sel.0, rect, YES))
            } else {
                let defer_b: BOOL = if defer_flag { YES } else { NO };
                let msg_send: extern "C" fn(
                    *mut c_void,
                    *const c_void,
                    CGRect,
                    NSUInteger,
                    NSUInteger,
                    BOOL,
                ) -> *mut c_void = std::mem::transmute(native_bindings::objc_msgSend as *const ());
                Id(msg_send(
                    receiver.0, sel.0, rect, style_mask, backing, defer_b,
                ))
            }
        }
    }

    /// Sends a zero-argument message and returns a boolean value (e.g., isFlipped)
    pub fn send_bool_return(receiver: Id, sel: Sel) -> bool {
        if receiver.is_nil() {
            return false;
        }

        #[cfg(not(target_os = "macos"))]
        {
            let runtime = MockObjcRuntime::instance();
            let sel_name = runtime
                .selectors
                .lock()
                .unwrap()
                .iter()
                .find(|(_, &v)| v == (sel.0 as usize))
                .map(|(k, _)| k.clone())
                .unwrap_or_default();

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
            let sel = sel.0 as *const c_void;
            let ptr = receiver.0;
            let res: BOOL = unsafe {
                let msg_send: unsafe extern "C" fn(*mut c_void, *const c_void) -> BOOL =
                    std::mem::transmute(native_bindings::objc_msgSend as *const ());
                msg_send(ptr, sel)
            };
            res == YES
        }
    }

    /// Sends a message with 1 object argument that returns a BOOL (e.g., isEqualToString:).
    pub fn send_id_bool_return(receiver: Id, sel: Sel, arg: Id) -> bool {
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
            let res: BOOL = unsafe {
                let msg_send: unsafe extern "C" fn(
                    *mut c_void,
                    *const c_void,
                    *mut c_void,
                ) -> BOOL = std::mem::transmute(native_bindings::objc_msgSend as *const ());
                msg_send(receiver.0, sel.0, arg.0)
            };
            res == YES
        }
    }

    /// Sends a zero-argument message and returns a CGRect struct value (e.g., frame, visibleFrame)
    pub fn send_rect_return(receiver: Id, sel: Sel) -> CGRect {
        if receiver.is_nil() {
            return CGRect::ZERO;
        }

        #[cfg(not(target_os = "macos"))]
        {
            let runtime = MockObjcRuntime::instance();
            let sel_name = runtime
                .selectors
                .lock()
                .unwrap()
                .iter()
                .find(|(_, &v)| v == (sel.0 as usize))
                .map(|(k, _)| k.clone())
                .unwrap_or_default();

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
        unsafe {
            #[cfg(target_arch = "aarch64")]
            {
                let msg_send: extern "C" fn(*mut c_void, *const c_void) -> CGRect =
                    std::mem::transmute(native_bindings::objc_msgSend as *const ());
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

    /// Sends a zero-argument message and returns a 64-bit float (e.g., backingScaleFactor)
    pub fn send_f64_return(receiver: Id, _sel: Sel) -> f64 {
        if receiver.is_nil() {
            return 1.0;
        }

        #[cfg(not(target_os = "macos"))]
        {
            let runtime = MockObjcRuntime::instance();
            runtime
                .get_object_data(receiver)
                .map(|d| d.scale_factor)
                .unwrap_or(2.0)
        }

        #[cfg(target_os = "macos")]
        unsafe {
            let msg_send: extern "C" fn(*mut c_void, *const c_void) -> f64 =
                std::mem::transmute(native_bindings::objc_msgSend as *const ());
            msg_send(receiver.0, _sel.0)
        }
    }

    /// Sends a zero-argument message and returns a usize (e.g., [NSArray count])
    pub fn send_usize_return(receiver: Id, _sel: Sel) -> usize {
        if receiver.is_nil() {
            return 0;
        }

        #[cfg(not(target_os = "macos"))]
        {
            let runtime = MockObjcRuntime::instance();
            runtime
                .get_object_data(receiver)
                .map(|d| d.children.len())
                .unwrap_or(0)
        }

        #[cfg(target_os = "macos")]
        unsafe {
            let msg_send: extern "C" fn(*mut c_void, *const c_void) -> NSUInteger =
                std::mem::transmute(native_bindings::objc_msgSend as *const ());
            msg_send(receiver.0, _sel.0)
        }
    }

    /// Sends an index lookup message (e.g., [NSArray objectAtIndex:])
    pub fn send_id_with_usize(receiver: Id, _sel: Sel, idx: usize) -> Id {
        if receiver.is_nil() {
            return Id::NIL;
        }

        #[cfg(not(target_os = "macos"))]
        {
            let runtime = MockObjcRuntime::instance();
            runtime
                .get_object_data(receiver)
                .and_then(|d| d.children.get(idx).copied())
                .unwrap_or(Id::NIL)
        }

        #[cfg(target_os = "macos")]
        unsafe {
            let msg_send: extern "C" fn(*mut c_void, *const c_void, NSUInteger) -> *mut c_void =
                std::mem::transmute(native_bindings::objc_msgSend as *const ());
            Id(msg_send(receiver.0, _sel.0, idx))
        }
    }
}
