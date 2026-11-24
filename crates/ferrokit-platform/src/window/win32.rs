// SPDX-License-Identifier: LGPL-3.0-only
use super::*;

#[cfg(windows)]
static DCOMP_ENABLED_OVERRIDE: AtomicI8 = AtomicI8::new(0);

/// Queries whether DirectComposition presenter is enabled via `FERROKIT_ENABLE_DCOMP=1`
/// or programmatic override (`set_dcomp_enabled`).
#[cfg(windows)]
pub fn is_dcomp_enabled() -> bool {
    match DCOMP_ENABLED_OVERRIDE.load(Ordering::Relaxed) {
        1 => true,
        -1 => false,
        _ => {
            static DCOMP_ENV_ONCE: Once = Once::new();
            static DCOMP_ENV_ENABLED: AtomicI8 = AtomicI8::new(0);
            DCOMP_ENV_ONCE.call_once(|| {
                let enabled = std::env::var("FERROKIT_ENABLE_DCOMP")
                    .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                    .unwrap_or(false);
                DCOMP_ENV_ENABLED.store(if enabled { 1 } else { -1 }, Ordering::Relaxed);
            });
            DCOMP_ENV_ENABLED.load(Ordering::Relaxed) == 1
        }
    }
}

/// Overrides DComp policy for testing or runtime control (Some(true) = force enable, Some(false) = force disable, None = follow env).
#[cfg(windows)]
pub fn set_dcomp_enabled(enabled: Option<bool>) {
    let val = match enabled {
        Some(true) => 1,
        Some(false) => -1,
        None => 0,
    };
    DCOMP_ENABLED_OVERRIDE.store(val, Ordering::Relaxed);
}

#[cfg(windows)]
static REGISTER_WINDOW_CLASS_ONCE: Once = Once::new();
#[cfg(windows)]
pub(crate) const NATIVE_WINDOW_CLASS_NAME: &[u16] = &[
    'Q' as u16, 't' as u16, 'r' as u16, 's' as u16, 'N' as u16, 'a' as u16, 't' as u16, 'i' as u16,
    'v' as u16, 'e' as u16, 'W' as u16, 'i' as u16, 'n' as u16, 'd' as u16, 'o' as u16, 'w' as u16,
    'C' as u16, 'l' as u16, 'a' as u16, 's' as u16, 's' as u16, 0,
];

#[cfg(windows)]
pub(super) fn ensure_native_window_class_registered() {
    // SAFETY: the closure's block runs once: `GetModuleHandleW(null)` returns this module's handle;
    // `wc` is a fully initialised WNDCLASSEXW with `cbSize` set and a `'static` wndproc and class
    // name (`NATIVE_WINDOW_CLASS_NAME` is NUL-terminated), outliving the call; LoadCursorW with a
    // null instance and IDC_ARROW loads the shared system cursor. RegisterClassExW copies `wc`.
    REGISTER_WINDOW_CLASS_ONCE.call_once(|| unsafe {
        let h_instance = GetModuleHandleW(ptr::null());
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            // Qt registers every window class with CS_DBLCLKS (qwindowswindowclassdescription.cpp:67).
            style: CS_DBLCLKS,
            lpfnWndProc: Some(native_window_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: h_instance,
            hIcon: ptr::null_mut(),
            hCursor: LoadCursorW(ptr::null_mut(), IDC_ARROW),
            hbrBackground: ptr::null_mut(),
            lpszMenuName: ptr::null(),
            lpszClassName: NATIVE_WINDOW_CLASS_NAME.as_ptr(),
            hIconSm: ptr::null_mut(),
        };
        RegisterClassExW(&wc);
    });
}

#[cfg(windows)]
pub struct NativeWindow {
    pub(super) hwnd: HWND,
    pub(super) title: String,
    pub(super) flags: WindowFlags,
    pub(super) geometry: Rect,
    pub(super) drop_target: Option<crate::drag_drop::win32_ole::DropTargetRegistration>,
    pub(super) opacity: f32,
    pub(super) size_limits: crate::size_limits::SizeLimits,
    pub(super) layered_surface: Option<crate::surface::WindowsSurface>,
    pub(super) presenter: Option<crate::presenter::WindowsPresenter>,
    pub(super) owner_thread: std::thread::ThreadId,
    pub(super) state_flags: std::cell::Cell<PlatformWindowStateFlags>,
    pub(super) ime_enabled: std::cell::Cell<bool>,
    pub(super) target_pos: Option<ferrokit_gui::geometry::Point>,
    pub(super) dcomp_downgraded: bool,
    /// A backdrop other than `BackdropType::None` is applied (it keeps `WS_EX_LAYERED` in place).
    pub(super) backdrop_active: bool,
    /// The requested backdrop and whether its Acrylic blur is suspended.
    pub(super) backdrop_state: crate::backdrop::BackdropState,
}

// Deliberately neither `Send` nor `Sync`: it owns an `HWND` (window APIs such as `DestroyWindow`
// require the creating thread), a raw OLE drop-target pointer and unsynchronised `Cell` state, and
// the window procedure reaches it from that thread only. `owner_thread` records that thread.

/// `set_stays_on_top`'s `SetWindowPos` insert-after target: `HWND_TOPMOST` for the topmost band,
/// `HWND_NOTOPMOST` to leave it.
#[cfg(windows)]
pub(super) fn topmost_insert_after(enabled: bool) -> HWND {
    use windows_sys::Win32::UI::WindowsAndMessaging::{HWND_NOTOPMOST, HWND_TOPMOST};
    if enabled {
        HWND_TOPMOST
    } else {
        HWND_NOTOPMOST
    }
}

#[cfg(windows)]
impl NativeWindow {
    pub fn new(title: &str, rect: Rect, flags: WindowFlags) -> Result<Self, &'static str> {
        ensure_native_window_class_registered();

        let mut dw_style = WS_CLIPCHILDREN | WS_CLIPSIBLINGS;
        let mut dw_ex_style = 0u32;

        if flags.contains(WindowFlags::LAYERED) || flags.contains(WindowFlags::TOOLTIP) {
            dw_style |= WS_POPUP;
        } else if flags.contains(WindowFlags::CUSTOM_FRAMELESS) {
            dw_style |= WS_THICKFRAME | WS_CAPTION | WS_MINIMIZEBOX | WS_MAXIMIZEBOX;
        } else if flags.contains(WindowFlags::FRAMELESS) {
            dw_style |= WS_POPUP;
        } else {
            dw_style |= WS_OVERLAPPEDWINDOW;
        }

        if flags.contains(WindowFlags::STAYS_ON_TOP) || flags.contains(WindowFlags::TOOLTIP) {
            dw_ex_style |= WS_EX_TOPMOST;
        }
        if flags.contains(WindowFlags::TOOLTIP) {
            dw_ex_style |= WS_EX_NOACTIVATE;
        }
        if flags.contains(WindowFlags::TOOL) || flags.contains(WindowFlags::TOOLTIP) {
            dw_ex_style |= WS_EX_TOOLWINDOW;
        } else {
            dw_ex_style |= WS_EX_APPWINDOW;
        }
        if flags.contains(WindowFlags::LAYERED) {
            dw_ex_style |= WS_EX_LAYERED;
        }
        if flags.contains(WindowFlags::CLICK_THROUGH) {
            dw_ex_style |= WS_EX_TRANSPARENT;
        }

        let wide_title: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();

        // SAFETY: the class was registered by `ensure_native_window_class_registered` above; the
        // class name and `wide_title` are NUL-terminated UTF-16 buffers that outlive the call (the
        // title is copied by the system); null parent/menu/param are permitted for a top-level
        // window; a null result is checked right after.
        let hwnd = unsafe {
            let h_instance = GetModuleHandleW(ptr::null());
            CreateWindowExW(
                dw_ex_style,
                NATIVE_WINDOW_CLASS_NAME.as_ptr(),
                wide_title.as_ptr(),
                dw_style,
                rect.x,
                rect.y,
                rect.width,
                rect.height,
                ptr::null_mut(),
                ptr::null_mut(),
                h_instance,
                ptr::null(),
            )
        };

        if hwnd.is_null() {
            return Err("CreateWindowExW failed");
        }

        // A frameless window has no frame of its own to darken (`Qt::FramelessWindowHint`).
        crate::dark_mode::window_created(
            hwnd,
            flags.intersects(
                WindowFlags::FRAMELESS
                    | WindowFlags::CUSTOM_FRAMELESS
                    | WindowFlags::LAYERED
                    | WindowFlags::TOOLTIP,
            ),
        );

        crate::ime::set_window_ime_enabled(hwnd, false);

        if flags.contains(WindowFlags::CUSTOM_FRAMELESS) && !flags.contains(WindowFlags::LAYERED) {
            // SAFETY: `hwnd` is the window just created above (non-null, checked); `margins` is a
            // local MARGINS that outlives the call; the window is on this thread.
            unsafe {
                let margins = MARGINS {
                    cxLeftWidth: 1,
                    cxRightWidth: 1,
                    cyTopHeight: 0,
                    cyBottomHeight: 1,
                };
                DwmExtendFrameIntoClientArea(hwnd, &margins);
                SetWindowPos(
                    hwnd,
                    ptr::null_mut(),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
                );
            }
            set_window_frameless_config(hwnd, CustomFramelessConfig::default());
        }

        let layered_surface = if flags.contains(WindowFlags::LAYERED) {
            crate::surface::WindowsSurface::create(
                hwnd,
                rect.width.max(1) as u32,
                rect.height.max(1) as u32,
            )
            .ok()
        } else {
            None
        };

        // A click-through window needs `WS_EX_LAYERED` with attributes (`setWindowLayered`).
        if flags.contains(WindowFlags::CLICK_THROUGH) && !flags.contains(WindowFlags::LAYERED) {
            apply_system_window_opacity(
                hwnd,
                LayeredNeeds {
                    opacity: 1.0,
                    click_through: true,
                    per_pixel: layered_surface.is_some(),
                    backdrop: false,
                },
            );
        }

        Ok(Self {
            hwnd,
            title: title.to_string(),
            flags,
            geometry: rect,
            drop_target: None,
            opacity: 1.0,
            size_limits: crate::size_limits::SizeLimits::default(),
            layered_surface,
            presenter: None,
            owner_thread: std::thread::current().id(),
            state_flags: std::cell::Cell::new(PlatformWindowStateFlags::NONE),
            ime_enabled: std::cell::Cell::new(false),
            target_pos: None,
            dcomp_downgraded: false,
            backdrop_active: false,
            backdrop_state: crate::backdrop::BackdropState::new(),
        })
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn flags(&self) -> WindowFlags {
        self.flags
    }

    pub fn geometry(&self) -> Rect {
        #[cfg(windows)]
        if !self.hwnd.is_null() {
            // SAFETY: RECT is a plain integer struct, so the all-zero value is valid.
            let mut r: RECT = unsafe { std::mem::zeroed() };
            // SAFETY: `self.hwnd` is non-null (checked above) and is this window's own handle (set
            // in `new`, nulled only after `DestroyWindow`); `r` is a local out pointer valid for the
            // call; a 0 return falls back to the cached geometry.
            if unsafe { GetWindowRect(self.hwnd, &mut r) } != 0 {
                return Rect::new(r.left, r.top, r.right - r.left, r.bottom - r.top);
            }
        }
        self.geometry
    }

    pub fn bind_event_loop(&self, handle: EventLoopHandle, receiver: ObjectId) {
        if !self.hwnd.is_null() {
            register_window_event_binding(self.hwnd, handle, receiver);
        }
    }

    pub fn unbind_event_loop(&self) {
        if !self.hwnd.is_null() {
            unregister_window_event_binding(self.hwnd);
        }
    }

    pub fn set_event_handler(&mut self, handler: Box<dyn WindowSystemEventHandler>) {
        if !self.hwnd.is_null() {
            set_window_event_handler(self.hwnd, handler);
        }
    }

    pub fn show(&self) {
        if !self.hwnd.is_null() {
            // SAFETY: `self.hwnd` is non-null (checked above) and is this window's own handle (set
            // in `new`, nulled only after `DestroyWindow`); ShowWindow takes no pointers.
            unsafe {
                let cmd = if self.flags.contains(WindowFlags::TOOLTIP) {
                    SW_SHOWNOACTIVATE
                } else {
                    SW_SHOW
                };
                ShowWindow(self.hwnd, cmd);
            }
        }
    }

    pub fn hide(&self) {
        if !self.hwnd.is_null() {
            // SAFETY: as in `show`: `self.hwnd` is this window's own handle (live, or stale after
            // WM_CLOSE destroyed it, which user32 rejects); no pointers.
            unsafe {
                ShowWindow(self.hwnd, SW_HIDE);
            }
        }
    }

    /// `QWindowsWindow::raise`: `SetWindowPos(HWND_TOP, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE)`.
    /// Reorders the window in the z-order without activating it; a topmost window stays in the
    /// topmost band.
    pub fn raise(&self) {
        use windows_sys::Win32::UI::WindowsAndMessaging::HWND_TOP;
        if !self.hwnd.is_null() {
            // SAFETY: `self.hwnd` is non-null (checked above) and is this window's own handle (set
            // in `new`, nulled only after `DestroyWindow`); `HWND_TOP` is a predefined constant.
            unsafe {
                SetWindowPos(
                    self.hwnd,
                    HWND_TOP,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
            }
        }
    }

    /// [`PlatformWindow::stack_under`]: `SetWindowPos(self, hWndInsertAfter = sibling,
    /// SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE)` puts the window right after `sibling` in the
    /// z-order, which is directly below it. A `sibling` in the other topmost band is refused and
    /// nothing changes: the system would move the window into the band of the window it is
    /// inserted after, behind the back of [`WindowFlags::STAYS_ON_TOP`].
    ///
    /// The result is read back, because the system keeps an owned window above its owner and a
    /// `sibling` that is no top-level window cannot precede this one. Windows owned by this
    /// window (the default IME window the system creates for it, for one) stay above it by
    /// the same rule, so they may sit between `sibling` and the window and are not counted as
    /// windows in between.
    pub fn stack_under(&self, sibling: isize) -> bool {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GW_HWNDPREV, GW_OWNER, GWL_EXSTYLE, GetWindow, GetWindowLongPtrW, IsWindow,
        };
        let sibling = sibling as HWND;
        if self.hwnd.is_null() || sibling.is_null() || sibling == self.hwnd {
            return false;
        }
        // SAFETY: `self.hwnd` is non-null (checked above) and is this window's own handle (set in
        // `new`, nulled only after `DestroyWindow`). `sibling` is a handle the caller got from
        // `native_handle`; user32 validates handles (`IsWindow`, and a stale one is rejected by
        // the calls), and none of the calls dereferences a pointer.
        unsafe {
            if IsWindow(sibling) == 0 {
                return false;
            }
            let topmost = |hwnd: HWND| GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST;
            if topmost(self.hwnd) != topmost(sibling) {
                return false;
            }
            if SetWindowPos(
                self.hwnd,
                sibling,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            ) == 0
            {
                return false;
            }
            // Whether `window` is owned by this window, directly or through other owned windows
            // (the owner chain of a window ends in null and is short).
            let owned_by_self = |window: HWND| {
                let mut owner = GetWindow(window, GW_OWNER);
                for _ in 0..16 {
                    if owner.is_null() {
                        return false;
                    }
                    if owner == self.hwnd {
                        return true;
                    }
                    owner = GetWindow(owner, GW_OWNER);
                }
                false
            };
            let mut above = GetWindow(self.hwnd, GW_HWNDPREV);
            while !above.is_null() && above != sibling && owned_by_self(above) {
                above = GetWindow(above, GW_HWNDPREV);
            }
            above == sibling
        }
    }

    /// `QWindowsWindow::requestActivateWindow`: `SetForegroundWindow` on a visible window that
    /// takes activation. The system's foreground lock is not worked around: when this process may
    /// not take the foreground, Windows flashes the window's taskbar button instead and the window
    /// does not become active.
    pub fn request_activate_window(&self) {
        use windows_sys::Win32::UI::WindowsAndMessaging::{IsWindowVisible, SetForegroundWindow};
        if self.hwnd.is_null() || self.flags.contains(WindowFlags::TOOLTIP) {
            return;
        }
        // SAFETY: `self.hwnd` is non-null (checked above) and is this window's own handle (set in
        // `new`, nulled only after `DestroyWindow`); the calls take no pointers.
        unsafe {
            if IsWindowVisible(self.hwnd) != 0 {
                SetForegroundWindow(self.hwnd);
            }
        }
    }

    pub fn set_geometry(&mut self, rect: Rect) {
        if !self.hwnd.is_null() {
            let _guard = SetGeometryGuard::new(&self.state_flags);
            // SAFETY: `self.hwnd` is non-null (checked above) and is this window's own handle; the
            // null insert-after is permitted because `SWP_NOZORDER` is set; no other pointers.
            unsafe {
                SetWindowPos(
                    self.hwnd,
                    ptr::null_mut(),
                    rect.x,
                    rect.y,
                    rect.width,
                    rect.height,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            if (rect.width != self.geometry.width || rect.height != self.geometry.height)
                && rect.width > 0
                && rect.height > 0
            {
                sync_interactive_resize(self.hwnd, &mut self.presenter, &mut self.layered_surface);
                if let Some(surface) = &mut self.layered_surface
                    && surface
                        .resize(rect.width as u32, rect.height as u32)
                        .is_err()
                {
                    self.layered_surface = None;
                }
                if let Some(presenter) = &mut self.presenter
                    && presenter
                        .resize(rect.width as u32, rect.height as u32)
                        .is_err()
                {
                    self.presenter = None;
                }
            }
            self.geometry = rect;
        }
    }

    pub fn set_stays_on_top(&mut self, enabled: bool) {
        if !self.hwnd.is_null() {
            let insert_after = topmost_insert_after(enabled);
            // SAFETY: `self.hwnd` is non-null (checked above) and is this window's own handle;
            // `insert_after` is HWND_TOPMOST/HWND_NOTOPMOST from `topmost_insert_after`.
            unsafe {
                SetWindowPos(
                    self.hwnd,
                    insert_after,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
            }
            self.flags.set(WindowFlags::STAYS_ON_TOP, enabled);
        }
    }

    pub fn set_click_through(&mut self, enabled: bool) {
        if !self.hwnd.is_null() {
            use windows_sys::Win32::UI::WindowsAndMessaging::{
                GWL_EXSTYLE, GetWindowLongPtrW, SetWindowLongPtrW,
            };
            // SAFETY: `self.hwnd` is non-null (checked above) and is this window's own handle; the
            // calls take only integers/handles, no pointers.
            unsafe {
                let ex_style = GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE) as u32;
                let new_style = if enabled {
                    ex_style | WS_EX_TRANSPARENT
                } else {
                    ex_style & !WS_EX_TRANSPARENT
                };
                SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, new_style as isize);
            }
            self.flags.set(WindowFlags::CLICK_THROUGH, enabled);
            // `qwindowswindow.cpp:498`: Qt requires `WS_EX_LAYERED` for `WindowTransparentForInput`
            // in combination with `WS_EX_TRANSPARENT`, with layered attributes set so the window is
            // drawn. A `LAYERED` window is managed by its presenter.
            if !self.flags.contains(WindowFlags::LAYERED) {
                apply_system_window_opacity(self.hwnd, self.layered_needs());
            }
        }
    }

    pub fn start_system_drag(&self) {
        let _ = self.start_system_move();
    }

    pub fn start_system_move(&self) -> bool {
        if !self.hwnd.is_null() {
            post_system_command(self.hwnd, 0xF012 /*SC_DRAGMOVE*/);
            true
        } else {
            false
        }
    }

    pub fn start_system_resize(&self, edges: crate::platform_window::WindowEdges) -> bool {
        if !self.hwnd.is_null() {
            post_system_command(self.hwnd, edges_to_win_orientation(edges));
            true
        } else {
            false
        }
    }

    /// `QWindowsWindow::setOpacity`. A `LAYERED` window's presenter blends the opacity into every
    /// present; any other window is made layered by the system while it is translucent
    /// (`QWindowsWindow::setWindowLayered` / `setWindowOpacity`, `qwindowswindow.cpp:494-530`).
    pub fn set_opacity(&mut self, opacity: f32) {
        let opacity = opacity.clamp(0.0, 1.0);
        let changed = self.opacity != opacity;
        self.opacity = opacity;
        if let Some(p) = &mut self.presenter {
            p.set_opacity(opacity);
        }
        if changed && !self.hwnd.is_null() && !self.flags.contains(WindowFlags::LAYERED) {
            apply_system_window_opacity(self.hwnd, self.layered_needs());
        }
    }

    pub fn opacity(&self) -> f32 {
        self.opacity
    }

    /// What this window needs `WS_EX_LAYERED` for right now.
    pub(super) fn layered_needs(&self) -> LayeredNeeds {
        LayeredNeeds {
            opacity: self.opacity,
            click_through: self.flags.contains(WindowFlags::CLICK_THROUGH),
            per_pixel: self.flags.contains(WindowFlags::LAYERED) || self.layered_surface.is_some(),
            backdrop: self.backdrop_active,
        }
    }

    /// Applies the state's effective backdrop and refreshes `backdrop_active` and the layered style.
    pub(super) fn apply_backdrop_state(&mut self) -> bool {
        let effective = self.backdrop_state.effective();
        let applied = crate::backdrop::set_window_backdrop(
            self.hwnd,
            effective,
            self.backdrop_state.dark_mode(),
        );
        // Tracks the request, not the outcome: the DWM calls can fail after changing part of the
        // state, so a failed request may still leave a backdrop that the style must not strip.
        // `FlatTint` is a backdrop too: it is an accent policy that needs the layered style.
        self.backdrop_active = effective != crate::backdrop::BackdropType::None;
        // Dropping the backdrop lets a window that nothing else keeps layered go plain again.
        let plain = !self.hwnd.is_null() && !self.flags.contains(WindowFlags::LAYERED);
        if !self.backdrop_active && plain {
            apply_system_window_opacity(self.hwnd, self.layered_needs());
        }
        applied
    }

    /// `QWindow::setMinimumSize` (logical pixels). Windows learns it through `WM_GETMINMAXINFO`,
    /// which converts it with the window's ratio at that moment (`SizeLimits::native_track`).
    pub fn set_minimum_size(&mut self, min_w: i32, min_h: i32) {
        self.size_limits.min = crate::size_limits::clamp_size_limit(min_w, min_h);
        self.publish_size_limits();
    }

    pub fn minimum_size(&self) -> (i32, i32) {
        self.size_limits.min
    }

    /// `QWindow::setMaximumSize`, same contract as `set_minimum_size`.
    pub fn set_maximum_size(&mut self, max_w: i32, max_h: i32) {
        self.size_limits.max = crate::size_limits::clamp_size_limit(max_w, max_h);
        self.publish_size_limits();
    }

    pub fn maximum_size(&self) -> (i32, i32) {
        self.size_limits.max
    }

    /// Makes the limits visible to the window procedure, which only has the `HWND`.
    pub(super) fn publish_size_limits(&self) {
        if !self.hwnd.is_null() {
            set_window_size_limits(self.hwnd, self.size_limits);
        }
    }

    pub fn create_layered_surface(&self) -> Result<crate::layered::LayeredSurface, &'static str> {
        crate::layered::LayeredSurface::new(
            self.hwnd,
            self.geometry.width as u32,
            self.geometry.height as u32,
        )
    }

    pub fn get_or_create_layered_surface(
        &mut self,
        width: u32,
        height: u32,
    ) -> Result<&mut crate::surface::WindowsSurface, &'static str> {
        if self.layered_surface.is_none() {
            self.layered_surface = Some(crate::surface::WindowsSurface::create(
                self.hwnd, width, height,
            )?);
        }
        sync_interactive_resize(self.hwnd, &mut self.presenter, &mut self.layered_surface);
        let surface = self.layered_surface.as_mut().unwrap();
        if (surface.width() != width || surface.height() != height)
            && let Err(e) = surface.resize(width, height)
        {
            self.layered_surface = None;
            return Err(e);
        }
        Ok(self.layered_surface.as_mut().unwrap())
    }

    /// The current presenter, if one has been created (instrumentation/tests).
    pub fn presenter(&self) -> Option<&crate::presenter::WindowsPresenter> {
        self.presenter.as_ref()
    }

    pub fn presenter_mut(&mut self) -> Option<&mut crate::presenter::WindowsPresenter> {
        self.presenter.as_mut()
    }

    pub fn is_dcomp_downgraded(&self) -> bool {
        self.dcomp_downgraded
    }

    pub fn set_dcomp_downgraded(&mut self, downgraded: bool) {
        self.dcomp_downgraded = downgraded;
    }

    /// Sets the presenter directly, for tests / fault injection.
    #[cfg(any(test, feature = "test-hooks"))]
    pub fn set_presenter_for_test(
        &mut self,
        presenter: Option<crate::presenter::WindowsPresenter>,
    ) {
        self.presenter = presenter;
    }
    pub fn get_or_create_presenter(
        &mut self,
        width: u32,
        height: u32,
    ) -> Result<&mut crate::presenter::WindowsPresenter, &'static str> {
        if self.presenter.is_none() {
            let p = if self.flags.contains(WindowFlags::LAYERED) {
                if !self.dcomp_downgraded && is_dcomp_enabled() {
                    match crate::surface::dcomp::DCompSurface::new(self.hwnd, width, height) {
                        Ok(dcomp) => crate::presenter::WindowsPresenter::DirectComposition(dcomp),
                        _ => {
                            let layered = crate::presenter::Win32LayeredPresenter::new(
                                self.hwnd,
                                width,
                                height,
                                self.opacity,
                            )?;
                            crate::presenter::WindowsPresenter::Layered(layered)
                        }
                    }
                } else {
                    let layered = crate::presenter::Win32LayeredPresenter::new(
                        self.hwnd,
                        width,
                        height,
                        self.opacity,
                    )?;
                    crate::presenter::WindowsPresenter::Layered(layered)
                }
            } else {
                let dc = crate::presenter::Win32DcPresenter::new(self.hwnd, width, height)?;
                crate::presenter::WindowsPresenter::Dc(dc)
            };
            self.presenter = Some(p);
        }
        sync_interactive_resize(self.hwnd, &mut self.presenter, &mut self.layered_surface);
        let p = self.presenter.as_mut().unwrap();
        if let Err(e) = p.resize(width, height) {
            if let crate::presenter::WindowsPresenter::DirectComposition(_) = p {
                self.dcomp_downgraded = true;
            }
            self.presenter = None;
            return Err(e);
        }
        Ok(self.presenter.as_mut().unwrap())
    }

    pub fn close(&mut self) {
        self.layered_surface = None;
        self.presenter = None;
        // `DestroyWindow` neither revokes the OLE registration nor releases our reference, so the
        // registration goes before the window.
        self.drop_target = None;
        if !self.hwnd.is_null() {
            // Windows would destroy the windows this one owns with it (QTBUG-35499).
            crate::window::region::release_owned_windows(self.hwnd, NATIVE_WINDOW_CLASS_NAME);
            remove_window_size_limits(self.hwnd);
            // SAFETY: `self.hwnd` is non-null (checked above) and is this window's own handle, created
            // in `new`; this is its only DestroyWindow call because `hwnd` is nulled right after, and
            // `NativeWindow` is `!Send`, so this runs on the creating thread DestroyWindow requires.
            unsafe {
                DestroyWindow(self.hwnd);
            }
            self.hwnd = ptr::null_mut();
        }
    }

    pub fn set_custom_frameless_config(&mut self, caption_height: i32, resize_border: i32) {
        if !self.hwnd.is_null() {
            set_window_frameless_config(
                self.hwnd,
                CustomFramelessConfig {
                    caption_height,
                    resize_border,
                },
            );
        }
    }

    pub fn custom_frameless_config(&self) -> Option<CustomFramelessConfig> {
        if !self.hwnd.is_null() {
            get_window_frameless_config(self.hwnd)
        } else {
            None
        }
    }
}

#[cfg(windows)]
impl crate::platform_window::PlatformWindow for NativeWindow {
    fn state_flags(&self) -> PlatformWindowStateFlags {
        self.state_flags.get()
    }

    fn owner_thread(&self) -> std::thread::ThreadId {
        self.owner_thread
    }

    fn show(&self) {
        self.show();
    }

    fn hide(&self) {
        self.hide();
    }

    fn raise(&self) {
        self.raise();
    }

    fn stack_under(&self, sibling: isize) -> bool {
        self.stack_under(sibling)
    }

    fn request_activate_window(&self) {
        self.request_activate_window();
    }

    /// `QWindowsWindow::isActive` (`qwindowswindow.cpp:1904-1910`): the window is the foreground
    /// window, or a child of it.
    fn is_active(&self) -> bool {
        if self.hwnd.is_null() {
            return false;
        }
        // SAFETY: GetForegroundWindow takes no arguments; the result is only compared or passed on.
        let foreground = unsafe { GetForegroundWindow() };
        !foreground.is_null()
            // SAFETY: `foreground` is non-null (checked just above) and `self.hwnd` is non-null
            // (checked at the top); IsChild takes handles only and validates them.
            && (foreground == self.hwnd || unsafe { IsChild(foreground, self.hwnd) } != 0)
    }

    fn geometry(&self) -> Rect {
        self.geometry()
    }

    fn set_geometry(&mut self, rect: Rect) {
        self.set_geometry(rect);
    }
    fn set_target_pos(&mut self, pos: Option<ferrokit_gui::geometry::Point>) {
        self.target_pos = pos;
    }

    fn set_stays_on_top(&mut self, enabled: bool) {
        self.set_stays_on_top(enabled);
    }

    fn set_click_through(&mut self, enabled: bool) {
        self.set_click_through(enabled);
    }

    fn flags(&self) -> WindowFlags {
        self.flags
    }

    fn start_system_drag(&self) {
        let _ = self.start_system_move();
    }

    fn start_system_move(&self) -> bool {
        self.start_system_move()
    }

    fn start_system_resize(&self, edges: crate::platform_window::WindowEdges) -> bool {
        self.start_system_resize(edges)
    }
    fn set_cursor(&mut self, shape: crate::cursor::CursorShape) {
        set_window_cursor_shape(self.hwnd, shape);
        // An application override cursor stays on top (`QWindowsWindow::applyCursor`).
        if crate::cursor::win32_cursor::Win32Cursor::override_shape().is_none() {
            crate::cursor::win32_cursor::Win32Cursor::apply_shape(shape);
        }
    }

    fn unset_cursor(&mut self) {
        use crate::cursor::win32_cursor::Win32Cursor;
        unset_window_cursor_shape(self.hwnd);
        // A window without a cursor shows the default (`QWindowsWindow::applyCursor`, null cursor).
        if Win32Cursor::override_shape().is_none() {
            Win32Cursor::apply_shape(Win32Cursor::current_global_shape());
        }
    }

    fn set_mouse_grab_enabled(&mut self, grab: bool) -> bool {
        set_mouse_grab_enabled_native(self.hwnd, grab)
    }

    fn has_mouse_grab(&self) -> bool {
        // SAFETY: GetCapture takes no arguments; `self.hwnd` is only compared with the result.
        !self.hwnd.is_null() && unsafe { GetCapture() } == self.hwnd
    }

    fn set_opacity(&mut self, opacity: f32) {
        self.set_opacity(opacity);
    }

    fn opacity(&self) -> f32 {
        self.opacity
    }

    fn set_minimum_size(&mut self, min_w: i32, min_h: i32) {
        self.set_minimum_size(min_w, min_h);
    }

    fn minimum_size(&self) -> (i32, i32) {
        self.minimum_size()
    }

    fn set_maximum_size(&mut self, max_w: i32, max_h: i32) {
        self.set_maximum_size(max_w, max_h);
    }

    fn maximum_size(&self) -> (i32, i32) {
        self.maximum_size()
    }

    fn present_region(
        &mut self,
        pixmap: &ferrokit_gui::paint::Pixmap,
        dirty: &ferrokit_gui::geometry::Region,
    ) -> Result<(), &'static str> {
        let width = pixmap.physical_width();
        let height = pixmap.physical_height();
        let opacity = self.opacity;
        // Native: the caller mapped it through the screen the window lands on.
        let target_pos = self.target_pos.take();
        let dbg_t = crate::resize_debug::start();
        let res = match self.get_or_create_presenter(width, height) {
            Ok(p) => {
                p.set_opacity(opacity);
                if let crate::presenter::WindowsPresenter::Layered(lp) = p
                    && let Some(pos) = target_pos
                {
                    lp.set_target_pos(Some(windows_sys::Win32::Foundation::POINT {
                        x: pos.x,
                        y: pos.y,
                    }));
                }
                p.present(pixmap, dirty)
            }
            Err(e) => Err(e),
        };

        if res.is_err() {
            crate::resize_debug::count(crate::resize_debug::Count::PresentError);
            if let Some(crate::presenter::WindowsPresenter::DirectComposition(_)) = &self.presenter
            {
                // Runtime DComp Present failure: downgrade this window to Layered GDI permanently
                self.dcomp_downgraded = true;
            }
            self.presenter.take();
            let p = self.get_or_create_presenter(width, height)?;
            if let crate::presenter::WindowsPresenter::Layered(lp) = p
                && let Some(pos) = target_pos
            {
                lp.set_target_pos(Some(windows_sys::Win32::Foundation::POINT {
                    x: pos.x,
                    y: pos.y,
                }));
            }
            let full =
                ferrokit_gui::geometry::Region::from_coords(0, 0, width as i32, height as i32);
            let r = p.present(pixmap, &full);
            if r.is_ok() {
                if let Some(pos) = target_pos {
                    self.geometry.x = pos.x;
                    self.geometry.y = pos.y;
                }
                self.geometry.width = width as i32;
                self.geometry.height = height as i32;
            }
            crate::resize_debug::end(crate::resize_debug::Phase::Present, dbg_t);
            return r;
        }

        if let Some(pos) = target_pos {
            self.geometry.x = pos.x;
            self.geometry.y = pos.y;
        }
        self.geometry.width = width as i32;
        self.geometry.height = height as i32;

        crate::resize_debug::end(crate::resize_debug::Phase::Present, dbg_t);
        Ok(())
    }

    fn present(
        &mut self,
        pixmap: &mut ferrokit_gui::paint::Pixmap,
        opacity: f32,
    ) -> Result<(), &'static str> {
        self.set_opacity(opacity);
        let width = pixmap.physical_width();
        let height = pixmap.physical_height();
        let full = ferrokit_gui::geometry::Region::from_coords(0, 0, width as i32, height as i32);
        self.present_region(pixmap, &full)
    }

    fn present_dirty(
        &mut self,
        pixmap: &mut ferrokit_gui::paint::Pixmap,
        opacity: f32,
        dirty_rect: Rect,
    ) -> Result<(), &'static str> {
        self.set_opacity(opacity);
        let region = ferrokit_gui::geometry::Region::from_rect(dirty_rect);
        self.present_region(pixmap, &region)
    }
    fn set_event_handler(&mut self, handler: Box<dyn WindowSystemEventHandler>) {
        self.set_event_handler(handler);
    }

    fn reports_native_double_clicks(&self) -> bool {
        // `WM_*BUTTONDBLCLK` arrives from the OS (`CS_DBLCLKS` on the window class).
        true
    }

    fn native_handle(&self) -> isize {
        self.hwnd as isize
    }

    fn device_pixel_ratio(&self) -> f32 {
        get_window_dpr(self.hwnd)
    }

    fn native_size(&self) -> Option<(u32, u32)> {
        if self.hwnd.is_null() {
            return None;
        }
        // SAFETY: RECT is a plain integer struct, so the all-zero value is valid.
        let mut r: RECT = unsafe { std::mem::zeroed() };
        // Same rect `UpdateLayeredWindowIndirect` is given as `psize`.
        // SAFETY: `self.hwnd` is non-null (checked above) and is this window's own handle; `r` is a
        // local out pointer valid for the call; a 0 return is handled.
        if unsafe { GetWindowRect(self.hwnd, &mut r) } == 0 {
            return None;
        }
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        (w > 0 && h > 0).then_some((w as u32, h as u32))
    }

    fn set_mask(&mut self, region: &ferrokit_gui::geometry::Region) {
        crate::window::region::set_window_mask(self.hwnd, region);
    }

    fn set_transient_parent(&mut self, parent: Option<isize>) {
        crate::window::region::set_owner(self.hwnd, parent.map(|p| p as HWND));
    }

    fn transient_parent(&self) -> Option<isize> {
        crate::window::region::owner_of(self.hwnd).map(|h| h as isize)
    }

    fn set_backdrop(&mut self, backdrop: crate::backdrop::BackdropType, dark_mode: bool) -> bool {
        self.backdrop_state.request(backdrop, dark_mode);
        self.apply_backdrop_state()
    }

    fn suspend_backdrop(&mut self) -> bool {
        // Tracks the request, not the outcome, like `set_backdrop`.
        let changed = self.backdrop_state.suspend();
        if changed {
            self.apply_backdrop_state();
        }
        changed
    }

    fn resume_backdrop(&mut self) -> bool {
        let changed = self.backdrop_state.resume();
        if changed {
            self.apply_backdrop_state();
        }
        changed
    }

    fn set_ime_focus(&mut self, pos: ferrokit_gui::geometry::primitives::Point) {
        if !self.ime_enabled.replace(true) {
            crate::ime::set_window_ime_enabled(self.hwnd, true);
        }
        let mut ime = crate::ime::Win32InputContext::new(self.hwnd);
        ime.set_micro_focus(pos);
    }

    fn enable_drop_target(&mut self, enabled: bool) -> bool {
        if enabled {
            if self.drop_target.is_some() {
                return true;
            }
            let hwnd_isize = self.hwnd as isize;
            let callback = move |ev: crate::drag_drop::DropEvent| -> u32 {
                let hwnd = hwnd_isize as HWND;
                match ev {
                    crate::drag_drop::DropEvent::Enter {
                        pos,
                        formats,
                        effect,
                    } => send_window_system_event_accepted(
                        hwnd,
                        WindowSystemEvent::DragEnter {
                            pos,
                            formats,
                            drop_action: effect,
                        },
                    ),
                    crate::drag_drop::DropEvent::Over { pos, effect } => {
                        send_window_system_event_accepted(
                            hwnd,
                            WindowSystemEvent::DragMove {
                                pos,
                                drop_action: effect,
                            },
                        )
                    }
                    crate::drag_drop::DropEvent::Leave => {
                        dispatch_window_system_event(
                            Delivery::Default,
                            hwnd,
                            WindowSystemEvent::DragLeave,
                        );
                        0
                    }
                    crate::drag_drop::DropEvent::Drop {
                        pos,
                        formats,
                        data,
                        effect,
                    } => send_window_system_event_accepted(
                        hwnd,
                        WindowSystemEvent::Drop {
                            pos,
                            formats,
                            data,
                            drop_action: effect,
                        },
                    ),
                }
            };
            match crate::drag_drop::win32_ole::register_drop_target(self.hwnd, callback) {
                Ok(target) => {
                    self.drop_target = Some(target);
                    true
                }
                _ => false,
            }
        } else {
            self.drop_target = None;
            true
        }
    }
}

#[cfg(windows)]
impl Drop for NativeWindow {
    fn drop(&mut self) {
        self.close();
    }
}
