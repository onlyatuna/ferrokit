// SPDX-License-Identifier: LGPL-3.0-only
use ferrokit_core::signal::Signal;
use ferrokit_gui::geometry::primitives::Point;
use std::ptr;
use std::sync::{Arc, Mutex};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, PostMessageW, SetForegroundWindow, TrackPopupMenu,
    HMENU, MF_CHECKED, MF_GRAYED, MF_POPUP, MF_SEPARATOR, MF_STRING, MF_UNCHECKED, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, WM_NULL,
};

use super::{PlatformMenu, PlatformMenuItem};

pub struct Win32MenuItem {
    id: u32,
    text: Mutex<String>,
    is_separator: bool,
    checkable: bool,
    checked: Mutex<bool>,
    enabled: Mutex<bool>,
    activated_signal: Signal<()>,
}

impl Win32MenuItem {
    pub fn new_action(id: u32, text: &str) -> Self {
        Self {
            id,
            text: Mutex::new(text.to_string()),
            is_separator: false,
            checkable: false,
            checked: Mutex::new(false),
            enabled: Mutex::new(true),
            activated_signal: Signal::new(),
        }
    }

    pub fn new_checkable(id: u32, text: &str, checked: bool) -> Self {
        Self {
            id,
            text: Mutex::new(text.to_string()),
            is_separator: false,
            checkable: true,
            checked: Mutex::new(checked),
            enabled: Mutex::new(true),
            activated_signal: Signal::new(),
        }
    }

    pub fn new_separator() -> Self {
        Self {
            id: 0,
            text: Mutex::new(String::new()),
            is_separator: true,
            checkable: false,
            checked: Mutex::new(false),
            enabled: Mutex::new(false),
            activated_signal: Signal::new(),
        }
    }
}

impl PlatformMenuItem for Win32MenuItem {
    fn id(&self) -> u32 {
        self.id
    }

    fn text(&self) -> String {
        self.text.lock().unwrap().clone()
    }

    fn set_text(&mut self, text: &str) {
        *self.text.lock().unwrap() = text.to_string();
    }

    fn is_separator(&self) -> bool {
        self.is_separator
    }

    fn is_checkable(&self) -> bool {
        self.checkable
    }

    fn is_checked(&self) -> bool {
        *self.checked.lock().unwrap()
    }

    fn set_checked(&mut self, checked: bool) {
        *self.checked.lock().unwrap() = checked;
    }

    fn is_enabled(&self) -> bool {
        *self.enabled.lock().unwrap()
    }

    fn set_enabled(&mut self, enabled: bool) {
        *self.enabled.lock().unwrap() = enabled;
    }

    fn activated(&self) -> &Signal<()> {
        &self.activated_signal
    }
}

enum MenuEntry {
    Item(Arc<Win32MenuItem>),
    SubMenu {
        text: String,
        #[allow(dead_code)]
        menu: Box<dyn PlatformMenu>,
    },
}

/// Win32 native menu container implementation.
pub struct Win32Menu {
    hwnd: HWND,
    entries: Vec<MenuEntry>,
}

unsafe impl Send for Win32Menu {}
unsafe impl Sync for Win32Menu {}

impl Win32Menu {
    pub fn new(hwnd: HWND) -> Self {
        Self {
            hwnd,
            entries: Vec::new(),
        }
    }

    /// Builds complete native Win32 HMENU tree structure.
    ///
    /// # Safety
    /// The caller must ensure that Win32 API functions can be safely executed on the current thread
    /// and that the returned `HMENU` handle is properly destroyed when no longer needed.
    pub unsafe fn build_hmenu(&self) -> Result<HMENU, &'static str> {
        let hmenu = CreatePopupMenu();
        if hmenu.is_null() {
            return Err("CreatePopupMenu failed");
        }

        for entry in &self.entries {
            match entry {
                MenuEntry::Item(item) => {
                    if item.is_separator() {
                        AppendMenuW(hmenu, MF_SEPARATOR, 0, ptr::null());
                        continue;
                    }

                    let mut flags = MF_STRING;
                    if !item.is_enabled() {
                        flags |= MF_GRAYED;
                    }
                    if item.is_checkable() {
                        if item.is_checked() {
                            flags |= MF_CHECKED;
                        } else {
                            flags |= MF_UNCHECKED;
                        }
                    }

                    let text = item.text();
                    let wide_text: Vec<u16> =
                        text.encode_utf16().chain(std::iter::once(0)).collect();

                    if AppendMenuW(hmenu, flags, item.id() as usize, wide_text.as_ptr()) == 0 {
                        DestroyMenu(hmenu);
                        return Err("AppendMenuW failed to add item");
                    }
                }
                MenuEntry::SubMenu { text, menu } => {
                    let sub_hmenu = menu.build_native_menu()? as HMENU;
                    let wide_text: Vec<u16> =
                        text.encode_utf16().chain(std::iter::once(0)).collect();
                    if AppendMenuW(
                        hmenu,
                        MF_POPUP | MF_STRING,
                        sub_hmenu as usize,
                        wide_text.as_ptr(),
                    ) == 0
                    {
                        DestroyMenu(sub_hmenu);
                        DestroyMenu(hmenu);
                        return Err("AppendMenuW failed to add submenu");
                    }
                }
            }
        }

        Ok(hmenu)
    }

    /// Finds Win32MenuItem by ID (including within submenus).
    pub fn find_item_by_id(&self, id: u32) -> Option<Arc<dyn PlatformMenuItem>> {
        for entry in &self.entries {
            match entry {
                MenuEntry::Item(item) => {
                    if item.id() == id {
                        return Some(item.clone() as Arc<dyn PlatformMenuItem>);
                    }
                }
                MenuEntry::SubMenu { menu, .. } => {
                    if let Some(item) = menu.find_item(id) {
                        return Some(item);
                    }
                }
            }
        }
        None
    }
}

impl PlatformMenu for Win32Menu {
    fn add_action(&mut self, id: u32, text: &str) -> Arc<dyn PlatformMenuItem> {
        let item = Arc::new(Win32MenuItem::new_action(id, text));
        self.entries.push(MenuEntry::Item(item.clone()));
        item
    }

    fn add_checkable(&mut self, id: u32, text: &str, checked: bool) -> Arc<dyn PlatformMenuItem> {
        let item = Arc::new(Win32MenuItem::new_checkable(id, text, checked));
        self.entries.push(MenuEntry::Item(item.clone()));
        item
    }

    fn add_separator(&mut self) {
        let item = Arc::new(Win32MenuItem::new_separator());
        self.entries.push(MenuEntry::Item(item));
    }

    fn add_submenu(&mut self, text: &str, submenu: Box<dyn PlatformMenu>) {
        self.entries.push(MenuEntry::SubMenu {
            text: text.to_string(),
            menu: submenu,
        });
    }

    fn show_popup(&self, screen_pos: Point) {
        let _ = self.exec_popup(screen_pos);
    }

    fn exec_popup(&self, screen_pos: Point) -> Option<u32> {
        unsafe {
            let hmenu = match self.build_hmenu() {
                Ok(m) => m,
                Err(_) => return None,
            };

            SetForegroundWindow(self.hwnd);

            let cmd_id = TrackPopupMenu(
                hmenu,
                TPM_RIGHTBUTTON | TPM_RETURNCMD,
                screen_pos.x,
                screen_pos.y,
                0,
                self.hwnd,
                ptr::null(),
            ) as u32;

            PostMessageW(self.hwnd, WM_NULL, 0, 0);
            DestroyMenu(hmenu);

            if cmd_id != 0 {
                if let Some(item) = self.find_item_by_id(cmd_id) {
                    item.activated().emit(&());
                }
                Some(cmd_id)
            } else {
                None
            }
        }
    }

    fn build_native_menu(&self) -> Result<isize, &'static str> {
        unsafe { self.build_hmenu().map(|h| h as isize) }
    }

    fn find_item(&self, id: u32) -> Option<Arc<dyn PlatformMenuItem>> {
        self.find_item_by_id(id)
    }

    fn dismiss(&self) {
        unsafe {
            PostMessageW(self.hwnd, WM_NULL, 0, 0);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn test_win32_menu_and_item_lifecycle() {
        let hwnd = ptr::null_mut();
        let mut menu = Win32Menu::new(hwnd);

        let action_item = menu.add_action(101, "Open Dashboard");
        assert_eq!(action_item.id(), 101);
        assert_eq!(action_item.text(), "Open Dashboard");
        assert!(action_item.is_enabled());
        assert!(!action_item.is_checkable());
        assert!(!action_item.is_separator());

        let check_item = menu.add_checkable(102, "Window Click-Through", true);
        assert_eq!(check_item.id(), 102);
        assert_eq!(check_item.text(), "Window Click-Through");
        assert!(check_item.is_checkable());
        assert!(check_item.is_checked());

        menu.add_separator();

        let triggered = Arc::new(AtomicBool::new(false));
        let triggered_clone = triggered.clone();
        action_item.activated().connect(move |_| {
            triggered_clone.store(true, Ordering::SeqCst);
        });

        // Simulate click on 101
        let found = menu.find_item_by_id(101).expect("failed to find item 101");
        found.activated().emit(&());
        assert!(triggered.load(Ordering::SeqCst));

        // Test native HMENU construction and destruction
        unsafe {
            let hmenu = menu.build_hmenu().expect("failed to build HMENU");
            assert!(!hmenu.is_null());
            assert_ne!(DestroyMenu(hmenu), 0);
        }
    }
}
