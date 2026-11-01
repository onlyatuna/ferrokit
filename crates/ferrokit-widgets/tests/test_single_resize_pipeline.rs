// SPDX-License-Identifier: LGPL-3.0-only
//! One native `WM_SIZE` must produce exactly one canonical resize delivery
//! (`WindowSystemEvent::Resize`) and at most one render/present, even when the window is bound
//! to an event loop (which makes the platform layer post window events to the QObject).
//!
//! Real `ferrokit_widgets::Window`, real HWND, real `WM_SIZE` (via `SetWindowPos`), real
//! `WM_ENTERSIZEMOVE`/`WM_EXITSIZEMOVE`, and the event loop's posted-event pump.
#![cfg(windows)]

use std::ptr::null_mut;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use ferrokit_core::event_loop::EventLoop;
use ferrokit_gui::geometry::primitives::{Rect, Size};
use ferrokit_platform::WindowFlags;
use ferrokit_platform::resize_trace::{self, TraceKind};
use ferrokit_widgets::window::{RenderStats, Window};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, SendMessageW, SetWindowPos,
};

const WM_ENTERSIZEMOVE: u32 = 0x0231;
const WM_EXITSIZEMOVE: u32 = 0x0232;

struct Fixture {
    el: EventLoop,
    win: Box<Window>,
    hwnd: HWND,
    callbacks: Arc<AtomicU32>,
}

fn fixture() -> Fixture {
    let el = EventLoop::new();
    let mut win = Box::new(
        Window::new(
            "Single Resize Pipeline",
            Rect::new(0, 0, 400, 300),
            WindowFlags::empty(),
        )
        .expect("create window"),
    );
    let hwnd = win.native_handle() as HWND;
    // The platform layer posts window events (Close/Move/Resize/...) to this receiver.
    ferrokit_platform::window::register_window_event_binding(hwnd, el.handle(), win.id());

    let callbacks = Arc::new(AtomicU32::new(0));
    let cb = Arc::clone(&callbacks);
    win.set_resize_handler(move |_: Size| {
        cb.fetch_add(1, Ordering::SeqCst);
    });

    let mut fx = Fixture {
        el,
        win,
        hwnd,
        callbacks,
    };
    fx.win.render_and_present();
    pump_idle(&mut fx);
    fx
}

/// Pumps posted events until a turn changes nothing (bounded: a ping-pong fails, not hangs).
fn pump_idle(fx: &mut Fixture) {
    for _ in 0..16 {
        let before = fx.win.render_stats();
        fx.el.process_events(false);
        if fx.win.render_stats() == before {
            return;
        }
    }
    panic!("posted-event queue never drained within 16 turns");
}

fn native_resize(fx: &Fixture, i: i32) {
    // Each step has a distinct size, so every WM_SIZE is a real size change.
    // SAFETY: `fx.hwnd` is the handle of `fx.win`, which the fixture owns and keeps alive and
    // which was created on this thread, so the synchronous `WM_SIZE` runs on this thread; the
    // call passes integers and a null insert-after handle, no pointer into our memory.
    let ok = unsafe {
        SetWindowPos(
            fx.hwnd,
            null_mut(),
            0,
            0,
            400 + 3 * i,
            300 + 2 * i,
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
        )
    };
    assert_ne!(ok, 0, "SetWindowPos failed");
}

struct Delta {
    wm_size: usize,
    stats: RenderStats,
    callbacks: u32,
}

fn drag(interactive: bool, n: i32) -> Delta {
    let mut fx = fixture();
    resize_trace::set_enabled(true);
    resize_trace::clear();
    let before = fx.win.render_stats();
    let cb_before = fx.callbacks.load(Ordering::SeqCst);

    if interactive {
        // SAFETY: `fx.hwnd` is the handle of `fx.win`, which the fixture owns and keeps alive,
        // created on this thread; `WM_ENTERSIZEMOVE` takes no parameters and no pointer is passed.
        unsafe { SendMessageW(fx.hwnd, WM_ENTERSIZEMOVE, 0, 0) };
    }
    for i in 1..=n {
        native_resize(&fx, i);
    }
    // The modal loop keeps pumping posted events while the drag is in progress.
    pump_idle(&mut fx);
    if interactive {
        // SAFETY: `fx.hwnd` is the handle of `fx.win`, which the fixture owns and keeps alive,
        // created on this thread; `WM_EXITSIZEMOVE` takes no parameters and no pointer is passed.
        unsafe { SendMessageW(fx.hwnd, WM_EXITSIZEMOVE, 0, 0) };
        pump_idle(&mut fx);
    }

    let wm_size = resize_trace::take()
        .iter()
        .filter(|e| e.kind == TraceKind::WmSize && e.hwnd == fx.hwnd as usize)
        .count();
    let after = fx.win.render_stats();
    let stats = RenderStats {
        resize_event_count: after.resize_event_count - before.resize_event_count,
        deferred_render_schedule_count: after.deferred_render_schedule_count
            - before.deferred_render_schedule_count,
        layout_activation_count: after.layout_activation_count - before.layout_activation_count,
        render_count: after.render_count - before.render_count,
        present_count: after.present_count - before.present_count,
        borrow_skipped_count: after.borrow_skipped_count - before.borrow_skipped_count,
        borrow_retry_count: after.borrow_retry_count - before.borrow_retry_count,
        interactive_render_count: after.interactive_render_count - before.interactive_render_count,
        resize_callback_count: after.resize_callback_count - before.resize_callback_count,
        event_resize_count: after.event_resize_count - before.event_resize_count,
        direct_render_count: after.direct_render_count - before.direct_render_count,
        direct_present_count: after.direct_present_count - before.direct_present_count,
        last_painted_region: after.last_painted_region,
    };
    println!(
        "interactive={interactive} N={n}: WM_SIZE={wm_size} resize_semantic={} callbacks={} \
         event_resize(EventKind::Resize)={} layout={} render_phase={} present={} \
         direct_render={} direct_present={} deferred_scheduled={}",
        stats.resize_event_count,
        stats.resize_callback_count,
        stats.event_resize_count,
        stats.layout_activation_count,
        stats.render_count,
        stats.present_count,
        stats.direct_render_count,
        stats.direct_present_count,
        stats.deferred_render_schedule_count,
    );
    Delta {
        wm_size,
        stats,
        callbacks: fx.callbacks.load(Ordering::SeqCst) - cb_before,
    }
}

fn assert_one_canonical_resize(d: &Delta, n: i32) {
    let n = n as u64;
    // WM_SIZE accounting is only available where the trace is compiled in.
    if cfg!(debug_assertions) {
        assert_eq!(d.wm_size as u64, n, "one WM_SIZE per SetWindowPos");
    }
    assert_eq!(
        d.stats.resize_event_count, n,
        "one resize semantic per WM_SIZE"
    );
    assert_eq!(d.stats.resize_callback_count, n);
    assert_eq!(d.callbacks as u64, n, "one resize callback per WM_SIZE");
    assert_eq!(
        d.stats.event_resize_count, 0,
        "native WM_SIZE must not also reach Window::event as EventKind::Resize"
    );
    assert_eq!(
        d.stats.direct_render_count, 0,
        "no synchronous render outside the canonical gate"
    );
}

#[test]
fn interactive_one_wm_size_one_resize_one_present() {
    let n = 100;
    let d = drag(true, n);
    assert_one_canonical_resize(&d, n);
    assert_eq!(
        d.stats.render_count, n as u64,
        "one render phase per WM_SIZE"
    );
    assert_eq!(d.stats.layout_activation_count, n as u64);
    assert_eq!(
        d.stats.present_count, n as u64,
        "one present per WM_SIZE, not 2N"
    );
    assert_eq!(d.stats.interactive_render_count, n as u64);
}

#[test]
fn normal_resize_stays_deferred_with_a_single_pipeline() {
    let n = 100;
    let d = drag(false, n);
    assert_one_canonical_resize(&d, n);
    assert!(
        (1..=n as u64).contains(&d.stats.render_count),
        "coalesced render count out of range: {}",
        d.stats.render_count
    );
    assert_eq!(d.stats.interactive_render_count, 0);
    assert_eq!(d.stats.render_count, d.stats.layout_activation_count);
    assert!(d.stats.present_count <= d.stats.render_count);
}
