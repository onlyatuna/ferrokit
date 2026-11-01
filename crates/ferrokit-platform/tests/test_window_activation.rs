// SPDX-License-Identifier: LGPL-3.0-only
//! `PlatformWindow::is_active` and `WindowFlags::TOOLTIP` (`Qt::ToolTip`): a tooltip window is
//! shown without taking activation, so showing it never moves the foreground away from the
//! window it annotates.

use ferrokit_gui::geometry::primitives::Rect;
use ferrokit_platform::{
    ClosureWindowEventHandler, GenericWindow, PlatformWindow, WaylandEvent, WaylandNativeWindow,
    WindowFlags, WindowSystemEvent, X11Event, X11NativeWindow,
};

fn rect() -> Rect {
    Rect::new(120, 120, 160, 80)
}

// ---- Windows: real Win32 windows -------------------------------------------------------------

#[cfg(windows)]
mod win32 {
    use super::*;
    use ferrokit_platform::NativeWindow;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{KEYEVENTF_KEYUP, VK_MENU, keybd_event};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GWL_STYLE, GetForegroundWindow, GetWindowLongW, IsWindowVisible,
        SetForegroundWindow, WS_CAPTION, WS_EX_APPWINDOW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
        WS_EX_TOPMOST, WS_POPUP,
    };

    fn ex_style(w: &NativeWindow) -> u32 {
        // SAFETY: `w.hwnd()` is the window's own live handle (held by the borrowed `NativeWindow`);
        // `GetWindowLongW` only reads window data and validates the handle.
        unsafe { GetWindowLongW(w.hwnd(), GWL_EXSTYLE) as u32 }
    }

    fn foreground() -> isize {
        // SAFETY: `GetForegroundWindow` takes no arguments and returns a handle value only.
        unsafe { GetForegroundWindow() as isize }
    }

    #[test]
    fn tooltip_window_is_a_non_activating_topmost_tool_popup() {
        let tip = NativeWindow::new("tip", rect(), WindowFlags::TOOLTIP).unwrap();
        let ex = ex_style(&tip);
        assert_ne!(ex & WS_EX_NOACTIVATE, 0, "WS_EX_NOACTIVATE");
        assert_ne!(ex & WS_EX_TOPMOST, 0, "WS_EX_TOPMOST");
        assert_ne!(
            ex & WS_EX_TOOLWINDOW,
            0,
            "WS_EX_TOOLWINDOW (no taskbar button)"
        );
        assert_eq!(ex & WS_EX_APPWINDOW, 0);
        // SAFETY: `tip.hwnd()` is the live window created at the top of this test; user32
        // validates the handle and only reads its style.
        let style = unsafe { GetWindowLongW(tip.hwnd(), GWL_STYLE) as u32 };
        assert_ne!(style & WS_POPUP, 0);
        assert_eq!(style & WS_CAPTION, 0, "no frame");
    }

    #[test]
    fn a_regular_window_does_not_get_noactivate() {
        let w = NativeWindow::new("w", rect(), WindowFlags::FRAMELESS).unwrap();
        assert_eq!(ex_style(&w) & WS_EX_NOACTIVATE, 0);
    }

    /// Makes `w` the foreground window. A process that lost foreground rights (for example
    /// because the previous foreground window was destroyed) may need the Alt-tap trick.
    fn take_foreground(w: &NativeWindow) -> bool {
        // SAFETY: `w.hwnd()` is the live handle of the borrowed `NativeWindow`; the calls take
        // integer arguments only (`keybd_event` synthesises a key press, with no pointers).
        unsafe {
            SetForegroundWindow(w.hwnd());
            if foreground() != w.hwnd() as isize {
                keybd_event(VK_MENU as u8, 0, 0, 0);
                keybd_event(VK_MENU as u8, 0, KEYEVENTF_KEYUP, 0);
                SetForegroundWindow(w.hwnd());
            }
        }
        foreground() == w.hwnd() as isize
    }

    const SKIP: &str = "SKIPPED: the desktop session refused to give this process the foreground";

    #[test]
    fn showing_a_tooltip_window_does_not_take_the_foreground() {
        let owner = NativeWindow::new("owner", rect(), WindowFlags::FRAMELESS).unwrap();
        owner.show();
        if !take_foreground(&owner) {
            eprintln!("{SKIP}");
            return;
        }

        let tip = NativeWindow::new("tip", rect(), WindowFlags::TOOLTIP).unwrap();
        PlatformWindow::show(&tip);
        assert_ne!(
            // SAFETY: `tip.hwnd()` is the live window created above; `IsWindowVisible` only reads.
            unsafe { IsWindowVisible(tip.hwnd()) },
            0,
            "the tip is visible"
        );
        assert_eq!(
            foreground(),
            owner.hwnd() as isize,
            "showing the tip changed the foreground window"
        );
        assert!(PlatformWindow::is_active(&owner));
        assert!(!PlatformWindow::is_active(&tip));

        // Control: the same check can fail. An ordinary window shown the same way does take the
        // foreground, so the assertions above are not vacuous.
        let ordinary = NativeWindow::new("ordinary", rect(), WindowFlags::FRAMELESS).unwrap();
        PlatformWindow::show(&ordinary);
        assert_eq!(
            foreground(),
            ordinary.hwnd() as isize,
            "control: show() activates"
        );
        assert!(PlatformWindow::is_active(&ordinary));
        assert!(!PlatformWindow::is_active(&owner));
    }

    #[test]
    fn is_active_is_the_foreground_window() {
        let a = NativeWindow::new("a", rect(), WindowFlags::FRAMELESS).unwrap();
        let b = NativeWindow::new("b", rect(), WindowFlags::FRAMELESS).unwrap();
        a.show();
        b.show();
        if !take_foreground(&a) {
            eprintln!("{SKIP}");
            return;
        }
        assert!(PlatformWindow::is_active(&a));
        assert!(!PlatformWindow::is_active(&b));

        assert!(take_foreground(&b));
        assert!(!PlatformWindow::is_active(&a));
        assert!(PlatformWindow::is_active(&b));
    }

    #[test]
    fn a_hidden_window_is_not_active() {
        let w = NativeWindow::new("w", rect(), WindowFlags::FRAMELESS).unwrap();
        assert!(!PlatformWindow::is_active(&w), "never shown");
        w.show();
        let _ = take_foreground(&w);
        w.hide();
        assert!(!PlatformWindow::is_active(&w));
    }

    #[test]
    fn request_activate_window_makes_a_visible_window_the_foreground_window() {
        let a = NativeWindow::new("a", rect(), WindowFlags::FRAMELESS).unwrap();
        let b = NativeWindow::new("b", rect(), WindowFlags::FRAMELESS).unwrap();
        a.show();
        b.show();
        if !take_foreground(&a) {
            eprintln!("{SKIP}");
            return;
        }
        assert!(!PlatformWindow::is_active(&b));

        // This process holds the foreground, so the system lets it hand the foreground on.
        PlatformWindow::request_activate_window(&b);
        assert_eq!(foreground(), b.hwnd() as isize);
        assert!(PlatformWindow::is_active(&b));
        assert!(!PlatformWindow::is_active(&a));
    }

    #[test]
    fn request_activate_window_does_nothing_for_a_hidden_window() {
        let owner = NativeWindow::new("owner", rect(), WindowFlags::FRAMELESS).unwrap();
        owner.show();
        if !take_foreground(&owner) {
            eprintln!("{SKIP}");
            return;
        }
        let hidden = NativeWindow::new("hidden", rect(), WindowFlags::FRAMELESS).unwrap();
        PlatformWindow::request_activate_window(&hidden);
        assert_eq!(foreground(), owner.hwnd() as isize);
        assert!(!PlatformWindow::is_active(&hidden));
    }

    #[test]
    fn request_activate_window_does_not_activate_a_tooltip_window() {
        let owner = NativeWindow::new("owner", rect(), WindowFlags::FRAMELESS).unwrap();
        owner.show();
        if !take_foreground(&owner) {
            eprintln!("{SKIP}");
            return;
        }
        let tip = NativeWindow::new("tip", rect(), WindowFlags::TOOLTIP).unwrap();
        PlatformWindow::show(&tip);
        PlatformWindow::request_activate_window(&tip);
        assert_eq!(foreground(), owner.hwnd() as isize);
        assert!(!PlatformWindow::is_active(&tip));
    }

    #[test]
    fn raise_puts_the_window_above_another_without_activating_it() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{GW_HWNDNEXT, GetWindow};
        // Whether `upper` is above `lower` in the system z-order.
        let is_above = |upper: &NativeWindow, lower: &NativeWindow| {
            // SAFETY: `upper.hwnd()` is the live handle of the borrowed window; `GetWindow` only
            // reads the z-order and validates the handle.
            let mut cur = unsafe { GetWindow(upper.hwnd(), GW_HWNDNEXT) };
            while !cur.is_null() {
                if cur == lower.hwnd() {
                    return true;
                }
                // SAFETY: `cur` is non-null (loop condition) and came from `GetWindow`; if that
                // window was destroyed meanwhile, user32 rejects the stale handle.
                cur = unsafe { GetWindow(cur, GW_HWNDNEXT) };
            }
            false
        };
        let a = NativeWindow::new("a", rect(), WindowFlags::FRAMELESS).unwrap();
        let b = NativeWindow::new("b", rect(), WindowFlags::FRAMELESS).unwrap();
        a.show();
        b.show();
        assert!(is_above(&b, &a), "control: the window shown last is on top");
        let before = foreground();

        PlatformWindow::raise(&a);
        assert!(is_above(&a, &b));
        assert_eq!(foreground(), before, "raise does not activate");

        PlatformWindow::raise(&b);
        assert!(is_above(&b, &a));
    }
}

// ---- Simulated backends: activation comes from the focus events the window system delivers ----

fn noop_handler() -> Box<dyn ferrokit_platform::WindowSystemEventHandler> {
    Box::new(ClosureWindowEventHandler::new(|_| {}))
}

#[test]
fn generic_window_is_active_between_focus_in_and_focus_out() {
    let mut w = GenericWindow::new("g", rect(), WindowFlags::NORMAL);
    w.set_event_handler(noop_handler());
    w.show();
    assert!(!w.is_active(), "showing does not activate by itself");

    w.queue_event(WindowSystemEvent::FocusIn);
    w.poll_events();
    assert!(w.is_active());

    w.queue_event(WindowSystemEvent::FocusOut);
    w.poll_events();
    assert!(!w.is_active());

    w.queue_event(WindowSystemEvent::FocusIn);
    w.poll_events();
    w.hide();
    assert!(!w.is_active(), "a hidden window is never active");
}

#[test]
fn x11_window_is_active_between_focus_in_and_focus_out() {
    let mut w = X11NativeWindow::new_offscreen("x", rect(), WindowFlags::NORMAL).unwrap();
    w.show();
    assert!(!w.is_active());

    // Activation is window-system state: it must not depend on a handler being installed.
    assert!(!w.dispatch_x11_event(X11Event::FocusIn));
    assert!(w.is_active());
    w.set_event_handler(noop_handler());
    assert!(w.dispatch_x11_event(X11Event::FocusOut));
    assert!(!w.is_active());

    w.dispatch_x11_event(X11Event::FocusIn);
    w.hide();
    assert!(!w.is_active());
}

#[test]
fn wayland_window_is_active_between_keyboard_enter_and_leave() {
    let mut w = WaylandNativeWindow::new_offscreen("w", rect(), WindowFlags::NORMAL).unwrap();
    w.show();
    assert!(!w.is_active());

    w.dispatch_wayland_event(WaylandEvent::KeyboardEnter);
    assert!(w.is_active());
    w.dispatch_wayland_event(WaylandEvent::KeyboardLeave);
    assert!(!w.is_active());

    w.dispatch_wayland_event(WaylandEvent::KeyboardEnter);
    w.hide();
    assert!(!w.is_active());
}

#[test]
fn simulated_focus_events_reach_the_handler_as_focus_in_and_out() {
    use std::sync::{Arc, Mutex};
    let log = Arc::new(Mutex::new(Vec::<&'static str>::new()));
    let sink = Arc::clone(&log);
    let mut w = WaylandNativeWindow::new_offscreen("w", rect(), WindowFlags::NORMAL).unwrap();
    w.set_event_handler(Box::new(ClosureWindowEventHandler::new(move |e| match e {
        WindowSystemEvent::FocusIn => sink.lock().unwrap().push("in"),
        WindowSystemEvent::FocusOut => sink.lock().unwrap().push("out"),
        _ => {}
    })));
    w.dispatch_wayland_event(WaylandEvent::KeyboardEnter);
    w.dispatch_wayland_event(WaylandEvent::KeyboardLeave);
    assert_eq!(*log.lock().unwrap(), vec!["in", "out"]);
}

// Mock-runtime test. On macOS the real NSWindow key status and level are checked on the main
// thread: examples/appkit_main_thread.rs (`window_key_status`, `window_level`).
#[cfg(not(target_os = "macos"))]
#[test]
fn cocoa_window_is_active_only_when_it_is_the_key_window() {
    use ferrokit_core::object::ThreadContext;
    use ferrokit_platform::{CocoaNativeWindow, MockObjcRuntime};
    ThreadContext::init_current(true, None);

    let normal = CocoaNativeWindow::new("n", rect(), WindowFlags::NORMAL).unwrap();
    assert!(!normal.is_active(), "not shown yet");
    normal.show();
    assert!(
        normal.is_active(),
        "makeKeyAndOrderFront: makes it the key window"
    );
    normal.hide();
    assert!(!normal.is_active(), "orderOut: resigns key");

    // A tooltip is ordered front but never becomes key.
    let tip = CocoaNativeWindow::new("t", rect(), WindowFlags::TOOLTIP).unwrap();
    tip.show();
    let data = MockObjcRuntime::instance()
        .get_object_data(tip.ns_window())
        .unwrap();
    assert!(data.is_visible, "the tip is on screen");
    assert!(
        !tip.is_active(),
        "showing a tooltip must not take key status"
    );
    assert_eq!(
        data.level,
        ferrokit_platform::NS_SCREEN_SAVER_WINDOW_LEVEL,
        "tooltips go above stays-on-top windows"
    );
}

#[test]
fn a_tooltip_window_is_not_activated_by_being_shown_on_any_backend() {
    let mut g = GenericWindow::new("g", rect(), WindowFlags::TOOLTIP);
    g.set_event_handler(noop_handler());
    g.show();
    g.poll_events();
    assert!(!g.is_active());

    let x = X11NativeWindow::new_offscreen("x", rect(), WindowFlags::TOOLTIP).unwrap();
    x.show();
    assert!(!x.is_active());

    let w = WaylandNativeWindow::new_offscreen("w", rect(), WindowFlags::TOOLTIP).unwrap();
    w.show();
    assert!(!w.is_active());
}

#[test]
fn a_backend_without_a_window_system_ignores_raise_and_activation_requests() {
    let mut g = GenericWindow::new("g", rect(), WindowFlags::NORMAL);
    g.set_event_handler(noop_handler());
    g.show();
    g.raise();
    g.request_activate_window();
    g.poll_events();
    assert!(g.is_visible());
    assert!(
        !g.is_active(),
        "activation arrives as a focus event, never from the request itself"
    );
}
