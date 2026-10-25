// SPDX-License-Identifier: LGPL-3.0-only
//! ferrokit-platform: Platform abstraction and window integration.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

/// Real user id of this process (the `/run/user/<uid>` runtime directory, D-Bus `AUTH EXTERNAL`).
#[cfg(target_os = "linux")]
pub(crate) fn current_uid() -> u32 {
    extern "C" {
        fn getuid() -> u32;
    }
    // SAFETY: getuid takes no arguments, cannot fail and touches no memory.
    unsafe { getuid() }
}

pub mod dialogs;
pub mod dialogs_cocoa;
pub use dialogs::{
    platform_dialogs, DialogIcon, FileFilter, MessageBoxButtons, MessageBoxResult, PlatformDialogs,
    SystemDialogs,
};
pub mod desktop_services;
pub use desktop_services::{open_file, open_url, set_url_handler, UrlHandler};
pub mod backdrop;
pub mod clipboard;
pub mod clipboard_cocoa;
pub mod cursor;
pub mod cursor_cocoa;
pub mod cursor_bitmap;
pub mod drag_drop;
pub mod high_dpi;
pub use high_dpi::*;
pub mod hotkey;
pub mod ime;
pub mod integration;
pub mod layered;
pub mod menu;
pub mod objc_runtime;
pub mod platform_tray;
pub mod platform_window;
pub mod presenter;
pub mod resize_debug;
pub mod resize_trace;
pub mod screen;
pub mod single_instance;
pub mod size_limits;
pub mod surface;
pub mod theme;
pub mod tray;
pub mod tray_icon;
pub mod window;
pub mod window_cocoa;
pub mod window_cocoa_input;
pub mod window_system_interface;
pub mod window_wayland;
pub mod window_x11;
#[cfg(windows)]
mod window_region;
#[cfg(test)]
mod thread_affinity;
pub use single_instance::*;

#[cfg(windows)]
pub use windows_sys::Win32::Foundation::HWND;
#[cfg(not(windows))]
pub type HWND = isize;
#[cfg(windows)]
pub use clipboard::Win32Clipboard;
pub use clipboard_cocoa::CocoaClipboard;
pub use clipboard::{Clipboard, GenericClipboard, MimeData, PlatformClipboard};
#[cfg(windows)]
pub use cursor::Win32Cursor;
pub use cursor::{CocoaCursor, CursorShape, GenericCursor, PlatformCursor, UnixCursor};

#[cfg(windows)]
pub use hotkey::Win32HotkeyManager;
pub use hotkey::{
    CocoaHotkeyManager, GenericHotkeyManager, HotkeyModifiers, PlatformHotkeyManager,
    UnixHotkeyManager,
};
#[cfg(windows)]
pub use integration::Win32PlatformIntegration;
pub use integration::{
    cocoa::CocoaPlatformIntegration, generic::GenericPlatformIntegration, platform,
    set_platform_integration, unix::DisplayServerKind, unix::UnixPlatformIntegration,
    PlatformIntegration,
};
#[cfg(windows)]
pub use layered::LayeredSurface;
pub use menu::{
    create_platform_menu, CocoaMenu, CocoaMenuItem, DBusMenu, DBusMenuItem, PlatformMenu,
    PlatformMenuItem,
};
#[cfg(windows)]
pub use menu::{Win32Menu, Win32MenuItem};
pub use objc_runtime::{
    objc_get_class, sel_register_name, ActionTarget, CGFloat, CGPoint, CGRect, CGSize, Class, Id,
    MockObjcRuntime, NSInteger, NSUInteger, ObjcMsg, Sel, BOOL, NO, NS_CONTROL_STATE_VALUE_MIXED,
    NS_CONTROL_STATE_VALUE_OFF, NS_CONTROL_STATE_VALUE_ON, NS_FLOATING_WINDOW_LEVEL,
    NS_MODAL_PANEL_WINDOW_LEVEL, NS_NORMAL_WINDOW_LEVEL, NS_SCREEN_SAVER_WINDOW_LEVEL,
    NS_SQUARE_STATUS_ITEM_LENGTH, NS_STATUS_WINDOW_LEVEL, NS_VARIABLE_STATUS_ITEM_LENGTH,
    NS_WINDOW_STYLE_MASK_BORDERLESS, NS_WINDOW_STYLE_MASK_CLOSABLE,
    NS_WINDOW_STYLE_MASK_FULL_SIZE_CONTENT_VIEW, NS_WINDOW_STYLE_MASK_MINIATURIZABLE,
    NS_WINDOW_STYLE_MASK_RESIZABLE, NS_WINDOW_STYLE_MASK_TITLED, YES,
};
pub use platform_tray::{GenericTrayIcon, PlatformTrayIcon, TrayMessageIcon};
pub use platform_window::{GenericWindow, PlatformWindow, WindowEdge, WindowEdges};
pub use presenter::SurfacePresenter;
#[cfg(windows)]
pub use presenter::{Win32DcPresenter, Win32LayeredPresenter, WindowsPresenter};
#[cfg(windows)]
pub use screen::Win32Screen;
pub use screen::{
    clamp_window_rect_to_screens, ensure_within_screens, CocoaScreen, GenericScreen,
    PlatformScreen, X11Screen,
};
pub use surface::PlatformSurface;
#[cfg(windows)]
pub use surface::Win32LayeredSurface;
pub use surface::{CocoaLayerSurface, WaylandShmSurface, X11ShmSurface};

#[cfg(windows)]
pub use theme::Win32Theme;
pub use theme::{CocoaTheme, ColorScheme, GenericTheme, NativeStyle, PlatformTheme, UnixTheme};
#[cfg(windows)]
pub use tray::Win32TrayIcon;
pub use tray::{
    dbus_connection::DBUS_MESSAGE_TYPE_METHOD_CALL, CocoaStatusItem, DbusConnection, DbusMessage,
    DbusStatusNotifierItem,
};
pub use tray_icon::{Menu, MenuItem, TrayActivation, TrayIcon, WM_TRAY_CALLBACK};
pub use window::{
    dispatch_window_system_event, flush_window_system_events, handle_geometry_change,
    post_window_system_event, send_window_system_event_immediately, set_dpi_awareness,
    unregister_window_event_binding, CustomFramelessConfig, NativeWindow, PlatformWindowStateFlags,
    SetGeometryGuard, WindowFlags,
};
#[cfg(windows)]
pub use window::{is_dcomp_enabled, set_dcomp_enabled};
pub use window_cocoa::{qt_mac_flip_point, qt_mac_flip_rect, CocoaNativeEvent, CocoaNativeWindow};
pub use window_system_interface::{
    ClosureWindowEventHandler, Delivery, KeyboardModifiers, MouseButton, PowerEvent, WheelDelta,
    WindowSystemEvent, WindowSystemEventHandler,
};
pub use window_wayland::{WaylandEvent, WaylandNativeWindow};
pub use window_x11::{X11Event, X11NativeWindow};
