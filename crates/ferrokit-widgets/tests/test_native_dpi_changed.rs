// SPDX-License-Identifier: LGPL-3.0-only
//! A native DPI change (`WM_DPICHANGED`) reaches the widget tree once and does not re-enter the
//! root widget's `RefCell` (a `BorrowMutError` inside a window procedure aborts the process).
//! Qt: `QWindowSystemInterface::handleScreenLogicalDotsPerInchChange` -> `QEvent::DpiChange`.

#[path = "support/headless.rs"]
mod headless;
#[path = "support/probe_widget.rs"]
mod probe_widget;

use ferrokit_gui::geometry::primitives::Rect;
use ferrokit_platform::{WindowFlags, WindowSystemEvent};
use ferrokit_widgets::window::Window;
use probe_widget::{Log, ProbeWidget};

#[test]
fn native_dpi_change_updates_ratio_and_notifies_the_root_once() {
    let _g = headless::install();
    let mut win = Window::new("dpi", Rect::new(0, 0, 200, 100), WindowFlags::FRAMELESS).unwrap();
    let idx = headless::last_handler();
    let log = Log::default();
    win.set_root_widget(ProbeWidget::new(&log).into_ref());

    headless::send(idx, WindowSystemEvent::DpiChanged { dpi_x: 144, dpi_y: 144 });

    assert_eq!(win.device_pixel_ratio(), 1.5);
    let dpi: Vec<String> = log.borrow().iter().filter(|l| l.starts_with("dpi")).cloned().collect();
    assert_eq!(dpi, ["dpi 1->1.5"]);
}
