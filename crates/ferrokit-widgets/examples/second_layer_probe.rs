// SPDX-License-Identifier: LGPL-3.0-only
//! Harness probe (not production): a frameless layered ferrokit `Window` titled "SLTEST" with an
//! opaque magenta rounded-rect fill (a typical root/paint-handler pattern), driven by
//! an external screen-capture driver. `SLTRACE=1` prints the debug resize trace.
//! Windows only; elsewhere it exits with an error so the workspace still builds.

#[cfg(windows)]
mod probe {
    use ferrokit_core::event_loop::EventLoop;
    use ferrokit_gui::geometry::primitives::{Rect, RectF};
    use ferrokit_gui::paint::Brush;
    use ferrokit_gui::tiny_skia::Color;
    use ferrokit_platform::WindowFlags;
    use ferrokit_widgets::EmptyWidget;
    use ferrokit_widgets::window::Window;

    #[link(name = "user32")]
    unsafe extern "system" {
        fn SetProcessDpiAwarenessContext(value: isize) -> i32;
    }

    fn drain() {
        if std::env::var("SLTRACE").is_ok() {
            for e in ferrokit_platform::resize_trace::take() {
                eprintln!(
                    "{:?} xy=({},{}) phys={}x{}",
                    e.kind, e.x, e.y, e.physical_width, e.physical_height
                );
            }
        }
    }

    pub fn main() {
        // Same awareness as a typical application (per-monitor v2); without it DWM bitmap-scales the window.
        // SAFETY: `-4` is the documented pseudo-handle value `DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2`,
        // not a pointer that is dereferenced; the call is made first in `main`, before any window
        // exists, as the process DPI awareness must be set before windows are created.
        unsafe { SetProcessDpiAwarenessContext(-4) };
        if std::env::var("SLTRACE").is_ok() {
            ferrokit_platform::resize_trace::set_enabled(true);
        }
        let el = EventLoop::new();
        let flags = WindowFlags::FRAMELESS | WindowFlags::LAYERED | WindowFlags::CUSTOM_FRAMELESS;
        let mut win =
            Box::new(Window::new("SLTEST", Rect::new(200, 150, 500, 400), flags).expect("window"));
        let root = win.root_widget();
        if let Some(empty) = root.borrow_mut().as_any_mut().downcast_mut::<EmptyWidget>() {
            empty.set_paint_handler(move |p| {
                let (w, h) = (p.device().width(), p.device().height());
                drain();
                p.set_brush(Brush::Color(Color::from_rgba8(255, 0, 255, 255)));
                p.set_pen(None);
                p.draw_rounded_rect(RectF::new(0.0, 0.0, w, h), 8.0, 8.0);
            });
        }
        let hwnd = win.native_handle();
        win.set_mouse_press_handler(move |pos, button| {
            if button == ferrokit_platform::MouseButton::Left {
                let edges = ferrokit_platform::window::calc_frameless_edge(hwnd, pos, false);
                if !edges.is_empty() {
                    ferrokit_platform::window::post_system_resize(hwnd, edges);
                    return true;
                }
            }
            false
        });
        win.set_mouse_move_handler(|_| drain());
        win.show();
        el.exec();
    }
}

#[cfg(windows)]
fn main() {
    probe::main();
}

#[cfg(not(windows))]
fn main() {
    eprintln!("second_layer_probe drives a Win32 layered window and runs on Windows only");
    std::process::exit(1);
}
