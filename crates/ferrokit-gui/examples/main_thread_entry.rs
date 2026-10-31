// SPDX-License-Identifier: LGPL-3.0-only
//! macOS main-thread entry: application setup and the Cocoa event loop on the process's real main
//! thread.
//!
//! libtest runs every `#[test]` on a worker thread, so no `#[test]` can show what happens when an
//! application is created on the OS main thread. With no argument this program runs each check
//! below in its own child process (fresh application singleton and thread-locals), on that child's
//! main thread, and kills a child that runs past `CHILD_TIMEOUT`. It exits 1 if any check failed
//! or timed out.
//!
//! - `gui`: `GuiApplication::new` must create `CocoaEventDispatcher` (the dispatcher exists), and
//!   its event loop must deliver a ferrokit timer event and return the code that event passed to `exit`.
//! - `core`: the same with `CoreApplication::new`, which must create `UnixEventDispatcher`.
//! - `runloop`: a `CFRunLoopTimer` on the main run loop's default mode must fire while `exec` runs
//!   (the Cocoa event loop runs). Qt waits in `[NSApp nextEventMatchingMask:…]` or `[NSApp run]`,
//!   both of which run the main run loop (qcocoaeventdispatcher.mm:273-285, 375, 428-431).
//! - `nsevent`: two `NSEventTypeApplicationDefined` events posted with `[NSApp postEvent:atStart:]`
//!   must reach the native event filter as `"NSEvent"`, and the one the filter does not consume must
//!   reach `[NSApp sendEvent:]` (qcocoaeventdispatcher.mm:428-445). The events are injected by this
//!   program, not real user input.
//! - `wake_chain`: an event posted by a handler of an event that another thread posted must run on
//!   the next turn. Qt's `wakeUp` always signals its run loop source (qcocoaeventdispatcher.mm:525-531).
//! - `exit_from_thread`: `exit` called from another thread must end `exec` with no ferrokit timer running.
//!
//! Qt: `QCoreApplication` gets `QEventDispatcherUNIX` on Darwin (qcoreapplication.cpp:518-523,
//! qthread_unix.cpp:316-325); `QGuiApplication` takes its dispatcher from the platform plugin
//! (qguiapplication.cpp:1629-1643), `QCocoaEventDispatcher` on macOS (qcocoaintegration.mm:353-356).
//!
//! Usage: cargo run -p ferrokit-gui --example main_thread_entry   (macOS only)

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("main_thread_entry checks macOS behavior only");
    std::process::exit(2);
}

#[cfg(target_os = "macos")]
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let code = match args.get(1).map(String::as_str) {
        None => macos::supervise(&args[0]),
        Some(mode) => macos::run_child(mode),
    };
    std::process::exit(code);
}

#[cfg(target_os = "macos")]
mod macos {
    use std::process::Command;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use ferrokit_core::application::CoreApplication;
    use ferrokit_core::event::{Event, EventKind, NativeEventFilter, NativeMessage};
    use ferrokit_core::event_loop::{
        get_thread_event_sender, post_event_to_thread, CocoaEventDispatcher, UnixEventDispatcher,
    };
    use ferrokit_core::object::{ObjectId, ThreadId};
    use ferrokit_core::timer::Timer;
    use ferrokit_gui::application::GuiApplication;
    use parking_lot::Mutex;

    use super::appkit::{self, Id};

    const MODES: [&str; 6] = [
        "gui",
        "core",
        "runloop",
        "nsevent",
        "wake_chain",
        "exit_from_thread",
    ];
    const CHILD_TIMEOUT: Duration = Duration::from_secs(30);
    const EXIT_CODE: i32 = 7;
    /// `NSEventTypeApplicationDefined`.
    const APPLICATION_DEFINED: usize = 15;
    /// Subtype that marks the events this program posts.
    const CHECK_SUBTYPE: i16 = 0x5154;

    extern "C" {
        fn pthread_main_np() -> std::os::raw::c_int;
    }

    fn on_os_main_thread() -> bool {
        // SAFETY: `pthread_main_np` takes no arguments, has no preconditions and only returns
        // whether the calling thread is the process's main thread.
        unsafe { pthread_main_np() == 1 }
    }

    pub fn supervise(exe: &str) -> i32 {
        let mut failed = 0;
        for mode in MODES {
            let start = Instant::now();
            let mut child = Command::new(exe).arg(mode).spawn().expect("spawn child");
            let status = loop {
                if let Some(status) = child.try_wait().expect("wait for child") {
                    break Some(status);
                }
                if start.elapsed() > CHILD_TIMEOUT {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                std::thread::sleep(Duration::from_millis(20));
            };
            let ok = status.is_some_and(|s| s.success());
            let how = match status {
                Some(s) => s.to_string(),
                None => format!("TIMEOUT after {} s", CHILD_TIMEOUT.as_secs()),
            };
            let verdict = if ok { "ok" } else { "FAILED" };
            let secs = start.elapsed().as_secs_f64();
            println!("{verdict:6} {mode} ({how}, {secs:.1} s)");
            failed += usize::from(!ok);
        }
        println!("{} checks, {failed} not ok", MODES.len());
        i32::from(failed > 0)
    }

    /// Starts a ferrokit timer that records it ran and asks the loop to exit with `EXIT_CODE`.
    fn schedule_exit(after_ms: u64, exit: fn(i32)) -> Arc<AtomicBool> {
        let fired = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&fired);
        Timer::single_shot(after_ms, move || {
            flag.store(true, Ordering::SeqCst);
            exit(EXIT_CODE);
        });
        fired
    }

    fn meta_call(f: impl FnOnce() + Send + 'static) -> Event {
        Event::new(EventKind::MetaCall(Box::new(move |_| f())))
    }

    pub fn run_child(mode: &str) -> i32 {
        // The thread check must itself tell threads apart, or the rest proves nothing.
        let main = on_os_main_thread();
        let worker = std::thread::spawn(on_os_main_thread).join().unwrap();
        println!("[{mode}] pthread_main_np: first thread {main}, spawned thread {worker}");
        if !main || worker {
            println!("[{mode}] FAIL: not on the OS main thread, or the check cannot tell");
            return 1;
        }
        let ok = match mode {
            "gui" | "core" => dispatcher_and_timer(mode),
            "runloop" => runloop(),
            "nsevent" => nsevent(),
            "wake_chain" => wake_chain(),
            "exit_from_thread" => exit_from_thread(),
            other => {
                println!("unknown mode `{other}`; expected one of {MODES:?}");
                return 2;
            }
        };
        i32::from(!ok)
    }

    fn args() -> Vec<String> {
        vec!["main_thread_entry".to_string()]
    }

    fn dispatcher_and_timer(mode: &str) -> bool {
        let (expected, dispatcher, code, fired) = if mode == "gui" {
            let mut app = GuiApplication::new(args());
            let dispatcher = CoreApplication::event_dispatcher_class_name();
            let fired = schedule_exit(20, GuiApplication::exit);
            let code = app.exec();
            let expected = std::any::type_name::<CocoaEventDispatcher>();
            (expected, dispatcher, code, fired)
        } else {
            let mut app = CoreApplication::new(args());
            let dispatcher = CoreApplication::event_dispatcher_class_name();
            let fired = schedule_exit(20, CoreApplication::exit);
            let code = app.exec();
            let expected = std::any::type_name::<UnixEventDispatcher>();
            (expected, dispatcher, code, fired)
        };
        let fired = fired.load(Ordering::SeqCst);
        println!(
            "[{mode}] dispatcher {dispatcher:?}, exec returned {code}, timer event ran {fired}"
        );

        let mut ok = true;
        if dispatcher != Some(expected) {
            println!("[{mode}] FAIL: expected dispatcher {expected}");
            ok = false;
        }
        if !fired || code != EXIT_CODE {
            println!(
                "[{mode}] FAIL: expected the timer event to run and exec to return {EXIT_CODE}"
            );
            ok = false;
        }
        ok
    }

    static CF_TIMER_FIRED: AtomicBool = AtomicBool::new(false);

    extern "C" fn cf_timer_fired(_timer: appkit::CFTypeRef, _info: *mut std::ffi::c_void) {
        CF_TIMER_FIRED.store(true, Ordering::SeqCst);
    }

    /// The Cocoa event loop runs: a run loop timer in the default mode fires during `exec`.
    fn runloop() -> bool {
        let mut app = GuiApplication::new(args());
        let timer = appkit::add_main_run_loop_timer(0.05, cf_timer_fired);
        let ferrokit_timer = schedule_exit(500, GuiApplication::exit);
        let code = app.exec();
        appkit::remove_run_loop_timer(timer);
        let fired = CF_TIMER_FIRED.load(Ordering::SeqCst);
        let exited = ferrokit_timer.load(Ordering::SeqCst);
        println!(
            "[runloop] CFRunLoopTimer (main run loop, default mode, 50 ms) fired during exec: \
             {fired}; ferrokit exit timer ran {exited}, exec returned {code}"
        );
        if !fired {
            println!("[runloop] FAIL: the main run loop did not run while exec waited");
        }
        if !exited || code != EXIT_CODE {
            println!("[runloop] FAIL: expected the ferrokit timer to end exec with {EXIT_CODE}");
        }
        fired && exited && code == EXIT_CODE
    }

    static ORIGINAL_SEND_EVENT: AtomicUsize = AtomicUsize::new(0);
    static SENT: Mutex<Vec<isize>> = Mutex::new(Vec::new());
    static FILTERED: Mutex<Vec<isize>> = Mutex::new(Vec::new());

    /// The check's events: `data1` if `event` is one of them.
    fn check_event_data(event: Id) -> Option<isize> {
        if event.is_null()
            || appkit::send::<usize>(event, "type") != APPLICATION_DEFINED
            || appkit::send::<i16>(event, "subtype") != CHECK_SUBTYPE
        {
            return None;
        }
        Some(appkit::send::<isize>(event, "data1"))
    }

    /// Replacement for `-[NSApplication sendEvent:]` that records the check's events and then
    /// calls the original implementation (Qt redirects the same method, qcocoaapplication.mm:117-141).
    extern "C" fn recording_send_event(this: Id, cmd: appkit::Sel, event: Id) {
        if let Some(data) = check_event_data(event) {
            SENT.lock().push(data);
        }
        // SAFETY: `ORIGINAL_SEND_EVENT` holds the previous implementation of
        // `-[NSApplication sendEvent:]`, whose signature is `(Id, Sel, Id)` with no result, the
        // type transmuted to. It is stored on the main thread right after the replacement is
        // installed and before `exec` starts the run loop, which is what calls `sendEvent:`, so
        // it is non-zero whenever this function runs. [inferred: that `sendEvent:` is only
        // called from the run loop on the main thread; it cannot be run here.]
        let original: extern "C" fn(Id, appkit::Sel, Id) =
            unsafe { std::mem::transmute(ORIGINAL_SEND_EVENT.load(Ordering::SeqCst)) };
        original(this, cmd, event);
    }

    /// Records the check's events and consumes the one with `data1 == 2`.
    struct RecordingFilter;

    impl NativeEventFilter for RecordingFilter {
        fn native_event_filter(
            &mut self,
            event_type: &str,
            msg: &NativeMessage,
            _result: &mut isize,
        ) -> bool {
            let NativeMessage::Mac(event) = msg else {
                return false;
            };
            if event_type != "NSEvent" {
                return false;
            }
            match check_event_data(*event) {
                Some(data) => {
                    FILTERED.lock().push(data);
                    data == 2
                }
                None => false,
            }
        }
    }

    /// Native NSEvents are dispatched: program-injected events reach the filter and `sendEvent:`.
    fn nsevent() -> bool {
        let mut app = GuiApplication::new(args());
        let original = appkit::replace_instance_method(
            "NSApplication",
            "sendEvent:",
            recording_send_event as *const (),
        );
        ORIGINAL_SEND_EVENT.store(original as usize, Ordering::SeqCst);
        CoreApplication::install_native_event_filter(Box::new(RecordingFilter));

        // Posted from inside exec, after the loop has started.
        Timer::single_shot(50, || {
            for data in [1, 2] {
                appkit::post_application_defined_event(CHECK_SUBTYPE, data);
            }
        });
        let exited = schedule_exit(600, GuiApplication::exit);
        let code = app.exec();

        let filtered = FILTERED.lock().clone();
        let sent = SENT.lock().clone();
        println!(
            "[nsevent] program-injected ApplicationDefined events (data1 1 and 2; the filter \
             consumes 2): native filter saw {filtered:?}, [NSApp sendEvent:] saw {sent:?}; \
             exec returned {code}"
        );
        let mut ok = true;
        if filtered != [1, 2] {
            println!(
                "[nsevent] FAIL: expected the native event filter to see [1, 2] as \"NSEvent\""
            );
            ok = false;
        }
        if sent != [1] {
            println!(
                "[nsevent] FAIL: expected [NSApp sendEvent:] to get only the unfiltered event 1"
            );
            ok = false;
        }
        if !exited.load(Ordering::SeqCst) || code != EXIT_CODE {
            println!("[nsevent] FAIL: expected the ferrokit timer to end exec with {EXIT_CODE}");
            ok = false;
        }
        ok
    }

    /// A handler of a cross-thread posted event posts again; the second event runs promptly.
    fn wake_chain() -> bool {
        const GUARD_CODE: i32 = 9;
        let mut app = GuiApplication::new(args());
        let main_thread = ThreadId::current();
        let first_ran: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
        let latency: Arc<Mutex<Option<Duration>>> = Arc::new(Mutex::new(None));
        Timer::single_shot(1500, || GuiApplication::exit(GUARD_CODE));

        let (first, lat) = (Arc::clone(&first_ran), Arc::clone(&latency));
        let poster = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            post_event_to_thread(
                main_thread,
                ObjectId(0),
                meta_call(move || {
                    *first.lock() = Some(Instant::now());
                    let first = Arc::clone(&first);
                    post_event_to_thread(
                        ThreadId::current(),
                        ObjectId(0),
                        meta_call(move || {
                            let start = first.lock().expect("first event ran");
                            *lat.lock() = Some(start.elapsed());
                            GuiApplication::exit(EXIT_CODE);
                        }),
                    );
                }),
            );
        });
        let code = app.exec();
        poster.join().unwrap();

        let ran = first_ran.lock().is_some();
        let latency = *latency.lock();
        println!(
            "[wake_chain] event posted from another thread ran {ran}; event it posted ran after \
             {latency:?}; exec returned {code} ({GUARD_CODE} = 1.5 s guard timer)"
        );
        let ok = code == EXIT_CODE && latency.is_some_and(|l| l < Duration::from_millis(500));
        if !ok {
            println!(
                "[wake_chain] FAIL: expected the second event to run within 500 ms and exec to \
                 return {EXIT_CODE}"
            );
        }
        ok
    }

    /// `exit` from another thread wakes a loop that waits with no ferrokit timer.
    fn exit_from_thread() -> bool {
        let mut app = GuiApplication::new(args());
        let main_thread = ThreadId::current();
        let exiter = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            get_thread_event_sender(main_thread)
                .expect("main thread event loop")
                .exit(EXIT_CODE);
        });
        let start = Instant::now();
        let code = app.exec();
        let elapsed = start.elapsed();
        exiter.join().unwrap();
        println!("[exit_from_thread] exec returned {code} after {elapsed:?}");
        let ok = code == EXIT_CODE && elapsed < Duration::from_secs(5);
        if !ok {
            println!("[exit_from_thread] FAIL: expected exec to return {EXIT_CODE} within 5 s");
        }
        ok
    }
}

/// The Objective-C and CoreFoundation calls the checks make.
#[cfg(target_os = "macos")]
mod appkit {
    use std::ffi::{c_char, c_void, CString};

    pub type Id = *mut c_void;
    pub type Sel = *const c_void;
    pub type CFTypeRef = *mut c_void;
    type TimerCallback = extern "C" fn(CFTypeRef, *mut c_void);

    #[repr(C)]
    struct NSPoint {
        x: f64,
        y: f64,
    }

    #[link(name = "AppKit", kind = "framework")]
    extern "C" {}

    #[link(name = "objc")]
    extern "C" {
        fn objc_getClass(name: *const c_char) -> Id;
        fn sel_registerName(name: *const c_char) -> Sel;
        fn objc_msgSend();
        fn class_getInstanceMethod(class: Id, sel: Sel) -> *mut c_void;
        fn method_setImplementation(method: *mut c_void, imp: *const ()) -> *const ();
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        static kCFRunLoopDefaultMode: *const c_void;
        fn CFRunLoopGetMain() -> CFTypeRef;
        fn CFAbsoluteTimeGetCurrent() -> f64;
        fn CFRunLoopTimerCreate(
            allocator: *const c_void,
            fire_date: f64,
            interval: f64,
            flags: usize,
            order: isize,
            callout: TimerCallback,
            context: *mut c_void,
        ) -> CFTypeRef;
        fn CFRunLoopAddTimer(run_loop: CFTypeRef, timer: CFTypeRef, mode: *const c_void);
        fn CFRunLoopTimerInvalidate(timer: CFTypeRef);
        fn CFRelease(object: CFTypeRef);
    }

    fn sel(name: &str) -> Sel {
        let name = CString::new(name).unwrap();
        // SAFETY: `name` is a `CString` that outlives the call, so the pointer is a valid
        // NUL-terminated string; `sel_registerName` only reads it (it copies the name).
        unsafe { sel_registerName(name.as_ptr()) }
    }

    fn class(name: &str) -> Id {
        let name = CString::new(name).unwrap();
        // SAFETY: `name` is a `CString` that outlives the call, so the pointer is a valid
        // NUL-terminated string; `objc_getClass` only reads it and returns null for an unknown
        // class, which is asserted below.
        let class = unsafe { objc_getClass(name.as_ptr()) };
        assert!(!class.is_null(), "class {name:?}");
        class
    }

    pub fn send<R>(receiver: Id, selector: &str) -> R {
        // SAFETY: the `transmute` turns `objc_msgSend`'s address into a function type taking the
        // receiver and selector and returning `R`, which is how it is called. This is sound only
        // if `R` is the declared return type of the method and `receiver` is nil or an object
        // that responds to it; the function is not `unsafe`, so that is a contract on its callers
        // that the compiler does not check. The callers in this file use the scalar results of
        // `type` (`usize`), `subtype` (`i16`), `data1` (`isize`) on a non-null `NSEvent`
        // (`check_event_data` rejects null), and `sharedApplication` (`Id`) on `NSApplication`.
        // [inferred from the AppKit headers; it cannot be run here.]
        unsafe {
            let f: extern "C" fn(Id, Sel) -> R = std::mem::transmute(objc_msgSend as *const ());
            f(receiver, sel(selector))
        }
    }

    /// Installs `imp` as `class`'s `selector` and returns the previous implementation.
    pub fn replace_instance_method(class_name: &str, selector: &str, imp: *const ()) -> *const () {
        // SAFETY: `class(class_name)` is a non-null class (asserted there) and `sel(selector)` a
        // registered selector; `method` is asserted non-null. `imp` must have the C signature of
        // the method it replaces; the only caller passes `recording_send_event`, whose
        // `(Id, Sel, Id)` signature with no result matches `-[NSApplication sendEvent:]`.
        // [inferred from the AppKit headers.]
        unsafe {
            let method = class_getInstanceMethod(class(class_name), sel(selector));
            assert!(!method.is_null(), "-[{class_name} {selector}]");
            method_setImplementation(method, imp)
        }
    }

    /// `[NSApp postEvent:[NSEvent otherEventWithType:NSEventTypeApplicationDefined …] atStart:NO]`.
    pub fn post_application_defined_event(subtype: i16, data1: isize) {
        // SAFETY: each `transmute` turns the address of `objc_msgSend` into the function type of
        // the one message sent, which is the way `objc_msgSend` must be called. The argument and
        // result types are written to match the Objective-C signatures
        // `+[NSEvent otherEventWithType:location:modifierFlags:timestamp:windowNumber:context:subtype:data1:data2:]`
        // (NSEventType/NSUInteger -> `usize`, `NSPoint` -> the `#[repr(C)]` `NSPoint`,
        // NSTimeInterval -> `f64`, NSInteger -> `isize`, `NSGraphicsContext *` null, `short` ->
        // `i16`) and `-[NSApplication postEvent:atStart:]` (`BOOL` -> `i8`). `class(..)` is a
        // non-null class, `event` is asserted non-null, and `app` comes from `sharedApplication`.
        // [inferred from the AppKit headers; it cannot be compiled or run here.]
        unsafe {
            let make: extern "C" fn(
                Id,
                Sel,
                usize,
                NSPoint,
                usize,
                f64,
                isize,
                Id,
                i16,
                isize,
                isize,
            ) -> Id = std::mem::transmute(objc_msgSend as *const ());
            let event = make(
                class("NSEvent"),
                sel("otherEventWithType:location:modifierFlags:timestamp:windowNumber:context:subtype:data1:data2:"),
                15,
                NSPoint { x: 0.0, y: 0.0 },
                0,
                0.0,
                0,
                std::ptr::null_mut(),
                subtype,
                data1,
                0,
            );
            assert!(!event.is_null(), "NSEvent otherEventWithType:");
            let app: Id = send(class("NSApplication"), "sharedApplication");
            let post: extern "C" fn(Id, Sel, Id, i8) =
                std::mem::transmute(objc_msgSend as *const ());
            post(app, sel("postEvent:atStart:"), event, 0);
        }
    }

    /// A one-shot `CFRunLoopTimer` on the main run loop's default mode, `delay` seconds from now.
    pub fn add_main_run_loop_timer(delay: f64, callback: TimerCallback) -> CFTypeRef {
        // SAFETY: `CFRunLoopTimerCreate` gets the default allocator (null), plain numeric
        // arguments, a null context, and `callback`, an `extern "C" fn(CFTypeRef, *mut c_void)`
        // that matches `CFRunLoopTimerCallBack`; its result is checked non-null before use.
        // `CFRunLoopGetMain` takes no arguments, and `kCFRunLoopDefaultMode` is the framework's
        // exported constant, linked by the `CoreFoundation` block above. The timer is owned by
        // us (Create rule) until `remove_run_loop_timer`. [inferred from the Core Foundation
        // headers; it cannot be compiled here.]
        unsafe {
            let fire = CFAbsoluteTimeGetCurrent() + delay;
            let timer = CFRunLoopTimerCreate(
                std::ptr::null(),
                fire,
                0.0,
                0,
                0,
                callback,
                std::ptr::null_mut(),
            );
            assert!(!timer.is_null(), "CFRunLoopTimerCreate");
            CFRunLoopAddTimer(CFRunLoopGetMain(), timer, kCFRunLoopDefaultMode);
            timer
        }
    }

    pub fn remove_run_loop_timer(timer: CFTypeRef) {
        // SAFETY: `timer` is the timer `add_main_run_loop_timer` returned (non-null, asserted
        // there) and the caller passes it here once, so the `CFRelease` balances the single
        // reference that `CFRunLoopTimerCreate` gave us (the run loop holds its own reference
        // while the timer is added). `CFRunLoopTimerInvalidate` is valid on a timer that already
        // fired and was invalidated. [inferred: Core Foundation's Create/Release ownership rule;
        // it cannot be run here.]
        unsafe {
            CFRunLoopTimerInvalidate(timer);
            CFRelease(timer);
        }
    }
}
