// SPDX-License-Identifier: LGPL-3.0-only
//! Parity test for `QGuiApplication::lastWindowClosed` and `focusWindowChanged` (`qguiapplication.cpp:3939 / 2417`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use ferrokit_gui::geometry::primitives::Rect;
use ferrokit_platform::WindowFlags;
use ferrokit_widgets::application::Application;
use ferrokit_widgets::window::Window;

#[test]
fn last_window_closed_is_emitted_when_last_visible_window_closes() {
    let app = Application::new(vec!["test_app".to_string()]);
    // Disable automatic quit so we observe the signal
    ferrokit_gui::application::GuiApplication::set_quit_on_last_window_closed(false);

    let emitted = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&emitted);
    let _conn = app
        .gui_application()
        .last_window_closed()
        .connect(move |()| {
            flag.store(true, Ordering::SeqCst);
        });

    let mut win = Window::new(
        "test window",
        Rect::new(0, 0, 100, 100),
        WindowFlags::NORMAL,
    )
    .expect("window");
    win.show();
    assert!(!emitted.load(Ordering::SeqCst));

    win.close();
    assert!(
        emitted.load(Ordering::SeqCst),
        "last_window_closed must emit when last window closes"
    );
}

#[test]
fn focus_window_changed_is_emitted_on_active_window_change() {
    Application::reset_for_test();
    let app = Application::new(vec!["test_app".to_string()]);

    let focused_id = Arc::new(parking_lot::Mutex::new(None));
    let target = Arc::clone(&focused_id);
    let _conn = app
        .gui_application()
        .focus_window_changed()
        .connect(move |id| {
            *target.lock() = *id;
        });

    let dummy_id = ferrokit_core::object::ObjectId(42);
    Application::set_active_window(Some(dummy_id));
    assert_eq!(*focused_id.lock(), Some(dummy_id));

    Application::set_active_window(None);
    assert_eq!(*focused_id.lock(), None);
}
