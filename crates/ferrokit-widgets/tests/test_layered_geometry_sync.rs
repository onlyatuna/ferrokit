// SPDX-License-Identifier: LGPL-3.0-only
//! Does the layered content always describe the same rectangle as the HWND?
//!
//! Real `Window` (frameless + layered, like an overlay window), real HWND, real `WM_SIZE` from
//! `SetWindowPos` inside a `WM_ENTERSIZEMOVE` .. `WM_EXITSIZEMOVE` bracket, resizing from every
//! corner (so both the window origin and its size change). For each iteration the debug trace
//! gives: the rect `SetWindowPos` produced, ferrokit' own geometry, `GetWindowRect` before the
//! render / before `UpdateLayeredWindowIndirect` / after it, and the `pptDst`/`psize` that were
//! passed to `UpdateLayeredWindowIndirect`.
#![cfg(windows)]

use std::ptr::null_mut;

use ferrokit_core::event_loop::EventLoop;
use ferrokit_gui::geometry::primitives::Rect;
use ferrokit_platform::resize_trace::{self, TraceEntry, TraceKind};
use ferrokit_platform::WindowFlags;
use ferrokit_widgets::window::Window;
use windows_sys::Win32::Foundation::{HWND, RECT};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetWindowRect, SendMessageW, SetWindowPos, SWP_NOACTIVATE, SWP_NOZORDER,
};

const WM_ENTERSIZEMOVE: u32 = 0x0231;
const WM_EXITSIZEMOVE: u32 = 0x0232;

type R4 = (i32, i32, u32, u32);

fn rect_of(hwnd: HWND) -> R4 {
    // SAFETY: `RECT` consists of four `i32`, for which the all-zero bit pattern is valid.
    let mut r: RECT = unsafe { std::mem::zeroed() };
    // SAFETY: `hwnd` is only passed to `GetWindowRect`, which fails (leaving `r` zeroed) on an
    // invalid handle instead of writing; `&mut r` points to the live local `RECT` above, the
    // size the API writes.
    unsafe { GetWindowRect(hwnd, &mut r) };
    (
        r.left,
        r.top,
        (r.right - r.left) as u32,
        (r.bottom - r.top) as u32,
    )
}

fn e_rect(e: &TraceEntry) -> R4 {
    (e.x, e.y, e.physical_width, e.physical_height)
}

#[derive(Default, Debug)]
struct Iter {
    set_window_pos: R4,
    wm_size_client: (u32, u32),
    requested: Option<R4>,
    before_render: Option<R4>,
    before_ulw: Option<R4>,
    ulw_args: Option<R4>,
    after_ulw: Option<R4>,
    ulw_end_seen: bool,
    wm_size_count: u32,
    t_wm_size: Option<std::time::Instant>,
    t_ulw_end: Option<std::time::Instant>,
}

fn run(frame_flags: WindowFlags, n: i32) -> Vec<Iter> {
    let el = EventLoop::new();
    let mut win = Box::new(
        Window::new(
            "Layered Geometry Sync",
            Rect::new(100, 100, 400, 300),
            frame_flags,
        )
        .expect("create window"),
    );
    let hwnd = win.native_handle() as HWND;
    win.render_and_present();
    el.process_events(false);

    resize_trace::set_enabled(true);
    resize_trace::clear();
    // SAFETY: `hwnd` is the handle of `win`, created above on this thread and alive until `run`
    // returns; `WM_ENTERSIZEMOVE` takes no parameters and the call passes no pointer.
    unsafe { SendMessageW(hwnd, WM_ENTERSIZEMOVE, 0, 0) };

    let mut iters = Vec::new();
    let mut all = Vec::new();
    let mut origin = rect_of(hwnd);
    for i in 1..=n {
        // Cycle: bottom-right grow, top-left grow, shrink, top-left shrink (origin moves too).
        let (dx, dy, dw, dh) = match i % 4 {
            1 => (0, 0, 7, 5),
            2 => (-6, -4, 6, 4),
            3 => (0, 0, -5, -3),
            _ => (5, 3, -5, -3),
        };
        let (x, y, w, h) = (
            origin.0 + dx,
            origin.1 + dy,
            (origin.2 as i32 + dw) as u32,
            (origin.3 as i32 + dh) as u32,
        );
        // SAFETY: `hwnd` is the handle of `win`, alive until `run` returns and owned by this
        // thread, so the synchronous `WM_SIZE` and render run on this thread; the call passes
        // integers and a null "insert after" handle (allowed with `SWP_NOZORDER`), no pointer
        // into our memory.
        let ok = unsafe {
            SetWindowPos(
                hwnd,
                null_mut(),
                x,
                y,
                w as i32,
                h as i32,
                SWP_NOZORDER | SWP_NOACTIVATE,
            )
        };
        assert_ne!(ok, 0);
        let got = rect_of(hwnd);
        origin = got;
        let mut it = Iter {
            set_window_pos: got,
            ..Default::default()
        };
        // Entries since the previous iteration.
        let entries = resize_trace::take();
        for e in &entries {
            if e.hwnd != hwnd as usize {
                continue;
            }
            match e.kind {
                TraceKind::WmSize => {
                    it.wm_size_client = (e.physical_width, e.physical_height);
                    it.wm_size_count += 1;
                    it.t_wm_size.get_or_insert(e.at);
                }
                TraceKind::RequestedGeometry => it.requested = Some(e_rect(e)),
                TraceKind::WindowRectBeforeRender => it.before_render = Some(e_rect(e)),
                TraceKind::WindowRectBeforeUlw => it.before_ulw = Some(e_rect(e)),
                TraceKind::UlwArgs => it.ulw_args = Some(e_rect(e)),
                TraceKind::WindowRectAfterUlw => it.after_ulw = Some(e_rect(e)),
                TraceKind::UpdateLayeredWindowEnd => {
                    it.ulw_end_seen = true;
                    it.t_ulw_end = Some(e.at);
                }
                _ => {}
            }
        }
        all.extend(entries);
        iters.push(it);
    }
    // SAFETY: `hwnd` is the handle of `win`, which is still alive here (it is dropped when `run`
    // returns) and was created on this thread; `WM_EXITSIZEMOVE` takes no parameters and the
    // call passes no pointer.
    unsafe { SendMessageW(hwnd, WM_EXITSIZEMOVE, 0, 0) };
    el.process_events(false);
    drop(all);
    iters
}

fn report(label: &str, iters: &[Iter]) -> (usize, usize, usize, usize) {
    let n = iters.len();
    let mut no_ulw = 0;
    let mut rect_vs_args = 0;
    let mut before_vs_after = 0;
    let mut render_vs_ulw = 0;
    for it in iters {
        if !it.ulw_end_seen {
            no_ulw += 1;
            continue;
        }
        if it.before_ulw != it.ulw_args {
            rect_vs_args += 1;
        }
        if it.before_ulw != it.after_ulw {
            before_vs_after += 1;
        }
        if it.before_render != it.before_ulw {
            render_vs_ulw += 1;
        }
    }
    println!(
        "{label}: N={n} no_ulw={no_ulw} GetWindowRect(before_ulw)!=pptDst/psize={rect_vs_args} \
         ULW changed HWND rect={before_vs_after} rect(before_render)!=rect(before_ulw)={render_vs_ulw}"
    );
    if let Some(it) = iters.iter().find(|it| it.before_ulw != it.ulw_args) {
        println!("  first pptDst/psize disagreement: {it:?}");
    }
    (no_ulw, rect_vs_args, before_vs_after, render_vs_ulw)
}

#[test]
fn layered_window_content_rect_equals_hwnd_rect_every_iteration() {
    if !cfg!(debug_assertions) {
        return; // trace is compiled out in release
    }
    let flags = WindowFlags::FRAMELESS | WindowFlags::LAYERED | WindowFlags::TOOL;
    let iters = run(flags, 120);
    let (no_ulw, rect_vs_args, before_vs_after, render_vs_ulw) = report("layered-style", &iters);
    let nested = iters.iter().filter(|i| i.wm_size_count != 1).count();
    let size_vs_requested = iters
        .iter()
        .filter(|i| {
            i.requested.map(|r| (r.2, r.3)) != Some((i.set_window_pos.2, i.set_window_pos.3))
        })
        .count();
    println!("WM_SIZE-count != 1 iterations={nested}, ferrokit size != HWND size iterations={size_vs_requested}");
    let mut lat: Vec<u128> = iters
        .iter()
        .filter_map(|i| Some(i.t_ulw_end?.duration_since(i.t_wm_size?).as_micros()))
        .collect();
    lat.sort_unstable();
    if !lat.is_empty() {
        println!(
            "WM_SIZE -> UpdateLayeredWindowEnd (debug build): median={}us p90={}us max={}us",
            lat[lat.len() / 2],
            lat[lat.len() * 9 / 10],
            lat[lat.len() - 1]
        );
    }
    assert_eq!(
        nested, 0,
        "ULW (or anything else) must not cause extra WM_SIZE per resize"
    );
    assert_eq!(
        size_vs_requested, 0,
        "ferrokit geometry size must equal the HWND size it rendered for"
    );
    assert_eq!(
        no_ulw, 0,
        "every WM_SIZE must reach UpdateLayeredWindowIndirect"
    );
    assert_eq!(
        rect_vs_args, 0,
        "pptDst/psize must equal the HWND rect used for the update"
    );
    assert_eq!(before_vs_after, 0, "ULW must not move/resize the HWND");
    assert_eq!(
        render_vs_ulw, 0,
        "HWND rect must not change between render and ULW"
    );
    // And that rect is the one SetWindowPos produced.
    for it in &iters {
        assert_eq!(
            it.ulw_args,
            Some(it.set_window_pos),
            "content rect != HWND rect: {it:?}"
        );
    }
}
