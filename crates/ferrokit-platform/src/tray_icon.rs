// SPDX-License-Identifier: LGPL-3.0-only
use ferrokit_core::signal::Signal;
use ferrokit_gui::geometry::primitives::Point;
use ferrokit_gui::paint::Pixmap;
#[allow(unused_imports)]
use std::collections::HashMap;
#[allow(unused_imports)]
use std::ptr;
#[allow(unused_imports)]
use std::sync::{Mutex, Once};
#[cfg(windows)]
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
#[cfg(windows)]
use windows_sys::Win32::Graphics::Gdi::{
    CreateBitmap, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
};
#[cfg(windows)]
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
#[cfg(windows)]
use windows_sys::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIN_SELECT,
    NOTIFYICONDATAW,
};
#[cfg(windows)]
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIconIndirect, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon,
    DestroyMenu, DestroyWindow, GetCursorPos, RegisterClassExW, RegisterWindowMessageW,
    SetForegroundWindow, TrackPopupMenuEx, HICON, HMENU, ICONINFO, MF_CHECKED, MF_DISABLED,
    MF_GRAYED, MF_POPUP, MF_SEPARATOR, MF_STRING, MF_UNCHECKED, TPM_LEFTALIGN, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, WM_CONTEXTMENU, WM_LBUTTONDBLCLK, WM_LBUTTONUP, WM_RBUTTONUP, WNDCLASSEXW,
};

pub const WM_TRAY_CALLBACK: u32 = 0x8000 + 101;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuItem {
    pub id: u32,
    pub text: String,
    pub enabled: bool,
    pub checked: Option<bool>,
    pub is_separator: bool,
    pub submenu: Option<Box<Menu>>,
}

impl MenuItem {
    pub fn action(id: u32, text: impl Into<String>) -> Self {
        Self {
            id,
            text: text.into(),
            enabled: true,
            checked: None,
            is_separator: false,
            submenu: None,
        }
    }

    pub fn checkable(id: u32, text: impl Into<String>, checked: bool) -> Self {
        Self {
            id,
            text: text.into(),
            enabled: true,
            checked: Some(checked),
            is_separator: false,
            submenu: None,
        }
    }

    pub fn separator() -> Self {
        Self {
            id: 0,
            text: String::new(),
            enabled: false,
            checked: None,
            is_separator: true,
            submenu: None,
        }
    }

    pub fn submenu(text: impl Into<String>, menu: Menu) -> Self {
        Self {
            id: 0,
            text: text.into(),
            enabled: true,
            checked: None,
            is_separator: false,
            submenu: Some(Box::new(menu)),
        }
    }
}

#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct Menu {
    pub items: Vec<MenuItem>,
}

impl Menu {
    pub fn new() -> Self {
        Self { items: Vec::new() }
    }

    pub fn add_action(&mut self, id: u32, text: impl Into<String>) -> &mut Self {
        self.items.push(MenuItem::action(id, text));
        self
    }

    pub fn add_checkable(&mut self, id: u32, text: impl Into<String>, checked: bool) -> &mut Self {
        self.items.push(MenuItem::checkable(id, text, checked));
        self
    }

    pub fn add_separator(&mut self) -> &mut Self {
        self.items.push(MenuItem::separator());
        self
    }
    pub fn add_submenu(&mut self, text: impl Into<String>, submenu: Menu) -> &mut Self {
        self.items.push(MenuItem::submenu(text, submenu));
        self
    }


    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    #[cfg(windows)]
    /// Creates a native Win32 HMENU handle
    ///
    /// # Safety
    /// The caller must destroy this handle with `DestroyMenu` when appropriate to prevent resource leaks.
    pub unsafe fn create_native_hmenu(&self) -> Result<HMENU, &'static str> {
        let hmenu = CreatePopupMenu();
        if hmenu.is_null() {
            return Err("CreatePopupMenu failed");
        }

        for item in &self.items {
            if let Some(sub) = &item.submenu {
                let sub_hmenu = match sub.create_native_hmenu() {
                    Ok(h) => h,
                    Err(e) => {
                        DestroyMenu(hmenu);
                        return Err(e);
                    }
                };
                let wide_text: Vec<u16> =
                    item.text.encode_utf16().chain(std::iter::once(0)).collect();
                let mut sub_flags = MF_POPUP | MF_STRING;
                if !item.enabled {
                    sub_flags |= MF_GRAYED | MF_DISABLED;
                }
                if AppendMenuW(
                    hmenu,
                    sub_flags,
                    sub_hmenu as usize,
                    wide_text.as_ptr(),
                ) == 0
                {
                    DestroyMenu(sub_hmenu);
                    DestroyMenu(hmenu);
                    return Err("AppendMenuW failed to add submenu");
                }
                continue;
            }

            if item.is_separator {
                AppendMenuW(hmenu, MF_SEPARATOR, 0, ptr::null());
                continue;
            }

            let mut flags = MF_STRING;
            if !item.enabled {
                flags |= MF_GRAYED | MF_DISABLED;
            }
            if let Some(c) = item.checked {
                if c {
                    flags |= MF_CHECKED;
                } else {
                    flags |= MF_UNCHECKED;
                }
            }

            let wide_text: Vec<u16> = item.text.encode_utf16().chain(std::iter::once(0)).collect();

            if AppendMenuW(hmenu, flags, item.id as usize, wide_text.as_ptr()) == 0 {
                DestroyMenu(hmenu);
                return Err("AppendMenuW failed");
            }
        }

        Ok(hmenu)
    }

    #[cfg(windows)]
    pub fn track_popup_at(
        &self,
        hwnd: HWND,
        screen_x: i32,
        screen_y: i32,
    ) -> Result<Option<u32>, &'static str> {
        unsafe {
            let hmenu = self.create_native_hmenu()?;

            SetForegroundWindow(hwnd);

            let selected = TrackPopupMenuEx(
                hmenu,
                TPM_LEFTALIGN | TPM_RIGHTBUTTON | TPM_RETURNCMD,
                screen_x,
                screen_y,
                hwnd,
                ptr::null(),
            );

            DestroyMenu(hmenu);

            if selected > 0 {
                Ok(Some(selected as u32))
            } else {
                Ok(None)
            }
        }
    }

    #[cfg(windows)]
    pub fn track_popup(&self, hwnd: HWND) -> Result<Option<u32>, &'static str> {
        let mut pt = POINT { x: 0, y: 0 };
        unsafe {
            GetCursorPos(&mut pt);
        }
        self.track_popup_at(hwnd, pt.x, pt.y)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayActivation {
    Trigger,
    DoubleClick,
    Context,
    MiddleClick,
}

#[cfg(windows)]
static TRAY_WINDOW_CLASS_ONCE: Once = Once::new();
#[cfg(windows)]
static TRAY_INSTANCES: Mutex<Option<HashMap<isize, usize>>> = Mutex::new(None);
#[cfg(windows)]
const TRAY_WINDOW_CLASS_NAME: &[u16] = &[
    'Q' as u16, 't' as u16, 'R' as u16, 'u' as u16, 's' as u16, 't' as u16, 'T' as u16, 'r' as u16,
    'a' as u16, 'y' as u16, 'W' as u16, 'i' as u16, 'n' as u16, 0,
];

/// Whether the tray registered for `hwnd` at `addr` is still alive. A slot may drop the icon
/// (a "Quit" entry) while the window procedure is still inside one of its signals.
#[cfg(windows)]
fn tray_is_alive(hwnd: HWND, addr: usize) -> bool {
    TRAY_INSTANCES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|m| m.get(&(hwnd as isize)).copied())
        == Some(addr)
}

#[cfg(windows)]
unsafe fn tray_window_proc_inner(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let tray_ptr = {
        let guard = TRAY_INSTANCES.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .as_ref()
            .and_then(|m| m.get(&(hwnd as isize)).copied())
    };

    if let Some(addr) = tray_ptr {
        // Shared access only: this procedure re-enters itself (the context menu's modal loop
        // pumps this very window), so a `&mut TrayIcon` here would alias. Nothing below mutates
        // the icon except through `Cell`s, and no reference is kept across an emit: a slot may
        // drop the icon, so the pointer is re-validated before every later use.
        let tray = || &*(addr as *const TrayIcon);
        if msg == tray().taskbar_created_msg {
            if tray().visible {
                let _ = tray().send_notify_icon_message(NIM_ADD);
            }
            return 0;
        } else if msg == WM_TRAY_CALLBACK {
            let event_type = (lparam & 0xFFFF) as u32;
            match event_type {
                NIN_SELECT | WM_LBUTTONUP => {
                    // The release that ends a double click is not a click (Qt:
                    // `m_ignoreNextMouseRelease`, qwindowssystemtrayicon.cpp).
                    if tray().ignore_next_release.replace(false) {
                        return 0;
                    }
                    let activated = tray().on_activated.clone();
                    activated.emit(&TrayActivation::Trigger);
                    return 0;
                }
                WM_LBUTTONDBLCLK => {
                    tray().ignore_next_release.set(true);
                    let activated = tray().on_activated.clone();
                    activated.emit(&TrayActivation::DoubleClick);
                    return 0;
                }
                WM_RBUTTONUP | WM_CONTEXTMENU => {
                    let mut pt = POINT { x: 0, y: 0 };
                    GetCursorPos(&mut pt);
                    let pos = crate::screen::native_cursor_to_logical(Point::new(pt.x, pt.y));
                    let requested = tray().on_context_menu_requested.clone();
                    let activated = tray().on_activated.clone();
                    let has_menu = tray().menu.is_some();
                    requested.emit(&pos);
                    if !tray_is_alive(hwnd, addr) {
                        return 0;
                    }
                    activated.emit(&TrayActivation::Context);
                    if has_menu && tray_is_alive(hwnd, addr) {
                        tray().show_context_menu_at(pos);
                    }
                    return 0;
                }
                0x0405 => {
                    // NIN_BALLOONUSERCLICK (aligned with QWindowsSystemTrayIcon)
                    let clicked = tray().on_message_clicked.clone();
                    clicked.emit(&());
                    return 0;
                }
                _ => {}
            }
        } else if msg == 0x0218 {
            // WM_POWERBROADCAST. Qt has no suspend/resume event (it only handles PBT_POWERSETTINGCHANGE
            // in qwindowscontext.cpp); this is a ferrokit extension.
            let power_event = match wparam as u32 {
                0x0012 | 0x0007 => Some(crate::window_system_interface::PowerEvent::Resume),
                0x0004 => Some(crate::window_system_interface::PowerEvent::Suspend),
                _ => None,
            };
            if let Some(event) = power_event {
                let power = tray().on_power_event.clone();
                power.emit(&event);
            }
            return 1;
        }
    }

    DefWindowProcW(hwnd, msg, wparam, lparam)
}

/// Tray window procedure. Unwinding out of an `extern "system"` callback aborts the process, so a
/// raising handler is contained and the message falls back to the default.
#[cfg(windows)]
unsafe extern "system" fn tray_window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        tray_window_proc_inner(hwnd, msg, wparam, lparam)
    })) {
        Ok(result) => result,
        Err(_) => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

#[cfg(windows)]
fn ensure_tray_window_class_registered() {
    TRAY_WINDOW_CLASS_ONCE.call_once(|| unsafe {
        let h_instance = GetModuleHandleW(ptr::null());
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: 0,
            lpfnWndProc: Some(tray_window_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: h_instance,
            hIcon: ptr::null_mut(),
            hCursor: ptr::null_mut(),
            hbrBackground: ptr::null_mut(),
            lpszMenuName: ptr::null(),
            lpszClassName: TRAY_WINDOW_CLASS_NAME.as_ptr(),
            hIconSm: ptr::null_mut(),
        };
        RegisterClassExW(&wc);
    });
}

#[cfg(windows)]
pub struct TrayIcon {
    hwnd: HWND,
    hicon: HICON,
    /// Whether this icon created `hicon` (and so destroys it); an `HICON` passed to `new` or
    /// `set_icon` stays the caller's.
    owns_icon: bool,
    tooltip: String,
    visible: bool,
    taskbar_created_msg: u32,
    /// Set by a double click: the release that ends it must not count as a click.
    ignore_next_release: std::cell::Cell<bool>,
    menu: Option<Box<dyn crate::menu::PlatformMenu>>,

    pub on_activated: Signal<TrayActivation>,
    pub on_menu_action: Signal<u32>,
    pub on_message_clicked: Signal<()>,
    pub on_power_event: Signal<crate::window_system_interface::PowerEvent>,
    pub on_context_menu_requested: Signal<Point>,
}

// Deliberately neither `Send` nor `Sync` (raw `HWND`/`HICON`): the tray window, its window
// procedure (which reaches the icon through a raw pointer) and `DestroyWindow` in `Drop` all
// belong to the thread that created the icon.
#[cfg(windows)]
impl TrayIcon {
    pub fn new(tooltip: impl Into<String>, hicon: HICON) -> Result<Box<Self>, &'static str> {
        ensure_tray_window_class_registered();

        let taskbar_created_msg = unsafe {
            let taskbar_msg_name: Vec<u16> = "TaskbarCreated"
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            RegisterWindowMessageW(taskbar_msg_name.as_ptr())
        };

        let hwnd = unsafe {
            CreateWindowExW(
                0,
                TRAY_WINDOW_CLASS_NAME.as_ptr(),
                ptr::null(),
                0,
                0,
                0,
                0,
                0,
                ptr::null_mut(),
                ptr::null_mut(),
                GetModuleHandleW(ptr::null()),
                ptr::null(),
            )
        };

        if hwnd.is_null() {
            return Err("failed to create tray message window");
        }

        let mut tray = Box::new(Self {
            hwnd,
            hicon,
            owns_icon: false,
            tooltip: tooltip.into(),
            visible: false,
            taskbar_created_msg,
            ignore_next_release: std::cell::Cell::new(false),
            menu: None,
            on_activated: Signal::new(),
            on_menu_action: Signal::new(),
            on_message_clicked: Signal::new(),
            on_power_event: Signal::new(),
            on_context_menu_requested: Signal::new(),
        });

        // Register into global HWND router table
        {
            let mut guard = TRAY_INSTANCES.lock().unwrap_or_else(|e| e.into_inner());
            if guard.is_none() {
                *guard = Some(HashMap::new());
            }
            guard
                .as_mut()
                .unwrap()
                .insert(hwnd as isize, &mut *tray as *mut TrayIcon as usize);
        }

        Ok(tray)
    }

    pub fn from_pixmap(tooltip: impl Into<String>, pixmap: &Pixmap) -> Result<Box<Self>, &'static str> {
        let hicon = Self::create_hicon_from_pixmap(pixmap)?;
        match Self::new(tooltip, hicon) {
            Ok(mut tray) => {
                tray.owns_icon = true;
                Ok(tray)
            }
            Err(e) => {
                Self::destroy_hicon(hicon);
                Err(e)
            }
        }
    }

    pub fn set_menu(&mut self, menu: Box<dyn crate::menu::PlatformMenu>) {
        self.menu = Some(menu);
    }

    pub fn menu(&self) -> Option<&dyn crate::menu::PlatformMenu> {
        self.menu.as_deref()
    }

    pub fn menu_mut(&mut self) -> Option<&mut (dyn crate::menu::PlatformMenu + 'static)> {
        self.menu.as_deref_mut()
    }

    fn send_notify_icon_message(&self, msg: u32) -> Result<(), &'static str> {
        let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = self.hwnd;
        nid.uID = 1;

        if msg != NIM_DELETE {
            nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
            nid.uCallbackMessage = WM_TRAY_CALLBACK;
            nid.hIcon = self.hicon;

            let wide_tip: Vec<u16> = self.tooltip.encode_utf16().collect();
            let copy_len = wide_tip.len().min(127);
            nid.szTip[..copy_len].copy_from_slice(&wide_tip[..copy_len]);
            nid.szTip[copy_len] = 0;
        }

        let res = unsafe { Shell_NotifyIconW(msg, &nid) };
        if res == 0 {
            let tray_wnd = unsafe {
                let cls: Vec<u16> = "Shell_TrayWnd"
                    .encode_utf16()
                    .chain(std::iter::once(0))
                    .collect();
                windows_sys::Win32::UI::WindowsAndMessaging::FindWindowW(cls.as_ptr(), ptr::null())
            };
            if tray_wnd.is_null() {
                return Ok(());
            }
            return Err("Shell_NotifyIconW call failed");
        }

        if msg == windows_sys::Win32::UI::Shell::NIM_ADD {
            nid.Anonymous.uVersion = windows_sys::Win32::UI::Shell::NOTIFYICON_VERSION_4;
            unsafe {
                Shell_NotifyIconW(windows_sys::Win32::UI::Shell::NIM_SETVERSION, &nid);
            }
        }
        Ok(())
    }

    pub fn show(&mut self) -> Result<(), &'static str> {
        if self.visible {
            return Ok(());
        }
        self.send_notify_icon_message(NIM_ADD)?;
        self.visible = true;
        Ok(())
    }

    pub fn hide(&mut self) -> Result<(), &'static str> {
        if !self.visible {
            return Ok(());
        }
        self.send_notify_icon_message(NIM_DELETE)?;
        self.visible = false;
        Ok(())
    }

    pub fn set_tooltip(&mut self, tooltip: impl Into<String>) -> Result<(), &'static str> {
        self.tooltip = tooltip.into();
        if self.visible {
            self.send_notify_icon_message(NIM_MODIFY)?;
        }
        Ok(())
    }

    /// Shows `hicon`, which stays owned by the caller (it is not destroyed with the tray icon).
    pub fn set_icon(&mut self, hicon: HICON) -> Result<(), &'static str> {
        self.replace_icon(hicon, false)
    }

    fn replace_icon(&mut self, hicon: HICON, owned: bool) -> Result<(), &'static str> {
        let old = std::mem::replace(&mut self.hicon, hicon);
        let old_owned = std::mem::replace(&mut self.owns_icon, owned);
        let result = if self.visible {
            self.send_notify_icon_message(NIM_MODIFY)
        } else {
            Ok(())
        };
        if old_owned && old != hicon {
            Self::destroy_hicon(old);
        }
        result
    }

    pub fn icon(&self) -> HICON {
        self.hicon
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    pub fn show_message(
        &mut self,
        title: &str,
        msg: &str,
        icon: crate::platform_tray::TrayMessageIcon,
        duration_ms: u32,
    ) -> Result<(), &'static str> {
        let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = self.hwnd;
        nid.uID = 1;
        const NIF_INFO: u32 = 0x00000010;
        nid.uFlags = NIF_INFO;

        let wide_msg: Vec<u16> = msg.encode_utf16().collect();
        let msg_len = wide_msg.len().min(255);
        nid.szInfo[..msg_len].copy_from_slice(&wide_msg[..msg_len]);
        nid.szInfo[msg_len] = 0;

        let wide_title: Vec<u16> = title.encode_utf16().collect();
        let title_len = wide_title.len().min(63);
        nid.szInfoTitle[..title_len].copy_from_slice(&wide_title[..title_len]);
        nid.szInfoTitle[title_len] = 0;

        nid.Anonymous.uTimeout = if duration_ms == 0 { 10000 } else { duration_ms };

        const NIIF_NONE: u32 = 0x00000000;
        const NIIF_INFO: u32 = 0x00000001;
        const NIIF_WARNING: u32 = 0x00000002;
        const NIIF_ERROR: u32 = 0x00000003;

        nid.dwInfoFlags = match icon {
            crate::platform_tray::TrayMessageIcon::NoIcon => NIIF_NONE,
            crate::platform_tray::TrayMessageIcon::Information => NIIF_INFO,
            crate::platform_tray::TrayMessageIcon::Warning => NIIF_WARNING,
            crate::platform_tray::TrayMessageIcon::Critical => NIIF_ERROR,
        };

        let res = unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) };
        if res == 0 {
            let tray_wnd = unsafe {
                let cls: Vec<u16> = "Shell_TrayWnd"
                    .encode_utf16()
                    .chain(std::iter::once(0))
                    .collect();
                windows_sys::Win32::UI::WindowsAndMessaging::FindWindowW(cls.as_ptr(), ptr::null())
            };
            if tray_wnd.is_null() {
                return Ok(());
            }
            return Err("Shell_NotifyIconW (show_message) failed");
        }
        Ok(())
    }

    pub fn show_context_menu(&self) {
        let mut pt = POINT { x: 0, y: 0 };
        unsafe {
            GetCursorPos(&mut pt);
        }
        self.show_context_menu_at(crate::screen::native_cursor_to_logical(Point::new(pt.x, pt.y)));
    }

    pub fn show_context_menu_at(&self, screen_pos: Point) {
        if let Some(menu) = &self.menu {
            // The menu's modal loop runs slots, one of which may drop this icon: emit through a
            // handle that does not live in `self`.
            let action = self.on_menu_action.clone();
            if let Some(cmd_id) = menu.exec_popup(screen_pos) {
                action.emit(&cmd_id);
            }
        }
    }

    pub fn create_hicon_from_pixmap(pixmap: &Pixmap) -> Result<HICON, &'static str> {
        let w = pixmap.physical_width() as i32;
        let h = pixmap.physical_height() as i32;

        if w <= 0 || h <= 0 {
            return Err("invalid pixmap size");
        }

        unsafe {
            let mut bmi: BITMAPINFO = std::mem::zeroed();
            bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            bmi.bmiHeader.biWidth = w;
            bmi.bmiHeader.biHeight = -h;
            bmi.bmiHeader.biPlanes = 1;
            bmi.bmiHeader.biBitCount = 32;
            bmi.bmiHeader.biCompression = BI_RGB;

            let src_data = pixmap.data();
            let pixel_count = (w as usize) * (h as usize);
            if src_data.len() < pixel_count * 4 {
                return Err("pixmap data shorter than its size");
            }

            let hdc = CreateCompatibleDC(ptr::null_mut());
            if hdc.is_null() {
                return Err("CreateCompatibleDC failed");
            }
            let mut bits_ptr: *mut u8 = ptr::null_mut();
            let hbm_color = CreateDIBSection(
                hdc,
                &bmi,
                DIB_RGB_COLORS,
                &mut bits_ptr as *mut _ as *mut *mut std::ffi::c_void,
                ptr::null_mut(),
                0,
            );

            DeleteDC(hdc);

            if hbm_color.is_null() || bits_ptr.is_null() {
                if !hbm_color.is_null() {
                    DeleteObject(hbm_color);
                }
                return Err("CreateDIBSection failed");
            }

            for i in 0..pixel_count {
                let r = src_data[i * 4];
                let g = src_data[i * 4 + 1];
                let b = src_data[i * 4 + 2];
                let a = src_data[i * 4 + 3];

                *bits_ptr.add(i * 4) = b;
                *bits_ptr.add(i * 4 + 1) = g;
                *bits_ptr.add(i * 4 + 2) = r;
                *bits_ptr.add(i * 4 + 3) = a;
            }

            let hbm_mask = CreateBitmap(w, h, 1, 1, ptr::null());
            if hbm_mask.is_null() {
                DeleteObject(hbm_color);
                return Err("CreateBitmap failed");
            }

            let icon_info = ICONINFO {
                fIcon: 1,
                xHotspot: 0,
                yHotspot: 0,
                hbmMask: hbm_mask,
                hbmColor: hbm_color,
            };

            let hicon = CreateIconIndirect(&icon_info);

            DeleteObject(hbm_color);
            DeleteObject(hbm_mask);

            if hicon.is_null() {
                return Err("CreateIconIndirect failed");
            }

            Ok(hicon)
        }
    }

    pub fn destroy_hicon(hicon: HICON) {
        if !hicon.is_null() {
            unsafe {
                DestroyIcon(hicon);
            }
        }
    }
}
#[cfg(windows)]
impl crate::platform_tray::PlatformTrayIcon for TrayIcon {
    fn set_icon(&mut self, pixmap: &Pixmap) -> Result<(), &'static str> {
        let new_hicon = Self::create_hicon_from_pixmap(pixmap)?;
        // The old icon is destroyed only if this tray created it (`replace_icon`).
        self.replace_icon(new_hicon, true)
    }

    fn set_tooltip(&mut self, tooltip: &str) -> Result<(), &'static str> {
        self.set_tooltip(tooltip)
    }

    fn set_menu(&mut self, menu: Box<dyn crate::menu::PlatformMenu>) {
        self.set_menu(menu);
    }

    fn show(&mut self) -> Result<(), &'static str> {
        self.show()
    }

    fn hide(&mut self) -> Result<(), &'static str> {
        self.hide()
    }

    fn show_message(
        &mut self,
        title: &str,
        msg: &str,
        icon: crate::platform_tray::TrayMessageIcon,
        duration_ms: u32,
    ) -> Result<(), &'static str> {
        self.show_message(title, msg, icon, duration_ms)
    }

    fn supports_messages(&self) -> bool {
        true
    }
}

#[cfg(windows)]
impl Drop for TrayIcon {
    fn drop(&mut self) {
        let _ = self.hide();

        {
            let mut guard = TRAY_INSTANCES.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(map) = guard.as_mut() {
                map.remove(&(self.hwnd as isize));
            }
        }

        if !self.hwnd.is_null() {
            unsafe {
                DestroyWindow(self.hwnd);
            }
        }
        if self.owns_icon {
            Self::destroy_hicon(self.hicon);
        }
    }
}

#[cfg(not(windows))]
pub struct TrayIcon {
    inner: Box<dyn crate::platform_tray::PlatformTrayIcon>,
    pub on_menu_action: Signal<u32>,
    pub on_activated: Signal<TrayActivation>,
    pub on_context_menu_requested: Signal<Point>,
}
#[cfg(not(windows))]
impl TrayIcon {
    pub fn from_pixmap(tooltip: impl Into<String>, pixmap: &Pixmap) -> Result<Box<Self>, &'static str> {
        let title_str = tooltip.into();
        let mut inner = crate::integration::platform().create_tray_icon(&title_str, pixmap)?;
        let _ = inner.show();
        Ok(Box::new(Self {
            inner,
            on_menu_action: Signal::new(),
            on_activated: Signal::new(),
            on_context_menu_requested: Signal::new(),
        }))
    }

    pub fn hwnd(&self) -> isize {
        0
    }

    pub fn set_menu(&mut self, menu: Box<dyn crate::menu::PlatformMenu>) {
        self.inner.set_menu(menu);
    }

    pub fn set_tooltip(&mut self, tooltip: &str) -> Result<(), &'static str> {
        self.inner.set_tooltip(tooltip)
    }

    pub fn show(&mut self) -> Result<(), &'static str> {
        self.inner.show()
    }

    pub fn hide(&mut self) -> Result<(), &'static str> {
        self.inner.hide()
    }

    pub fn show_message(
        &mut self,
        title: &str,
        msg: &str,
        icon: crate::platform_tray::TrayMessageIcon,
        duration_ms: u32,
    ) -> Result<(), &'static str> {
        self.inner.show_message(title, msg, icon, duration_ms)
    }
}
#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::menu::PlatformMenu;
    use ferrokit_gui::tiny_skia::Color;
    #[test]
    fn test_menu_item_constructors() {
        let act = MenuItem::action(1, "Open Main Window");
        assert_eq!(act.id, 1);
        assert_eq!(act.text, "Open Main Window");
        assert!(act.enabled);
        assert_eq!(act.checked, None);
        assert!(!act.is_separator);

        let chk = MenuItem::checkable(2, "Click Through", true);
        assert_eq!(chk.id, 2);
        assert_eq!(chk.text, "Click Through");
        assert!(chk.enabled);
        assert_eq!(chk.checked, Some(true));
        assert!(!chk.is_separator);

        let sep = MenuItem::separator();
        assert_eq!(sep.id, 0);
        assert!(!sep.enabled);
        assert_eq!(sep.checked, None);
        assert!(sep.is_separator);
    }

    #[test]
    fn test_menu_builder_and_hmenu_lifecycle() {
        let mut menu = Menu::new();
        assert!(menu.is_empty());

        menu.add_action(10, "Refresh")
            .add_checkable(11, "Always On Top", false)
            .add_separator()
            .add_action(99, "Quit");

        assert_eq!(menu.len(), 4);
        assert!(!menu.is_empty());
        assert_eq!(menu.items[0].id, 10);
        assert_eq!(menu.items[1].checked, Some(false));
        assert!(menu.items[2].is_separator);
        assert_eq!(menu.items[3].id, 99);

        // Test native Win32 HMENU creation and safe destruction
        unsafe {
            let hmenu = menu
                .create_native_hmenu()
                .expect("failed to create native menu");
            assert!(!hmenu.is_null());
            assert_ne!(DestroyMenu(hmenu), 0);
        }
    }

    #[test]
    fn test_create_hicon_from_pixmap() {
        let mut pixmap = Pixmap::new(32, 32).expect("failed to create pixmap");
        pixmap.fill(Color::from_rgba8(255, 128, 0, 200));

        let hicon = TrayIcon::create_hicon_from_pixmap(&pixmap).expect("failed to create HICON");
        assert!(!hicon.is_null());

        TrayIcon::destroy_hicon(hicon);
    }

    #[test]
    fn test_tray_icon_lifecycle() {
        let mut pixmap = Pixmap::new(16, 16).expect("failed to create pixmap");
        pixmap.fill(Color::from_rgba8(0, 180, 255, 255));

        let hicon = TrayIcon::create_hicon_from_pixmap(&pixmap).expect("failed to create HICON");

        let mut tray =
            TrayIcon::new("QtRust Monitor", hicon).expect("failed to create TrayIcon instance");
        assert!(!tray.hwnd().is_null());
        assert!(!tray.is_visible());

        // Configure abstract menu
        let mut win_menu = Box::new(crate::menu::Win32Menu::new(tray.hwnd()));
        win_menu.add_action(1, "Settings");
        win_menu.add_action(2, "Quit");
        tray.set_menu(win_menu);
        assert!(tray.menu().is_some());

        // Test show and hide
        assert!(tray.show().is_ok());
        assert!(tray.is_visible());

        assert!(tray.set_tooltip("QtRust Running").is_ok());

        assert!(tray.hide().is_ok());
        assert!(!tray.is_visible());

        TrayIcon::destroy_hicon(hicon);
    }
    #[test]
    fn test_tray_signals_and_window_proc_dispatch() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        use windows_sys::Win32::UI::WindowsAndMessaging::SendMessageW;

        let mut pixmap = Pixmap::new(16, 16).expect("failed to create pixmap");
        pixmap.fill(Color::from_rgba8(100, 200, 50, 255));
        let hicon = TrayIcon::create_hicon_from_pixmap(&pixmap).expect("failed to create HICON");

        let tray = TrayIcon::new("Signal Test", hicon).expect("failed to create TrayIcon");

        let activated_called = Arc::new(AtomicBool::new(false));
        let activated_clone = activated_called.clone();
        tray.on_activated.connect(move |reason| {
            if *reason == TrayActivation::Trigger {
                activated_clone.store(true, Ordering::SeqCst);
            }
        });

        // Simulate Win32 taskbar callback with WM_LBUTTONUP
        unsafe {
            SendMessageW(
                tray.hwnd(),
                WM_TRAY_CALLBACK,
                0,
                windows_sys::Win32::UI::WindowsAndMessaging::WM_LBUTTONUP as isize,
            );
        }

        assert!(
            activated_called.load(Ordering::SeqCst),
            "receiving WM_LBUTTONUP should trigger on_activated signal"
        );
        // Simulate Windows 7+ NIN_SELECT notification triggering Trigger
        activated_called.store(false, Ordering::SeqCst);
        unsafe {
            SendMessageW(tray.hwnd(), WM_TRAY_CALLBACK, 0, NIN_SELECT as isize);
        }
        assert!(
            activated_called.load(Ordering::SeqCst),
            "receiving NIN_SELECT should trigger on_activated signal"
        );

        // Simulate explorer.exe crash and restart broadcast (TaskbarCreated)
        unsafe {
            SendMessageW(tray.hwnd(), tray.taskbar_created_msg, 0, 0);
        }

        TrayIcon::destroy_hicon(hicon);
    }
}
