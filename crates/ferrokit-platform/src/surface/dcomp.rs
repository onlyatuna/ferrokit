// SPDX-License-Identifier: LGPL-3.0-only
#![allow(non_snake_case, dead_code)]
//! DirectComposition Hardware Accelerated Surface for Windows.
//!
//! Provides true differential presentation using DirectComposition and DXGI SwapChain1
//! (`Present1` with `pDirtyRects`), breaking the Win32 `UpdateLayeredWindow` full-surface
//! composition bottleneck on high-resolution screens.

use crate::surface::PlatformSurface;
use crate::win32_handles::{DibTarget, MemDc, create_dib_section};
use ferrokit_gui::geometry::Rect;
use ferrokit_gui::paint::Pixmap;
use std::ptr;
use windows::Win32::Foundation::{HWND as WinHwnd, RECT};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device,
    ID3D11DeviceContext,
};
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice, IDCompositionDevice, IDCompositionTarget, IDCompositionVisual,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ERROR_DEVICE_REMOVED, DXGI_ERROR_DEVICE_RESET, DXGI_PRESENT,
    DXGI_PRESENT_PARAMETERS, DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1,
    DXGI_SWAP_CHAIN_FLAG_GDI_COMPATIBLE, DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
    DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIDevice, IDXGIFactory2, IDXGISurface1, IDXGISwapChain1,
};
use windows::core::Interface as _;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::Graphics::Gdi::{BitBlt, HDC, SRCCOPY};

// -----------------------------------------------------------------------------
// DirectComposition Surface Implementation
// -----------------------------------------------------------------------------

/// Upper bound of a D3D11 texture dimension.
const MAX_SWAP_CHAIN_DIM: u32 = 16384;

/// Swap chain capacity for a visible extent `n`: 1.5x headroom rounded up to 64 px.
/// Interactive resizing then stays inside the allocation (no `ResizeBuffers`); growth beyond it
/// reallocates geometrically, so a long drag costs O(log) reallocations, not one per `WM_SIZE`.
fn capacity_for(n: u32) -> u32 {
    let padded = (n.saturating_mul(3) / 2).div_ceil(64) * 64;
    padded.clamp(n.max(64), MAX_SWAP_CHAIN_DIM.max(n))
}

/// Counters for instrumentation and tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DCompStats {
    /// `IDXGISwapChain::ResizeBuffers` calls (capacity growth/shrink only).
    pub resize_buffers_count: u64,
    /// Successful `Present1` calls.
    pub present_count: u64,
    /// Visible-size changes applied (clip update), whether or not buffers were reallocated.
    pub visible_resize_count: u64,
    /// `IDCompositionVisual::SetClip` calls.
    pub clip_update_count: u64,
    /// `IDCompositionDevice::Commit` calls.
    pub commit_count: u64,
}

/// Sets `clip_visual` clip rect `[0, w) x [0, h)` via `IDCompositionVisual::SetClip(D2D_RECT_F)`.
///
/// # Safety
/// `clip_visual` must be a live `IDCompositionVisual` belonging to the active device.
unsafe fn set_clip_rect(
    clip_visual: &IDCompositionVisual,
    w: u32,
    h: u32,
) -> Result<(), &'static str> {
    let rect = D2D_RECT_F {
        left: 0.0,
        top: 0.0,
        right: w as f32,
        bottom: h as f32,
    };
    // SAFETY: `rect` is a valid local `D2D_RECT_F` and `clip_visual` is a live COM visual.
    unsafe { clip_visual.SetClip2(&rect) }.map_err(|_| "SetClip failed on clip visual")
}

/// Owned D3D11 / DXGI / DirectComposition COM objects.
///
/// Field declaration order matches drop order: visuals and target drop before the DComp device,
/// and the swap chain drops before the D3D11 context and device.
struct DCompComState {
    dcomp_clip_visual: IDCompositionVisual,
    dcomp_visual: IDCompositionVisual,
    _dcomp_target: IDCompositionTarget,
    dcomp_device: IDCompositionDevice,
    swap_chain: IDXGISwapChain1,
    _d3d_context: ID3D11DeviceContext,
    _d3d_device: ID3D11Device,
}

/// Visual tree: `Target -> ClipVisual (clip = visible rect) -> ContentVisual (swap chain)`.
/// The swap chain has a fixed `allocated` size; only the clip follows the window (`visible`).
pub struct DCompSurface {
    hwnd: HWND,
    /// Visible (client) size: what the window shows and what `present` expects.
    width: u32,
    height: u32,
    /// Allocated swap chain / staging DIB size (>= visible).
    alloc_w: u32,
    alloc_h: u32,
    com: Option<DCompComState>,
    // Staging CPU GDI DIB for format blit (allocated size)
    staging: Option<DibTarget>,
    staging_w: u32,
    staging_h: u32,
    /// Back buffers were reallocated: the next present must cover the whole visible area.
    force_full: bool,
    /// The area the previous present changed. The flip-model swap chain has two buffers, so the
    /// back buffer holds the frame before the previous one: it needs the previous change as well
    /// as the current one.
    prev_dirty: Option<Rect>,
    /// The window opacity the owner asked for; presented with every `present_region`.
    opacity: f32,
    /// The opacity the staging DIB's pixels were last scaled by. A different value means every
    /// staged pixel is stale, so the next present covers the whole visible area.
    staged_opacity: f32,
    stats: DCompStats,
    #[cfg(any(test, feature = "test-hooks"))]
    fail_present_for_test: bool,
    #[cfg(any(test, feature = "test-hooks"))]
    fail_resize_for_test: bool,
}

/// Converts premultiplied RGBA pixels to the premultiplied BGRA of the staging DIB, scaling every
/// channel (alpha included) by `opacity`.
///
/// Scaling a premultiplied pixel by a constant is exactly `UpdateLayeredWindow`'s
/// `SourceConstantAlpha`, which is how Qt applies `windowOpacity` (:
/// `BYTE(qRound(255.0 * opacity))`). DirectComposition's swap chain is premultiplied
/// (`DXGI_ALPHA_MODE_PREMULTIPLIED`), so the composed result matches the layered window's.
pub fn convert_rgba_to_staging_bgra(src: &[u8], dst: &mut [u8], opacity: f32) {
    let alpha = (opacity.clamp(0.0, 1.0) * 255.0).round() as u32;
    let (src_px, _) = src.as_chunks::<4>();
    let (dst_px, _) = dst.as_chunks_mut::<4>();
    if alpha == 255 {
        for (s, d) in src_px.iter().zip(dst_px.iter_mut()) {
            *d = [s[2], s[1], s[0], s[3]];
        }
    } else {
        let scale = |c: u8| ((c as u32 * alpha + 127) / 255) as u8;
        for (s, d) in src_px.iter().zip(dst_px.iter_mut()) {
            *d = [scale(s[2]), scale(s[1]), scale(s[0]), scale(s[3])];
        }
    }
}

// Deliberately neither `Send` nor `Sync`: owns D3D11/DXGI/DirectComposition COM pointers
// whose immediate context and visual tree may only be driven from the window's thread.

impl DCompSurface {
    /// Creates a hardware accelerated DirectComposition surface for `hwnd`.
    // `hwnd` is passed only to D3D, DXGI and DirectComposition, which validate it (creation fails
    // for an invalid window); nothing here dereferences it.
    #[allow(clippy::not_unsafe_ptr_arg_deref)]
    pub fn new(hwnd: HWND, width: u32, height: u32) -> Result<Self, &'static str> {
        if width == 0 || height == 0 {
            return Err("Width and height must be greater than 0");
        }

        // SAFETY: `D3D11CreateDevice`, `CreateDXGIFactory1`, `CreateSwapChainForComposition`,
        // `DCompositionCreateDevice`, and `IDComposition*` calls follow the documented COM
        // contracts; `hwnd` is only passed to `CreateTargetForHwnd`, which validates it, and all
        // returned COM interfaces are owned by smart pointers in `DCompComState`.
        unsafe {
            // 1. Create Direct3D11 Device
            let mut d3d_device: Option<ID3D11Device> = None;
            let mut d3d_context: Option<ID3D11DeviceContext> = None;
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                Default::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut d3d_device),
                None,
                Some(&mut d3d_context),
            )
            .map_err(|_| "D3D11CreateDevice failed")?;
            let d3d_device = d3d_device.ok_or("D3D11CreateDevice failed")?;
            let d3d_context = d3d_context.ok_or("D3D11CreateDevice failed")?;

            // 2. Create IDXGIFactory2
            let dxgi_factory2: IDXGIFactory2 =
                CreateDXGIFactory1().map_err(|_| "CreateDXGIFactory1 failed for IDXGIFactory2")?;

            // 3. Create SwapChain for DirectComposition
            let (alloc_w, alloc_h) = (capacity_for(width), capacity_for(height));
            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: alloc_w,
                Height: alloc_h,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                Stereo: false.into(),
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                Scaling: DXGI_SCALING_STRETCH,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
                Flags: DXGI_SWAP_CHAIN_FLAG_GDI_COMPATIBLE.0 as u32,
            };

            let swap_chain = dxgi_factory2
                .CreateSwapChainForComposition(&d3d_device, &desc, None)
                .map_err(|_| "CreateSwapChainForComposition failed")?;

            // 4. Query IDXGIDevice from ID3D11Device and create DirectComposition Device
            let dxgi_device: IDXGIDevice = d3d_device
                .cast()
                .map_err(|_| "QueryInterface(IDXGIDevice) failed")?;
            let dcomp_device: IDCompositionDevice = DCompositionCreateDevice(&dxgi_device)
                .map_err(|_| "DCompositionCreateDevice failed")?;

            // 5. Create Target for HWND
            let dcomp_target = dcomp_device
                .CreateTargetForHwnd(WinHwnd(hwnd), false)
                .map_err(|_| "CreateTargetForHwnd failed")?;

            // 6. Create clip visual (root) and content visual (swap chain).
            let dcomp_visual = dcomp_device
                .CreateVisual()
                .map_err(|_| "CreateVisual failed")?;
            let dcomp_clip_visual = dcomp_device
                .CreateVisual()
                .map_err(|_| "CreateVisual (clip) failed")?;

            dcomp_visual
                .SetContent(&swap_chain)
                .map_err(|_| "SetContent failed on visual")?;
            dcomp_clip_visual
                .AddVisual(&dcomp_visual, true, None)
                .map_err(|_| "AddVisual failed on clip visual")?;
            set_clip_rect(&dcomp_clip_visual, width, height)?;

            dcomp_target
                .SetRoot(&dcomp_clip_visual)
                .map_err(|_| "SetRoot failed on target")?;
            let _ = dcomp_device.Commit();

            // 7. Allocate staging GDI DIB (allocated size) for software pixel translation
            let staging_dc = MemDc::create_compatible(ptr::null_mut())
                .ok_or("CreateCompatibleDC failed for the staging DIB")?;
            // A failed DIB drops `staging_dc` (DeleteDC) on the way out.
            let (staging_bitmap, staging_bits) =
                create_dib_section(staging_dc.as_raw(), alloc_w, alloc_h)
                    .ok_or("CreateDIBSection failed for the staging DIB")?;
            let staging = DibTarget::select(staging_dc, staging_bitmap, staging_bits);

            Ok(Self {
                hwnd,
                width,
                height,
                alloc_w,
                alloc_h,
                com: Some(DCompComState {
                    dcomp_clip_visual,
                    dcomp_visual,
                    _dcomp_target: dcomp_target,
                    dcomp_device,
                    swap_chain,
                    _d3d_context: d3d_context,
                    _d3d_device: d3d_device,
                }),
                staging: Some(staging),
                staging_w: alloc_w,
                staging_h: alloc_h,
                force_full: true,
                prev_dirty: None,
                opacity: 1.0,
                staged_opacity: 1.0,
                stats: DCompStats::default(),
                #[cfg(any(test, feature = "test-hooks"))]
                fail_present_for_test: false,
                #[cfg(any(test, feature = "test-hooks"))]
                fail_resize_for_test: false,
            })
        }
    }

    /// Creates a mock DCompSurface for deterministic unit/integration testing of
    /// failure recovery and downgrade pathways without requiring real DXGI/DComp hardware.
    #[cfg(any(test, feature = "test-hooks"))]
    pub fn new_mock_for_test(hwnd: HWND, width: u32, height: u32) -> Self {
        Self {
            hwnd,
            width,
            height,
            alloc_w: width,
            alloc_h: height,
            com: None,
            staging: None,
            staging_w: width,
            staging_h: height,
            force_full: true,
            prev_dirty: None,
            opacity: 1.0,
            staged_opacity: 1.0,
            stats: DCompStats::default(),
            fail_present_for_test: false,
            fail_resize_for_test: false,
        }
    }

    /// Sets the window opacity applied by the next present.
    pub fn set_opacity(&mut self, opacity: f32) {
        self.opacity = opacity.clamp(0.0, 1.0);
    }

    pub fn opacity(&self) -> f32 {
        self.opacity
    }

    /// The staged (about to be presented) pixel at `(x, y)` as `[b, g, r, a]`, for tests.
    pub fn staged_pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if self.staging_bits().is_null()
            || x >= self.width.min(self.staging_w)
            || y >= self.height.min(self.staging_h)
        {
            return None;
        }
        let off = (y as usize * self.staging_w as usize + x as usize) * 4;
        // SAFETY: (x, y) is inside the staging DIB, which is `staging_w * staging_h * 4` bytes.
        Some(unsafe { *(self.staging_bits().add(off) as *const [u8; 4]) })
    }

    /// The staging DC; null for the mock surface, which has no staging DIB.
    fn staging_dc(&self) -> HDC {
        self.staging.as_ref().map_or(ptr::null_mut(), DibTarget::dc)
    }

    /// The staging DIB pixels; null for the mock surface, which has no staging DIB.
    fn staging_bits(&self) -> *mut u8 {
        self.staging
            .as_ref()
            .map_or(ptr::null_mut(), DibTarget::bits)
    }

    /// Allocated swap chain size (>= visible size).
    pub fn allocated_width(&self) -> u32 {
        self.alloc_w
    }

    pub fn allocated_height(&self) -> u32 {
        self.alloc_h
    }

    pub fn stats(&self) -> DCompStats {
        self.stats
    }

    /// Injects a synthetic present failure for testing downgrade paths.
    #[cfg(any(test, feature = "test-hooks"))]
    pub fn set_fail_present_for_test(&mut self, fail: bool) {
        self.fail_present_for_test = fail;
    }

    /// Injects a synthetic resize failure for testing downgrade paths.
    #[cfg(any(test, feature = "test-hooks"))]
    pub fn set_fail_resize_for_test(&mut self, fail: bool) {
        self.fail_resize_for_test = fail;
    }

    /// Changes the visible size. Inside the allocated capacity this only moves the clip:
    /// no `ResizeBuffers`, no staging reallocation. Swap chain buffers are reallocated only
    /// when the capacity is too small, or more than 2x too large (hysteresis: the new
    /// capacity has 1.5x headroom, so shrink/grow do not alternate).
    pub fn resize(&mut self, width: u32, height: u32) -> Result<(), &'static str> {
        #[cfg(any(test, feature = "test-hooks"))]
        if self.fail_resize_for_test {
            return Err("Injected DirectComposition resize failure");
        }
        if self.com.is_none() {
            if width == 0 || height == 0 {
                return Err("Width and height must be greater than 0");
            }
            self.width = width;
            self.height = height;
            return Ok(());
        }
        if width == 0 || height == 0 {
            return Err("Width and height must be greater than 0");
        }
        if self.width == width && self.height == height {
            return Ok(());
        }

        // One `ResizeBuffers` per capacity change: when either axis leaves its band, both axes get
        // fresh headroom together (otherwise width and height would reallocate at different sizes
        // and double the calls).
        let outside =
            |alloc: u32, visible: u32| visible > alloc || alloc > visible.saturating_mul(2);
        let (new_alloc_w, new_alloc_h) =
            if outside(self.alloc_w, width) || outside(self.alloc_h, height) {
                (capacity_for(width), capacity_for(height))
            } else {
                (self.alloc_w, self.alloc_h)
            };
        if new_alloc_w != self.alloc_w || new_alloc_h != self.alloc_h {
            self.resize_buffers(new_alloc_w, new_alloc_h)?;
        }

        self.width = width;
        self.height = height;
        self.stats.visible_resize_count += 1;
        // Content outside the previous visible area is stale: repaint all visible pixels.
        self.force_full = true;
        if let Some(com) = self.com.as_ref() {
            // SAFETY: `com.dcomp_clip_visual` is a live `IDCompositionVisual` owned by `self.com`.
            unsafe { set_clip_rect(&com.dcomp_clip_visual, width, height)? };
        }
        self.stats.clip_update_count += 1;
        crate::resize_trace::record(
            crate::resize_trace::TraceKind::SurfaceResize,
            self.hwnd as usize,
            (0, 0),
            (width, height),
        );
        Ok(())
    }

    /// Reallocates swap chain buffers and the staging DIB to `alloc_w x alloc_h`.
    fn resize_buffers(&mut self, alloc_w: u32, alloc_h: u32) -> Result<(), &'static str> {
        let Some(com) = self.com.as_ref() else {
            return Ok(());
        };
        // Allocate the new staging DIB first: if it fails nothing has changed yet (the swap
        // chain keeps its size, the old DIB stays selected).
        let new_dib = if self.staging.is_some() {
            match create_dib_section(self.staging_dc(), alloc_w, alloc_h) {
                Some(dib) => Some(dib),
                None => return Err("CreateDIBSection failed for the staging DIB"),
            }
        } else {
            None
        };

        // SAFETY: `com.swap_chain` is live, `&mut self` guarantees exclusive access, and no
        // back-buffer references remain outstanding after `present_dirty_ref` returns.
        let res = unsafe {
            com.swap_chain.ResizeBuffers(
                2,
                alloc_w,
                alloc_h,
                DXGI_FORMAT_B8G8R8A8_UNORM,
                DXGI_SWAP_CHAIN_FLAG_GDI_COMPATIBLE,
            )
        };
        if let Err(err) = res {
            let hr = err.code();
            if hr == DXGI_ERROR_DEVICE_REMOVED || hr == DXGI_ERROR_DEVICE_RESET {
                return Err("DXGI_ERROR_DEVICE_LOST");
            }
            return Err("ResizeBuffers failed on swap chain");
        }
        self.stats.resize_buffers_count += 1;

        // Swap the staging DIB: restore the previous object, delete the old DIB, select the
        // new one.
        if let (Some(staging), Some((new_bmp, bits))) = (&mut self.staging, new_dib) {
            staging.replace_deselecting(new_bmp, bits);
            self.staging_w = alloc_w;
            self.staging_h = alloc_h;
        }
        self.alloc_w = alloc_w;
        self.alloc_h = alloc_h;
        // Fresh buffers + staging: the next present must repaint everything visible.
        self.force_full = true;

        Ok(())
    }

    pub fn present_dirty_ref(
        &mut self,
        pixmap: &Pixmap,
        opacity: f32,
        dirty: Rect,
    ) -> Result<(), &'static str> {
        #[cfg(any(test, feature = "test-hooks"))]
        if self.fail_present_for_test {
            return Err("Injected DirectComposition present failure");
        }
        if self.com.is_none() {
            self.width = pixmap.physical_width();
            self.height = pixmap.physical_height();
            return Ok(());
        }
        let p_width = pixmap.physical_width();
        let p_height = pixmap.physical_height();
        if p_width != self.width || p_height != self.height {
            self.resize(p_width, p_height)?;
        }
        if opacity != self.staged_opacity {
            self.force_full = true;
        }

        let full_window_rect = Rect::new(0, 0, self.width as i32, self.height as i32);
        let full_present = self.force_full;
        let dirty = if self.force_full {
            full_window_rect
        } else {
            dirty
        };
        let clipped = full_window_rect.intersected(&dirty);
        if clipped.is_empty() {
            return Ok(());
        }

        let staging_dc = self.staging_dc();
        let staging_bits = self.staging_bits();
        let Some(com) = self.com.as_ref() else {
            return Ok(());
        };

        // SAFETY: only reached for a real surface with live `com` and `staging`:
        // - `resize` above matched `pixmap` dimensions to `self.width/height`; `clipped` lies
        //   inside `[0, width) x [0, height)` and `staging_w/h >= width/height`, so row slices
        //   stay in bounds on distinct allocations.
        // - `GetBuffer`, `GetDC`, `ReleaseDC`, `Present1`, and `Commit` follow DXGI/DComp docs.
        unsafe {
            // 1. Copy dirty rect pixels from Pixmap (RGBA) into staging DIB (BGRA)
            let src_stride = (self.width * 4) as usize;
            let dst_stride = (self.staging_w * 4) as usize;
            let dirty_x = clipped.x as usize;
            let dirty_w = clipped.width as usize;
            let src_data = pixmap.data();

            for y in clipped.y..(clipped.y + clipped.height) {
                let y = y as usize;
                let src_offset = y * src_stride + dirty_x * 4;
                let dst_offset = y * dst_stride + dirty_x * 4;
                let copy_bytes = dirty_w * 4;
                let src_row =
                    std::slice::from_raw_parts(src_data.as_ptr().add(src_offset), copy_bytes);
                let dst_row =
                    std::slice::from_raw_parts_mut(staging_bits.add(dst_offset), copy_bytes);

                convert_rgba_to_staging_bgra(src_row, dst_row, opacity);
            }
            self.staged_opacity = opacity;

            // 2. Query back buffer IDXGISurface1
            let dxgi_surface1: IDXGISurface1 = com
                .swap_chain
                .GetBuffer(0)
                .map_err(|_| "IDXGISwapChain::GetBuffer(0, IDXGISurface1) failed")?;

            // 3. Acquire HDC on DXGI Surface
            let dxgi_dc = dxgi_surface1
                .GetDC(false)
                .map_err(|_| "IDXGISurface1::GetDC failed")?;

            // 4. BitBlt the changed area from the staging DIB to the back buffer, together with the
            // previous present's area: this buffer last showed the frame before that.
            let blit = match (full_present, self.prev_dirty) {
                (false, Some(prev)) => full_window_rect.intersected(&clipped.united(&prev)),
                _ => clipped,
            };
            BitBlt(
                dxgi_dc.0,
                blit.x,
                blit.y,
                blit.width,
                blit.height,
                staging_dc,
                blit.x,
                blit.y,
                SRCCOPY,
            );

            // 5. Release DC
            let dirty_gdi_rect = RECT {
                left: blit.x,
                top: blit.y,
                right: blit.x + blit.width,
                bottom: blit.y + blit.height,
            };
            let _ = dxgi_surface1.ReleaseDC(Some(&dirty_gdi_rect));
            drop(dxgi_surface1);

            // 6. Present1 with pDirtyRects
            let mut dxgi_dirty_rect = RECT {
                left: clipped.x,
                top: clipped.y,
                right: clipped.x + clipped.width,
                bottom: clipped.y + clipped.height,
            };
            // DXGI rejects dirty rectangles (`DXGI_ERROR_INVALID_CALL`) on the first present of
            // fresh buffers, which is exactly when everything is dirty anyway.
            let present_params = DXGI_PRESENT_PARAMETERS {
                DirtyRectsCount: if full_present { 0 } else { 1 },
                pDirtyRects: &mut dxgi_dirty_rect,
                pScrollRect: ptr::null_mut(),
                pScrollOffset: ptr::null_mut(),
            };

            com.swap_chain
                .Present1(0, DXGI_PRESENT(0), &present_params)
                .ok()
                .map_err(|_| "IDXGISwapChain1::Present1 failed")?;
            self.force_full = false;
            self.prev_dirty = Some(clipped);
            self.stats.present_count += 1;
            crate::resize_trace::record(
                crate::resize_trace::TraceKind::Present1,
                self.hwnd as usize,
                (0, 0),
                (self.width, self.height),
            );

            // 7. Commit DirectComposition device (also publishes any clip change)
            com.dcomp_device
                .Commit()
                .map_err(|_| "IDCompositionDevice::Commit failed")?;
            self.stats.commit_count += 1;
            crate::resize_trace::record(
                crate::resize_trace::TraceKind::DCompCommit,
                self.hwnd as usize,
                (0, 0),
                (self.width, self.height),
            );
        }

        Ok(())
    }
}

impl PlatformSurface for DCompSurface {
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
        let full_dirty = Rect::new(0, 0, self.width as i32, self.height as i32);
        self.present_dirty(pixmap, opacity, full_dirty)
    }

    fn present_dirty(
        &mut self,
        pixmap: &mut Pixmap,
        opacity: f32,
        dirty: Rect,
    ) -> Result<(), &'static str> {
        self.present_dirty_ref(pixmap, opacity, dirty)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::window::{NativeWindow, WindowFlags};
    use ferrokit_gui::tiny_skia::Color;

    #[test]
    fn test_dcomp_surface_creation_and_present_dirty() {
        let window = NativeWindow::new(
            "DComp Surface Test",
            Rect::new(10, 10, 100, 80),
            WindowFlags::FRAMELESS,
        )
        .expect("native window create");

        let dcomp_res = DCompSurface::new(window.hwnd(), 100, 80);
        if let Ok(mut surface) = dcomp_res {
            assert_eq!(surface.width(), 100);
            assert_eq!(surface.height(), 80);

            let mut pixmap = Pixmap::new(100, 80).unwrap();
            pixmap.fill(Color::from_rgba8(255, 100, 50, 200));

            // Test dirty present
            let dirty = Rect::new(10, 10, 40, 30);
            let res = surface.present_dirty(&mut pixmap, 1.0, dirty);
            assert!(res.is_ok(), "DComp present_dirty failed: {:?}", res.err());

            // Test resize
            let resize_res = surface.resize(120, 90);
            assert!(
                resize_res.is_ok(),
                "DComp resize failed: {:?}",
                resize_res.err()
            );
            assert_eq!(surface.width(), 120);
            assert_eq!(surface.height(), 90);

            let mut pixmap2 = Pixmap::new(120, 90).unwrap();
            pixmap2.fill(Color::from_rgba8(50, 150, 250, 255));
            let res2 = surface.present_dirty(&mut pixmap2, 1.0, Rect::new(0, 0, 120, 90));
            assert!(
                res2.is_ok(),
                "DComp resized present_dirty failed: {:?}",
                res2.err()
            );
        } else {
            eprintln!("DirectComposition not supported on this device/driver, skipping test");
        }
    }

    #[test]
    fn test_windows_surface_backend_negotiation() {
        use crate::surface::WindowsSurface;

        let window = NativeWindow::new(
            "Negotiation Test",
            Rect::new(20, 20, 80, 60),
            WindowFlags::FRAMELESS | WindowFlags::LAYERED,
        )
        .expect("native window create");

        let mut surface = WindowsSurface::create(window.hwnd(), 80, 60)
            .expect("WindowsSurface::create must succeed via DComp or GDI");

        assert_eq!(surface.width(), 80);
        assert_eq!(surface.height(), 60);

        let mut pixmap = Pixmap::new(80, 60).unwrap();
        pixmap.fill(Color::from_rgba8(30, 60, 90, 220));

        let res = surface.present_dirty(&mut pixmap, 0.9, Rect::new(5, 5, 30, 25));
        assert!(
            res.is_ok(),
            "WindowsSurface present_dirty failed: {:?}",
            res.err()
        );
    }
}
