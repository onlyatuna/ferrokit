// SPDX-License-Identifier: LGPL-3.0-only
use ferrokit_gui::geometry::primitives::{Point, Rect};

pub trait PlatformScreen: Send + Sync {
    fn name(&self) -> String;
    fn geometry(&self) -> Rect;
    fn available_geometry(&self) -> Rect;
    fn is_primary(&self) -> bool;
    fn device_pixel_ratio(&self) -> f32;
}

/// Physical cursor point to logical, through the screen that contains it. Qt does the same:
/// `QWindowsSystemTrayIcon` uses `screenAtDp(globalPos)` (`qwindowssystemtrayicon.cpp:398-404`)
/// and `QSystemTrayIcon` maps with `QHighDpi::fromNativePixels(pos, screen)` (`qsystemtrayicon.cpp:184`).
/// Falls back to the primary screen, as Qt does (`screens().value(0)`).
pub fn native_cursor_to_logical(native: Point) -> Point {
    let platform = crate::integration::platform();
    let screens = platform.screens();
    let hit = screens.iter().find(|s| {
        let g = s.geometry();
        crate::high_dpi::to_native_global_rect(g, Point::new(g.x, g.y), s.device_pixel_ratio())
            .contains(native)
    });
    let (origin, dpr) = match hit {
        Some(s) => {
            let g = s.geometry();
            (Point::new(g.x, g.y), s.device_pixel_ratio())
        }
        None => {
            let primary = platform.primary_screen();
            let g = primary.geometry();
            (Point::new(g.x, g.y), primary.device_pixel_ratio())
        }
    };
    crate::high_dpi::from_native_global_point(native, origin, dpr)
}

/// Logical desktop point to the physical desktop point native APIs (`TrackPopupMenu`,
/// `SetCursorPos`, ...) take, through the screen that contains it
/// (`QHighDpi::toNativePixels(pos, screen)`); the primary screen when none does. The inverse of
/// [`native_cursor_to_logical`].
pub fn logical_point_to_native(logical: Point) -> Point {
    let platform = crate::integration::platform();
    let primary = platform.primary_screen();
    let mut screens: Vec<(Rect, f32)> = vec![(primary.geometry(), primary.device_pixel_ratio())];
    screens.extend(
        platform
            .screens()
            .iter()
            .map(|s| (s.geometry(), s.device_pixel_ratio())),
    );
    logical_to_native_on(&screens, logical)
}

/// `logical_point_to_native` over explicit `(logical geometry, ratio)` screens; the first entry is
/// the primary screen, used when no screen contains the point.
fn logical_to_native_on(screens: &[(Rect, f32)], logical: Point) -> Point {
    let (g, dpr) = screens
        .iter()
        .skip(1)
        .find(|(g, _)| g.contains(logical))
        .or_else(|| screens.first())
        .copied()
        .unwrap_or((Rect::new(0, 0, 0, 0), 1.0));
    crate::high_dpi::to_native_global_point(logical, Point::new(g.x, g.y), dpr)
}

#[cfg(test)]
mod logical_to_native_tests {
    use super::*;

    const PRIMARY_100: (Rect, f32) = (Rect { x: 0, y: 0, width: 1920, height: 1080 }, 1.0);
    // Native top-left (-2880, 0), logical (-2880, 0, 1920, 1080), ratio 1.5.
    const LEFT_150: (Rect, f32) = (Rect { x: -2880, y: 0, width: 1920, height: 1080 }, 1.5);

    #[test]
    fn a_point_on_a_scaled_secondary_screen_becomes_physical_pixels() {
        let screens = [PRIMARY_100, LEFT_150];
        // Inverse of `native_cursor_to_logical`: (-1440, 300) native <-> (-1920, 200) logical.
        assert_eq!(
            logical_to_native_on(&screens, Point::new(-1920, 200)),
            Point::new(-1440, 300)
        );
    }

    #[test]
    fn a_point_off_every_screen_uses_the_primary_ratio() {
        let screens = [(Rect { x: 0, y: 0, width: 1920, height: 1080 }, 1.5), LEFT_150];
        assert_eq!(
            logical_to_native_on(&screens, Point::new(-300, -300)),
            Point::new(-450, -450)
        );
    }
}

#[cfg(windows)]
pub mod win32_screen {
    use super::*;
    use std::ptr;
    use windows_sys::core::BOOL;
    use windows_sys::Win32::Foundation::{LPARAM, POINT, RECT};
    use windows_sys::Win32::Graphics::Gdi::{
        EnumDisplayMonitors, GetMonitorInfoW, MonitorFromPoint, MonitorFromWindow, HDC, HMONITOR,
        MONITORINFO, MONITORINFOEXW, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTONULL,
    };
    use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};

    /// The monitor's effective DPI as a ratio, at least 1.0.
    fn monitor_dpr(h_monitor: HMONITOR) -> f32 {
        let mut dpi_x = 96u32;
        let mut dpi_y = 96u32;
        // SAFETY: valid out pointers; a stale monitor handle makes the call fail.
        let hr = unsafe { GetDpiForMonitor(h_monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) };
        let dpr = if hr == 0 {
            dpi_x as f32 / 96.0
        } else {
            // SAFETY: no arguments.
            unsafe { windows_sys::Win32::UI::HiDpi::GetDpiForSystem() as f32 / 96.0 }
        };
        ferrokit_gui::round_scale_factor(dpr as f64) as f32
    }

    /// The monitor's physical top-left and ratio: what a desktop position on it maps with.
    fn monitor_mapping(h_monitor: HMONITOR) -> Option<(Point, f32)> {
        if h_monitor.is_null() {
            return None;
        }
        // SAFETY: a zeroed MONITORINFO with cbSize set is a valid out parameter.
        let rc = unsafe {
            let mut info: MONITORINFO = std::mem::zeroed();
            info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
            if GetMonitorInfoW(h_monitor, &mut info) == 0 {
                return None;
            }
            info.rcMonitor
        };
        Some((Point::new(rc.left, rc.top), monitor_dpr(h_monitor)))
    }

    /// The mapping of the screen a top-level window is on (`MonitorFromWindow`, as
    /// `QWindowsWindow` finds its screen); `None` without a monitor.
    pub fn window_screen_mapping(hwnd: isize) -> Option<(Point, f32)> {
        // SAFETY: any handle value is accepted; an invalid one yields the nearest monitor or null.
        monitor_mapping(unsafe { MonitorFromWindow(hwnd as _, MONITOR_DEFAULTTONEAREST) })
    }

    /// `QHighDpi::fromNativeGlobalPosition(pos, window)`: a desktop position maps through the
    /// screen that contains it, else through the window's screen (`qhighdpiscaling.cpp:646-681,
    /// 733-744`, used for mouse events at `qwindowsysteminterface.cpp:419-420`).
    pub fn from_native_global_point(pt: Point, hwnd: isize) -> Point {
        // SAFETY: plain value arguments.
        let at = unsafe { MonitorFromPoint(POINT { x: pt.x, y: pt.y }, MONITOR_DEFAULTTONULL) };
        match monitor_mapping(at).or_else(|| window_screen_mapping(hwnd)) {
            Some((origin, dpr)) => crate::high_dpi::from_native_global_point(pt, origin, dpr),
            None => pt,
        }
    }

    #[derive(Debug, Clone)]
    pub struct Win32Screen {
        name: String,
        geometry: Rect,
        available_geometry: Rect,
        is_primary: bool,
        dpr: f32,
    }

    impl Win32Screen {
        pub fn from_hmonitor(h_monitor: HMONITOR) -> Option<Self> {
            if h_monitor.is_null() {
                return None;
            }

            unsafe {
                let mut info_ex: MONITORINFOEXW = std::mem::zeroed();
                info_ex.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;

                if GetMonitorInfoW(h_monitor, &mut info_ex as *mut _ as *mut MONITORINFO) == 0 {
                    return None;
                }

                let rc = info_ex.monitorInfo.rcMonitor;
                let rc_work = info_ex.monitorInfo.rcWork;

                let geometry = Rect::new(
                    rc.left,
                    rc.top,
                    (rc.right - rc.left).max(0),
                    (rc.bottom - rc.top).max(0),
                );

                let available_geometry = Rect::new(
                    rc_work.left,
                    rc_work.top,
                    (rc_work.right - rc_work.left).max(0),
                    (rc_work.bottom - rc_work.top).max(0),
                );

                let is_primary = (info_ex.monitorInfo.dwFlags & 1) != 0;

                let mut name_len = 0;
                while name_len < info_ex.szDevice.len() && info_ex.szDevice[name_len] != 0 {
                    name_len += 1;
                }
                let name = String::from_utf16_lossy(&info_ex.szDevice[..name_len]);

                Some(Self {
                    name,
                    geometry,
                    available_geometry,
                    is_primary,
                    dpr: monitor_dpr(h_monitor),
                })
            }
        }

        /// `QWindowsScreen::primaryScreen` (`qwindowsscreen.cpp:355`):
        /// finds the screen whose monitor flags contain `MONITORINFOF_PRIMARY`.
        /// Falls back to system metrics if no monitors are enumerated.
        pub fn primary() -> Self {
            let all = Self::all_screens();
            if let Some(p) = all.into_iter().find(|s| s.is_primary) {
                return p;
            }
            Self::system_fallback_primary()
        }

        fn system_fallback_primary() -> Self {
            unsafe {
                use windows_sys::Win32::UI::WindowsAndMessaging::{
                    GetSystemMetrics, SystemParametersInfoW, SM_CXSCREEN, SM_CYSCREEN,
                    SPI_GETWORKAREA,
                };
                let width = GetSystemMetrics(SM_CXSCREEN);
                let height = GetSystemMetrics(SM_CYSCREEN);
                let mut work_area: RECT = std::mem::zeroed();
                SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut work_area as *mut _ as *mut _, 0);
                let dpi = windows_sys::Win32::UI::HiDpi::GetDpiForSystem();
                let dpr = (dpi as f32 / 96.0).max(1.0);

                Self {
                    name: "Primary Monitor".to_string(),
                    geometry: Rect::new(0, 0, width, height),
                    available_geometry: Rect::new(
                        work_area.left,
                        work_area.top,
                        (work_area.right - work_area.left).max(0),
                        (work_area.bottom - work_area.top).max(0),
                    ),
                    is_primary: true,
                    dpr,
                }
            }
        }

        pub fn all_screens() -> Vec<Self> {
            let mut list = Vec::<Self>::new();
            unsafe {
                unsafe extern "system" fn monitor_enum_proc(
                    h_monitor: HMONITOR,
                    _hdc: HDC,
                    _rect: *mut RECT,
                    lparam: LPARAM,
                ) -> BOOL {
                    let list_ref = &mut *(lparam as *mut Vec<Win32Screen>);
                    if let Some(screen) = Win32Screen::from_hmonitor(h_monitor) {
                        if screen.is_primary {
                            list_ref.insert(0, screen);
                        } else {
                            list_ref.push(screen);
                        }
                    }
                    1
                }

                EnumDisplayMonitors(
                    ptr::null_mut(),
                    ptr::null(),
                    Some(monitor_enum_proc),
                    &mut list as *mut _ as LPARAM,
                );
            }

            if list.is_empty() {
                list.push(Self::system_fallback_primary());
            }

            list
        }

        /// `QGuiApplication::screenAt` (qguiapplication.cpp:1147-1164): queries screens in
        /// logical coordinate space (`sibling->geometry().contains(point)`), not native pixels.
        pub fn screen_at(pos: Point) -> Option<Self> {
            Self::all_screens()
                .into_iter()
                .find(|s| PlatformScreen::geometry(s).contains(pos))
        }
    }

    impl PlatformScreen for Win32Screen {
        fn name(&self) -> String {
            self.name.clone()
        }

        /// `QScreenPrivate::updateGeometry` maps `rcMonitor` and `rcWork` with
        /// `fromNativeGlobalPosition` against this screen's native top-left (`qscreen.cpp:65-69`,
        /// `qhighdpiscaling.cpp:713-721`), so the screen keeps its native top-left.
        fn geometry(&self) -> Rect {
            crate::high_dpi::from_native_global_rect(
                self.geometry,
                self.geometry.top_left(),
                self.dpr,
            )
        }

        fn available_geometry(&self) -> Rect {
            crate::high_dpi::from_native_global_rect(
                self.available_geometry,
                self.geometry.top_left(),
                self.dpr,
            )
        }

        fn is_primary(&self) -> bool {
            self.is_primary
        }

        fn device_pixel_ratio(&self) -> f32 {
            self.dpr
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn screen(native: Rect, work: Rect, dpr: f32) -> Win32Screen {
            Win32Screen {
                name: String::new(),
                geometry: native,
                available_geometry: work,
                is_primary: false,
                dpr,
            }
        }

        // QScreenPrivate::updateGeometry maps rcMonitor and rcWork with fromNativeGlobalPosition,
        // relative to the screen's native top-left (qscreen.cpp:65-69, qhighdpiscaling.cpp:720),
        // so a screen keeps its native top-left and only its size scales.

        #[test]
        fn a_scaled_screen_right_of_the_primary_keeps_its_native_origin() {
            // 150% screen right of a 1920 px primary, taskbar of 72 native px at its bottom.
            let s = screen(
                Rect::new(1920, 0, 2880, 1620),
                Rect::new(1920, 0, 2880, 1548),
                1.5,
            );
            assert_eq!(s.geometry(), Rect::new(1920, 0, 1920, 1080));
            assert_eq!(s.available_geometry(), Rect::new(1920, 0, 1920, 1032));
        }

        #[test]
        fn a_scaled_screen_left_of_the_primary_keeps_its_negative_origin() {
            let s = screen(
                Rect::new(-2880, 0, 2880, 1620),
                Rect::new(-2880, 0, 2880, 1620),
                1.5,
            );
            assert_eq!(s.geometry(), Rect::new(-2880, 0, 1920, 1080));
            assert_eq!(s.available_geometry(), Rect::new(-2880, 0, 1920, 1080));
        }

        #[test]
        fn a_work_area_offset_from_the_screen_origin_scales_relative_to_that_origin() {
            // 150% screen below the primary with a 48 native px taskbar at its top: rcWork starts
            // 48 px below rcMonitor, which is 32 logical px.
            let s = screen(
                Rect::new(0, 1080, 2880, 1620),
                Rect::new(0, 1128, 2880, 1572),
                1.5,
            );
            assert_eq!(s.geometry(), Rect::new(0, 1080, 1920, 1080));
            assert_eq!(s.available_geometry(), Rect::new(0, 1112, 1920, 1048));
        }

        #[test]
        fn a_scaled_screen_at_the_desktop_origin_and_an_unscaled_screen_beside_it() {
            let primary = screen(
                Rect::new(0, 0, 2880, 1620),
                Rect::new(0, 0, 2880, 1572),
                1.5,
            );
            assert_eq!(primary.geometry(), Rect::new(0, 0, 1920, 1080));
            assert_eq!(primary.available_geometry(), Rect::new(0, 0, 1920, 1048));
            let side = screen(
                Rect::new(2880, 0, 1920, 1080),
                Rect::new(2880, 0, 1920, 1032),
                1.0,
            );
            assert_eq!(side.geometry(), Rect::new(2880, 0, 1920, 1080));
            assert_eq!(side.available_geometry(), Rect::new(2880, 0, 1920, 1032));
        }
    }
}

#[cfg(windows)]
pub use win32_screen::Win32Screen;

#[derive(Debug, Clone)]
pub struct GenericScreen {
    name: String,
    geometry: Rect,
    available_geometry: Rect,
    is_primary: bool,
    dpr: f32,
}

impl GenericScreen {
    pub fn new(
        name: impl Into<String>,
        geometry: Rect,
        available_geometry: Rect,
        is_primary: bool,
        dpr: f32,
    ) -> Self {
        Self {
            name: name.into(),
            geometry,
            available_geometry,
            is_primary,
            dpr,
        }
    }

    pub fn default_primary() -> Self {
        Self {
            name: "DefaultScreen".to_string(),
            geometry: Rect::new(0, 0, 1920, 1080),
            available_geometry: Rect::new(0, 0, 1920, 1040),
            is_primary: true,
            dpr: 1.0,
        }
    }
}

impl PlatformScreen for GenericScreen {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn geometry(&self) -> Rect {
        self.geometry
    }

    fn available_geometry(&self) -> Rect {
        self.available_geometry
    }

    fn is_primary(&self) -> bool {
        self.is_primary
    }

    fn device_pixel_ratio(&self) -> f32 {
        self.dpr
    }
}

pub mod cocoa_screen {
    use super::*;
    use crate::objc_runtime::{Class, Id, ObjcMsg, Sel};
    use crate::window_cocoa::qt_mac_flip_rect;

    #[derive(Debug, Clone)]
    pub struct CocoaScreen {
        name: String,
        geometry: Rect,
        available_geometry: Rect,
        is_primary: bool,
        dpr: f32,
    }

    impl CocoaScreen {
        pub fn from_nsscreen(screen: Id, is_primary: bool, primary_height: i32) -> Self {
            let sel_frame = Sel::register("frame");
            let sel_visible_frame = Sel::register("visibleFrame");
            let sel_scale = Sel::register("backingScaleFactor");

            let frame = unsafe { ObjcMsg::send_rect_return(screen, sel_frame) };
            let visible_frame = unsafe { ObjcMsg::send_rect_return(screen, sel_visible_frame) };
            let scale = unsafe { ObjcMsg::send_f64_return(screen, sel_scale) };

            let raw_geom = Rect::new(
                frame.origin.x as i32,
                frame.origin.y as i32,
                frame.size.width as i32,
                frame.size.height as i32,
            );
            let raw_avail = Rect::new(
                visible_frame.origin.x as i32,
                visible_frame.origin.y as i32,
                visible_frame.size.width as i32,
                visible_frame.size.height as i32,
            );

            let geometry = qt_mac_flip_rect(raw_geom, primary_height);
            let available_geometry = qt_mac_flip_rect(raw_avail, primary_height);

            Self {
                name: if is_primary {
                    "CocoaPrimaryScreen".to_string()
                } else {
                    "CocoaSecondaryScreen".to_string()
                },
                geometry,
                available_geometry,
                is_primary,
                dpr: scale as f32,
            }
        }

        pub fn primary() -> Self {
            let nsscreen_class = Class::get("NSScreen").unwrap_or(Class::NIL);
            let main_screen =
                unsafe { ObjcMsg::send_class_0(nsscreen_class, Sel::register("mainScreen")) };
            if !main_screen.is_nil() {
                let frame =
                    unsafe { ObjcMsg::send_rect_return(main_screen, Sel::register("frame")) };
                let primary_height = frame.size.height as i32;
                Self::from_nsscreen(main_screen, true, primary_height)
            } else {
                Self {
                    name: "CocoaPrimaryScreen".to_string(),
                    geometry: Rect::new(0, 0, 1920, 1080),
                    available_geometry: Rect::new(0, 25, 1920, 1055),
                    is_primary: true,
                    dpr: 2.0,
                }
            }
        }

        pub fn screens() -> Vec<Self> {
            let nsscreen_class = Class::get("NSScreen").unwrap_or(Class::NIL);
            let screens_array =
                unsafe { ObjcMsg::send_class_0(nsscreen_class, Sel::register("screens")) };
            if screens_array.is_nil() {
                return vec![Self::primary()];
            }

            let count =
                unsafe { ObjcMsg::send_usize_return(screens_array, Sel::register("count")) };
            if count == 0 {
                return vec![Self::primary()];
            }

            let first = unsafe {
                ObjcMsg::send_id_with_usize(screens_array, Sel::register("objectAtIndex:"), 0)
            };
            let first_frame = unsafe { ObjcMsg::send_rect_return(first, Sel::register("frame")) };
            let primary_height = first_frame.size.height as i32;

            let mut result = Vec::with_capacity(count);
            for i in 0..count {
                let screen_id = unsafe {
                    ObjcMsg::send_id_with_usize(screens_array, Sel::register("objectAtIndex:"), i)
                };
                if !screen_id.is_nil() {
                    result.push(Self::from_nsscreen(screen_id, i == 0, primary_height));
                }
            }

            if result.is_empty() {
                result.push(Self::primary());
            }

            result
        }

        pub fn screen_at(pos: Point) -> Option<Self> {
            Self::screens()
                .into_iter()
                .find(|s| s.geometry.contains(pos))
        }
    }

    impl PlatformScreen for CocoaScreen {
        fn name(&self) -> String {
            self.name.clone()
        }

        fn geometry(&self) -> Rect {
            self.geometry
        }

        fn available_geometry(&self) -> Rect {
            self.available_geometry
        }

        fn is_primary(&self) -> bool {
            self.is_primary
        }

        fn device_pixel_ratio(&self) -> f32 {
            self.dpr
        }
    }
}

pub use cocoa_screen::CocoaScreen;

pub mod x11_screen {
    use super::*;

    #[derive(Debug, Clone)]
    pub struct X11Screen {
        name: String,
        geometry: Rect,
        available_geometry: Rect,
        is_primary: bool,
        dpr: f32,
    }

    impl X11Screen {
        pub fn primary() -> Self {
            Self {
                name: "X11PrimaryScreen".to_string(),
                geometry: Rect::new(0, 0, 1920, 1080),
                available_geometry: Rect::new(0, 0, 1920, 1040),
                is_primary: true,
                dpr: 1.0,
            }
        }

        pub fn screens() -> Vec<Self> {
            vec![Self::primary()]
        }

        pub fn screen_at(pos: Point) -> Option<Self> {
            let primary = Self::primary();
            if primary.geometry.contains(pos) {
                Some(primary)
            } else {
                None
            }
        }
    }

    impl PlatformScreen for X11Screen {
        fn name(&self) -> String {
            self.name.clone()
        }

        fn geometry(&self) -> Rect {
            self.geometry
        }

        fn available_geometry(&self) -> Rect {
            self.available_geometry
        }

        fn is_primary(&self) -> bool {
            self.is_primary
        }

        fn device_pixel_ratio(&self) -> f32 {
            self.dpr
        }
    }
}

pub use x11_screen::X11Screen;

/// Clamps a candidate window rectangle so that it is guaranteed to be visible on an active screen.
///
/// Behavior:
/// 1. Finds the screen with the largest intersection with `rect`.
/// 2. If the intersection is non-empty, shifts and clamps the window inside that screen's `available_geometry`.
/// 3. If `rect` is completely off-screen (e.g. disconnected external monitor), safely positions
///    the window on the primary screen (or first available screen) within its `available_geometry`.
pub fn ensure_within_screens(
    rect: Rect,
    screens: &[&dyn PlatformScreen],
    min_visible_px: i32,
) -> Rect {
    if screens.is_empty() {
        return rect;
    }

    let min_visible = min_visible_px.max(1);

    // Find primary screen (fallback to first)
    let primary_screen = screens
        .iter()
        .find(|s| s.is_primary())
        .copied()
        .unwrap_or(screens[0]);

    // Find screen with largest intersection
    let mut best_screen: Option<&dyn PlatformScreen> = None;
    let mut best_intersection_area = 0i64;

    for &screen in screens {
        let avail = screen.available_geometry();
        // Calculate intersection of rect and avail
        let inter_x1 = rect.x.max(avail.x);
        let inter_y1 = rect.y.max(avail.y);
        let inter_x2 = (rect.x + rect.width).min(avail.x + avail.width);
        let inter_y2 = (rect.y + rect.height).min(avail.y + avail.height);

        let inter_w = (inter_x2 - inter_x1).max(0);
        let inter_h = (inter_y2 - inter_y1).max(0);

        if inter_w >= min_visible && inter_h >= min_visible {
            let area = (inter_w as i64) * (inter_h as i64);
            if area > best_intersection_area {
                best_intersection_area = area;
                best_screen = Some(screen);
            }
        }
    }

    let target_screen = best_screen.unwrap_or(primary_screen);
    let avail = target_screen.available_geometry();

    let clamped_w = rect.width.min(avail.width).max(10);
    let clamped_h = rect.height.min(avail.height).max(10);

    if best_screen.is_some() {
        // Clamp existing coordinates inside target available area
        let max_x = avail.x + (avail.width - clamped_w).max(0);
        let max_y = avail.y + (avail.height - clamped_h).max(0);
        let clamped_x = rect.x.clamp(avail.x, max_x);
        let clamped_y = rect.y.clamp(avail.y, max_y);
        Rect::new(clamped_x, clamped_y, clamped_w, clamped_h)
    } else {
        // Window was completely off-screen, reposition safely into primary available area
        let default_x = avail.x + ((avail.width - clamped_w) / 2).max(0);
        let default_y = avail.y + ((avail.height - clamped_h) / 4).max(0);
        Rect::new(default_x, default_y, clamped_w, clamped_h)
    }
}

/// Convenience helper to clamp a window rectangle to the active platform screens.
pub fn clamp_window_rect_to_screens(rect: Rect) -> Rect {
    let p = crate::integration::platform();
    let screens = p.screens();
    let screen_refs: Vec<&dyn PlatformScreen> = screens.iter().map(|s| s.as_ref()).collect();
    ensure_within_screens(rect, &screen_refs, 32)
}
