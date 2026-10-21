// SPDX-License-Identifier: LGPL-3.0-only
use ferrokit_core::signal::Signal;
use ferrokit_gui::geometry::primitives::Point;
use std::sync::Arc;

pub mod cocoa_menu;
pub mod dbus_menu;
#[cfg(windows)]
pub mod win32_menu;

#[cfg(windows)]
pub use win32_menu::{Win32Menu, Win32MenuItem};

pub use cocoa_menu::{CocoaMenu, CocoaMenuItem};
pub use dbus_menu::{DBusMenu, DBusMenuItem, DBusMenuLayoutNode, DBusMenuPropValue};

pub trait PlatformMenuItem {
    fn id(&self) -> u32;
    fn text(&self) -> String;
    fn set_text(&mut self, text: &str);

    fn is_separator(&self) -> bool;

    fn is_checkable(&self) -> bool;
    fn is_checked(&self) -> bool;
    fn set_checked(&mut self, checked: bool);

    fn is_enabled(&self) -> bool;
    fn set_enabled(&mut self, enabled: bool);

    fn activated(&self) -> &Signal<()>;
}

pub trait PlatformMenu {
    fn add_action(&mut self, id: u32, text: &str) -> Arc<dyn PlatformMenuItem>;
    fn add_checkable(&mut self, id: u32, text: &str, checked: bool) -> Arc<dyn PlatformMenuItem>;
    fn add_separator(&mut self);
    fn add_submenu(&mut self, text: &str, submenu: Box<dyn PlatformMenu>);
    fn show_popup(&self, screen_pos: Point);
    fn exec_popup(&self, screen_pos: Point) -> Option<u32> {
        self.show_popup(screen_pos);
        None
    }
    fn dismiss(&self);
    fn build_native_menu(&self) -> Result<isize, &'static str> {
        Ok(0)
    }
    fn find_item(&self, _id: u32) -> Option<Arc<dyn PlatformMenuItem>> {
        None
    }
    fn native_handle(&self) -> isize {
        0
    }
}

/// Create a platform-appropriate native menu instance (aligned with `QPlatformIntegration::createPlatformMenu`).
pub fn create_platform_menu(_native_handle: isize) -> Box<dyn PlatformMenu> {
    #[cfg(windows)]
    {
        Box::new(Win32Menu::new(_native_handle as windows_sys::Win32::Foundation::HWND))
    }
    #[cfg(target_os = "macos")]
    {
        Box::new(CocoaMenu::new())
    }
    #[cfg(target_os = "linux")]
    {
        Box::new(DBusMenu::new())
    }
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    {
        Box::new(CocoaMenu::new())
    }
}
