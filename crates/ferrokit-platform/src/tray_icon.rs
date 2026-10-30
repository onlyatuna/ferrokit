// SPDX-License-Identifier: LGPL-3.0-only
use ferrokit_core::signal::Signal;
use ferrokit_gui::geometry::primitives::Point;
#[cfg(windows)]
use ferrokit_gui::geometry::primitives::Rect;
#[cfg(windows)]
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
    Shell_NotifyIconGetRect, Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP,
    NIF_TIP, NOTIFYICONIDENTIFIER, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIN_SELECT,
    NOTIFYICONDATAW,
};
#[cfg(windows)]
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIconIndirect, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon,
    DestroyMenu, DestroyWindow, GetCursorPos, RegisterClassExW, RegisterWindowMessageW,
    SetForegroundWindow, TrackPopupMenuEx, HICON, HMENU, ICONINFO, MF_CHECKED, MF_DISABLED,
    MF_GRAYED, MF_POPUP, MF_SEPARATOR, MF_STRING, MF_UNCHECKED, TPM_LEFTALIGN, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, WM_CLOSE, WM_CONTEXTMENU, WM_LBUTTONDBLCLK, WM_MBUTTONUP, WNDCLASSEXW,
};

/// `NIN_KEYSELECT` (`NIN_SELECT | NINF_KEY`): the icon was chosen with the keyboard.
#[cfg(windows)]
const NIN_KEYSELECT: u32 = NIN_SELECT | 0x1;
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

/// `QSystemTrayIcon::ActivationReason`, with Qt's values (qsystemtrayicon.h).
///
/// Which reasons arrive depends on the platform engine, as in Qt:
/// - Windows (`QWindowsSystemTrayIcon::winEvent`): `Trigger` on a left click (`NIN_SELECT`),
///   `DoubleClick`, `Context` (`WM_CONTEXTMENU`) and `MiddleClick`; never `Unknown`.
/// - macOS (`QCocoaSystemTrayIcon::emitActivated`): a status item click is classified by
///   `[NSApp currentEvent]`: click count 2 is `DoubleClick`, otherwise the middle button gives
///   `MiddleClick`, the right button `Context` and anything else `Trigger`; `Unknown` is only
///   the initial value Qt assigns before classifying. Not run on macOS.
/// - Linux (`QStatusNotifierItemAdaptor`): the StatusNotifierItem methods `Activate`,
///   `ContextMenu` and `SecondaryActivate` give `Trigger`, `Context` and `MiddleClick`; there is
///   no `DoubleClick`. The item also answers the host's `org.freedesktop.DBus.Properties`
///   reads and confirms its registration from the watcher's reply. Not run on Linux.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum TrayActivation {
    Unknown = 0,
    Context = 1,
    DoubleClick = 2,
    Trigger = 3,
    MiddleClick = 4,
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
            tray().rebuild_icon_from_source();
            if tray().visible {
                let _ = tray().send_notify_icon_message(NIM_ADD);
            }
            return 0;
        } else if msg == WM_TRAY_CALLBACK {
            let event_type = (lparam & 0xFFFF) as u32;
            match event_type {
                // Under NOTIFYICON_VERSION_4 the shell sends `NIN_SELECT` for a click and
                // `WM_CONTEXTMENU` for the context menu; the raw button-up messages accompany them
                // and Qt (`QWindowsSystemTrayIcon::winEvent`) does not act on those.
                NIN_SELECT | NIN_KEYSELECT => {
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
                WM_CONTEXTMENU => {
                    // Under NOTIFYICON_VERSION_4 `wParam` holds the anchor point in physical
                    // desktop pixels (Qt: `GET_X_LPARAM(message.wParam)`), not where the cursor
                    // is by now, and not the cursor at all for a menu opened with the keyboard.
                    let native = Point::new(
                        (wparam & 0xFFFF) as u16 as i16 as i32,
                        ((wparam >> 16) & 0xFFFF) as u16 as i16 as i32,
                    );
                    let pos = crate::screen::native_cursor_to_logical(native);
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
                WM_MBUTTONUP => {
                    // `QWindowsSystemTrayIcon::winEvent`: the only raw button message it acts on.
                    let activated = tray().on_activated.clone();
                    activated.emit(&TrayActivation::MiddleClick);
                    return 0;
                }
                _ => {}
            }
        } else if msg == WM_CLOSE {
            // `QWindowsSystemTrayIcon::winEvent`: the tray window is closed (session end, a
            // `taskkill` without force) -> `handleApplicationTermination`, which sends the
            // application a `Quit`; Qt then lets `DefWindowProc` destroy the window.
            ferrokit_core::application::CoreApplication::quit();
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

/// An icon for a balloon message and whether this code owns it. `LoadIcon` hands out shared
/// handles that belong to the system and must not be destroyed; an icon made by
/// `SHDefExtractIcon` is ours to free.
#[cfg(windows)]
struct BalloonIcon {
    hicon: HICON,
    owned: bool,
}

#[cfg(windows)]
impl BalloonIcon {
    /// Frees the icon if it is owned; a shared system icon is left alone.
    fn release(self) {
        if self.owned {
            unsafe { DestroyIcon(self.hicon) };
        }
    }
}

#[cfg(windows)]
pub struct TrayIcon {
    hwnd: HWND,
    /// Cells: `TaskbarCreated` replaces the icon from the window procedure, which only has
    /// shared access (see `tray_window_proc_inner`).
    hicon: std::cell::Cell<HICON>,
    /// Whether this icon created `hicon` (and so destroys it); an `HICON` passed to `new` or
    /// `set_icon` stays the caller's.
    owns_icon: std::cell::Cell<bool>,
    /// The icon `hicon` was made from, when the tray was given an [`Icon`]
    /// (`QWindowsSystemTrayIcon::m_icon`): `TaskbarCreated` builds the `HICON` again from it.
    source: Option<ferrokit_gui::image::Icon>,
    /// The icon of the last balloon message, kept as `QWindowsSystemTrayIcon::m_hMessageIcon`.
    balloon_icon: Option<BalloonIcon>,
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
        // Let `TaskbarCreated` through the UIPI filter, so a tray in an elevated process still
        // hears an Explorer that was restarted without elevation
        // (`QWindowsSystemTrayIcon::ensureInstalled`).
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::ChangeWindowMessageFilterEx(
                hwnd,
                taskbar_created_msg,
                windows_sys::Win32::UI::WindowsAndMessaging::MSGFLT_ALLOW,
                ptr::null_mut(),
            );
        }

        let mut tray = Box::new(Self {
            hwnd,
            hicon: std::cell::Cell::new(hicon),
            owns_icon: std::cell::Cell::new(false),
            source: None,
            balloon_icon: None,
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
        // Under NOTIFYICON_VERSION_4 the shell shows no tooltip without `NIF_SHOWTIP`.
        nid.uFlags = NIF_SHOWTIP;

        if msg != NIM_DELETE {
            nid.uFlags |= NIF_MESSAGE | NIF_ICON | NIF_TIP;
            nid.uCallbackMessage = WM_TRAY_CALLBACK;
            nid.hIcon = self.hicon.get();

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
        self.set_balloon_icon(None);
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
        let old = self.hicon.replace(hicon);
        let old_owned = self.owns_icon.replace(owned);
        self.source = None;
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
        self.hicon.get()
    }

    /// A tray icon showing `icon` at the size of a small icon (`QSystemTrayIcon(const QIcon &)`).
    /// The tray keeps `icon`, so it can build the `HICON` again when the taskbar is re-created.
    pub fn from_icon(
        tooltip: impl Into<String>,
        icon: &ferrokit_gui::image::Icon,
    ) -> Result<Box<Self>, &'static str> {
        let hicon = Self::hicon_for_small_icon(icon)?;
        match Self::new(tooltip, hicon) {
            Ok(mut tray) => {
                tray.owns_icon.set(true);
                tray.source = Some(icon.clone());
                Ok(tray)
            }
            Err(e) => {
                Self::destroy_hicon(hicon);
                Err(e)
            }
        }
    }

    /// Shows `icon` (`QSystemTrayIcon::setIcon`) and keeps it for `TaskbarCreated`.
    pub fn set_icon_from(&mut self, icon: &ferrokit_gui::image::Icon) -> Result<(), &'static str> {
        let hicon = Self::hicon_for_small_icon(icon)?;
        let result = self.replace_icon(hicon, true);
        // Kept even when the shell refused the change: the `HICON` is shown from now on.
        self.source = Some(icon.clone());
        result
    }

    /// `QWindowsSystemTrayIcon::createIcon`: the size of a small icon (`SM_CXSMICON`,
    /// `SM_CYSMICON`) as far as `icon` has one (`QIcon::actualSize`), as one `HICON`.
    fn hicon_for_small_icon(icon: &ferrokit_gui::image::Icon) -> Result<HICON, &'static str> {
        use ferrokit_gui::geometry::primitives::Size;
        use ferrokit_gui::image::{IconMode, IconState};
        use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSMICON, SM_CYSMICON};
        if icon.is_null() {
            return Err("null icon");
        }
        let requested = unsafe { Size::new(GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON)) };
        let size = icon.actual_size(requested, IconMode::Normal, IconState::Off);
        Self::create_hicon_from_pixmap(&icon.pixmap(size, IconMode::Normal, IconState::Off))
    }

    /// The `TaskbarCreated` half of `QWindowsSystemTrayIcon::winEvent`: the resolution or DPI
    /// may have changed, so the `HICON` is built again from the kept [`Icon`] before the icon is
    /// added to the new taskbar. Without a kept icon the `HICON` stays as it is.
    fn rebuild_icon_from_source(&self) {
        let Some(icon) = &self.source else { return };
        let Ok(new) = Self::hicon_for_small_icon(icon) else { return };
        let old = self.hicon.replace(new);
        let old_owned = self.owns_icon.replace(true);
        if old_owned && old != new {
            // The shell has no icon of ours any more (it was re-created); the old one is ours.
            Self::destroy_hicon(old);
        }
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// The tooltip (`QSystemTrayIcon::toolTip`).
    pub fn tooltip(&self) -> &str {
        &self.tooltip
    }

    /// `QSystemTrayIcon::isSystemTrayAvailable`: Qt's Windows engine answers `true` without
    /// looking for a taskbar (`QWindowsSystemTrayIcon::isSystemTrayAvailable`).
    pub fn is_system_tray_available() -> bool {
        true
    }

    /// Whether the shell reports the icon on the taskbar (`QWindowsSystemTrayIcon::isIconVisible`):
    /// a rectangle for it, that is not hidden by the overflow area (Windows 10 answers `S_FALSE`,
    /// Windows 11 a rectangle below the monitor).
    fn is_icon_shown(&self) -> Option<windows_sys::Win32::Foundation::RECT> {
        use windows_sys::Win32::Graphics::Gdi::{
            GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
        };
        if !self.visible {
            return None;
        }
        let mut id: NOTIFYICONIDENTIFIER = unsafe { std::mem::zeroed() };
        id.cbSize = std::mem::size_of::<NOTIFYICONIDENTIFIER>() as u32;
        id.hWnd = self.hwnd;
        id.uID = 1;
        let mut rect = windows_sys::Win32::Foundation::RECT { left: 0, top: 0, right: 0, bottom: 0 };
        let hr = unsafe { Shell_NotifyIconGetRect(&id, &mut rect) };
        if hr != 0 {
            return None;
        }
        let mut info: MONITORINFO = unsafe { std::mem::zeroed() };
        info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        let monitor = unsafe { MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST) };
        unsafe { GetMonitorInfoW(monitor, &mut info) };
        (rect.bottom <= info.rcMonitor.bottom).then_some(rect)
    }

    /// Where the shell shows the icon, in logical desktop pixels (`QSystemTrayIcon::geometry`);
    /// an empty rectangle while the icon is hidden or not on the taskbar.
    pub fn geometry(&self) -> Rect {
        match self.is_icon_shown() {
            Some(r) => crate::screen::native_rect_to_logical(Rect::new(
                r.left,
                r.top,
                r.right - r.left,
                r.bottom - r.top,
            )),
            None => Rect::new(0, 0, 0, 0),
        }
    }

    /// `QWindowsSystemTrayIcon::supportsMessages`: balloon messages are on unless the user turned
    /// them off (`HKCU\...\Explorer\Advanced\EnableBalloonTips`; the key is typically absent
    /// on Windows 10 and means on).
    pub fn supports_messages(&self) -> bool {
        enable_balloon_tips() != 0
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
        // `QSystemTrayIcon::showMessage` (both overloads) does nothing while the icon is hidden.
        if !self.visible || !self.supports_messages() {
            return Ok(());
        }
        // The previous message's icon is freed before this one is made.
        self.set_balloon_icon(None);
        let (flags, balloon) = Self::balloon_style(icon);
        let result = self.notify_balloon(
            title,
            msg,
            flags,
            balloon.as_ref().map_or(ptr::null_mut(), |b| b.hicon),
            duration_ms,
        );
        self.set_balloon_icon(balloon);
        result
    }

    /// Replaces the icon of the last balloon message and frees the old one. As
    /// `QWindowsSystemTrayIcon::m_hMessageIcon` the icon is kept after `NIM_MODIFY` returns (the
    /// `hBalloonIcon` documentation does not say the shell is done with the handle by then) and
    /// freed when the next message is shown, when the icon is hidden and when it is dropped.
    fn set_balloon_icon(&mut self, icon: Option<BalloonIcon>) {
        if let Some(old) = std::mem::replace(&mut self.balloon_icon, icon) {
            old.release();
        }
    }

    /// The pixel size of the balloon icon made from a stock icon: `QWindowsTheme::
    /// refreshIconPixmapSizes` gives the style icon the sizes `SM_CXSMICON`, `SM_CXICON`,
    /// 1.5 x `SM_CXICON` and 8 x `SM_CXICON` (16, 32, 48 and 256 at 100% scaling), and
    /// `actualSize(QSize(256, 256))` takes the first with at least that area, else the largest,
    /// scaled down to fit 256 px. (Qt scales the extracted icon down; here it is extracted at the
    /// final size.)
    fn stock_icon_size(small: i32, large: i32) -> u32 {
        [small, large, large + large / 2, 8 * large]
            .into_iter()
            .find(|&s| i64::from(s) * i64::from(s) >= 256 * 256)
            .unwrap_or(8 * large)
            .min(256)
            .max(1) as u32
    }

    /// `QSystemTrayIcon::showMessage(title, msg, MessageIcon, msecs)` hands the platform the
    /// style's standard icon for the type (`SP_MessageBoxInformation`, `SP_MessageBoxWarning`,
    /// `SP_MessageBoxCritical`) and none for `NoIcon`; `QWindowsSystemTrayIcon::showMessage` then
    /// shows `NIIF_USER | NIIF_LARGE_ICON` with that icon, and `NIIF_INFO` when there is none
    /// (`NoIcon` included).
    /// `QWindowsTheme::standardPixmap` tries the shell's stock icon, then (for the types that
    /// have one, not these) a shell32 resource, then `LoadIcon(IDI_INFORMATION / IDI_WARNING /
    /// IDI_ERROR)`, and gives a null pixmap, hence `NIIF_INFO`, when all fail.
    fn balloon_style(icon: crate::platform_tray::TrayMessageIcon) -> (u32, Option<BalloonIcon>) {
        use crate::platform_tray::TrayMessageIcon;
        use windows_sys::Win32::UI::Shell::{SIID_ERROR, SIID_INFO, SIID_WARNING};
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            IDI_ERROR, IDI_INFORMATION, IDI_WARNING,
        };
        const NIIF_INFO: u32 = 0x00000001;
        const NIIF_USER: u32 = 0x00000004;
        const NIIF_LARGE_ICON: u32 = 0x00000020;

        let (siid, idi) = match icon {
            TrayMessageIcon::NoIcon => return (NIIF_INFO, None),
            TrayMessageIcon::Information => (SIID_INFO, IDI_INFORMATION),
            TrayMessageIcon::Warning => (SIID_WARNING, IDI_WARNING),
            TrayMessageIcon::Critical => (SIID_ERROR, IDI_ERROR),
        };
        let owned_stock = Self::stock_icon(siid).map(|hicon| BalloonIcon { hicon, owned: true });
        let shared = || Self::system_icon(idi).map(|hicon| BalloonIcon { hicon, owned: false });
        match owned_stock.or_else(shared) {
            Some(balloon) => (NIIF_USER | NIIF_LARGE_ICON, Some(balloon)),
            None => (NIIF_INFO, None),
        }
    }

    /// The shell's stock icon `siid`, extracted at `stock_icon_size` (`SHGetStockIconInfo` with
    /// `SHGSI_ICONLOCATION`, then `SHDefExtractIcon`). The caller destroys it.
    fn stock_icon(siid: i32) -> Option<HICON> {
        use windows_sys::Win32::UI::Shell::{
            SHDefExtractIconW, SHGetStockIconInfo, SHGSI_ICONLOCATION, SHSTOCKICONINFO,
        };
        use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXICON, SM_CXSMICON};
        let mut info: SHSTOCKICONINFO = unsafe { std::mem::zeroed() };
        info.cbSize = std::mem::size_of::<SHSTOCKICONINFO>() as u32;
        if unsafe { SHGetStockIconInfo(siid, SHGSI_ICONLOCATION, &mut info) } < 0 {
            return None;
        }
        let size = unsafe {
            Self::stock_icon_size(GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CXICON))
        };
        let mut hicon: HICON = ptr::null_mut();
        let hr = unsafe {
            SHDefExtractIconW(info.szPath.as_ptr(), info.iIcon, 0, &mut hicon, ptr::null_mut(), size)
        };
        (hr >= 0 && !hicon.is_null()).then_some(hicon)
    }

    /// A system icon (`LoadIcon(nullptr, idi)`), at the size the system chooses. It is a shared
    /// handle owned by the system: it must never be passed to `DestroyIcon`.
    fn system_icon(idi: windows_sys::core::PCWSTR) -> Option<HICON> {
        let hicon = unsafe { windows_sys::Win32::UI::WindowsAndMessaging::LoadIconW(ptr::null_mut(), idi) };
        (!hicon.is_null()).then_some(hicon)
    }

    /// Shows a balloon whose icon is `icon` (`QSystemTrayIcon::showMessage(title, msg, QIcon,
    /// msecs)`): `NIIF_USER | NIIF_LARGE_ICON` with an `hBalloonIcon` made from the icon at its
    /// actual size for 256x256 (`icon.actualSize(QSize(256, 256))`).
    /// A null icon shows the balloon without one.
    pub fn show_message_with_icon(
        &mut self,
        title: &str,
        msg: &str,
        icon: &ferrokit_gui::image::Icon,
        duration_ms: u32,
    ) -> Result<(), &'static str> {
        if !self.visible || !self.supports_messages() {
            return Ok(());
        }
        if icon.is_null() {
            return self.show_message(
                title,
                msg,
                crate::platform_tray::TrayMessageIcon::NoIcon,
                duration_ms,
            );
        }
        const NIIF_USER: u32 = 0x00000004;
        const NIIF_LARGE_ICON: u32 = 0x00000020;
        // The previous message's icon is freed before this one is made.
        self.set_balloon_icon(None);
        let hballoon = Self::hicon_for_balloon(icon)?;
        let result =
            self.notify_balloon(title, msg, NIIF_USER | NIIF_LARGE_ICON, hballoon, duration_ms);
        self.set_balloon_icon(Some(BalloonIcon { hicon: hballoon, owned: true }));
        result
    }

    /// The balloon icon is made at `icon.actualSize(QSize(256, 256))` (as Qt), but never smaller
    /// than the large icon size: the shell refuses `NIIF_USER | NIIF_LARGE_ICON` with a smaller
    /// `hBalloonIcon` (documented as `SM_CXICON` x `SM_CYICON`; `Shell_NotifyIconW` failed for
    /// 31 px and below, measured). Qt does not do this and does not look at the result.
    fn hicon_for_balloon(icon: &ferrokit_gui::image::Icon) -> Result<HICON, &'static str> {
        use ferrokit_gui::geometry::primitives::Size;
        use ferrokit_gui::image::{IconMode, IconState};
        use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXICON, SM_CYICON};
        let actual = icon.actual_size(Size::new(256, 256), IconMode::Normal, IconState::Off);
        let large = unsafe { Size::new(GetSystemMetrics(SM_CXICON), GetSystemMetrics(SM_CYICON)) };
        let size = Self::balloon_icon_size(actual, large);
        Self::create_hicon_from_pixmap(&icon.pixmap(size, IconMode::Normal, IconState::Off))
    }

    /// `large` when `actual` is smaller than it in either direction, else `actual`.
    fn balloon_icon_size(
        actual: ferrokit_gui::geometry::primitives::Size,
        large: ferrokit_gui::geometry::primitives::Size,
    ) -> ferrokit_gui::geometry::primitives::Size {
        if actual.width < large.width || actual.height < large.height {
            large
        } else {
            actual
        }
    }

    fn notify_balloon(
        &mut self,
        title: &str,
        msg: &str,
        info_flags: u32,
        balloon_icon: HICON,
        duration_ms: u32,
    ) -> Result<(), &'static str> {
        let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = self.hwnd;
        nid.uID = 1;
        nid.uFlags = NIF_INFO | NIF_SHOWTIP;

        // A message that is empty next to a title still shows (qwindowssystemtrayicon.cpp).
        let msg = if msg.is_empty() && !title.is_empty() { " " } else { msg };
        let wide_msg: Vec<u16> = msg.encode_utf16().collect();
        let msg_len = wide_msg.len().min(255);
        nid.szInfo[..msg_len].copy_from_slice(&wide_msg[..msg_len]);
        nid.szInfo[msg_len] = 0;

        let wide_title: Vec<u16> = title.encode_utf16().collect();
        let title_len = wide_title.len().min(63);
        nid.szInfoTitle[..title_len].copy_from_slice(&wide_title[..title_len]);
        nid.szInfoTitle[title_len] = 0;

        nid.Anonymous.uTimeout = if duration_ms == 0 { 10000 } else { duration_ms };
        nid.dwInfoFlags = info_flags;
        nid.hBalloonIcon = balloon_icon;

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
    fn set_icon(&mut self, icon: &ferrokit_gui::image::Icon) -> Result<(), &'static str> {
        self.set_icon_from(icon)
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

    fn show_message_with_icon(
        &mut self,
        title: &str,
        msg: &str,
        icon: &ferrokit_gui::image::Icon,
        duration_ms: u32,
    ) -> Result<(), &'static str> {
        self.show_message_with_icon(title, msg, icon, duration_ms)
    }

    fn supports_messages(&self) -> bool {
        self.supports_messages()
    }
}

/// `EnableBalloonTips` of the explorer's advanced settings, 1 when it is not set.
#[cfg(windows)]
fn enable_balloon_tips() -> u32 {
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ, REG_DWORD,
    };
    let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };
    let subkey = wide("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Advanced");
    let name = wide("EnableBalloonTips");
    unsafe {
        let mut hkey: HKEY = ptr::null_mut();
        if RegOpenKeyExW(HKEY_CURRENT_USER, subkey.as_ptr(), 0, KEY_READ, &mut hkey) != 0 {
            return 1;
        }
        let mut kind = 0u32;
        let mut data = 0u32;
        let mut size = std::mem::size_of::<u32>() as u32;
        let status = RegQueryValueExW(
            hkey,
            name.as_ptr(),
            ptr::null_mut(),
            &mut kind,
            &mut data as *mut u32 as *mut u8,
            &mut size,
        );
        RegCloseKey(hkey);
        if status == 0 && kind == REG_DWORD {
            data
        } else {
            1
        }
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
        if self.owns_icon.get() {
            Self::destroy_hicon(self.hicon.get());
        }
    }
}

#[cfg(not(windows))]
pub struct TrayIcon {
    inner: Box<dyn crate::platform_tray::PlatformTrayIcon>,
    /// Whether the icon is shown (`QSystemTrayIcon::isVisible`): messages are dropped while it is not.
    visible: bool,
    pub on_menu_action: Signal<u32>,
    pub on_activated: Signal<TrayActivation>,
    pub on_context_menu_requested: Signal<Point>,
}
#[cfg(not(windows))]
impl TrayIcon {
    /// A tray icon showing `icon` (`QSystemTrayIcon(const QIcon &)`).
    pub fn from_icon(
        tooltip: impl Into<String>,
        icon: &ferrokit_gui::image::Icon,
    ) -> Result<Box<Self>, &'static str> {
        let title_str = tooltip.into();
        let mut inner = crate::integration::platform().create_tray_icon(&title_str, icon)?;
        let shown = inner.show().is_ok();
        let mut tray = Self::with_backend(inner);
        tray.visible = shown;
        Ok(tray)
    }

    /// Wraps `inner`, forwarding the activations it reports to [`TrayIcon::on_activated`]
    /// (`QSystemTrayIcon::activated`).
    pub fn with_backend(mut inner: Box<dyn crate::platform_tray::PlatformTrayIcon>) -> Box<Self> {
        let on_activated = Signal::new();
        crate::platform_tray::forward_activations(inner.as_mut(), &on_activated);
        Box::new(Self {
            inner,
            visible: false,
            on_menu_action: Signal::new(),
            on_activated,
            on_context_menu_requested: Signal::new(),
        })
    }

    /// Dispatches what the desktop sent to the icon since the last call, which may emit
    /// [`TrayIcon::on_activated`]. Only the Linux StatusNotifierItem back end needs this (its
    /// calls, the registration reply and `NameOwnerChanged` arrive on the session bus socket),
    /// and nothing calls it automatically: the application must bind the socket with
    /// [`TrayIcon::bind_event_dispatcher`] and call this when the notifier fires (the event
    /// loop's dispatcher is not reachable from here, and the icon is `!Send`). On macOS AppKit
    /// calls back by itself.
    pub fn process_events(&mut self) {
        self.inner.process_events();
    }

    /// Watches the descriptor that signals pending activations (Linux: the session bus
    /// socket); connect the notifier's `activated` signal to [`TrayIcon::process_events`].
    /// `Ok(None)` when the back end has none.
    pub fn bind_event_dispatcher(
        &mut self,
        dispatcher: &mut dyn ferrokit_core::event_loop::EventDispatcher,
    ) -> Result<
        Option<std::sync::Arc<ferrokit_core::event_loop::SocketNotifier>>,
        ferrokit_core::event_loop::SocketNotifierError,
    > {
        self.inner.bind_event_dispatcher(dispatcher)
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

    /// Shows the icon. On Linux `Ok` only means the StatusNotifierWatcher registration was
    /// queued on the session bus: nothing here reads the answer, so the application must drive
    /// [`TrayIcon::process_events`] (from the notifier returned by
    /// [`TrayIcon::bind_event_dispatcher`]) for the registration to be confirmed and for the
    /// host's activations and property reads to be served. Without that the icon stays
    /// unconfirmed and the host's calls go unanswered. macOS shows it at once.
    pub fn show(&mut self) -> Result<(), &'static str> {
        self.inner.show()?;
        self.visible = true;
        Ok(())
    }

    pub fn hide(&mut self) -> Result<(), &'static str> {
        self.inner.hide()?;
        self.visible = false;
        Ok(())
    }

    pub fn show_message(
        &mut self,
        title: &str,
        msg: &str,
        icon: crate::platform_tray::TrayMessageIcon,
        duration_ms: u32,
    ) -> Result<(), &'static str> {
        // `QSystemTrayIcon::showMessage` does nothing while the icon is hidden.
        if !self.visible {
            return Ok(());
        }
        self.inner.show_message(title, msg, icon, duration_ms)
    }

    pub fn show_message_with_icon(
        &mut self,
        title: &str,
        msg: &str,
        icon: &ferrokit_gui::image::Icon,
        duration_ms: u32,
    ) -> Result<(), &'static str> {
        if !self.visible {
            return Ok(());
        }
        self.inner.show_message_with_icon(title, msg, icon, duration_ms)
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
    fn create_tray_icon_keeps_router_address_and_owns_icon() {
        use crate::integration::{PlatformIntegration, Win32PlatformIntegration};
        let mut pixmap = Pixmap::new(16, 16).expect("failed to create pixmap");
        pixmap.fill(Color::from_rgba8(1, 2, 3, 255));
        let tray = Win32PlatformIntegration::default()
            .create_tray_icon("Router Test", &ferrokit_gui::image::Icon::from_pixmap(&pixmap))
            .expect("create_tray_icon");
        let addr = &*tray as *const dyn crate::platform_tray::PlatformTrayIcon as *const () as usize;
        {
            let guard = TRAY_INSTANCES.lock().unwrap_or_else(|e| e.into_inner());
            assert!(
                guard.as_ref().unwrap().values().any(|&a| a == addr),
                "router table must point at the returned allocation"
            );
        }
        drop(tray);
    }

    #[test]
    fn the_balloon_icon_lives_until_the_next_message_or_hide() {
        // `QWindowsSystemTrayIcon::m_hMessageIcon`: kept after the notification, freed before the
        // next message's icon is made and on cleanup.
        use crate::platform_tray::TrayMessageIcon;
        use windows_sys::Win32::UI::WindowsAndMessaging::GetIconInfo;
        let alive = |icon: HICON| {
            let mut info: ICONINFO = unsafe { std::mem::zeroed() };
            let ok = unsafe { GetIconInfo(icon, &mut info) } != 0;
            if ok {
                unsafe {
                    DeleteObject(info.hbmColor);
                    DeleteObject(info.hbmMask);
                }
            }
            ok
        };
        let pixmap = Pixmap::new(16, 16).unwrap();
        let mut tray = TrayIcon::from_icon("balloon", &ferrokit_gui::image::Icon::from_pixmap(&pixmap))
            .expect("tray");
        if !tray.supports_messages() {
            return;
        }
        // `QSystemTrayIcon::showMessage` does nothing while the icon is hidden (`d->visible`):
        // no notification, no icon kept.
        assert!(!tray.is_visible());
        tray.show_message(" ", "Hidden", TrayMessageIcon::Information, 1000).unwrap();
        tray.show_message_with_icon(
            " ",
            "Hidden",
            &ferrokit_gui::image::Icon::from_pixmap(&pixmap),
            1000,
        )
        .unwrap();
        assert!(tray.balloon_icon.is_none());

        tray.show().expect("show");
        tray.show_message(" ", "Info", TrayMessageIcon::Information, 1000).unwrap();
        let first = tray.balloon_icon.as_ref().expect("kept after the message");
        assert!(first.owned && alive(first.hicon), "still alive after the call");
        // A small image is scaled up to the large icon size, which the shell accepts.
        tray.show_message_with_icon(
            " ",
            "Custom",
            &ferrokit_gui::image::Icon::from_pixmap(&pixmap),
            1000,
        )
        .unwrap();
        let second = tray.balloon_icon.as_ref().expect("replaced by the next message");
        assert!(second.owned && alive(second.hicon));
        let handle = second.hicon;
        // A message without an icon frees the previous one and keeps none.
        tray.show_message(" ", "None", TrayMessageIcon::NoIcon, 1000).unwrap();
        assert!(tray.balloon_icon.is_none());
        assert!(!alive(handle), "freed by the next message");
        tray.show_message(" ", "Info", TrayMessageIcon::Information, 1000).unwrap();
        let handle = tray.balloon_icon.as_ref().expect("kept").hicon;
        tray.hide().unwrap();
        assert!(tray.balloon_icon.is_none());
        assert!(!alive(handle), "freed on hide");
    }

    #[test]
    fn the_stock_icon_size_follows_the_style_icon_sizes_of_the_theme() {
        // Sizes `SM_CXSMICON`, `SM_CXICON`, 1.5 x and 8 x `SM_CXICON`; the first whose area is at
        // least 256 x 256, else the largest, never above 256 px.
        assert_eq!(TrayIcon::stock_icon_size(16, 32), 256, "100%: 16, 32, 48, 256");
        assert_eq!(TrayIcon::stock_icon_size(20, 40), 256, "125%: 20, 40, 60, 320 -> 256");
        assert_eq!(TrayIcon::stock_icon_size(16, 16), 128, "16, 16, 24, 128: the largest");
        assert_eq!(TrayIcon::stock_icon_size(32, 40), 256, "32, 40, 60, 320 -> 256");
    }

    #[test]
    fn the_system_icons_are_the_fallback_when_the_shell_has_no_stock_icon() {
        // `QWindowsTheme::standardPixmap`: `LoadIcon(nullptr, IDI_INFORMATION / IDI_WARNING /
        // IDI_ERROR)` after the stock icon. The shell picks their size (40 px here, where
        // `SM_CXICON` is 32), so only that they exist and are square is checked.
        use windows_sys::Win32::Graphics::Gdi::{GetObjectW, BITMAP};
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetIconInfo, IDI_ERROR, IDI_INFORMATION, IDI_WARNING,
        };
        for idi in [IDI_INFORMATION, IDI_WARNING, IDI_ERROR] {
            let icon = TrayIcon::system_icon(idi).expect("a system icon");
            let mut info: ICONINFO = unsafe { std::mem::zeroed() };
            assert_ne!(unsafe { GetIconInfo(icon, &mut info) }, 0);
            let mut bm: BITMAP = unsafe { std::mem::zeroed() };
            unsafe {
                GetObjectW(info.hbmColor, std::mem::size_of_val(&bm) as i32, &mut bm as *mut _ as *mut _);
                DeleteObject(info.hbmColor);
                DeleteObject(info.hbmMask);
            }
            assert!(bm.bmWidth >= 16 && bm.bmWidth == bm.bmHeight, "square: {}", bm.bmWidth);
            // A shared system icon is not destroyed: it still works after `release`.
            BalloonIcon { hicon: icon, owned: false }.release();
            let mut again: ICONINFO = unsafe { std::mem::zeroed() };
            assert_ne!(unsafe { GetIconInfo(icon, &mut again) }, 0, "the shared icon survives");
            unsafe {
                DeleteObject(again.hbmColor);
                DeleteObject(again.hbmMask);
            }
        }
    }

    #[test]
    fn a_balloon_message_shows_the_standard_icon_of_its_type() {
        // `QSystemTrayIcon::showMessage(.., MessageIcon, ..)`: the style's standard icon is shown
        // as `NIIF_USER | NIIF_LARGE_ICON`; `NoIcon` has none and gets `NIIF_INFO`
        // (`QWindowsSystemTrayIcon::showMessage` with a null pixmap).
        use crate::platform_tray::TrayMessageIcon;
        const NIIF_INFO: u32 = 1;
        const NIIF_USER_LARGE: u32 = 0x4 | 0x20;
        let (flags, none) = TrayIcon::balloon_style(TrayMessageIcon::NoIcon);
        assert_eq!(flags, NIIF_INFO);
        assert!(none.is_none());
        for kind in [
            TrayMessageIcon::Information,
            TrayMessageIcon::Warning,
            TrayMessageIcon::Critical,
        ] {
            let (flags, balloon) = TrayIcon::balloon_style(kind);
            assert_eq!(flags, NIIF_USER_LARGE, "{kind:?}");
            let balloon = balloon.expect("the shell has a stock icon");
            let icon = balloon.hicon;
            assert!(!icon.is_null());
            // Extracted at 256 px, the largest size the balloon asks the style icon for.
            let mut info: ICONINFO = unsafe { std::mem::zeroed() };
            assert_ne!(unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetIconInfo(icon, &mut info) }, 0);
            let mut bm: windows_sys::Win32::Graphics::Gdi::BITMAP = unsafe { std::mem::zeroed() };
            unsafe {
                windows_sys::Win32::Graphics::Gdi::GetObjectW(
                    info.hbmColor,
                    std::mem::size_of_val(&bm) as i32,
                    &mut bm as *mut _ as *mut _,
                );
                DeleteObject(info.hbmColor);
                DeleteObject(info.hbmMask);
            }
            let expected = unsafe {
                use windows_sys::Win32::UI::WindowsAndMessaging::{
                    GetSystemMetrics, SM_CXICON, SM_CXSMICON,
                };
                TrayIcon::stock_icon_size(GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CXICON))
            };
            assert_eq!(bm.bmWidth, expected as i32, "{kind:?}");
            // An owned icon is destroyed by `release`.
            let (hicon, owned) = (balloon.hicon, balloon.owned);
            assert!(owned, "{kind:?}: the extracted stock icon is ours");
            balloon.release();
            let mut gone: ICONINFO = unsafe { std::mem::zeroed() };
            assert_eq!(
                unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetIconInfo(hicon, &mut gone) },
                0,
                "{kind:?}: destroyed"
            );
        }
    }

    #[test]
    fn a_balloon_icon_is_made_at_the_actual_size_for_256_pixels() {
        // `QWindowsSystemTrayIcon::showMessage`: `icon.actualSize(QSize(256, 256))`.
        use ferrokit_gui::image::{Icon, IconMode, IconState};
        use windows_sys::Win32::Graphics::Gdi::{GetObjectW, BITMAP};
        use windows_sys::Win32::UI::WindowsAndMessaging::GetIconInfo;
        let width_of = |sides: &[u32]| {
            let mut icon = Icon::new();
            for &side in sides {
                let pixmap = Pixmap::new(side, side).unwrap();
                icon.add_pixmap(&pixmap, IconMode::Normal, IconState::Off);
            }
            let hicon = TrayIcon::hicon_for_balloon(&icon).expect("balloon icon");
            let mut info: ICONINFO = unsafe { std::mem::zeroed() };
            assert_ne!(unsafe { GetIconInfo(hicon, &mut info) }, 0);
            let mut bm: BITMAP = unsafe { std::mem::zeroed() };
            unsafe {
                GetObjectW(info.hbmColor, std::mem::size_of::<BITMAP>() as i32, &mut bm as *mut _ as *mut _);
                DeleteObject(info.hbmColor);
                DeleteObject(info.hbmMask);
            }
            TrayIcon::destroy_hicon(hicon);
            bm.bmWidth
        };
        assert_eq!(width_of(&[16, 128]), 128, "the image that fits, not a fixed 32 px");
        assert_eq!(width_of(&[16, 512]), 256, "scaled down to fit 256 px");
        let large = unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::GetSystemMetrics(
                windows_sys::Win32::UI::WindowsAndMessaging::SM_CXICON,
            )
        };
        assert_eq!(width_of(&[16]), large, "a smaller image is scaled up to SM_CXICON");
        assert_eq!(width_of(&[large as u32 + 8]), large + 8, "a larger one is not touched");
    }

    #[test]
    fn a_balloon_icon_under_the_large_icon_size_is_scaled_up_to_it() {
        // The shell takes `NIIF_USER | NIIF_LARGE_ICON` only with an `hBalloonIcon` of the
        // large icon size, `SM_CXICON` x `SM_CYICON` (31 px and below were refused, measured).
        use ferrokit_gui::geometry::primitives::Size;
        let large = Size::new(32, 32);
        assert_eq!(TrayIcon::balloon_icon_size(Size::new(16, 16), large), large);
        assert_eq!(TrayIcon::balloon_icon_size(Size::new(31, 40), large), large);
        assert_eq!(TrayIcon::balloon_icon_size(Size::new(40, 20), large), large);
        assert_eq!(TrayIcon::balloon_icon_size(Size::new(32, 32), large), large);
        assert_eq!(TrayIcon::balloon_icon_size(Size::new(48, 48), large), Size::new(48, 48));
        assert_eq!(TrayIcon::balloon_icon_size(Size::new(40, 33), Size::new(40, 40)), Size::new(40, 40));
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

        // Simulate the shell's NIN_SELECT notification triggering Trigger
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
