// SPDX-License-Identifier: LGPL-3.0-only
//! Closing a window releases its OLE drop target. Own binary: it asserts exact counts of the
//! process-wide live-target counter, which other drop-target tests would disturb.
#![cfg(windows)]

use ferrokit_gui::geometry::primitives::Rect;
use ferrokit_platform::integration::platform;
use ferrokit_platform::window::WindowFlags;
use std::sync::atomic::Ordering;
use std::sync::Mutex;

/// The tests below assert exact counts of one process-wide counter, so they run one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

#[test]
fn closing_a_window_releases_its_drop_target() {
    use ferrokit_platform::drag_drop::win32_ole::LIVE_DROP_TARGETS;
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
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

#[test]
fn a_window_destroyed_outside_close_releases_its_drop_target() {
    use ferrokit_platform::drag_drop::win32_ole::LIVE_DROP_TARGETS;
    use windows_sys::Win32::UI::WindowsAndMessaging::DestroyWindow;
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let before = LIVE_DROP_TARGETS.load(Ordering::SeqCst);
    let mut win = platform()
        .create_window(
            "Destroyed",
            Rect::new(10, 10, 200, 150),
            WindowFlags::NORMAL,
        )
        .expect("create window");
    assert!(win.enable_drop_target(true));
    // SAFETY: the handle is the live window created above on this thread; destroying it behind
    // the `NativeWindow`'s back is the case under test (as when a parent takes it down).
    let destroyed = unsafe { DestroyWindow(win.native_handle() as _) };
    assert_ne!(destroyed, 0, "DestroyWindow");
    drop(win);
    assert_eq!(
        LIVE_DROP_TARGETS.load(Ordering::SeqCst),
        before,
        "OLE gave up its reference while the window still existed"
    );
}

#[test]
fn a_failed_registration_frees_the_target() {
    use ferrokit_platform::drag_drop::win32_ole::{register_drop_target, LIVE_DROP_TARGETS};
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let before = LIVE_DROP_TARGETS.load(Ordering::SeqCst);
    // No window: `RegisterDragDrop` fails with `DRAGDROP_E_INVALIDHWND`.
    assert!(register_drop_target(std::ptr::null_mut(), |_| 0).is_err());
    assert_eq!(LIVE_DROP_TARGETS.load(Ordering::SeqCst), before);
}
