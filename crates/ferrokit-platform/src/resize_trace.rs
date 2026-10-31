// SPDX-License-Identifier: LGPL-3.0-only
//! Debug-only timestamp trace for the interactive-resize pipeline.
//!
//! Disabled by default; recording is a single relaxed atomic load when off, and compiled out
//! entirely in release builds. Enable with [`set_enabled`], read with [`take`]. Entries are
//! per-thread (all window work runs on the UI thread).
//!
//! One resize iteration N should read, in order:
//! `WmSize(N) -> RenderStart(N) -> RenderEnd(N) -> SurfaceResize(N) -> Present1(N) -> DCompCommit(N)`
//! (`SurfaceResize` happens inside the present, hence after `RenderEnd`). The remaining gap,
//! `DCompCommit -> DWM visible`, is outside the process and cannot be timestamped here.
//!
//! Layered-surface path (the production fallback when DComp is unavailable), per iteration:
//! `WmSize -> RenderStart -> RenderEnd -> [LayeredResizeStart -> LayeredResizeEnd] -> PixelCopyStart
//! -> PixelCopyEnd -> UpdateLayeredWindowStart -> UpdateLayeredWindowEnd`. The bracketed pair is
//! only present when the visible size changed.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceKind {
    WmSize,
    RenderStart,
    RenderEnd,
    SurfaceResize,
    Present1,
    DCompCommit,
    LayeredResizeStart,
    LayeredResizeEnd,
    PixelCopyStart,
    PixelCopyEnd,
    UpdateLayeredWindowStart,
    UpdateLayeredWindowEnd,
    /// ferrokit' own (logical) geometry for the frame being rendered. `pos` = ferrokit x/y.
    RequestedGeometry,
    /// `GetWindowRect(hwnd)` just before the render phase. `pos`/physical = live HWND rect.
    WindowRectBeforeRender,
    /// `GetWindowRect(hwnd)` inside the layered present, the value used for `pptDst`.
    WindowRectBeforeUlw,
    /// Arguments handed to `UpdateLayeredWindowIndirect`: `pos` = `pptDst`, physical = `psize`.
    UlwArgs,
    /// `GetWindowRect(hwnd)` right after `UpdateLayeredWindowIndirect` returned.
    WindowRectAfterUlw,
}

#[derive(Debug, Clone, Copy)]
pub struct TraceEntry {
    pub kind: TraceKind,
    pub hwnd: usize,
    /// Logical size; `(0, 0)` where the recording layer only knows physical pixels.
    pub logical_width: u32,
    pub logical_height: u32,
    pub physical_width: u32,
    pub physical_height: u32,
    /// Screen position where the recorded kind carries one (otherwise `(0, 0)`).
    pub x: i32,
    pub y: i32,
    pub at: Instant,
}

const MAX_ENTRIES: usize = 4096;

static ENABLED: AtomicBool = AtomicBool::new(false);

thread_local! {
    static ENTRIES: RefCell<Vec<TraceEntry>> = const { RefCell::new(Vec::new()) };
}

pub fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
}

pub fn clear() {
    let _ = ENTRIES.try_with(|e| e.borrow_mut().clear());
}

/// Drains this thread's entries.
pub fn take() -> Vec<TraceEntry> {
    ENTRIES
        .try_with(|e| std::mem::take(&mut *e.borrow_mut()))
        .unwrap_or_default()
}

#[inline]
pub fn record(kind: TraceKind, hwnd: usize, logical: (u32, u32), physical: (u32, u32)) {
    record_at(kind, hwnd, (0, 0), logical, physical);
}

/// Like [`record`], with a screen position.
#[inline]
pub fn record_at(
    kind: TraceKind,
    hwnd: usize,
    pos: (i32, i32),
    logical: (u32, u32),
    physical: (u32, u32),
) {
    #[cfg(debug_assertions)]
    {
        if !ENABLED.load(Ordering::Relaxed) {
            return;
        }
        let _ = ENTRIES.try_with(|e| {
            let mut e = e.borrow_mut();
            if e.len() >= MAX_ENTRIES {
                e.remove(0);
            }
            e.push(TraceEntry {
                kind,
                hwnd,
                logical_width: logical.0,
                logical_height: logical.1,
                physical_width: physical.0,
                physical_height: physical.1,
                x: pos.0,
                y: pos.1,
                at: Instant::now(),
            });
        });
    }
    #[cfg(not(debug_assertions))]
    {
        let _ = (kind, hwnd, pos, logical, physical);
    }
}

/// Records the live `GetWindowRect(hwnd)` (debug builds, only while tracing is enabled).
#[inline]
pub fn record_window_rect(kind: TraceKind, hwnd: usize) {
    #[cfg(all(windows, debug_assertions))]
    {
        if !ENABLED.load(Ordering::Relaxed) {
            return;
        }
        // SAFETY: RECT is a plain integer struct, so the all-zero value is valid.
        let mut r: windows_sys::Win32::Foundation::RECT = unsafe { std::mem::zeroed() };
        // SAFETY: `r` is a local out pointer valid for the call; GetWindowRect fails harmlessly
        // for a stale `hwnd`, which `ok == 0` then skips.
        let ok = unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd as _, &mut r)
        };
        if ok != 0 {
            record_at(
                kind,
                hwnd,
                (r.left, r.top),
                (0, 0),
                ((r.right - r.left) as u32, (r.bottom - r.top) as u32),
            );
        }
    }
    #[cfg(not(all(windows, debug_assertions)))]
    {
        let _ = (kind, hwnd);
    }
}
