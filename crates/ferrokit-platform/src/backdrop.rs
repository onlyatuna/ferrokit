// SPDX-License-Identifier: LGPL-3.0-only
// Backdrop materials module

/// Backdrop materials corresponding to Windows 11 / Windows 10 DWM visual styles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackdropType {
    /// Default window background (opaque or system-controlled).
    None,
    /// Windows 11 Mica material for main windows (`DWMSBT_MAINWINDOW`).
    Mica,
    /// Windows 11 Mica Alt material for tabbed windows (`DWMSBT_TABBEDWINDOW`).
    MicaAlt,
    /// Windows 11 / Windows 10 Desktop Acrylic blur effect (`DWMSBT_TRANSIENTWINDOW` or `ACCENT_ENABLE_ACRYLICBLURBEHIND`).
    Acrylic,
    /// Windows 7 / 10 Classic DWM Aero Glass blur behind (`DwmEnableBlurBehindWindow`).
    BlurBehind,
    /// Flat tinted panel without blur, the Acrylic tint on `ACCENT_ENABLE_TRANSPARENTGRADIENT`.
    /// Windows only: Cocoa removes the effect view and reports `false` (the tint is not drawn);
    /// X11 and Wayland keep their stub behaviour.
    FlatTint,
}

/// Gradient colour (`0xAABBGGRR` accent format) shared by the `Acrylic` and `FlatTint` accents.
pub fn accent_tint_color(dark_mode: bool) -> u32 {
    if dark_mode {
        0x99161a22
    } else {
        0x99f0f2f8
    }
}

/// Tracks a window's requested backdrop and whether its Acrylic blur is suspended.
///
/// Suspending replaces the expensive Acrylic blur with the flat tint of the same colour (used
/// while a window is dragged or resized); every other backdrop type is unaffected by suspension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackdropState {
    requested: BackdropType,
    dark_mode: bool,
    suspended: bool,
}

impl Default for BackdropState {
    fn default() -> Self {
        Self::new()
    }
}

impl BackdropState {
    /// No backdrop requested, not suspended.
    pub const fn new() -> Self {
        Self {
            requested: BackdropType::None,
            dark_mode: false,
            suspended: false,
        }
    }

    /// Records the requested backdrop and tint. A suspension survives only for `Acrylic`.
    pub fn request(&mut self, backdrop: BackdropType, dark_mode: bool) {
        self.requested = backdrop;
        self.dark_mode = dark_mode;
        self.suspended = self.suspended && backdrop == BackdropType::Acrylic;
    }

    /// The last requested backdrop, regardless of suspension.
    pub fn requested(&self) -> BackdropType {
        self.requested
    }

    /// The remembered tint (dark or light gradient).
    pub fn dark_mode(&self) -> bool {
        self.dark_mode
    }

    /// Whether the Acrylic blur is currently suspended.
    pub fn is_suspended(&self) -> bool {
        self.suspended
    }

    /// The backdrop that must be applied now: `FlatTint` for a suspended `Acrylic`, else the request.
    pub fn effective(&self) -> BackdropType {
        if self.suspended && self.requested == BackdropType::Acrylic {
            BackdropType::FlatTint
        } else {
            self.requested
        }
    }

    /// Suspends the Acrylic blur. Returns `true` if the effective backdrop changed, i.e. only
    /// when `Acrylic` is requested and not already suspended.
    pub fn suspend(&mut self) -> bool {
        if self.requested != BackdropType::Acrylic || self.suspended {
            return false;
        }
        self.suspended = true;
        true
    }

    /// Ends a suspension. Returns `true` if the effective backdrop changed.
    pub fn resume(&mut self) -> bool {
        let changed = self.suspended;
        self.suspended = false;
        changed
    }
}

/// Applies the specified backdrop material effect to the window.
///
/// The window's dark frame attribute: every backdrop but [`BackdropType::None`] writes it from
/// `dark_mode`, and the colour scheme then leaves that window's frame alone (the application
/// chose the tint, and the frame goes with it). [`BackdropType::None`] gives the frame back to
/// the colour scheme (`dark_mode` is then the tint of nothing and is ignored) and applies the
/// scheme at once. A window this crate did not create is not tracked by the scheme, so there
/// `None` writes `dark_mode` as the other backdrops do.
#[cfg(windows)]
pub fn set_window_backdrop(
    hwnd: windows_sys::Win32::Foundation::HWND,
    backdrop: BackdropType,
    dark_mode: bool,
) -> bool {
    use windows_sys::Win32::Graphics::Dwm::{
        DwmEnableBlurBehindWindow, DwmSetWindowAttribute, DWMWA_SYSTEMBACKDROP_TYPE, DWM_BB_ENABLE,
        DWM_BLURBEHIND,
    };
    // unused import removed

    if hwnd.is_null() {
        return false;
    }

    unsafe {
        // First, configure dark mode preference (attribute 20, or 19 before 20H1, as Qt does)
        if backdrop == BackdropType::None {
            if !crate::dark_mode::release_frame_to_scheme(hwnd) {
                crate::dark_mode::set_dark_border_to_window(hwnd, dark_mode);
            }
        } else {
            crate::dark_mode::set_frame_by_application(hwnd);
            crate::dark_mode::set_dark_border_to_window(hwnd, dark_mode);
        }

        match backdrop {
            BackdropType::None => {
                let none_type: u32 = 1; // DWMSBT_NONE
                let _ = DwmSetWindowAttribute(
                    hwnd,
                    DWMWA_SYSTEMBACKDROP_TYPE as u32,
                    &none_type as *const _ as *const _,
                    std::mem::size_of::<u32>() as u32,
                );
                // Clear Windows 10/11 SetWindowCompositionAttribute accent policy (ACCENT_DISABLED = 0)
                let _ = set_win10_accent(hwnd, 0, 0, 0);
                // Also disable blur behind if active
                let bb = DWM_BLURBEHIND {
                    dwFlags: DWM_BB_ENABLE,
                    fEnable: 0,
                    hRgnBlur: std::ptr::null_mut(),
                    fTransitionOnMaximized: 0,
                };
                let _ = DwmEnableBlurBehindWindow(hwnd, &bb);
                true
            }
            BackdropType::Mica => {
                // DWMSBT_MAINWINDOW = 2
                let mica_type: u32 = 2;
                let hr = DwmSetWindowAttribute(
                    hwnd,
                    DWMWA_SYSTEMBACKDROP_TYPE as u32,
                    &mica_type as *const _ as *const _,
                    std::mem::size_of::<u32>() as u32,
                );
                hr == 0
            }
            BackdropType::MicaAlt => {
                // DWMSBT_TABBEDWINDOW = 4
                let mica_alt_type: u32 = 4;
                let hr = DwmSetWindowAttribute(
                    hwnd,
                    DWMWA_SYSTEMBACKDROP_TYPE as u32,
                    &mica_alt_type as *const _ as *const _,
                    std::mem::size_of::<u32>() as u32,
                );
                hr == 0
            }
            BackdropType::Acrylic => {
                // Use SetWindowCompositionAttribute (ACCENT_ENABLE_ACRYLICBLURBEHIND)
                // Compatible with WS_EX_LAYERED + UpdateLayeredWindow
                // (DWMWA_SYSTEMBACKDROP_TYPE DWMSBT_TRANSIENTWINDOW breaks UpdateLayeredWindow with error 87)
                set_win10_accent(hwnd, 4, 2, accent_tint_color(dark_mode))
            }
            BackdropType::FlatTint => {
                // ACCENT_ENABLE_TRANSPARENTGRADIENT (2): the same tint as Acrylic without the
                // blur, so a window can drop the expensive blur while it is dragged or resized.
                set_win10_accent(hwnd, 2, 2, accent_tint_color(dark_mode))
            }
            BackdropType::BlurBehind => {
                let bb = DWM_BLURBEHIND {
                    dwFlags: DWM_BB_ENABLE,
                    fEnable: 1,
                    hRgnBlur: std::ptr::null_mut(),
                    fTransitionOnMaximized: 0,
                };
                DwmEnableBlurBehindWindow(hwnd, &bb) == 0
            }
        }
    }
}

#[cfg(windows)]
fn set_win10_accent(
    hwnd: windows_sys::Win32::Foundation::HWND,
    state: u32,
    flags: u32,
    gradient_color: u32,
) -> bool {
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryA};

    #[repr(C)]
    struct AccentPolicy {
        accent_state: u32,
        accent_flags: u32,
        gradient_color: u32,
        animation_id: u32,
    }

    #[repr(C)]
    struct WindowCompositionAttributeData {
        attribute: u32,
        data: *mut AccentPolicy,
        size_of_data: usize,
    }

    type SetWindowCompositionAttributeFn = unsafe extern "system" fn(
        windows_sys::Win32::Foundation::HWND,
        *mut WindowCompositionAttributeData,
    ) -> i32;
    unsafe {
        let user32 = LoadLibraryA(c"user32.dll".as_ptr().cast());
        if user32.is_null() {
            return false;
        }
        let fn_ptr = GetProcAddress(user32, c"SetWindowCompositionAttribute".as_ptr().cast());
        if let Some(set_wca) = fn_ptr {
            let set_wca: SetWindowCompositionAttributeFn = std::mem::transmute(set_wca);
            let mut policy = AccentPolicy {
                accent_state: state,
                accent_flags: flags,
                gradient_color,
                animation_id: 0,
            };
            let mut data = WindowCompositionAttributeData {
                attribute: 19, // WCA_ACCENT_POLICY
                data: &mut policy,
                size_of_data: std::mem::size_of::<AccentPolicy>(),
            };
            set_wca(hwnd, &mut data) != 0
        } else {
            false
        }
    }
}

/// Applies frosted-glass / backdrop material effect on macOS using NSVisualEffectView.
///
/// `backdrop_view` is the window's effect view slot: set when an effect view is added and
/// taken when `BackdropType::None` removes it.
pub fn set_cocoa_window_backdrop(
    ns_window: crate::objc_runtime::Id,
    ns_view: crate::objc_runtime::Id,
    backdrop: BackdropType,
    dark_mode: bool,
    backdrop_view: &mut Option<crate::objc_runtime::Id>,
) -> bool {
    use crate::objc_runtime::{Class, Id, ObjcMsg, Sel};

    if ns_window.is_nil() && ns_view.is_nil() {
        return false;
    }

    let target_window = if !ns_window.is_nil() {
        ns_window
    } else {
        unsafe { ObjcMsg::send_0(ns_view, Sel::register("window")) }
    };

    if target_window.is_nil() {
        return false;
    }

    match backdrop {
        BackdropType::None | BackdropType::FlatTint => {
            // Qt removes the area's effect view from its superview and forgets it; the content
            // view is untouched (QCocoaWindow::manageVisualEffectArea, qcocoawindow.mm:2258-2263).
            if let Some(effect_view) = backdrop_view.take() {
                unsafe { ObjcMsg::send_0(effect_view, Sel::register("removeFromSuperview")) };
                // Drop our +1 from `alloc`/`init`; the superview's retain went with the removal.
                unsafe { ObjcMsg::send_0(effect_view, Sel::register("release")) };
            }
            // `FlatTint` is a Windows accent tint that AppKit has no counterpart for: the tint
            // is not drawn, so it reports `false` while `None` reports `true`.
            backdrop != BackdropType::FlatTint
        }
        BackdropType::Mica
        | BackdropType::MicaAlt
        | BackdropType::Acrylic
        | BackdropType::BlurBehind => {
            // Qt keeps one effect view per area and updates it in place
            // (QCocoaWindow::manageVisualEffectArea, qcocoawindow.mm:2258-2259,2278-2281).
            let existing = *backdrop_view;
            let effect_view = match existing {
                Some(view) => view,
                None => {
                    let effect_class = Class::get("NSVisualEffectView").unwrap_or(Class::NIL);
                    let effect_alloc =
                        unsafe { ObjcMsg::send_class_0(effect_class, Sel::register("alloc")) };
                    if effect_alloc.is_nil() {
                        return false;
                    }
                    unsafe { ObjcMsg::send_0(effect_alloc, Sel::register("init")) }
                }
            };

            // Material selection:
            // HUDWindow = 13, Popover = 6, UnderWindowBackground = 2
            let material = match backdrop {
                BackdropType::Mica | BackdropType::MicaAlt => 13,
                BackdropType::Acrylic => 6,
                BackdropType::BlurBehind => 2,
                BackdropType::None | BackdropType::FlatTint => 0,
            };

            unsafe { ObjcMsg::send_int(effect_view, Sel::register("setMaterial:"), material) };
            unsafe { ObjcMsg::send_int(effect_view, Sel::register("setBlendingMode:"), 0) }; // BehindWindow
            unsafe { ObjcMsg::send_int(effect_view, Sel::register("setState:"), 1) }; // Active

            let app_name = if dark_mode {
                "NSAppearanceNameDarkAqua"
            } else {
                "NSAppearanceNameAqua"
            };
            let appearance_class = Class::get("NSAppearance").unwrap_or(Class::NIL);
            let appearance = unsafe {
                ObjcMsg::send_str(
                    Id(appearance_class.0),
                    Sel::register("appearanceNamed:"),
                    app_name,
                )
            };
            if !appearance.is_nil() {
                unsafe {
                    ObjcMsg::send_id(effect_view, Sel::register("setAppearance:"), appearance)
                };
            }

            if existing.is_none() {
                // Qt stacks the effect layer below the content layer (z 0): wantsLayer, then
                // layer.zPosition = -FLT_MAX (qcocoawindow.mm:2267-2270). zPosition is a CGFloat.
                unsafe { ObjcMsg::send_bool(effect_view, Sel::register("setWantsLayer:"), true) };
                let effect_layer = unsafe { ObjcMsg::send_0(effect_view, Sel::register("layer")) };
                unsafe {
                    ObjcMsg::send_length(
                        effect_layer,
                        Sel::register("setZPosition:"),
                        f64::from(-f32::MAX),
                    )
                };
                let content_view =
                    unsafe { ObjcMsg::send_0(target_window, Sel::register("contentView")) };
                if !content_view.is_nil() {
                    unsafe {
                        ObjcMsg::send_id(content_view, Sel::register("addSubview:"), effect_view)
                    };
                } else {
                    unsafe {
                        ObjcMsg::send_id(
                            target_window,
                            Sel::register("setContentView:"),
                            effect_view,
                        )
                    };
                }
                *backdrop_view = Some(effect_view);
            }

            true
        }
    }
}

#[cfg(test)]
mod backdrop_state_tests {
    use super::{accent_tint_color, BackdropState, BackdropType};

    #[test]
    fn tint_color_matches_the_hud_accent() {
        assert_eq!(accent_tint_color(true), 0x99161a22);
        assert_eq!(accent_tint_color(false), 0x99f0f2f8);
    }

    #[test]
    fn new_state_has_no_backdrop() {
        let state = BackdropState::new();
        assert_eq!(state.effective(), BackdropType::None);
        assert_eq!(state.requested(), BackdropType::None);
        assert!(!state.is_suspended());
    }

    #[test]
    fn suspend_turns_acrylic_into_flat_tint_and_resume_restores_it() {
        let mut state = BackdropState::new();
        state.request(BackdropType::Acrylic, true);
        assert_eq!(state.effective(), BackdropType::Acrylic);
        assert!(state.suspend());
        assert_eq!(state.effective(), BackdropType::FlatTint);
        assert_eq!(state.requested(), BackdropType::Acrylic);
        assert!(state.resume());
        assert_eq!(state.effective(), BackdropType::Acrylic);
    }

    #[test]
    fn suspend_and_resume_report_only_real_changes() {
        let mut state = BackdropState::new();
        state.request(BackdropType::Acrylic, false);
        assert!(!state.resume());
        assert!(state.suspend());
        assert!(!state.suspend());
        assert!(state.resume());
        assert!(!state.resume());
    }

    #[test]
    fn tint_is_remembered_across_suspend_and_resume() {
        for dark in [true, false] {
            let mut state = BackdropState::new();
            state.request(BackdropType::Acrylic, dark);
            state.suspend();
            assert_eq!(state.dark_mode(), dark);
            state.resume();
            assert_eq!(state.dark_mode(), dark);
        }
    }

    #[test]
    fn only_acrylic_is_suspended() {
        for backdrop in [
            BackdropType::None,
            BackdropType::Mica,
            BackdropType::MicaAlt,
            BackdropType::BlurBehind,
            BackdropType::FlatTint,
        ] {
            let mut state = BackdropState::new();
            state.request(backdrop, true);
            assert!(!state.suspend(), "{backdrop:?} must not be suspended");
            assert_eq!(state.effective(), backdrop);
            assert!(!state.is_suspended());
            assert!(!state.resume());
        }
    }

    #[test]
    fn acrylic_request_while_suspended_stays_suspended_with_new_tint() {
        let mut state = BackdropState::new();
        state.request(BackdropType::Acrylic, true);
        assert!(state.suspend());
        state.request(BackdropType::Acrylic, false);
        assert_eq!(state.effective(), BackdropType::FlatTint);
        assert!(!state.dark_mode());
        assert!(state.resume());
        assert_eq!(state.effective(), BackdropType::Acrylic);
    }

    #[test]
    fn other_request_while_suspended_drops_the_suspension() {
        let mut state = BackdropState::new();
        state.request(BackdropType::Acrylic, true);
        assert!(state.suspend());
        state.request(BackdropType::Mica, true);
        assert_eq!(state.effective(), BackdropType::Mica);
        assert!(!state.is_suspended());
        state.request(BackdropType::Acrylic, true);
        assert_eq!(state.effective(), BackdropType::Acrylic);
        assert!(!state.resume());
    }
}
