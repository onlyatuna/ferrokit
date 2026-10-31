// SPDX-License-Identifier: LGPL-3.0-only
//! `QCocoaEventDispatcher` (qcocoaeventdispatcher.mm): the GUI event loop's dispatcher on macOS.
//!
//! ferrokit's `EventLoop` sends posted events and timer events itself, around each `process_events`
//! call (loop.rs), so every call takes Qt's path for a `processEvents` that is not `exec`
//! (qcocoaeventdispatcher.mm:378-466): make sure `NSApp` has launched, send every queued `NSEvent`
//! through the native event filters and `[NSApp sendEvent:]`, and if nothing was sent and the
//! caller may wait, wait in `[NSApp nextEventMatchingMask:untilDate:…]` (which runs the main run
//! loop) until an event arrives or the next ferrokit timer is due (:273-285, :479-487). `wake_up`, from
//! any thread, signals a run loop source whose callback posts an application-defined `NSEvent`
//! that ends the wait (:525-531, :877-916).

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use crate::event::{NativeEventFilter, NativeEventFilterChain, NativeMessage};
use crate::event_loop::dispatcher::{DispatchResult, EventDispatcher, EventDispatcherHandle};
use crate::timer::{TimerEntry, TimerRegistry};

mod ffi {
    use std::ffi::{c_char, c_void};

    pub type Id = *mut c_void;
    pub type Sel = *const c_void;
    pub type CFTypeRef = *mut c_void;

    #[repr(C)]
    pub struct NSPoint {
        pub x: f64,
        pub y: f64,
    }

    /// `CFRunLoopSourceContext` (version 0).
    #[repr(C)]
    pub struct CFRunLoopSourceContext {
        pub version: isize,
        pub info: *mut c_void,
        pub retain: Option<extern "C" fn(*const c_void) -> *const c_void>,
        pub release: Option<extern "C" fn(*const c_void)>,
        pub copy_description: Option<extern "C" fn(*const c_void) -> *const c_void>,
        pub equal: Option<extern "C" fn(*const c_void, *const c_void) -> u8>,
        pub hash: Option<extern "C" fn(*const c_void) -> usize>,
        pub schedule: Option<extern "C" fn(*mut c_void, CFTypeRef, *const c_void)>,
        pub cancel: Option<extern "C" fn(*mut c_void, CFTypeRef, *const c_void)>,
        pub perform: Option<extern "C" fn(*mut c_void)>,
    }

    pub type TimerCallback = extern "C" fn(CFTypeRef, *mut c_void);

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        pub static kCFRunLoopCommonModes: *const c_void;
        pub fn CFRunLoopGetMain() -> CFTypeRef;
        pub fn CFRunLoopWakeUp(run_loop: CFTypeRef);
        pub fn CFRunLoopSourceCreate(
            allocator: *const c_void,
            order: isize,
            context: *mut CFRunLoopSourceContext,
        ) -> CFTypeRef;
        pub fn CFRunLoopAddSource(run_loop: CFTypeRef, source: CFTypeRef, mode: *const c_void);
        pub fn CFRunLoopSourceSignal(source: CFTypeRef);
        pub fn CFRunLoopSourceInvalidate(source: CFTypeRef);
        pub fn CFAbsoluteTimeGetCurrent() -> f64;
        pub fn CFRunLoopTimerCreate(
            allocator: *const c_void,
            fire_date: f64,
            interval: f64,
            flags: usize,
            order: isize,
            callout: TimerCallback,
            context: *mut c_void,
        ) -> CFTypeRef;
        pub fn CFRunLoopAddTimer(run_loop: CFTypeRef, timer: CFTypeRef, mode: *const c_void);
        pub fn CFRunLoopTimerInvalidate(timer: CFTypeRef);
        pub fn CFRelease(object: CFTypeRef);
    }

    #[link(name = "Foundation", kind = "framework")]
    extern "C" {
        pub static NSDefaultRunLoopMode: Id;
    }

    #[link(name = "AppKit", kind = "framework")]
    extern "C" {}

    #[link(name = "objc")]
    extern "C" {
        pub fn objc_getClass(name: *const c_char) -> Id;
        pub fn sel_registerName(name: *const c_char) -> Sel;
        pub fn objc_msgSend();
        pub fn objc_autoreleasePoolPush() -> *mut c_void;
        pub fn objc_autoreleasePoolPop(pool: *mut c_void);
    }
}

use ffi::{CFTypeRef, Id, Sel};

/// `NSEventTypeApplicationDefined`.
const NS_EVENT_TYPE_APPLICATION_DEFINED: usize = 15;
/// `NSEventMaskAny`.
const NS_EVENT_MASK_ANY: usize = usize::MAX;
/// `QtCocoaEventSubTypeWakeup` (qcocoahelpers.h:89-92).
const WAKEUP_SUBTYPE: i16 = i16::MAX;

/// The classes and selectors the dispatcher sends to, looked up once.
struct Runtime {
    ns_application: Id,
    ns_event: Id,
    ns_date: Id,
    shared_application: Sel,
    is_running: Sel,
    run: Sel,
    stop: Sel,
    next_event: Sel,
    send_event: Sel,
    post_event: Sel,
    other_event: Sel,
    distant_future: Sel,
    date_from_now: Sel,
}

// SAFETY: [inferred] the `Id`/`Sel` fields are class objects and interned selectors from the
// Objective-C runtime; they are looked up once, never mutated afterwards, and the runtime
// documents class lookup and selectors as process-wide and thread-safe. The field types are
// raw pointers only, which is why `Send` is not automatic.
unsafe impl Send for Runtime {}
// SAFETY: same as `Send` above: the fields are immutable after construction, so shared
// access from several threads reads only constant pointers.
unsafe impl Sync for Runtime {}

// SAFETY: `objc_getClass` and `sel_registerName` are given NUL-terminated C-string literals
// (`c"..."`) that outlive the calls and may be called from any thread per the Objective-C
// runtime documentation. [inferred] AppKit and Foundation are linked by the `#[link]` blocks
// in `ffi`, so the classes exist; a null class would only make later message sends no-ops.
static RUNTIME: LazyLock<Runtime> = LazyLock::new(|| unsafe {
    use ffi::{objc_getClass, sel_registerName};
    Runtime {
        ns_application: objc_getClass(c"NSApplication".as_ptr()),
        ns_event: objc_getClass(c"NSEvent".as_ptr()),
        ns_date: objc_getClass(c"NSDate".as_ptr()),
        shared_application: sel_registerName(c"sharedApplication".as_ptr()),
        is_running: sel_registerName(c"isRunning".as_ptr()),
        run: sel_registerName(c"run".as_ptr()),
        stop: sel_registerName(c"stop:".as_ptr()),
        next_event: sel_registerName(c"nextEventMatchingMask:untilDate:inMode:dequeue:".as_ptr()),
        send_event: sel_registerName(c"sendEvent:".as_ptr()),
        post_event: sel_registerName(c"postEvent:atStart:".as_ptr()),
        other_event: sel_registerName(
            c"otherEventWithType:location:modifierFlags:timestamp:windowNumber:context:subtype:data1:data2:"
                .as_ptr(),
        ),
        distant_future: sel_registerName(c"distantFuture".as_ptr()),
        date_from_now: sel_registerName(c"dateWithTimeIntervalSinceNow:".as_ptr()),
    }
});

unsafe fn send0<R>(receiver: Id, sel: Sel) -> R {
    let f: unsafe extern "C" fn(Id, Sel) -> R = std::mem::transmute(ffi::objc_msgSend as *const ());
    f(receiver, sel)
}

unsafe fn send1<A, R>(receiver: Id, sel: Sel, a: A) -> R {
    let f: unsafe extern "C" fn(Id, Sel, A) -> R =
        std::mem::transmute(ffi::objc_msgSend as *const ());
    f(receiver, sel, a)
}

unsafe fn send2<A, B, R>(receiver: Id, sel: Sel, a: A, b: B) -> R {
    let f: unsafe extern "C" fn(Id, Sel, A, B) -> R =
        std::mem::transmute(ffi::objc_msgSend as *const ());
    f(receiver, sel, a, b)
}

/// An Objective-C autorelease pool for the scope (Qt's `QMacAutoReleasePool`).
struct AutoreleasePool(*mut c_void);

impl AutoreleasePool {
    fn new() -> Self {
        // SAFETY: `objc_autoreleasePoolPush` takes no arguments and may be called on any
        // thread; the returned token is popped once by `Drop`.
        Self(unsafe { ffi::objc_autoreleasePoolPush() })
    }
}

impl Drop for AutoreleasePool {
    fn drop(&mut self) {
        // SAFETY: [inferred] `self.0` is the token returned by the `objc_autoreleasePoolPush`
        // in `new`; pools are popped exactly once and, being stack-scoped guards, in reverse
        // order of creation on the same thread.
        unsafe { ffi::objc_autoreleasePoolPop(self.0) }
    }
}

fn ns_app() -> Id {
    let rt = &*RUNTIME;
    // SAFETY: [inferred] `ns_application` is the `NSApplication` class and `sharedApplication`
    // takes no argument and returns an object, matching `send0::<Id>`. The class and selector
    // were resolved by `RUNTIME` just above.
    unsafe { send0(rt.ns_application, rt.shared_application) }
}

/// `[NSApp nextEventMatchingMask:NSEventMaskAny untilDate:until inMode:NSDefaultRunLoopMode
/// dequeue:YES]`.
fn next_event(app: Id, until: Id) -> Id {
    let rt = &*RUNTIME;
    // SAFETY: [inferred] `f` is `objc_msgSend` cast to the signature of
    // `nextEventMatchingMask:untilDate:inMode:dequeue:` (mask `NSUInteger`, `NSDate*`,
    // mode `NSString*`, `BOOL`; returns `NSEvent*`); `app` is `NSApp` and `until` is null or a
    // live `NSDate`, both supplied by this module's callers on the main thread
    // (`process_events` asserts it); `NSDefaultRunLoopMode` is an AppKit constant.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, usize, Id, Id, i8) -> Id =
            std::mem::transmute(ffi::objc_msgSend as *const ());
        f(
            app,
            rt.next_event,
            NS_EVENT_MASK_ANY,
            until,
            ffi::NSDefaultRunLoopMode,
            1,
        )
    }
}

/// `cancelWaitForMoreEvents` (qcocoaeventdispatcher.mm:898-906): posts the application-defined
/// wake-up event that ends a wait in `nextEventMatchingMask:`.
fn post_wakeup_event() {
    let _pool = AutoreleasePool::new();
    let rt = &*RUNTIME;
    // SAFETY: [inferred] `make` is declared with the argument and return types of
    // `+[NSEvent otherEventWithType:location:modifierFlags:timestamp:windowNumber:context:
    // subtype:data1:data2:]` (type, `NSPoint`, flags, timestamp, window number, context,
    // subtype, two data words; returns an object), receiver `rt.ns_event` is the `NSEvent`
    // class, and the `NSPoint` is passed by value. `postEvent:atStart:` takes (`Id`, `BOOL`)
    // and returns void, matching `send2::<Id, i8, ()>`; the event is autoreleased by the
    // pool above, which outlives both calls. [inferred] `postEvent:atStart:` only enqueues the
    // event; this function runs on the main thread (from the run loop source callback, the
    // launch timer, and `wait_for_more_events`' caller), as Qt's `cancelWaitForMoreEvents`.
    unsafe {
        let make: unsafe extern "C" fn(
            Id,
            Sel,
            usize,
            ffi::NSPoint,
            usize,
            f64,
            isize,
            Id,
            i16,
            isize,
            isize,
        ) -> Id = std::mem::transmute(ffi::objc_msgSend as *const ());
        let event = make(
            rt.ns_event,
            rt.other_event,
            NS_EVENT_TYPE_APPLICATION_DEFINED,
            ffi::NSPoint { x: 0.0, y: 0.0 },
            0,
            0.0,
            0,
            std::ptr::null_mut(),
            WAKEUP_SUBTYPE,
            0,
            0,
        );
        send2::<Id, i8, ()>(ns_app(), rt.post_event, event, 0);
    }
}

/// Qt's `nsAppRunCalledByQt`: `NSApp` was launched by `ensure_nsapp_initialized`.
static NSAPP_RUN_CALLED: AtomicBool = AtomicBool::new(false);
/// Qt's `initializingNSApplication`.
static INITIALIZING_NSAPP: AtomicBool = AtomicBool::new(false);
/// A wake-up source fired while `NSApp` was launching and must fire again afterwards.
static WAKE_DEFERRED: AtomicBool = AtomicBool::new(false);
/// The main thread is inside a `process_events` call that may wait (Qt's `processEventsFlags`
/// with `WaitForMoreEvents` and without `EventLoopExec`, :908-916).
static WAIT_FOR_MORE_EVENTS: AtomicBool = AtomicBool::new(false);

/// The posted-events source's callback, on the main thread (:877-896). Posted and timer events
/// are sent by the ferrokit event loop, so the callback only ends a wait.
extern "C" fn wake_source_perform(_info: *mut c_void) {
    if INITIALIZING_NSAPP.load(Ordering::SeqCst) {
        WAKE_DEFERRED.store(true, Ordering::SeqCst);
        return;
    }
    if WAIT_FOR_MORE_EVENTS.load(Ordering::SeqCst) {
        post_wakeup_event();
    }
}

/// Runs once `[NSApp run]` has launched the application (:568-572).
extern "C" fn stop_nsapp_after_launch(_timer: CFTypeRef, _info: *mut c_void) {
    let app = ns_app();
    // SAFETY: [inferred] `stop:` takes one object argument and returns void, matching
    // `send1::<Id, ()>`; `app` is `NSApp` from `ns_app()` and the sender argument may be
    // `NSApp` itself. This timer callout runs on the main run loop (the timer was added to
    // `CFRunLoopGetMain` in `ensure_nsapp_initialized`).
    unsafe { send1::<Id, ()>(app, RUNTIME.stop, app) };
    post_wakeup_event();
}

/// The version-0 run loop source on the main run loop's common modes that `wake_up` signals
/// (Qt's `postedEventsSource`, :786-801).
struct WakeSource(CFTypeRef);

// SAFETY: CFRunLoopSourceSignal, CFRunLoopWakeUp and CFRunLoopSourceInvalidate are thread-safe.
unsafe impl Send for WakeSource {}
unsafe impl Sync for WakeSource {}

impl WakeSource {
    fn new() -> Self {
        let mut context = ffi::CFRunLoopSourceContext {
            version: 0,
            info: std::ptr::null_mut(),
            retain: None,
            release: None,
            copy_description: None,
            equal: None,
            hash: None,
            schedule: None,
            cancel: None,
            perform: Some(wake_source_perform),
        };
        // SAFETY: `context` is a live local of the declared `CFRunLoopSourceContext` layout
        // (version 0, every unused callback `None`, `perform` an `extern "C"` fn of the right
        // type); `CFRunLoopSourceCreate` copies it, so it need not outlive the call. The
        // result is checked non-null before it is added to the main run loop, and
        // `kCFRunLoopCommonModes` is a CoreFoundation constant.
        unsafe {
            let source = ffi::CFRunLoopSourceCreate(std::ptr::null(), 0, &mut context);
            assert!(!source.is_null(), "CFRunLoopSourceCreate");
            ffi::CFRunLoopAddSource(ffi::CFRunLoopGetMain(), source, ffi::kCFRunLoopCommonModes);
            Self(source)
        }
    }

    /// `QCocoaEventDispatcher::wakeUp` (:525-531).
    fn signal(&self) {
        // SAFETY: `self.0` is the source created in `new` and stays alive while `&self` does
        // (released only in `Drop`); per the Core Foundation documentation
        // `CFRunLoopSourceSignal`, `CFRunLoopWakeUp` and `CFRunLoopGetMain` may be called from
        // any thread.
        unsafe {
            ffi::CFRunLoopSourceSignal(self.0);
            ffi::CFRunLoopWakeUp(ffi::CFRunLoopGetMain());
        }
    }
}

impl Drop for WakeSource {
    fn drop(&mut self) {
        // SAFETY: `self.0` is the source created in `new`, owned only by this `WakeSource`;
        // `Drop` runs once (the `Arc` in the dispatcher and its handles drops it after the last
        // holder), so it is invalidated and its single retain released exactly once.
        unsafe {
            ffi::CFRunLoopSourceInvalidate(self.0);
            ffi::CFRelease(self.0);
        }
    }
}

#[derive(Clone)]
pub struct CocoaEventDispatcherHandle {
    wake: Arc<WakeSource>,
}

impl EventDispatcherHandle for CocoaEventDispatcherHandle {
    fn wake_up(&self) {
        self.wake.signal();
    }
}

pub struct CocoaEventDispatcher {
    wake: Arc<WakeSource>,
    native_filters: NativeEventFilterChain,
}

impl Default for CocoaEventDispatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl CocoaEventDispatcher {
    pub fn new() -> Self {
        Self {
            wake: Arc::new(WakeSource::new()),
            native_filters: NativeEventFilterChain::new(),
        }
    }

    pub fn clone_handle(&self) -> CocoaEventDispatcherHandle {
        CocoaEventDispatcherHandle {
            wake: Arc::clone(&self.wake),
        }
    }

    /// `ensureNSAppInitialized` (:537-575): launch `NSApp` with `[NSApp run]` and stop it as soon
    /// as it runs, so AppKit does its launch work (`finishLaunching`) before events are processed.
    fn ensure_nsapp_initialized(&self, app: Id) {
        let rt = &*RUNTIME;
        if NSAPP_RUN_CALLED.load(Ordering::SeqCst)
            // SAFETY: [inferred] `app` is the `NSApp` instance from `ns_app()` and `isRunning`
            // takes no arguments and returns `BOOL` (read as `i8`), matching `send0::<i8>`.
            || unsafe { send0::<i8>(app, rt.is_running) } != 0
        {
            return;
        }
        NSAPP_RUN_CALLED.store(true, Ordering::SeqCst);
        INITIALIZING_NSAPP.store(true, Ordering::SeqCst);
        // SAFETY: [inferred] Runs on the main thread (`process_events` asserts it before
        // calling this). `CFRunLoopTimerCreate` gets a null allocator (default), a plain
        // `extern "C"` callout of the declared `TimerCallback` type and a null context;
        // its result is checked non-null before use, and the timer is invalidated and released
        // once after `[NSApp run]` returns (`stop_nsapp_after_launch` stops it on the first
        // run-loop turn). `run` takes no arguments, matching `send0::<()>`.
        unsafe {
            let main = ffi::CFRunLoopGetMain();
            let now = ffi::CFAbsoluteTimeGetCurrent();
            let timer = ffi::CFRunLoopTimerCreate(
                std::ptr::null(),
                now,
                0.0,
                0,
                0,
                stop_nsapp_after_launch,
                std::ptr::null_mut(),
            );
            assert!(!timer.is_null(), "CFRunLoopTimerCreate");
            ffi::CFRunLoopAddTimer(main, timer, ffi::kCFRunLoopCommonModes);
            send0::<()>(app, rt.run);
            ffi::CFRunLoopTimerInvalidate(timer);
            ffi::CFRelease(timer);
        }
        INITIALIZING_NSAPP.store(false, Ordering::SeqCst);
        if WAKE_DEFERRED.swap(false, Ordering::SeqCst) {
            self.wake.signal();
        }
    }

    /// Sends every queued `NSEvent` the native event filters do not consume to
    /// `[NSApp sendEvent:]` (:426-451). Returns whether any was sent.
    fn send_queued_events(&self, app: Id) -> bool {
        let rt = &*RUNTIME;
        let mut sent = false;
        loop {
            let _pool = AutoreleasePool::new();
            let event = next_event(app, std::ptr::null_mut());
            if event.is_null() {
                return sent;
            }
            let mut result = 0isize;
            if !self.native_filters.filter_native(
                "NSEvent",
                &NativeMessage::Mac(event),
                &mut result,
            ) {
                // SAFETY: [inferred] `sendEvent:` takes one object argument and returns void,
                // matching `send1::<Id, ()>`; `app` is `NSApp` and `event` is the non-null
                // `NSEvent` just dequeued above, inside this iteration's autorelease pool. Runs
                // on the main thread (only reached from `process_events`, which asserts it).
                unsafe { send1::<Id, ()>(app, rt.send_event, event) };
                sent = true;
            }
        }
    }

    /// `qt_mac_waitForMoreEvents` (:273-285), bounded by `timeout`: waits for an event, then puts
    /// it back at the front of the queue. Returns whether one arrived.
    fn wait_for_more_events(app: Id, timeout: Option<Duration>) -> bool {
        let rt = &*RUNTIME;
        let _pool = AutoreleasePool::new();
        // SAFETY: [inferred] `rt.ns_date` is the `NSDate` class and the selectors are
        // `dateWithTimeIntervalSinceNow:` (one `f64` argument, returns an object) and
        // `distantFuture` (no argument, returns an object), so the `send0`/`send1` signatures
        // match the methods; `until` is passed straight to `next_event`. `app` is `NSApp` from
        // `process_events`, the only caller, which asserts it runs on the main thread; the
        // event it returns is non-null (checked) and re-posted with `postEvent:atStart:`
        // (`Id`, `BOOL`) as in Qt's `qt_mac_waitForMoreEvents`.
        unsafe {
            let until: Id = match timeout {
                Some(timeout) => send1(rt.ns_date, rt.date_from_now, timeout.as_secs_f64()),
                None => send0(rt.ns_date, rt.distant_future),
            };
            let event = next_event(app, until);
            if event.is_null() {
                return false;
            }
            send2::<Id, i8, ()>(app, rt.post_event, event, 1);
        }
        true
    }
}

impl EventDispatcher for CocoaEventDispatcher {
    fn wake_up(&self) {
        self.wake.signal();
    }

    fn clone_handle(&self) -> Arc<dyn EventDispatcherHandle> {
        Arc::new(self.clone_handle())
    }

    fn install_native_event_filter(&self, filter: Box<dyn NativeEventFilter>) {
        self.native_filters.install(filter);
    }

    fn filter_native_event(
        &self,
        event_type: &str,
        msg: &NativeMessage,
        result: &mut isize,
    ) -> bool {
        self.native_filters.filter_native(event_type, msg, result)
    }

    /// `QCocoaEventDispatcher::processEvents` without `EventLoopExec` (:287-510): `Normal` if an
    /// event was sent (Qt's `true`), otherwise `Timeout`.
    fn process_events(
        &self,
        can_wait: bool,
        next_timer_timeout: Option<Duration>,
    ) -> DispatchResult {
        crate::object::ThreadContext::assert_main_thread("CocoaEventDispatcher::process_events");

        let _pool = AutoreleasePool::new();
        let app = ns_app();
        let outer_wait = WAIT_FOR_MORE_EVENTS.swap(can_wait, Ordering::SeqCst);
        self.ensure_nsapp_initialized(app);

        let mut wait = can_wait;
        let sent = loop {
            if self.send_queued_events(app) {
                break true;
            }
            if !wait || !Self::wait_for_more_events(app, next_timer_timeout) {
                break false;
            }
            // Send the event that ended the wait, without waiting again (:483-487).
            WAIT_FOR_MORE_EVENTS.store(false, Ordering::SeqCst);
            wait = false;
        };
        WAIT_FOR_MORE_EVENTS.store(outer_wait, Ordering::SeqCst);

        if sent {
            DispatchResult::Normal
        } else {
            DispatchResult::Timeout
        }
    }

    /// Timers live in the event loop's `TimerRegistry`; `process_events` waits until the next one
    /// is due.
    fn register_timer(&self, _entry: &TimerEntry) {}

    fn unregister_timer(&self, _entry: &TimerEntry) {}

    fn send_timer_events(&self, registry: &Mutex<TimerRegistry>) {
        crate::timer::deliver_due_timers(registry, crate::timer::current_time_ms(), []);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cocoa_dispatcher_main_thread_enforcement() {
        let handle = std::thread::spawn(|| {
            crate::object::ThreadContext::init_current(false, None);
            let dispatcher = CocoaEventDispatcher::new();
            let _ = dispatcher.process_events(false, None);
        });

        let join_res = handle.join();
        assert!(
            join_res.is_err(),
            "Calling process_events from non-main thread must panic"
        );
    }
}
