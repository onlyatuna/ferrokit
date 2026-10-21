// SPDX-License-Identifier: LGPL-3.0-only
use crate::resize_trace::{record as trace_record, TraceKind};

#[inline]
fn trace_record_at(kind: TraceKind, hwnd: usize, pos: (i32, i32), physical: (u32, u32)) {
    crate::resize_trace::record_at(kind, hwnd, pos, (0, 0), physical);
}
use crate::surface::PlatformSurface;
use ferrokit_gui::geometry::Rect;
use ferrokit_gui::paint::Pixmap;
use std::ptr;
use windows_sys::Win32::Foundation::{HWND, POINT, RECT, SIZE};
use windows_sys::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject,
    AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS,
    HBITMAP, HDC, HGDIOBJ,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetWindowLongPtrW, GetWindowRect, SetWindowLongPtrW, UpdateLayeredWindow,
    UpdateLayeredWindowIndirect, GWL_EXSTYLE, ULW_ALPHA, UPDATELAYEREDWINDOWINFO, WS_EX_LAYERED,
};

/// Largest capacity (in pixels) the interactive-resize headroom may allocate (128 MiB of BGRA).
/// Beyond this the DIB is sized exactly instead.
const MAX_CAPACITY_PIXELS: u64 = 32 * 1024 * 1024;

/// DIB extent used for a visible extent `n` while persistent capacity is active: 1.5x headroom
/// rounded up to a multiple of 64.
fn capacity_for(n: u32) -> u32 {
    (n.saturating_mul(3) / 2)
        .div_ceil(64)
        .saturating_mul(64)
        .max(n)
}

/// `(w, h)` allocation for a visible size, falling back to exact when headroom is too large.
fn capacity_size(w: u32, h: u32) -> (u32, u32) {
    let (cw, ch) = (capacity_for(w), capacity_for(h));
    if u64::from(cw) * u64::from(ch) > MAX_CAPACITY_PIXELS {
        (w, h)
    } else {
        (cw, ch)
    }
}

#[cfg(debug_assertions)]
type Tick = std::time::Instant;
#[cfg(not(debug_assertions))]
#[derive(Clone, Copy)]
struct Tick;

#[inline]
fn tick() -> Tick {
    #[cfg(debug_assertions)]
    {
        std::time::Instant::now()
    }
    #[cfg(not(debug_assertions))]
    {
        Tick
    }
}

/// Nanoseconds since `t`; always 0 in release builds (timing is compiled out).
#[inline]
fn ns_since(t: Tick) -> u64 {
    #[cfg(debug_assertions)]
    {
        t.elapsed().as_nanos() as u64
    }
    #[cfg(not(debug_assertions))]
    {
        let _ = t;
        0
    }
}

/// Counters for A/B measurement and tests. The `*_ns` fields are only accumulated in debug
/// builds; counts are always maintained.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LayeredStats {
    /// Visible-size changes applied by `resize`.
    pub visible_resize_count: u64,
    /// `CreateDIBSection` calls made after construction.
    pub dib_realloc_count: u64,
    /// Presents that reached `UpdateLayeredWindow*`.
    pub present_count: u64,
    /// Times `UpdateLayeredWindowIndirect` failed and the plain `UpdateLayeredWindow` was used.
    pub ulw_fallback_count: u64,
    /// Pixels copied (RGBA -> premultiplied BGRA) into the DIB.
    pub copied_pixels: u64,
    /// Area of the last rectangle copied/passed as `prcDirty`.
    pub last_copied_width: u32,
    pub last_copied_height: u32,
    /// Debug builds only: time in `resize` (incl. `CreateDIBSection`).
    pub resize_ns: u64,
    /// Debug builds only: time in the CPU pixel copy.
    pub pixel_copy_ns: u64,
    /// Debug builds only: time in `UpdateLayeredWindowIndirect` (and its fallback).
    pub ulw_ns: u64,
    /// Presents where the live `GetWindowRect` size differed from `psize` (the visible size the
    /// content was rendered for), i.e. the HWND and the layered content disagreed.
    pub ulw_size_mismatch_count: u64,
    /// Presents where `UpdateLayeredWindowIndirect` itself changed the HWND rect (debug builds
    /// only: needs a `GetWindowRect` after the call).
    pub ulw_rect_changed_by_ulw_count: u64,
}

pub struct Win32LayeredSurface {
    hwnd: HWND,
    /// Visible size: what is presented (`psize`) and the size of the source pixmap.
    width: u32,
    height: u32,
    /// DIB allocation. `>=` the visible size; rows are `alloc_width * 4` bytes apart.
    alloc_width: u32,
    alloc_height: u32,
    screen_dc: HDC,
    mem_dc: HDC,
    hbitmap: HBITMAP,
    old_hbitmap: HGDIOBJ,
    bits: *mut u8,
    /// Native interactive sizing loop active (see [`Self::set_interactive_resize`]).
    interactive: bool,
    /// Policy switch for A/B comparison: when `false`, the DIB is always sized exactly.
    persistent_capacity: bool,
    /// The DIB holds stale/uninitialised pixels in the visible area; next present copies it all.
    force_full: bool,
    target_pos: Option<POINT>,
    stats: LayeredStats,
}

// Deliberately neither `Send` nor `Sync`: owns GDI device contexts and a DIB-section pointer
// that are used with the window's `HWND` on the window's thread only.

impl Win32LayeredSurface {
    /// Creates layered surface for given window and dimensions.
    pub fn new(hwnd: HWND, width: u32, height: u32) -> Result<Self, &'static str> {
        if width == 0 || height == 0 {
            return Err("Width and height must be greater than 0");
        }

        unsafe {
            // Ensure window has WS_EX_LAYERED extended style for UpdateLayeredWindow
            let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            if (ex_style as u32 & WS_EX_LAYERED) == 0 {
                SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex_style | WS_EX_LAYERED as isize);
            }
            let screen_dc = GetDC(ptr::null_mut());
            if screen_dc.is_null() {
                return Err("Failed to get screen DC (GetDC)");
            }

            let mem_dc = CreateCompatibleDC(screen_dc);
            if mem_dc.is_null() {
                ReleaseDC(ptr::null_mut(), screen_dc);
                return Err("Failed to create compatible DC (CreateCompatibleDC)");
            }

            let Some((hbitmap, bits)) = create_dib(mem_dc, width, height) else {
                ReleaseDC(ptr::null_mut(), screen_dc);
                DeleteDC(mem_dc);
                return Err("CreateDIBSection failed to create layered bitmap");
            };

            let old_hbitmap = SelectObject(mem_dc, hbitmap);

            Ok(Self {
                hwnd,
                width,
                height,
                alloc_width: width,
                alloc_height: height,
                screen_dc,
                mem_dc,
                hbitmap,
                old_hbitmap,
                bits,
                interactive: false,
                persistent_capacity: true,
                force_full: true,
                target_pos: None,
                stats: LayeredStats::default(),
            })
        }
    }

    /// Visible width.
    #[inline]
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Visible height.
    #[inline]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// DIB allocation width (`>= width()`).
    #[inline]
    pub fn allocated_width(&self) -> u32 {
        self.alloc_width
    }

    /// DIB allocation height (`>= height()`).
    #[inline]
    pub fn allocated_height(&self) -> u32 {
        self.alloc_height
    }

    /// Destination position for next UpdateLayeredWindow (atomic move with present).
    #[inline]
    pub fn set_target_pos(&mut self, pos: Option<POINT>) {
        self.target_pos = pos;
    }

    #[inline]
    pub fn stats(&self) -> LayeredStats {
        self.stats
    }

    pub fn reset_stats(&mut self) {
        self.stats = LayeredStats::default();
    }

    #[inline]
    pub fn hdc(&self) -> HDC {
        self.mem_dc
    }

    #[inline]
    pub fn bits(&self) -> *mut u8 {
        self.bits
    }

    /// Whole DIB allocation (`allocated_width * allocated_height * 4` bytes). Equals the visible
    /// area exactly unless persistent capacity is in effect; rows are `allocated_width * 4` apart.
    #[inline]
    pub fn buffer(&self) -> &[u8] {
        let len = (self.alloc_width as usize) * (self.alloc_height as usize) * 4;
        unsafe { std::slice::from_raw_parts(self.bits, len) }
    }

    #[inline]
    pub fn buffer_mut(&mut self) -> &mut [u8] {
        let len = (self.alloc_width as usize) * (self.alloc_height as usize) * 4;
        unsafe { std::slice::from_raw_parts_mut(self.bits, len) }
    }

    /// Enables/disables the persistent-capacity policy (default: enabled). Disabled, the DIB is
    /// always exactly the visible size (the pre-3A-7 behaviour, kept for A/B measurement).
    pub fn set_persistent_capacity(&mut self, enabled: bool) {
        self.persistent_capacity = enabled;
    }

    /// Marks the native interactive sizing loop (`WM_ENTERSIZEMOVE`..`WM_EXITSIZEMOVE`).
    ///
    /// While active (and persistent capacity is enabled), `resize` keeps the existing DIB whenever
    /// the new visible size fits in it, and entering allocates the headroom once up front. Outside
    /// the loop the DIB is sized exactly, as before; leaving the loop does not itself reallocate —
    /// the next differing `resize` does.
    pub fn set_interactive_resize(&mut self, active: bool) {
        if self.interactive == active {
            return;
        }
        self.interactive = active;
        if active && self.persistent_capacity {
            let (cw, ch) = capacity_size(self.width, self.height);
            if cw > self.alloc_width || ch > self.alloc_height {
                // Best effort: on failure the surface keeps working with its current DIB.
                let _ = self.realloc_dib(cw.max(self.alloc_width), ch.max(self.alloc_height));
            }
        }
    }

    #[inline]
    pub fn is_interactive_resize(&self) -> bool {
        self.interactive
    }

    /// Replaces the DIB with a `w`x`h` allocation. The new bitmap is uninitialised, so the next
    /// present repaints the whole visible area.
    fn realloc_dib(&mut self, w: u32, h: u32) -> Result<(), &'static str> {
        let Some((new_hbitmap, new_bits)) = (unsafe { create_dib(self.mem_dc, w, h) }) else {
            return Err("CreateDIBSection failed during resize");
        };
        unsafe {
            SelectObject(self.mem_dc, new_hbitmap);
            DeleteObject(self.hbitmap);
        }
        self.alloc_width = w;
        self.alloc_height = h;
        self.hbitmap = new_hbitmap;
        self.bits = new_bits;
        self.force_full = true;
        self.stats.dib_realloc_count += 1;
        Ok(())
    }

    /// Sets the visible size. Reallocates the DIB only when the policy requires it:
    /// - interactive + persistent capacity: only if the size exceeds the allocation (growing with
    ///   headroom);
    /// - otherwise: whenever the allocation differs from the requested size.
    pub fn resize(&mut self, width: u32, height: u32) -> Result<(), &'static str> {
        if width == 0 || height == 0 {
            return Err("Width and height must be greater than 0");
        }
        if self.width == width && self.height == height {
            return Ok(());
        }

        let t = tick();
        trace_record(
            TraceKind::LayeredResizeStart,
            self.hwnd as usize,
            (0, 0),
            (width, height),
        );

        let result = if self.interactive && self.persistent_capacity {
            if width <= self.alloc_width && height <= self.alloc_height {
                Ok(())
            } else {
                let (cw, ch) = capacity_size(width, height);
                self.realloc_dib(cw.max(self.alloc_width), ch.max(self.alloc_height))
            }
        } else if width != self.alloc_width || height != self.alloc_height {
            self.realloc_dib(width, height)
        } else {
            Ok(())
        };

        if result.is_ok() {
            self.width = width;
            self.height = height;
            self.force_full = true;
            self.stats.visible_resize_count += 1;
        }
        self.stats.resize_ns += ns_since(t);
        trace_record(
            TraceKind::LayeredResizeEnd,
            self.hwnd as usize,
            (0, 0),
            (width, height),
        );
        result
    }

    pub fn present_ref(&mut self, pixmap: &Pixmap, opacity: f32) -> Result<(), &'static str> {
        let full = Rect::new(0, 0, self.width as i32, self.height as i32);
        self.present_dirty_ref(pixmap, opacity, full)
    }

    pub fn present(&mut self, pixmap: &mut Pixmap, opacity: f32) -> Result<(), &'static str> {
        self.present_ref(pixmap, opacity)
    }

    pub fn present_dirty_ref(
        &mut self,
        pixmap: &Pixmap,
        opacity: f32,
        dirty: Rect,
    ) -> Result<(), &'static str> {
        let p_width = pixmap.physical_width();
        let p_height = pixmap.physical_height();

        let mut resized = false;
        if p_width != self.width || p_height != self.height {
            self.resize(p_width, p_height)?;
            resized = true;
        }

        let dbg_force_full = self.force_full;
        let full_window_rect = Rect::new(0, 0, self.width as i32, self.height as i32);
        // After a (re)allocation or visible-size change the DIB's visible area is stale, so the
        // whole of it is repainted regardless of what the caller marked dirty.
        let clipped_dirty = if self.force_full {
            full_window_rect
        } else {
            full_window_rect.intersected(&dirty)
        };

        if clipped_dirty.is_empty() {
            return Ok(());
        }

        let src_stride = (p_width as usize) * 4;
        let dst_stride = (self.alloc_width as usize) * 4;
        let dirty_x = clipped_dirty.x as usize;
        let dirty_w = clipped_dirty.width as usize;
        let hwnd_id = self.hwnd as usize;
        let phys = (p_width, p_height);

        trace_record(
            TraceKind::PixelCopyStart,
            hwnd_id,
            (0, 0),
            (clipped_dirty.width as u32, clipped_dirty.height as u32),
        );
        let t_copy = tick();
        let dbg_copy = crate::resize_debug::start();
        unsafe {
            let src_data = pixmap.data();
            for y in clipped_dirty.y..(clipped_dirty.y + clipped_dirty.height) {
                let y = y as usize;
                let copy_bytes = dirty_w * 4;
                let src_row = std::slice::from_raw_parts(
                    src_data.as_ptr().add(y * src_stride + dirty_x * 4),
                    copy_bytes,
                );
                let dst_row = std::slice::from_raw_parts_mut(
                    self.bits.add(y * dst_stride + dirty_x * 4),
                    copy_bytes,
                );

                let (src_chunks, _) = src_row.as_chunks::<4>();
                let (dst_chunks, _) = dst_row.as_chunks_mut::<4>();
                for (src_chunk, dst_chunk) in src_chunks.iter().zip(dst_chunks.iter_mut()) {
                    dst_chunk[0] = src_chunk[2]; // B
                    dst_chunk[1] = src_chunk[1]; // G
                    dst_chunk[2] = src_chunk[0]; // R
                    dst_chunk[3] = src_chunk[3]; // A
                }
            }
        }
        crate::resize_debug::end(crate::resize_debug::Phase::PixelCopy, dbg_copy);
        self.stats.pixel_copy_ns += ns_since(t_copy);
        self.stats.copied_pixels += (clipped_dirty.width as u64) * (clipped_dirty.height as u64);
        self.stats.last_copied_width = clipped_dirty.width as u32;
        self.stats.last_copied_height = clipped_dirty.height as u32;
        trace_record(TraceKind::PixelCopyEnd, hwnd_id, (0, 0), phys);

        unsafe {
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: (opacity.clamp(0.0, 1.0) * 255.0).round() as u8,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };

            let mut pt_dst = POINT { x: 0, y: 0 };
            let mut win_rect: RECT = std::mem::zeroed();
            GetWindowRect(self.hwnd, &mut win_rect);
            if let Some(target) = self.target_pos.take() {
                pt_dst = target;
            } else {
                pt_dst.x = win_rect.left;
                pt_dst.y = win_rect.top;
            }
            trace_record_at(
                TraceKind::WindowRectBeforeUlw,
                hwnd_id,
                (win_rect.left, win_rect.top),
                (
                    (win_rect.right - win_rect.left) as u32,
                    (win_rect.bottom - win_rect.top) as u32,
                ),
            );
            if (win_rect.right - win_rect.left) as u32 != self.width
                || (win_rect.bottom - win_rect.top) as u32 != self.height
            {
                self.stats.ulw_size_mismatch_count += 1;
            }

            // `psize` is the visible size; the (possibly larger) DIB is read from (0,0).
            let size = SIZE {
                cx: self.width as i32,
                cy: self.height as i32,
            };
            let pt_src = POINT { x: 0, y: 0 };
            trace_record_at(
                TraceKind::UlwArgs,
                hwnd_id,
                (pt_dst.x, pt_dst.y),
                (self.width, self.height),
            );

            let mut dirty_win_rect = RECT {
                left: clipped_dirty.x,
                top: clipped_dirty.y,
                right: clipped_dirty.x + clipped_dirty.width,
                bottom: clipped_dirty.y + clipped_dirty.height,
            };

            let mut info: UPDATELAYEREDWINDOWINFO = std::mem::zeroed();
            info.cbSize = std::mem::size_of::<UPDATELAYEREDWINDOWINFO>() as u32;
            info.hdcDst = self.screen_dc;
            info.pptDst = &pt_dst;
            info.psize = &size;
            info.hdcSrc = self.mem_dc;
            info.pptSrc = &pt_src;
            info.crKey = 0;
            info.pblend = &blend;
            // Plain `ULW_ALPHA`, as Qt's QWindowsBackingStore::flush does. `ULW_EX_NORESIZE` made the
            // call fail (GetLastError 31) whenever `psize` was 1 px off the HWND (DPI rounding) and
            // measured no faster.
            let dw_flags = ULW_ALPHA;
            info.dwFlags = dw_flags;
            info.prcDirty = &mut dirty_win_rect;

            trace_record(TraceKind::UpdateLayeredWindowStart, hwnd_id, (0, 0), phys);
            let t_ulw = tick();
            let dbg_t0 = std::time::Instant::now();
            let mut dbg_indirect_err: Option<u32> = None;
            let mut dbg_fallback_ok: Option<bool> = None;
            let res = UpdateLayeredWindowIndirect(self.hwnd, &info);
            let mut outcome = Ok(());
            if res == 0 {
                let err = windows_sys::Win32::Foundation::GetLastError();
                dbg_indirect_err = Some(err);
                let dbg_fb = crate::resize_debug::start();
                eprintln!("[Win32Surface] UpdateLayeredWindowIndirect failed (GetLastError = {}), falling back to UpdateLayeredWindow", err);
                self.stats.ulw_fallback_count += 1;
                let fallback_res = UpdateLayeredWindow(
                    self.hwnd,
                    ptr::null_mut(),
                    &pt_dst,
                    &size,
                    self.mem_dc,
                    &pt_src,
                    0,
                    &blend,
                    ULW_ALPHA,
                );
                crate::resize_debug::end(crate::resize_debug::Phase::UlwFallback, dbg_fb);
                dbg_fallback_ok = Some(fallback_res != 0);
                if fallback_res == 0 {
                    let err2 = windows_sys::Win32::Foundation::GetLastError();
                    eprintln!(
                        "[Win32Surface] Fallback UpdateLayeredWindow failed, GetLastError = {}",
                        err2
                    );
                    outcome = Err(
                        "UpdateLayeredWindowIndirect and fallback UpdateLayeredWindow both failed",
                    );
                }
            }
            if crate::resize_debug::enabled() {
                let dbg_dur = dbg_t0.elapsed();
                crate::resize_debug::end(crate::resize_debug::Phase::Ulw, Some(dbg_t0));
                let mut after: RECT = std::mem::zeroed();
                GetWindowRect(self.hwnd, &mut after);
                crate::resize_debug::ulw(crate::resize_debug::UlwSample {
                    hwnd_before: (win_rect.left, win_rect.top, win_rect.right, win_rect.bottom),
                    hwnd_after: (after.left, after.top, after.right, after.bottom),
                    pt_dst: (pt_dst.x, pt_dst.y),
                    psize: (self.width, self.height),
                    pixmap: (p_width, p_height),
                    alloc: (self.alloc_width, self.alloc_height),
                    dirty: (
                        clipped_dirty.x,
                        clipped_dirty.y,
                        clipped_dirty.width,
                        clipped_dirty.height,
                    ),
                    no_resize_flag: false,
                    resized,
                    force_full: dbg_force_full,
                    indirect_error: dbg_indirect_err,
                    fallback_ok: dbg_fallback_ok,
                    duration: dbg_dur,
                });
            }
            self.stats.ulw_ns += ns_since(t_ulw);
            trace_record(TraceKind::UpdateLayeredWindowEnd, hwnd_id, (0, 0), phys);
            crate::resize_trace::record_window_rect(TraceKind::WindowRectAfterUlw, hwnd_id);
            #[cfg(debug_assertions)]
            {
                let mut after: RECT = std::mem::zeroed();
                if GetWindowRect(self.hwnd, &mut after) != 0
                    && (after.left != win_rect.left
                        || after.top != win_rect.top
                        || after.right != win_rect.right
                        || after.bottom != win_rect.bottom)
                {
                    self.stats.ulw_rect_changed_by_ulw_count += 1;
                }
            }
            outcome?;
        }

        self.force_full = false;
        self.stats.present_count += 1;
        Ok(())
    }

    pub fn present_dirty(
        &mut self,
        pixmap: &mut Pixmap,
        opacity: f32,
        dirty: Rect,
    ) -> Result<(), &'static str> {
        self.present_dirty_ref(pixmap, opacity, dirty)
    }
}

/// Creates a top-down 32-bit DIB section of `w`x`h` compatible with `dc`.
unsafe fn create_dib(dc: HDC, w: u32, h: u32) -> Option<(HBITMAP, *mut u8)> {
    let mut bmi: BITMAPINFO = std::mem::zeroed();
    bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
    bmi.bmiHeader.biWidth = w.max(1) as i32;
    bmi.bmiHeader.biHeight = -(h.max(1) as i32); // Top-down
    bmi.bmiHeader.biPlanes = 1;
    bmi.bmiHeader.biBitCount = 32;
    bmi.bmiHeader.biCompression = BI_RGB;

    let mut bits: *mut core::ffi::c_void = ptr::null_mut();
    let hbitmap = CreateDIBSection(dc, &bmi, DIB_RGB_COLORS, &mut bits, ptr::null_mut(), 0);
    if hbitmap.is_null() || bits.is_null() {
        None
    } else {
        Some((hbitmap, bits as *mut u8))
    }
}

impl PlatformSurface for Win32LayeredSurface {
    fn width(&self) -> u32 {
        self.width
    }

    fn height(&self) -> u32 {
        self.height
    }

    fn resize(&mut self, width: u32, height: u32) -> Result<(), &'static str> {
        self.resize(width, height)
    }

    fn present(&mut self, pixmap: &mut Pixmap, opacity: f32) -> Result<(), &'static str> {
        self.present(pixmap, opacity)
    }

    fn present_dirty(
        &mut self,
        pixmap: &mut Pixmap,
        opacity: f32,
        dirty: Rect,
    ) -> Result<(), &'static str> {
        self.present_dirty(pixmap, opacity, dirty)
    }
}

impl Drop for Win32LayeredSurface {
    fn drop(&mut self) {
        unsafe {
            if !self.mem_dc.is_null() && !self.old_hbitmap.is_null() {
                SelectObject(self.mem_dc, self.old_hbitmap);
            }
            if !self.hbitmap.is_null() {
                DeleteObject(self.hbitmap);
            }
            if !self.mem_dc.is_null() {
                DeleteDC(self.mem_dc);
            }
            if !self.screen_dc.is_null() {
                ReleaseDC(ptr::null_mut(), self.screen_dc);
            }
        }
    }
}
