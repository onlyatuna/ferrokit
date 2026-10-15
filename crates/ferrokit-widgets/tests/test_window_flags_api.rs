// SPDX-License-Identifier: LGPL-3.0-only
//! Parity test for `QWidget::setWindowFlags` and `QWidget::setWindowFlag` (`qwidget.cpp:10623-10648`).

use ferrokit_gui::geometry::primitives::Rect;
use ferrokit_platform::WindowFlags;
use ferrokit_widgets::window::Window;

#[test]
fn window_flags_toggle_dynamically() {
    let mut win = Window::new(
        "Window Flags Test",
        Rect::new(100, 100, 200, 150),
        WindowFlags::FRAMELESS,
    )
    .expect("create window");

    assert_eq!(win.window_flags(), WindowFlags::FRAMELESS);

    // Toggle set_window_flag(STAYS_ON_TOP, true)
    win.set_window_flag(WindowFlags::STAYS_ON_TOP, true);
    assert!(win.window_flags().contains(WindowFlags::STAYS_ON_TOP));

    // Toggle set_window_flag(STAYS_ON_TOP, false)
    win.set_window_flag(WindowFlags::STAYS_ON_TOP, false);
    assert!(!win.window_flags().contains(WindowFlags::STAYS_ON_TOP));

    // Toggle click_through
    win.set_window_flag(WindowFlags::CLICK_THROUGH, true);
    assert!(win.window_flags().contains(WindowFlags::CLICK_THROUGH));

    win.set_window_flag(WindowFlags::CLICK_THROUGH, false);
    assert!(!win.window_flags().contains(WindowFlags::CLICK_THROUGH));

    // set_window_flags batch update
    win.set_window_flags(WindowFlags::STAYS_ON_TOP | WindowFlags::CLICK_THROUGH);
    assert!(win.window_flags().contains(WindowFlags::STAYS_ON_TOP));
    assert!(win.window_flags().contains(WindowFlags::CLICK_THROUGH));
}
