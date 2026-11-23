// SPDX-License-Identifier: LGPL-3.0-only
//! Single integration-test binary (one link step instead of one per file).

#[path = "test_action_target.rs"]
mod test_action_target;
#[path = "test_cocoa_clipboard.rs"]
mod test_cocoa_clipboard;
#[path = "test_cocoa_dialogs.rs"]
mod test_cocoa_dialogs;
#[path = "test_cocoa_hotkey.rs"]
mod test_cocoa_hotkey;
#[path = "test_cocoa_window_close.rs"]
mod test_cocoa_window_close;
#[path = "test_cocoa_window_input.rs"]
mod test_cocoa_window_input;
#[path = "test_cocoa_window_methods.rs"]
mod test_cocoa_window_methods;
#[path = "test_cross_platform_cursor.rs"]
mod test_cross_platform_cursor;
#[path = "test_cursor_bitmap.rs"]
mod test_cursor_bitmap;
#[path = "test_cursor_size.rs"]
mod test_cursor_size;
#[path = "test_dark_mode_window_frames.rs"]
mod test_dark_mode_window_frames;
#[path = "test_dcomp_creation.rs"]
mod test_dcomp_creation;
#[path = "test_dcomp_interactive_resize.rs"]
mod test_dcomp_interactive_resize;
#[path = "test_dcomp_recovery.rs"]
mod test_dcomp_recovery;
#[path = "test_drag_drop_close.rs"]
mod test_drag_drop_close;
#[path = "test_drag_drop_ole.rs"]
mod test_drag_drop_ole;
#[path = "test_global_hotkey.rs"]
mod test_global_hotkey;
#[path = "test_layered_interactive_resize.rs"]
mod test_layered_interactive_resize;
#[path = "test_native_double_click.rs"]
mod test_native_double_click;
#[path = "test_platform_abstractions.rs"]
mod test_platform_abstractions;
#[path = "test_platform_modern_features.rs"]
mod test_platform_modern_features;
#[path = "test_screen_at_logical.rs"]
mod test_screen_at_logical;
#[path = "test_size_limits.rs"]
mod test_size_limits;
#[path = "test_style_hints.rs"]
mod test_style_hints;
#[path = "test_tray_activation.rs"]
mod test_tray_activation;
#[path = "test_tray_icon_win.rs"]
mod test_tray_icon_win;
#[path = "test_window_activation.rs"]
mod test_window_activation;
#[path = "test_window_event_dispatch.rs"]
mod test_window_event_dispatch;
#[path = "test_window_opacity.rs"]
mod test_window_opacity;
#[path = "test_window_size_limits.rs"]
mod test_window_size_limits;
