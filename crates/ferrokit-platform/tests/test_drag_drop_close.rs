// SPDX-License-Identifier: LGPL-3.0-only
//! Closing a window releases its OLE drop target. Own binary: it asserts exact counts of the
//! process-wide live-target counter, which other drop-target tests would disturb.
#![cfg(windows)]

use ferrokit_gui::geometry::primitives::Rect;
use ferrokit_platform::integration::platform;
use ferrokit_platform::window::WindowFlags;
use std::sync::atomic::Ordering;

#[test]
fn closing_a_window_releases_its_drop_target() {
    use ferrokit_platform::drag_drop::win32_ole::LIVE_DROP_TARGETS;
    let before = LIVE_DROP_TARGETS.load(Ordering::SeqCst);
    let mut win = platform()
        .create_window("Close", Rect::new(10, 10, 200, 150), WindowFlags::NORMAL)
        .expect("create window");
    assert!(win.enable_drop_target(true));
    assert_eq!(LIVE_DROP_TARGETS.load(Ordering::SeqCst), before + 1);
    drop(win);
    assert_eq!(
        LIVE_DROP_TARGETS.load(Ordering::SeqCst),
        before,
        "the target and the OLE registration are gone with the window"
    );
}
