// SPDX-License-Identifier: LGPL-3.0-only
//! AppKit messages `window_cocoa` needs beyond the original [`ObjcMsg`] set, the mock runtime's
//! property store and call log that make them testable on any OS, and the Objective-C classes
//! whose methods call back into Rust (`QNSView`'s input methods, `QNSWindow`).
//!
//! Every function here follows the contract documented on [`ObjcMsg`]. The `*_logged` family
//! additionally appends to the mock's call log (`MockObjcRuntime::calls`), so a test sees the
//! exact sequence of messages a window operation sent, not only the state the mock models.

use super::*;

/// `NSNotFound` (`NSIntegerMax`): "no such index" in `NSRange` based APIs.
pub const NS_NOT_FOUND: NSUInteger = isize::MAX as NSUInteger;

/// `NSRange`, passed and returned by value by the `NSTextInputClient` methods.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NSRange {
    pub location: NSUInteger,
    pub length: NSUInteger,
}

impl NSRange {
    /// `{NSNotFound, 0}`: no range.
    pub const NOT_FOUND: Self = Self {
        location: NS_NOT_FOUND,
        length: 0,
    };
}

// ---------------------------------------------------------------------------
// Mock runtime: property store and call log
// ---------------------------------------------------------------------------

/// A value the mock runtime returns for a property read or records for a message argument.
#[cfg(not(target_os = "macos"))]
#[derive(Debug, Clone, PartialEq)]
pub enum MockValue {
    Bool(bool),
    Int(i64),
    Float(f64),
    Point(CGPoint),
    Rect(CGRect),
    Id(Id),
    Str(String),
}

// SAFETY: in the mock runtime an `Id` is an integer handle, never dereferenced, so a `MockValue`
// holds no foreign state that could be touched from another thread (same as `MockObjectData`).
#[cfg(not(target_os = "macos"))]
unsafe impl Send for MockValue {}

/// One message recorded by a `*_logged` function: `[receiver selector args...]`.
#[cfg(not(target_os = "macos"))]
#[derive(Debug, Clone, PartialEq)]
pub struct MockCall {
    pub receiver: Id,
    pub selector: String,
    pub args: Vec<MockValue>,
}

// SAFETY: see `MockValue`.
#[cfg(not(target_os = "macos"))]
unsafe impl Send for MockCall {}

#[cfg(not(target_os = "macos"))]
#[derive(Default)]
struct MockAppKitState {
    /// `(object, selector)` -> value of that read-only property (`[event keyCode]`, ...).
    properties: HashMap<(usize, String), MockValue>,
    calls: Vec<MockCall>,
    /// Shared `NSCursor` instances by class-method name, like AppKit's immortal singletons.
    cursors: HashMap<String, usize>,
    /// `[NSApp currentEvent]`, as an object handle (0: nil).
    current_event: usize,
}

#[cfg(not(target_os = "macos"))]
static MOCK_APPKIT: LazyLock<Mutex<MockAppKitState>> = LazyLock::new(Default::default);

#[cfg(not(target_os = "macos"))]
fn mock_state() -> std::sync::MutexGuard<'static, MockAppKitState> {
    MOCK_APPKIT.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(not(target_os = "macos"))]
impl MockObjcRuntime {
    /// Makes `[receiver selector]` return `value` from the property reads of this module
    /// (`ObjcMsg::send_point_return`, `send_isize_return`, `send_id_prop`, ...).
    pub fn set_property(&self, receiver: Id, selector: &str, value: MockValue) {
        mock_state()
            .properties
            .insert((receiver.0 as usize, selector.to_string()), value);
    }

    /// The value `set_property` stored for `[receiver selector]`, if any.
    pub fn property(&self, receiver: Id, selector: &str) -> Option<MockValue> {
        mock_state()
            .properties
            .get(&(receiver.0 as usize, selector.to_string()))
            .cloned()
    }

    /// Forgets a property, so its read returns the default again.
    pub fn clear_property(&self, receiver: Id, selector: &str) {
        mock_state()
            .properties
            .remove(&(receiver.0 as usize, selector.to_string()));
    }

    /// What `[NSApp currentEvent]` returns: the event being dispatched, nil outside of one.
    pub fn set_current_event(&self, event: Id) {
        mock_state().current_event = event.0 as usize;
    }

    /// The event `set_current_event` stored, nil if none.
    pub fn current_event(&self) -> Id {
        Id(mock_state().current_event as *mut c_void)
    }

    /// A new mock object of `class_name` whose properties are `properties`.
    pub fn object_with(&self, class_name: &str, properties: &[(&str, MockValue)]) -> Id {
        let object = self.allocate_object(class_name);
        for (selector, value) in properties {
            self.set_property(object, selector, value.clone());
        }
        object
    }

    /// Every message recorded so far, oldest first.
    pub fn calls(&self) -> Vec<MockCall> {
        mock_state().calls.clone()
    }

    /// The recorded messages sent to `receiver`, oldest first.
    pub fn calls_for(&self, receiver: Id) -> Vec<MockCall> {
        mock_state()
            .calls
            .iter()
            .filter(|call| call.receiver == receiver)
            .cloned()
            .collect()
    }

    /// Empties the call log.
    pub fn clear_calls(&self) {
        mock_state().calls.clear();
    }
}

#[cfg(not(target_os = "macos"))]
fn record(receiver: Id, sel: Sel, args: Vec<MockValue>) {
    mock_state().calls.push(MockCall {
        receiver,
        selector: mock_selector_name(sel),
        args,
    });
}

#[cfg(not(target_os = "macos"))]
fn mock_property(receiver: Id, sel: Sel) -> Option<MockValue> {
    MockObjcRuntime::instance().property(receiver, &mock_selector_name(sel))
}

/// `-[NSWindow childWindows]` as the mock keeps it: a new `NSArray` of the windows whose
/// `parentWindow` (set by `addChildWindow:ordered:`, cleared by `removeChildWindow:`) is `parent`.
#[cfg(not(target_os = "macos"))]
fn mock_child_windows(parent: Id) -> Id {
    let mut children: Vec<usize> = mock_state()
        .properties
        .iter()
        .filter(|((_, selector), value)| {
            selector == "parentWindow" && **value == MockValue::Id(parent)
        })
        .map(|((object, _), _)| *object)
        .collect();
    children.sort_unstable();
    let runtime = MockObjcRuntime::instance();
    let array = runtime.allocate_object("NSArray");
    runtime.update_object(array, |data| {
        data.children
            .extend(children.iter().map(|&child| Id(child as *mut c_void)))
    });
    array
}

// ---------------------------------------------------------------------------
// Typed messages
// ---------------------------------------------------------------------------

impl ObjcMsg {
    /// `-[receiver sel]` returning an `NSPoint` (`-[NSEvent locationInWindow]`). The mock returns
    /// the property of that name, `(0, 0)` if unset.
    ///
    /// # Safety
    /// See [`ObjcMsg`].
    pub unsafe fn send_point_return(receiver: Id, sel: Sel) -> CGPoint {
        if receiver.is_nil() {
            return CGPoint::ZERO;
        }

        #[cfg(target_os = "macos")]
        // SAFETY: caller guarantees receiver responds to sel with matching types.
        unsafe {
            let msg_send: extern "C" fn(*mut c_void, *const c_void) -> CGPoint = msg_send_fn();
            msg_send(receiver.0, sel.0)
        }

        #[cfg(not(target_os = "macos"))]
        {
            match mock_property(receiver, sel) {
                Some(MockValue::Point(point)) => point,
                _ => CGPoint::ZERO,
            }
        }
    }

    /// `-[receiver sel:NSPoint]` returning an `NSPoint` (`-[NSWindow convertPointToScreen:]`,
    /// `convertPointFromScreen:`). The mock treats the window's frame origin as its screen origin
    /// (no title bar, a content rectangle equal to the frame).
    ///
    /// # Safety
    /// See [`ObjcMsg`].
    pub unsafe fn send_point_arg_return(receiver: Id, sel: Sel, point: CGPoint) -> CGPoint {
        if receiver.is_nil() {
            return CGPoint::ZERO;
        }

        #[cfg(target_os = "macos")]
        // SAFETY: caller guarantees receiver responds to sel with matching types.
        unsafe {
            let msg_send: extern "C" fn(*mut c_void, *const c_void, CGPoint) -> CGPoint =
                msg_send_fn();
            msg_send(receiver.0, sel.0, point)
        }

        #[cfg(not(target_os = "macos"))]
        {
            let origin = MockObjcRuntime::instance()
                .get_object_data(receiver)
                .map(|d| d.frame.origin)
                .unwrap_or(CGPoint::ZERO);
            let sign = if mock_selector_name(sel) == "convertPointFromScreen:" {
                -1.0
            } else {
                1.0
            };
            CGPoint::new(point.x + sign * origin.x, point.y + sign * origin.y)
        }
    }

    /// `-[receiver sel:NSPoint view:id]` returning an `NSPoint` (`-[NSView convertPoint:fromView:]`
    /// and `convertPoint:toView:`; a nil `view` is the window's base coordinate system, whose
    /// origin is at the bottom left).
    ///
    /// The mock puts the view's frame origin in base coordinates and honours `isFlipped`: a
    /// flipped view's y axis grows downward from the top edge of its frame. A non-nil `view` is
    /// treated like nil.
    ///
    /// # Safety
    /// See [`ObjcMsg`].
    pub unsafe fn send_convert_point(receiver: Id, sel: Sel, point: CGPoint, view: Id) -> CGPoint {
        if receiver.is_nil() {
            return point;
        }

        #[cfg(target_os = "macos")]
        // SAFETY: caller guarantees receiver responds to sel with matching types.
        unsafe {
            let msg_send: extern "C" fn(
                *mut c_void,
                *const c_void,
                CGPoint,
                *mut c_void,
            ) -> CGPoint = msg_send_fn();
            msg_send(receiver.0, sel.0, point, view.0)
        }

        #[cfg(not(target_os = "macos"))]
        {
            let _ = view;
            let Some(data) = MockObjcRuntime::instance().get_object_data(receiver) else {
                return point;
            };
            let frame = data.frame;
            if mock_selector_name(sel) == "convertPoint:toView:" {
                // View coordinates -> base coordinates.
                let y = if data.is_flipped {
                    frame.origin.y + frame.size.height - point.y
                } else {
                    frame.origin.y + point.y
                };
                CGPoint::new(frame.origin.x + point.x, y)
            } else {
                // Base coordinates -> view coordinates.
                let y = if data.is_flipped {
                    frame.origin.y + frame.size.height - point.y
                } else {
                    point.y - frame.origin.y
                };
                CGPoint::new(point.x - frame.origin.x, y)
            }
        }
    }

    /// `-[receiver sel]` returning an `NSInteger` (`-[NSEvent buttonNumber]`, `clickCount`). The
    /// mock returns the property of that name, 0 if unset.
    ///
    /// # Safety
    /// See [`ObjcMsg`].
    pub unsafe fn send_isize_return(receiver: Id, sel: Sel) -> NSInteger {
        if receiver.is_nil() {
            return 0;
        }

        #[cfg(target_os = "macos")]
        // SAFETY: caller guarantees receiver responds to sel with matching types.
        unsafe {
            let msg_send: extern "C" fn(*mut c_void, *const c_void) -> NSInteger = msg_send_fn();
            msg_send(receiver.0, sel.0)
        }

        #[cfg(not(target_os = "macos"))]
        {
            match mock_property(receiver, sel) {
                Some(MockValue::Int(value)) => value as NSInteger,
                _ => 0,
            }
        }
    }

    /// `-[receiver sel]` returning an `NSUInteger` (`-[NSEvent modifierFlags]`). The mock returns
    /// the property of that name, 0 if unset.
    ///
    /// # Safety
    /// See [`ObjcMsg`].
    pub unsafe fn send_nsuinteger_return(receiver: Id, sel: Sel) -> NSUInteger {
        if receiver.is_nil() {
            return 0;
        }

        #[cfg(target_os = "macos")]
        // SAFETY: caller guarantees receiver responds to sel with matching types.
        unsafe {
            let msg_send: extern "C" fn(*mut c_void, *const c_void) -> NSUInteger = msg_send_fn();
            msg_send(receiver.0, sel.0)
        }

        #[cfg(not(target_os = "macos"))]
        {
            match mock_property(receiver, sel) {
                Some(MockValue::Int(value)) => value as NSUInteger,
                _ => 0,
            }
        }
    }

    /// `-[receiver sel]` returning an `unsigned short` (`-[NSEvent keyCode]`). The return type
    /// matters: reading a 16-bit result as a wider integer picks up whatever the callee left in
    /// the upper register bits.
    ///
    /// # Safety
    /// See [`ObjcMsg`].
    pub unsafe fn send_u16_return(receiver: Id, sel: Sel) -> u16 {
        if receiver.is_nil() {
            return 0;
        }

        #[cfg(target_os = "macos")]
        // SAFETY: caller guarantees receiver responds to sel with matching types.
        unsafe {
            let msg_send: extern "C" fn(*mut c_void, *const c_void) -> u16 = msg_send_fn();
            msg_send(receiver.0, sel.0)
        }

        #[cfg(not(target_os = "macos"))]
        {
            match mock_property(receiver, sel) {
                Some(MockValue::Int(value)) => value as u16,
                _ => 0,
            }
        }
    }

    /// `-[receiver sel]` returning a `CGFloat` (`-[NSEvent scrollingDeltaX]`, `deltaY`). The mock
    /// returns the property of that name, 0 if unset.
    ///
    /// # Safety
    /// See [`ObjcMsg`].
    pub unsafe fn send_cgfloat_return(receiver: Id, sel: Sel) -> CGFloat {
        if receiver.is_nil() {
            return 0.0;
        }

        #[cfg(target_os = "macos")]
        // SAFETY: caller guarantees receiver responds to sel with matching types.
        unsafe {
            let msg_send: extern "C" fn(*mut c_void, *const c_void) -> CGFloat = msg_send_fn();
            msg_send(receiver.0, sel.0)
        }

        #[cfg(not(target_os = "macos"))]
        {
            match mock_property(receiver, sel) {
                Some(MockValue::Float(value)) => value,
                _ => 0.0,
            }
        }
    }

    /// `-[receiver sel]` returning a `BOOL` that is state of the receiver, not something the
    /// mock models itself (`-[NSEvent isARepeat]`, `-[CALayer isGeometryFlipped]`). The mock
    /// returns the property of that name, false if unset.
    ///
    /// # Safety
    /// See [`ObjcMsg`].
    pub unsafe fn send_bool_prop(receiver: Id, sel: Sel) -> bool {
        if receiver.is_nil() {
            return false;
        }

        #[cfg(target_os = "macos")]
        // SAFETY: caller guarantees receiver responds to sel with matching types.
        unsafe {
            let msg_send: extern "C" fn(*mut c_void, *const c_void) -> BOOL = msg_send_fn();
            msg_send(receiver.0, sel.0) == YES
        }

        #[cfg(not(target_os = "macos"))]
        {
            matches!(mock_property(receiver, sel), Some(MockValue::Bool(true)))
        }
    }

    /// `-[receiver sel]` returning an object that is state of the receiver (`-[NSWindow
    /// parentWindow]`, `-[NSEvent window]`). The mock returns the property of that name, nil if
    /// unset (unlike `send_0`, which returns the receiver itself), and for `childWindows` an
    /// `NSArray` of the windows whose `parentWindow` is the receiver.
    ///
    /// # Safety
    /// See [`ObjcMsg`].
    pub unsafe fn send_id_prop(receiver: Id, sel: Sel) -> Id {
        if receiver.is_nil() {
            return Id::NIL;
        }

        #[cfg(target_os = "macos")]
        // SAFETY: caller guarantees receiver responds to sel with matching types.
        unsafe {
            Self::send_0(receiver, sel)
        }

        #[cfg(not(target_os = "macos"))]
        {
            if mock_selector_name(sel) == "childWindows" {
                return mock_child_windows(receiver);
            }
            match mock_property(receiver, sel) {
                Some(MockValue::Id(id)) => id,
                _ => Id::NIL,
            }
        }
    }

    /// `-[receiver sel]` returning an `NSString` (`-[NSEvent characters]`) as a Rust string; `None`
    /// for nil. The mock returns the string property of that name.
    ///
    /// # Safety
    /// See [`ObjcMsg`].
    pub unsafe fn send_string_prop(receiver: Id, sel: Sel) -> Option<String> {
        if receiver.is_nil() {
            return None;
        }

        #[cfg(target_os = "macos")]
        // SAFETY: caller guarantees receiver responds to sel with matching types.
        unsafe {
            ns_string_to_rust(Self::send_0(receiver, sel))
        }

        #[cfg(not(target_os = "macos"))]
        {
            match mock_property(receiver, sel) {
                Some(MockValue::Str(text)) => Some(text),
                _ => None,
            }
        }
    }

    /// `-[receiver sel:NSUInteger]` (`-[NSWindow setStyleMask:]`), logged by the mock, which
    /// also applies `setStyleMask:` to the object's style mask.
    ///
    /// # Safety
    /// See [`ObjcMsg`].
    pub unsafe fn send_uint_logged(receiver: Id, sel: Sel, val: NSUInteger) -> Id {
        if receiver.is_nil() {
            return Id::NIL;
        }

        #[cfg(target_os = "macos")]
        // SAFETY: caller guarantees receiver responds to sel with matching types.
        unsafe {
            let msg_send: extern "C" fn(*mut c_void, *const c_void, NSUInteger) -> *mut c_void =
                msg_send_fn();
            Id(msg_send(receiver.0, sel.0, val))
        }

        #[cfg(not(target_os = "macos"))]
        {
            record(receiver, sel, vec![MockValue::Int(val as i64)]);
            if mock_selector_name(sel) == "setStyleMask:" {
                MockObjcRuntime::instance().update_object(receiver, |data| data.style_mask = val);
            }
            receiver
        }
    }

    /// `-[receiver sel:id]`, logged (`addTrackingArea:`, `invalidateCursorRectsForView:`,
    /// `setMask:`, ...). The mock only records.
    ///
    /// # Safety
    /// See [`ObjcMsg`].
    pub unsafe fn send_id_logged(receiver: Id, sel: Sel, arg: Id) -> Id {
        // SAFETY: caller guarantees receiver conforms to ObjcMsg contract.
        unsafe {
            if receiver.is_nil() {
                return Id::NIL;
            }

            #[cfg(not(target_os = "macos"))]
            {
                record(receiver, sel, vec![MockValue::Id(arg)]);
                // The properties some of these selectors change, as the mock keeps them.
                match mock_selector_name(sel).as_str() {
                    "removeChildWindow:" => {
                        // AppKit ignores a window that is not a child of the receiver.
                        let mock = MockObjcRuntime::instance();
                        if mock.property(arg, "parentWindow") == Some(MockValue::Id(receiver)) {
                            mock.clear_property(arg, "parentWindow");
                        }
                        return receiver;
                    }
                    // Nothing else of `send_id`'s mock applies to these selectors.
                    "addTrackingArea:"
                    | "removeTrackingArea:"
                    | "invalidateCursorRectsForView:"
                    | "setMask:"
                    | "setPath:"
                    | "interpretKeyEvents:"
                    | "setDelegate:"
                    | "makeFirstResponder:" => return receiver,
                    _ => {}
                }
            }

            Self::send_id(receiver, sel, arg)
        }
    }

    /// `-[receiver sel:BOOL]`, logged (`setHidesOnDeactivate:`, `setAcceptsMouseMovedEvents:`,
    /// `setOpaque:`, ...).
    ///
    /// # Safety
    /// See [`ObjcMsg`].
    pub unsafe fn send_bool_logged(receiver: Id, sel: Sel, val: bool) -> Id {
        // SAFETY: caller guarantees receiver conforms to ObjcMsg contract.
        unsafe {
            #[cfg(not(target_os = "macos"))]
            if !receiver.is_nil() {
                record(receiver, sel, vec![MockValue::Bool(val)]);
            }
            Self::send_bool(receiver, sel, val)
        }
    }

    /// `-[receiver sel:NSInteger]`, logged (`setLevel:`, `setState:`, ...).
    ///
    /// # Safety
    /// See [`ObjcMsg`].
    pub unsafe fn send_int_logged(receiver: Id, sel: Sel, val: NSInteger) -> Id {
        // SAFETY: caller guarantees receiver conforms to ObjcMsg contract.
        unsafe {
            #[cfg(not(target_os = "macos"))]
            if !receiver.is_nil() {
                record(receiver, sel, vec![MockValue::Int(val as i64)]);
            }
            Self::send_int(receiver, sel, val)
        }
    }

    /// `-[receiver sel]` returning an object, logged (`set`, `discardCursorRects`,
    /// `invalidateShadow`, ...).
    ///
    /// # Safety
    /// See [`ObjcMsg`].
    pub unsafe fn send_0_logged(receiver: Id, sel: Sel) -> Id {
        // SAFETY: caller guarantees receiver conforms to ObjcMsg contract.
        unsafe {
            #[cfg(not(target_os = "macos"))]
            if !receiver.is_nil() {
                record(receiver, sel, Vec::new());
            }
            Self::send_0(receiver, sel)
        }
    }

    /// `-[receiver sel:id ordered:NSInteger]` (`-[NSWindow addChildWindow:ordered:]`), logged. The
    /// mock makes `receiver` the child's `parentWindow`.
    ///
    /// # Safety
    /// See [`ObjcMsg`].
    pub unsafe fn send_id_int_logged(receiver: Id, sel: Sel, arg: Id, ordering: NSInteger) -> Id {
        if receiver.is_nil() {
            return Id::NIL;
        }

        #[cfg(target_os = "macos")]
        // SAFETY: caller guarantees receiver responds to sel with matching types.
        unsafe {
            let msg_send: extern "C" fn(
                *mut c_void,
                *const c_void,
                *mut c_void,
                NSInteger,
            ) -> *mut c_void = msg_send_fn();
            Id(msg_send(receiver.0, sel.0, arg.0, ordering))
        }

        #[cfg(not(target_os = "macos"))]
        {
            record(
                receiver,
                sel,
                vec![MockValue::Id(arg), MockValue::Int(ordering as i64)],
            );
            if mock_selector_name(sel) == "addChildWindow:ordered:" {
                MockObjcRuntime::instance().set_property(
                    arg,
                    "parentWindow",
                    MockValue::Id(receiver),
                );
            }
            receiver
        }
    }

    /// `-[receiver addObserver:id selector:SEL name:NSString object:id]`
    /// (`-[NSNotificationCenter addObserver:selector:name:object:]`), logged. `name` is a
    /// temporary `NSString` the notification centre copies.
    ///
    /// # Safety
    /// See [`ObjcMsg`].
    pub unsafe fn send_add_observer_logged(
        receiver: Id,
        sel: Sel,
        observer: Id,
        action: Sel,
        name: &str,
        object: Id,
    ) -> Id {
        if receiver.is_nil() {
            return Id::NIL;
        }

        #[cfg(target_os = "macos")]
        // SAFETY: caller guarantees receiver responds to sel with matching types.
        unsafe {
            let name_obj = nsstring_from_str(name);
            let msg_send: extern "C" fn(
                *mut c_void,
                *const c_void,
                *mut c_void,
                *const c_void,
                *mut c_void,
                *mut c_void,
            ) -> *mut c_void = msg_send_fn();
            let result = Id(msg_send(
                receiver.0, sel.0, observer.0, action.0, name_obj.0, object.0,
            ));
            // The centre copies the name; balance the +1 from `nsstring_from_str`.
            ObjcMsg::send_0(name_obj, Sel::register("release"));
            result
        }

        #[cfg(not(target_os = "macos"))]
        {
            record(
                receiver,
                sel,
                vec![
                    MockValue::Id(observer),
                    MockValue::Str(mock_selector_name(action)),
                    MockValue::Str(name.to_string()),
                    MockValue::Id(object),
                ],
            );
            receiver
        }
    }

    /// `-[receiver sel:NSRect id:id]` (`-[NSView addCursorRect:cursor:]`), logged.
    ///
    /// # Safety
    /// See [`ObjcMsg`].
    pub unsafe fn send_rect_id_logged(receiver: Id, sel: Sel, rect: CGRect, arg: Id) -> Id {
        if receiver.is_nil() {
            return Id::NIL;
        }

        #[cfg(target_os = "macos")]
        // SAFETY: caller guarantees receiver responds to sel with matching types.
        unsafe {
            let msg_send: extern "C" fn(
                *mut c_void,
                *const c_void,
                CGRect,
                *mut c_void,
            ) -> *mut c_void = msg_send_fn();
            Id(msg_send(receiver.0, sel.0, rect, arg.0))
        }

        #[cfg(not(target_os = "macos"))]
        {
            record(
                receiver,
                sel,
                vec![MockValue::Rect(rect), MockValue::Id(arg)],
            );
            receiver
        }
    }

    /// `-[NSTrackingArea initWithRect:options:owner:userInfo:]` on an allocated area; the result
    /// is the +1 object (the caller adds it to its view and keeps the +1 until it removes it).
    ///
    /// # Safety
    /// See [`ObjcMsg`]; `receiver` is an allocated, not yet initialised `NSTrackingArea`.
    pub unsafe fn send_tracking_area_init(
        receiver: Id,
        rect: CGRect,
        options: NSUInteger,
        owner: Id,
    ) -> Id {
        if receiver.is_nil() {
            return Id::NIL;
        }

        #[cfg(target_os = "macos")]
        // SAFETY: caller guarantees receiver responds to sel with matching types.
        unsafe {
            let msg_send: extern "C" fn(
                *mut c_void,
                *const c_void,
                CGRect,
                NSUInteger,
                *mut c_void,
                *mut c_void,
            ) -> *mut c_void = msg_send_fn();
            Id(msg_send(
                receiver.0,
                Sel::register("initWithRect:options:owner:userInfo:").0,
                rect,
                options,
                owner.0,
                std::ptr::null_mut(),
            ))
        }

        #[cfg(not(target_os = "macos"))]
        {
            record(
                receiver,
                Sel::register("initWithRect:options:owner:userInfo:"),
                vec![
                    MockValue::Rect(rect),
                    MockValue::Int(options as i64),
                    MockValue::Id(owner),
                ],
            );
            MockObjcRuntime::instance().update_object(receiver, |data| data.frame = rect);
            receiver
        }
    }
}

/// The shared `NSCursor` that class method `selector` (`arrowCursor`, `IBeamCursor`, ...)
/// returns, or nil. The mock hands out one object per selector, like AppKit's singletons.
///
/// # Safety
/// See [`ObjcMsg`].
pub unsafe fn standard_cursor(selector: &str) -> Id {
    #[cfg(target_os = "macos")]
    // SAFETY: caller guarantees receiver responds to sel with matching types.
    unsafe {
        ObjcMsg::send_class_0(objc_get_class("NSCursor"), Sel::register(selector))
    }

    #[cfg(not(target_os = "macos"))]
    {
        let runtime = MockObjcRuntime::instance();
        let existing = mock_state().cursors.get(selector).copied();
        match existing {
            Some(id) => Id(id as *mut c_void),
            None => {
                let id = runtime.allocate_object("NSCursor");
                mock_state()
                    .cursors
                    .insert(selector.to_string(), id.0 as usize);
                id
            }
        }
    }
}

/// `[NSArray arrayWithObject:object]`: an autoreleased one-element array.
///
/// # Safety
/// See [`ObjcMsg`].
pub unsafe fn array_with_object(object: Id) -> Id {
    #[cfg(target_os = "macos")]
    // SAFETY: caller guarantees receiver responds to sel with matching types.
    unsafe {
        ObjcMsg::send_id(
            Id(objc_get_class("NSArray").0),
            Sel::register("arrayWithObject:"),
            object,
        )
    }

    #[cfg(not(target_os = "macos"))]
    {
        let runtime = MockObjcRuntime::instance();
        let array = runtime.allocate_object("NSArray");
        runtime.update_object(array, |data| data.children.push(object));
        array
    }
}

/// `[object retain]`.
///
/// # Safety
/// See [`ObjcMsg`]; the caller owns the extra reference and balances it with [`release`].
pub unsafe fn retain(object: Id) {
    if object.is_nil() {
        return;
    }
    #[cfg(target_os = "macos")]
    // SAFETY: caller guarantees receiver responds to sel with matching types.
    unsafe {
        ObjcMsg::send_0(object, Sel::register("retain"));
    }
    #[cfg(not(target_os = "macos"))]
    {
        MockObjcRuntime::instance().retain(object);
    }
}

/// `[object release]`.
///
/// # Safety
/// See [`ObjcMsg`]; the caller owns the reference it gives up.
pub unsafe fn release(object: Id) {
    // SAFETY: caller guarantees object conforms to ObjcMsg contract and owns the reference given up.
    unsafe {
        if !object.is_nil() {
            ObjcMsg::send_0(object, Sel::register("release"));
        }
    }
}

/// The UTF-8 text of an `NSString` or `NSAttributedString` argument of an `NSTextInputClient`
/// method; `None` for nil. The mock reads the object's `string` property.
///
/// # Safety
/// See [`ObjcMsg`]; `text` is nil or a live `NSString`/`NSAttributedString`.
pub unsafe fn ns_text_to_string(text: Id) -> Option<String> {
    // SAFETY: caller guarantees text conforms to ObjcMsg contract.
    unsafe {
        if text.is_nil() {
            return None;
        }

        #[cfg(target_os = "macos")]
        {
            let attributed = objc_get_class("NSAttributedString");
            let plain = if !attributed.is_nil()
                && ObjcMsg::send_id_bool_return(
                    text,
                    Sel::register("isKindOfClass:"),
                    Id(attributed.0),
                ) {
                ObjcMsg::send_0(text, Sel::register("string"))
            } else {
                text
            };
            ns_string_to_rust(plain)
        }

        #[cfg(not(target_os = "macos"))]
        {
            ObjcMsg::send_string_prop(text, Sel::register("string"))
        }
    }
}

/// `-[NSString UTF8String]` copied into a Rust string; `None` for nil.
///
/// # Safety
/// `string` is nil or a live `NSString`.
#[cfg(target_os = "macos")]
unsafe fn ns_string_to_rust(string: Id) -> Option<String> {
    if string.is_nil() {
        return None;
    }
    // SAFETY: caller guarantees string is nil or live NSString.
    let utf8 = unsafe { ObjcMsg::send_0(string, Sel::register("UTF8String")).0 as *const c_char };
    if utf8.is_null() {
        return None;
    }
    // SAFETY: `UTF8String` is a NUL-terminated buffer owned by `string`, valid until the
    // enclosing autorelease pool drains; it is copied before returning.
    Some(unsafe {
        std::ffi::CStr::from_ptr(utf8)
            .to_string_lossy()
            .into_owned()
    })
}

// ---------------------------------------------------------------------------
// CoreGraphics / Core Animation: window mask
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
mod core_graphics {
    use std::ffi::c_void;

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        pub fn CGPathCreateMutable() -> *mut c_void;
        pub fn CGPathAddRect(path: *mut c_void, transform: *const c_void, rect: super::CGRect);
        pub fn CGPathRelease(path: *mut c_void);
    }
}

/// A `CAShapeLayer` whose path is the union of `rects`, for use as a layer mask: `rects` are in
/// the masked layer's coordinate system of `bounds` (y growing downward when `flipped`, upward
/// otherwise). The result is the +1 layer, or nil when the layer cannot be created.
///
/// The mock records the rectangles as the arguments of a `setPath:` message on the new layer.
///
/// # Safety
/// See [`ObjcMsg`].
pub unsafe fn make_region_mask_layer(rects: &[CGRect], bounds: CGRect, flipped: bool) -> Id {
    // SAFETY: caller guarantees arguments conform to ObjcMsg contract.
    unsafe {
        let class = objc_get_class("CAShapeLayer");
        if class.is_nil() {
            return Id::NIL;
        }
        let alloc = ObjcMsg::send_class_0(class, Sel::register("alloc"));
        let layer = ObjcMsg::send_0(alloc, Sel::register("init"));
        if layer.is_nil() {
            return Id::NIL;
        }
        // The shape layer covers the masked layer, so path coordinates are the masked layer's.
        ObjcMsg::send_rect(layer, Sel::register("setFrame:"), bounds);

        let placed: Vec<CGRect> = rects
            .iter()
            .map(|r| {
                if flipped {
                    *r
                } else {
                    // Qt's top-left y becomes a bottom-left y inside `bounds`.
                    CGRect::new(
                        r.origin.x,
                        bounds.size.height - (r.origin.y + r.size.height),
                        r.size.width,
                        r.size.height,
                    )
                }
            })
            .collect();

        #[cfg(target_os = "macos")]
        {
            use core_graphics::*;
            // SAFETY: the path is created, filled and released here; `setPath:` copies it.
            let path = CGPathCreateMutable();
            if path.is_null() {
                release(layer);
                return Id::NIL;
            }
            for rect in &placed {
                CGPathAddRect(path, std::ptr::null(), *rect);
            }
            ObjcMsg::send_id(layer, Sel::register("setPath:"), Id(path));
            CGPathRelease(path);
        }

        #[cfg(not(target_os = "macos"))]
        {
            record(
                layer,
                Sel::register("setPath:"),
                placed.into_iter().map(MockValue::Rect).collect(),
            );
        }
        layer
    }
}

// ---------------------------------------------------------------------------
// Native classes
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
mod native {
    use super::*;
    use crate::window_cocoa_input as input;
    use crate::window_cocoa_input::NsEventKind;
    use std::ffi::CStr;

    #[link(name = "objc")]
    unsafe extern "C" {
        fn objc_getProtocol(name: *const c_char) -> *mut c_void;
        fn class_addProtocol(class: *mut c_void, protocol: *mut c_void) -> BOOL;
        fn objc_msgSendSuper();
    }

    #[cfg(target_arch = "aarch64")]
    const ENC_BOOL_GETTER: &CStr = c"B@:";
    #[cfg(not(target_arch = "aarch64"))]
    const ENC_BOOL_GETTER: &CStr = c"c@:";
    #[cfg(target_arch = "aarch64")]
    const ENC_ACCEPTS_FIRST_MOUSE: &CStr = c"B@:@";
    #[cfg(not(target_arch = "aarch64"))]
    const ENC_ACCEPTS_FIRST_MOUSE: &CStr = c"c@:@";
    #[cfg(target_arch = "aarch64")]
    const ENC_HAS_MARKED_TEXT: &CStr = c"B@:";
    #[cfg(not(target_arch = "aarch64"))]
    const ENC_HAS_MARKED_TEXT: &CStr = c"c@:";

    /// `struct objc_super`: where `objc_msgSendSuper` starts its method lookup.
    #[repr(C)]
    struct ObjcSuper {
        receiver: *mut c_void,
        super_class: *mut c_void,
    }

    /// `objc_msgSendSuper` as the function pointer type `F`.
    ///
    /// # Safety
    /// `F` must be an `unsafe extern "C" fn` pointer type taking `*const ObjcSuper`, the selector,
    /// then the arguments and return type of the method each call through it sends.
    #[inline]
    unsafe fn super_send_fn<F: Copy>() -> F {
        debug_assert_eq!(std::mem::size_of::<F>(), std::mem::size_of::<*const ()>());
        // SAFETY: caller guarantees F matches objc_msgSendSuper signature.
        unsafe { std::mem::transmute_copy(&(objc_msgSendSuper as *const ())) }
    }

    /// Runs `f`; a panic must not unwind into AppKit (undefined behavior), so it yields
    /// `fallback` instead, as the Win32 window procedure does.
    fn guarded<R>(fallback: R, f: impl FnOnce() -> R) -> R {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or(fallback)
    }

    /// An event method: the window's handler gets the event; when there is none (no handler,
    /// the window is torn down, an event with nothing to report) AppKit's own handling of it
    /// runs, through NSView, so the responder chain still sees it.
    macro_rules! ns_event_imp {
        ($name:ident, $kind:expr) => {
            extern "C" fn $name(this: *mut c_void, cmd: *const c_void, event: *mut c_void) {
                // SAFETY: AppKit sends this method (installed for `$kind`'s selector) with the
                // `NSEvent` it is dispatching, nil or live for the call, so `event` is an event
                // of the kind `$kind` names [inferred: AppKit documentation of the NSResponder
                // event methods].
                let handled = guarded(true, || unsafe {
                    input::handle_ns_event(Id(this), $kind, Id(event))
                });
                if !handled {
                    // SAFETY: `sup` lives across the call and names `this` (the receiver AppKit
                    // is dispatching to) with `NSView` as the class where lookup starts, which
                    // `QNSView` is a subclass of (see `qnsview_class`), so `this` is an
                    // instance of a subclass of `super_class` as `objc_msgSendSuper` requires.
                    // `super_send_fn` is transmuted to the exact signature used: (`*const
                    // ObjcSuper`, selector, event), and every method installed with this macro
                    // takes the event and returns nothing. `cmd`/`event` are the selector and
                    // argument AppKit passed to this very method.
                    unsafe {
                        let sup = ObjcSuper {
                            receiver: this,
                            super_class: objc_get_class("NSView").0,
                        };
                        let send: extern "C" fn(*const ObjcSuper, *const c_void, *mut c_void) =
                            super_send_fn();
                        send(&sup, cmd, event);
                    }
                }
            }
        };
    }

    ns_event_imp!(mouse_down, NsEventKind::MouseDown);
    ns_event_imp!(mouse_up, NsEventKind::MouseUp);
    ns_event_imp!(mouse_dragged, NsEventKind::MouseDragged);
    ns_event_imp!(right_mouse_down, NsEventKind::RightMouseDown);
    ns_event_imp!(right_mouse_up, NsEventKind::RightMouseUp);
    ns_event_imp!(right_mouse_dragged, NsEventKind::RightMouseDragged);
    ns_event_imp!(other_mouse_down, NsEventKind::OtherMouseDown);
    ns_event_imp!(other_mouse_up, NsEventKind::OtherMouseUp);
    ns_event_imp!(other_mouse_dragged, NsEventKind::OtherMouseDragged);
    ns_event_imp!(mouse_moved, NsEventKind::MouseMoved);
    ns_event_imp!(mouse_entered, NsEventKind::MouseEntered);
    ns_event_imp!(mouse_exited, NsEventKind::MouseExited);
    ns_event_imp!(scroll_wheel, NsEventKind::ScrollWheel);
    ns_event_imp!(key_down, NsEventKind::KeyDown);
    ns_event_imp!(key_up, NsEventKind::KeyUp);
    ns_event_imp!(flags_changed, NsEventKind::FlagsChanged);

    extern "C" fn returns_yes(_this: *mut c_void, _cmd: *const c_void) -> BOOL {
        YES
    }

    extern "C" fn accepts_first_mouse(
        _this: *mut c_void,
        _cmd: *const c_void,
        _event: *mut c_void,
    ) -> BOOL {
        YES
    }

    extern "C" fn update_tracking_areas(this: *mut c_void, cmd: *const c_void) {
        guarded((), || input::update_tracking_area(Id(this)));
        // SAFETY: `sup` lives across the call and names `this` with `NSView` as the lookup start
        // class; `this` is a `QNSView`, an `NSView` subclass. `updateTrackingAreas` takes no
        // arguments and returns nothing, matching the transmuted signature (`*const ObjcSuper`,
        // selector); `cmd` is the selector AppKit passed. Cocoa asks subclasses to call the
        // superclass implementation [inferred: AppKit documentation].
        unsafe {
            let sup = ObjcSuper {
                receiver: this,
                super_class: objc_get_class("NSView").0,
            };
            let send: extern "C" fn(*const ObjcSuper, *const c_void) = super_send_fn();
            send(&sup, cmd);
        }
    }

    extern "C" fn reset_cursor_rects(this: *mut c_void, cmd: *const c_void) {
        let applied = guarded(false, || input::reset_cursor_rects(Id(this)));
        if !applied {
            // SAFETY: same as in `update_tracking_areas`: `sup` names `this` (a `QNSView`) with
            // `NSView` as the lookup start; `resetCursorRects` takes no arguments and returns
            // nothing, matching the transmuted signature; `cmd` is the selector AppKit passed.
            unsafe {
                let sup = ObjcSuper {
                    receiver: this,
                    super_class: objc_get_class("NSView").0,
                };
                let send: extern "C" fn(*const ObjcSuper, *const c_void) = super_send_fn();
                send(&sup, cmd);
            }
        }
    }

    extern "C" fn cursor_update(this: *mut c_void, cmd: *const c_void, event: *mut c_void) {
        let applied = guarded(false, || input::cursor_update(Id(this)));
        if !applied {
            // SAFETY: `sup` names `this` (a `QNSView`) with `NSView` as the lookup start;
            // `cursorUpdate:` takes the event and returns nothing, matching the transmuted
            // signature (`*const ObjcSuper`, selector, event); `cmd`/`event` are what AppKit
            // passed to this method.
            unsafe {
                let sup = ObjcSuper {
                    receiver: this,
                    super_class: objc_get_class("NSView").0,
                };
                let send: extern "C" fn(*const ObjcSuper, *const c_void, *mut c_void) =
                    super_send_fn();
                send(&sup, cmd, event);
            }
        }
    }

    extern "C" fn insert_text(
        this: *mut c_void,
        _cmd: *const c_void,
        text: *mut c_void,
        _replacement: NSRange,
    ) {
        // SAFETY: `text` is the argument AppKit passed to `insertText:replacementRange:`: an
        // NSString or NSAttributedString (or nil), live for this call [inferred:
        // NSTextInputClient documentation].
        guarded((), || unsafe {
            input::text_input_insert(Id(this), Id(text))
        });
    }

    extern "C" fn set_marked_text(
        this: *mut c_void,
        _cmd: *const c_void,
        text: *mut c_void,
        selected: NSRange,
        _replacement: NSRange,
    ) {
        // SAFETY: `text` is the argument AppKit passed to `setMarkedText:...`: an NSString or
        // NSAttributedString (or nil), live for this call [inferred: NSTextInputClient
        // documentation].
        guarded((), || unsafe {
            input::text_input_set_marked(Id(this), Id(text), selected)
        });
    }

    extern "C" fn unmark_text(this: *mut c_void, _cmd: *const c_void) {
        guarded((), || input::text_input_unmark(Id(this)));
    }

    extern "C" fn selected_range(_this: *mut c_void, _cmd: *const c_void) -> NSRange {
        NSRange::NOT_FOUND
    }

    extern "C" fn marked_range(this: *mut c_void, _cmd: *const c_void) -> NSRange {
        guarded(NSRange::NOT_FOUND, || {
            input::text_input_marked_range(Id(this))
        })
    }

    extern "C" fn has_marked_text(this: *mut c_void, _cmd: *const c_void) -> BOOL {
        if guarded(false, || input::text_input_has_marked_text(Id(this))) {
            YES
        } else {
            NO
        }
    }

    extern "C" fn attributed_substring(
        _this: *mut c_void,
        _cmd: *const c_void,
        _range: NSRange,
        _actual: *mut NSRange,
    ) -> *mut c_void {
        std::ptr::null_mut()
    }

    extern "C" fn valid_attributes_for_marked_text(
        _this: *mut c_void,
        _cmd: *const c_void,
    ) -> *mut c_void {
        // SAFETY: `+[NSArray array]` takes no arguments and returns an object, matching
        // `send_class_0`; the class is looked up by name and a nil class would only yield nil.
        // The result is autoreleased [inferred: Foundation convention for class factory methods],
        // which is what a `validAttributesForMarkedText` getter returns.
        unsafe { ObjcMsg::send_class_0(objc_get_class("NSArray"), Sel::register("array")).0 }
    }

    extern "C" fn first_rect_for_character_range(
        this: *mut c_void,
        _cmd: *const c_void,
        range: NSRange,
        actual: *mut NSRange,
    ) -> CGRect {
        if !actual.is_null() {
            // SAFETY: `actual` was just checked non-null, and the `NSTextInputClient` contract
            // for `firstRectForCharacterRange:actualRange:` is that a non-NULL `actualRange` points
            // to a writable `NSRange` that the method fills in [inferred: AppKit documentation;
            // not checkable in code]. The reported rectangle is for the range asked about,
            // whatever it is, so it is the actual range.
            unsafe { *actual = range };
        }
        guarded(CGRect::ZERO, || input::text_input_first_rect(Id(this)))
    }

    extern "C" fn character_index_for_point(
        _this: *mut c_void,
        _cmd: *const c_void,
        _point: CGPoint,
    ) -> NSUInteger {
        NS_NOT_FOUND
    }

    extern "C" fn do_command_by_selector(
        _this: *mut c_void,
        _cmd: *const c_void,
        _selector: *const c_void,
    ) {
        // Commands such as `insertNewline:` have no text; the key press itself was delivered.
    }

    /// Adds the input methods to `QNSView`, the `NSTextInputClient` ones included, and declares
    /// the protocol so `-inputContext` exists.
    ///
    /// # Safety
    /// `class` is an allocated, not yet registered class pair whose superclass is `NSView`.
    pub(crate) unsafe fn install_view_methods(class: *mut c_void) {
        let add = |name: &str, imp: *const c_void, types: &CStr| {
            // SAFETY: adding method implementation to un-registered class pair.
            unsafe {
                native_bindings::class_addMethod(class, Sel::register(name).0, imp, types.as_ptr())
            };
        };
        let event_methods: [(&str, *const c_void); 12] = [
            ("mouseDown:", mouse_down as *const c_void),
            ("mouseUp:", mouse_up as *const c_void),
            ("mouseDragged:", mouse_dragged as *const c_void),
            ("rightMouseDown:", right_mouse_down as *const c_void),
            ("rightMouseUp:", right_mouse_up as *const c_void),
            ("rightMouseDragged:", right_mouse_dragged as *const c_void),
            ("otherMouseDown:", other_mouse_down as *const c_void),
            ("otherMouseUp:", other_mouse_up as *const c_void),
            ("otherMouseDragged:", other_mouse_dragged as *const c_void),
            ("mouseMoved:", mouse_moved as *const c_void),
            ("mouseEntered:", mouse_entered as *const c_void),
            ("mouseExited:", mouse_exited as *const c_void),
        ];
        for (name, imp) in event_methods {
            add(name, imp, c"v@:@");
        }
        add("scrollWheel:", scroll_wheel as *const c_void, c"v@:@");
        add("keyDown:", key_down as *const c_void, c"v@:@");
        add("keyUp:", key_up as *const c_void, c"v@:@");
        add("flagsChanged:", flags_changed as *const c_void, c"v@:@");
        add("cursorUpdate:", cursor_update as *const c_void, c"v@:@");
        add(
            "resetCursorRects",
            reset_cursor_rects as *const c_void,
            c"v@:",
        );
        add(
            "updateTrackingAreas",
            update_tracking_areas as *const c_void,
            c"v@:",
        );
        add(
            "acceptsFirstResponder",
            returns_yes as *const c_void,
            ENC_BOOL_GETTER,
        );
        add(
            "acceptsFirstMouse:",
            accepts_first_mouse as *const c_void,
            ENC_ACCEPTS_FIRST_MOUSE,
        );

        // NSTextInputClient
        add(
            "insertText:replacementRange:",
            insert_text as *const c_void,
            c"v@:@{_NSRange=QQ}",
        );
        add(
            "setMarkedText:selectedRange:replacementRange:",
            set_marked_text as *const c_void,
            c"v@:@{_NSRange=QQ}{_NSRange=QQ}",
        );
        add("unmarkText", unmark_text as *const c_void, c"v@:");
        add(
            "selectedRange",
            selected_range as *const c_void,
            c"{_NSRange=QQ}@:",
        );
        add(
            "markedRange",
            marked_range as *const c_void,
            c"{_NSRange=QQ}@:",
        );
        add(
            "hasMarkedText",
            has_marked_text as *const c_void,
            ENC_HAS_MARKED_TEXT,
        );
        add(
            "attributedSubstringForProposedRange:actualRange:",
            attributed_substring as *const c_void,
            c"@@:{_NSRange=QQ}^{_NSRange=QQ}",
        );
        add(
            "validAttributesForMarkedText",
            valid_attributes_for_marked_text as *const c_void,
            c"@@:",
        );
        add(
            "firstRectForCharacterRange:actualRange:",
            first_rect_for_character_range as *const c_void,
            c"{CGRect={CGPoint=dd}{CGSize=dd}}@:{_NSRange=QQ}^{_NSRange=QQ}",
        );
        add(
            "characterIndexForPoint:",
            character_index_for_point as *const c_void,
            c"Q@:{CGPoint=dd}",
        );
        add(
            "doCommandBySelector:",
            do_command_by_selector as *const c_void,
            c"v@::",
        );
        // SAFETY: retrieves NSTextInputClient protocol and adds to un-registered class pair.
        unsafe {
            let protocol = objc_getProtocol(c"NSTextInputClient".as_ptr());
            if !protocol.is_null() {
                class_addProtocol(class, protocol);
            }
        }
    }

    /// The `NSWindow` subclass of every window that can take the keyboard focus: like Qt's
    /// `QNSWindow` it says yes to `-canBecomeKeyWindow` and `-canBecomeMainWindow`, which a plain
    /// borderless `NSWindow` (a frameless window) refuses.
    pub fn qnswindow_class() -> Class {
        // SAFETY: the closure runs once (LazyLock). `QNSWindow` is only allocated when absent;
        // `ns_window` is `NSWindow` (always present with AppKit linked), `name` is a
        // NUL-terminated `CString` alive for the call, and a nil `class` returns early before any
        // use. `returns_yes` is an `extern "C"` function of (self, _cmd) returning `BOOL`, which
        // is what `ENC_BOOL_GETTER` (`c@:` / `B@:` per architecture) describes for the
        // `canBecomeKeyWindow` / `canBecomeMainWindow` getters. The class is registered after
        // its methods are added.
        static CLASS: LazyLock<usize> = LazyLock::new(|| unsafe {
            let existing = objc_get_class("QNSWindow");
            if !existing.is_nil() {
                return existing.0 as usize;
            }
            let ns_window = objc_get_class("NSWindow");
            let name = CString::new("QNSWindow").unwrap();
            let class = native_bindings::objc_allocateClassPair(ns_window.0, name.as_ptr(), 0);
            if class.is_null() {
                return ns_window.0 as usize;
            }
            for selector in ["canBecomeKeyWindow", "canBecomeMainWindow"] {
                native_bindings::class_addMethod(
                    class,
                    Sel::register(selector).0,
                    returns_yes as *const c_void,
                    ENC_BOOL_GETTER.as_ptr(),
                );
            }
            native_bindings::objc_registerClassPair(class);
            class as usize
        });
        Class(*CLASS as *mut c_void)
    }
}

#[cfg(target_os = "macos")]
pub(crate) use native::install_view_methods;
#[cfg(target_os = "macos")]
pub use native::qnswindow_class;
