// SPDX-License-Identifier: LGPL-3.0-only
//! Opt-in interactive-resize diagnostics that work in release builds.
//!
//! Enabled by the `FERROKIT_RESIZE_DEBUG` environment variable (unset/`0` = off, zero behaviour
//! change). Between `WM_ENTERSIZEMOVE` and `WM_EXITSIZEMOVE` it records, per `WM_SIZE` / `WM_MOVE`
//! ("frame"): when it arrived, the gap since the previous one, how long each pipeline phase took
//! (resize dispatch, resize callback, layout, backing-store resize, clear, paint, present, pixel
//! copy, `UpdateLayeredWindow*`), whether a render happened, and the HWND rect / `psize` /
//! `pptDst` around every `UpdateLayeredWindowIndirect`. On exit it prints one report to stderr and
//! appends it to a log file: the variable's value if it is a path, else
//! `%TEMP%/ferrokit_resize_debug.log`.
//!
//! Everything runs on the window thread, so the state is thread-local. It only observes; it never
//! changes what is rendered or presented. Phases of a nested frame are also counted in the frame
//! that contains it.

use std::cell::RefCell;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

static ON: LazyLock<bool> = LazyLock::new(|| {
    std::env::var("FERROKIT_RESIZE_DEBUG")
        .map(|v| !v.is_empty() && v != "0")
        .unwrap_or(false)
});

/// `true` when `FERROKIT_RESIZE_DEBUG` is set to something other than `0`/empty.
pub fn enabled() -> bool {
    *ON
}

/// A timed stage of handling one native resize / move.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Root `set_geometry` + widget `Resize` event dispatch.
    ResizeDispatch,
    /// The window's resize callback (the application's own handler).
    ResizeCallback,
    /// `LayoutScheduler::invalidate`.
    LayoutInvalidate,
    /// `LayoutScheduler::activate_pending` (the actual layout).
    LayoutActivate,
    /// `BackingStore::resize` (pixmap reallocation).
    BackingStoreResize,
    /// `collect_dirty_region`.
    CollectDirty,
    /// `BackingStore::clear_rect`.
    Clear,
    /// Widget painting.
    Paint,
    /// `PlatformWindow::present_region` as a whole.
    Present,
    /// RGBA -> BGRA copy into the DIB.
    PixelCopy,
    /// `UpdateLayeredWindowIndirect`.
    Ulw,
    /// The fallback `UpdateLayeredWindow` (only after a failure).
    UlwFallback,
}

const PHASES: [(Phase, &str); 12] = [
    (Phase::ResizeDispatch, "dispatch"),
    (Phase::ResizeCallback, "callback"),
    (Phase::LayoutInvalidate, "lay-inval"),
    (Phase::LayoutActivate, "layout"),
    (Phase::BackingStoreResize, "bs-resize"),
    (Phase::CollectDirty, "collect"),
    (Phase::Clear, "clear"),
    (Phase::Paint, "paint"),
    (Phase::Present, "present"),
    (Phase::PixelCopy, "copy"),
    (Phase::Ulw, "ulw"),
    (Phase::UlwFallback, "ulw-fb"),
];
const NP: usize = PHASES.len();

/// Things worth counting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Count {
    RenderNow,
    RenderRequested,
    RenderQueuedDeferred,
    RenderRan,
    SkipReentrant,
    SkipNotDirty,
    SkipBorrowConflict,
    NoDirtyRegion,
    BackingStoreResized,
    PresentError,
    WmPaint,
    NcCalcSize,
}

const COUNTS: [(Count, &str); 12] = [
    (Count::RenderNow, "render_now calls"),
    (Count::RenderRequested, "request_render calls"),
    (Count::RenderQueuedDeferred, "deferred render queued"),
    (Count::RenderRan, "render_phase ran"),
    (Count::SkipReentrant, "render skipped: already rendering"),
    (Count::SkipNotDirty, "render skipped: not dirty"),
    (Count::SkipBorrowConflict, "render skipped: borrow conflict"),
    (Count::NoDirtyRegion, "render: empty dirty region (no present)"),
    (Count::BackingStoreResized, "backing store reallocated"),
    (Count::PresentError, "present_region error (full re-present)"),
    (Count::WmPaint, "WM_PAINT"),
    (Count::NcCalcSize, "WM_NCCALCSIZE"),
];
const NC: usize = COUNTS.len();

/// One `UpdateLayeredWindowIndirect` call as seen at the call site.
#[derive(Clone, Copy, Debug)]
pub struct UlwSample {
    /// `GetWindowRect` just before the call.
    pub hwnd_before: (i32, i32, i32, i32),
    /// `GetWindowRect` right after the call (and after the fallback, if any).
    pub hwnd_after: (i32, i32, i32, i32),
    pub pt_dst: (i32, i32),
    pub psize: (u32, u32),
    /// Visible pixmap size at the time of the call.
    pub pixmap: (u32, u32),
    /// Allocated DIB size (>= `psize` with persistent capacity).
    pub alloc: (u32, u32),
    pub dirty: (i32, i32, i32, i32),
    pub no_resize_flag: bool,
    /// The surface had to resize itself inside this present.
    pub resized: bool,
    pub force_full: bool,
    pub indirect_error: Option<u32>,
    /// `Some(true)` when the fallback `UpdateLayeredWindow` succeeded.
    pub fallback_ok: Option<bool>,
    pub duration: Duration,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Size,
    Move,
}

#[derive(Clone, Debug)]
struct Frame {
    kind: Kind,
    depth: u32,
    start: Instant,
    /// Microseconds since the drag began.
    at_us: u64,
    /// Microseconds since the previous frame of the same kind ended (0 for the first).
    gap_us: u64,
    client: (i32, i32),
    hwnd_at_begin: (i32, i32, i32, i32),
    total_us: u64,
    phase_us: [u64; NP],
    rendered: bool,
    ulws: Vec<UlwSample>,
    /// `GetWindowRect` when the frame finished.
    hwnd_at_end: (i32, i32, i32, i32),
    nested_frames: u32,
    /// `(microseconds since drag start, text)`.
    notes: Vec<(u64, String)>,
}

impl Frame {
    fn new(kind: Kind, depth: u32, at_us: u64, gap_us: u64, client: (i32, i32), hwnd: (i32, i32, i32, i32)) -> Self {
        Frame {
            kind,
            depth,
            start: Instant::now(),
            at_us,
            gap_us,
            client,
            hwnd_at_begin: hwnd,
            total_us: 0,
            phase_us: [0; NP],
            rendered: false,
            ulws: Vec::new(),
            hwnd_at_end: (0, 0, 0, 0),
            nested_frames: 0,
            notes: Vec::new(),
        }
    }
}

#[derive(Default)]
struct Session {
    active: bool,
    started: Option<Instant>,
    stack: Vec<Frame>,
    done: Vec<Frame>,
    max_depth: u32,
    last_end: [Option<Instant>; 2],
    counts: [u32; NC],
    /// Phase time accumulated while no frame was open (deferred renders, etc.).
    outside_us: [u64; NP],
    outside_ulw: Vec<(u64, UlwSample)>,
    outside_notes: Vec<(u64, String)>,
}

thread_local! {
    static S: RefCell<Session> = RefCell::new(Session::default());
}

const MAX_FRAMES: usize = 20_000;

fn hwnd_rect(hwnd: isize) -> (i32, i32, i32, i32) {
    #[cfg(windows)]
    unsafe {
        let mut r: windows_sys::Win32::Foundation::RECT = std::mem::zeroed();
        windows_sys::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd as _, &mut r);
        (r.left, r.top, r.right, r.bottom)
    }
    #[cfg(not(windows))]
    {
        let _ = hwnd;
        (0, 0, 0, 0)
    }
}

pub fn enter_size_move() {
    if !enabled() {
        return;
    }
    S.with(|s| {
        *s.borrow_mut() = Session {
            active: true,
            started: Some(Instant::now()),
            ..Default::default()
        }
    });
}

fn frame_begin(kind: Kind, hwnd: isize, client: (i32, i32)) {
    S.with(|s| {
        let mut s = s.borrow_mut();
        if !s.active {
            return;
        }
        let now = Instant::now();
        let at = s.started.map(|t| (now - t).as_micros() as u64).unwrap_or(0);
        let idx = kind as usize;
        let gap = s.last_end[idx].map(|t| (now - t).as_micros() as u64).unwrap_or(0);
        let depth = s.stack.len() as u32;
        s.max_depth = s.max_depth.max(depth + 1);
        if let Some(parent) = s.stack.last_mut() {
            parent.nested_frames += 1;
        }
        s.stack.push(Frame::new(kind, depth, at, gap, client, hwnd_rect(hwnd)));
    });
}

fn frame_end(hwnd: isize) {
    S.with(|s| {
        let mut s = s.borrow_mut();
        if !s.active {
            return;
        }
        let Some(mut f) = s.stack.pop() else { return };
        f.total_us = f.start.elapsed().as_micros() as u64;
        f.hwnd_at_end = hwnd_rect(hwnd);
        s.last_end[f.kind as usize] = Some(Instant::now());
        if s.done.len() < MAX_FRAMES {
            s.done.push(f);
        }
    });
}

/// Top of the `WM_SIZE` handler. `client` is the size from `lparam` (physical pixels).
pub fn wm_size_begin(hwnd: isize, phys_w: i32, phys_h: i32) {
    if enabled() {
        frame_begin(Kind::Size, hwnd, (phys_w, phys_h));
    }
}

/// After the `WM_SIZE` handler has delivered the resize.
pub fn wm_size_end(hwnd: isize) {
    if enabled() {
        frame_end(hwnd);
    }
}

pub fn wm_move_begin(hwnd: isize) {
    if enabled() {
        frame_begin(Kind::Move, hwnd, (0, 0));
    }
}

pub fn wm_move_end(hwnd: isize) {
    if enabled() {
        frame_end(hwnd);
    }
}

/// Starts timing a phase; `None` (free) when diagnostics are off or no drag is active.
#[inline]
pub fn start() -> Option<Instant> {
    if enabled() {
        Some(Instant::now())
    } else {
        None
    }
}

/// Ends a phase started with [`start`].
#[inline]
pub fn end(phase: Phase, t: Option<Instant>) {
    let Some(t) = t else { return };
    let us = t.elapsed().as_micros() as u64;
    S.with(|s| {
        let mut s = s.borrow_mut();
        if !s.active {
            return;
        }
        let i = phase as usize;
        match s.stack.last_mut() {
            Some(f) => f.phase_us[i] += us,
            None => s.outside_us[i] += us,
        }
    });
}

#[inline]
pub fn count(c: Count) {
    if !enabled() {
        return;
    }
    S.with(|s| {
        let mut s = s.borrow_mut();
        if s.active {
            s.counts[c as usize] += 1;
        }
    });
}

/// The innermost open frame (if any) rendered.
pub fn mark_rendered() {
    if !enabled() {
        return;
    }
    S.with(|s| {
        let mut s = s.borrow_mut();
        if !s.active {
            return;
        }
        s.counts[Count::RenderRan as usize] += 1;
        if let Some(f) = s.stack.last_mut() {
            f.rendered = true;
        }
    });
}

pub fn ulw(sample: UlwSample) {
    if !enabled() {
        return;
    }
    S.with(|s| {
        let mut s = s.borrow_mut();
        if !s.active {
            return;
        }
        let at = s.started.map(|t| t.elapsed().as_micros() as u64).unwrap_or(0);
        match s.stack.last_mut() {
            Some(f) => f.ulws.push(sample),
            None => s.outside_ulw.push((at, sample)),
        }
    });
}

/// Attaches a free-form note to the innermost open frame (or to the "outside frames" list).
/// The closure only runs when diagnostics are on and a drag is active.
pub fn note(text: impl FnOnce() -> String) {
    if !enabled() {
        return;
    }
    S.with(|s| {
        let mut s = s.borrow_mut();
        if !s.active {
            return;
        }
        let at = s.started.map(|t| t.elapsed().as_micros() as u64).unwrap_or(0);
        let t = text();
        match s.stack.last_mut() {
            Some(f) => f.notes.push((at, t)),
            None => s.outside_notes.push((at, t)),
        }
    });
}

fn pct(v: &mut [u64], p: f64) -> u64 {
    if v.is_empty() {
        return 0;
    }
    v.sort_unstable();
    v[(((v.len() - 1) as f64) * p).round() as usize]
}

fn ms(us: u64) -> String {
    format!("{:.1}", us as f64 / 1000.0)
}

fn sz(r: (i32, i32, i32, i32)) -> (i32, i32) {
    (r.2 - r.0, r.3 - r.1)
}

fn stat3(v: &mut Vec<u64>) -> String {
    format!("p50={} p90={} max={}", ms(pct(v, 0.5)), ms(pct(v, 0.9)), ms(pct(v, 1.0)))
}

fn frame_line(i: usize, f: &Frame) -> String {
    let mut s = format!(
        "  #{i:<3} {} d{} +{}ms gap={} client={}x{} total={}ms",
        if f.kind == Kind::Size { "SIZE" } else { "MOVE" },
        f.depth,
        ms(f.at_us),
        ms(f.gap_us),
        f.client.0,
        f.client.1,
        ms(f.total_us)
    );
    s.push_str(" [");
    let mut first = true;
    for (k, (_, name)) in PHASES.iter().enumerate() {
        if f.phase_us[k] > 0 {
            if !first {
                s.push(' ');
            }
            first = false;
            s.push_str(&format!("{name}={}", ms(f.phase_us[k])));
        }
    }
    s.push(']');
    s.push_str(&format!(" render={}", if f.rendered { "Y" } else { "n" }));
    if f.nested_frames > 0 {
        s.push_str(&format!(" nested={}", f.nested_frames));
    }
    let (bw, bh) = sz(f.hwnd_at_begin);
    let (ew, eh) = sz(f.hwnd_at_end);
    s.push_str(&format!(" hwnd {}x{}@({},{})", bw, bh, f.hwnd_at_begin.0, f.hwnd_at_begin.1));
    if f.hwnd_at_begin != f.hwnd_at_end {
        s.push_str(&format!("->{}x{}@({},{})", ew, eh, f.hwnd_at_end.0, f.hwnd_at_end.1));
    }
    for u in &f.ulws {
        let (b, a) = (sz(u.hwnd_before), sz(u.hwnd_after));
        s.push_str(&format!(
            "\n         ULW psize={}x{} pix={}x{} alloc={}x{} dirty={}x{} hwnd {}x{}->{}x{}{}{}{}",
            u.psize.0,
            u.psize.1,
            u.pixmap.0,
            u.pixmap.1,
            u.alloc.0,
            u.alloc.1,
            u.dirty.2,
            u.dirty.3,
            b.0,
            b.1,
            a.0,
            a.1,
            if u.resized { " resized" } else { "" },
            if u.force_full { " full" } else { "" },
            if u.no_resize_flag { " NORESIZE" } else { "" },
        ));
        if let Some(e) = u.indirect_error {
            s.push_str(&format!(" FAIL err={e} fallback_ok={:?}", u.fallback_ok));
        }
        if u.hwnd_before != u.hwnd_after {
            s.push_str(" HWND-CHANGED-BY-ULW");
        }
    }
    for (at, n) in &f.notes {
        s.push_str(&format!("\n         note +{}ms: {n}", ms(*at)));
    }
    s
}

fn ulw_line(u: &UlwSample) -> String {
    let (b, a) = (sz(u.hwnd_before), sz(u.hwnd_after));
    format!(
        "psize={}x{} pix={}x{} hwnd {}x{}->{}x{}{}{}{}",
        u.psize.0,
        u.psize.1,
        u.pixmap.0,
        u.pixmap.1,
        b.0,
        b.1,
        a.0,
        a.1,
        if u.no_resize_flag { " NORESIZE" } else { "" },
        if u.indirect_error.is_some() { " FAIL" } else { "" },
        if u.hwnd_before != u.hwnd_after { " HWND-CHANGED-BY-ULW" } else { "" },
    )
}

fn build_report(s: &mut Session) -> String {
    let secs = s.started.map(|t| t.elapsed().as_secs_f64()).unwrap_or(0.0);
    let frames = std::mem::take(&mut s.done);
    let sizes: Vec<&Frame> = frames.iter().filter(|f| f.kind == Kind::Size).collect();
    let moves = frames.len() - sizes.len();
    let mut out = String::new();
    out.push_str(&format!(
        "[resize-debug] drag {:.2}s  WM_SIZE={}  WM_MOVE={}  max nesting={}\n",
        secs,
        sizes.len(),
        moves,
        s.max_depth
    ));

    // --- Arrival rhythm -------------------------------------------------------------------
    let mut gaps: Vec<u64> = sizes.iter().skip(1).map(|f| f.gap_us).collect();
    let mut totals: Vec<u64> = sizes.iter().map(|f| f.total_us).collect();
    out.push_str(&format!(
        "  WM_SIZE handler total ms: {}   gap between WM_SIZEs ms (time outside our handler): {}\n",
        stat3(&mut totals),
        stat3(&mut gaps)
    ));
    if secs > 0.0 {
        out.push_str(&format!("  WM_SIZE rate: {:.1}/s\n", sizes.len() as f64 / secs));
    }

    // --- Phase breakdown over WM_SIZE frames ----------------------------------------------
    out.push_str("  per-phase ms across WM_SIZE frames (frames where the phase ran): n, p50 / p90 / max, share of handler time\n");
    let total_all: u64 = sizes.iter().map(|f| f.total_us).sum();
    for (k, (_, name)) in PHASES.iter().enumerate() {
        let mut v: Vec<u64> = sizes.iter().map(|f| f.phase_us[k]).filter(|&x| x > 0).collect();
        if v.is_empty() {
            continue;
        }
        let sum: u64 = v.iter().sum();
        out.push_str(&format!(
            "    {:<10} n={:<3} {}   {:.0}%\n",
            name,
            v.len(),
            stat3(&mut v),
            if total_all == 0 { 0.0 } else { sum as f64 * 100.0 / total_all as f64 }
        ));
    }
    let outside_total: u64 = s.outside_us.iter().sum();
    if outside_total > 0 {
        out.push_str("    phases that ran outside any WM_SIZE/WM_MOVE (deferred render, etc.), total ms:");
        for (k, (_, name)) in PHASES.iter().enumerate() {
            if s.outside_us[k] > 0 {
                out.push_str(&format!(" {name}={}", ms(s.outside_us[k])));
            }
        }
        out.push('\n');
    }

    // --- Render / present accounting ------------------------------------------------------
    let rendered = sizes.iter().filter(|f| f.rendered).count();
    let all_ulw: Vec<&UlwSample> = frames
        .iter()
        .flat_map(|f| f.ulws.iter())
        .chain(s.outside_ulw.iter().map(|(_, u)| u))
        .collect();
    out.push_str(&format!(
        "  WM_SIZE frames that rendered: {}/{}   ULW calls: {} (in frames {}, outside frames {})   ULW per WM_SIZE: {:.2}\n",
        rendered,
        sizes.len(),
        all_ulw.len(),
        all_ulw.len() - s.outside_ulw.len(),
        s.outside_ulw.len(),
        if sizes.is_empty() { 0.0 } else { all_ulw.len() as f64 / sizes.len() as f64 }
    ));
    let no_render: Vec<usize> = frames
        .iter()
        .enumerate()
        .filter(|(_, f)| f.kind == Kind::Size && !f.rendered)
        .map(|(i, _)| i)
        .collect();
    if !no_render.is_empty() {
        out.push_str(&format!("  WM_SIZE frames with NO render: {:?}\n", &no_render[..no_render.len().min(30)]));
    }
    for (k, (_, name)) in COUNTS.iter().enumerate() {
        if s.counts[k] > 0 {
            out.push_str(&format!("    {:<42} {}\n", name, s.counts[k]));
        }
    }

    // --- Geometry consistency -------------------------------------------------------------
    let mut ulw_us: Vec<u64> = all_ulw.iter().map(|u| u.duration.as_micros() as u64).collect();
    let psize_vs_before = all_ulw.iter().filter(|u| (sz(u.hwnd_before).0 as u32, sz(u.hwnd_before).1 as u32) != u.psize).count();
    let psize_vs_after = all_ulw.iter().filter(|u| (sz(u.hwnd_after).0 as u32, sz(u.hwnd_after).1 as u32) != u.psize).count();
    let pos_mismatch = all_ulw.iter().filter(|u| (u.hwnd_before.0, u.hwnd_before.1) != u.pt_dst).count();
    let changed = all_ulw.iter().filter(|u| u.hwnd_before != u.hwnd_after).count();
    let pix_vs_psize = all_ulw.iter().filter(|u| u.pixmap != u.psize).count();
    let norz = all_ulw.iter().filter(|u| u.no_resize_flag).count();
    let fails = all_ulw.iter().filter(|u| u.indirect_error.is_some()).count();
    let fb_fail = all_ulw.iter().filter(|u| u.fallback_ok == Some(false)).count();
    let full = all_ulw.iter().filter(|u| u.force_full).count();
    let resized = all_ulw.iter().filter(|u| u.resized).count();
    out.push_str(&format!(
        "  ULW call ms: {}   force_full={} resized-in-present={} with ULW_EX_NORESIZE={}\n",
        stat3(&mut ulw_us),
        full,
        resized,
        norz
    ));
    out.push_str(&format!(
        "  psize != HWND size before ULW: {}   after ULW: {}   pptDst != HWND pos: {}   HWND rect changed by ULW: {}   pixmap != psize: {}\n",
        psize_vs_before, psize_vs_after, pos_mismatch, changed, pix_vs_psize
    ));
    out.push_str(&format!(
        "  UpdateLayeredWindowIndirect failures: {} (fallback also failed: {})\n",
        fails, fb_fail
    ));
    let cl_vs_hwnd = sizes
        .iter()
        .filter(|f| sz(f.hwnd_at_begin) != f.client)
        .count();
    let hwnd_moved = sizes.iter().filter(|f| f.hwnd_at_begin != f.hwnd_at_end).count();
    out.push_str(&format!(
        "  WM_SIZE client size != HWND size at entry: {}   HWND rect changed during handler: {}\n",
        cl_vs_hwnd, hwnd_moved
    ));

    // --- Timeline -------------------------------------------------------------------------
    let n = frames.len();
    out.push_str("  timeline (ms; gap = since previous frame of the same kind ended; phase times in [] are inclusive of nested frames):\n");
    let head = 50usize.min(n);
    for (i, f) in frames.iter().enumerate().take(head) {
        out.push_str(&frame_line(i, f));
        out.push('\n');
    }
    if n > head {
        let tail_from = n.saturating_sub(20).max(head);
        if tail_from > head {
            out.push_str(&format!("  ... {} frames omitted ...\n", tail_from - head));
        }
        for (i, f) in frames.iter().enumerate().skip(tail_from) {
            out.push_str(&frame_line(i, f));
            out.push('\n');
        }
    }
    if !s.outside_ulw.is_empty() || !s.outside_notes.is_empty() {
        out.push_str("  events outside any WM_SIZE/WM_MOVE frame (ms since drag start); frames are listed above with their own +ms:\n");
        let mut ev: Vec<(u64, String)> = s
            .outside_ulw
            .iter()
            .map(|(t, u)| (*t, format!("ULW {}", ulw_line(u))))
            .chain(s.outside_notes.iter().map(|(t, n)| (*t, format!("note: {n}"))))
            .collect();
        ev.sort_by_key(|(t, _)| *t);
        for (t, l) in ev {
            out.push_str(&format!("    +{}ms {l}\n", ms(t)));
        }
    }
    let mut slow: Vec<(usize, &Frame)> = frames.iter().enumerate().filter(|(_, f)| f.kind == Kind::Size).collect();
    slow.sort_by_key(|(_, f)| std::cmp::Reverse(f.total_us));
    if n > head {
        out.push_str("  slowest WM_SIZE frames:\n");
        for (i, f) in slow.into_iter().take(5) {
            out.push_str(&frame_line(i, f));
            out.push('\n');
        }
    }
    let fail_lines: Vec<String> = frames
        .iter()
        .enumerate()
        .flat_map(|(i, f)| f.ulws.iter().map(move |u| (i, u)))
        .filter(|(_, u)| u.indirect_error.is_some())
        .take(12)
        .map(|(i, u)| {
            format!(
                "    frame#{i}: err={} psize={}x{} hwnd_before={}x{} hwnd_after={}x{} pixmap={}x{} NORESIZE={} fallback_ok={:?}",
                u.indirect_error.unwrap_or(0),
                u.psize.0,
                u.psize.1,
                sz(u.hwnd_before).0,
                sz(u.hwnd_before).1,
                sz(u.hwnd_after).0,
                sz(u.hwnd_after).1,
                u.pixmap.0,
                u.pixmap.1,
                u.no_resize_flag,
                u.fallback_ok
            )
        })
        .collect();
    if !fail_lines.is_empty() {
        out.push_str("  first ULW failures:\n");
        for l in fail_lines {
            out.push_str(&l);
            out.push('\n');
        }
    }
    out
}

/// `WM_EXITSIZEMOVE`: prints and logs the report, then resets.
pub fn exit_size_move() {
    if !enabled() {
        return;
    }
    let text = S.with(|s| {
        let mut s = s.borrow_mut();
        if !s.active {
            return None;
        }
        s.active = false;
        // Close any frame left open (should not happen).
        while let Some(mut f) = s.stack.pop() {
            f.total_us = f.start.elapsed().as_micros() as u64;
            s.done.push(f);
        }
        Some(build_report(&mut s))
    });
    let Some(text) = text else { return };
    eprint!("{text}");
    let path = match std::env::var("FERROKIT_RESIZE_DEBUG") {
        Ok(v) if v != "1" => std::path::PathBuf::from(v),
        _ => std::env::temp_dir().join("ferrokit_resize_debug.log"),
    };
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        use std::io::Write;
        let _ = f.write_all(text.as_bytes());
        let _ = f.write_all(b"\n");
    }
}
