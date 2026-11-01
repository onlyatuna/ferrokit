// SPDX-License-Identifier: LGPL-3.0-only
use crate::win32_handles::OwnedMenu;
use ferrokit_core::signal::Signal;
use ferrokit_gui::geometry::primitives::Point;
use std::ptr;
use std::sync::{Arc, Mutex};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, HMENU, MF_CHECKED, MF_GRAYED, MF_POPUP, MF_SEPARATOR, MF_STRING, MF_UNCHECKED,
    PostMessageW, SetForegroundWindow, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, WM_NULL,
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

// Deliberately neither `Send` nor `Sync` (raw `HWND`): the menu is shown and destroyed through
// window APIs that must run on the thread that owns `hwnd`.

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
        let hmenu = OwnedMenu::create_popup().ok_or("CreatePopupMenu failed")?;

        for entry in &self.entries {
            match entry {
                MenuEntry::Item(item) => {
                    if item.is_separator() {
                        // SAFETY: hmenu is a live menu and separator requires no string.
                        unsafe { AppendMenuW(hmenu.as_raw(), MF_SEPARATOR, 0, ptr::null()) };
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

                    // SAFETY: hmenu is a live menu and wide_text is NUL-terminated.
                    if unsafe {
                        AppendMenuW(
                            hmenu.as_raw(),
                            flags,
                            item.id() as usize,
                            wide_text.as_ptr(),
                        )
                    } == 0
                    {
                        return Err("AppendMenuW failed to add item");
                    }
                }
                MenuEntry::SubMenu { text, menu } => {
                    // A failed build leaves nothing to free here; `hmenu` drops on the way out.
                    let raw = menu.build_native_menu()?;
                    if raw == 0 {
                        // A submenu from another back end (the trait default returns 0) has no
                        // native menu: appending `MF_POPUP` with a null menu would fail, so the
                        // item is skipped and the rest of the menu is still built.
                        eprintln!(
                            "ferrokit: submenu {text:?} has no native Win32 menu; it is left out"
                        );
                        continue;
                    }
                    let sub_hmenu = OwnedMenu::from_raw(raw as HMENU);
                    let wide_text: Vec<u16> =
                        text.encode_utf16().chain(std::iter::once(0)).collect();
                    // On failure both guards destroy their menu; on success the parent owns the
                    // submenu.
                    // SAFETY: hmenu and sub_hmenu are live menus and wide_text is NUL-terminated.
                    if unsafe {
                        AppendMenuW(
                            hmenu.as_raw(),
                            MF_POPUP | MF_STRING,
                            sub_hmenu.as_raw() as usize,
                            wide_text.as_ptr(),
                        )
                    } == 0
                    {
                        return Err("AppendMenuW failed to add submenu");
                    }
                    let _ = sub_hmenu.into_raw();
                }
            }
        }

        Ok(hmenu.into_raw())
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
        // SAFETY: `build_hmenu`'s contract is that Win32 menu calls run on this thread and that the
        // returned `HMENU` is destroyed: it is wrapped in `OwnedMenu` immediately, which destroys
        // it on drop (also on the early return). `self.hwnd` is the window handle given to
        // `Win32Menu::new`; `SetForegroundWindow`, `TrackPopupMenu` and `PostMessageW` validate it
        // and fail harmlessly for a null or stale one, and `TrackPopupMenu`'s window-owned-by-the-
        // calling-thread requirement is the caller's, as menus are shown from the UI thread
        // [inferred: thread of `exec_popup` callers not checked]. The `ptr::null()` `prcRect` is
        // documented as ignored. `hmenu` lives past `TrackPopupMenu` and is dropped only after it
        // returns.
        unsafe {
            let hmenu = match self.build_hmenu() {
                Ok(m) => OwnedMenu::from_raw(m),
                Err(_) => return None,
            };

            SetForegroundWindow(self.hwnd);

            // `screen_pos` is a logical desktop point; `TrackPopupMenu` takes physical pixels.
            let native_pos = crate::screen::logical_point_to_native(screen_pos);
            let cmd_id = TrackPopupMenu(
                hmenu.as_raw(),
                TPM_RIGHTBUTTON | TPM_RETURNCMD,
                native_pos.x,
                native_pos.y,
                0,
                self.hwnd,
                ptr::null(),
            ) as u32;

            PostMessageW(self.hwnd, WM_NULL, 0, 0);
            drop(hmenu);

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
        // SAFETY: `build_hmenu` requires Win32 menu calls on this thread and that the returned
        // `HMENU` be destroyed; `build_native_menu` hands that ownership to its caller as the raw
        // `isize` (the trait contract: a parent wraps it in `OwnedMenu`, see `build_hmenu`'s submenu
        // arm), and menu construction needs no window, only the UI thread the menu is used on
        // [inferred: callers' thread not checked].
        unsafe { self.build_hmenu().map(|h| h as isize) }
    }

    fn find_item(&self, id: u32) -> Option<Arc<dyn PlatformMenuItem>> {
        self.find_item_by_id(id)
    }

    fn dismiss(&self) {
        // SAFETY: `PostMessageW` takes the window handle by value and validates it, failing
        // harmlessly for a null or stale `self.hwnd` (set in `Win32Menu::new`); `WM_NULL` with zero
        // parameters carries no pointers.
        unsafe {
            PostMessageW(self.hwnd, WM_NULL, 0, 0);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use windows_sys::Win32::UI::WindowsAndMessaging::DestroyMenu;

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
        // SAFETY: the test runs on one thread (`build_hmenu`'s thread requirement); `hmenu` is the
        // popup just built, owned by the test, and is destroyed exactly once by `DestroyMenu`.
        unsafe {
            let hmenu = menu.build_hmenu().expect("failed to build HMENU");
            assert!(!hmenu.is_null());
            assert_ne!(DestroyMenu(hmenu), 0);
        }
    }

    /// A built submenu belongs to the parent after `AppendMenuW(MF_POPUP)`; destroying the
    /// parent destroys it, and the builder must not have destroyed it before.
    #[test]
    fn test_win32_menu_submenu_is_owned_by_the_built_parent() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{GetSubMenu, IsMenu};
        let mut sub = Win32Menu::new(ptr::null_mut());
        sub.add_action(201, "Inner");
        let mut menu = Win32Menu::new(ptr::null_mut());
        menu.add_submenu("Sub", Box::new(sub));

        // SAFETY: the test runs on one thread (`build_hmenu`'s thread requirement); `hmenu` is the
        // popup just built, destroyed exactly once by `DestroyMenu`; `GetSubMenu` and `IsMenu`
        // validate their handles, so querying `sub_hmenu` after its destruction is defined
        // (`IsMenu` returns 0, which the test asserts).
        unsafe {
            let hmenu = menu.build_hmenu().expect("failed to build HMENU");
            let sub_hmenu = GetSubMenu(hmenu, 0);
            assert!(!sub_hmenu.is_null());
            assert_ne!(
                IsMenu(sub_hmenu),
                0,
                "the builder must keep the submenu alive"
            );
            assert_ne!(DestroyMenu(hmenu), 0);
            assert_eq!(
                IsMenu(sub_hmenu),
                0,
                "the parent owns and destroys the submenu"
            );
        }
    }

    /// A submenu without a native menu (another back end, whose `build_native_menu` returns 0)
    /// is skipped; the remaining items are still built.
    #[test]
    fn test_win32_menu_skips_a_submenu_without_a_native_menu() {
        use crate::menu::CocoaMenu;
        use windows_sys::Win32::UI::WindowsAndMessaging::{GetMenuItemCount, GetMenuItemID};
        let mut menu = Win32Menu::new(ptr::null_mut());
        menu.add_action(1, "Before");
        menu.add_submenu("Foreign", Box::new(CocoaMenu::new()));
        menu.add_action(2, "After");
        // SAFETY: the test runs on one thread (`build_hmenu`'s thread requirement); `hmenu` is the
        // popup just built, destroyed exactly once by `DestroyMenu` after the queries.
        unsafe {
            let hmenu = menu.build_hmenu().expect("failed to build HMENU");
            assert_eq!(GetMenuItemCount(hmenu), 2);
            assert_eq!(GetMenuItemID(hmenu, 0), 1);
            assert_eq!(GetMenuItemID(hmenu, 1), 2);
            assert_ne!(DestroyMenu(hmenu), 0);
        }
    }
}
