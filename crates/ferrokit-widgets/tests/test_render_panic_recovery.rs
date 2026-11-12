// SPDX-License-Identifier: LGPL-3.0-only
//! A panic in a paint handler that unwinds out of `Window::render_and_present` must not leave the
//! window believing it is still rendering: that flag turns every later `update()` into a no-op.
//! (Qt does not unwind; this is the panic containment the embedding application relies on.)
#![cfg(windows)]

use std::cell::{Cell, RefCell};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;

use ferrokit_core::event_loop::EventLoop;
use ferrokit_gui::geometry::primitives::Rect;
use ferrokit_platform::WindowFlags;
use ferrokit_widgets::widget::{EmptyWidget, WidgetRef};
use ferrokit_widgets::window::Window;

#[test]
fn a_window_renders_again_after_a_paint_handler_panicked_in_render_and_present() {
    let el = EventLoop::new();
    let mut win = Box::new(
        Window::new(
            "Render Panic",
            Rect::new(0, 0, 300, 200),
            WindowFlags::empty(),
        )
        .expect("create window"),
    );
    let panic_next = Rc::new(Cell::new(false));
    let mut probe = EmptyWidget::with_geometry(Rect::new(0, 0, 50, 50));
    {
        let panic_next = Rc::clone(&panic_next);
        probe.set_paint_handler(move |_painter| {
            if panic_next.replace(false) {
                panic!("paint handler failed");
            }
        });
    }
    let probe: WidgetRef = Rc::new(RefCell::new(Box::new(probe)));
    win.root_widget().borrow_mut().add_child(Rc::clone(&probe));
    win.render_and_present();
    el.process_events(false);

    panic_next.set(true);
    probe.borrow().update();
    let outcome = catch_unwind(AssertUnwindSafe(|| win.render_and_present()));
    assert!(
        outcome.is_err(),
        "the paint handler's panic reaches the caller"
    );

    let before = win.render_stats().render_count;
    probe.borrow().update();
    for _ in 0..4 {
        el.process_events(false);
    }
    assert!(
        win.render_stats().render_count > before,
        "update() renders again after the panic"
    );
}
