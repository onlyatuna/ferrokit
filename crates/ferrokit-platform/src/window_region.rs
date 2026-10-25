// SPDX-License-Identifier: LGPL-3.0-only
//! Windows window regions (`QWindowsWindow::setMask`) and owner windows
//! (`QWindowsWindow::updateTransientParent`).

use ferrokit_gui::geometry::primitives::Rect;
use ferrokit_gui::geometry::Region;
use std::ptr;
use windows_sys::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    ClientToScreen, CreateRectRgn, DeleteObject, ExtCreateRegion, OffsetRgn, SetWindowRgn, HRGN,
    RDH_RECTANGLES, RGNDATA,
};
#[cfg(test)]
use windows_sys::Win32::Graphics::Gdi::{GetRegionData, GetWindowRgn};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetParent, GetWindow, GetWindowLongPtrW, GetWindowRect,
    SetWindowLongPtrW, SetWindowPos, GWLP_HWNDPARENT, GWL_STYLE, GW_OWNER, SWP_FRAMECHANGED,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, WS_CHILD,
};

/// `RGNDATAHEADER` is 32 bytes (`dwSize`, `iType`, `nCount`, `nRgnSize`, `rcBound`).
const HEADER_WORDS: usize = 8;

/// `qRegionToWinRegion`: the GDI region with the rectangles of `region` (`RECT` edges are
/// exclusive, as `Rect`'s are). An empty region gives an empty GDI region. Null when GDI
/// refuses. The caller owns the handle.
pub(crate) fn region_to_hrgn(region: &Region) -> HRGN {
    let rects = region.rects();
    match rects {
        [] => unsafe { CreateRectRgn(0, 0, 0, 0) },
        [r] => unsafe { CreateRectRgn(r.x, r.y, r.right(), r.bottom()) },
        _ => {
            // Header followed by the rectangles; every field is 4 bytes, so an `i32` buffer
            // has the alignment `RGNDATA` needs.
            let bound = region.bounding_rect();
            let mut words = vec![0i32; HEADER_WORDS + rects.len() * 4];
            words[0] = (HEADER_WORDS * 4) as i32; // dwSize
            words[1] = RDH_RECTANGLES as i32; // iType
            words[2] = rects.len() as i32; // nCount
            words[3] = (rects.len() * 16) as i32; // nRgnSize
            words[4] = bound.x;
            words[5] = bound.y;
            words[6] = bound.right();
            words[7] = bound.bottom();
            for (i, r) in rects.iter().enumerate() {
                let at = HEADER_WORDS + i * 4;
                words[at] = r.x;
                words[at + 1] = r.y;
                words[at + 2] = r.right();
                words[at + 3] = r.bottom();
            }
            unsafe {
                ExtCreateRegion(
                    ptr::null(),
                    (words.len() * 4) as u32,
                    words.as_ptr().cast::<RGNDATA>(),
                )
            }
        }
    }
}

/// The rectangles of a GDI region (`GetRegionData`), or `None` when it cannot be read.
#[cfg(test)]
pub(crate) fn hrgn_rects(rgn: HRGN) -> Option<Vec<Rect>> {
    unsafe {
        let bytes = GetRegionData(rgn, 0, ptr::null_mut());
        if bytes == 0 {
            return None;
        }
        let mut words = vec![0i32; (bytes as usize).div_ceil(4)];
        if GetRegionData(rgn, bytes, words.as_mut_ptr().cast::<RGNDATA>()) == 0 {
            return None;
        }
        let count = words[2].max(0) as usize;
        if HEADER_WORDS + count * 4 > words.len() {
            return None;
        }
        Some(
            (0..count)
                .map(|i| {
                    let at = HEADER_WORDS + i * 4;
                    Rect::new(
                        words[at],
                        words[at + 1],
                        words[at + 2] - words[at],
                        words[at + 3] - words[at + 1],
                    )
                })
                .collect(),
        )
    }
}

/// Where the client area starts inside the window rectangle: a window region is in window
/// coordinates, a mask in client coordinates (`QWindowsWindow::setMask` offsets by the frame
/// margins, `qwindowswindow.cpp`).
fn client_origin_in_window(hwnd: HWND) -> (i32, i32) {
    unsafe {
        let mut window: RECT = std::mem::zeroed();
        let mut client = POINT { x: 0, y: 0 };
        if GetWindowRect(hwnd, &mut window) == 0 || ClientToScreen(hwnd, &mut client) == 0 {
            return (0, 0);
        }
        (client.x - window.left, client.y - window.top)
    }
}

/// `QWindowsWindow::setMask`: an empty region removes the window region (`SetWindowRgn(NULL)`);
/// otherwise the window is clipped to `region`, given in client coordinates. The system takes the
/// region over on success. Returns whether the window has the requested region.
pub(crate) fn set_window_mask(hwnd: HWND, region: &Region) -> bool {
    if hwnd.is_null() {
        return false;
    }
    unsafe {
        if region.is_empty() {
            return SetWindowRgn(hwnd, ptr::null_mut(), 1) != 0;
        }
        // The client origin depends on the frame the window procedure last computed
        // (`WM_NCCALCSIZE`); a window that never had it computed still has the default frame,
        // which `SetWindowRgn` then replaces. Settle it first so the offset is the final one.
        SetWindowPos(
            hwnd,
            ptr::null_mut(),
            0,
            0,
            0,
            0,
            SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
        let rgn = region_to_hrgn(region);
        if rgn.is_null() {
            return false;
        }
        let (dx, dy) = client_origin_in_window(hwnd);
        if dx != 0 || dy != 0 {
            OffsetRgn(rgn, dx, dy);
        }
        if SetWindowRgn(hwnd, rgn, 1) == 0 {
            // The system owns the region only when the call succeeds.
            DeleteObject(rgn);
            return false;
        }
        true
    }
}

/// The window region of `hwnd` as client-coordinate rectangles, or `None` when it has none.
#[cfg(test)]
pub(crate) fn window_mask_rects(hwnd: HWND) -> Option<Vec<Rect>> {
    if hwnd.is_null() {
        return None;
    }
    unsafe {
        let probe = CreateRectRgn(0, 0, 0, 0);
        if probe.is_null() {
            return None;
        }
        // GetWindowRgn copies the window region into `probe`; ERROR (0) means there is none.
        let kind = GetWindowRgn(hwnd, probe);
        let rects = if kind == 0 { None } else { hrgn_rects(probe) };
        DeleteObject(probe);
        let (dx, dy) = client_origin_in_window(hwnd);
        rects.map(|r| {
            r.into_iter()
                .map(|r| Rect::new(r.x - dx, r.y - dy, r.width, r.height))
                .collect()
        })
    }
}

/// `QWindowsWindow::updateTransientParent`: makes `owner` the owner of `hwnd` (a top-level
/// window stays top-level, `SetParent` would make it a child). A child-window owner is replaced
/// by its parent until a top-level window is found (QTSOLBUG-71).
pub(crate) fn set_owner(hwnd: HWND, owner: Option<HWND>) {
    if hwnd.is_null() {
        return;
    }
    unsafe {
        let old = GetWindow(hwnd, GW_OWNER);
        let mut new = owner.unwrap_or(ptr::null_mut());
        while !new.is_null() && (GetWindowLongPtrW(new, GWL_STYLE) as u32 & WS_CHILD) != 0 {
            new = GetParent(new);
        }
        if new != old {
            SetWindowLongPtrW(hwnd, GWLP_HWNDPARENT, new as isize);
        }
    }
}

/// The owner of `hwnd`, if it has one.
pub(crate) fn owner_of(hwnd: HWND) -> Option<HWND> {
    if hwnd.is_null() {
        return None;
    }
    let owner = unsafe { GetWindow(hwnd, GW_OWNER) };
    (!owner.is_null()).then_some(owner)
}

struct OwnedScan<'a> {
    owner: HWND,
    class: &'a [u16],
    found: Vec<HWND>,
}

unsafe extern "system" fn collect_owned(hwnd: HWND, lparam: LPARAM) -> i32 {
    // SAFETY: `lparam` is the `OwnedScan` that `release_owned_windows` keeps alive across
    // `EnumWindows`, which calls this on the same thread before it returns.
    let scan = unsafe { &mut *(lparam as *mut OwnedScan) };
    if unsafe { GetWindow(hwnd, GW_OWNER) } == scan.owner {
        let mut name = [0u16; 64];
        let len = unsafe { GetClassNameW(hwnd, name.as_mut_ptr(), name.len() as i32) };
        if len > 0 && name[..len as usize] == *scan.class {
            scan.found.push(hwnd);
        }
    }
    1
}

/// `QWindowsWindow::destroyWindow` (`qwindowswindow.cpp:1708-1718`): the windows this one owns
/// stop being owned before it is destroyed, as Windows would destroy them with it
/// (QTBUG-35499, QTBUG-36666). Only windows of `class` (the ferrokit window class, without its
/// terminating NUL) are released.
pub(crate) fn release_owned_windows(owner: HWND, class: &[u16]) {
    if owner.is_null() {
        return;
    }
    let mut scan = OwnedScan {
        owner,
        class: class.strip_suffix(&[0]).unwrap_or(class),
        found: Vec::new(),
    };
    unsafe {
        EnumWindows(Some(collect_owned), &mut scan as *mut OwnedScan as LPARAM);
    }
    for hwnd in scan.found {
        set_owner(hwnd, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::Graphics::Gdi::{PtInRegion, RectInRegion};

    fn hit(rgn: HRGN, x: i32, y: i32) -> bool {
        unsafe { PtInRegion(rgn, x, y) != 0 }
    }

    /// A region with several rectangles (a rounded rectangle) becomes one GDI region with the
    /// same area, edges exclusive.
    #[test]
    fn a_multi_rect_region_converts_to_the_same_gdi_area() {
        let mut region = Region::new();
        region = region.united_rect(&Rect::new(2, 0, 6, 1));
        region = region.united_rect(&Rect::new(1, 1, 8, 1));
        region = region.united_rect(&Rect::new(0, 2, 10, 6));
        assert!(region.rect_count() > 1);
        let rgn = region_to_hrgn(&region);
        assert!(!rgn.is_null());
        for y in -1..10 {
            for x in -1..12 {
                assert_eq!(
                    hit(rgn, x, y),
                    region.contains_point(ferrokit_gui::geometry::primitives::Point::new(x, y)),
                    "pixel ({x}, {y})"
                );
            }
        }
        let rects = hrgn_rects(rgn).expect("region data");
        assert_eq!(Region::from_rects(&rects), region);
        unsafe { DeleteObject(rgn) };
    }

    #[test]
    fn a_single_rect_region_converts_with_exclusive_edges() {
        let region = Region::from_rect(Rect::new(3, 4, 5, 2));
        let rgn = region_to_hrgn(&region);
        assert!(hit(rgn, 3, 4) && hit(rgn, 7, 5));
        assert!(!hit(rgn, 8, 5) && !hit(rgn, 7, 6) && !hit(rgn, 2, 4));
        let probe = RECT { left: 3, top: 4, right: 8, bottom: 6 };
        assert_ne!(unsafe { RectInRegion(rgn, &probe) }, 0);
        unsafe { DeleteObject(rgn) };
    }

    #[test]
    fn an_empty_region_converts_to_an_empty_gdi_region() {
        let rgn = region_to_hrgn(&Region::new());
        assert!(!rgn.is_null());
        assert_eq!(hrgn_rects(rgn).map(|r| r.len()), Some(0));
        unsafe { DeleteObject(rgn) };
    }

    // The tests below create hidden native windows (never shown) and run in CI.

    use crate::platform_window::PlatformWindow;
    use crate::window::{NativeWindow, WindowFlags};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, IsWindow, GWL_EXSTYLE, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
        WS_EX_TOPMOST,
    };

    fn rounded_region() -> Region {
        Region::from_rects(&[
            Rect::new(3, 0, 94, 1),
            Rect::new(1, 1, 98, 2),
            Rect::new(0, 3, 100, 54),
            Rect::new(1, 57, 98, 2),
            Rect::new(3, 59, 94, 1),
        ])
    }

    /// `QWindowsWindow::setMask`: the mask is the window region, given in client coordinates
    /// whether or not the window has a frame, and an empty mask removes it.
    #[test]
    fn a_mask_is_the_window_region_in_client_coordinates() {
        for flags in [WindowFlags::FRAMELESS, WindowFlags::NORMAL] {
            let mut win = NativeWindow::new("mask", Rect::new(50, 50, 220, 160), flags)
                .expect("native window");
            let hwnd = win.hwnd();
            assert_eq!(window_mask_rects(hwnd), None, "a new window has no region");

            let region = rounded_region();
            PlatformWindow::set_mask(&mut win, &region);
            let rects = window_mask_rects(hwnd).expect("the mask is the window region");
            assert_eq!(Region::from_rects(&rects), region, "flags {flags:?}");

            PlatformWindow::set_mask(&mut win, &Region::new());
            assert_eq!(window_mask_rects(hwnd), None, "an empty mask removes the region");
        }
    }

    /// A Tool | Frameless | StaysOnTop window with a transient parent: owned by the parent
    /// (`GW_OWNER`), no taskbar button, topmost; it survives its owner's destruction because the
    /// owner releases it first (`QWindowsWindow::destroyWindow`).
    #[test]
    fn a_tool_window_is_owned_by_its_transient_parent() {
        let parent = NativeWindow::new("owner", Rect::new(50, 50, 220, 160), WindowFlags::NORMAL)
            .expect("owner");
        let mut tool = NativeWindow::new(
            "tool",
            Rect::new(80, 80, 100, 60),
            WindowFlags::TOOL | WindowFlags::FRAMELESS | WindowFlags::STAYS_ON_TOP,
        )
        .expect("tool window");
        let tool_hwnd = tool.hwnd();
        let ex_style = unsafe { GetWindowLongPtrW(tool_hwnd, GWL_EXSTYLE) } as u32;
        assert_ne!(ex_style & WS_EX_TOOLWINDOW, 0, "no taskbar button");
        assert_eq!(ex_style & WS_EX_APPWINDOW, 0);
        assert_ne!(ex_style & WS_EX_TOPMOST, 0);

        assert_eq!(tool.transient_parent(), None);
        tool.set_transient_parent(Some(parent.hwnd() as isize));
        assert_eq!(tool.transient_parent(), Some(parent.hwnd() as isize));
        assert_eq!(owner_of(tool_hwnd), Some(parent.hwnd()));

        tool.set_transient_parent(None);
        assert_eq!(tool.transient_parent(), None);

        tool.set_transient_parent(Some(parent.hwnd() as isize));
        drop(parent);
        assert_ne!(
            unsafe { IsWindow(tool_hwnd) },
            0,
            "dropping the owner must not destroy the window it owned"
        );
        assert_eq!(tool.transient_parent(), None);
    }
}
