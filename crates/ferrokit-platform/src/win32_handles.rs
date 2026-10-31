// SPDX-License-Identifier: LGPL-3.0-only
//! Ownership guards for Win32 handles.
//!
//! Every guard owns one handle, tolerates a null handle in `Drop`, and releases it with the one
//! API that matches the allocator. `into_raw` hands the handle to the system (or to an API that
//! must return it raw) and disarms the guard; call it only after the system accepted ownership.

use std::ffi::c_void;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::Foundation::{GlobalFree, HGLOBAL};
use windows_sys::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject,
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ, HRGN,
};
use windows_sys::Win32::System::DataExchange::{CloseClipboard, OpenClipboard};
use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreatePopupMenu, DestroyIcon, DestroyMenu, HICON, HMENU,
};

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

/// The clipboard opened by this thread, closed with `CloseClipboard` on drop, so an early
/// return or a panic between open and close cannot leave the clipboard open for everyone else.
pub(crate) struct ClipboardSession(std::marker::PhantomData<*const ()>);

impl ClipboardSession {
    /// Opens the clipboard, retrying briefly while another process holds it; `None` if it stays
    /// busy.
    pub(crate) fn open_with_retry() -> Option<Self> {
        for _ in 0..3 {
            // SAFETY: a null window associates the clipboard with the current task; no pointers.
            if unsafe { OpenClipboard(std::ptr::null_mut()) } != 0 {
                return Some(Self(std::marker::PhantomData));
            }
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        None
    }
}

impl Drop for ClipboardSession {
    fn drop(&mut self) {
        // SAFETY: a session exists only after a successful OpenClipboard on this thread and is
        // `!Send` (the raw-pointer marker), so it is dropped on that same thread; CloseClipboard
        // is therefore called exactly once per open, by the thread that opened it.
        unsafe { CloseClipboard() };
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

/// A device context from `GetDC`, released with `ReleaseDC` on the same window.
///
/// `hwnd` is null for the screen DC. The handle is never stored beyond one scope: it stops
/// being valid for `UpdateLayeredWindow` after a display change, so every user acquires its own.
/// The held handle is null when `GetDC` failed; check `as_raw` where that matters.
pub(crate) struct WindowDc {
    hwnd: HWND,
    hdc: HDC,
}

impl WindowDc {
    /// The client DC of `hwnd`.
    pub(crate) fn acquire(hwnd: HWND) -> Self {
        // SAFETY: GetDC takes no pointers and validates `hwnd` itself (null selects the screen,
        // per the GetDC documentation); a null result is stored and tolerated by `Drop`.
        Self {
            hwnd,
            // SAFETY: see the comment above `Self {`; `hwnd` is the same value passed to GetDC.
            hdc: unsafe { GetDC(hwnd) },
        }
    }

    /// The screen DC (`GetDC(NULL)`).
    pub(crate) fn screen() -> Self {
        Self::acquire(std::ptr::null_mut())
    }

    /// The raw handle, still owned by the guard; null if `GetDC` failed.
    pub(crate) fn as_raw(&self) -> HDC {
        self.hdc
    }
}

impl Drop for WindowDc {
    fn drop(&mut self) {
        if self.hdc.is_null() {
            return;
        }
        // SAFETY: `self.hdc` came from `GetDC(self.hwnd)` in `acquire` and this guard is its only
        // owner and never releases it elsewhere; ReleaseDC with the same window is the matching
        // call. If the window was destroyed meanwhile ReleaseDC just fails.
        unsafe { ReleaseDC(self.hwnd, self.hdc) };
    }
}

/// A memory DC from `CreateCompatibleDC`, deleted with `DeleteDC`.
pub(crate) struct MemDc(HDC);

impl MemDc {
    /// A memory DC compatible with `hdc` (null: compatible with the screen); `None` on failure.
    pub(crate) fn create_compatible(hdc: HDC) -> Option<Self> {
        // SAFETY: CreateCompatibleDC accepts any DC handle or null and validates it itself.
        let dc = unsafe { CreateCompatibleDC(hdc) };
        (!dc.is_null()).then_some(Self(dc))
    }

    /// The raw handle, still owned by the guard.
    pub(crate) fn as_raw(&self) -> HDC {
        self.0
    }
}

impl Drop for MemDc {
    fn drop(&mut self) {
        if self.0.is_null() {
            return;
        }
        // SAFETY: `self.0` is a DC made by CreateCompatibleDC that this guard solely owns, so
        // DeleteDC is the matching single release. Any bitmap still selected into it is the
        // concern of `DibTarget`, which restores the previous object before this runs.
        unsafe { DeleteDC(self.0) };
    }
}

/// An owned bitmap (`CreateDIBSection`, `CreateBitmap`), deleted with `DeleteObject`.
///
/// A bitmap must not be selected into a DC when it is deleted (`DeleteObject` fails and the
/// bitmap leaks); a bitmap that is selected belongs in a [`DibTarget`].
pub(crate) struct OwnedBitmap(HBITMAP);

impl OwnedBitmap {
    /// Takes ownership of a bitmap the caller just created; `None` when GDI returned null.
    pub(crate) fn from_raw(bitmap: HBITMAP) -> Option<Self> {
        (!bitmap.is_null()).then_some(Self(bitmap))
    }

    /// The raw handle, still owned by the guard.
    pub(crate) fn as_raw(&self) -> HBITMAP {
        self.0
    }
}

impl Drop for OwnedBitmap {
    fn drop(&mut self) {
        if self.0.is_null() {
            return;
        }
        // SAFETY: `self.0` is a bitmap created by GDI that this guard solely owns. Within this
        // module only `DibTarget` selects an `OwnedBitmap` into a DC, and it restores the
        // previous object (or selects a replacement) before the old bitmap drops; callers
        // elsewhere only create bitmaps they never select (cursor and icon builders).
        unsafe { DeleteObject(self.0) };
    }
}

/// Creates a top-down 32-bit DIB section of `w` x `h` (each at least 1) compatible with `dc`.
///
/// The bitmap is wrapped before the pixel pointer is checked, so a bitmap returned without bits
/// is deleted rather than leaked.
pub(crate) fn create_dib_section(dc: HDC, w: u32, h: u32) -> Option<(OwnedBitmap, *mut u8)> {
    dib_section_with(w, h, |bmi, bits| {
        // SAFETY: `bmi` is a fully initialised BITMAPINFO that outlives the call, `bits` is a
        // valid out pointer, and `dc` is a DC (or null) the caller owns.
        unsafe { CreateDIBSection(dc, bmi, DIB_RGB_COLORS, bits, std::ptr::null_mut(), 0) }
    })
}

/// `create_dib_section` with the GDI call supplied by `create`, which receives the bitmap
/// description and the out pointer for the pixels. A bitmap returned without a pixel pointer is
/// owned first, so rejecting it deletes it instead of leaking it.
fn dib_section_with(
    w: u32,
    h: u32,
    create: impl FnOnce(&BITMAPINFO, &mut *mut c_void) -> HBITMAP,
) -> Option<(OwnedBitmap, *mut u8)> {
    // SAFETY: a zeroed BITMAPINFO is a valid all-zero C struct; the header fields are set below.
    let mut bmi: BITMAPINFO = unsafe { std::mem::zeroed() };
    bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
    bmi.bmiHeader.biWidth = w.max(1) as i32;
    bmi.bmiHeader.biHeight = -(h.max(1) as i32); // Top-down
    bmi.bmiHeader.biPlanes = 1;
    bmi.bmiHeader.biBitCount = 32;
    bmi.bmiHeader.biCompression = BI_RGB;

    let mut bits: *mut c_void = std::ptr::null_mut();
    let bitmap = OwnedBitmap::from_raw(create(&bmi, &mut bits))?;
    (!bits.is_null()).then_some((bitmap, bits.cast::<u8>()))
}

/// A memory DC with a DIB section selected into it.
///
/// Encodes the selection: it remembers the object the bitmap replaced, and its `Drop` restores
/// that object first, then deletes the bitmap, then deletes the DC (field order: `bitmap`
/// before `dc`). `bits` points into the DIB section and is valid while the bitmap lives.
pub(crate) struct DibTarget {
    bitmap: OwnedBitmap,
    dc: MemDc,
    previous: HGDIOBJ,
    bits: *mut u8,
}

impl DibTarget {
    /// Selects `bitmap` (whose pixels are at `bits`) into `dc`.
    pub(crate) fn select(dc: MemDc, bitmap: OwnedBitmap, bits: *mut u8) -> Self {
        // SAFETY: both handles are live and owned by the guards being moved into `Self`; the
        // bitmap is not selected into another DC.
        let previous = unsafe { SelectObject(dc.as_raw(), bitmap.as_raw()) };
        Self {
            bitmap,
            dc,
            previous,
            bits,
        }
    }

    /// The memory DC.
    pub(crate) fn dc(&self) -> HDC {
        self.dc.as_raw()
    }

    /// The pixels of the selected DIB section.
    pub(crate) fn bits(&self) -> *mut u8 {
        self.bits
    }

    /// The selected bitmap.
    #[cfg(test)]
    pub(crate) fn bitmap(&self) -> HBITMAP {
        self.bitmap.as_raw()
    }

    /// Replaces the bitmap, selecting the new one (which deselects the old one) before the old
    /// one is deleted.
    pub(crate) fn replace_selecting(&mut self, bitmap: OwnedBitmap, bits: *mut u8) {
        let old = std::mem::replace(&mut self.bitmap, bitmap);
        // SAFETY: the DC and the new bitmap are live and owned by `self`; selecting the new
        // bitmap deselects `old`, which is dropped (deleted) only afterwards.
        unsafe { SelectObject(self.dc.as_raw(), self.bitmap.as_raw()) };
        drop(old);
        self.bits = bits;
    }

    /// Replaces the bitmap: restores the previous object, deletes the old bitmap, then selects
    /// the new one. If there is no previous object to restore (the original selection failed),
    /// the old bitmap may still be selected and cannot be deleted safely first, so this falls
    /// back to [`Self::replace_selecting`].
    pub(crate) fn replace_deselecting(&mut self, bitmap: OwnedBitmap, bits: *mut u8) {
        if self.previous.is_null() {
            self.replace_selecting(bitmap, bits);
            return;
        }
        self.restore_previous();
        let old = std::mem::replace(&mut self.bitmap, bitmap);
        drop(old);
        // SAFETY: the DC and the new bitmap are live and owned by `self`; the old bitmap was
        // deselected above and deleted just now.
        self.previous = unsafe { SelectObject(self.dc.as_raw(), self.bitmap.as_raw()) };
        self.bits = bits;
    }

    fn restore_previous(&self) {
        if self.previous.is_null() {
            return;
        }
        // SAFETY: `previous` is what SelectObject returned for this DC when the bitmap was
        // selected, so it is a GDI object valid for this DC; both handles are owned by `self`.
        unsafe { SelectObject(self.dc.as_raw(), self.previous) };
    }
}

impl Drop for DibTarget {
    fn drop(&mut self) {
        // The remaining fields drop after this: `bitmap` is deleted, then `dc`.
        self.restore_previous();
    }
}

/// An icon handle and whether this code owns it.
///
/// `LoadIcon` hands out shared handles that belong to the system and must not be destroyed; an
/// icon made by `CreateIconIndirect` or `SHDefExtractIcon` is ours to free with `DestroyIcon`. An
/// owned handle is destroyed on drop; a shared (borrowed) one never is.
pub(crate) struct IconHandle {
    hicon: HICON,
    owned: bool,
}

impl IconHandle {
    /// An icon this code created and must destroy.
    pub(crate) fn owned(hicon: HICON) -> Self {
        Self { hicon, owned: true }
    }

    /// A handle someone else owns (a system icon, or one the caller keeps); never destroyed here.
    pub(crate) fn shared(hicon: HICON) -> Self {
        Self {
            hicon,
            owned: false,
        }
    }

    /// The raw handle, still owned by the guard if `is_owned`.
    pub(crate) fn as_raw(&self) -> HICON {
        self.hicon
    }

    /// Whether dropping this destroys the icon.
    #[cfg(test)]
    pub(crate) fn is_owned(&self) -> bool {
        self.owned
    }

    /// Stores `new` in place of the current icon. Returns the previous handle, which the caller
    /// drops (destroying it if owned) once nothing shows it any more.
    ///
    /// When `new` is the same handle as the current one nothing is replaced and nothing is
    /// returned: destroying it would destroy the icon `new` refers to, and downgrading an owned
    /// handle to shared would leak it, so the handle stays owned if either side owned it.
    #[must_use = "the replaced icon is destroyed when the returned handle drops"]
    pub(crate) fn replace(&mut self, new: IconHandle) -> Option<IconHandle> {
        if self.hicon == new.hicon {
            self.owned |= new.owned;
            // The ownership moved into `self`; `new` must not destroy the shared handle.
            std::mem::forget(new);
            return None;
        }
        Some(std::mem::replace(self, new))
    }
}

impl Drop for IconHandle {
    fn drop(&mut self) {
        if !self.owned || self.hicon.is_null() {
            return;
        }
        // SAFETY: an owned handle is an icon this guard created and solely owns (`owned` is only
        // set for handles from CreateIconIndirect/SHDefExtractIcon, and `replace` never keeps two
        // owners of one handle). DestroyIcon validates the handle and merely fails otherwise.
        unsafe { DestroyIcon(self.hicon) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::Graphics::Gdi::CreateRectRgn;
    use windows_sys::Win32::Graphics::Gdi::{
        CreateCompatibleBitmap, GetCurrentObject, GetObjectW, BITMAP, OBJ_BITMAP,
    };
    use windows_sys::Win32::System::Memory::GlobalSize;
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetGuiResources, GR_GDIOBJECTS,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{AppendMenuW, IsMenu, MF_POPUP, MF_STRING};
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetIconInfo, ICONINFO};

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
                // SAFETY: CreateRectRgn takes only integers; a null result is rejected by `from_raw`.
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
        // SAFETY: IsMenu takes a handle value only and returns 0 for one that is not a menu.
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

    /// A leaked screen DC would exhaust the per-process GDI quota (10 000 handles by default)
    /// and `GetDC` would start returning NULL.
    #[test]
    fn window_dc_is_released_on_drop() {
        for i in 0..12_000 {
            let dc = WindowDc::screen();
            assert!(
                !dc.as_raw().is_null(),
                "GetDC(NULL) failed at iteration {i}: DCs leaked"
            );
        }
    }

    fn new_target(w: u32, h: u32) -> DibTarget {
        let screen = WindowDc::screen();
        let dc = MemDc::create_compatible(screen.as_raw()).expect("memory DC");
        let (bitmap, bits) = create_dib_section(dc.as_raw(), w, h).expect("DIB section");
        DibTarget::select(dc, bitmap, bits)
    }

    fn selected_bitmap(dc: HDC) -> HGDIOBJ {
        // SAFETY: `dc` is a live DC in the callers (a `DibTarget`'s memory DC); GetCurrentObject only reads its selection.
        unsafe { GetCurrentObject(dc, OBJ_BITMAP as u32) }
    }

    /// `GetObjectW` fails (0) for a handle that is not a live bitmap.
    fn bitmap_is_live(bitmap: HBITMAP) -> bool {
        // SAFETY: BITMAP is a plain C struct of integers and a pointer, for which all-zero is valid.
        let mut info: BITMAP = unsafe { std::mem::zeroed() };
        // SAFETY: GetObjectW writes at most `size_of::<BITMAP>()` bytes into `info`.
        unsafe {
            GetObjectW(
                bitmap,
                std::mem::size_of::<BITMAP>() as i32,
                (&mut info as *mut BITMAP).cast(),
            ) != 0
        }
    }

    /// A bitmap created without a pixel pointer is rejected and deleted, not leaked.
    #[test]
    fn dib_without_bits_is_rejected_and_the_bitmap_deleted() {
        let screen = WindowDc::screen();
        let mut raw: HBITMAP = std::ptr::null_mut();
        // GDI returns a bitmap but leaves the pixel pointer null.
        let created = dib_section_with(4, 4, |bmi, bits| {
            assert!(bits.is_null());
            assert_eq!(bmi.bmiHeader.biBitCount, 32);
            // SAFETY: `screen` is a live screen DC owned above.
            raw = unsafe { CreateCompatibleBitmap(screen.as_raw(), 4, 4) };
            assert!(bitmap_is_live(raw));
            raw
        });
        assert!(created.is_none());
        assert!(!raw.is_null());
        assert!(!bitmap_is_live(raw), "the rejected bitmap must be deleted");
    }

    #[test]
    fn dib_target_selects_the_bitmap_and_drop_releases_it() {
        let target = new_target(8, 4);
        let bitmap = target.bitmap();
        assert_eq!(selected_bitmap(target.dc()), bitmap as HGDIOBJ);
        assert!(bitmap_is_live(bitmap));
        // The pixels are writable up to w * h * 4 bytes.
        // SAFETY: the DIB section is 8 * 4 * 4 bytes.
        unsafe { *target.bits().add(8 * 4 * 4 - 1) = 0xAB };
        drop(target);
        assert!(!bitmap_is_live(bitmap), "drop must delete the bitmap");
    }

    /// `DeleteObject` fails for a bitmap that is still selected into a DC and the bitmap then
    /// leaks: the object count staying flat proves the previous object is restored before the
    /// bitmap is deleted.
    #[test]
    fn dib_target_drop_restores_the_previous_object_before_deleting() {
        let before = gdi_objects();
        let targets: Vec<DibTarget> = (0..BATCH).map(|_| new_target(4, 4)).collect();
        assert!(gdi_objects() >= before + BATCH, "targets are GDI objects");
        drop(targets);
        assert!(
            gdi_objects() < before + BATCH / 2,
            "dropping must delete the bitmaps and the DCs"
        );
    }

    #[test]
    fn dib_target_replace_selecting_deletes_the_old_bitmap() {
        let mut target = new_target(4, 4);
        let old = target.bitmap();
        let (bitmap, bits) = create_dib_section(target.dc(), 16, 16).expect("DIB section");
        let new = bitmap.as_raw();
        target.replace_selecting(bitmap, bits);
        assert_eq!(target.bitmap(), new);
        assert_eq!(selected_bitmap(target.dc()), new as HGDIOBJ);
        assert_eq!(target.bits(), bits);
        assert!(!bitmap_is_live(old), "the old bitmap is deleted");
        assert!(bitmap_is_live(new));
    }

    #[test]
    fn dib_target_replace_deselecting_deletes_the_old_bitmap() {
        let before = gdi_objects();
        let mut target = new_target(4, 4);
        let old = target.bitmap();
        let (bitmap, bits) = create_dib_section(target.dc(), 16, 16).expect("DIB section");
        let new = bitmap.as_raw();
        target.replace_deselecting(bitmap, bits);
        assert_eq!(target.bitmap(), new);
        assert_eq!(selected_bitmap(target.dc()), new as HGDIOBJ);
        assert!(!bitmap_is_live(old), "the old bitmap is deleted");
        drop(target);
        assert!(!bitmap_is_live(new));
        assert!(gdi_objects() <= before + 1);
    }

    fn new_icon() -> HICON {
        let pixmap = ferrokit_gui::paint::Pixmap::new(16, 16).expect("pixmap");
        crate::tray_icon::TrayIcon::create_hicon_from_pixmap(&pixmap).expect("icon")
    }

    /// `GetIconInfo` fails for a destroyed icon.
    fn icon_is_live(icon: HICON) -> bool {
        // SAFETY: GetIconInfo writes into `info`; the bitmaps it returns are ours to delete.
        unsafe {
            let mut info: ICONINFO = std::mem::zeroed();
            let ok = GetIconInfo(icon, &mut info) != 0;
            if ok {
                DeleteObject(info.hbmColor);
                DeleteObject(info.hbmMask);
            }
            ok
        }
    }

    #[test]
    fn icon_handle_destroys_an_owned_icon_and_leaves_a_shared_one() {
        drop(IconHandle::owned(std::ptr::null_mut()));
        let owned = new_icon();
        let shared = new_icon();
        drop(IconHandle::owned(owned));
        drop(IconHandle::shared(shared));
        assert!(!icon_is_live(owned), "an owned icon is destroyed on drop");
        assert!(icon_is_live(shared), "a shared icon is not destroyed");
        // SAFETY: the test still owns `shared` and destroys it once.
        unsafe { DestroyIcon(shared) };
    }

    #[test]
    fn icon_handle_replace_with_the_same_handle_keeps_ownership() {
        let icon = new_icon();
        let mut slot = IconHandle::owned(icon);
        assert!(slot.replace(IconHandle::shared(icon)).is_none());
        assert!(slot.is_owned(), "still owned after replacing with itself");
        assert!(icon_is_live(icon));
        drop(slot);
        assert!(!icon_is_live(icon), "destroyed with its owner");
    }

    #[test]
    fn icon_handle_replace_returns_the_previous_handle() {
        let (first, second) = (new_icon(), new_icon());
        let mut slot = IconHandle::owned(first);
        let old = slot.replace(IconHandle::owned(second)).expect("replaced");
        assert_eq!(old.as_raw(), first);
        assert!(
            icon_is_live(first),
            "the caller decides when it is destroyed"
        );
        drop(old);
        assert!(!icon_is_live(first));
        assert!(icon_is_live(second));
        drop(slot);
        assert!(!icon_is_live(second));
    }

    /// Whether this thread has the clipboard open: reading a format fails with
    /// `ERROR_CLIPBOARD_NOT_OPEN` (1418) when it is not.
    fn clipboard_is_open_here() -> bool {
        use windows_sys::Win32::Foundation::{GetLastError, SetLastError};
        use windows_sys::Win32::System::DataExchange::GetClipboardData;
        // SAFETY: only reads the clipboard state and the thread's last error.
        unsafe {
            SetLastError(0);
            GetClipboardData(13); // CF_UNICODETEXT
            GetLastError() != 1418
        }
    }

    #[test]
    fn clipboard_session_closes_on_drop_and_on_panic() {
        assert!(!clipboard_is_open_here());
        let session = ClipboardSession::open_with_retry().expect("open");
        assert!(clipboard_is_open_here(), "open while the session lives");
        drop(session);
        assert!(!clipboard_is_open_here(), "drop must close the clipboard");

        let unwound = std::panic::catch_unwind(|| {
            let _session = ClipboardSession::open_with_retry().expect("open");
            panic!("between open and close");
        });
        assert!(unwound.is_err());
        assert!(!clipboard_is_open_here(), "a panic must still close it");
    }
}
