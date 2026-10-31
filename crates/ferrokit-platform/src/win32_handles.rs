// SPDX-License-Identifier: LGPL-3.0-only
//! Ownership guards for Win32 handles.
//!
//! Every guard owns one handle, tolerates a null handle in `Drop`, and releases it with the one
//! API that matches the allocator. `into_raw` hands the handle to the system (or to an API that
//! must return it raw) and disarms the guard; call it only after the system accepted ownership.

use std::ffi::c_void;
use windows_sys::Win32::Foundation::{GlobalFree, HGLOBAL};
use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};

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

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::Memory::GlobalSize;

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
}
