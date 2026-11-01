// SPDX-License-Identifier: LGPL-3.0-only
//! Native `Resize` events are never coalesced, but layout activation, paint and present are
//! deferred to one event-loop render phase that uses the latest geometry. Widget `update()`
//! and `Resize` share the same `RenderState` gate.
#![cfg(windows)]

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use ferrokit_core::event_loop::EventLoop;
use ferrokit_gui::geometry::primitives::{Rect, Size};
use ferrokit_platform::window_system_interface::Delivery;
use ferrokit_platform::{WindowFlags, handle_geometry_change};
use ferrokit_widgets::widget::{EmptyWidget, WidgetRef};
use ferrokit_widgets::window::Window;

struct Fixture {
    el: EventLoop,
    /// Boxed so the window keeps a stable address.
    win: Box<Window>,
    hwnd: windows_sys::Win32::Foundation::HWND,
    resize_cb_count: Arc<AtomicU32>,
    /// Root width observed by each paint of the probe child.
    painted_widths: Rc<RefCell<Vec<i32>>>,
    paint_calls: Rc<Cell<u32>>,
}

/// `paint_updates`: number of initial paints that call `root.update()`.
fn fixture(paint_updates: u32) -> Fixture {
    let el = EventLoop::new();
    let mut win = Box::new(
        Window::new(
            "Deferred Render",
            Rect::new(0, 0, 300, 200),
            WindowFlags::empty(),
        )
        .expect("create window"),
    );
    let hwnd = win.native_handle() as windows_sys::Win32::Foundation::HWND;

    let resize_cb_count = Arc::new(AtomicU32::new(0));
    let cb = Arc::clone(&resize_cb_count);
    win.set_resize_handler(move |_: Size| {
        cb.fetch_add(1, Ordering::SeqCst);
    });

    let root = win.root_widget();
    let painted_widths = Rc::new(RefCell::new(Vec::new()));
    let paint_calls = Rc::new(Cell::new(0));
    let budget = Rc::new(Cell::new(0u32));
    let mut probe = EmptyWidget::with_geometry(Rect::new(0, 0, 50, 50));
    {
        let root = root.clone();
        let widths = Rc::clone(&painted_widths);
        let calls = Rc::clone(&paint_calls);
        let budget = Rc::clone(&budget);
        probe.set_paint_handler(move |_painter| {
            widths.borrow_mut().push(root.borrow().geometry().width);
            calls.set(calls.get() + 1);
            if budget.get() > 0 {
                budget.set(budget.get() - 1);
                root.borrow().update();
            }
        });
    }
    let probe: WidgetRef = Rc::new(RefCell::new(Box::new(probe)));
    root.borrow_mut().add_child(probe);

    let mut fx = Fixture {
        el,
        win,
        hwnd,
        resize_cb_count,
        painted_widths,
        paint_calls,
    };
    // Settle initial paint / queued work.
    fx.win.render_and_present();
    pump_idle(&mut fx);
    fx.painted_widths.borrow_mut().clear();
    fx.paint_calls.set(0);
    budget.set(paint_updates);
    fx
}

fn resize(fx: &Fixture, width: i32) {
    handle_geometry_change(Delivery::Default, fx.hwnd, Rect::new(0, 0, width, 200));
}

/// Pumps until a turn delivers nothing (bounded, so a ping-pong fails instead of hanging).
fn pump_idle(fx: &mut Fixture) -> usize {
    let mut turns = 0;
    while turns < 16 {
        let before = fx.win.render_stats();
        let paints = fx.paint_calls.get();
        fx.el.process_events(false);
        let after = fx.win.render_stats();
        turns += 1;
        if after == before && fx.paint_calls.get() == paints {
            break;
        }
    }
    assert!(
        turns < 16,
        "render ping-pong: still rendering after 16 turns"
    );
    turns
}

fn ten_resizes_one_render() {
    let mut fx = fixture(0);
    let before = fx.win.render_stats();

    for i in 0..10 {
        resize(&fx, 310 + i * 10);
    }

    let mid = fx.win.render_stats();
    assert_eq!(mid.resize_event_count - before.resize_event_count, 10);
    assert_eq!(fx.resize_cb_count.load(Ordering::SeqCst), 10);
    assert_eq!(
        mid.render_count, before.render_count,
        "Resize must not render synchronously"
    );
    assert_eq!(
        fx.paint_calls.get(),
        0,
        "Resize must not paint synchronously"
    );
    assert_eq!(
        mid.deferred_render_schedule_count - before.deferred_render_schedule_count,
        1
    );

    fx.el.process_events(false);
    // Further turns must not render again (stale UpdateRequests are ignored).
    pump_idle(&mut fx);

    let after = fx.win.render_stats();
    assert_eq!(
        after.layout_activation_count - before.layout_activation_count,
        1
    );
    assert_eq!(after.render_count - before.render_count, 1);
    assert_eq!(after.present_count - before.present_count, 1);
    assert_eq!(
        after.deferred_render_schedule_count - before.deferred_render_schedule_count,
        1
    );
    assert_eq!(
        fx.paint_calls.get(),
        1,
        "exactly one paint (no UpdateRequest render)"
    );
    // Latest geometry (310 + 9 * 10 = 400), not the first resize.
    assert_eq!(*fx.painted_widths.borrow(), vec![400]);
    assert_eq!(fx.win.geometry().width, 400);
}

#[test]
fn resize_events_are_not_coalesced_but_render_is_deferred() {
    ten_resizes_one_render();
}

#[test]
fn separate_turns_render_separately() {
    let mut fx = fixture(0);
    let before = fx.win.render_stats();

    resize(&fx, 350);
    pump_idle(&mut fx);
    resize(&fx, 420);
    pump_idle(&mut fx);

    let after = fx.win.render_stats();
    assert_eq!(
        after.layout_activation_count - before.layout_activation_count,
        2
    );
    assert_eq!(after.render_count - before.render_count, 2);
    assert_eq!(after.present_count - before.present_count, 2);
    assert_eq!(*fx.painted_widths.borrow(), vec![350, 420]);
}

#[test]
fn update_request_and_resize_share_one_gate() {
    let mut fx = fixture(0);
    let before = fx.win.render_stats();
    let root = fx.win.root_widget();

    // UpdateRequest first, Resize's MetaCall after: still one render.
    root.borrow().update();
    root.borrow().update();
    resize(&fx, 350);
    pump_idle(&mut fx);

    let after = fx.win.render_stats();
    assert_eq!(after.render_count - before.render_count, 1);
    assert_eq!(after.present_count - before.present_count, 1);
    assert_eq!(fx.paint_calls.get(), 1);
}

#[test]
fn update_alone_is_deferred_through_the_gate() {
    let mut fx = fixture(0);
    let before = fx.win.render_stats();
    let root = fx.win.root_widget();

    for _ in 0..5 {
        root.borrow().update();
    }
    // `QWidget::update` only marks dirty and posts (qwidget.cpp:11380-11382,
    // qwidgetrepaintmanager.cpp:356-363): nothing is painted inside `update()`.
    assert_eq!(fx.win.render_stats().render_count, before.render_count);
    assert_eq!(
        fx.paint_calls.get(),
        0,
        "update() must not render synchronously"
    );
    // The next pass of the loop paints once for all five calls.
    fx.el.process_events(false);
    assert_eq!(fx.paint_calls.get(), 1);
    pump_idle(&mut fx);

    let after = fx.win.render_stats();
    assert_eq!(after.render_count - before.render_count, 1);
    assert_eq!(
        after.deferred_render_schedule_count - before.deferred_render_schedule_count,
        1
    );
    assert_eq!(after.direct_render_count, before.direct_render_count);
    assert_eq!(fx.paint_calls.get(), 1);
}

#[test]
fn paint_time_update_neither_recurses_nor_ping_pongs() {
    // The first paint calls update(): exactly one follow-up render, then it settles.
    let mut fx = fixture(1);
    let before = fx.win.render_stats();

    resize(&fx, 350);
    fx.el.process_events(false);
    let first = fx.win.render_stats();
    assert_eq!(
        first.render_count - before.render_count,
        1,
        "no recursive render in one turn"
    );
    assert_eq!(fx.paint_calls.get(), 1);

    pump_idle(&mut fx);
    let settled = fx.win.render_stats();
    assert_eq!(
        settled.render_count - before.render_count,
        2,
        "one follow-up render only"
    );
    assert_eq!(fx.paint_calls.get(), 2);

    // Stuck `render_pending` would swallow this schedule.
    let sched = settled.deferred_render_schedule_count;
    resize(&fx, 380);
    assert_eq!(
        fx.win.render_stats().deferred_render_schedule_count - sched,
        1
    );
    pump_idle(&mut fx);
    assert_eq!(fx.win.render_stats().render_count - settled.render_count, 1);
}

#[test]
fn borrow_conflict_retries_next_turn_and_recovers() {
    let mut fx = fixture(0);
    let before = fx.win.render_stats();
    let bs = fx.win.backing_store_handle();

    resize(&fx, 350);
    let held = bs.borrow_mut();
    fx.el.process_events(false);
    let s = fx.win.render_stats();
    assert_eq!(s.render_count, before.render_count);
    assert_eq!(s.borrow_skipped_count - before.borrow_skipped_count, 1);
    assert_eq!(
        s.borrow_retry_count - before.borrow_retry_count,
        1,
        "one retry queued"
    );
    assert_eq!(fx.paint_calls.get(), 0);

    drop(held);
    pump_idle(&mut fx);
    let s = fx.win.render_stats();
    assert_eq!(s.render_count - before.render_count, 1, "retry rendered");
    assert_eq!(fx.paint_calls.get(), 1);
    assert_eq!(*fx.painted_widths.borrow(), vec![350]);
}

#[test]
fn borrow_conflict_is_bounded_then_rearmed_without_losing_repaint() {
    let mut fx = fixture(0);
    let before = fx.win.render_stats();
    let bs = fx.win.backing_store_handle();

    resize(&fx, 350);
    let held = bs.borrow_mut();
    for _ in 0..6 {
        fx.el.process_events(false);
    }
    let s = fx.win.render_stats();
    assert_eq!(s.render_count, before.render_count);
    assert_eq!(
        s.borrow_retry_count - before.borrow_retry_count,
        1,
        "bounded: no spin"
    );
    assert_eq!(
        s.deferred_render_schedule_count - before.deferred_render_schedule_count,
        2
    );
    assert_eq!(s.borrow_skipped_count - before.borrow_skipped_count, 2);

    // Parked (dirty kept). A synchronous render paints the latest geometry, which settles the
    // parked repaint: no second, deferred render follows it.
    drop(held);
    fx.win.render_and_present();
    pump_idle(&mut fx);
    let s = fx.win.render_stats();
    assert_eq!(s.direct_render_count - before.direct_render_count, 1);
    assert_eq!(
        *fx.painted_widths.borrow(),
        vec![350],
        "parked repaint was recovered"
    );
    assert_eq!(
        s.render_count, before.render_count,
        "the synchronous render already painted it"
    );

    // A fresh invalidation also recovers a parked window.
    let mut fx = fixture(0);
    let before = fx.win.render_stats();
    let bs = fx.win.backing_store_handle();
    resize(&fx, 350);
    let held = bs.borrow_mut();
    for _ in 0..4 {
        fx.el.process_events(false);
    }
    drop(held);
    resize(&fx, 360);
    pump_idle(&mut fx);
    let s = fx.win.render_stats();
    assert_eq!(s.render_count - before.render_count, 1);
    assert_eq!(*fx.painted_widths.borrow(), vec![360]);
}

#[test]
fn set_geometry_adds_no_deferred_render() {
    let mut fx = fixture(0);
    let before = fx.win.render_stats();

    fx.win.set_geometry(Rect::new(0, 0, 350, 200));
    assert_eq!(
        fx.paint_calls.get(),
        1,
        "set_geometry renders synchronously once"
    );

    pump_idle(&mut fx);
    let after = fx.win.render_stats();
    assert_eq!(
        after.render_count, before.render_count,
        "no extra deferred render"
    );
    assert_eq!(
        after.deferred_render_schedule_count,
        before.deferred_render_schedule_count
    );
    assert_eq!(fx.paint_calls.get(), 1);
}

#[test]
fn destroyed_window_deferred_render_is_a_noop() {
    let fx = fixture(0);
    resize(&fx, 350);
    assert_eq!(fx.painted_widths.borrow().len(), 0);

    let Fixture {
        el,
        win,
        painted_widths,
        ..
    } = fx;
    drop(win);
    for _ in 0..3 {
        el.process_events(false);
    }
    assert_eq!(painted_widths.borrow().len(), 0);
}

// ---------------------------------------------------------------------------
// Native interactive sizing loop (WM_ENTERSIZEMOVE .. WM_EXITSIZEMOVE)
// ---------------------------------------------------------------------------

use ferrokit_platform::{WindowSystemEvent, dispatch_window_system_event};

fn interactive(fx: &Fixture, start: bool) {
    let ev = if start {
        WindowSystemEvent::InteractiveResizeStart
    } else {
        WindowSystemEvent::InteractiveResizeEnd
    };
    dispatch_window_system_event(Delivery::Default, fx.hwnd, ev);
}

fn interactive_five_resizes() {
    let mut fx = fixture(0);
    let before = fx.win.render_stats();

    interactive(&fx, true);
    assert!(fx.win.is_interactive_resize());
    for i in 0..5 {
        resize(&fx, 320 + i * 20);
        // Synchronous: painted at exactly the size just received.
        assert_eq!(fx.paint_calls.get(), i as u32 + 1);
        assert_eq!(*fx.painted_widths.borrow().last().unwrap(), 320 + i * 20);
    }
    let mid = fx.win.render_stats();
    assert_eq!(mid.resize_event_count - before.resize_event_count, 5);
    assert_eq!(fx.resize_cb_count.load(Ordering::SeqCst), 5);
    assert_eq!(mid.render_count - before.render_count, 5);
    assert_eq!(mid.present_count - before.present_count, 5);
    assert_eq!(
        mid.layout_activation_count - before.layout_activation_count,
        5
    );
    assert_eq!(
        mid.interactive_render_count - before.interactive_render_count,
        5
    );
    assert_eq!(
        mid.deferred_render_schedule_count, before.deferred_render_schedule_count,
        "interactive resize must not queue a deferred render"
    );

    // No sync + deferred double render, including stale UpdateRequests.
    pump_idle(&mut fx);
    let after = fx.win.render_stats();
    assert_eq!(after.render_count, mid.render_count);
    assert_eq!(fx.paint_calls.get(), 5);

    interactive(&fx, false);
    assert!(!fx.win.is_interactive_resize());
    pump_idle(&mut fx);
    assert_eq!(fx.win.render_stats().render_count, mid.render_count);
    assert!(fx.win.render_idle());
}

#[test]
fn interactive_resize_renders_synchronously() {
    interactive_five_resizes();
}

#[test]
fn normal_resize_still_defers_after_interactive_loop_ends() {
    let mut fx = fixture(0);
    interactive(&fx, true);
    interactive(&fx, false);
    let before = fx.win.render_stats();

    for i in 0..5 {
        resize(&fx, 320 + i * 20);
    }
    let mid = fx.win.render_stats();
    assert_eq!(mid.render_count, before.render_count);
    assert_eq!(
        mid.deferred_render_schedule_count - before.deferred_render_schedule_count,
        1
    );
    pump_idle(&mut fx);
    let after = fx.win.render_stats();
    assert_eq!(after.resize_event_count - before.resize_event_count, 5);
    assert_eq!(after.render_count - before.render_count, 1);
    assert_eq!(
        after.interactive_render_count,
        before.interactive_render_count
    );
    assert_eq!(*fx.painted_widths.borrow(), vec![400]);
}

#[test]
fn queued_deferred_render_before_interactive_start_does_not_double_render() {
    let mut fx = fixture(0);
    let before = fx.win.render_stats();

    resize(&fx, 340); // normal: MetaCall queued
    interactive(&fx, true);
    resize(&fx, 360); // sync render of 360
    assert_eq!(*fx.painted_widths.borrow(), vec![360]);
    interactive(&fx, false);

    pump_idle(&mut fx); // stale MetaCall must be a no-op
    let after = fx.win.render_stats();
    assert_eq!(after.render_count - before.render_count, 1);
    assert_eq!(after.present_count - before.present_count, 1);
    assert_eq!(*fx.painted_widths.borrow(), vec![360]);
    assert!(fx.win.render_idle());
}

#[test]
fn interactive_paint_time_update_does_not_recurse() {
    let mut fx = fixture(1);
    let before = fx.win.render_stats();

    interactive(&fx, true);
    resize(&fx, 350);
    // The paint called update(): no nested render inside the sync render.
    assert_eq!(fx.win.render_stats().render_count - before.render_count, 1);
    assert_eq!(fx.paint_calls.get(), 1);
    pump_idle(&mut fx);
    assert!(fx.win.render_stats().render_count - before.render_count <= 2);
    interactive(&fx, false);
    pump_idle(&mut fx);
    assert!(fx.win.render_idle());
}

#[test]
fn enter_resize_exit_ends_with_latest_geometry_and_clean_state() {
    let mut fx = fixture(0);
    interactive(&fx, true);
    resize(&fx, 377);
    interactive(&fx, false);
    pump_idle(&mut fx);

    assert_eq!(fx.win.geometry().width, 377);
    assert_eq!(*fx.painted_widths.borrow(), vec![377]);
    assert!(!fx.win.is_interactive_resize());
    assert!(fx.win.render_idle());
}

#[test]
fn interactive_borrow_conflict_is_recovered_on_exit() {
    let mut fx = fixture(0);
    let bs = fx.win.backing_store_handle();
    interactive(&fx, true);
    let held = bs.borrow_mut();
    resize(&fx, 350); // sync render fails: dirty kept, one retry queued
    assert_eq!(fx.paint_calls.get(), 0);
    drop(held);
    interactive(&fx, false);
    pump_idle(&mut fx);
    assert_eq!(*fx.painted_widths.borrow(), vec![350]);
    assert!(fx.win.render_idle());
}
