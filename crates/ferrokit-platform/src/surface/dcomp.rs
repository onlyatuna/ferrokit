// SPDX-License-Identifier: LGPL-3.0-only
#![allow(non_snake_case, dead_code)]
//! DirectComposition Hardware Accelerated Surface for Windows.
//!
//! Provides true differential presentation using DirectComposition and DXGI SwapChain1
//! (`Present1` with `pDirtyRects`), breaking the Win32 `UpdateLayeredWindow` full-surface
//! composition bottleneck on high-resolution screens.

use crate::surface::PlatformSurface;
use ferrokit_gui::geometry::Rect;
use ferrokit_gui::paint::Pixmap;
use std::ffi::c_void;
use std::ptr;
use windows_sys::core::{GUID, HRESULT};
use windows_sys::Win32::Foundation::{HMODULE, HWND, POINT, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, SelectObject, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryA};

#[link(name = "kernel32")]
extern "system" {
    fn FreeLibrary(hLibModule: HMODULE) -> i32;
}
// -----------------------------------------------------------------------------
// Constants and GUIDs
// -----------------------------------------------------------------------------

const DXGI_FORMAT_B8G8R8A8_UNORM: u32 = 87;
const DXGI_USAGE_RENDER_TARGET_OUTPUT: u32 = 0x00000020;
const DXGI_SCALING_STRETCH: u32 = 0;
const DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL: u32 = 3;
const DXGI_ALPHA_MODE_PREMULTIPLIED: u32 = 2;
const DXGI_SWAP_CHAIN_FLAG_GDI_COMPATIBLE: u32 = 0x200;

const D3D_DRIVER_TYPE_HARDWARE: u32 = 1;
const D3D11_CREATE_DEVICE_BGRA_SUPPORT: u32 = 0x20;
const D3D11_SDK_VERSION: u32 = 7;

const IID_IUNKNOWN: GUID = GUID {
    data1: 0x00000000,
    data2: 0x0000,
    data3: 0x0000,
    data4: [0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46],
};

const IID_IDXGI_FACTORY2: GUID = GUID {
    data1: 0x50c83a1c,
    data2: 0xe072,
    data3: 0x4d48,
    data4: [0x87, 0xb0, 0x36, 0x30, 0xfa, 0x36, 0xa6, 0xd0],
};

const IID_IDXGI_SWAP_CHAIN1: GUID = GUID {
    data1: 0x790a45f8,
    data2: 0x0f42,
    data3: 0x4876,
    data4: [0x98, 0x52, 0x38, 0xb8, 0x2e, 0x7e, 0x1b, 0x6a],
};

const IID_IDXGI_SURFACE1: GUID = GUID {
    data1: 0x4AE63092,
    data2: 0x6327,
    data3: 0x4c1b,
    data4: [0x80, 0xAE, 0xBF, 0xE1, 0xE0, 0x84, 0x10, 0xCD],
};

const IID_IDCOMPOSITION_DEVICE: GUID = GUID {
    data1: 0xC37D25AE,
    data2: 0x2E15,
    data3: 0x4673,
    data4: [0x9F, 0x94, 0xDB, 0x76, 0x08, 0x16, 0x0F, 0xDE],
};

// -----------------------------------------------------------------------------
// DXGI / Direct3D / DirectComposition Structs
// -----------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy)]
pub struct DXGI_SWAP_CHAIN_DESC1 {
    pub Width: u32,
    pub Height: u32,
    pub Format: u32,
    pub Stereo: i32,
    pub SampleDesc_Count: u32,
    pub SampleDesc_Quality: u32,
    pub BufferUsage: u32,
    pub BufferCount: u32,
    pub Scaling: u32,
    pub SwapEffect: u32,
    pub AlphaMode: u32,
    pub Flags: u32,
}

#[repr(C)]
pub struct DXGI_PRESENT_PARAMETERS {
    pub DirtyRectsCount: u32,
    pub pDirtyRects: *const RECT,
    pub pScrollRect: *const RECT,
    pub pScrollOffset: *const POINT,
}

// -----------------------------------------------------------------------------
// Function Pointer Types
// -----------------------------------------------------------------------------

type PfnD3D11CreateDevice = unsafe extern "system" fn(
    pAdapter: *mut c_void,
    DriverType: u32,
    Software: *mut c_void,
    Flags: u32,
    pFeatureLevels: *const u32,
    FeatureLevels: u32,
    SDKVersion: u32,
    ppDevice: *mut *mut c_void,
    pFeatureLevel: *mut u32,
    ppImmediateContext: *mut *mut c_void,
) -> HRESULT;

type PfnCreateDXGIFactory1 =
    unsafe extern "system" fn(riid: *const GUID, ppFactory: *mut *mut c_void) -> HRESULT;

type PfnDCompositionCreateDevice = unsafe extern "system" fn(
    dxgiDevice: *mut c_void,
    riid: *const GUID,
    pDCompositionDevice: *mut *mut c_void,
) -> HRESULT;

// -----------------------------------------------------------------------------
// COM Helper Utilities
// -----------------------------------------------------------------------------

unsafe fn com_release(ptr: *mut c_void) {
    if !ptr.is_null() {
        let vtbl = *(ptr as *mut *mut usize);
        let release_fn: unsafe extern "system" fn(*mut c_void) -> u32 =
            std::mem::transmute(*vtbl.add(2));
        release_fn(ptr);
    }
}

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

/// `IDCompositionVisual::SetClip(const D2D_RECT_F&)` (vtable slot 14; slot 13 is the
/// `IDCompositionClip*` overload) with the rect `[0, w) x [0, h)`.
unsafe fn set_clip_rect(clip_visual: *mut c_void, w: u32, h: u32) -> HRESULT {
    #[repr(C)]
    struct D2dRectF {
        left: f32,
        top: f32,
        right: f32,
        bottom: f32,
    }
    type PfnSetClipRect =
        unsafe extern "system" fn(this: *mut c_void, rect: *const D2dRectF) -> HRESULT;
    let vtbl = *(clip_visual as *mut *mut usize);
    let f: PfnSetClipRect = std::mem::transmute(*vtbl.add(14));
    let rect = D2dRectF {
        left: 0.0,
        top: 0.0,
        right: w as f32,
        bottom: h as f32,
    };
    f(clip_visual, &rect)
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
    d3d11_mod: HMODULE,
    dxgi_mod: HMODULE,
    dcomp_mod: HMODULE,
    d3d_device: *mut c_void,
    d3d_context: *mut c_void,
    dcomp_device: *mut c_void,
    dcomp_target: *mut c_void,
    dcomp_clip_visual: *mut c_void,
    dcomp_visual: *mut c_void,
    swap_chain: *mut c_void,
    // Staging CPU GDI DIB for format blit (allocated size)
    staging_dc: HDC,
    staging_bitmap: HBITMAP,
    staging_old_bitmap: HGDIOBJ,
    staging_bits: *mut u8,
    staging_w: u32,
    staging_h: u32,
    /// Back buffers were reallocated: the next present must cover the whole visible area.
    force_full: bool,
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
/// `SourceConstantAlpha`, which is how Qt applies `windowOpacity` (`qwindowsbackingstore.cpp:66`:
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

unsafe impl Send for DCompSurface {}
unsafe impl Sync for DCompSurface {}

impl DCompSurface {
    /// Creates a hardware accelerated DirectComposition surface for `hwnd`.
    pub fn new(hwnd: HWND, width: u32, height: u32) -> Result<Self, &'static str> {
        if width == 0 || height == 0 {
            return Err("Width and height must be greater than 0");
        }

        unsafe {
            // 1. Dynamically load runtime DLLs
            let d3d11_mod = LoadLibraryA(c"d3d11.dll".as_ptr() as *const u8);
            if d3d11_mod.is_null() {
                return Err("Failed to load d3d11.dll");
            }

            let dxgi_mod = LoadLibraryA(c"dxgi.dll".as_ptr() as *const u8);
            if dxgi_mod.is_null() {
                FreeLibrary(d3d11_mod);
                return Err("Failed to load dxgi.dll");
            }

            let dcomp_mod = LoadLibraryA(c"dcomp.dll".as_ptr() as *const u8);
            if dcomp_mod.is_null() {
                FreeLibrary(dxgi_mod);
                FreeLibrary(d3d11_mod);
                return Err("Failed to load dcomp.dll (DirectComposition unsupported)");
            }

            // 2. Resolve entry points
            let d3d11_create_device_ptr =
                GetProcAddress(d3d11_mod, c"D3D11CreateDevice".as_ptr() as *const u8);
            let create_dxgi_factory_ptr =
                GetProcAddress(dxgi_mod, c"CreateDXGIFactory1".as_ptr() as *const u8);
            let dcomp_create_device_ptr =
                GetProcAddress(dcomp_mod, c"DCompositionCreateDevice".as_ptr() as *const u8);

            if d3d11_create_device_ptr.is_none()
                || create_dxgi_factory_ptr.is_none()
                || dcomp_create_device_ptr.is_none()
            {
                FreeLibrary(dcomp_mod);
                FreeLibrary(dxgi_mod);
                FreeLibrary(d3d11_mod);
                return Err("Failed to resolve DirectComposition entry points");
            }

            let d3d11_create_device: PfnD3D11CreateDevice =
                std::mem::transmute(d3d11_create_device_ptr);
            let create_dxgi_factory: PfnCreateDXGIFactory1 =
                std::mem::transmute(create_dxgi_factory_ptr);
            let dcomp_create_device: PfnDCompositionCreateDevice =
                std::mem::transmute(dcomp_create_device_ptr);

            // 3. Create Direct3D11 Device
            let mut d3d_device: *mut c_void = ptr::null_mut();
            let mut d3d_context: *mut c_void = ptr::null_mut();
            let hr = d3d11_create_device(
                ptr::null_mut(),
                D3D_DRIVER_TYPE_HARDWARE,
                ptr::null_mut(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                ptr::null(),
                0,
                D3D11_SDK_VERSION,
                &mut d3d_device,
                ptr::null_mut(),
                &mut d3d_context,
            );
            if hr < 0 || d3d_device.is_null() {
                FreeLibrary(dcomp_mod);
                FreeLibrary(dxgi_mod);
                FreeLibrary(d3d11_mod);
                return Err("D3D11CreateDevice failed");
            }

            // 4. Create IDXGIFactory2
            let mut dxgi_factory2: *mut c_void = ptr::null_mut();
            let hr = create_dxgi_factory(&IID_IDXGI_FACTORY2, &mut dxgi_factory2);
            if hr < 0 || dxgi_factory2.is_null() {
                com_release(d3d_context);
                com_release(d3d_device);
                FreeLibrary(dcomp_mod);
                FreeLibrary(dxgi_mod);
                FreeLibrary(d3d11_mod);
                return Err("CreateDXGIFactory1 failed for IDXGIFactory2");
            }

            // 5. Create SwapChain for DirectComposition
            let (alloc_w, alloc_h) = (capacity_for(width), capacity_for(height));
            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: alloc_w,
                Height: alloc_h,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                Stereo: 0,
                SampleDesc_Count: 1,
                SampleDesc_Quality: 0,
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                Scaling: DXGI_SCALING_STRETCH,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
                Flags: DXGI_SWAP_CHAIN_FLAG_GDI_COMPATIBLE,
            };

            let fac_vtbl = *(dxgi_factory2 as *mut *mut usize);
            // CreateSwapChainForComposition is slot 24 in IDXGIFactory2
            type PfnCreateSwapChainForComposition = unsafe extern "system" fn(
                this: *mut c_void,
                pDevice: *mut c_void,
                pDesc: *const DXGI_SWAP_CHAIN_DESC1,
                pRestrictToOutput: *mut c_void,
                ppSwapChain: *mut *mut c_void,
            )
                -> HRESULT;
            let create_swap_chain_fn: PfnCreateSwapChainForComposition =
                std::mem::transmute(*fac_vtbl.add(24));

            let mut swap_chain: *mut c_void = ptr::null_mut();
            let hr = create_swap_chain_fn(
                dxgi_factory2,
                d3d_device,
                &desc,
                ptr::null_mut(),
                &mut swap_chain,
            );
            com_release(dxgi_factory2);

            if hr < 0 || swap_chain.is_null() {
                com_release(d3d_context);
                com_release(d3d_device);
                FreeLibrary(dcomp_mod);
                FreeLibrary(dxgi_mod);
                FreeLibrary(d3d11_mod);
                return Err("CreateSwapChainForComposition failed");
            }

            // 6. Create DirectComposition Device
            let mut dcomp_device: *mut c_void = ptr::null_mut();
            let hr = dcomp_create_device(d3d_device, &IID_IDCOMPOSITION_DEVICE, &mut dcomp_device);
            if hr < 0 || dcomp_device.is_null() {
                com_release(swap_chain);
                com_release(d3d_context);
                com_release(d3d_device);
                FreeLibrary(dcomp_mod);
                FreeLibrary(dxgi_mod);
                FreeLibrary(d3d11_mod);
                return Err("DCompositionCreateDevice failed");
            }

            // 7. Create Target for HWND
            let dev_vtbl = *(dcomp_device as *mut *mut usize);
            // CreateTargetForHwnd is slot 6 on IDCompositionDevice
            type PfnCreateTargetForHwnd = unsafe extern "system" fn(
                this: *mut c_void,
                hwnd: HWND,
                topmost: i32,
                target: *mut *mut c_void,
            ) -> HRESULT;
            let create_target_fn: PfnCreateTargetForHwnd = std::mem::transmute(*dev_vtbl.add(6));

            let mut dcomp_target: *mut c_void = ptr::null_mut();
            let hr = create_target_fn(dcomp_device, hwnd, 0, &mut dcomp_target);
            if hr < 0 || dcomp_target.is_null() {
                com_release(dcomp_device);
                com_release(swap_chain);
                com_release(d3d_context);
                com_release(d3d_device);
                FreeLibrary(dcomp_mod);
                FreeLibrary(dxgi_mod);
                FreeLibrary(d3d11_mod);
                return Err("CreateTargetForHwnd failed");
            }

            // 8. Create clip visual (root) and content visual (swap chain).
            // CreateVisual is slot 7 on IDCompositionDevice
            type PfnCreateVisual =
                unsafe extern "system" fn(this: *mut c_void, visual: *mut *mut c_void) -> HRESULT;
            let create_visual_fn: PfnCreateVisual = std::mem::transmute(*dev_vtbl.add(7));

            let cleanup = |extras: &[*mut c_void]| {
                for p in extras {
                    com_release(*p);
                }
                com_release(dcomp_target);
                com_release(dcomp_device);
                com_release(swap_chain);
                com_release(d3d_context);
                com_release(d3d_device);
                FreeLibrary(dcomp_mod);
                FreeLibrary(dxgi_mod);
                FreeLibrary(d3d11_mod);
            };

            let mut dcomp_visual: *mut c_void = ptr::null_mut();
            let hr = create_visual_fn(dcomp_device, &mut dcomp_visual);
            if hr < 0 || dcomp_visual.is_null() {
                cleanup(&[]);
                return Err("CreateVisual failed");
            }
            let mut dcomp_clip_visual: *mut c_void = ptr::null_mut();
            let hr = create_visual_fn(dcomp_device, &mut dcomp_clip_visual);
            if hr < 0 || dcomp_clip_visual.is_null() {
                cleanup(&[dcomp_visual]);
                return Err("CreateVisual (clip) failed");
            }

            // 9. Content visual shows the swap chain: SetContent is slot 15 on IDCompositionVisual
            let vis_vtbl = *(dcomp_visual as *mut *mut usize);
            type PfnSetContent =
                unsafe extern "system" fn(this: *mut c_void, content: *mut c_void) -> HRESULT;
            let set_content_fn: PfnSetContent = std::mem::transmute(*vis_vtbl.add(15));
            let hr = set_content_fn(dcomp_visual, swap_chain);
            if hr < 0 {
                cleanup(&[dcomp_clip_visual, dcomp_visual]);
                return Err("SetContent failed on visual");
            }

            // Clip visual: AddVisual is slot 16, SetClip(const D2D_RECT_F&) is slot 14.
            let clip_vtbl = *(dcomp_clip_visual as *mut *mut usize);
            type PfnAddVisual = unsafe extern "system" fn(
                this: *mut c_void,
                visual: *mut c_void,
                insert_above: i32,
                reference: *mut c_void,
            ) -> HRESULT;
            let add_visual_fn: PfnAddVisual = std::mem::transmute(*clip_vtbl.add(16));
            let hr = add_visual_fn(dcomp_clip_visual, dcomp_visual, 1, ptr::null_mut());
            if hr < 0 {
                cleanup(&[dcomp_clip_visual, dcomp_visual]);
                return Err("AddVisual failed on clip visual");
            }
            let hr = set_clip_rect(dcomp_clip_visual, width, height);
            if hr < 0 {
                cleanup(&[dcomp_clip_visual, dcomp_visual]);
                return Err("SetClip failed on clip visual");
            }

            // 10. Set root visual: SetRoot is slot 3 on IDCompositionTarget
            let target_vtbl = *(dcomp_target as *mut *mut usize);
            type PfnSetRoot =
                unsafe extern "system" fn(this: *mut c_void, visual: *mut c_void) -> HRESULT;
            let set_root_fn: PfnSetRoot = std::mem::transmute(*target_vtbl.add(3));
            let hr = set_root_fn(dcomp_target, dcomp_clip_visual);
            if hr < 0 {
                cleanup(&[dcomp_clip_visual, dcomp_visual]);
                return Err("SetRoot failed on target");
            }

            // 11. Commit device: Commit is slot 3 on IDCompositionDevice
            type PfnCommit = unsafe extern "system" fn(this: *mut c_void) -> HRESULT;
            let commit_fn: PfnCommit = std::mem::transmute(*dev_vtbl.add(3));
            let _ = commit_fn(dcomp_device);

            // 12. Allocate staging GDI DIB (allocated size) for software pixel translation
            let staging_dc = CreateCompatibleDC(ptr::null_mut());
            let mut bmi: BITMAPINFO = std::mem::zeroed();
            bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            bmi.bmiHeader.biWidth = alloc_w.max(1) as i32;
            bmi.bmiHeader.biHeight = -(alloc_h.max(1) as i32); // Top-down
            bmi.bmiHeader.biPlanes = 1;
            bmi.bmiHeader.biBitCount = 32;
            bmi.bmiHeader.biCompression = BI_RGB;

            let mut staging_bits: *mut c_void = ptr::null_mut();
            let staging_bitmap = CreateDIBSection(
                staging_dc,
                &bmi,
                DIB_RGB_COLORS,
                &mut staging_bits,
                ptr::null_mut(),
                0,
            );
            let staging_old_bitmap = SelectObject(staging_dc, staging_bitmap);

            Ok(Self {
                hwnd,
                width,
                height,
                alloc_w,
                alloc_h,
                d3d11_mod,
                dxgi_mod,
                dcomp_mod,
                d3d_device,
                d3d_context,
                dcomp_device,
                dcomp_target,
                dcomp_clip_visual,
                dcomp_visual,
                swap_chain,
                staging_dc,
                staging_bitmap,
                staging_old_bitmap,
                staging_bits: staging_bits as *mut u8,
                staging_w: alloc_w,
                staging_h: alloc_h,
                force_full: true,
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
            d3d11_mod: ptr::null_mut(),
            dxgi_mod: ptr::null_mut(),
            dcomp_mod: ptr::null_mut(),
            d3d_device: ptr::null_mut(),
            d3d_context: ptr::null_mut(),
            dcomp_device: ptr::null_mut(),
            dcomp_target: ptr::null_mut(),
            dcomp_clip_visual: ptr::null_mut(),
            dcomp_visual: ptr::null_mut(),
            swap_chain: ptr::null_mut(),
            staging_dc: ptr::null_mut(),
            staging_bitmap: ptr::null_mut(),
            staging_old_bitmap: ptr::null_mut(),
            staging_bits: ptr::null_mut(),
            staging_w: width,
            staging_h: height,
            force_full: true,
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
        if x >= self.width.min(self.staging_w) || y >= self.height.min(self.staging_h) {
            return None;
        }
        let off = (y as usize * self.staging_w as usize + x as usize) * 4;
        // SAFETY: (x, y) is inside the staging DIB, which is `staging_w * staging_h * 4` bytes.
        Some(unsafe { *(self.staging_bits.add(off) as *const [u8; 4]) })
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
        #[cfg(any(test, feature = "test-hooks"))]
        if self.dcomp_device.is_null() {
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

        let realloc = |alloc: u32, visible: u32| {
            if visible > alloc || alloc > visible.saturating_mul(2) {
                capacity_for(visible)
            } else {
                alloc
            }
        };
        let new_alloc_w = realloc(self.alloc_w, width);
        let new_alloc_h = realloc(self.alloc_h, height);
        if new_alloc_w != self.alloc_w || new_alloc_h != self.alloc_h {
            self.resize_buffers(new_alloc_w, new_alloc_h)?;
        }

        self.width = width;
        self.height = height;
        self.stats.visible_resize_count += 1;
        // Content outside the previous visible area is stale: repaint all visible pixels.
        self.force_full = true;
        unsafe {
            if set_clip_rect(self.dcomp_clip_visual, width, height) < 0 {
                return Err("SetClip failed on clip visual");
            }
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
        unsafe {
            // ResizeBuffers is slot 13 on IDXGISwapChain
            let sc_vtbl = *(self.swap_chain as *mut *mut usize);
            type PfnResizeBuffers = unsafe extern "system" fn(
                this: *mut c_void,
                bufferCount: u32,
                width: u32,
                height: u32,
                newFormat: u32,
                swapChainFlags: u32,
            ) -> HRESULT;
            let resize_fn: PfnResizeBuffers = std::mem::transmute(*sc_vtbl.add(13));
            let hr = resize_fn(
                self.swap_chain,
                2,
                alloc_w,
                alloc_h,
                DXGI_FORMAT_B8G8R8A8_UNORM,
                DXGI_SWAP_CHAIN_FLAG_GDI_COMPATIBLE,
            );
            if hr < 0 {
                let u_hr = hr as u32;
                if u_hr == 0x887A0005 || u_hr == 0x887A0007 {
                    return Err("DXGI_ERROR_DEVICE_LOST");
                }
                return Err("ResizeBuffers failed on swap chain");
            }
            self.stats.resize_buffers_count += 1;

            // Resize staging DIB
            SelectObject(self.staging_dc, self.staging_old_bitmap);
            DeleteObject(self.staging_bitmap);

            let mut bmi: BITMAPINFO = std::mem::zeroed();
            bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            bmi.bmiHeader.biWidth = alloc_w.max(1) as i32;
            bmi.bmiHeader.biHeight = -(alloc_h.max(1) as i32);
            bmi.bmiHeader.biPlanes = 1;
            bmi.bmiHeader.biBitCount = 32;
            bmi.bmiHeader.biCompression = BI_RGB;

            let mut bits: *mut c_void = ptr::null_mut();
            let new_bmp = CreateDIBSection(
                self.staging_dc,
                &bmi,
                DIB_RGB_COLORS,
                &mut bits,
                ptr::null_mut(),
                0,
            );
            self.staging_old_bitmap = SelectObject(self.staging_dc, new_bmp);
            self.staging_bitmap = new_bmp;
            self.staging_bits = bits as *mut u8;
            self.staging_w = alloc_w;
            self.staging_h = alloc_h;
            self.alloc_w = alloc_w;
            self.alloc_h = alloc_h;
            // Fresh buffers + staging: the next present must repaint everything visible.
            self.force_full = true;
        }

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
        #[cfg(any(test, feature = "test-hooks"))]
        if self.dcomp_device.is_null() {
            // Mock surface for unit testing: if present failure is not requested, mock success
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
        let dirty = if self.force_full {
            full_window_rect
        } else {
            dirty
        };
        let clipped = full_window_rect.intersected(&dirty);
        if clipped.is_empty() {
            return Ok(());
        }

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
                    std::slice::from_raw_parts_mut(self.staging_bits.add(dst_offset), copy_bytes);

                convert_rgba_to_staging_bgra(src_row, dst_row, opacity);
            }
            self.staged_opacity = opacity;

            // 2. Query back buffer IDXGISurface1: GetBuffer is slot 9 on IDXGISwapChain
            let sc_vtbl = *(self.swap_chain as *mut *mut usize);
            type PfnGetBuffer = unsafe extern "system" fn(
                this: *mut c_void,
                buffer: u32,
                riid: *const windows_sys::core::GUID,
                pp_surface: *mut *mut c_void,
            ) -> HRESULT;
            let get_buffer_fn: PfnGetBuffer = std::mem::transmute(*sc_vtbl.add(9));
            let iid_idxgi_surface1 = windows_sys::core::GUID {
                data1: 0x4AE63092,
                data2: 0x6327,
                data3: 0x4827,
                data4: [0x88, 0x2F, 0x20, 0x00, 0x57, 0xA3, 0xD0, 0x33],
            };
            let mut dxgi_surface1: *mut c_void = ptr::null_mut();
            let hr = get_buffer_fn(self.swap_chain, 0, &iid_idxgi_surface1, &mut dxgi_surface1);
            if hr < 0 || dxgi_surface1.is_null() {
                return Err("IDXGISwapChain::GetBuffer(0, IDXGISurface1) failed");
            }

            // 3. Acquire HDC on DXGI Surface: GetDC is slot 11 on IDXGISurface1
            let surf_vtbl = *(dxgi_surface1 as *mut *mut usize);
            type PfnGetDC = unsafe extern "system" fn(
                this: *mut c_void,
                discard: i32,
                phdc: *mut HDC,
            ) -> HRESULT;
            let get_dc_fn: PfnGetDC = std::mem::transmute(*surf_vtbl.add(11));
            let mut dxgi_dc: HDC = ptr::null_mut();
            let hr_dc = get_dc_fn(dxgi_surface1, 0, &mut dxgi_dc);
            if hr_dc < 0 || dxgi_dc.is_null() {
                let release_fn: unsafe extern "system" fn(*mut c_void) -> u32 =
                    std::mem::transmute(**(dxgi_surface1 as *mut *mut usize).add(2));
                release_fn(dxgi_surface1);
                return Err("IDXGISurface1::GetDC failed");
            }

            // 4. BitBlt differential dirty rect from staging DIB to back buffer
            windows_sys::Win32::Graphics::Gdi::BitBlt(
                dxgi_dc,
                clipped.x,
                clipped.y,
                clipped.width,
                clipped.height,
                self.staging_dc,
                clipped.x,
                clipped.y,
                windows_sys::Win32::Graphics::Gdi::SRCCOPY,
            );
            // 5. Release DC: ReleaseDC is slot 12 on IDXGISurface1
            type PfnReleaseDC =
                unsafe extern "system" fn(this: *mut c_void, p_dirty_rect: *const RECT) -> HRESULT;
            let rel_dc_fn: PfnReleaseDC = std::mem::transmute(*surf_vtbl.add(12));
            let dirty_gdi_rect = RECT {
                left: clipped.x,
                top: clipped.y,
                right: clipped.x + clipped.width,
                bottom: clipped.y + clipped.height,
            };
            rel_dc_fn(dxgi_surface1, &dirty_gdi_rect);

            // Release dxgi_surface1 COM reference
            let release_fn: unsafe extern "system" fn(*mut c_void) -> u32 =
                std::mem::transmute(*surf_vtbl.add(2));
            release_fn(dxgi_surface1);

            // 6. Present1 with pDirtyRects (slot 22 on IDXGISwapChain1)
            type PfnPresent1 = unsafe extern "system" fn(
                this: *mut c_void,
                sync_interval: u32,
                present_flags: u32,
                p_present_parameters: *const DXGI_PRESENT_PARAMETERS,
            ) -> HRESULT;
            let present1_fn: PfnPresent1 = std::mem::transmute(*sc_vtbl.add(22));

            let mut dxgi_dirty_rect = RECT {
                left: clipped.x,
                top: clipped.y,
                right: clipped.x + clipped.width,
                bottom: clipped.y + clipped.height,
            };
            let present_params = DXGI_PRESENT_PARAMETERS {
                DirtyRectsCount: 1,
                pDirtyRects: &mut dxgi_dirty_rect,
                pScrollRect: ptr::null_mut(),
                pScrollOffset: ptr::null_mut(),
            };

            let hr = present1_fn(self.swap_chain, 0, 0, &present_params);
            if hr < 0 {
                return Err("IDXGISwapChain1::Present1 failed");
            }
            self.force_full = false;
            self.stats.present_count += 1;
            crate::resize_trace::record(
                crate::resize_trace::TraceKind::Present1,
                self.hwnd as usize,
                (0, 0),
                (self.width, self.height),
            );

            // 7. Commit DirectComposition device (also publishes any clip change)
            let dev_vtbl = *(self.dcomp_device as *mut *mut usize);
            type PfnCommit = unsafe extern "system" fn(this: *mut c_void) -> HRESULT;
            let commit_fn: PfnCommit = std::mem::transmute(*dev_vtbl.add(3));
            let hr_commit = commit_fn(self.dcomp_device);
            if hr_commit < 0 {
                return Err("IDCompositionDevice::Commit failed");
            }
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

impl Drop for DCompSurface {
    fn drop(&mut self) {
        unsafe {
            if !self.staging_dc.is_null() {
                SelectObject(self.staging_dc, self.staging_old_bitmap);
                DeleteObject(self.staging_bitmap);
                DeleteDC(self.staging_dc);
            }
            com_release(self.dcomp_clip_visual);
            com_release(self.dcomp_visual);
            com_release(self.dcomp_target);
            com_release(self.dcomp_device);
            com_release(self.swap_chain);
            com_release(self.d3d_context);
            com_release(self.d3d_device);

            if !self.dcomp_mod.is_null() {
                FreeLibrary(self.dcomp_mod);
            }
            if !self.dxgi_mod.is_null() {
                FreeLibrary(self.dxgi_mod);
            }
            if !self.d3d11_mod.is_null() {
                FreeLibrary(self.d3d11_mod);
            }
        }
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
