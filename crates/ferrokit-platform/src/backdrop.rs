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
}

/// Applies the specified backdrop material effect to the window.
#[cfg(windows)]
pub fn set_window_backdrop(
    hwnd: windows_sys::Win32::Foundation::HWND,
    backdrop: BackdropType,
    dark_mode: bool,
) -> bool {
    use windows_sys::Win32::Graphics::Dwm::{
        DwmEnableBlurBehindWindow, DwmSetWindowAttribute, DWMWA_SYSTEMBACKDROP_TYPE,
        DWMWA_USE_IMMERSIVE_DARK_MODE, DWM_BB_ENABLE, DWM_BLURBEHIND,
    };
    // unused import removed

    if hwnd.is_null() {
        return false;
    }

    unsafe {
        // First, configure dark mode preference if requested
        let dark: i32 = if dark_mode { 1 } else { 0 };
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
            &dark as *const _ as *const _,
            std::mem::size_of::<i32>() as u32,
        );

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
                let grad_color = if dark_mode { 0x99161a22 } else { 0x99f0f2f8 };
                set_win10_accent(hwnd, 4, 2, grad_color)
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
fn set_win10_accent(hwnd: windows_sys::Win32::Foundation::HWND, state: u32, flags: u32, gradient_color: u32) -> bool {
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
        ObjcMsg::send_0(ns_view, Sel::register("window"))
    };

    if target_window.is_nil() {
        return false;
    }

    match backdrop {
        BackdropType::None => {
            // Qt removes the area's effect view from its superview and forgets it; the content
            // view is untouched (QCocoaWindow::manageVisualEffectArea, qcocoawindow.mm:2258-2263).
            if let Some(effect_view) = backdrop_view.take() {
                ObjcMsg::send_0(effect_view, Sel::register("removeFromSuperview"));
            }
            true
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
                    let effect_alloc = ObjcMsg::send_class_0(effect_class, Sel::register("alloc"));
                    if effect_alloc.is_nil() {
                        return false;
                    }
                    ObjcMsg::send_0(effect_alloc, Sel::register("init"))
                }
            };

            // Material selection:
            // HUDWindow = 13, Popover = 6, UnderWindowBackground = 2
            let material = match backdrop {
                BackdropType::Mica | BackdropType::MicaAlt => 13,
                BackdropType::Acrylic => 6,
                BackdropType::BlurBehind => 2,
                BackdropType::None => 0,
            };

            ObjcMsg::send_int(effect_view, Sel::register("setMaterial:"), material);
            ObjcMsg::send_int(effect_view, Sel::register("setBlendingMode:"), 0); // BehindWindow
            ObjcMsg::send_int(effect_view, Sel::register("setState:"), 1); // Active

            let app_name = if dark_mode {
                "NSAppearanceNameDarkAqua"
            } else {
                "NSAppearanceNameAqua"
            };
            let appearance_class = Class::get("NSAppearance").unwrap_or(Class::NIL);
            let appearance = ObjcMsg::send_str(
                Id(appearance_class.0),
                Sel::register("appearanceNamed:"),
                app_name,
            );
            if !appearance.is_nil() {
                ObjcMsg::send_id(effect_view, Sel::register("setAppearance:"), appearance);
            }

            if existing.is_none() {
                // Qt stacks the effect layer below the content layer (z 0): wantsLayer, then
                // layer.zPosition = -FLT_MAX (qcocoawindow.mm:2267-2270). zPosition is a CGFloat.
                ObjcMsg::send_bool(effect_view, Sel::register("setWantsLayer:"), true);
                let effect_layer = ObjcMsg::send_0(effect_view, Sel::register("layer"));
                ObjcMsg::send_length(
                    effect_layer,
                    Sel::register("setZPosition:"),
                    f64::from(-f32::MAX),
                );
                let content_view = ObjcMsg::send_0(target_window, Sel::register("contentView"));
                if !content_view.is_nil() {
                    ObjcMsg::send_id(content_view, Sel::register("addSubview:"), effect_view);
                } else {
                    ObjcMsg::send_id(target_window, Sel::register("setContentView:"), effect_view);
                }
                *backdrop_view = Some(effect_view);
            }

            true
        }
    }
}
