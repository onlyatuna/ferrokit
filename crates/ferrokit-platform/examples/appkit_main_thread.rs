// SPDX-License-Identifier: LGPL-3.0-only
//! macOS AppKit checks on the process's real main thread.
//!
//! AppKit objects (NSWindow, NSStatusItem, NSMenu, NSCursor, NSApplication) belong to the main
//! thread, and libtest runs every `#[test]` on a worker thread. Qt runs its tests the same way as
//! this program: `QTEST_MAIN` expands to `main()`, which creates the application and then runs the
//! test functions on that thread (testlib/qtest.h:233-241, 278-297).
//!
//! With no argument this program is the supervisor: it runs each check in [`checks::CHECKS`] in
//! its own child process (one Objective-C exception must not hide the other results), with a
//! time limit, and without the variables that make `CocoaPlatformIntegration::is_headless` hand
//! out Generic stand-ins. Each child first proves it is on the OS main thread
//! (`pthread_main_np`), creates `GuiApplication` like `QTEST_MAIN`, then runs one check.
//!
//! A check asserts what AppKit reports back (window level, visibility, subviews, ...), compared
//! with what Qt does with the same API (cited per check); ferrokit-side state alone does not pass.
//! Child results:
//! - `ok` (exit 0): every assertion held.
//! - `FAILED` (exit 1): an assertion about AppKit or ferrokit behavior failed.
//! - `ENV` (exit 3): the machine lacks something the check needs (said which); no behavior
//!   assertion failed.
//! - `CRASH` (signal): typically an Objective-C exception, which Rust cannot unwind; the child
//!   prints its name, reason and call stack first.
//! - `PANIC` (exit 101), `TIMEOUT`, `ERROR` (any other exit).
//!
//! Not covered: native event delivery. No check here runs the event loop; the Cocoa event loop is
//! checked by ferrokit-gui's `main_thread_entry` example.
//!
//! Usage: cargo run -p ferrokit-platform --example appkit_main_thread [CHECK | --known-open=A,B,...]
//! (macOS only). `--known-open` names checks that are expected not to be `ok` because of an open
//! gap: the supervisor then exits 0 when every other check is `ok` and every listed one
//! is still not `ok`, and exits 1 otherwise, so a newly passing check must leave the list.

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("appkit_main_thread checks macOS behavior only");
    std::process::exit(2);
}

#[cfg(target_os = "macos")]
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let code = match args.get(1).map(String::as_str) {
        None => harness::supervise(&args[0], &[]),
        Some(arg) => match arg.strip_prefix("--known-open=") {
            Some(list) => harness::supervise(&args[0], &list.split(',').collect::<Vec<_>>()),
            None => harness::run_child(arg),
        },
    };
    std::process::exit(code);
}

#[cfg(target_os = "macos")]
mod harness {
    use std::os::unix::process::ExitStatusExt;
    use std::process::{Command, ExitStatus};
    use std::time::{Duration, Instant};

    use ferrokit_gui::application::GuiApplication;

    use super::checks::{Recorder, CHECKS};
    use super::objc;

    const CHILD_TIMEOUT: Duration = Duration::from_secs(30);
    /// `CocoaPlatformIntegration::is_headless` (integration.rs) turns windows and tray icons into
    /// Generic stand-ins when any of these is set, and CI sets the first two.
    const HEADLESS_VARS: [&str; 4] = ["CI", "GITHUB_ACTIONS", "QT_QPA_PLATFORM", "FERROKIT_HEADLESS"];

    fn classify(status: Option<ExitStatus>) -> (&'static str, String) {
        let Some(status) = status else {
            return (
                "TIMEOUT",
                format!("killed after {} s", CHILD_TIMEOUT.as_secs()),
            );
        };
        match (status.code(), status.signal()) {
            (Some(0), _) => ("ok", "exit 0".into()),
            (Some(1), _) => ("FAILED", "exit 1: a behavior assertion failed".into()),
            (Some(3), _) => (
                "ENV",
                "exit 3: environment lacks what the check needs".into(),
            ),
            (Some(101), _) => ("PANIC", "exit 101: Rust panic".into()),
            (Some(code), _) => ("ERROR", format!("exit {code}")),
            (None, Some(6)) => (
                "CRASH",
                "SIGABRT: Objective-C exception (see its log above) or Rust abort".into(),
            ),
            (None, Some(sig)) => ("CRASH", format!("signal {sig}")),
            (None, None) => ("ERROR", "no exit code or signal".into()),
        }
    }

    pub fn supervise(exe: &str, known_open: &[&str]) -> i32 {
        println!(
            "children run without {HEADLESS_VARS:?}, so Cocoa factories create real AppKit objects"
        );
        if let Some(unknown) = known_open
            .iter()
            .find(|name| !CHECKS.iter().any(|c| c.name == **name))
        {
            println!("--known-open names no check: {unknown}");
            return 1;
        }
        let mut results = Vec::new();
        for check in CHECKS {
            println!("::group::{}", check.name);
            let start = Instant::now();
            let mut cmd = Command::new(exe);
            cmd.arg(check.name);
            for var in HEADLESS_VARS {
                cmd.env_remove(var);
            }
            let mut child = cmd.spawn().expect("spawn child");
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
            println!("::endgroup::");
            let (verdict, how) = classify(status);
            results.push((check.name, verdict, how, start.elapsed().as_secs_f64()));
        }

        println!("\n=== AppKit main-thread checks ===");
        for (name, verdict, how, secs) in &results {
            let open = if known_open.contains(name) {
                " [known open]"
            } else {
                ""
            };
            println!("{verdict:7} {name} ({how}, {secs:.1} s){open}");
        }
        let bad = results.iter().filter(|r| r.1 != "ok").count();
        println!("{} checks, {bad} not ok", results.len());
        let unexpected: Vec<_> = results
            .iter()
            .filter(|r| (r.1 != "ok") != known_open.contains(&r.0))
            .map(|r| r.0)
            .collect();
        if unexpected.is_empty() {
            0
        } else {
            println!(
                "not as expected (not ok and not listed, or listed and now ok): {unexpected:?}"
            );
            1
        }
    }

    extern "C" {
        fn pthread_main_np() -> std::os::raw::c_int;
    }

    fn on_os_main_thread() -> bool {
        unsafe { pthread_main_np() == 1 }
    }

    pub fn run_child(name: &str) -> i32 {
        let Some(check) = CHECKS.iter().find(|c| c.name == name) else {
            let names: Vec<_> = CHECKS.iter().map(|c| c.name).collect();
            println!("unknown check `{name}`; known: {names:?}");
            return 2;
        };
        println!("[{name}] from {}", check.origin);

        // The thread check must itself tell threads apart, or the rest proves nothing.
        let main = on_os_main_thread();
        let worker = std::thread::spawn(on_os_main_thread).join().unwrap();
        println!("[{name}] pthread_main_np: first thread {main}, spawned thread {worker}");
        if !main || worker {
            println!("[{name}] not on the OS main thread, or the check cannot tell");
            return 2;
        }

        // `QTEST_MAIN`: the application first, on this thread.
        let _app = GuiApplication::new(vec!["appkit_main_thread".to_string()]);
        objc::log_exceptions_for(name);

        let mut rec = Recorder::default();
        (check.run)(&mut rec);
        rec.exit_code()
    }
}

/// CoreGraphics calls for reading a layer's image back.
#[cfg(target_os = "macos")]
mod cg {
    use std::ffi::c_void;

    use ferrokit_platform::objc_runtime::CGRect;

    pub type CGImageRef = *mut c_void;
    pub type CGColorSpaceRef = *mut c_void;
    pub type CGContextRef = *mut c_void;

    pub const ALPHA_PREMULTIPLIED_LAST: u32 = 1;
    pub const BYTE_ORDER_32_BIG: u32 = 4 << 12;

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        pub fn CFGetTypeID(object: *const c_void) -> usize;
        pub fn CGImageGetTypeID() -> usize;
        pub fn CGImageGetWidth(image: CGImageRef) -> usize;
        pub fn CGImageGetHeight(image: CGImageRef) -> usize;
        pub fn CGColorSpaceCreateDeviceRGB() -> CGColorSpaceRef;
        pub fn CGColorSpaceRelease(space: CGColorSpaceRef);
        pub fn CGBitmapContextCreate(
            data: *mut c_void,
            width: usize,
            height: usize,
            bits_per_component: usize,
            bytes_per_row: usize,
            space: CGColorSpaceRef,
            bitmap_info: u32,
        ) -> CGContextRef;
        pub fn CGContextDrawImage(context: CGContextRef, rect: CGRect, image: CGImageRef);
        pub fn CGContextRelease(context: CGContextRef);
        pub fn CFRetain(object: *const c_void) -> *const c_void;
        pub fn CFRelease(object: *const c_void);
        pub fn CGImageGetDataProvider(image: CGImageRef) -> *mut c_void;
        pub fn CGDataProviderCopyData(provider: *mut c_void) -> *const c_void;
        pub fn CFDataGetBytePtr(data: *const c_void) -> *const u8;
        pub fn CFDataGetLength(data: *const c_void) -> isize;
    }
}

/// Raw message sends for reading AppKit state back; ferrokit's `ObjcMsg` has no getter for most of it.
#[cfg(target_os = "macos")]
mod objc {
    use std::ffi::{c_char, CStr};
    use std::sync::OnceLock;

    use ferrokit_platform::objc_runtime::{nsstring_from_str, CGRect, Class, Id, ObjcMsg, Sel};

    type Preprocessor = extern "C" fn(Id) -> Id;

    #[link(name = "objc")]
    extern "C" {
        fn objc_msgSend();
        fn objc_setExceptionPreprocessor(f: Preprocessor) -> Option<Preprocessor>;
    }

    pub fn send<R>(receiver: Id, sel: &str) -> R {
        unsafe {
            let f: extern "C" fn(Id, Sel) -> R = std::mem::transmute(objc_msgSend as *const ());
            f(receiver, Sel::register(sel))
        }
    }

    pub fn send2<A, B, R>(receiver: Id, sel: &str, a: A, b: B) -> R {
        unsafe {
            let f: extern "C" fn(Id, Sel, A, B) -> R =
                std::mem::transmute(objc_msgSend as *const ());
            f(receiver, Sel::register(sel), a, b)
        }
    }

    pub fn send1<A, R>(receiver: Id, sel: &str, arg: A) -> R {
        unsafe {
            let f: extern "C" fn(Id, Sel, A) -> R = std::mem::transmute(objc_msgSend as *const ());
            f(receiver, Sel::register(sel), arg)
        }
    }

    /// A `BOOL`-returning getter (`signed char` on x86_64, `bool` on arm64: the low byte).
    pub fn yes(receiver: Id, sel: &str) -> bool {
        send::<i8>(receiver, sel) != 0
    }

    pub fn class(name: &str) -> Id {
        Id(Class::get(name).unwrap_or_else(|| panic!("class {name}")).0)
    }

    pub fn is_kind_of(object: Id, class_name: &str) -> bool {
        !object.is_nil() && send1::<Id, i8>(object, "isKindOfClass:", class(class_name)) != 0
    }

    pub fn string(ns_string: Id) -> Option<String> {
        if ns_string.is_nil() {
            return None;
        }
        let utf8: *const c_char = send(ns_string, "UTF8String");
        (!utf8.is_null()).then(|| {
            unsafe { CStr::from_ptr(utf8) }
                .to_string_lossy()
                .into_owned()
        })
    }

    pub fn ns_string(text: &str) -> Id {
        nsstring_from_str(text)
    }

    pub fn rect(receiver: Id, sel: &str) -> CGRect {
        // SAFETY: callers pass a live object and a selector returning a `CGRect` (`frame`).
        unsafe { ObjcMsg::send_rect_return(receiver, Sel::register(sel)) }
    }

    /// Objects of `array` (an NSArray).
    pub fn array(array: Id) -> Vec<Id> {
        let count: usize = send(array, "count");
        (0..count)
            .map(|i| send1::<usize, Id>(array, "objectAtIndex:", i))
            .collect()
    }

    static CHECK_NAME: OnceLock<String> = OnceLock::new();
    static PREVIOUS: OnceLock<Option<Preprocessor>> = OnceLock::new();

    /// Logs every Objective-C exception before it is thrown, then lets it continue unchanged: it
    /// is not caught, and an exception unwinding into Rust still aborts this child. Calls the
    /// preprocessor installed before (CoreFoundation's) first, and sends only Foundation messages.
    extern "C" fn log_exception(exception: Id) -> Id {
        let exception = match PREVIOUS.get().copied().flatten() {
            Some(previous) => previous(exception),
            None => exception,
        };
        let check = CHECK_NAME.get().map(String::as_str).unwrap_or("?");
        let name = string(send(exception, "name")).unwrap_or_default();
        let reason = string(send(exception, "reason")).unwrap_or_default();
        let mut stack: Id = send(exception, "callStackSymbols");
        if stack.is_nil() {
            stack = send(class("NSThread"), "callStackSymbols");
        }
        println!("[{check}] Objective-C exception {name}: {reason}");
        for frame in array(stack) {
            println!(
                "[{check}]     {}",
                string(send(frame, "description")).unwrap_or_default()
            );
        }
        use std::io::Write;
        let _ = std::io::stdout().flush();
        exception
    }

    pub fn log_exceptions_for(check: &str) {
        let _ = CHECK_NAME.set(check.to_string());
        let previous = unsafe { objc_setExceptionPreprocessor(log_exception) };
        let _ = PREVIOUS.set(previous);
    }
}

#[cfg(target_os = "macos")]
mod checks {
    use std::fmt::Debug;
    use std::sync::{Arc, Mutex};

    use ferrokit_gui::geometry::primitives::{Point, Rect};
    use ferrokit_gui::paint::Pixmap;
    use ferrokit_gui::tiny_skia::Color;
    use ferrokit_platform::backdrop::BackdropType;
    use ferrokit_platform::menu::{CocoaMenu, PlatformMenu};
    use ferrokit_platform::objc_runtime::{CGRect, CGSize, Id};
    use ferrokit_platform::theme::PlatformTheme;
    use ferrokit_platform::tray::{CocoaStatusItem, PlatformTrayIcon};
    use ferrokit_platform::{
        ClosureWindowEventHandler, CocoaCursor, CocoaNativeEvent, CocoaNativeWindow,
        CocoaPlatformIntegration, CocoaScreen, CocoaTheme, ColorScheme, CursorShape,
        PlatformCursor, PlatformIntegration, PlatformScreen, PlatformWindow, TrayMessageIcon,
        WindowEdges, WindowFlags, WindowSystemEvent,
    };

    use super::cg;
    use super::objc::{array, class, is_kind_of, ns_string, rect, send, send1, send2, string, yes};

    /// Soft assertions: every result is printed and the check runs to the end.
    #[derive(Default)]
    pub struct Recorder {
        failed: usize,
        env_unmet: usize,
    }

    impl Recorder {
        pub fn expect(&mut self, ok: bool, what: &str) {
            println!("  {} {what}", if ok { "PASS" } else { "FAIL" });
            self.failed += usize::from(!ok);
        }

        pub fn eq<T: PartialEq + Debug>(&mut self, what: &str, actual: T, expected: T) {
            let ok = actual == expected;
            println!(
                "  {} {what}: {actual:?}{}",
                if ok { "PASS" } else { "FAIL" },
                if ok {
                    String::new()
                } else {
                    format!(", expected {expected:?}")
                }
            );
            self.failed += usize::from(!ok);
        }

        pub fn info(&self, what: &str) {
            println!("  INFO {what}");
        }

        pub fn env_unmet(&mut self, what: &str) {
            println!("  ENV  {what}");
            self.env_unmet += 1;
        }

        pub fn exit_code(&self) -> i32 {
            if self.failed > 0 {
                1
            } else if self.env_unmet > 0 {
                3
            } else {
                0
            }
        }
    }

    pub struct CheckDef {
        pub name: &'static str,
        /// The libtest test whose macOS (real AppKit) part this check replaces.
        pub origin: &'static str,
        pub run: fn(&mut Recorder),
    }

    pub const CHECKS: &[CheckDef] = &[
        CheckDef {
            name: "window_title_and_style",
            origin: "test_platform_abstractions::test_macos_objc_runtime_and_cocoa_window_lifecycle, ::test_platform_parity_gaps_verification",
            run: window_title_and_style,
        },
        CheckDef {
            name: "window_geometry",
            origin: "new: CocoaNativeWindow geometry against the NSWindow frame",
            run: window_geometry,
        },
        CheckDef {
            name: "window_level",
            origin: "test_window_activation::cocoa_window_is_active_only_when_it_is_the_key_window (tooltip level), test_platform_abstractions::test_macos_objc_runtime_and_cocoa_window_lifecycle (stays-on-top level)",
            run: window_level,
        },
        CheckDef {
            name: "window_show_hide",
            origin: "test_platform_abstractions::test_macos_objc_runtime_and_cocoa_window_lifecycle",
            run: window_show_hide,
        },
        CheckDef {
            name: "window_key_status",
            origin: "test_window_activation::cocoa_window_is_active_only_when_it_is_the_key_window",
            run: window_key_status,
        },
        CheckDef {
            name: "app_activation_policy",
            origin: "new: QCocoaIntegration makes an executable without LSUIElement a regular app",
            run: app_activation_policy,
        },
        CheckDef {
            name: "window_opacity_min_size_move",
            origin: "test_platform_modern_features::test_platform_window_move_resize_opacity_minsize",
            run: window_opacity_min_size_move,
        },
        CheckDef {
            name: "window_click_through",
            origin: "test_platform_modern_features::test_cross_platform_backdrop_and_click_through",
            run: window_click_through,
        },
        CheckDef {
            name: "window_flipped",
            origin: "test_platform_abstractions::test_macos_flipped_coordinates_and_wayland_wheel_scale",
            run: window_flipped,
        },
        CheckDef {
            name: "window_present_orientation",
            origin: "test_platform_abstractions::test_platform_parity_gaps_verification (CocoaLayerSurface present)",
            run: window_present_orientation,
        },
        CheckDef {
            name: "window_event_bridge",
            origin: "test_platform_abstractions::test_macos_objc_runtime_and_cocoa_window_lifecycle, ::test_cross_platform_input_event_bridge_queuing_and_polling",
            run: window_event_bridge,
        },
        CheckDef {
            name: "window_present",
            origin: "test_platform_abstractions::test_platform_parity_gaps_verification (CocoaLayerSurface present)",
            run: window_present,
        },
        CheckDef {
            name: "window_present_pixels",
            origin: "test_platform_abstractions::test_platform_parity_gaps_verification (CocoaLayerSurface present)",
            run: window_present_pixels,
        },
        CheckDef {
            name: "window_present_opacity",
            origin: "test_platform_abstractions::test_platform_parity_gaps_verification (CocoaLayerSurface present)",
            run: window_present_opacity,
        },
        CheckDef {
            name: "window_present_frame_immutable",
            origin: "test_platform_abstractions::test_platform_parity_gaps_verification (CocoaLayerSurface present)",
            run: window_present_frame_immutable,
        },
        CheckDef {
            name: "backdrop_on",
            origin: "test_platform_modern_features::test_cross_platform_backdrop_and_click_through",
            run: backdrop_on,
        },
        CheckDef {
            name: "backdrop_off",
            origin: "test_platform_modern_features::test_cross_platform_backdrop_and_click_through",
            run: backdrop_off,
        },
        CheckDef {
            name: "backdrop_off_without_backdrop",
            origin: "application startup without a backdrop",
            run: backdrop_off_without_backdrop,
        },
        CheckDef {
            name: "backdrop_on_off_on_off",
            origin: "application theme and mode switches",
            run: backdrop_on_off_on_off,
        },
        CheckDef {
            name: "backdrop_on_twice",
            origin: "application theme and mode switches",
            run: backdrop_on_twice,
        },
        CheckDef {
            name: "status_item_lifecycle",
            origin: "test_platform_abstractions::test_cross_platform_tray_icon_implementations, ::test_macos_cocoa_status_item_and_menu_objc_integration",
            run: status_item_lifecycle,
        },
        CheckDef {
            name: "status_item_menu",
            origin: "test_platform_abstractions::test_macos_cocoa_status_item_and_menu_objc_integration",
            run: status_item_menu,
        },
        CheckDef {
            name: "status_item_message",
            origin: "test_platform_modern_features::test_platform_tray_messages",
            run: status_item_message,
        },
        CheckDef {
            name: "menu_native",
            origin: "test_platform_abstractions::test_cross_platform_menu_implementations",
            run: menu_native,
        },
        CheckDef {
            name: "screen_metrics",
            origin: "test_platform_modern_features::test_cross_platform_screens_parity, test_platform_abstractions::test_platform_singleton_does_not_panic",
            run: screen_metrics,
        },
        CheckDef {
            name: "theme_color_scheme",
            origin: "test_platform_modern_features::test_cross_platform_theme_and_dynamic_detection",
            run: theme_color_scheme,
        },
        CheckDef {
            name: "cursor_shape",
            origin: "test_platform_modern_features::test_cross_platform_cursor_shapes",
            run: cursor_shape,
        },
        CheckDef {
            name: "integration_factory",
            origin: "test_platform_abstractions::test_platform_integration_factory, ::test_cocoa_platform_integration_status_item_and_retina",
            run: integration_factory,
        },
    ];

    fn window(title: &str, geometry: Rect, flags: WindowFlags) -> CocoaNativeWindow {
        CocoaNativeWindow::new(title, geometry, flags).expect("CocoaNativeWindow::new")
    }

    fn pixmap(size: u32, color: Color) -> Pixmap {
        let mut pixmap = Pixmap::new(size, size).expect("pixmap");
        pixmap.fill(color);
        pixmap
    }

    /// Height of the screen with the menu bar, the reference of Qt's `qt_mac_flip`
    /// (qcocoascreen.mm:227-229).
    fn primary_screen_height() -> f64 {
        let screens = array(send(class("NSScreen"), "screens"));
        rect(screens[0], "frame").size.height
    }

    fn effect_views(window: Id) -> Vec<Id> {
        let content: Id = send(window, "contentView");
        array(send(content, "subviews"))
            .into_iter()
            .filter(|&view| is_kind_of(view, "NSVisualEffectView"))
            .collect()
    }

    fn window_title_and_style(t: &mut Recorder) {
        let geometry = Rect::new(100, 100, 600, 400);
        let flags = WindowFlags::FRAMELESS | WindowFlags::STAYS_ON_TOP | WindowFlags::CLICK_THROUGH;
        let win = window("macOS HUD Panel", geometry, flags);
        t.eq("ferrokit title()", win.title(), "macOS HUD Panel");
        t.eq("ferrokit geometry()", win.geometry(), geometry);
        t.expect(!win.is_visible(), "ferrokit is_visible() is false before show");

        let w = win.ns_window();
        t.expect(
            is_kind_of(w, "NSWindow"),
            "the native window is an NSWindow",
        );
        t.eq(
            "[window title]",
            string(send(w, "title")),
            Some("macOS HUD Panel".into()),
        );
        // Qt: FramelessWindowHint -> NSWindowStyleMaskBorderless (qcocoawindow.mm:605-622).
        t.eq(
            "[window styleMask] of a frameless window",
            send::<usize>(w, "styleMask"),
            0,
        );
        // Qt: WindowTransparentForInput -> ignoresMouseEvents (qcocoawindow.mm:759-767).
        t.expect(
            yes(w, "ignoresMouseEvents"),
            "[window ignoresMouseEvents] for CLICK_THROUGH",
        );
        t.eq(
            "[window contentView] is ns_view()",
            send::<Id>(w, "contentView"),
            win.ns_view(),
        );
    }

    fn window_geometry(t: &mut Recorder) {
        // Qt maps its top-left geometry to Cocoa's bottom-left screen coordinates with
        // QCocoaScreen::mapToNative (setCocoaGeometry, qcocoawindow.mm:304).
        let height = primary_screen_height();
        t.info(&format!("primary screen height {height}"));
        let expect_frame = |t: &mut Recorder, w: Id, g: Rect, what: &str| {
            let frame = rect(w, "frame");
            let expected_y = height - f64::from(g.y + g.height);
            t.eq(
                what,
                (
                    frame.origin.x,
                    frame.origin.y,
                    frame.size.width,
                    frame.size.height,
                ),
                (
                    f64::from(g.x),
                    expected_y,
                    f64::from(g.width),
                    f64::from(g.height),
                ),
            );
        };
        let first = Rect::new(100, 120, 600, 400);
        let mut win = window("geometry", first, WindowFlags::FRAMELESS);
        expect_frame(
            t,
            win.ns_window(),
            first,
            "[window frame] after creation (x, y, w, h)",
        );
        let second = Rect::new(150, 160, 500, 300);
        win.set_geometry(second);
        expect_frame(
            t,
            win.ns_window(),
            second,
            "[window frame] after set_geometry",
        );
        t.eq("ferrokit geometry()", win.geometry(), second);
    }

    fn window_level(t: &mut Recorder) {
        // Qt: QCocoaWindow::windowLevel (qcocoawindow.mm:548-563).
        let cases = [
            (WindowFlags::NORMAL, 0, "NORMAL: NSNormalWindowLevel"),
            (WindowFlags::TOOL, 3, "TOOL: NSFloatingWindowLevel"),
            (
                WindowFlags::STAYS_ON_TOP,
                8,
                "STAYS_ON_TOP: NSModalPanelWindowLevel",
            ),
            (
                WindowFlags::TOOLTIP,
                1000,
                "TOOLTIP: NSScreenSaverWindowLevel",
            ),
        ];
        for (flags, expected, what) in cases {
            let win = window("level", Rect::new(120, 120, 160, 80), flags);
            t.eq(
                &format!("[window level] {what}"),
                send::<isize>(win.ns_window(), "level"),
                expected,
            );
        }
        let mut win = window("level", Rect::new(120, 120, 160, 80), WindowFlags::NORMAL);
        win.set_stays_on_top(true);
        t.eq(
            "[window level] after set_stays_on_top(true)",
            send::<isize>(win.ns_window(), "level"),
            8,
        );
        win.set_stays_on_top(false);
        t.eq(
            "[window level] after set_stays_on_top(false)",
            send::<isize>(win.ns_window(), "level"),
            0,
        );
        // The level comes from all flags: dropping the hint from a Tool window keeps it floating.
        let mut tool = window("level", Rect::new(120, 120, 160, 80), WindowFlags::TOOL);
        tool.set_stays_on_top(true);
        tool.set_stays_on_top(false);
        t.eq(
            "[window level] TOOL after set_stays_on_top(true) then (false)",
            send::<isize>(tool.ns_window(), "level"),
            3,
        );
    }

    fn window_show_hide(t: &mut Recorder) {
        let win = window(
            "show",
            Rect::new(100, 100, 600, 400),
            WindowFlags::FRAMELESS,
        );
        let w = win.ns_window();
        t.expect(!yes(w, "isVisible"), "[window isVisible] is NO before show");
        win.show();
        t.expect(win.is_visible(), "ferrokit is_visible() after show");
        t.expect(yes(w, "isVisible"), "[window isVisible] after show");
        win.hide();
        t.expect(!win.is_visible(), "ferrokit is_visible() is false after hide");
        t.expect(!yes(w, "isVisible"), "[window isVisible] is NO after hide");
    }

    /// `QTest::qWaitForWindowActive`: processes events until `done` or `timeout` passes.
    fn wait_until(timeout: std::time::Duration, done: impl Fn() -> bool) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if done() {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            ferrokit_core::application::CoreApplication::process_events(false);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    fn app_state(t: &Recorder) {
        let app: Id = send(class("NSApplication"), "sharedApplication");
        let policy: isize = send(app, "activationPolicy");
        t.info(&format!(
            "[NSApp isActive] = {}, [NSApp activationPolicy] = {policy}",
            yes(app, "isActive")
        ));
    }

    fn window_key_status(t: &mut Recorder) {
        // QGuiApplication creates the platform integration (QCocoaIntegration) with itself.
        let _integration = ferrokit_platform::platform();
        let geometry = Rect::new(120, 120, 160, 80);
        let normal = window("n", geometry, WindowFlags::NORMAL);
        t.expect(!normal.is_active(), "not active before show");
        normal.show();
        let became_key = wait_until(std::time::Duration::from_secs(5), || normal.is_active());
        t.expect(
            became_key,
            "after makeKeyAndOrderFront: and up to 5 s of event processing, the window is the \
             key window ([window isKeyWindow])",
        );
        normal.hide();
        t.expect(!normal.is_active(), "orderOut: resigns key");

        // A tooltip is ordered front but never becomes key (Qt::ToolTip takes no activation).
        let tip = window("t", geometry, WindowFlags::TOOLTIP);
        tip.show();
        t.expect(
            yes(tip.ns_window(), "isVisible"),
            "the tooltip is on screen ([window isVisible])",
        );
        t.expect(
            !tip.is_active(),
            "showing a tooltip does not take key status",
        );
        app_state(t);
    }

    /// Qt's `QCocoaIntegration` constructor calls `qt_mac_transformProccessToForegroundApplication`
    /// (qcocoaintegration.mm:140-147), which sets `NSApplicationActivationPolicyRegular` unless
    /// the Info.plist sets `LSUIElement` or `LSBackgroundOnly` (qcocoahelpers.mm:138-180). This
    /// executable has no Info.plist.
    fn app_activation_policy(t: &mut Recorder) {
        let _integration = ferrokit_platform::platform();
        let app: Id = send(class("NSApplication"), "sharedApplication");
        let policy: isize = send(app, "activationPolicy");
        t.eq(
            "[NSApp activationPolicy] after the integration exists (0 = Regular)",
            policy,
            0,
        );
        app_state(t);
    }

    fn window_opacity_min_size_move(t: &mut Recorder) {
        let mut win = window(
            "Cocoa Test",
            Rect::new(0, 0, 640, 480),
            WindowFlags::empty(),
        );
        let w = win.ns_window();
        t.eq("ferrokit minimum_size() initially", win.minimum_size(), (0, 0));
        win.set_minimum_size(200, 150);
        t.eq("ferrokit minimum_size()", win.minimum_size(), (200, 150));
        // Qt: window.contentMinSize (qcocoawindow.mm:1185).
        t.eq(
            "[window contentMinSize]",
            send::<CGSize>(w, "contentMinSize"),
            CGSize::new(200.0, 150.0),
        );
        win.set_opacity(0.75);
        t.expect(
            (win.opacity() - 0.75).abs() < 1e-4,
            "ferrokit opacity() is 0.75",
        );
        // Qt: window.alphaValue = level (qcocoawindow.mm:1212).
        t.eq("[window alphaValue]", send::<f64>(w, "alphaValue"), 0.75);
        // Qt: startSystemMove only while the left button alone is pressed (qcocoawindow.mm:366-370).
        let pressed: usize = send(class("NSEvent"), "pressedMouseButtons");
        t.eq(
            &format!("start_system_move() with [NSEvent pressedMouseButtons] = {pressed}"),
            win.start_system_move(),
            pressed == 1,
        );
        // Qt: QCocoaWindow has no startSystemResize; QPlatformWindow's returns false
        // (qplatformwindow.cpp:495-498).
        t.eq(
            "start_system_resize(RIGHT)",
            win.start_system_resize(WindowEdges::RIGHT),
            false,
        );
    }

    fn window_click_through(t: &mut Recorder) {
        let mut win = window(
            "CocoaHUD",
            Rect::new(50, 50, 600, 400),
            WindowFlags::FRAMELESS,
        );
        let w = win.ns_window();
        win.set_click_through(true);
        t.expect(
            yes(w, "ignoresMouseEvents"),
            "[window ignoresMouseEvents] after set_click_through(true)",
        );
        win.set_click_through(false);
        t.expect(
            !yes(w, "ignoresMouseEvents"),
            "[window ignoresMouseEvents] is NO after set_click_through(false)",
        );
    }

    fn window_flipped(t: &mut Recorder) {
        let win = window(
            "Flipped View Test",
            Rect::new(100, 200, 640, 480),
            WindowFlags::NORMAL,
        );
        let content: Id = send(win.ns_window(), "contentView");
        // Qt: QNSView -isFlipped returns YES (qnsview_drawing.mm:67-70), and every QCocoaWindow's
        // view is a QNSView (qcocoawindow.mm:120).
        t.info(&format!(
            "content view class responds as NSView: {}",
            is_kind_of(content, "NSView")
        ));
        t.expect(
            yes(content, "isFlipped"),
            "[contentView isFlipped] (QNSView returns YES)",
        );
        t.eq(
            "ferrokit is_flipped() matches AppKit",
            win.is_flipped(),
            yes(content, "isFlipped"),
        );
    }

    fn window_present_orientation(t: &mut Recorder) {
        // Qt's QNSView is flipped and its layer shows the backing store's first row at the top
        // (qnsview_drawing.mm:67-70; qcocoabackingstore.mm:392). Render the content layer the way
        // Core Animation composites it and check which half of the frame comes out on top.
        let mut win = window(
            "orientation",
            Rect::new(100, 100, 100, 100),
            WindowFlags::FRAMELESS,
        );
        win.show();
        let mut frame = pixmap(100, Color::from_rgba8(0, 0, 255, 255));
        for px in frame.data_mut()[..100 * 50 * 4].as_chunks_mut::<4>().0 {
            *px = [255, 0, 0, 255];
        }
        println!("  INFO present() -> {:?}", win.present(&mut frame, 1.0));
        let content: Id = send(win.ns_window(), "contentView");
        let layer: Id = send(content, "layer");
        t.info(&format!(
            "[contentView isFlipped] {}, [layer contentsAreFlipped] {}, [layer isGeometryFlipped] {}",
            yes(content, "isFlipped"),
            yes(layer, "contentsAreFlipped"),
            yes(layer, "isGeometryFlipped")
        ));
        // A bitmap context's first memory row is its top edge.
        let mut rgba = vec![0u8; 100 * 100 * 4];
        unsafe {
            let space = cg::CGColorSpaceCreateDeviceRGB();
            let context = cg::CGBitmapContextCreate(
                rgba.as_mut_ptr().cast(),
                100,
                100,
                8,
                100 * 4,
                space,
                cg::ALPHA_PREMULTIPLIED_LAST | cg::BYTE_ORDER_32_BIG,
            );
            let _: () = send1(layer, "renderInContext:", context);
            cg::CGContextRelease(context);
            cg::CGColorSpaceRelease(space);
        }
        let row = |y: usize| rgba[(y * 100 + 50) * 4..(y * 100 + 50) * 4 + 4].to_vec();
        t.eq(
            "RGBA near the top of the rendered layer",
            row(10),
            vec![255, 0, 0, 255],
        );
        t.eq(
            "RGBA near the bottom of the rendered layer",
            row(90),
            vec![0, 0, 255, 255],
        );
    }

    fn window_event_bridge(t: &mut Recorder) {
        let mut win = window(
            "macOS HUD Panel",
            Rect::new(100, 100, 600, 400),
            WindowFlags::FRAMELESS,
        );
        let received = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = Arc::clone(&received);
        win.set_event_handler(Box::new(ClosureWindowEventHandler::new(move |event| {
            let entry = match event {
                WindowSystemEvent::MousePress { pos, .. } => {
                    format!("MousePress({},{})", pos.x, pos.y)
                }
                WindowSystemEvent::MouseRelease { button, .. } => {
                    format!("MouseRelease({button:?})")
                }
                WindowSystemEvent::MouseMove { pos, .. } => {
                    format!("MouseMove({},{})", pos.x, pos.y)
                }
                WindowSystemEvent::Wheel { delta, .. } => format!("Wheel({})", delta.y),
                WindowSystemEvent::Resize { size } => {
                    format!("Resize({},{})", size.width, size.height)
                }
                WindowSystemEvent::CloseRequest => "CloseRequest".to_string(),
                _ => return,
            };
            sink.lock().unwrap().push(entry);
        })));

        let dispatched = [
            CocoaNativeEvent::MouseDown {
                x: 40.0,
                y: 60.0,
                button: 0,
                modifiers: 0,
            },
            CocoaNativeEvent::MouseMoved {
                x: 80.0,
                y: 120.0,
                modifiers: 0,
            },
            CocoaNativeEvent::ScrollWheel {
                x: 80.0,
                y: 120.0,
                delta_x: 0.0,
                delta_y: -15.0,
                modifiers: 0,
            },
            CocoaNativeEvent::WindowResized {
                width: 800.0,
                height: 500.0,
            },
            CocoaNativeEvent::WindowCloseRequested,
        ]
        .into_iter()
        .all(|event| win.dispatch_cocoa_event(event));
        t.expect(dispatched, "dispatch_cocoa_event accepts every event");
        t.eq(
            "events delivered by dispatch_cocoa_event",
            received.lock().unwrap().clone(),
            vec![
                "MousePress(40,60)",
                "MouseMove(80,120)",
                "Wheel(-15)",
                "Resize(800,500)",
                "CloseRequest",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
        );

        received.lock().unwrap().clear();
        win.queue_cocoa_event(CocoaNativeEvent::MouseUp {
            x: 50.0,
            y: 80.0,
            button: 0,
            modifiers: 0,
        });
        t.eq("poll_events() after one queued event", win.poll_events(), 1);
        t.eq(
            "events delivered by poll_events",
            received.lock().unwrap().clone(),
            vec!["MouseRelease(Left)".to_string()],
        );
    }

    /// A shown 100x100 frameless window after `present(red, opacity)`, and its content layer.
    fn presented_window(opacity: f32) -> (CocoaNativeWindow, Id) {
        let mut win = window(
            "present",
            Rect::new(100, 100, 100, 100),
            WindowFlags::FRAMELESS,
        );
        win.show();
        let mut frame = pixmap(100, Color::from_rgba8(255, 0, 0, 255));
        let presented = win.present(&mut frame, opacity);
        println!("  INFO present() -> {presented:?}");
        let content: Id = send(win.ns_window(), "contentView");
        let layer: Id = send(content, "layer");
        (win, layer)
    }

    fn window_present(t: &mut Recorder) {
        let (win, layer) = presented_window(1.0);
        let content: Id = send(win.ns_window(), "contentView");
        // Qt: QNSView is layer-backed (qnsview_drawing.mm:57) and the backing store sets the
        // content layer's contents (QCocoaWindow::contentLayer, qcocoawindow.mm:2235-2241;
        // qcocoabackingstore.mm:392).
        t.expect(yes(content, "wantsLayer"), "[contentView wantsLayer]");
        t.expect(!layer.is_nil(), "the content view has a layer");
        let contents: Id = send(layer, "contents");
        let image = contents.as_ptr() as cg::CGImageRef;
        let is_image =
            !contents.is_nil() && unsafe { cg::CFGetTypeID(image) == cg::CGImageGetTypeID() };
        t.expect(
            is_image,
            "[[contentView layer] contents] is a CGImage after present",
        );
        if is_image {
            t.eq(
                "contents CGImage size (the presented pixmap)",
                unsafe { (cg::CGImageGetWidth(image), cg::CGImageGetHeight(image)) },
                (100, 100),
            );
        }
    }

    fn window_present_pixels(t: &mut Recorder) {
        // The pixmap is premultiplied RGBA8 in memory (tiny-skia). Qt describes that layout
        // (QImage::Format_RGBA8888_Premultiplied, qimage.cpp:6235) to CoreGraphics as
        // kCGImageAlphaPremultipliedLast | kCGImageByteOrder32Big (qcoregraphics.mm:46-64), so
        // CoreGraphics must read back the colour that was painted, premultiplication included.
        let cases: [([u8; 4], [u8; 4]); 4] = [
            ([255, 0, 0, 255], [255, 0, 0, 255]),
            ([0, 255, 0, 255], [0, 255, 0, 255]),
            ([0, 0, 255, 255], [0, 0, 255, 255]),
            ([255, 0, 0, 128], [128, 0, 0, 128]),
        ];
        for ([r, g, b, a], expected) in cases {
            let mut win = window(
                "present",
                Rect::new(100, 100, 100, 100),
                WindowFlags::FRAMELESS,
            );
            win.show();
            let mut frame = pixmap(100, Color::from_rgba8(r, g, b, a));
            println!("  INFO present() -> {:?}", win.present(&mut frame, 1.0));
            let content: Id = send(win.ns_window(), "contentView");
            let layer: Id = send(content, "layer");
            let contents: Id = send(layer, "contents");
            let image = contents.as_ptr() as cg::CGImageRef;
            if contents.is_nil() || unsafe { cg::CFGetTypeID(image) != cg::CGImageGetTypeID() } {
                t.expect(
                    false,
                    "[[contentView layer] contents] is a CGImage after present",
                );
                return;
            }
            // Draw the layer's image into an RGBA (premultiplied last, big endian) bitmap and
            // read the centre pixel.
            let mut rgba = vec![0u8; 100 * 100 * 4];
            unsafe {
                let space = cg::CGColorSpaceCreateDeviceRGB();
                let context = cg::CGBitmapContextCreate(
                    rgba.as_mut_ptr().cast(),
                    100,
                    100,
                    8,
                    100 * 4,
                    space,
                    cg::ALPHA_PREMULTIPLIED_LAST | cg::BYTE_ORDER_32_BIG,
                );
                cg::CGContextDrawImage(context, CGRect::new(0.0, 0.0, 100.0, 100.0), image);
                cg::CGContextRelease(context);
                cg::CGColorSpaceRelease(space);
            }
            let centre = (50 * 100 + 50) * 4;
            let got = &rgba[centre..centre + 4];
            t.expect(
                got.iter().zip(expected).all(|(&g, e)| g.abs_diff(e) <= 1),
                &format!(
                    "premultiplied RGBA of the layer image's centre pixel for rgba({r},{g},{b},{a}): {got:?}, expected {expected:?} (+-1)"
                ),
            );
        }
    }

    /// The bytes behind a CGImage, copied (`CGDataProviderCopyData`).
    fn image_bytes(image: cg::CGImageRef) -> Vec<u8> {
        unsafe {
            let data = cg::CGDataProviderCopyData(cg::CGImageGetDataProvider(image));
            let bytes = std::slice::from_raw_parts(
                cg::CFDataGetBytePtr(data),
                cg::CFDataGetLength(data) as usize,
            )
            .to_vec();
            cg::CFRelease(data);
            bytes
        }
    }

    fn window_present_frame_immutable(t: &mut Recorder) {
        // Qt never writes into a buffer Core Animation still holds: a flushed IOSurface stays
        // untouched while in use and the next frame goes to another one
        // (qcocoabackingstore.mm:141,163,347,392-402). A frame given to the layer must keep its
        // bytes after the next present. Bytes are compared, not colours (the channel order is a known open item).
        let (mut win, layer) = presented_window(1.0);
        let first: Id = send(layer, "contents");
        if first.is_nil() || unsafe { cg::CFGetTypeID(first.as_ptr()) != cg::CGImageGetTypeID() } {
            t.expect(
                false,
                "[[contentView layer] contents] is a CGImage after present",
            );
            return;
        }
        let first = unsafe { cg::CFRetain(first.as_ptr()) } as cg::CGImageRef;
        let red = image_bytes(first);
        let mut blue = pixmap(100, Color::from_rgba8(0, 0, 255, 255));
        println!(
            "  INFO second present() -> {:?}",
            win.present(&mut blue, 1.0)
        );
        let second: Id = send(layer, "contents");
        t.expect(
            image_bytes(second.as_ptr() as cg::CGImageRef) != red,
            "the second frame's image has different bytes from the first",
        );
        t.expect(
            image_bytes(first) == red,
            "the first frame's image keeps its bytes after the second present",
        );
        unsafe { cg::CFRelease(first as *const _) };
    }

    fn window_present_opacity(t: &mut Recorder) {
        // Qt has one opacity knob: QCocoaWindow::setOpacity sets [NSWindow alphaValue]
        // (qcocoawindow.mm:1206-1213) and never touches the layer. The usual call sequence is
        // set_opacity(v) then present(_, v) (PlatformWindow::present_region), so the frame
        // must be composited at v exactly once: alphaValue = v and [layer opacity] = 1.
        for v in [0.0f32, 0.5, 0.85, 0.88, 1.0] {
            let mut win = window(
                "present",
                Rect::new(100, 100, 100, 100),
                WindowFlags::FRAMELESS,
            );
            win.show();
            win.set_opacity(v);
            let mut frame = pixmap(100, Color::from_rgba8(255, 0, 0, 255));
            println!("  INFO present(_, {v}) -> {:?}", win.present(&mut frame, v));
            let content: Id = send(win.ns_window(), "contentView");
            let layer: Id = send(content, "layer");
            let alpha: f64 = send(win.ns_window(), "alphaValue");
            let layer_opacity: f32 = send(layer, "opacity");
            println!("  INFO [window alphaValue] {alpha}, [layer opacity] {layer_opacity}");
            t.expect(
                (alpha - f64::from(v)).abs() < 1e-3,
                &format!("set_opacity({v}): alphaValue = {alpha}, expected {v}"),
            );
            t.expect(
                (layer_opacity - 1.0).abs() < 1e-3,
                &format!("present(_, {v}): [layer opacity] = {layer_opacity}, expected 1"),
            );
        }
    }

    fn backdrop_on(t: &mut Recorder) {
        let mut win = window(
            "CocoaHUD",
            Rect::new(50, 50, 600, 400),
            WindowFlags::FRAMELESS,
        );
        t.expect(
            win.set_backdrop(BackdropType::Acrylic, true),
            "set_backdrop(Acrylic) returns true",
        );
        // Qt: an NSVisualEffectView added to the content view, configured with the requested
        // material, blending mode and state, layer-backed and stacked below the content
        // (QCocoaWindow::manageVisualEffectArea, qcocoawindow.mm:2265-2281). Material, blending
        // mode and state are the values ferrokit maps Acrylic to (backdrop.rs).
        let views = effect_views(win.ns_window());
        t.eq(
            "NSVisualEffectView subviews of the content view",
            views.len(),
            1,
        );
        if let Some(&view) = views.first() {
            t.eq(
                "[effectView material] (Popover)",
                send::<isize>(view, "material"),
                6,
            );
            t.eq(
                "[effectView blendingMode] (BehindWindow)",
                send::<isize>(view, "blendingMode"),
                0,
            );
            t.eq(
                "[effectView state] (Active)",
                send::<isize>(view, "state"),
                1,
            );
            t.expect(yes(view, "wantsLayer"), "[effectView wantsLayer]");
            let layer: Id = send(view, "layer");
            t.eq(
                "[[effectView layer] zPosition] (below the content)",
                send::<f64>(layer, "zPosition"),
                f64::from(-f32::MAX),
            );
        }
    }

    fn backdrop_off(t: &mut Recorder) {
        let mut win = window(
            "CocoaHUD",
            Rect::new(50, 50, 600, 400),
            WindowFlags::FRAMELESS,
        );
        win.set_backdrop(BackdropType::Acrylic, true);
        let views = effect_views(win.ns_window());
        t.info(&format!("effect views after Acrylic: {}", views.len()));
        // Keep the effect view alive across its removal so it can be inspected afterwards.
        let removed = views.first().map(|&view| send::<Id>(view, "retain"));
        t.expect(
            win.set_backdrop(BackdropType::None, false),
            "set_backdrop(None) returns true",
        );
        // Qt: an empty area removes the effect view from its superview (qcocoawindow.mm:2258-2263).
        t.eq(
            "NSVisualEffectView subviews after set_backdrop(None)",
            effect_views(win.ns_window()).len(),
            0,
        );
        if let Some(view) = removed {
            t.expect(
                send::<Id>(view, "superview").is_nil(),
                "[effectView superview] is nil after set_backdrop(None)",
            );
            send::<()>(view, "release");
        }
        // Qt removes only the effect view; the content view stays (qcocoawindow.mm:2261).
        t.eq(
            "[window contentView] is ns_view() after set_backdrop(None)",
            send::<Id>(win.ns_window(), "contentView"),
            win.ns_view(),
        );
    }

    fn backdrop_off_without_backdrop(t: &mut Recorder) {
        // Applications call set_backdrop(None) on a new window that has no backdrop.
        // Qt: no effect view for the identifier and an empty
        // area -> nothing is created or removed (qcocoawindow.mm:2258-2276).
        let mut win = window(
            "CocoaHUD",
            Rect::new(50, 50, 600, 400),
            WindowFlags::FRAMELESS,
        );
        t.expect(
            win.set_backdrop(BackdropType::None, false),
            "set_backdrop(None) on a new window returns true",
        );
        t.eq(
            "NSVisualEffectView subviews",
            effect_views(win.ns_window()).len(),
            0,
        );
        t.eq(
            "[window contentView] is ns_view()",
            send::<Id>(win.ns_window(), "contentView"),
            win.ns_view(),
        );
    }

    fn backdrop_on_off_on_off(t: &mut Recorder) {
        let mut win = window(
            "CocoaHUD",
            Rect::new(50, 50, 600, 400),
            WindowFlags::FRAMELESS,
        );
        for round in 1..=2 {
            t.expect(
                win.set_backdrop(BackdropType::Acrylic, true),
                &format!("round {round}: set_backdrop(Acrylic) returns true"),
            );
            t.eq(
                &format!("round {round}: NSVisualEffectView subviews after Acrylic"),
                effect_views(win.ns_window()).len(),
                1,
            );
            t.expect(
                win.set_backdrop(BackdropType::None, false),
                &format!("round {round}: set_backdrop(None) returns true"),
            );
            t.eq(
                &format!("round {round}: NSVisualEffectView subviews after None"),
                effect_views(win.ns_window()).len(),
                0,
            );
        }
    }

    fn backdrop_on_twice(t: &mut Recorder) {
        // Qt keeps one effect view per area and updates it in place (qcocoawindow.mm:2258-2259,
        // 2278-2281).
        let mut win = window(
            "CocoaHUD",
            Rect::new(50, 50, 600, 400),
            WindowFlags::FRAMELESS,
        );
        win.set_backdrop(BackdropType::Acrylic, true);
        win.set_backdrop(BackdropType::Mica, true);
        let views = effect_views(win.ns_window());
        t.eq(
            "NSVisualEffectView subviews after Acrylic, Mica",
            views.len(),
            1,
        );
        if let Some(&view) = views.last() {
            t.eq(
                "[effectView material] (HUDWindow, the Mica mapping in backdrop.rs)",
                send::<isize>(view, "material"),
                13,
            );
        }
    }

    fn status_item_lifecycle(t: &mut Recorder) {
        let mut item = CocoaStatusItem::new(1001);
        t.eq("ferrokit item_id()", item.item_id(), 1001);
        let native = item.native_status_item();
        t.expect(
            is_kind_of(native, "NSStatusItem"),
            "the native item is an NSStatusItem",
        );
        // Qt: statusItemWithLength:NSSquareStatusItemLength (qcocoasystemtrayicon.mm:37).
        t.eq("[statusItem length]", send::<f64>(native, "length"), -2.0);

        let button: Id = send(native, "button");
        t.expect(
            item.set_icon(&pixmap(18, Color::from_rgba8(255, 255, 255, 255)))
                .is_ok(),
            "set_icon returns Ok",
        );
        // Qt: button.image (qcocoasystemtrayicon.mm:162).
        t.expect(
            !send::<Id>(button, "image").is_nil(),
            "[[statusItem button] image] is set",
        );
        t.expect(
            item.set_tooltip("Claude HUD macOS").is_ok(),
            "set_tooltip returns Ok",
        );
        // Qt: button.toolTip (qcocoasystemtrayicon.mm:195-200).
        t.eq(
            "[[statusItem button] toolTip]",
            string(send(button, "toolTip")),
            Some("Claude HUD macOS".into()),
        );
        // Qt never sets the button title, so the tooltip text is not drawn in the menu bar.
        let title = string(send(button, "title"));
        t.expect(
            title.as_deref() != Some("Claude HUD macOS"),
            &format!("[[statusItem button] title] is not the tooltip: {title:?}"),
        );

        t.expect(item.show().is_ok(), "show returns Ok");
        t.expect(
            item.is_visible() && yes(native, "isVisible"),
            "ferrokit is_visible() and [statusItem isVisible] after show",
        );
        t.expect(item.hide().is_ok(), "hide returns Ok");
        t.expect(
            !item.is_visible() && !yes(native, "isVisible"),
            "ferrokit is_visible() and [statusItem isVisible] are false after hide",
        );

        // Qt: the destructor removes the item from the status bar (qcocoasystemtrayicon.mm:54).
        // No AppKit state found so far reports the removal reliably: `statusBar` stays set after
        // removeStatusItem: and `isVisible` differs between runs, on an item ferrokit never touched
        // too. Report both for the record; drop's removal is not asserted.
        let bar: Id = send(class("NSStatusBar"), "systemStatusBar");
        let raw: Id = send1(bar, "statusItemWithLength:", -2.0f64);
        let _: Id = send(raw, "retain");
        let _: () = send1(bar, "removeStatusItem:", raw);
        t.info(&format!(
            "AppKit only, after removeStatusItem: statusBar nil {}, isVisible {}",
            send::<Id>(raw, "statusBar").is_nil(),
            yes(raw, "isVisible")
        ));
        let _: () = send(raw, "release");

        let _: Id = send(native, "retain");
        drop(item);
        t.info(&format!(
            "ferrokit item after drop: statusBar nil {}, isVisible {}",
            send::<Id>(native, "statusBar").is_nil(),
            yes(native, "isVisible")
        ));
        let _: () = send(native, "release");
    }

    fn status_item_menu(t: &mut Recorder) {
        let mut item = CocoaStatusItem::new(1001);
        let mut menu = Box::new(CocoaMenu::new());
        let open = menu.add_action(1, "Open Monitor");
        let click_through = menu.add_checkable(2, "Click Through", false);
        menu.add_separator();
        let _quit = menu.add_action(3, "Quit");
        t.eq("ferrokit action text", open.text(), "Open Monitor".to_string());
        t.expect(!click_through.is_checked(), "checkable starts unchecked");
        menu.trigger_item(2);
        t.expect(
            click_through.is_checked(),
            "trigger_item toggles the checkable",
        );

        let ns_menu = Id(menu.native_handle() as *mut std::ffi::c_void);
        item.set_menu(menu);
        t.eq(
            "[statusItem menu] is the ferrokit menu",
            send::<Id>(item.native_status_item(), "menu"),
            ns_menu,
        );
        t.eq(
            "[menu numberOfItems]",
            send::<isize>(ns_menu, "numberOfItems"),
            4,
        );
        t.expect(item.show().is_ok() && item.is_visible(), "show with a menu");
        t.expect(item.hide().is_ok(), "hide returns Ok");
    }

    fn status_item_message(t: &mut Recorder) {
        let mut item = CocoaStatusItem::new(101);
        t.expect(item.supports_messages(), "supports_messages()");
        t.eq("last_message() before", item.last_message(), None);
        // Qt uses the same (deprecated) NSUserNotificationCenter (qcocoasystemtrayicon.mm:219-234).
        let center: Id = send(
            class("NSUserNotificationCenter"),
            "defaultUserNotificationCenter",
        );
        if center.is_nil() {
            t.env_unmet("[NSUserNotificationCenter defaultUserNotificationCenter] is nil (no app bundle); delivery is not observable");
        }
        t.expect(
            item.show_message("ClaudeHUD", "Quota reached", TrayMessageIcon::Warning, 8000)
                .is_ok(),
            "show_message returns Ok",
        );
        t.eq(
            "last_message()",
            item.last_message(),
            Some(("ClaudeHUD", "Quota reached", TrayMessageIcon::Warning, 8000)),
        );
        if !center.is_nil() {
            let delivered = array(send(center, "deliveredNotifications"));
            let titles: Vec<_> = delivered
                .iter()
                .map(|&n| string(send(n, "title")))
                .collect();
            t.info(&format!("delivered notification titles: {titles:?}"));
        }
    }

    fn menu_native(t: &mut Recorder) {
        use std::sync::atomic::{AtomicBool, Ordering};
        let mut menu = CocoaMenu::new();
        let action = menu.add_action(201, "Preferences");
        let check = menu.add_checkable(202, "Always on Top", false);
        menu.add_separator();
        let clicked = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&clicked);
        action
            .activated()
            .connect(move |()| flag.store(true, Ordering::SeqCst));

        let ns_menu = Id(menu.native_handle() as *mut std::ffi::c_void);
        t.expect(
            is_kind_of(ns_menu, "NSMenu"),
            "the native menu is an NSMenu",
        );
        t.eq(
            "[menu numberOfItems]",
            send::<isize>(ns_menu, "numberOfItems"),
            3,
        );
        let item = |i: isize| send1::<isize, Id>(ns_menu, "itemAtIndex:", i);
        t.eq(
            "[[menu itemAtIndex:0] title]",
            string(send(item(0), "title")),
            Some("Preferences".into()),
        );
        t.eq(
            "[[menu itemAtIndex:1] state] before",
            send::<isize>(item(1), "state"),
            0,
        );
        t.expect(
            yes(item(2), "isSeparatorItem"),
            "[[menu itemAtIndex:2] isSeparatorItem]",
        );

        menu.show_popup(Point::new(120, 240));
        t.expect(menu.is_popped_up(), "ferrokit is_popped_up() after show_popup");
        t.eq(
            "ferrokit popup_pos()",
            menu.popup_pos(),
            Some(Point::new(120, 240)),
        );
        t.expect(
            menu.trigger_item(201) && clicked.load(Ordering::SeqCst),
            "trigger_item(201) emits activated",
        );
        t.expect(!check.is_checked(), "checkable starts unchecked");
        t.expect(
            menu.trigger_item(202) && check.is_checked(),
            "trigger_item(202) checks it",
        );
        t.eq(
            "[[menu itemAtIndex:1] state] after trigger (NSControlStateValueOn)",
            send::<isize>(item(1), "state"),
            1,
        );
        menu.dismiss();
        t.expect(
            !menu.is_popped_up(),
            "ferrokit is_popped_up() is false after dismiss",
        );
    }

    fn screen_metrics(t: &mut Recorder) {
        // Qt's primary screen is the main display, the first NSScreen (qcocoascreen.mm:132, 227).
        let ns_screens = array(send(class("NSScreen"), "screens"));
        let first = ns_screens[0];
        let frame = rect(first, "frame");
        let scale: f64 = send(first, "backingScaleFactor");
        t.info(&format!(
            "{} NSScreen(s); first: {}x{} at backingScaleFactor {scale}",
            ns_screens.len(),
            frame.size.width,
            frame.size.height
        ));

        let primary = CocoaScreen::primary();
        t.expect(primary.is_primary(), "ferrokit primary().is_primary()");
        t.eq(
            "ferrokit primary() size",
            (primary.geometry().width, primary.geometry().height),
            (frame.size.width as i32, frame.size.height as i32),
        );
        t.expect(
            primary.available_geometry().height <= primary.geometry().height,
            "available height <= height",
        );
        t.eq(
            "ferrokit primary() device_pixel_ratio",
            f64::from(primary.device_pixel_ratio()),
            scale,
        );
        t.eq(
            "ferrokit screens().len()",
            CocoaScreen::screens().len(),
            ns_screens.len(),
        );
        let g = primary.geometry();
        let center = Point::new(g.x + g.width / 2, g.y + g.height / 2);
        t.expect(
            CocoaScreen::screen_at(center).is_some(),
            "screen_at(center of the primary screen)",
        );

        let win = window("dpr", Rect::new(100, 100, 200, 150), WindowFlags::FRAMELESS);
        win.show();
        let window_scale: f64 = send(win.ns_window(), "backingScaleFactor");
        t.eq(
            "window device_pixel_ratio() vs [window backingScaleFactor]",
            f64::from(win.device_pixel_ratio()),
            window_scale,
        );
        if scale != 2.0 {
            t.info("this display is not 2x; the libtest's 2.0 expectations were the non-macOS mock's values");
        }
    }

    fn theme_color_scheme(t: &mut Recorder) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        // Qt: the effective appearance's best match among Aqua and DarkAqua
        // (qcocoatheme.mm:507-509).
        let app: Id = send(class("NSApplication"), "sharedApplication");
        let appearance: Id = send(app, "effectiveAppearance");
        let names = [
            ns_string("NSAppearanceNameAqua"),
            ns_string("NSAppearanceNameDarkAqua"),
        ];
        let names_array: Id = send2(
            class("NSArray"),
            "arrayWithObjects:count:",
            names.as_ptr(),
            names.len(),
        );
        let best = string(send1(
            appearance,
            "bestMatchFromAppearancesWithNames:",
            names_array,
        ));
        t.info(&format!(
            "effective appearance {:?}, best match {best:?}",
            string(send(appearance, "name"))
        ));
        let expected = if best.as_deref() == Some("NSAppearanceNameDarkAqua") {
            ColorScheme::Dark
        } else {
            ColorScheme::Light
        };

        let theme = CocoaTheme::new();
        t.eq(
            "ferrokit color_scheme() vs the appearance",
            theme.color_scheme(),
            expected,
        );
        let changes = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&changes);
        theme.theme_changed().connect(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        theme.set_color_scheme(ColorScheme::Light);
        t.eq(
            "color_scheme() after set Light",
            theme.color_scheme(),
            ColorScheme::Light,
        );
        theme.set_color_scheme(ColorScheme::Dark);
        t.eq(
            "color_scheme() after set Dark",
            theme.color_scheme(),
            ColorScheme::Dark,
        );
        t.expect(changes.load(Ordering::SeqCst) >= 1, "theme_changed emitted");
    }

    fn cursor_shape(t: &mut Recorder) {
        let mut cursor = CocoaCursor::new();
        t.eq(
            "ferrokit current_shape() initially",
            cursor.current_shape(),
            CursorShape::Arrow,
        );
        for (shape, native) in [
            (CursorShape::SizeHor, "resizeLeftRightCursor"),
            (CursorShape::SizeVer, "resizeUpDownCursor"),
            (CursorShape::PointingHand, "pointingHandCursor"),
        ] {
            cursor.change_cursor(shape);
            t.eq("ferrokit current_shape()", cursor.current_shape(), shape);
            let current: Id = send(class("NSCursor"), "currentCursor");
            let wanted: Id = send(class("NSCursor"), native);
            t.eq(
                &format!("[NSCursor currentCursor] is [NSCursor {native}]"),
                current,
                wanted,
            );
        }
    }

    fn integration_factory(t: &mut Recorder) {
        let integration = CocoaPlatformIntegration::default();
        let geometry = Rect::new(50, 50, 200, 150);
        let win = integration
            .create_window(
                "Factory Window",
                geometry,
                WindowFlags::FRAMELESS | WindowFlags::LAYERED,
            )
            .expect("create_window");
        let handle = Id(win.native_handle() as *mut std::ffi::c_void);
        t.expect(
            is_kind_of(handle, "NSWindow"),
            "create_window returns a window backed by an NSWindow (not a Generic stand-in)",
        );
        t.eq("geometry()", win.geometry(), geometry);
        win.show();
        t.expect(yes(handle, "isVisible"), "[window isVisible] after show");
        win.hide();

        let mut tray = integration
            .create_tray_icon(
                "Factory Tray",
                &pixmap(16, Color::from_rgba8(20, 120, 220, 255)),
            )
            .expect("create_tray_icon");
        t.expect(tray.show().is_ok(), "tray show returns Ok");
        t.expect(
            tray.set_tooltip("Updated Factory Tray").is_ok(),
            "tray set_tooltip returns Ok",
        );
        t.expect(tray.hide().is_ok(), "tray hide returns Ok");

        let ns_screens = array(send(class("NSScreen"), "screens"));
        let scale: f64 = send(ns_screens[0], "backingScaleFactor");
        let primary = integration.primary_screen();
        t.expect(primary.is_primary(), "primary_screen().is_primary()");
        t.eq(
            "primary_screen().device_pixel_ratio() vs the first NSScreen",
            f64::from(primary.device_pixel_ratio()),
            scale,
        );
        let scheme = integration.theme().color_scheme();
        t.expect(
            scheme != ColorScheme::Unknown,
            "theme().color_scheme() is known",
        );
    }
}
