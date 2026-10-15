// SPDX-License-Identifier: LGPL-3.0-only
//! Native size limits of a real Win32 window: what `WM_GETMINMAXINFO` reports for the logical
//! limits a `NativeWindow` carries, and what Windows does with them (`QWindowsGeometryHint`,
//! qwindowswindow.cpp:1216-1262).
//!
//! These tests use real `HWND`s and the real window procedure (`SendMessageW`, `SetWindowPos`).
//! The scale factor is the monitor's: a window created under a per-monitor-v2 thread context gets
//! the DPI of the monitor it is on, a window created under the unaware context always gets 96.
//! `TestWindow` runs every call under the awareness the window was created with, because the
//! system virtualizes the sizes an unaware thread passes to or reads from an aware window.
//!
//! On a 100% monitor (a CI runner) both ratios are 1.0 and the scaling is not exercised by the real
//! windows (`scaling_exercised` says so; `test_size_limits.rs` covers the arithmetic for every
//! ratio). This is not a test of a mouse drag, of a window that moves between monitors, or of a
//! monitor with a ratio this machine does not have.
#![cfg(windows)]

use ferrokit_gui::geometry::primitives::Rect;
use ferrokit_platform::size_limits::WINDOW_SIZE_MAX;
use ferrokit_platform::window::get_window_dpr;
use ferrokit_platform::{NativeWindow, WindowFlags};
use windows_sys::Win32::Foundation::{POINT, RECT};
use windows_sys::Win32::UI::HiDpi::{
    SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT,
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, DPI_AWARENESS_CONTEXT_UNAWARE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetClientRect, GetWindowRect, SendMessageW, SetWindowPos, HTBOTTOM, HTBOTTOMLEFT,
    HTBOTTOMRIGHT, HTCAPTION, HTCLIENT, HTLEFT, HTRIGHT, HTTOP, HTTOPLEFT, HTTOPRIGHT, MINMAXINFO,
    SWP_NOACTIVATE, SWP_NOZORDER, WM_GETMINMAXINFO, WM_NCHITTEST,
};

const AWARE: DPI_AWARENESS_CONTEXT = DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2;
const UNAWARE: DPI_AWARENESS_CONTEXT = DPI_AWARENESS_CONTEXT_UNAWARE;

/// Switches the calling thread's DPI awareness while it lives.
struct ThreadDpi(DPI_AWARENESS_CONTEXT);

impl ThreadDpi {
    fn enter(context: DPI_AWARENESS_CONTEXT) -> Self {
        // SAFETY: plain value argument; the previous context is restored on drop.
        Self(unsafe { SetThreadDpiAwarenessContext(context) })
    }
}

impl Drop for ThreadDpi {
    fn drop(&mut self) {
        // SAFETY: restores the context returned by `SetThreadDpiAwarenessContext`.
        unsafe { SetThreadDpiAwarenessContext(self.0) };
    }
}

// Values standing in for what Windows fills into `MINMAXINFO` before it asks the window. A field
// the window does not set must come back unchanged.
const SYSTEM_MIN: (i32, i32) = (1111, 2222);
const SYSTEM_MAX_TRACK: (i32, i32) = (7777, 8888);
const SYSTEM_MAX_SIZE: (i32, i32) = (3333, 4444);

struct TestWindow {
    window: NativeWindow,
    context: DPI_AWARENESS_CONTEXT,
}

impl TestWindow {
    fn new(context: DPI_AWARENESS_CONTEXT, flags: WindowFlags) -> Self {
        let _dpi = ThreadDpi::enter(context);
        let window = NativeWindow::new("size limits", Rect::new(100, 100, 400, 300), flags)
            .expect("native window");
        Self { window, context }
    }

    /// A window with a sizing frame style and no non-client area: `CUSTOM_FRAMELESS` without
    /// `LAYERED`. Windows applies the tracking limits to a programmatic resize of such a window;
    /// it does not for a `WS_POPUP` window (measured in this session).
    fn sizing(context: DPI_AWARENESS_CONTEXT) -> Self {
        Self::new(context, WindowFlags::CUSTOM_FRAMELESS)
    }

    /// An overlay window style: a layered popup.
    fn popup(context: DPI_AWARENESS_CONTEXT) -> Self {
        Self::new(
            context,
            WindowFlags::FRAMELESS
                | WindowFlags::CUSTOM_FRAMELESS
                | WindowFlags::LAYERED
                | WindowFlags::TOOL,
        )
    }

    fn dpr(&self) -> f32 {
        get_window_dpr(self.window.hwnd())
    }

    fn set_minimum_size(&mut self, w: i32, h: i32) {
        let _dpi = ThreadDpi::enter(self.context);
        self.window.set_minimum_size(w, h);
    }

    fn set_maximum_size(&mut self, w: i32, h: i32) {
        let _dpi = ThreadDpi::enter(self.context);
        self.window.set_maximum_size(w, h);
    }

    /// `WM_GETMINMAXINFO` through the window's real procedure.
    fn query(&self) -> MINMAXINFO {
        let _dpi = ThreadDpi::enter(self.context);
        // SAFETY: an all-zero `MINMAXINFO` is valid; the pointer outlives the synchronous send.
        let mut mmi: MINMAXINFO = unsafe { std::mem::zeroed() };
        mmi.ptMinTrackSize = POINT {
            x: SYSTEM_MIN.0,
            y: SYSTEM_MIN.1,
        };
        mmi.ptMaxTrackSize = POINT {
            x: SYSTEM_MAX_TRACK.0,
            y: SYSTEM_MAX_TRACK.1,
        };
        mmi.ptMaxSize = POINT {
            x: SYSTEM_MAX_SIZE.0,
            y: SYSTEM_MAX_SIZE.1,
        };
        unsafe {
            SendMessageW(
                self.window.hwnd(),
                WM_GETMINMAXINFO,
                0,
                &mut mmi as *mut _ as isize,
            )
        };
        mmi
    }

    fn window_size(&self) -> (i32, i32) {
        let _dpi = ThreadDpi::enter(self.context);
        let mut r: RECT = unsafe { std::mem::zeroed() };
        unsafe { GetWindowRect(self.window.hwnd(), &mut r) };
        (r.right - r.left, r.bottom - r.top)
    }

    fn client_size(&self) -> (i32, i32) {
        let _dpi = ThreadDpi::enter(self.context);
        let mut r: RECT = unsafe { std::mem::zeroed() };
        unsafe { GetClientRect(self.window.hwnd(), &mut r) };
        (r.right - r.left, r.bottom - r.top)
    }

    /// `SetWindowPos` with a native size: what an interactive or programmatic resize asks Windows.
    fn resize_natively(&self, w: i32, h: i32) {
        let _dpi = ThreadDpi::enter(self.context);
        // SAFETY: a live window of this thread.
        unsafe {
            SetWindowPos(
                self.window.hwnd(),
                std::ptr::null_mut(),
                100,
                100,
                w,
                h,
                SWP_NOZORDER | SWP_NOACTIVATE,
            )
        };
    }
}

fn min_track(mmi: &MINMAXINFO) -> (i32, i32) {
    (mmi.ptMinTrackSize.x, mmi.ptMinTrackSize.y)
}

/// `qRound(dip * factor)` written out for a test value: independent of the crate's helper.
fn expected(dip: i32, dpr: f32) -> i32 {
    (f64::from(dip) * f64::from(dpr) + 0.5).floor() as i32
}

fn scaling_exercised(label: &str, dpr: f32) {
    if dpr == 1.0 {
        eprintln!(
            "NOTE {label}: this window is at ratio 1.0, so the scaling is not exercised by it"
        );
    }
}

#[test]
fn the_minimum_is_scaled_by_each_windows_own_dpi() {
    let mut aware = TestWindow::popup(AWARE);
    let mut unaware = TestWindow::popup(UNAWARE);
    let (dpr_aware, dpr_unaware) = (aware.dpr(), unaware.dpr());
    assert_eq!(dpr_unaware, 1.0, "an unaware window is always at 96 dpi");
    scaling_exercised("per-monitor-v2 window", dpr_aware);

    aware.set_minimum_size(300, 200);
    unaware.set_minimum_size(300, 200);

    assert_eq!(
        min_track(&aware.query()),
        (expected(300, dpr_aware), expected(200, dpr_aware)),
        "per-monitor-v2 window at ratio {dpr_aware}"
    );
    assert_eq!(
        min_track(&unaware.query()),
        (300, 200),
        "unaware window at ratio 1.0"
    );
}

#[test]
fn windows_enforces_the_scaled_minimum_on_a_programmatic_resize() {
    let mut w = TestWindow::sizing(AWARE);
    let dpr = w.dpr();
    scaling_exercised("sizing window", dpr);
    w.set_minimum_size(300, 200);

    w.resize_natively(50, 50);

    assert_eq!(
        w.window_size(),
        (expected(300, dpr), expected(200, dpr)),
        "ratio {dpr}"
    );
}

#[test]
fn an_axis_without_a_minimum_keeps_the_system_value() {
    let mut w = TestWindow::popup(AWARE);
    let dpr = w.dpr();
    w.set_minimum_size(300, 0);
    assert_eq!(min_track(&w.query()), (expected(300, dpr), SYSTEM_MIN.1));

    w.set_minimum_size(0, 200);
    assert_eq!(min_track(&w.query()), (SYSTEM_MIN.0, expected(200, dpr)));
}

#[test]
fn removing_the_minimum_restores_the_system_behaviour() {
    // Windows' own minimum of a captioned window (about 136 px wide at 96 dpi) stays: measure it
    // on a window that never had a limit.
    let reference = TestWindow::sizing(AWARE);
    reference.resize_natively(1, 1);
    let system_minimum = reference.window_size();

    let mut w = TestWindow::sizing(AWARE);
    w.set_minimum_size(300, 200);
    w.set_minimum_size(0, 0);

    assert_eq!(min_track(&w.query()), SYSTEM_MIN, "MINMAXINFO untouched");
    w.resize_natively(1, 1);
    assert_eq!(
        w.window_size(),
        system_minimum,
        "only the system's own minimum applies"
    );
}

#[test]
fn a_changed_minimum_is_seen_by_the_next_query() {
    let mut w = TestWindow::popup(AWARE);
    let dpr = w.dpr();
    w.set_minimum_size(300, 200);
    assert_eq!(
        min_track(&w.query()),
        (expected(300, dpr), expected(200, dpr))
    );

    w.set_minimum_size(500, 120);
    assert_eq!(
        min_track(&w.query()),
        (expected(500, dpr), expected(120, dpr))
    );
}

/// The limits are in window-rectangle units and carry no frame margins, which holds because the
/// window's client area is its whole rectangle once the window has been sized (`WM_NCCALCSIZE`
/// keeps the proposed rectangle). If this ever fails, `WM_GETMINMAXINFO` has to add the frame like
/// `QWindowsGeometryHint::frameSizeConstraints` (qwindowswindow.cpp:1225-1235).
#[test]
fn a_sized_window_has_no_non_client_area() {
    for context in [AWARE, UNAWARE] {
        for flags in [
            WindowFlags::NORMAL,
            WindowFlags::FRAMELESS,
            WindowFlags::CUSTOM_FRAMELESS,
            WindowFlags::FRAMELESS
                | WindowFlags::CUSTOM_FRAMELESS
                | WindowFlags::LAYERED
                | WindowFlags::TOOL,
        ] {
            let mut w = TestWindow::new(context, flags);
            {
                let _dpi = ThreadDpi::enter(context);
                w.window.set_geometry(Rect::new(100, 100, 500, 400));
            }
            assert_eq!(w.window_size(), (500, 400), "{flags:?}");
            assert_eq!(
                w.client_size(),
                (500, 400),
                "{flags:?}: client area equals the window"
            );
        }
    }
}

// ---- Maximum -------------------------------------------------------------------------------

fn max_track(mmi: &MINMAXINFO) -> (i32, i32) {
    (mmi.ptMaxTrackSize.x, mmi.ptMaxTrackSize.y)
}

fn max_size(mmi: &MINMAXINFO) -> (i32, i32) {
    (mmi.ptMaxSize.x, mmi.ptMaxSize.y)
}

#[test]
fn the_maximum_is_scaled_by_each_windows_own_dpi() {
    let mut aware = TestWindow::popup(AWARE);
    let mut unaware = TestWindow::popup(UNAWARE);
    let dpr_aware = aware.dpr();
    scaling_exercised("per-monitor-v2 window", dpr_aware);

    aware.set_maximum_size(800, 600);
    unaware.set_maximum_size(800, 600);

    let q = aware.query();
    assert_eq!(
        max_track(&q),
        (expected(800, dpr_aware), expected(600, dpr_aware)),
        "per-monitor-v2 window at ratio {dpr_aware}"
    );
    assert_eq!(
        min_track(&q),
        SYSTEM_MIN,
        "a maximum alone leaves the minimum fields"
    );
    assert_eq!(
        max_track(&unaware.query()),
        (800, 600),
        "unaware window at ratio 1.0"
    );
}

#[test]
fn windows_enforces_the_scaled_maximum_on_a_programmatic_resize() {
    let mut w = TestWindow::sizing(AWARE);
    let dpr = w.dpr();
    scaling_exercised("sizing window", dpr);
    w.set_maximum_size(500, 400);

    w.resize_natively(5000, 4000);

    assert_eq!(
        w.window_size(),
        (expected(500, dpr), expected(400, dpr)),
        "ratio {dpr}"
    );
}

#[test]
fn a_window_without_a_maximum_leaves_every_maximum_field_of_the_system() {
    // The rule that keeps Windows' own defaults: only what is set is written.
    let mut w = TestWindow::popup(AWARE);
    w.set_minimum_size(300, 200);

    let q = w.query();
    assert_eq!(max_track(&q), SYSTEM_MAX_TRACK);
    assert_eq!(
        max_size(&q),
        SYSTEM_MAX_SIZE,
        "the maximized size is never touched"
    );
}

#[test]
fn a_maximum_on_one_axis_leaves_the_other_axis_and_the_maximized_size_alone() {
    let mut w = TestWindow::popup(AWARE);
    let dpr = w.dpr();

    w.set_maximum_size(500, WINDOW_SIZE_MAX);
    let q = w.query();
    assert_eq!(max_track(&q), (expected(500, dpr), SYSTEM_MAX_TRACK.1));
    assert_eq!(max_size(&q), SYSTEM_MAX_SIZE);

    w.set_maximum_size(WINDOW_SIZE_MAX, 400);
    assert_eq!(
        max_track(&w.query()),
        (SYSTEM_MAX_TRACK.0, expected(400, dpr))
    );
}

#[test]
fn removing_the_maximum_restores_the_system_behaviour() {
    let reference = TestWindow::sizing(AWARE);
    reference.resize_natively(5000, 4000);
    let system_maximum = reference.window_size();

    let mut w = TestWindow::sizing(AWARE);
    w.set_maximum_size(500, 400);
    w.set_maximum_size(WINDOW_SIZE_MAX, WINDOW_SIZE_MAX);

    assert_eq!(
        max_track(&w.query()),
        SYSTEM_MAX_TRACK,
        "MINMAXINFO untouched"
    );
    w.resize_natively(5000, 4000);
    assert_eq!(
        w.window_size(),
        system_maximum,
        "only the system's own maximum applies"
    );
}

#[test]
fn the_minimum_and_the_maximum_are_kept_independently() {
    let mut w = TestWindow::popup(AWARE);
    let dpr = w.dpr();
    w.set_minimum_size(300, 200);
    w.set_maximum_size(800, 600);
    let q = w.query();
    assert_eq!(min_track(&q), (expected(300, dpr), expected(200, dpr)));
    assert_eq!(max_track(&q), (expected(800, dpr), expected(600, dpr)));

    w.set_minimum_size(0, 0);
    let q = w.query();
    assert_eq!(min_track(&q), SYSTEM_MIN, "the minimum is gone");
    assert_eq!(
        max_track(&q),
        (expected(800, dpr), expected(600, dpr)),
        "the maximum stays"
    );

    w.set_maximum_size(500, 400);
    assert_eq!(
        max_track(&w.query()),
        (expected(500, dpr), expected(400, dpr)),
        "a changed maximum"
    );
}

#[test]
fn a_maximum_below_the_minimum_is_raised_to_it() {
    let mut w = TestWindow::popup(AWARE);
    let dpr = w.dpr();
    w.set_minimum_size(300, 200);
    w.set_maximum_size(200, 100);

    let q = w.query();
    assert_eq!(
        max_track(&q),
        min_track(&q),
        "native maximum is not below the native minimum"
    );
    assert_eq!(max_track(&q), (expected(300, dpr), expected(200, dpr)));
}

#[test]
fn a_fixed_size_window_cannot_be_resized_either_way() {
    let mut w = TestWindow::sizing(AWARE);
    let dpr = w.dpr();
    w.set_minimum_size(400, 300);
    w.set_maximum_size(400, 300);
    let fixed = (expected(400, dpr), expected(300, dpr));

    let q = w.query();
    assert_eq!(min_track(&q), fixed);
    assert_eq!(max_track(&q), fixed);
    w.resize_natively(50, 50);
    assert_eq!(w.window_size(), fixed, "grown to the fixed size");
    w.resize_natively(5000, 4000);
    assert_eq!(w.window_size(), fixed, "shrunk to the fixed size");
}

// ---- Fixed size: the hit test ----------------------------------------------------------------
//
// The tracking limits only clamp a resize; the resize cursor comes from `WM_NCHITTEST`. A window
// with a custom frameless config (`set_custom_frameless_config`) answers that message with
// resize codes in a border zone inside the window rectangle. These tests send the message to the
// real window procedure with the cursor position in screen pixels; they do not move the mouse.

const CAPTION: i32 = 40;
const BORDER: i32 = 10;

impl TestWindow {
    fn with_frameless_config() -> Self {
        let mut w = Self::sizing(AWARE);
        w.window.set_custom_frameless_config(CAPTION, BORDER);
        w
    }

    /// The hit-test code at `(dx, dy)` from the window's top-left; negative values count from the
    /// bottom-right (`-1` is the last pixel).
    fn hit(&self, dx: i32, dy: i32) -> u32 {
        let _dpi = ThreadDpi::enter(self.context);
        let mut r: RECT = unsafe { std::mem::zeroed() };
        unsafe { GetWindowRect(self.window.hwnd(), &mut r) };
        let x = if dx >= 0 {
            r.left + dx
        } else {
            r.right + dx + 1
        };
        let y = if dy >= 0 {
            r.top + dy
        } else {
            r.bottom + dy + 1
        };
        let lparam = ((y as u16 as isize) << 16) | (x as u16 as isize);
        unsafe { SendMessageW(self.window.hwnd(), WM_NCHITTEST, 0, lparam) as u32 }
    }
}

/// The nine places of interest: each corner, each edge, the caption and the client area.
fn zones(w: &TestWindow) -> [(&'static str, u32); 9] {
    [
        ("top-left", w.hit(2, 2)),
        ("top", w.hit(200, 2)),
        ("top-right", w.hit(-3, 2)),
        ("left", w.hit(2, 200)),
        ("right", w.hit(-3, 200)),
        ("bottom-left", w.hit(2, -3)),
        ("bottom", w.hit(200, -3)),
        ("bottom-right", w.hit(-3, -3)),
        ("client", w.hit(200, 200)),
    ]
}

fn codes(w: &TestWindow) -> Vec<u32> {
    zones(w).iter().map(|&(_, code)| code).collect()
}

#[test]
fn an_unlimited_window_resizes_from_every_border() {
    // Pin: what the hit test answered before fixed sizes were considered.
    let w = TestWindow::with_frameless_config();
    assert_eq!(
        codes(&w),
        [
            HTTOPLEFT,
            HTTOP,
            HTTOPRIGHT,
            HTLEFT,
            HTRIGHT,
            HTBOTTOMLEFT,
            HTBOTTOM,
            HTBOTTOMRIGHT,
            HTCLIENT
        ]
    );
}

#[test]
fn a_fixed_width_offers_no_horizontal_resize() {
    let mut w = TestWindow::with_frameless_config();
    w.set_minimum_size(400, 100);
    w.set_maximum_size(400, 900);
    // The left and right zones are plain window area; the corners resize the height only.
    assert_eq!(
        codes(&w),
        [HTTOP, HTTOP, HTTOP, HTCLIENT, HTCLIENT, HTBOTTOM, HTBOTTOM, HTBOTTOM, HTCLIENT]
    );
}

#[test]
fn a_fixed_height_offers_no_vertical_resize() {
    let mut w = TestWindow::with_frameless_config();
    w.set_minimum_size(100, 300);
    w.set_maximum_size(900, 300);
    // The top zone is inside the caption, the bottom one is client area; the corners resize the
    // width only.
    assert_eq!(
        codes(&w),
        [HTLEFT, HTCAPTION, HTRIGHT, HTLEFT, HTRIGHT, HTLEFT, HTCLIENT, HTRIGHT, HTCLIENT]
    );
}

#[test]
fn a_fixed_size_offers_no_resize_at_all() {
    let mut w = TestWindow::with_frameless_config();
    w.set_minimum_size(400, 300);
    w.set_maximum_size(400, 300);
    assert_eq!(
        codes(&w),
        [
            HTCAPTION, HTCAPTION, HTCAPTION, HTCLIENT, HTCLIENT, HTCLIENT, HTCLIENT, HTCLIENT,
            HTCLIENT
        ]
    );
}

#[test]
fn an_empty_minimum_is_never_a_fixed_size() {
    // `minimumSize.isEmpty()` returns before any axis is compared (qwindowswindow.cpp:3501-3502),
    // even where minimum and maximum are equal on the other axis.
    let mut w = TestWindow::with_frameless_config();
    w.set_minimum_size(400, 0);
    w.set_maximum_size(400, 0);
    assert_eq!(
        codes(&w),
        [
            HTTOPLEFT,
            HTTOP,
            HTTOPRIGHT,
            HTLEFT,
            HTRIGHT,
            HTBOTTOMLEFT,
            HTBOTTOM,
            HTBOTTOMRIGHT,
            HTCLIENT
        ]
    );
}

#[test]
fn removing_the_fixed_size_restores_the_resize_zones() {
    let mut w = TestWindow::with_frameless_config();
    w.set_minimum_size(400, 300);
    w.set_maximum_size(400, 300);
    w.set_minimum_size(0, 0);
    w.set_maximum_size(WINDOW_SIZE_MAX, WINDOW_SIZE_MAX);
    assert_eq!(
        codes(&w),
        [
            HTTOPLEFT,
            HTTOP,
            HTTOPRIGHT,
            HTLEFT,
            HTRIGHT,
            HTBOTTOMLEFT,
            HTBOTTOM,
            HTBOTTOMRIGHT,
            HTCLIENT
        ]
    );
}
