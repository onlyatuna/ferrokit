// SPDX-License-Identifier: LGPL-3.0-only
//! DirectComposition surface policy for interactive resize: a persistent swap chain with a
//! clip visual follows the window; `ResizeBuffers` only when capacity is exceeded.
#![cfg(windows)]

use ferrokit_gui::geometry::Rect;
use ferrokit_gui::paint::Pixmap;
use ferrokit_gui::tiny_skia::Color;
use ferrokit_platform::resize_trace::{self, TraceKind};
use ferrokit_platform::surface::dcomp::DCompSurface;
use ferrokit_platform::surface::PlatformSurface;
use ferrokit_platform::{NativeWindow, WindowFlags};

fn setup(w: u32, h: u32) -> Option<(NativeWindow, DCompSurface)> {
    let window = NativeWindow::new(
        "DComp Interactive Resize",
        Rect::new(10, 10, w as i32, h as i32),
        WindowFlags::FRAMELESS | WindowFlags::LAYERED,
    )
    .expect("native window");
    match DCompSurface::new(window.hwnd(), w, h) {
        Ok(s) => Some((window, s)),
        Err(e) => {
            eprintln!("DirectComposition unavailable ({e}), skipping");
            None
        }
    }
}

fn present_at(surface: &mut DCompSurface, w: u32, h: u32) {
    let mut pm = Pixmap::new(w, h).expect("pixmap");
    pm.fill(Color::from_rgba8((w % 251) as u8, (h % 251) as u8, 90, 255));
    surface
        .present_dirty(&mut pm, 1.0, Rect::new(0, 0, w as i32, h as i32))
        .expect("present");
    assert_eq!(
        (surface.width(), surface.height()),
        (w, h),
        "visible follows latest frame"
    );
}

#[test]
fn interactive_resize_inside_capacity_never_resizes_buffers() {
    let Some((_win, mut s)) = setup(400, 300) else {
        return;
    };
    let (aw, ah) = (s.allocated_width(), s.allocated_height());
    assert!(aw >= 400 && ah >= 300);
    let before = s.stats();

    // 100 resizes sweeping up and down inside the allocation.
    let mut count = 0u64;
    for i in 0..100u32 {
        // Inside the capacity band: visible >= alloc / 2 (smaller would give memory back).
        let (lo_w, lo_h) = (aw.div_ceil(2), ah.div_ceil(2));
        let w = lo_w + (i * 7) % (aw - lo_w);
        let h = lo_h + (i * 5) % (ah - lo_h);
        if (w, h) == (s.width(), s.height()) {
            continue;
        }
        count += 1;
        present_at(&mut s, w, h);
        assert_eq!(
            (s.allocated_width(), s.allocated_height()),
            (aw, ah),
            "fixed allocation"
        );
    }
    let after = s.stats();
    assert_eq!(
        after.resize_buffers_count, before.resize_buffers_count,
        "no ResizeBuffers"
    );
    assert!(after.present_count - before.present_count >= 90);
    assert_eq!(
        after.visible_resize_count - before.visible_resize_count,
        count
    );
    assert_eq!(
        after.clip_update_count - before.clip_update_count,
        count,
        "clip tracks every size"
    );
}

#[test]
fn exceeding_capacity_reallocates_geometrically_not_per_frame() {
    let Some((_win, mut s)) = setup(200, 150) else {
        return;
    };
    let before = s.stats();
    // Grow 1 px at a time across a large range.
    for w in 201..=1200u32 {
        present_at(&mut s, w, 150 + (w - 200) / 4);
        assert!(s.allocated_width() >= s.width() && s.allocated_height() >= s.height());
    }
    let n = s.stats().resize_buffers_count - before.resize_buffers_count;
    assert!(n > 0, "capacity exceeded must reallocate");
    assert!(
        n <= 12,
        "geometric growth, got {n} ResizeBuffers for 1000 resizes"
    );
}

#[test]
fn shrinking_far_below_capacity_reallocates_once_with_hysteresis() {
    let Some((_win, mut s)) = setup(1600, 1000) else {
        return;
    };
    let before = s.stats();
    for w in (300..1600u32).rev().step_by(5) {
        present_at(&mut s, w, w * 5 / 8);
    }
    let n = s.stats().resize_buffers_count - before.resize_buffers_count;
    assert!(n >= 1, "far smaller than capacity must give memory back");
    assert!(n <= 8, "hysteresis keeps reallocations rare, got {n}");
    assert!(s.allocated_width() >= s.width());
}

#[test]
fn state_after_interactive_sequence_is_consistent() {
    let Some((_win, mut s)) = setup(400, 300) else {
        return;
    };
    for w in [450u32, 500, 380, 520, 410] {
        present_at(&mut s, w, 300);
    }
    assert_eq!((s.width(), s.height()), (410, 300));
    assert!(s.allocated_width() >= 410 && s.allocated_height() >= 300);
    // A further plain present at the final size still works and does not reallocate.
    let n = s.stats().resize_buffers_count;
    present_at(&mut s, 410, 300);
    assert_eq!(s.stats().resize_buffers_count, n);
}

#[test]
fn trace_orders_surface_resize_present_commit_per_iteration() {
    let Some((_win, mut s)) = setup(400, 300) else {
        return;
    };
    resize_trace::set_enabled(true);
    resize_trace::clear();
    for w in [410u32, 430, 420, 450] {
        present_at(&mut s, w, 300);
    }
    resize_trace::set_enabled(false);
    let entries = resize_trace::take();
    if !cfg!(debug_assertions) {
        return;
    }
    let kinds: Vec<_> = entries.iter().map(|e| (e.kind, e.physical_width)).collect();
    let expected: Vec<_> = [410u32, 430, 420, 450]
        .iter()
        .flat_map(|&w| {
            [
                (TraceKind::SurfaceResize, w),
                (TraceKind::Present1, w),
                (TraceKind::DCompCommit, w),
            ]
        })
        .collect();
    assert_eq!(kinds, expected);
    assert!(
        entries.windows(2).all(|p| p[0].at <= p[1].at),
        "monotonic timestamps"
    );
}
