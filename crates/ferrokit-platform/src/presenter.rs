// SPDX-License-Identifier: LGPL-3.0-only
//! Unified Surface Presentation Abstraction
//!
//! Decouples GUI window logic, layout, and event handling from platform-specific
//! presentation mechanisms (such as Win32 `UpdateLayeredWindowIndirect`, GDI `BitBlt`,
//! DirectComposition, or Wayland/Cocoa surface presentation).

#[allow(unused_imports)]
use ferrokit_gui::geometry::primitives::Rect;
use ferrokit_gui::geometry::Region;
use ferrokit_gui::paint::Pixmap;

/// Unified abstraction for presenting surface pixels to the native window system.
pub trait SurfacePresenter {
    /// Presents the specified dirty region of the surface to the native window.
    fn present(&mut self, surface: &Pixmap, dirty: &Region) -> Result<(), &'static str>;

    /// Notifies the presenter that the window backing surface size has changed.
    fn resize(&mut self, width: u32, height: u32) -> Result<(), &'static str> {
        let _ = (width, height);
        Ok(())
    }

    /// Sets the presentation opacity (alpha multiplier in [0.0, 1.0]).
    fn set_opacity(&mut self, _opacity: f32) {}

    /// Tells the presenter whether a native interactive sizing loop is in progress, so it may
    /// trade memory for fewer reallocations. Default: ignored.
    fn set_interactive_resize(&mut self, _active: bool) {}
}

#[cfg(windows)]
pub mod win32 {
    use super::*;
    use crate::win32_handles::{create_dib_section, DibTarget, MemDc, WindowDc};
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::Graphics::Gdi::{BitBlt, SRCCOPY};
    /// Presenter for standard Win32 windows using GDI `GetDC` + `BitBlt`.
    ///
    /// Mirrors Qt's `QWindowsBackingStore::flush` non-layered branch:
    /// standard windows with native frames or standard styles do NOT use layered window APIs.
    pub struct Win32DcPresenter {
        hwnd: HWND,
        width: u32,
        height: u32,
        /// The memory DC with the DIB section selected into it.
        target: DibTarget,
    }

    // Deliberately neither `Send` nor `Sync`: owns GDI handles and a DIB-section pointer that
    // are used with the window's device context on the window's thread only.

    impl Win32DcPresenter {
        pub fn new(hwnd: HWND, width: u32, height: u32) -> Result<Self, &'static str> {
            // Only needed to make the memory DC compatible; never kept (see `WindowDc`).
            let screen = WindowDc::screen();
            if screen.as_raw().is_null() {
                return Err("GetDC failed on screen");
            }
            let Some(mem_dc) = MemDc::create_compatible(screen.as_raw()) else {
                return Err("CreateCompatibleDC failed");
            };
            // A failed DIB drops `mem_dc` (DeleteDC) on the way out.
            let Some((bitmap, bits)) = create_dib_section(mem_dc.as_raw(), width, height) else {
                return Err("CreateDIBSection failed");
            };

            Ok(Self {
                hwnd,
                width: width.max(1),
                height: height.max(1),
                target: DibTarget::select(mem_dc, bitmap, bits),
            })
        }

        pub fn width(&self) -> u32 {
            self.width
        }

        pub fn height(&self) -> u32 {
            self.height
        }
    }

    impl SurfacePresenter for Win32DcPresenter {
        fn resize(&mut self, width: u32, height: u32) -> Result<(), &'static str> {
            if width == 0 || height == 0 {
                return Err("Width and height must be > 0");
            }
            if self.width == width && self.height == height {
                return Ok(());
            }

            // Restores the previous object, deletes the old DIB, then selects the new one; a
            // failed allocation leaves everything as it was.
            let Some((new_bitmap, bits)) = create_dib_section(self.target.dc(), width, height)
            else {
                return Err("Failed to resize DIBSection for Win32DcPresenter");
            };
            self.target.replace_deselecting(new_bitmap, bits);
            self.width = width;
            self.height = height;
            Ok(())
        }

        fn present(&mut self, surface: &Pixmap, dirty: &Region) -> Result<(), &'static str> {
            if dirty.is_empty() {
                return Ok(());
            }
            let p_width = surface.physical_width();
            let p_height = surface.physical_height();
            if self.width != p_width || self.height != p_height {
                self.resize(p_width, p_height)?;
            }

            let full = Rect::new(0, 0, self.width as i32, self.height as i32);
            let br = dirty.bounding_rect().intersected(&full);
            if br.is_empty() {
                return Ok(());
            }

            let stride = (self.width * 4) as usize;
            let dirty_x = br.x as usize;
            let dirty_w = br.width as usize;
            let src_data = surface.data();

            unsafe {
                for y in br.y..(br.y + br.height) {
                    let y = y as usize;
                    let row_offset = y * stride + dirty_x * 4;
                    let copy_bytes = dirty_w * 4;
                    let src_row =
                        std::slice::from_raw_parts(src_data.as_ptr().add(row_offset), copy_bytes);
                    let dst_row = std::slice::from_raw_parts_mut(
                        self.target.bits().add(row_offset),
                        copy_bytes,
                    );

                    let (src_chunks, _) = src_row.as_chunks::<4>();
                    let (dst_chunks, _) = dst_row.as_chunks_mut::<4>();
                    for (src, dst) in src_chunks.iter().zip(dst_chunks.iter_mut()) {
                        dst[0] = src[2]; // B
                        dst[1] = src[1]; // G
                        dst[2] = src[0]; // R
                        dst[3] = src[3]; // A
                    }
                }

                let window_dc = WindowDc::acquire(self.hwnd);
                let dc = window_dc.as_raw();
                if dc.is_null() {
                    return Err("GetDC failed on hwnd");
                }

                for r in dirty.rects() {
                    let clipped = r.intersected(&full);
                    if !clipped.is_empty() {
                        BitBlt(
                            dc,
                            clipped.x,
                            clipped.y,
                            clipped.width,
                            clipped.height,
                            self.target.dc(),
                            clipped.x,
                            clipped.y,
                            SRCCOPY,
                        );
                    }
                }

                drop(window_dc);
            }

            Ok(())
        }
    }

    /// Presenter for layered windows using `UpdateLayeredWindowIndirect`.
    ///
    /// Mirrors Qt's `QWindowsBackingStore::flush` layered branch:
    /// uses `UPDATELAYEREDWINDOWINFO` with `prcDirty` for true dirty-region presentation.
    pub struct Win32LayeredPresenter {
        surface: crate::surface::win32::Win32LayeredSurface,
        opacity: f32,
    }

    impl Win32LayeredPresenter {
        /// The underlying surface (counters, allocation sizes, policy switches).
        pub fn surface(&self) -> &crate::surface::win32::Win32LayeredSurface {
            &self.surface
        }

        pub fn surface_mut(&mut self) -> &mut crate::surface::win32::Win32LayeredSurface {
            &mut self.surface
        }

        pub fn new(
            hwnd: HWND,
            width: u32,
            height: u32,
            opacity: f32,
        ) -> Result<Self, &'static str> {
            let surface = crate::surface::win32::Win32LayeredSurface::new(hwnd, width, height)?;
            Ok(Self { surface, opacity })
        }

        pub fn set_target_pos(&mut self, pos: Option<windows_sys::Win32::Foundation::POINT>) {
            self.surface.set_target_pos(pos);
        }
    }

    impl SurfacePresenter for Win32LayeredPresenter {
        fn resize(&mut self, width: u32, height: u32) -> Result<(), &'static str> {
            self.surface.resize(width, height)
        }

        fn set_opacity(&mut self, opacity: f32) {
            self.opacity = opacity;
        }

        fn set_interactive_resize(&mut self, active: bool) {
            self.surface.set_interactive_resize(active);
        }

        fn present(&mut self, surface: &Pixmap, dirty: &Region) -> Result<(), &'static str> {
            let br = dirty.bounding_rect();
            self.surface.present_dirty_ref(surface, self.opacity, br)
        }
    }

    /// Windows unified presenter enum supporting Layered (UpdateLayeredWindowIndirect),
    /// Standard (BitBlt), and DirectComposition.
    pub enum WindowsPresenter {
        Layered(Win32LayeredPresenter),
        Dc(Win32DcPresenter),
        DirectComposition(crate::surface::dcomp::DCompSurface),
    }

    impl WindowsPresenter {
        /// The layered presenter, when this is one (instrumentation/tests).
        pub fn as_layered(&self) -> Option<&Win32LayeredPresenter> {
            match self {
                Self::Layered(p) => Some(p),
                _ => None,
            }
        }

        pub fn as_layered_mut(&mut self) -> Option<&mut Win32LayeredPresenter> {
            match self {
                Self::Layered(p) => Some(p),
                _ => None,
            }
        }

        pub fn as_dcomp_mut(&mut self) -> Option<&mut crate::surface::dcomp::DCompSurface> {
            match self {
                Self::DirectComposition(p) => Some(p),
                _ => None,
            }
        }
    }

    impl SurfacePresenter for WindowsPresenter {
        fn resize(&mut self, width: u32, height: u32) -> Result<(), &'static str> {
            match self {
                Self::Layered(p) => p.resize(width, height),
                Self::Dc(p) => p.resize(width, height),
                Self::DirectComposition(p) => p.resize(width, height),
            }
        }

        fn set_opacity(&mut self, opacity: f32) {
            match self {
                Self::Layered(p) => p.set_opacity(opacity),
                // A standard window's opacity is the window's own layered attribute, not a blend.
                Self::Dc(_) => {}
                Self::DirectComposition(p) => p.set_opacity(opacity),
            }
        }

        fn set_interactive_resize(&mut self, active: bool) {
            match self {
                Self::Layered(p) => p.set_interactive_resize(active),
                Self::Dc(_) | Self::DirectComposition(_) => {}
            }
        }

        fn present(&mut self, surface: &Pixmap, dirty: &Region) -> Result<(), &'static str> {
            match self {
                Self::Layered(p) => p.present(surface, dirty),
                Self::Dc(p) => p.present(surface, dirty),
                Self::DirectComposition(p) => {
                    let br = dirty.bounding_rect();
                    p.present_dirty_ref(surface, p.opacity(), br)
                }
            }
        }
    }
}

#[cfg(windows)]
pub use win32::{Win32DcPresenter, Win32LayeredPresenter, WindowsPresenter};
