// SPDX-License-Identifier: LGPL-3.0-only
//! Compile-time checks of which platform types may cross threads.
//!
//! Native-window, menu, tray, surface and presenter wrappers hold raw OS handles (`HWND`, GDI and
//! COM pointers, Objective-C objects) or boxed event handlers without a thread-safety bound. They
//! are thread-affine, so they must stay `!Send + !Sync` (the compiler then keeps them, and their
//! `Drop`, on the thread that created them). A new `unsafe impl Send`/`Sync` for one of them, or a
//! field change that silently makes one thread-safe, fails the build here.

fn assert_send_sync<T: Send + Sync>() {}

/// Fails to compile if the type is `Send` (the "ambiguous impl" trick for negative bounds).
macro_rules! assert_not_send {
    ($($t:ty),+ $(,)?) => {$(
        const _: fn() = || {
            trait AmbiguousIfSend<A> {
                fn some_item() {}
            }
            impl<T: ?Sized> AmbiguousIfSend<()> for T {}
            struct Invalid;
            impl<T: ?Sized + Send> AmbiguousIfSend<Invalid> for T {}
            let _ = <$t as AmbiguousIfSend<_>>::some_item;
        };
    )+};
}

/// Fails to compile if the type is `Sync`.
macro_rules! assert_not_sync {
    ($($t:ty),+ $(,)?) => {$(
        const _: fn() = || {
            trait AmbiguousIfSync<A> {
                fn some_item() {}
            }
            impl<T: ?Sized> AmbiguousIfSync<()> for T {}
            struct Invalid;
            impl<T: ?Sized + Sync> AmbiguousIfSync<Invalid> for T {}
            let _ = <$t as AmbiguousIfSync<_>>::some_item;
        };
    )+};
}

/// Types bound to the thread that owns their OS handles.
macro_rules! assert_thread_affine {
    ($($t:ty),+ $(,)?) => {
        assert_not_send!($($t),+);
        assert_not_sync!($($t),+);
    };
}

#[cfg(windows)]
assert_thread_affine!(
    crate::window::NativeWindow,
    crate::tray_icon::TrayIcon,
    crate::tray::Win32TrayIcon,
    crate::menu::Win32Menu,
    crate::presenter::Win32DcPresenter,
    crate::presenter::Win32LayeredPresenter,
    crate::presenter::WindowsPresenter,
    crate::surface::DCompSurface,
    crate::surface::Win32LayeredSurface,
    crate::surface::WindowsSurface,
    crate::hotkey::Win32HotkeyManager,
);

// These back ends compile on every platform (Objective-C is simulated off macOS), so they are
// checked everywhere.
assert_thread_affine!(
    crate::objc_runtime::Id,
    crate::objc_runtime::Class,
    crate::objc_runtime::Sel,
    crate::window_cocoa::CocoaNativeWindow,
    crate::window_x11::X11NativeWindow,
    crate::window_wayland::WaylandNativeWindow,
    crate::menu::CocoaMenuItem,
    crate::menu::CocoaMenu,
    crate::menu::DBusMenuItem,
    crate::menu::DBusMenu,
    crate::tray::CocoaStatusItem,
    crate::tray::DbusStatusNotifierItem,
    crate::hotkey::CocoaHotkeyManager,
    // Messages its CALayer and commits a CATransaction on the window's thread.
    crate::surface::CocoaLayerSurface,
);

#[test]
fn plain_data_back_ends_stay_thread_safe() {
    // No raw handles or thread-bound callbacks: nothing ties these to a thread.
    assert_send_sync::<crate::surface::X11ShmSurface>();
    assert_send_sync::<crate::surface::WaylandShmSurface>();
    assert_send_sync::<crate::tray::DbusConnection>();
    assert_send_sync::<crate::hotkey::UnixHotkeyManager>();
    // Requests go through a queue to the manager's own worker thread, so it can be shared.
    assert_send_sync::<crate::global_hotkey::GlobalHotkeyManager>();
    assert_send_sync::<crate::hotkey::HotkeyError>();
}

// No raw handles of their own: a menu entry is a label, flags and a signal.
#[cfg(windows)]
#[test]
fn win32_menu_item_stays_thread_safe() {
    assert_send_sync::<crate::menu::Win32MenuItem>();
}
