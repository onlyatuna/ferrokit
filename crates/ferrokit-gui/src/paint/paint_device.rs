// SPDX-License-Identifier: LGPL-3.0-only
//! Paint device and canvas surface abstractions.
//!
//! Aligned with Qt's `QPaintDevice` abstraction, representing surfaces that can be painted upon.

use crate::geometry::primitives::Size;

/// Paint device abstraction (`QPaintDevice`).
///
/// Defines the common interface for paintable surfaces, supporting HiDPI scaling and raster targets.
pub trait PaintDevice {
    /// Physical width in pixels.
    fn physical_width(&self) -> u32;

    /// Physical height in pixels.
    fn physical_height(&self) -> u32;

    /// Device pixel ratio (e.g., 1.0 for 100%, 2.0 for 200%).
    fn device_pixel_ratio(&self) -> f32 {
        1.0
    }

    /// Logical width in points = physical_width / DPR.
    fn width(&self) -> f32 {
        self.physical_width() as f32 / self.device_pixel_ratio()
    }

    /// Logical height in points = physical_height / DPR.
    fn height(&self) -> f32 {
        self.physical_height() as f32 / self.device_pixel_ratio()
    }

    /// Logical size in points.
    fn size(&self) -> Size {
        Size {
            width: self.width().round() as i32,
            height: self.height().round() as i32,
        }
    }

    /// Provides a mutable pixmap view for raster rendering (premultiplied RGBA).
    fn as_pixmap_mut(&mut self) -> tiny_skia::PixmapMut<'_>;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockSurface {
        pixels: Vec<u8>,
        width: u32,
        height: u32,
        dpr: f32,
    }

    impl MockSurface {
        fn new(width: u32, height: u32, dpr: f32) -> Self {
            let len = (width * height * 4) as usize;
            Self {
                pixels: vec![0; len],
                width,
                height,
                dpr,
            }
        }
    }

    impl PaintDevice for MockSurface {
        fn physical_width(&self) -> u32 {
            self.width
        }

        fn physical_height(&self) -> u32 {
            self.height
        }

        fn device_pixel_ratio(&self) -> f32 {
            self.dpr
        }

        fn as_pixmap_mut(&mut self) -> tiny_skia::PixmapMut<'_> {
            tiny_skia::PixmapMut::from_bytes(&mut self.pixels, self.width, self.height)
                .expect("valid pixmap dimensions")
        }
    }

    #[test]
    fn test_paint_device_logical_metrics_and_dpr() {
        let mut surface = MockSurface::new(800, 600, 2.0);

        assert_eq!(surface.physical_width(), 800);
        assert_eq!(surface.physical_height(), 600);
        assert_eq!(surface.device_pixel_ratio(), 2.0);

        assert_eq!(surface.width(), 400.0);
        assert_eq!(surface.height(), 300.0);
        assert_eq!(surface.size(), Size::new(400, 300));

        let pixmap = surface.as_pixmap_mut();
        assert_eq!(pixmap.width(), 800);
        assert_eq!(pixmap.height(), 600);
    }
}
