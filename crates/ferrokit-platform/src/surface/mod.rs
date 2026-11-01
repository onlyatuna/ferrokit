// SPDX-License-Identifier: LGPL-3.0-only
use ferrokit_gui::geometry::Rect;
use ferrokit_gui::paint::Pixmap;

pub trait PlatformSurface {
    fn width(&self) -> u32;
    fn height(&self) -> u32;
    fn resize(&mut self, width: u32, height: u32) -> Result<(), &'static str>;
    fn present(&mut self, pixmap: &mut Pixmap, opacity: f32) -> Result<(), &'static str>;
    fn present_dirty(
        &mut self,
        pixmap: &mut Pixmap,
        opacity: f32,
        dirty: Rect,
    ) -> Result<(), &'static str>;
}

#[cfg(windows)]
pub mod dcomp;
pub mod macos;
pub mod wayland;
#[cfg(windows)]
pub mod win32;
pub mod x11;

#[cfg(windows)]
pub use dcomp::DCompSurface;
#[cfg(windows)]
pub use win32::Win32LayeredSurface;
#[cfg(windows)]
pub type LayeredSurface = Win32LayeredSurface;

#[cfg(windows)]
pub enum WindowsSurface {
    DirectComposition(DCompSurface),
    Layered(Win32LayeredSurface),
}

#[cfg(windows)]
impl WindowsSurface {
    pub fn create(
        hwnd: windows_sys::Win32::Foundation::HWND,
        width: u32,
        height: u32,
    ) -> Result<Self, &'static str> {
        if crate::window::is_dcomp_enabled()
            && let Ok(dcomp) = DCompSurface::new(hwnd, width, height)
        {
            return Ok(Self::DirectComposition(dcomp));
        }
        let layered = Win32LayeredSurface::new(hwnd, width, height)?;
        Ok(Self::Layered(layered))
    }
}

#[cfg(windows)]
impl PlatformSurface for WindowsSurface {
    fn width(&self) -> u32 {
        match self {
            Self::DirectComposition(s) => s.width(),
            Self::Layered(s) => s.width(),
        }
    }

    fn height(&self) -> u32 {
        match self {
            Self::DirectComposition(s) => s.height(),
            Self::Layered(s) => s.height(),
        }
    }

    fn resize(&mut self, width: u32, height: u32) -> Result<(), &'static str> {
        match self {
            Self::DirectComposition(s) => s.resize(width, height),
            Self::Layered(s) => s.resize(width, height),
        }
    }

    fn present(&mut self, pixmap: &mut Pixmap, opacity: f32) -> Result<(), &'static str> {
        match self {
            Self::DirectComposition(s) => s.present(pixmap, opacity),
            Self::Layered(s) => s.present(pixmap, opacity),
        }
    }

    fn present_dirty(
        &mut self,
        pixmap: &mut Pixmap,
        opacity: f32,
        dirty: Rect,
    ) -> Result<(), &'static str> {
        match self {
            Self::DirectComposition(s) => s.present_dirty(pixmap, opacity, dirty),
            Self::Layered(s) => s.present_dirty(pixmap, opacity, dirty),
        }
    }
}
pub use macos::CocoaLayerSurface;
pub use wayland::WaylandShmSurface;
pub use x11::X11ShmSurface;
