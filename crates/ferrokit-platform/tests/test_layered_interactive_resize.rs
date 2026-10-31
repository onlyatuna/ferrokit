// SPDX-License-Identifier: LGPL-3.0-only
//! Win32 layered surface (`UpdateLayeredWindowIndirect`), the production backend whenever
//! DirectComposition is unavailable: persistent DIB capacity during the native sizing loop.
//!
//! These tests use a real `HWND` and the real GDI/`UpdateLayeredWindow` calls; they never skip.
#![cfg(windows)]

use ferrokit_gui::geometry::{Rect, Region};
use ferrokit_gui::paint::Pixmap;
use ferrokit_platform::resize_trace::{self, TraceKind};
use ferrokit_platform::surface::win32::{LayeredStats, Win32LayeredSurface};
use ferrokit_platform::{NativeWindow, PlatformWindow, WindowFlags};
use std::time::{Duration, Instant};
use windows_sys::Win32::UI::WindowsAndMessaging::SendMessageW;

const WM_ENTERSIZEMOVE: u32 = 0x0231;
const WM_EXITSIZEMOVE: u32 = 0x0232;

fn window(w: i32, h: i32) -> NativeWindow {
    NativeWindow::new(
        "Layered Interactive Resize",
        Rect::new(10, 10, w, h),
        WindowFlags::FRAMELESS | WindowFlags::LAYERED,
    )
    .expect("native window")
}

/// Pixmap whose RGBA value encodes its coordinates, so any misplaced pixel is detectable.
fn gradient(w: u32, h: u32) -> Pixmap {
    let mut pm = Pixmap::new(w, h).expect("pixmap");
    let data = pm.data_mut();
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            data[i] = (x % 251) as u8; // R
            data[i + 1] = (y % 241) as u8; // G
            data[i + 2] = ((x + y) % 239) as u8; // B
            data[i + 3] = 255;
        }
    }
    pm
}

fn present(s: &mut Win32LayeredSurface, w: u32, h: u32, dirty: Rect) {
    let pm = gradient(w, h);
    s.present_dirty_ref(&pm, 1.0, dirty).expect("present");
    assert_eq!(
        (s.width(), s.height()),
        (w, h),
        "visible follows latest frame"
    );
}

/// Verifies every visible pixel of the DIB against `gradient(w, h)` (BGRA, stride = alloc width).
fn assert_dib_matches_gradient(s: &Win32LayeredSurface, w: u32, h: u32) {
    let stride = s.allocated_width() as usize * 4;
    let buf = s.buffer();
    for y in 0..h {
        for x in 0..w {
            let o = y as usize * stride + x as usize * 4;
            let expect = [((x + y) % 239) as u8, (y % 241) as u8, (x % 251) as u8, 255];
            assert_eq!(&buf[o..o + 4], &expect, "pixel ({x},{y}) of {w}x{h}");
        }
    }
}

fn full(w: u32, h: u32) -> Rect {
    Rect::new(0, 0, w as i32, h as i32)
}

#[test]
fn entering_interactive_allocates_headroom_once_and_visible_stays_exact() {
    let win = window(640, 360);
    let mut s = Win32LayeredSurface::new(win.hwnd(), 640, 360).unwrap();
    assert_eq!((s.allocated_width(), s.allocated_height()), (640, 360));
    assert_eq!(s.stats().dib_realloc_count, 0);

    s.set_interactive_resize(true);
    assert_eq!((s.width(), s.height()), (640, 360));
    assert!(s.allocated_width() >= 960 && s.allocated_height() >= 540);
    assert_eq!(s.stats().dib_realloc_count, 1);

    // Idempotent: re-entering must not reallocate.
    s.set_interactive_resize(true);
    assert_eq!(s.stats().dib_realloc_count, 1);
}

#[test]
fn resizes_inside_capacity_never_reallocate_and_pixels_stay_correct() {
    let win = window(640, 360);
    let mut s = Win32LayeredSurface::new(win.hwnd(), 640, 360).unwrap();
    s.set_interactive_resize(true);
    let (aw, ah) = (s.allocated_width(), s.allocated_height());
    let reallocs = s.stats().dib_realloc_count;

    let mut changes = 0u64;
    for i in 0..100u32 {
        let w = 300 + (i * 37) % (aw - 300);
        let h = 200 + (i * 29) % (ah - 200);
        if (w, h) == (s.width(), s.height()) {
            continue;
        }
        changes += 1;
        // Callers mark only a sliver dirty: the stale DIB area must still be repainted.
        present(&mut s, w, h, Rect::new(0, 0, 1, 1));
        assert_eq!((s.allocated_width(), s.allocated_height()), (aw, ah));
        assert_dib_matches_gradient(&s, w, h);
    }
    let st = s.stats();
    assert!(changes >= 90, "sweep must actually change size ({changes})");
    assert_eq!(
        st.dib_realloc_count, reallocs,
        "no CreateDIBSection inside capacity"
    );
    assert_eq!(st.visible_resize_count, changes);
    assert_eq!(st.present_count, changes);
    assert_eq!(
        st.ulw_fallback_count, 0,
        "UpdateLayeredWindowIndirect accepted the larger DIB"
    );
}

#[test]
fn partial_dirty_without_size_change_copies_only_dirty_rect() {
    let win = window(400, 300);
    let mut s = Win32LayeredSurface::new(win.hwnd(), 400, 300).unwrap();
    s.set_interactive_resize(true);
    present(&mut s, 400, 300, full(400, 300)); // settles force_full
    let before = s.stats();
    present(&mut s, 400, 300, Rect::new(10, 20, 30, 40));
    let after = s.stats();
    assert_eq!(after.copied_pixels - before.copied_pixels, 30 * 40);
    assert_eq!(after.dib_realloc_count, before.dib_realloc_count);
    assert_eq!(after.ulw_fallback_count, 0);
}

#[test]
fn exceeding_capacity_reallocates_and_growth_is_geometric() {
    let win = window(320, 240);
    let mut s = Win32LayeredSurface::new(win.hwnd(), 320, 240).unwrap();
    s.set_interactive_resize(true);
    let base = s.stats().dib_realloc_count;
    let (aw0, ah0) = (s.allocated_width(), s.allocated_height());

    // First size beyond the allocation forces exactly one realloc.
    present(&mut s, aw0 + 1, 240, full(aw0 + 1, 240));
    assert_eq!(s.stats().dib_realloc_count, base + 1);
    assert!(s.allocated_width() > aw0 && s.allocated_height() >= ah0);
    assert_dib_matches_gradient(&s, aw0 + 1, 240);

    // 100 further 8px growth steps: O(log) reallocations, not one per step.
    let start_reallocs = s.stats().dib_realloc_count;
    let mut w = aw0 + 1;
    for _ in 0..100 {
        w += 8;
        present(&mut s, w, 240, full(w, 240));
    }
    let growth_reallocs = s.stats().dib_realloc_count - start_reallocs;
    assert!(growth_reallocs > 0, "growth past capacity must reallocate");
    assert!(
        growth_reallocs <= 4,
        "growth reallocated {growth_reallocs}x over 100 steps"
    );
    assert_dib_matches_gradient(&s, w, 240);
}

#[test]
fn normal_resize_is_exact_and_leaving_interactive_returns_to_exact() {
    let win = window(300, 200);
    let mut s = Win32LayeredSurface::new(win.hwnd(), 300, 200).unwrap();

    // Not interactive: every size change reallocates exactly (pre-existing behaviour).
    for (i, w) in [310u32, 330, 320, 400].into_iter().enumerate() {
        present(&mut s, w, 200, full(w, 200));
        assert_eq!((s.allocated_width(), s.allocated_height()), (w, 200));
        assert_eq!(s.buffer().len(), (w * 200 * 4) as usize);
        assert_eq!(s.stats().dib_realloc_count, i as u64 + 1);
        assert_dib_matches_gradient(&s, w, 200);
    }

    // Interactive then normal again.
    s.set_interactive_resize(true);
    present(&mut s, 350, 180, full(350, 180));
    let reallocs = s.stats().dib_realloc_count;
    s.set_interactive_resize(false);
    assert_eq!(
        s.stats().dib_realloc_count,
        reallocs,
        "leaving does not reallocate"
    );
    present(&mut s, 360, 190, full(360, 190));
    assert_eq!((s.allocated_width(), s.allocated_height()), (360, 190));
    assert_eq!(s.stats().dib_realloc_count, reallocs + 1);
    assert_dib_matches_gradient(&s, 360, 190);
    assert_eq!(s.stats().ulw_fallback_count, 0);
}

#[test]
fn capacity_policy_can_be_disabled_for_ab_comparison() {
    let win = window(300, 200);
    let mut s = Win32LayeredSurface::new(win.hwnd(), 300, 200).unwrap();
    s.set_persistent_capacity(false);
    s.set_interactive_resize(true);
    assert_eq!(
        (s.allocated_width(), s.allocated_height()),
        (300, 200),
        "no headroom"
    );
    for i in 1..=20u32 {
        present(&mut s, 300 + i, 200, full(300 + i, 200));
    }
    assert_eq!(s.stats().dib_realloc_count, 20);
}

/// One drag of `steps` sizes. The native window is resized first (as `WM_SIZE` would have been
/// delivered after the frame already moved), pixmaps are pre-built so only the surface is timed.
fn run_drag(
    win: &mut NativeWindow,
    s: &mut Win32LayeredSurface,
    steps: u32,
    base: (u32, u32),
) -> (LayeredStats, Duration, Duration) {
    let frames: Vec<(u32, u32, Pixmap)> = (0..steps)
        .map(|i| {
            // Triangle sweep so the drag both grows and shrinks.
            let phase = if i < steps / 2 { i } else { steps - i };
            let (w, h) = (base.0 + phase * 6, base.1 + phase * 4);
            (w, h, gradient(w, h))
        })
        .collect();
    s.reset_stats();
    let mut worst = Duration::ZERO;
    let mut total = Duration::ZERO;
    for (w, h, pm) in &frames {
        win.set_geometry(Rect::new(10, 10, *w as i32, *h as i32));
        let t = Instant::now();
        s.resize(*w, *h).unwrap();
        s.present_dirty_ref(pm, 1.0, full(*w, *h)).unwrap();
        let dt = t.elapsed();
        total += dt;
        worst = worst.max(dt);
    }
    (s.stats(), total, worst)
}

#[test]
fn ab_same_drag_sequence_persistent_vs_per_size_dib() {
    const STEPS: u32 = 100;
    const ROUNDS: usize = 3;
    let base = (1280u32, 720u32);
    let mut win = window(base.0 as i32, base.1 as i32);

    let row = |n: &str, s: &LayeredStats, t: Duration, w: Duration| {
        println!(
            "{n}: resizes={:3} dib_realloc={:3} presents={:3} copied_px={:>10} | resize={:>7.2}ms copy={:>7.2}ms ulw={:>7.2}ms | resize->present total={:>7.2}ms mean={:>6.3}ms worst={:>6.3}ms",
            s.visible_resize_count,
            s.dib_realloc_count,
            s.present_count,
            s.copied_pixels,
            s.resize_ns as f64 / 1e6,
            s.pixel_copy_ns as f64 / 1e6,
            s.ulw_ns as f64 / 1e6,
            t.as_secs_f64() * 1e3,
            t.as_secs_f64() * 1e3 / STEPS as f64,
            w.as_secs_f64() * 1e3,
        );
    };
    println!(
        "--- layered A/B ({STEPS} steps from {}x{}, {ROUNDS} alternating rounds, debug_assertions={}) ---",
        base.0,
        base.1,
        cfg!(debug_assertions)
    );

    let (mut sa, mut sb) = (LayeredStats::default(), LayeredStats::default());
    for round in 0..ROUNDS {
        // A: pre-3A-7 behaviour (exact DIB per size).
        win.set_geometry(Rect::new(10, 10, base.0 as i32, base.1 as i32));
        let mut a = Win32LayeredSurface::new(win.hwnd(), base.0, base.1).unwrap();
        a.set_persistent_capacity(false);
        a.set_interactive_resize(true);
        let (st, t, w) = run_drag(&mut win, &mut a, STEPS, base);
        row(&format!("round {round} A (exact DIB per size)"), &st, t, w);
        sa = st;
        drop(a);

        // B: persistent capacity while interactive.
        win.set_geometry(Rect::new(10, 10, base.0 as i32, base.1 as i32));
        let mut b = Win32LayeredSurface::new(win.hwnd(), base.0, base.1).unwrap();
        b.set_interactive_resize(true);
        let (st, t, w) = run_drag(&mut win, &mut b, STEPS, base);
        row(&format!("round {round} B (persistent capacity)"), &st, t, w);
        sb = st;
    }

    assert_eq!(
        sa.visible_resize_count, sb.visible_resize_count,
        "same sequence"
    );
    assert!(sa.visible_resize_count >= 99);
    assert_eq!(
        sa.dib_realloc_count, sa.visible_resize_count,
        "A reallocates every resize"
    );
    assert_eq!(
        sb.dib_realloc_count, 0,
        "B: sequence fits in the capacity allocated on entry"
    );
    assert_eq!(
        sa.copied_pixels, sb.copied_pixels,
        "same pixels copied: dirty is the full window"
    );
    assert_eq!(sa.ulw_fallback_count + sb.ulw_fallback_count, 0);
}

/// End to end through `NativeWindow`: real `WM_SIZE`, the native sizing-loop messages, the
/// presenter, and the trace. Asserts on the layered counters when the window's presenter is the
/// layered one (DirectComposition has its own tests).
#[test]
fn window_pipeline_interactive_drag_uses_persistent_dib_and_traces_every_wm_size() {
    use ferrokit_platform::presenter::SurfacePresenter as _;
    let mut win = window(640, 360);
    let hwnd = win.hwnd();
    let present_pm = |win: &mut NativeWindow, pm: &Pixmap| {
        let (w, h) = (pm.physical_width(), pm.physical_height());
        let region = Region::from_coords(0, 0, w as i32, h as i32);
        win.present_region(pm, &region).expect("present_region");
    };
    let present_at = |win: &mut NativeWindow, w: u32, h: u32| present_pm(win, &gradient(w, h));
    present_at(&mut win, 640, 360);
    let is_layered = win.presenter().and_then(|p| p.as_layered()).is_some();
    println!(
        "window presenter backend: {}",
        if is_layered {
            "Layered"
        } else {
            "other (DComp/DC)"
        }
    );

    // SAFETY: `hwnd` is the live window of `win` (alive for the rest of the test), messaged from
    // its own thread; the message has integer parameters only.
    unsafe { SendMessageW(hwnd, WM_ENTERSIZEMOVE, 0, 0) };
    assert!(ferrokit_platform::window::is_interactive_resize(hwnd));

    let mut sizes = Vec::new();
    for i in 0..100u32 {
        let (w, h) = (500 + (i * 13) % 200, 300 + (i * 7) % 120);
        if sizes.last() != Some(&(w, h)) {
            sizes.push((w, h));
        }
    }
    let frames: Vec<Pixmap> = sizes.iter().map(|&(w, h)| gradient(w, h)).collect();
    resize_trace::set_enabled(true);
    resize_trace::clear();
    for (&(w, h), pm) in sizes.iter().zip(&frames) {
        win.set_geometry(Rect::new(10, 10, w as i32, h as i32)); // sends WM_SIZE synchronously
        present_pm(&mut win, pm);
    }
    resize_trace::set_enabled(false);
    let entries = resize_trace::take();

    // SAFETY: `hwnd` is the live window of `win` (still alive), messaged from its own thread; the
    // message has integer parameters only.
    unsafe { SendMessageW(hwnd, WM_EXITSIZEMOVE, 0, 0) };
    assert!(!ferrokit_platform::window::is_interactive_resize(hwnd));

    if cfg!(debug_assertions) {
        let wm: Vec<_> = entries
            .iter()
            .filter(|e| e.kind == TraceKind::WmSize)
            .collect();
        assert!(
            wm.len() >= sizes.len(),
            "one WM_SIZE per geometry change ({} < {})",
            wm.len(),
            sizes.len()
        );
        assert!(wm.iter().all(|e| e.hwnd == hwnd as usize));
        // Latest geometry wins: after each WM_SIZE the next present is for that same size.
        let mut lat = Vec::new();
        for (idx, &(w, h)) in sizes.iter().enumerate() {
            let at = entries
                .iter()
                .position(|e| {
                    e.kind == TraceKind::WmSize && (e.physical_width, e.physical_height) == (w, h)
                })
                .unwrap_or_else(|| panic!("no WmSize for step {idx} {w}x{h}"));
            if is_layered {
                let end = entries[at..]
                    .iter()
                    .find(|e| e.kind == TraceKind::UpdateLayeredWindowEnd)
                    .expect("UpdateLayeredWindowEnd after WmSize");
                assert_eq!((end.physical_width, end.physical_height), (w, h));
                lat.push(end.at.duration_since(entries[at].at));
            }
        }
        if is_layered && !lat.is_empty() {
            lat.sort();
            println!(
                "WM_SIZE -> UpdateLayeredWindow complete: n={} median={:?} p90={:?} max={:?}",
                lat.len(),
                lat[lat.len() / 2],
                lat[lat.len() * 9 / 10],
                lat[lat.len() - 1]
            );
        }
    }

    if is_layered {
        let st = win
            .presenter()
            .and_then(|p| p.as_layered())
            .unwrap()
            .surface()
            .stats();
        println!("window pipeline layered stats: {st:?}");
        assert!(st.present_count >= sizes.len() as u64);
        assert!(
            st.dib_realloc_count <= 2,
            "{} reallocations over {} interactive WM_SIZEs",
            st.dib_realloc_count,
            sizes.len()
        );
        assert_eq!(st.ulw_fallback_count, 0);
    }

    // Normal resize after the loop: exact again, content intact.
    win.set_geometry(Rect::new(10, 10, 333, 222));
    present_at(&mut win, 333, 222);
    if let Some(l) = win.presenter().and_then(|p| p.as_layered()) {
        let s = l.surface();
        assert_eq!((s.width(), s.height()), (333, 222));
        assert_eq!((s.allocated_width(), s.allocated_height()), (333, 222));
        assert_dib_matches_gradient(s, 333, 222);
    }
    let _ = win.presenter_mut().map(|p| p.set_interactive_resize(false));
}
