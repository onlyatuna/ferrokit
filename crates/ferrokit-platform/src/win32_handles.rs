// SPDX-License-Identifier: LGPL-3.0-only
//! Ownership guards for Win32 handles.
//!
//! Every guard owns one handle, tolerates a null handle in `Drop`, and releases it with the one
//! API that matches the allocator. `into_raw` hands the handle to the system (or to an API that
//! must return it raw) and disarms the guard; call it only after the system accepted ownership.

use std::ffi::c_void;
use windows_sys::Win32::Foundation::{GlobalFree, HGLOBAL};
use windows_sys::Win32::Graphics::Gdi::{DeleteObject, HRGN};
use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows_sys::Win32::UI::WindowsAndMessaging::{CreatePopupMenu, DestroyMenu, HMENU};

/// An owned movable global memory block (`GlobalAlloc(GMEM_MOVEABLE)`), freed with `GlobalFree`.
///
/// The clipboard takes ownership of the block when `SetClipboardData` succeeds; until then the
/// guard owns it and frees it on every failure path.
pub(crate) struct GlobalMem(HGLOBAL);

impl GlobalMem {
    /// Allocates a movable block holding a copy of `bytes` (at least one byte is allocated so
    /// that an empty payload still yields a valid handle).
    pub(crate) fn alloc_copy(bytes: &[u8]) -> Result<Self, &'static str> {
        // SAFETY: GlobalAlloc takes no pointers; a null result is checked below.
        let hmem = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1)) };
        if hmem.is_null() {
            return Err("GlobalAlloc failed");
        }
        // From here on the guard owns the block, so every early return frees it.
        let mem = Self(hmem);
        // SAFETY: `hmem` is the live movable block allocated above and owned by `mem`.
        let ptr = unsafe { GlobalLock(hmem) as *mut u8 };
        if ptr.is_null() {
            return Err("GlobalLock failed");
        }
        // SAFETY: the block is locked and was allocated with `bytes.len().max(1)` bytes, so it
        // holds at least `bytes.len()` writable bytes; the source slice is a distinct Rust
        // allocation, so the ranges cannot overlap. `hmem` is locked once, so one unlock matches.
        unsafe {
            if !bytes.is_empty() {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
            }
            GlobalUnlock(hmem);
        }
        Ok(mem)
    }

    /// The raw handle, still owned by the guard.
    pub(crate) fn as_raw(&self) -> HGLOBAL {
        self.0
    }

    /// Releases ownership to the caller. Use only after the system accepted the block
    /// (`SetClipboardData` returned non-null).
    #[must_use = "the returned handle is no longer freed by the guard"]
    pub(crate) fn into_raw(self) -> *mut c_void {
        let hmem = self.0;
        std::mem::forget(self);
        hmem
    }
}

impl Drop for GlobalMem {
    fn drop(&mut self) {
        if self.0.is_null() {
            return;
        }
        // SAFETY: `self.0` is a global block allocated by `GlobalAlloc` that this guard still
        // owns: `into_raw` is the only way to give it away and it forgets the guard, and the
        // clipboard only takes blocks that `SetClipboardData` accepted, which are never wrapped
        // in a live guard.
        unsafe { GlobalFree(self.0) };
    }
}

/// An owned GDI region, deleted with `DeleteObject`.
///
/// `SetWindowRgn` takes ownership of the region when it succeeds (and deletes the previous
/// window region itself); on failure the caller keeps it. So `into_raw` is called only after a
/// successful `SetWindowRgn`.
pub(crate) struct OwnedRgn(HRGN);

impl OwnedRgn {
    /// Takes ownership of a region returned by `CreateRectRgn`/`ExtCreateRegion`; `None` when
    /// GDI returned null.
    pub(crate) fn from_raw(rgn: HRGN) -> Option<Self> {
        (!rgn.is_null()).then_some(Self(rgn))
    }

    /// The raw handle, still owned by the guard.
    pub(crate) fn as_raw(&self) -> HRGN {
        self.0
    }

    /// Releases ownership to the caller. Use only after the system accepted the region
    /// (`SetWindowRgn` returned non-zero).
    #[must_use = "the returned region is no longer deleted by the guard"]
    pub(crate) fn into_raw(self) -> HRGN {
        let rgn = self.0;
        std::mem::forget(self);
        rgn
    }
}

impl Drop for OwnedRgn {
    fn drop(&mut self) {
        if self.0.is_null() {
            return;
        }
        // SAFETY: `self.0` is a region created by GDI that this guard still owns: `from_raw` only
        // wraps a region its caller just created and does not share, and `into_raw` forgets the
        // guard. A region the system took over (SetWindowRgn success) is never wrapped any more.
        unsafe { DeleteObject(self.0) };
    }
}

/// An owned popup menu, destroyed with `DestroyMenu` (which also destroys its submenus).
///
/// `AppendMenuW(MF_POPUP)` makes the parent menu own the appended submenu once it succeeds;
/// until then the submenu is still owned by its guard, so `into_raw` on a submenu is called only
/// after that append succeeded.
pub(crate) struct OwnedMenu(HMENU);

impl OwnedMenu {
    /// A new empty popup menu; `None` when `CreatePopupMenu` fails.
    pub(crate) fn create_popup() -> Option<Self> {
        // SAFETY: CreatePopupMenu takes no arguments; null is mapped to `None`.
        let menu = unsafe { CreatePopupMenu() };
        (!menu.is_null()).then_some(Self(menu))
    }

    /// Takes ownership of `menu`, which may be null (the guard then does nothing on drop).
    pub(crate) fn from_raw(menu: HMENU) -> Self {
        Self(menu)
    }

    /// The raw handle, still owned by the guard.
    pub(crate) fn as_raw(&self) -> HMENU {
        self.0
    }

    /// Releases ownership to the caller: after `AppendMenuW(MF_POPUP)` succeeded (the parent
    /// owns the submenu now), or where an existing API hands the raw menu to its caller.
    #[must_use = "the returned menu is no longer destroyed by the guard"]
    pub(crate) fn into_raw(self) -> HMENU {
        let menu = self.0;
        std::mem::forget(self);
        menu
    }
}

impl Drop for OwnedMenu {
    fn drop(&mut self) {
        if self.0.is_null() {
            return;
        }
        // SAFETY: `self.0` is a menu this guard still owns: it came from CreatePopupMenu or from
        // a builder that handed it over, and it is never attached to a parent (AppendMenuW
        // MF_POPUP success) or released by `into_raw` while a guard exists, so no one else
        // destroys it. DestroyMenu is recursive and only fails on an invalid handle.
        unsafe { DestroyMenu(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::Graphics::Gdi::CreateRectRgn;
    use windows_sys::Win32::System::Memory::GlobalSize;
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetGuiResources, GR_GDIOBJECTS,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{AppendMenuW, IsMenu, MF_POPUP, MF_STRING};

    #[test]
    fn global_mem_copies_bytes_and_frees_on_drop() {
        let mem = GlobalMem::alloc_copy(&[1, 2, 3, 4]).unwrap();
        let raw = mem.as_raw();
        // SAFETY: `raw` is a live block owned by `mem`.
        assert!(unsafe { GlobalSize(raw) } >= 4);
        // SAFETY: as above; the block is locked, read, unlocked.
        let copied = unsafe {
            let ptr = GlobalLock(raw) as *const u8;
            let bytes = std::slice::from_raw_parts(ptr, 4).to_vec();
            GlobalUnlock(raw);
            bytes
        };
        assert_eq!(copied, [1, 2, 3, 4]);
        drop(mem);
        // SAFETY: GlobalSize only validates the handle; a freed handle yields 0.
        assert_eq!(unsafe { GlobalSize(raw) }, 0, "drop must free the block");
    }

    #[test]
    fn global_mem_into_raw_transfers_ownership() {
        let raw = GlobalMem::alloc_copy(&[]).unwrap().into_raw();
        // SAFETY: the guard was disarmed, so `raw` is still a live block.
        assert!(unsafe { GlobalSize(raw) } >= 1, "into_raw must not free");
        // SAFETY: the test now owns `raw` and frees it exactly once.
        assert!(unsafe { GlobalFree(raw) }.is_null());
    }

    /// GDI objects of this process. GDI keeps answering queries about a just-deleted region
    /// handle, so the object count is the reliable witness of a delete.
    fn gdi_objects() -> u32 {
        // SAFETY: GetGuiResources only reads the per-process GDI handle count.
        unsafe { GetGuiResources(GetCurrentProcess(), GR_GDIOBJECTS) }
    }

    const BATCH: u32 = 200;

    fn create_regions() -> Vec<OwnedRgn> {
        (0..BATCH)
            .map(|_| {
                // SAFETY: plain GDI call, the result is checked by from_raw.
                OwnedRgn::from_raw(unsafe { CreateRectRgn(0, 0, 4, 4) }).expect("region")
            })
            .collect()
    }

    #[test]
    fn owned_rgn_deletes_on_drop_and_tolerates_null() {
        assert!(OwnedRgn::from_raw(std::ptr::null_mut()).is_none());
        let before = gdi_objects();
        let regions = create_regions();
        assert!(
            gdi_objects() >= before + BATCH / 2,
            "regions are GDI objects"
        );
        drop(regions);
        assert!(
            gdi_objects() < before + BATCH / 2,
            "drop must delete the regions"
        );
    }

    #[test]
    fn owned_rgn_into_raw_does_not_delete() {
        let before = gdi_objects();
        let raw: Vec<HRGN> = create_regions()
            .into_iter()
            .map(OwnedRgn::into_raw)
            .collect();
        assert!(
            gdi_objects() >= before + BATCH / 2,
            "into_raw must not delete"
        );
        for rgn in raw {
            // SAFETY: the test owns `rgn` again and deletes it once.
            unsafe { DeleteObject(rgn) };
        }
        assert!(gdi_objects() < before + BATCH / 2);
    }

    fn is_menu(menu: HMENU) -> bool {
        // SAFETY: IsMenu only validates the handle value.
        unsafe { IsMenu(menu) != 0 }
    }

    #[test]
    fn owned_menu_destroys_on_drop_and_tolerates_null() {
        drop(OwnedMenu::from_raw(std::ptr::null_mut()));
        let menu = OwnedMenu::create_popup().expect("popup menu");
        let raw = menu.as_raw();
        assert!(is_menu(raw));
        drop(menu);
        assert!(!is_menu(raw), "drop must destroy the menu");
    }

    #[test]
    fn owned_menu_into_raw_after_append_hands_the_submenu_to_the_parent() {
        let parent = OwnedMenu::create_popup().expect("parent");
        let sub = OwnedMenu::create_popup().expect("submenu");
        let sub_raw = sub.as_raw();
        let text = [b'a' as u16, 0];
        // SAFETY: both menus are live; `text` is NUL terminated.
        let appended = unsafe {
            AppendMenuW(
                parent.as_raw(),
                MF_POPUP | MF_STRING,
                sub_raw as usize,
                text.as_ptr(),
            )
        };
        assert_ne!(appended, 0);
        assert_eq!(sub.into_raw(), sub_raw);
        assert!(is_menu(sub_raw), "into_raw must not destroy the submenu");
        drop(parent);
        assert!(
            !is_menu(sub_raw),
            "destroying the parent destroys the appended submenu"
        );
    }

    #[test]
    fn owned_menu_not_appended_submenu_is_destroyed_by_its_guard() {
        let sub = OwnedMenu::create_popup().expect("submenu");
        let sub_raw = sub.as_raw();
        drop(sub);
        assert!(!is_menu(sub_raw));
    }
}
