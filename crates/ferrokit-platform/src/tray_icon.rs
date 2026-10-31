// SPDX-License-Identifier: LGPL-3.0-only
#[cfg(windows)]
use crate::menu::PlatformMenu;
#[cfg(windows)]
use crate::win32_handles::{create_dib_section, IconHandle, MemDc, OwnedBitmap, OwnedMenu};
#[cfg(windows)]
use crate::window_registry::SharedRegistry;
use ferrokit_core::signal::Signal;
use ferrokit_gui::geometry::primitives::Point;
#[cfg(windows)]
use ferrokit_gui::geometry::primitives::Rect;
#[cfg(windows)]
use ferrokit_gui::paint::Pixmap;
#[cfg(windows)]
use std::cell::{Cell, RefCell};
#[allow(unused_imports)]
use std::ptr;
#[cfg(windows)]
use std::rc::Rc;
#[cfg(windows)]
use std::sync::Once;
#[cfg(windows)]
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
#[cfg(windows)]
use windows_sys::Win32::Graphics::Gdi::CreateBitmap;
#[cfg(windows)]
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
#[cfg(windows)]
use windows_sys::Win32::UI::Shell::{
    Shell_NotifyIconGetRect, Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP,
    NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIN_SELECT, NOTIFYICONDATAW, NOTIFYICONIDENTIFIER,
};
#[cfg(windows)]
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIconIndirect, CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyWindow,
    GetCursorPos, RegisterClassExW, RegisterWindowMessageW, SetForegroundWindow, TrackPopupMenuEx,
    HICON, HMENU, ICONINFO, MF_CHECKED, MF_DISABLED, MF_GRAYED, MF_POPUP, MF_SEPARATOR, MF_STRING,
    MF_UNCHECKED, TPM_LEFTALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_CLOSE, WM_CONTEXTMENU,
    WM_LBUTTONDBLCLK, WM_MBUTTONUP, WNDCLASSEXW,
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
        let hmenu = OwnedMenu::create_popup().ok_or("CreatePopupMenu failed")?;

        for item in &self.items {
            if let Some(sub) = &item.submenu {
                // A failed build leaves nothing to free here; `hmenu` drops on the way out.
                let sub_hmenu = OwnedMenu::from_raw(sub.create_native_hmenu()?);
                let wide_text: Vec<u16> =
                    item.text.encode_utf16().chain(std::iter::once(0)).collect();
                let mut sub_flags = MF_POPUP | MF_STRING;
                if !item.enabled {
                    sub_flags |= MF_GRAYED | MF_DISABLED;
                }
                // On failure both guards destroy their menu; on success the parent owns the
                // submenu.
                if AppendMenuW(
                    hmenu.as_raw(),
                    sub_flags,
                    sub_hmenu.as_raw() as usize,
                    wide_text.as_ptr(),
                ) == 0
                {
                    return Err("AppendMenuW failed to add submenu");
                }
                let _ = sub_hmenu.into_raw();
                continue;
            }

            if item.is_separator {
                AppendMenuW(hmenu.as_raw(), MF_SEPARATOR, 0, ptr::null());
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

            if AppendMenuW(hmenu.as_raw(), flags, item.id as usize, wide_text.as_ptr()) == 0 {
                return Err("AppendMenuW failed");
            }
        }

        Ok(hmenu.into_raw())
    }

    #[cfg(windows)]
    // `hwnd` is passed only to user32 (`SetForegroundWindow`, `TrackPopupMenuEx`), which
    // validates it; nothing here dereferences it.
    #[allow(clippy::not_unsafe_ptr_arg_deref)]
    pub fn track_popup_at(
        &self,
        hwnd: HWND,
        screen_x: i32,
        screen_y: i32,
    ) -> Result<Option<u32>, &'static str> {
        // SAFETY: `create_native_hmenu` requires that the returned `HMENU` be destroyed: it is
        // wrapped in `OwnedMenu` at once and dropped only after `TrackPopupMenuEx` returns, which
        // destroys it on every path. `hwnd` is only passed by value to `SetForegroundWindow` and
        // `TrackPopupMenuEx`, which validate it and fail (returning 0, mapped to `Ok(None)`) for a
        // handle that is stale or belongs to another thread; the `ptr::null()` `lptpm` is documented
        // as optional.
        unsafe {
            let hmenu = OwnedMenu::from_raw(self.create_native_hmenu()?);

            SetForegroundWindow(hwnd);

            let selected = TrackPopupMenuEx(
                hmenu.as_raw(),
                TPM_LEFTALIGN | TPM_RIGHTBUTTON | TPM_RETURNCMD,
                screen_x,
                screen_y,
                hwnd,
                ptr::null(),
            );

            drop(hmenu);

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
        // SAFETY: `GetCursorPos` writes one `POINT` through the pointer; `pt` is a live local.
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
thread_local! {
    /// Window handle -> the shared state of that tray window's procedure. Thread-local because
    /// the window, its procedure and its `TrayIcon` (`!Send`) all belong to the creating thread;
    /// it holds `Weak` handles, so it neither owns the state nor points into the `TrayIcon`
    /// (see `TrayShared` and `window_registry`).
    static TRAY_INSTANCES: SharedRegistry<TrayShared> = SharedRegistry::new();
}
#[cfg(windows)]
const TRAY_WINDOW_CLASS_NAME: &[u16] = &[
    'Q' as u16, 't' as u16, 'R' as u16, 'u' as u16, 's' as u16, 't' as u16, 'T' as u16, 'r' as u16,
    'a' as u16, 'y' as u16, 'W' as u16, 'i' as u16, 'n' as u16, 0,
];

/// The shared state of the tray registered for `hwnd`, as an owned handle: it stays valid for the
/// whole message even if a slot drops the `TrayIcon` meanwhile. `None` when no tray is registered
/// (not yet, or already unregistered by `Drop`, or during thread teardown).
#[cfg(windows)]
fn lookup_tray(hwnd: HWND) -> Option<Rc<TrayShared>> {
    TRAY_INSTANCES
        .try_with(|registry| registry.lookup(hwnd as isize))
        .ok()
        .flatten()
}

/// The window procedure's body. It reaches the tray only through its `TrayShared` (an owned
/// `Rc` from the registry) and never forms a reference to the `TrayIcon` itself, so it cannot
/// alias a `&mut TrayIcon` that is live further up this thread's stack (a `Shell_NotifyIconW`
/// call in a `&mut self` method may deliver a message to this window). The `RefCell` rule of
/// `TrayShared` applies to everything done here.
///
/// # Safety
/// As for [`tray_window_proc`].
#[cfg(windows)]
unsafe fn tray_window_proc_inner(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if let Some(shared) = lookup_tray(hwnd) {
        // `shared` is this invocation's own strong handle: a slot may drop the `TrayIcon` (a
        // "Quit" entry), which destroys the window and unregisters it but cannot free the state
        // under us. After any emit, `shared.closed` says whether that happened.
        if msg == shared.taskbar_created_msg {
            shared.rebuild_icon_from_source();
            if shared.visible.get() {
                let _ = shared.send_notify_icon_message(NIM_ADD);
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
                    if shared.ignore_next_release.replace(false) {
                        return 0;
                    }
                    shared.on_activated.emit(&TrayActivation::Trigger);
                    return 0;
                }
                WM_LBUTTONDBLCLK => {
                    shared.ignore_next_release.set(true);
                    shared.on_activated.emit(&TrayActivation::DoubleClick);
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
                    // `try_borrow`: busy while `TrayIcon::with_menu_mut` runs; then no menu.
                    let has_menu = shared.menu.try_borrow().is_ok_and(|m| m.is_some());
                    shared.on_context_menu_requested.emit(&pos);
                    if shared.closed.get() {
                        return 0;
                    }
                    shared.on_activated.emit(&TrayActivation::Context);
                    if has_menu && !shared.closed.get() {
                        shared.show_context_menu_at(pos);
                    }
                    return 0;
                }
                0x0405 => {
                    // NIN_BALLOONUSERCLICK (aligned with QWindowsSystemTrayIcon)
                    shared.on_message_clicked.emit(&());
                    return 0;
                }
                WM_MBUTTONUP => {
                    // `QWindowsSystemTrayIcon::winEvent`: the only raw button message it acts on.
                    shared.on_activated.emit(&TrayActivation::MiddleClick);
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
                shared.on_power_event.emit(&event);
            }
            return 1;
        }
    }

    DefWindowProcW(hwnd, msg, wparam, lparam)
}

/// Tray window procedure. Unwinding out of an `extern "system"` callback aborts the process, so a
/// raising handler is contained and the message falls back to the default.
///
/// # Safety
/// Called as a window procedure: `hwnd` is a tray window on the calling thread (the thread that
/// owns its `TrayIcon` registry entry), and `wparam`/`lparam` carry what Windows documents for
/// `msg`.
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
    // SAFETY: inside `call_once`, so at most once per process. `wc` is fully initialised and its
    // `cbSize` is its own size; `lpszClassName` is the `'static` NUL-terminated
    // `TRAY_WINDOW_CLASS_NAME`; `lpfnWndProc` is `tray_window_proc`, an `extern "system"` function
    // of the `WNDPROC` signature; `hInstance` is the current module's handle from
    // `GetModuleHandleW(null)`. `RegisterClassExW` copies the struct, so `wc` need not outlive the
    // call.
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

/// Everything the tray window procedure reads or writes, in its own allocation.
///
/// The window procedure runs whenever the thread dispatches a message to the tray window, which
/// includes the middle of a `&mut self` method of [`TrayIcon`]: `Shell_NotifyIconW` is a
/// cross-process call and may dispatch sent messages while it waits. So the procedure must never
/// form a reference to the `TrayIcon` (that would alias the live `&mut`); it reaches this state
/// instead, through an `Rc<TrayShared>` it looks up in `TRAY_INSTANCES` by window handle. The
/// `TrayIcon` holds the other `Rc`; its moves and `&mut` borrows never touch this allocation.
/// Everything here is therefore accessed through `&TrayShared` only, with interior mutability.
///
/// **`RefCell` rule.** A call out (`Shell_NotifyIconW`, `DestroyWindow`, a menu's modal loop, any
/// signal emission, dropping an `Icon` or a menu) can run the procedure, which can borrow the
/// same cell again. So every borrow is one statement that calls nothing foreign: the value
/// needed (an `HICON`, a cloned `Icon`, a copy of the tooltip, a cloned `Rc` menu) is copied out
/// and the guard dropped before anything is called, and replaced values are dropped after the
/// guard. No borrow is then live when the procedure re-enters, so the borrows cannot conflict
/// (and the plain `borrow`/`borrow_mut` never fail). A new use of these fields must keep this;
/// where a borrow could not be kept that short, use `try_borrow` and skip.
#[cfg(windows)]
struct TrayShared {
    hwnd: HWND,
    taskbar_created_msg: u32,
    /// The icon shown and whether the tray owns it (an owned one is destroyed when replaced or
    /// with the tray; an `HICON` passed to `new` or `set_icon` stays the caller's).
    /// `TaskbarCreated` replaces the icon from the window procedure.
    icon: RefCell<IconHandle>,
    /// The icon `icon` was made from, when the tray was given an [`Icon`]
    /// (`QWindowsSystemTrayIcon::m_icon`): `TaskbarCreated` builds the `HICON` again from it.
    source: RefCell<Option<ferrokit_gui::image::Icon>>,
    /// The tooltip sent with the icon (also on `TaskbarCreated`). `TrayIcon::tooltip` keeps its
    /// own copy because it returns a `&str`.
    tooltip: RefCell<String>,
    visible: Cell<bool>,
    /// Set by a double click: the release that ends it must not count as a click.
    ignore_next_release: Cell<bool>,
    /// Set by `TrayIcon::drop` once the window is going away: a slot that dropped the icon while
    /// the procedure was inside one of its signals leaves this behind for the procedure to see.
    closed: Cell<bool>,
    /// The context menu. An `Rc`, so the modal loop keeps the menu alive even if a slot replaces
    /// it meanwhile.
    menu: RefCell<Option<Rc<dyn PlatformMenu>>>,
    /// Handles to the same signals as the public fields of `TrayIcon` (a `Signal` clone shares
    /// its connections): the procedure emits through these.
    on_activated: Signal<TrayActivation>,
    on_menu_action: Signal<u32>,
    on_message_clicked: Signal<()>,
    on_power_event: Signal<crate::window_system_interface::PowerEvent>,
    on_context_menu_requested: Signal<Point>,
}

#[cfg(windows)]
impl TrayShared {
    fn send_notify_icon_message(&self, msg: u32) -> Result<(), &'static str> {
        // SAFETY: `NOTIFYICONDATAW` is a C struct of integers, raw pointers and arrays, for which
        // all-zero is a valid value (null handles, empty strings); `cbSize` is set right below.
        let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = self.hwnd;
        nid.uID = 1;
        // Under NOTIFYICON_VERSION_4 the shell shows no tooltip without `NIF_SHOWTIP`.
        nid.uFlags = NIF_SHOWTIP;

        if msg != NIM_DELETE {
            nid.uFlags |= NIF_MESSAGE | NIF_ICON | NIF_TIP;
            nid.uCallbackMessage = WM_TRAY_CALLBACK;
            nid.hIcon = self.icon.borrow().as_raw();

            let wide_tip: Vec<u16> = self.tooltip.borrow().encode_utf16().collect();
            let copy_len = wide_tip.len().min(127);
            nid.szTip[..copy_len].copy_from_slice(&wide_tip[..copy_len]);
            nid.szTip[copy_len] = 0;
        }

        // No `RefCell` borrow is live across the calls below: they may run the window procedure.
        // SAFETY: `nid` is fully initialised with `cbSize` set to its size; `hWnd` is this tray's
        // own window (created in `with_icon`, destroyed only in `Drop`); `hIcon` is the handle
        // `self.icon` holds; `szTip` is NUL-terminated (`copy_len <= 127` of 128 elements). `nid`
        // outlives the call, and the shell only reads it.
        let res = unsafe { Shell_NotifyIconW(msg, &nid) };
        if res == 0 {
            // SAFETY: `cls` is a NUL-terminated UTF-16 vector that lives to the end of the block;
            // the null window name is documented as allowed by `FindWindowW`.
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
            // Without version 4 the shell sends callbacks in the old layout, which the window
            // procedure would misread, so the icon is taken off again and the add fails
            // (`QWindowsSystemTrayIcon::sendTrayMessage` also reports this as a failure).
            // SAFETY: as for the first call: `nid` is initialised, `cbSize` set, `hWnd` is this
            // tray's live window, and `uVersion` is set just above as `NIM_SETVERSION` requires.
            if unsafe { Shell_NotifyIconW(windows_sys::Win32::UI::Shell::NIM_SETVERSION, &nid) }
                == 0
            {
                // SAFETY: as above: `nid` is initialised and `hWnd` is this tray's live window;
                // `NIM_DELETE` reads only `hWnd` and `uID`.
                unsafe { Shell_NotifyIconW(NIM_DELETE, &nid) };
                return Err("Shell_NotifyIconW (NIM_SETVERSION) failed");
            }
        }
        Ok(())
    }

    /// The `TaskbarCreated` half of `QWindowsSystemTrayIcon::winEvent`: the resolution or DPI
    /// may have changed, so the `HICON` is built again from the kept [`Icon`] before the icon is
    /// added to the new taskbar. Without a kept icon the `HICON` stays as it is.
    fn rebuild_icon_from_source(&self) {
        // Cloned out: the borrow must not span the (GDI) calls that build the icon.
        let source = self.source.borrow().clone();
        let Some(icon) = source else { return };
        let Ok(new) = TrayIcon::hicon_for_small_icon(&icon) else {
            return;
        };
        // The shell has no icon of ours any more (it was re-created); an old owned icon is ours
        // and is destroyed when `old` drops.
        let old = self.icon.borrow_mut().replace(IconHandle::owned(new));
        drop(old);
    }

    /// Runs the context menu's modal loop at `screen_pos` and emits the chosen command.
    fn show_context_menu_at(&self, screen_pos: Point) {
        // The modal loop runs slots, one of which may replace the menu or drop the icon: the loop
        // runs on a handle of its own (and the emit through this state's own signal handle), with
        // no borrow held.
        // `try_borrow`: `TrayIcon::with_menu_mut` holds the cell while its closure runs, and a
        // message dispatched from there finds it busy; no menu is shown then.
        let menu = self.menu.try_borrow().ok().and_then(|m| m.clone());
        if let Some(menu) = menu {
            if let Some(cmd_id) = menu.exec_popup(screen_pos) {
                self.on_menu_action.emit(&cmd_id);
            }
        }
    }
}

/// A Windows tray icon.
///
/// The window procedure of the tray window reaches the icon's state through `TrayShared`, not
/// through this struct, so the `TrayIcon` may be moved freely and borrowed `&mut` while the shell
/// is called. The public signals are handles that share their connections with the ones the
/// procedure emits through; they must not be replaced by assignment.
#[cfg(windows)]
pub struct TrayIcon {
    shared: Rc<TrayShared>,
    /// The icon of the last balloon message, kept as `QWindowsSystemTrayIcon::m_hMessageIcon`.
    /// Not used by the window procedure.
    balloon_icon: Option<IconHandle>,
    /// A copy of `shared.tooltip`, which `tooltip` can lend as a `&str`; `set_tooltip` updates
    /// both.
    tooltip: String,

    // These are handles to the signals the window procedure emits through (a `Signal` clone
    // shares its connections). Connect to them; do not replace them by assignment, which would
    // leave the procedure emitting through the old ones.
    pub on_activated: Signal<TrayActivation>,
    pub on_menu_action: Signal<u32>,
    pub on_message_clicked: Signal<()>,
    pub on_power_event: Signal<crate::window_system_interface::PowerEvent>,
    pub on_context_menu_requested: Signal<Point>,
}

// Deliberately neither `Send` nor `Sync` (`Rc`, raw `HWND`/`HICON`): the tray window, its window
// procedure (which reaches the state through the thread-local registry) and `DestroyWindow` in
// `Drop` all belong to the thread that created the icon.
#[cfg(windows)]
impl TrayIcon {
    pub fn new(tooltip: impl Into<String>, hicon: HICON) -> Result<Box<Self>, &'static str> {
        Self::with_icon(tooltip, IconHandle::shared(hicon))
    }

    /// A tray icon that shows `icon` and destroys it with the tray if it is owned; an early
    /// failure drops `icon` and so destroys an owned one.
    fn with_icon(tooltip: impl Into<String>, icon: IconHandle) -> Result<Box<Self>, &'static str> {
        ensure_tray_window_class_registered();

        // SAFETY: `taskbar_msg_name` is a NUL-terminated UTF-16 vector that lives to the end of the
        // block, which is all `RegisterWindowMessageW` needs.
        let taskbar_created_msg = unsafe {
            let taskbar_msg_name: Vec<u16> = "TaskbarCreated"
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            RegisterWindowMessageW(taskbar_msg_name.as_ptr())
        };

        // SAFETY: `TRAY_WINDOW_CLASS_NAME` is `'static` and NUL-terminated and names the class
        // registered by `ensure_tray_window_class_registered` just above (if registration failed the
        // call returns null, handled right below); the null window name, parent, menu and
        // `lpParam` are all documented as optional; `hInstance` is this module's own handle. The
        // window is created on the calling thread, which `TrayIcon` (neither `Send` nor `Sync`)
        // keeps for its whole life.
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
        // SAFETY: `hwnd` was created by `CreateWindowExW` above and checked non-null; a null
        // `pChangeFilterStruct` is documented as allowed for `MSGFLT_ALLOW`.
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::ChangeWindowMessageFilterEx(
                hwnd,
                taskbar_created_msg,
                windows_sys::Win32::UI::WindowsAndMessaging::MSGFLT_ALLOW,
                ptr::null_mut(),
            );
        }

        let tooltip = tooltip.into();
        let on_activated = Signal::new();
        let on_menu_action = Signal::new();
        let on_message_clicked = Signal::new();
        let on_power_event = Signal::new();
        let on_context_menu_requested = Signal::new();
        let shared = Rc::new(TrayShared {
            hwnd,
            taskbar_created_msg,
            icon: RefCell::new(icon),
            source: RefCell::new(None),
            tooltip: RefCell::new(String::clone(&tooltip)),
            visible: Cell::new(false),
            ignore_next_release: Cell::new(false),
            closed: Cell::new(false),
            menu: RefCell::new(None),
            on_activated: on_activated.clone(),
            on_menu_action: on_menu_action.clone(),
            on_message_clicked: on_message_clicked.clone(),
            on_power_event: on_power_event.clone(),
            on_context_menu_requested: on_context_menu_requested.clone(),
        });

        // Route the window's messages to the shared state (not to the `TrayIcon`, which the
        // caller is about to move and later borrow `&mut`). `Drop` unregisters the window before
        // it destroys it.
        TRAY_INSTANCES.with(|registry| registry.register(hwnd as isize, &shared));

        // Boxed as the public API returns it (and `PlatformTrayIcon` is used as a trait object);
        // nothing depends on the address.
        Ok(Box::new(Self {
            shared,
            balloon_icon: None,
            tooltip,
            on_activated,
            on_menu_action,
            on_message_clicked,
            on_power_event,
            on_context_menu_requested,
        }))
    }

    pub fn set_menu(&mut self, menu: Box<dyn PlatformMenu>) {
        // `Rc::from(Box<dyn _>)` moves the menu into a new allocation; the old one drops after
        // the borrow.
        let old = self.shared.menu.replace(Some(Rc::from(menu)));
        drop(old);
    }

    pub fn menu(&self) -> Option<&dyn PlatformMenu> {
        let menu: *const dyn PlatformMenu = Rc::as_ptr(self.shared.menu.borrow().as_ref()?);
        // SAFETY: `menu` points at the value inside the `Rc` held by `shared.menu`, which is
        // alive: the slot is replaced only by `set_menu(&mut self)` (and freed with `shared`,
        // which `self` keeps alive), and the returned reference borrows `self`, so neither can
        // run while it lives. The window procedure only clones that `Rc` and calls `&self`
        // methods of the menu, which is shared access like this one.
        Some(unsafe { &*menu })
    }

    /// Runs `f` with exclusive access to the menu; `None` when there is no menu or a context
    /// menu is being shown (its modal loop holds a second handle). The menu cell stays borrowed
    /// while `f` runs: if `f` dispatches messages, a context menu request meanwhile finds the cell
    /// busy and shows nothing (`TrayShared::show_context_menu_at`).
    pub fn with_menu_mut<R>(&mut self, f: impl FnOnce(&mut dyn PlatformMenu) -> R) -> Option<R> {
        let mut slot = self.shared.menu.borrow_mut();
        let menu = Rc::get_mut(slot.as_mut()?)?;
        Some(f(menu))
    }

    pub fn show(&mut self) -> Result<(), &'static str> {
        if self.shared.visible.get() {
            return Ok(());
        }
        self.shared.send_notify_icon_message(NIM_ADD)?;
        self.shared.visible.set(true);
        Ok(())
    }

    pub fn hide(&mut self) -> Result<(), &'static str> {
        self.set_balloon_icon(None);
        if !self.shared.visible.get() {
            return Ok(());
        }
        self.shared.send_notify_icon_message(NIM_DELETE)?;
        self.shared.visible.set(false);
        Ok(())
    }

    pub fn set_tooltip(&mut self, tooltip: impl Into<String>) -> Result<(), &'static str> {
        self.tooltip = tooltip.into();
        let old = self.shared.tooltip.replace(self.tooltip.clone());
        drop(old);
        if self.shared.visible.get() {
            self.shared.send_notify_icon_message(NIM_MODIFY)?;
        }
        Ok(())
    }

    /// Shows `hicon`, which stays owned by the caller (it is not destroyed with the tray icon),
    /// unless it is the handle the tray already owns: that one stays the tray's.
    pub fn set_icon(&mut self, hicon: HICON) -> Result<(), &'static str> {
        self.replace_icon(IconHandle::shared(hicon))
    }

    fn replace_icon(&mut self, new: IconHandle) -> Result<(), &'static str> {
        let old = self.shared.icon.borrow_mut().replace(new);
        let old_source = self.shared.source.replace(None);
        drop(old_source);
        let result = if self.shared.visible.get() {
            self.shared.send_notify_icon_message(NIM_MODIFY)
        } else {
            Ok(())
        };
        // An owned previous icon is destroyed only now that the shell shows the new one.
        drop(old);
        result
    }

    pub fn icon(&self) -> HICON {
        self.shared.icon.borrow().as_raw()
    }

    /// A tray icon showing `icon` at the size of a small icon (`QSystemTrayIcon(const QIcon &)`).
    /// The tray keeps `icon`, so it can build the `HICON` again when the taskbar is re-created.
    pub fn from_icon(
        tooltip: impl Into<String>,
        icon: &ferrokit_gui::image::Icon,
    ) -> Result<Box<Self>, &'static str> {
        let hicon = IconHandle::owned(Self::hicon_for_small_icon(icon)?);
        // If the tray cannot be created the guard destroys the icon.
        let tray = Self::with_icon(tooltip, hicon)?;
        let old = tray.shared.source.replace(Some(icon.clone()));
        drop(old);
        Ok(tray)
    }

    /// Shows `icon` (`QSystemTrayIcon::setIcon`) and keeps it for `TaskbarCreated`.
    pub fn set_icon_from(&mut self, icon: &ferrokit_gui::image::Icon) -> Result<(), &'static str> {
        let hicon = Self::hicon_for_small_icon(icon)?;
        let result = self.replace_icon(IconHandle::owned(hicon));
        // Kept even when the shell refused the change: the `HICON` is shown from now on.
        let old = self.shared.source.replace(Some(icon.clone()));
        drop(old);
        result
    }

    /// `QWindowsSystemTrayIcon::createIcon`: the size of a small icon (`SM_CXSMICON`,
    /// `SM_CYSMICON`) as far as `icon` has one (`QIcon::actualSize`), as one `HICON`.
    fn hicon_for_small_icon(icon: &ferrokit_gui::image::Icon) -> Result<HICON, &'static str> {
        use ferrokit_gui::geometry::primitives::Size;
        use ferrokit_gui::image::{IconMode, IconState};
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetSystemMetrics, SM_CXSMICON, SM_CYSMICON,
        };
        if icon.is_null() {
            return Err("null icon");
        }
        // SAFETY: `GetSystemMetrics` takes only an index constant and has no pointer arguments.
        let requested =
            unsafe { Size::new(GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON)) };
        let size = icon.actual_size(requested, IconMode::Normal, IconState::Off);
        Self::create_hicon_from_pixmap(&icon.pixmap(size, IconMode::Normal, IconState::Off))
    }

    pub fn is_visible(&self) -> bool {
        self.shared.visible.get()
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
        if !self.shared.visible.get() {
            return None;
        }
        // SAFETY: `NOTIFYICONIDENTIFIER` is a C struct of integers, a `GUID` and an `HWND`, for
        // which all-zero is a valid value; `cbSize` is set right below.
        let mut id: NOTIFYICONIDENTIFIER = unsafe { std::mem::zeroed() };
        id.cbSize = std::mem::size_of::<NOTIFYICONIDENTIFIER>() as u32;
        id.hWnd = self.shared.hwnd;
        id.uID = 1;
        let mut rect = windows_sys::Win32::Foundation::RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        // SAFETY: `id` is initialised with `cbSize`, `hWnd` (this tray's live window) and `uID`, as
        // `Shell_NotifyIconGetRect` requires; `id` and `rect` are live locals, `rect` being the
        // out pointer.
        let hr = unsafe { Shell_NotifyIconGetRect(&id, &mut rect) };
        if hr != 0 {
            return None;
        }
        // SAFETY: `MONITORINFO` is a C struct of integers and `RECT`s, for which all-zero is a
        // valid value; `cbSize` is set right below.
        let mut info: MONITORINFO = unsafe { std::mem::zeroed() };
        info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        // SAFETY: `MonitorFromWindow` takes the window handle by value and validates it; with
        // `MONITOR_DEFAULTTONEAREST` it always returns a monitor handle, which needs no cleanup.
        let monitor = unsafe { MonitorFromWindow(self.shared.hwnd, MONITOR_DEFAULTTONEAREST) };
        // SAFETY: `info` is a live `MONITORINFO` with `cbSize` set to its size, as the call
        // requires; `GetMonitorInfoW` validates `monitor`, and on failure leaves `info` zeroed (the
        // result is deliberately ignored).
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
        self.shared.hwnd
    }

    pub fn show_message(
        &mut self,
        title: &str,
        msg: &str,
        icon: crate::platform_tray::TrayMessageIcon,
        duration_ms: u32,
    ) -> Result<(), &'static str> {
        // `QSystemTrayIcon::showMessage` (both overloads) does nothing while the icon is hidden.
        if !self.shared.visible.get() || !self.supports_messages() {
            return Ok(());
        }
        // The previous message's icon is freed before this one is made.
        self.set_balloon_icon(None);
        let (flags, balloon) = Self::balloon_style(icon);
        let result = self.notify_balloon(
            title,
            msg,
            flags,
            balloon.as_ref().map_or(ptr::null_mut(), IconHandle::as_raw),
            duration_ms,
        );
        self.set_balloon_icon(balloon);
        result
    }

    /// Replaces the icon of the last balloon message; the old one is destroyed if owned. As
    /// `QWindowsSystemTrayIcon::m_hMessageIcon` the icon is kept after `NIM_MODIFY` returns (the
    /// `hBalloonIcon` documentation does not say the shell is done with the handle by then) and
    /// freed when the next message is shown, when the icon is hidden and when it is dropped.
    fn set_balloon_icon(&mut self, icon: Option<IconHandle>) {
        self.balloon_icon = icon;
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
            .clamp(1, 256) as u32
    }

    /// `QSystemTrayIcon::showMessage(title, msg, MessageIcon, msecs)` hands the platform the
    /// style's standard icon for the type (`SP_MessageBoxInformation`, `SP_MessageBoxWarning`,
    /// `SP_MessageBoxCritical`) and none for `NoIcon`; `QWindowsSystemTrayIcon::showMessage` then
    /// shows `NIIF_USER | NIIF_LARGE_ICON` with that icon, and `NIIF_INFO` when there is none
    /// (`NoIcon` included).
    /// `QWindowsTheme::standardPixmap` tries the shell's stock icon, then (for the types that
    /// have one, not these) a shell32 resource, then `LoadIcon(IDI_INFORMATION / IDI_WARNING /
    /// IDI_ERROR)`, and gives a null pixmap, hence `NIIF_INFO`, when all fail.
    fn balloon_style(icon: crate::platform_tray::TrayMessageIcon) -> (u32, Option<IconHandle>) {
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
        let owned_stock = Self::stock_icon(siid).map(IconHandle::owned);
        let shared = || Self::system_icon(idi).map(IconHandle::shared);
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
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetSystemMetrics, SM_CXICON, SM_CXSMICON,
        };
        // SAFETY: `SHSTOCKICONINFO` is a C struct of integers, an `HICON` and arrays, for which
        // all-zero is a valid value; `cbSize` is set right below.
        let mut info: SHSTOCKICONINFO = unsafe { std::mem::zeroed() };
        info.cbSize = std::mem::size_of::<SHSTOCKICONINFO>() as u32;
        // SAFETY: `info` is a live `SHSTOCKICONINFO` with `cbSize` set to its size, as
        // `SHGetStockIconInfo` requires; `siid` is an `SIID_*` value from `balloon_style`.
        if unsafe { SHGetStockIconInfo(siid, SHGSI_ICONLOCATION, &mut info) } < 0 {
            return None;
        }
        // SAFETY: `GetSystemMetrics` takes only an index constant and has no pointer arguments.
        let size = unsafe {
            Self::stock_icon_size(GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CXICON))
        };
        let mut hicon: HICON = ptr::null_mut();
        // SAFETY: `info.szPath` is the fixed `MAX_PATH` array filled by the successful
        // `SHGetStockIconInfo` above (it started zeroed, so it is NUL-terminated either way);
        // `hicon` is a live local out pointer; the null `phiconSmall` is documented as optional.
        let hr = unsafe {
            SHDefExtractIconW(
                info.szPath.as_ptr(),
                info.iIcon,
                0,
                &mut hicon,
                ptr::null_mut(),
                size,
            )
        };
        (hr >= 0 && !hicon.is_null()).then_some(hicon)
    }

    /// A system icon (`LoadIcon(nullptr, idi)`), at the size the system chooses. It is a shared
    /// handle owned by the system: it must never be passed to `DestroyIcon`.
    fn system_icon(idi: windows_sys::core::PCWSTR) -> Option<HICON> {
        // SAFETY: with a null `hInstance`, `LoadIconW` takes an `IDI_*` resource identifier, which
        // is what every caller of `system_icon` passes (`balloon_style` and the tests), so no
        // string is read from `idi`; the returned shared handle is never destroyed.
        let hicon =
            unsafe { windows_sys::Win32::UI::WindowsAndMessaging::LoadIconW(ptr::null_mut(), idi) };
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
        if !self.shared.visible.get() || !self.supports_messages() {
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
        let balloon = IconHandle::owned(Self::hicon_for_balloon(icon)?);
        let result = self.notify_balloon(
            title,
            msg,
            NIIF_USER | NIIF_LARGE_ICON,
            balloon.as_raw(),
            duration_ms,
        );
        self.set_balloon_icon(Some(balloon));
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
        // SAFETY: `GetSystemMetrics` takes only an index constant and has no pointer arguments.
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
        // SAFETY: `NOTIFYICONDATAW` is a C struct of integers, raw pointers and arrays, for which
        // all-zero is a valid value (null handles, empty strings); `cbSize` is set right below.
        let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = self.shared.hwnd;
        nid.uID = 1;
        nid.uFlags = NIF_INFO | NIF_SHOWTIP;

        // A message that is empty next to a title still shows (qwindowssystemtrayicon.cpp).
        let msg = if msg.is_empty() && !title.is_empty() {
            " "
        } else {
            msg
        };
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

        // SAFETY: `nid` is fully initialised with `cbSize` set to its size; `hWnd` is this tray's
        // own window; `hBalloonIcon` is `balloon_icon`, which the caller keeps alive across the
        // call; `szInfo` (256) and `szInfoTitle` (64) are NUL-terminated (`msg_len <= 255`,
        // `title_len <= 63`). `nid` outlives the call, and the shell only reads it.
        let res = unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) };
        if res == 0 {
            // SAFETY: `cls` is a NUL-terminated UTF-16 vector that lives to the end of the block;
            // the null window name is documented as allowed by `FindWindowW`.
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

    /// Shows the context menu at the cursor and emits `on_menu_action` with the chosen command;
    /// does nothing when there is no menu. (It cannot run during `with_menu_mut`: that holds
    /// `&mut self`.)
    pub fn show_context_menu(&self) {
        let mut pt = POINT { x: 0, y: 0 };
        // SAFETY: `GetCursorPos` writes one `POINT` through the pointer; `pt` is a live local.
        unsafe {
            GetCursorPos(&mut pt);
        }
        self.show_context_menu_at(crate::screen::native_cursor_to_logical(Point::new(
            pt.x, pt.y,
        )));
    }

    /// Shows the context menu at `screen_pos`; otherwise as `show_context_menu`.
    pub fn show_context_menu_at(&self, screen_pos: Point) {
        self.shared.show_context_menu_at(screen_pos);
    }

    pub fn create_hicon_from_pixmap(pixmap: &Pixmap) -> Result<HICON, &'static str> {
        let w = pixmap.physical_width() as i32;
        let h = pixmap.physical_height() as i32;

        if w <= 0 || h <= 0 {
            return Err("invalid pixmap size");
        }

        let src_data = pixmap.data();
        let pixel_count = (w as usize) * (h as usize);
        if src_data.len() < pixel_count * 4 {
            return Err("pixmap data shorter than its size");
        }

        let Some(hdc) = MemDc::create_compatible(ptr::null_mut()) else {
            return Err("CreateCompatibleDC failed");
        };
        let created = create_dib_section(hdc.as_raw(), w as u32, h as u32);
        // The bitmap is never selected into the DC, so the DC is not needed any more.
        drop(hdc);
        // The guards delete both bitmaps on every path out; `CreateIconIndirect` copies them.
        let Some((hbm_color, bits_ptr)) = created else {
            return Err("CreateDIBSection failed");
        };

        // SAFETY: the DIB section holds `w * h * 4` bytes (`pixel_count * 4`), and `src_data` was
        // checked above to hold at least as many.
        unsafe {
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
        }

        // SAFETY: a null bits pointer asks GDI for an uninitialised monochrome bitmap.
        let Some(hbm_mask) =
            OwnedBitmap::from_raw(unsafe { CreateBitmap(w, h, 1, 1, ptr::null()) })
        else {
            return Err("CreateBitmap failed");
        };

        let icon_info = ICONINFO {
            fIcon: 1,
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: hbm_mask.as_raw(),
            hbmColor: hbm_color.as_raw(),
        };

        // SAFETY: `icon_info` points at live bitmaps for the duration of the call.
        let hicon = unsafe { CreateIconIndirect(&icon_info) };
        if hicon.is_null() {
            return Err("CreateIconIndirect failed");
        }

        Ok(hicon)
    }

    // `DestroyIcon` validates the handle and fails for one that is not an icon. Destroying an
    // icon that someone else still uses is an ownership error, not memory unsafety here.
    #[allow(clippy::not_unsafe_ptr_arg_deref)]
    pub fn destroy_hicon(hicon: HICON) {
        if !hicon.is_null() {
            // SAFETY: `hicon` is non-null (checked above); `DestroyIcon` validates the handle and
            // fails for one that is not an icon; ownership of the icon is the caller's, as the
            // comment above says.
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
    // SAFETY: `subkey` and `name` are NUL-terminated UTF-16 vectors that outlive the calls; `hkey`
    // is written by the successful `RegOpenKeyExW` and closed exactly once, right after the query;
    // `data` is a live `u32` and `size` starts at 4, so `RegQueryValueExW` (per its documentation)
    // writes at most 4 bytes through the `*mut u8`.
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
        let hwnd = self.shared.hwnd;
        // Unrouted first: the messages `DestroyWindow` sends then take the `DefWindowProcW` path.
        // `try_with`: during thread teardown the table may already be gone (nothing to remove).
        let _ = TRAY_INSTANCES.try_with(|registry| registry.unregister(hwnd as isize));
        // A window procedure still running a slot of this tray (one that dropped it) holds its own
        // `Rc` to the state and checks this flag after the emit.
        self.shared.closed.set(true);
        if !hwnd.is_null() {
            // SAFETY: `hwnd` is non-null (checked above), was created by this thread in
            // `with_icon` (`TrayIcon` is `!Send`) and is destroyed only here, once.
            unsafe {
                DestroyWindow(hwnd);
            }
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
        self.inner
            .show_message_with_icon(title, msg, icon, duration_ms)
    }
}
#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::menu::PlatformMenu;
    use ferrokit_gui::tiny_skia::Color;
    use windows_sys::Win32::Graphics::Gdi::DeleteObject;
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
        use windows_sys::Win32::UI::WindowsAndMessaging::{DestroyMenu, GetSubMenu, IsMenu};
        let mut inner = Menu::new();
        inner.add_action(12, "Inner");
        menu.add_submenu("More", inner);
        // SAFETY: `create_native_hmenu` requires that the `HMENU` be destroyed: `hmenu` is destroyed
        // exactly once by `DestroyMenu` below; `GetSubMenu` and `IsMenu` validate their handles, so
        // querying `sub` after the parent's destruction is defined (`IsMenu` then returns 0).
        unsafe {
            let hmenu = menu
                .create_native_hmenu()
                .expect("failed to create native menu");
            assert!(!hmenu.is_null());
            // The parent owns the appended submenu and the builder must not have destroyed it.
            let sub = GetSubMenu(hmenu, 4);
            assert_ne!(IsMenu(sub), 0);
            assert_ne!(DestroyMenu(hmenu), 0);
            assert_eq!(IsMenu(sub), 0);
        }
    }

    #[test]
    fn the_tray_window_is_routed_to_its_state_until_the_tray_is_dropped() {
        let mut pixmap = Pixmap::new(16, 16).expect("failed to create pixmap");
        pixmap.fill(Color::from_rgba8(1, 2, 3, 255));
        let tray = TrayIcon::from_icon(
            "Router Test",
            &ferrokit_gui::image::Icon::from_pixmap(&pixmap),
        )
        .expect("from_icon");
        // Moved out of its first place, as callers do: the route does not depend on the address.
        let moved = vec![tray];
        let tray = moved.into_iter().next().unwrap();
        let hwnd = tray.hwnd();
        let routed = lookup_tray(hwnd).expect("the window is routed while the tray lives");
        assert!(
            Rc::ptr_eq(&routed, &tray.shared),
            "to this tray's own state"
        );
        drop(routed);
        drop(tray);
        assert!(
            lookup_tray(hwnd).is_none(),
            "unrouted once the tray is dropped"
        );
    }

    /// `set_icon` with the handle the tray already shows must not change who owns it: an icon
    /// the tray made stays the tray's and is destroyed with it.
    #[test]
    fn setting_the_icon_the_tray_already_owns_keeps_it_owned() {
        use windows_sys::Win32::UI::WindowsAndMessaging::GetIconInfo;
        let alive = |icon: HICON| {
            // SAFETY: `ICONINFO` is a C struct of integers and bitmap handles, for which all-zero is
            // a valid value; `GetIconInfo` overwrites it.
            let mut info: ICONINFO = unsafe { std::mem::zeroed() };
            // SAFETY: `GetIconInfo` validates `icon` (it fails for a destroyed handle, leaving
            // `info` zeroed) and writes one `ICONINFO` through the pointer; `info` is a live local.
            let ok = unsafe { GetIconInfo(icon, &mut info) } != 0;
            if ok {
                // SAFETY: after a successful `GetIconInfo` the caller owns `hbmColor` and `hbmMask`
                // (per its documentation); each is deleted exactly once here.
                unsafe {
                    DeleteObject(info.hbmColor);
                    DeleteObject(info.hbmMask);
                }
            }
            ok
        };
        let pixmap = Pixmap::new(16, 16).unwrap();
        let mut tray = TrayIcon::from_icon(
            "same handle",
            &ferrokit_gui::image::Icon::from_pixmap(&pixmap),
        )
        .expect("tray");
        let hicon = tray.icon();
        assert!(alive(hicon));

        tray.set_icon(hicon).expect("set the same icon");
        assert_eq!(tray.icon(), hicon);
        assert!(alive(hicon), "the tray must not destroy the icon it shows");

        drop(tray);
        let leaked = alive(hicon);
        if leaked {
            // Do not leak the handle out of the failing test.
            TrayIcon::destroy_hicon(hicon);
        }
        assert!(
            !leaked,
            "the tray still owns its icon and destroys it on drop"
        );
    }

    #[test]
    fn the_balloon_icon_lives_until_the_next_message_or_hide() {
        // `QWindowsSystemTrayIcon::m_hMessageIcon`: kept after the notification, freed before the
        // next message's icon is made and on cleanup.
        use crate::platform_tray::TrayMessageIcon;
        use windows_sys::Win32::UI::WindowsAndMessaging::GetIconInfo;
        let alive = |icon: HICON| {
            // SAFETY: `ICONINFO` is a C struct of integers and bitmap handles, for which all-zero is
            // a valid value; `GetIconInfo` overwrites it.
            let mut info: ICONINFO = unsafe { std::mem::zeroed() };
            // SAFETY: `GetIconInfo` validates `icon` (it fails for a destroyed handle, leaving
            // `info` zeroed) and writes one `ICONINFO` through the pointer; `info` is a live local.
            let ok = unsafe { GetIconInfo(icon, &mut info) } != 0;
            if ok {
                // SAFETY: after a successful `GetIconInfo` the caller owns `hbmColor` and `hbmMask`
                // (per its documentation); each is deleted exactly once here.
                unsafe {
                    DeleteObject(info.hbmColor);
                    DeleteObject(info.hbmMask);
                }
            }
            ok
        };
        let pixmap = Pixmap::new(16, 16).unwrap();
        let mut tray =
            TrayIcon::from_icon("balloon", &ferrokit_gui::image::Icon::from_pixmap(&pixmap))
                .expect("tray");
        if !tray.supports_messages() {
            return;
        }
        // `QSystemTrayIcon::showMessage` does nothing while the icon is hidden (`d->visible`):
        // no notification, no icon kept.
        assert!(!tray.is_visible());
        tray.show_message(" ", "Hidden", TrayMessageIcon::Information, 1000)
            .unwrap();
        tray.show_message_with_icon(
            " ",
            "Hidden",
            &ferrokit_gui::image::Icon::from_pixmap(&pixmap),
            1000,
        )
        .unwrap();
        assert!(tray.balloon_icon.is_none());

        tray.show().expect("show");
        tray.show_message(" ", "Info", TrayMessageIcon::Information, 1000)
            .unwrap();
        let first = tray.balloon_icon.as_ref().expect("kept after the message");
        assert!(
            first.is_owned() && alive(first.as_raw()),
            "still alive after the call"
        );
        // A small image is scaled up to the large icon size, which the shell accepts.
        tray.show_message_with_icon(
            " ",
            "Custom",
            &ferrokit_gui::image::Icon::from_pixmap(&pixmap),
            1000,
        )
        .unwrap();
        let second = tray
            .balloon_icon
            .as_ref()
            .expect("replaced by the next message");
        assert!(second.is_owned() && alive(second.as_raw()));
        let handle = second.as_raw();
        // A message without an icon frees the previous one and keeps none.
        tray.show_message(" ", "None", TrayMessageIcon::NoIcon, 1000)
            .unwrap();
        assert!(tray.balloon_icon.is_none());
        assert!(!alive(handle), "freed by the next message");
        tray.show_message(" ", "Info", TrayMessageIcon::Information, 1000)
            .unwrap();
        let handle = tray.balloon_icon.as_ref().expect("kept").as_raw();
        tray.hide().unwrap();
        assert!(tray.balloon_icon.is_none());
        assert!(!alive(handle), "freed on hide");
    }

    #[test]
    fn the_stock_icon_size_follows_the_style_icon_sizes_of_the_theme() {
        // Sizes `SM_CXSMICON`, `SM_CXICON`, 1.5 x and 8 x `SM_CXICON`; the first whose area is at
        // least 256 x 256, else the largest, never above 256 px.
        assert_eq!(
            TrayIcon::stock_icon_size(16, 32),
            256,
            "100%: 16, 32, 48, 256"
        );
        assert_eq!(
            TrayIcon::stock_icon_size(20, 40),
            256,
            "125%: 20, 40, 60, 320 -> 256"
        );
        assert_eq!(
            TrayIcon::stock_icon_size(16, 16),
            128,
            "16, 16, 24, 128: the largest"
        );
        assert_eq!(
            TrayIcon::stock_icon_size(32, 40),
            256,
            "32, 40, 60, 320 -> 256"
        );
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
            // SAFETY: `ICONINFO` is a C struct of integers and bitmap handles, for which all-zero is
            // a valid value; `GetIconInfo` overwrites it.
            let mut info: ICONINFO = unsafe { std::mem::zeroed() };
            // SAFETY: `icon` is the live shared system icon from `system_icon`; `info` is a live
            // local written through the pointer.
            assert_ne!(unsafe { GetIconInfo(icon, &mut info) }, 0);
            // SAFETY: `BITMAP` is a C struct of integers and a pointer, for which all-zero is a
            // valid value; `GetObjectW` overwrites it.
            let mut bm: BITMAP = unsafe { std::mem::zeroed() };
            // SAFETY: `hbmColor` was just returned by `GetIconInfo`; `GetObjectW` writes at most
            // `size_of_val(&bm)` bytes into the live `bm` (a null handle makes it return 0 and write
            // nothing); the caller owns both bitmaps (per the `GetIconInfo` documentation) and
            // deletes each exactly once.
            unsafe {
                GetObjectW(
                    info.hbmColor,
                    std::mem::size_of_val(&bm) as i32,
                    &mut bm as *mut _ as *mut _,
                );
                DeleteObject(info.hbmColor);
                DeleteObject(info.hbmMask);
            }
            assert!(
                bm.bmWidth >= 16 && bm.bmWidth == bm.bmHeight,
                "square: {}",
                bm.bmWidth
            );
            // A shared system icon is not destroyed: it still works after its guard drops.
            drop(IconHandle::shared(icon));
            // SAFETY: `ICONINFO` is a C struct of integers and bitmap handles, for which all-zero is
            // a valid value; `GetIconInfo` overwrites it.
            let mut again: ICONINFO = unsafe { std::mem::zeroed() };
            assert_ne!(
                // SAFETY: `icon` is the shared system icon, which dropping `IconHandle::shared` did
                // not destroy; `again` is a live local written through the pointer.
                unsafe { GetIconInfo(icon, &mut again) },
                0,
                "the shared icon survives"
            );
            // SAFETY: `again` was filled by the successful `GetIconInfo` above; the caller owns both
            // bitmaps (per its documentation) and deletes each exactly once.
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
            let icon = balloon.as_raw();
            assert!(!icon.is_null());
            // Extracted at 256 px, the largest size the balloon asks the style icon for.
            // SAFETY: `ICONINFO` is a C struct of integers and bitmap handles, for which all-zero is
            // a valid value; `GetIconInfo` overwrites it.
            let mut info: ICONINFO = unsafe { std::mem::zeroed() };
            assert_ne!(
                // SAFETY: `icon` is the live handle held by `balloon`; `info` is a live local written
                // through the pointer.
                unsafe {
                    windows_sys::Win32::UI::WindowsAndMessaging::GetIconInfo(icon, &mut info)
                },
                0
            );
            // SAFETY: `BITMAP` is a C struct of integers and a pointer, for which all-zero is a
            // valid value; `GetObjectW` overwrites it.
            let mut bm: windows_sys::Win32::Graphics::Gdi::BITMAP = unsafe { std::mem::zeroed() };
            // SAFETY: `hbmColor` was just returned by `GetIconInfo`; `GetObjectW` writes at most
            // `size_of_val(&bm)` bytes into the live `bm`; the caller owns both bitmaps (per the
            // `GetIconInfo` documentation) and deletes each exactly once.
            unsafe {
                windows_sys::Win32::Graphics::Gdi::GetObjectW(
                    info.hbmColor,
                    std::mem::size_of_val(&bm) as i32,
                    &mut bm as *mut _ as *mut _,
                );
                DeleteObject(info.hbmColor);
                DeleteObject(info.hbmMask);
            }
            // SAFETY: `GetSystemMetrics` takes only an index constant and has no pointer arguments.
            let expected = unsafe {
                use windows_sys::Win32::UI::WindowsAndMessaging::{
                    GetSystemMetrics, SM_CXICON, SM_CXSMICON,
                };
                TrayIcon::stock_icon_size(
                    GetSystemMetrics(SM_CXSMICON),
                    GetSystemMetrics(SM_CXICON),
                )
            };
            assert_eq!(bm.bmWidth, expected as i32, "{kind:?}");
            // An owned icon is destroyed when its guard drops.
            let (hicon, owned) = (balloon.as_raw(), balloon.is_owned());
            assert!(owned, "{kind:?}: the extracted stock icon is ours");
            drop(balloon);
            // SAFETY: `ICONINFO` is a C struct of integers and bitmap handles, for which all-zero is
            // a valid value; `GetIconInfo` overwrites it.
            let mut gone: ICONINFO = unsafe { std::mem::zeroed() };
            assert_eq!(
                // SAFETY: `hicon` was destroyed by dropping `balloon`; `GetIconInfo` validates the
                // handle and fails for it, and `gone` is a live local.
                unsafe {
                    windows_sys::Win32::UI::WindowsAndMessaging::GetIconInfo(hicon, &mut gone)
                },
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
            // SAFETY: `ICONINFO` is a C struct of integers and bitmap handles, for which all-zero is
            // a valid value; `GetIconInfo` overwrites it.
            let mut info: ICONINFO = unsafe { std::mem::zeroed() };
            // SAFETY: `hicon` was just made by `hicon_for_balloon` and is destroyed only below;
            // `info` is a live local written through the pointer.
            assert_ne!(unsafe { GetIconInfo(hicon, &mut info) }, 0);
            // SAFETY: `BITMAP` is a C struct of integers and a pointer, for which all-zero is a
            // valid value; `GetObjectW` overwrites it.
            let mut bm: BITMAP = unsafe { std::mem::zeroed() };
            // SAFETY: `hbmColor` was just returned by `GetIconInfo`; `GetObjectW` writes at most
            // `size_of::<BITMAP>()` bytes into the live `bm`; the caller owns both bitmaps (per the
            // `GetIconInfo` documentation) and deletes each exactly once.
            unsafe {
                GetObjectW(
                    info.hbmColor,
                    std::mem::size_of::<BITMAP>() as i32,
                    &mut bm as *mut _ as *mut _,
                );
                DeleteObject(info.hbmColor);
                DeleteObject(info.hbmMask);
            }
            TrayIcon::destroy_hicon(hicon);
            bm.bmWidth
        };
        assert_eq!(
            width_of(&[16, 128]),
            128,
            "the image that fits, not a fixed 32 px"
        );
        assert_eq!(width_of(&[16, 512]), 256, "scaled down to fit 256 px");
        // SAFETY: `GetSystemMetrics` takes only an index constant and has no pointer arguments.
        let large = unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::GetSystemMetrics(
                windows_sys::Win32::UI::WindowsAndMessaging::SM_CXICON,
            )
        };
        assert_eq!(
            width_of(&[16]),
            large,
            "a smaller image is scaled up to SM_CXICON"
        );
        assert_eq!(
            width_of(&[large as u32 + 8]),
            large + 8,
            "a larger one is not touched"
        );
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
        assert_eq!(
            TrayIcon::balloon_icon_size(Size::new(48, 48), large),
            Size::new(48, 48)
        );
        assert_eq!(
            TrayIcon::balloon_icon_size(Size::new(40, 33), Size::new(40, 40)),
            Size::new(40, 40)
        );
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
        // SAFETY: `tray.hwnd()` is the live window of `tray` (alive to the end of the test), created
        // on this thread, so `SendMessageW` calls `tray_window_proc` synchronously here; `lParam`
        // is an integer event code and `wParam` is 0, so no pointers cross the call.
        unsafe {
            SendMessageW(tray.hwnd(), WM_TRAY_CALLBACK, 0, NIN_SELECT as isize);
        }
        assert!(
            activated_called.load(Ordering::SeqCst),
            "receiving NIN_SELECT should trigger on_activated signal"
        );

        // Simulate explorer.exe crash and restart broadcast (TaskbarCreated)
        // SAFETY: same window and thread as above; `wParam` and `lParam` are 0, so no pointers
        // cross the call.
        unsafe {
            SendMessageW(tray.hwnd(), tray.shared.taskbar_created_msg, 0, 0);
        }

        TrayIcon::destroy_hicon(hicon);
    }
}
