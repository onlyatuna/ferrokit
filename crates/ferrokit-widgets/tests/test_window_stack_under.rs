// SPDX-License-Identifier: LGPL-3.0-only
//! `Window::stack_under`: a top-level window placed directly below another one.
//!
//! Real Win32 windows, checked against the system z-order (a walk of `GetWindow(GW_HWNDNEXT)`
//! from the top), the foreground window, the window rectangles and `WS_EX_TOPMOST`. The topmost
//! band depends on the desktop session applying `HWND_TOPMOST` (the GitHub runner does not); a
//! scenario that finds it not applied says so and returns.
#![cfg(windows)]

use ferrokit_gui::geometry::primitives::Rect;
use ferrokit_platform::WindowFlags;
use ferrokit_widgets::window::Window;
use windows_sys::Win32::Foundation::{HWND, RECT};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GW_HWNDNEXT, GW_HWNDPREV, GW_OWNER, GWL_EXSTYLE, GetForegroundWindow, GetTopWindow, GetWindow,
    GetWindowLongPtrW, GetWindowRect, WS_EX_TOPMOST,
};

fn window(title: &str, x: i32) -> Window {
    Window::new(title, Rect::new(x, 100, 300, 200), WindowFlags::NORMAL).expect("window")
}

fn hwnd(win: &Window) -> HWND {
    win.native_handle() as HWND
}

fn foreground() -> HWND {
    // SAFETY: `GetForegroundWindow` takes no arguments, has no preconditions and returns a handle
    // value (possibly null) that is only compared, never dereferenced.
    unsafe { GetForegroundWindow() }
}

/// The names of `windows` from the top of the z-order down, whatever other windows lie between.
fn z_order(windows: &[(&'static str, HWND)]) -> Vec<&'static str> {
    let mut order = Vec::new();
    // SAFETY: `GetTopWindow`/`GetWindow` take a window handle (null: the desktop) and an integer
    // relation and no pointer; every handle comes from the system, and one destroyed by another
    // process in the meantime only makes the next call return null.
    let mut cur = unsafe { GetTopWindow(std::ptr::null_mut()) };
    while !cur.is_null() {
        if let Some((name, _)) = windows.iter().find(|(_, h)| *h == cur) {
            order.push(*name);
        }
        // SAFETY: as above.
        cur = unsafe { GetWindow(cur, GW_HWNDNEXT) };
    }
    order
}

/// Whether `owner` owns `window`, directly or through other owned windows.
fn owns(owner: HWND, window: HWND) -> bool {
    // SAFETY: `GetWindow` takes a window handle and an integer relation and no pointer; a stale
    // handle only makes it return null.
    let mut cur = unsafe { GetWindow(window, GW_OWNER) };
    for _ in 0..16 {
        if cur.is_null() {
            return false;
        }
        if cur == owner {
            return true;
        }
        // SAFETY: as above.
        cur = unsafe { GetWindow(cur, GW_OWNER) };
    }
    false
}

/// Whether nothing but windows owned by `lower` (which the system keeps right above it, such as
/// the default IME window it creates for it) lies between `upper` and `lower` in the z-order,
/// with `upper` above.
fn directly_below(lower: HWND, upper: HWND) -> bool {
    // SAFETY: `GetWindow` takes a window handle and an integer relation and no pointer; a stale
    // handle only makes it return null.
    let mut cur = unsafe { GetWindow(lower, GW_HWNDPREV) };
    while !cur.is_null() && cur != upper && owns(lower, cur) {
        // SAFETY: as above.
        cur = unsafe { GetWindow(cur, GW_HWNDPREV) };
    }
    cur == upper
}

fn rect(hwnd: HWND) -> (i32, i32, i32, i32) {
    let mut r = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    // SAFETY: `r` is a live, writable `RECT` for the duration of the call; `hwnd` is a window that
    // is alive in the test.
    assert_ne!(unsafe { GetWindowRect(hwnd, &mut r) }, 0);
    (r.left, r.top, r.right, r.bottom)
}

fn is_topmost(hwnd: HWND) -> bool {
    // SAFETY: `GetWindowLongPtrW` takes a window handle and an index and no pointer; `hwnd` is a
    // window that is alive in the test.
    unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST != 0 }
}

struct Stack {
    a: Window,
    b: Window,
    c: Window,
}

impl Stack {
    /// Shows `a`, `b`, `c` in that order, so `c` is above `b` above `a`.
    fn shown() -> Self {
        let (mut a, mut b, mut c) = (window("a", 100), window("b", 200), window("c", 300));
        a.show();
        b.show();
        c.show();
        let stack = Self { a, b, c };
        assert_eq!(
            stack.order(),
            ["c", "b", "a"],
            "control: shown last is on top"
        );
        stack
    }

    fn handles(&self) -> [(&'static str, HWND); 3] {
        [
            ("a", hwnd(&self.a)),
            ("b", hwnd(&self.b)),
            ("c", hwnd(&self.c)),
        ]
    }

    fn order(&self) -> Vec<&'static str> {
        z_order(&self.handles())
    }

    fn rects(&self) -> [(i32, i32, i32, i32); 3] {
        self.handles().map(|(_, h)| rect(h))
    }
}

fn stack_under_puts_the_window_directly_below_the_sibling_without_activating_or_moving_it() {
    let s = Stack::shown();
    let (ha, hc) = (hwnd(&s.a), hwnd(&s.c));
    let foreground_before = foreground();
    let rects_before = s.rects();

    // Up: `a` was at the bottom; it ends directly below `c`, between `c` and `b`.
    assert!(s.a.stack_under(&s.c));
    assert_eq!(s.order(), ["c", "a", "b"]);
    assert!(directly_below(ha, hc), "nothing else lies between c and a");

    // Down: `c` was on top; it ends directly below `a`, and so below `b` too.
    assert!(s.c.stack_under(&s.a));
    assert_eq!(s.order(), ["a", "c", "b"]);
    assert!(directly_below(hc, ha), "nothing else lies between a and c");

    assert_eq!(foreground(), foreground_before, "no window was activated");
    assert_eq!(s.rects(), rects_before, "no window was moved or resized");
}

fn stack_under_refuses_the_window_itself() {
    let s = Stack::shown();
    assert!(!s.a.stack_under(&s.a));
    assert_eq!(s.order(), ["c", "b", "a"]);
}

fn stack_under_leaves_a_window_that_is_already_directly_below_in_place() {
    let s = Stack::shown();
    assert!(s.a.stack_under(&s.b));
    assert_eq!(s.order(), ["c", "b", "a"]);
    assert!(directly_below(hwnd(&s.a), hwnd(&s.b)));
}

fn stack_under_cannot_put_an_owned_window_below_its_owner() {
    let mut owner = window("owner", 100);
    owner.show();
    let mut owned = Window::with_parent(
        &owner,
        "owned",
        Rect::new(150, 150, 300, 200),
        WindowFlags::NORMAL,
    )
    .expect("owned window");
    owned.show();
    let windows = [("owner", hwnd(&owner)), ("owned", hwnd(&owned))];
    assert!(!owned.stack_under(&owner), "the system keeps it above");
    assert_eq!(z_order(&windows), ["owned", "owner"]);
}

const NO_TOPMOST: &str = "SKIPPED: the desktop session did not apply HWND_TOPMOST";

fn stack_under_orders_two_topmost_windows() {
    let mut s = Stack::shown();
    s.a.set_stays_on_top(true);
    s.b.set_stays_on_top(true);
    s.c.set_stays_on_top(true);
    let handles = s.handles();
    if !handles.iter().all(|(_, h)| is_topmost(*h)) {
        eprintln!("{NO_TOPMOST}");
        return;
    }
    let (ha, hb, hc) = (handles[0].1, handles[1].1, handles[2].1);
    let foreground_before = foreground();
    let rects_before = s.rects();

    // Make the order among the topmost windows known: c above b above a.
    assert!(s.b.stack_under(&s.c));
    assert!(s.a.stack_under(&s.b));
    assert_eq!(s.order(), ["c", "b", "a"]);

    assert!(s.a.stack_under(&s.c), "a moves up to directly below c");
    assert_eq!(s.order(), ["c", "a", "b"]);
    assert!(directly_below(ha, hc));
    assert!(s.c.stack_under(&s.b), "c moves down to directly below b");
    assert_eq!(s.order(), ["a", "b", "c"]);
    assert!(directly_below(hc, hb));

    for (_, h) in handles {
        assert!(is_topmost(h), "stack_under keeps the topmost band");
    }
    assert_eq!(foreground(), foreground_before, "no window was activated");
    assert_eq!(s.rects(), rects_before, "no window was moved or resized");
}

fn stack_under_refuses_a_window_in_the_other_band_and_changes_nothing() {
    let mut s = Stack::shown();
    s.a.set_stays_on_top(true);
    let handles = s.handles();
    if !is_topmost(handles[0].1) {
        eprintln!("{NO_TOPMOST}");
        return;
    }
    let order_before = s.order();

    assert!(!s.b.stack_under(&s.a), "normal window below a topmost one");
    assert!(!s.a.stack_under(&s.b), "topmost window below a normal one");

    assert_eq!(s.order(), order_before);
    assert!(is_topmost(handles[0].1), "the topmost window stays topmost");
    assert!(!is_topmost(handles[1].1), "the normal window stays normal");
    assert!(!is_topmost(handles[2].1), "the normal window stays normal");
}

/// One test function runs every scenario in turn. The z-order is global to the desktop, so
/// scenarios that test adjacency must not run on parallel test threads, whose windows would be
/// stacked in between.
#[test]
fn stack_under_orders_top_level_windows() {
    stack_under_puts_the_window_directly_below_the_sibling_without_activating_or_moving_it();
    stack_under_refuses_the_window_itself();
    stack_under_leaves_a_window_that_is_already_directly_below_in_place();
    stack_under_cannot_put_an_owned_window_below_its_owner();
    stack_under_orders_two_topmost_windows();
    stack_under_refuses_a_window_in_the_other_band_and_changes_nothing();
}
