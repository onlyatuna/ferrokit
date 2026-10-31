// SPDX-License-Identifier: LGPL-3.0-only
//! In-memory offscreen pixmap surface (`QPixmap` / `QImage` equivalent).
//!
//! Provides pixel storage and offscreen rasterization backed by `tiny_skia::Pixmap`.

use crate::geometry::primitives::Rect;
use crate::paint::paint_device::PaintDevice;
use std::path::Path;

/// Offscreen raster surface supporting HiDPI scaling and pixel buffer operations.
#[derive(Clone, Debug)]
pub struct Pixmap {
    pixmap: tiny_skia::Pixmap,
    dpr: f32,
}

impl Pixmap {
    /// Creates a pixmap with the specified physical dimensions and default DPR of 1.0.
    pub fn new(width: u32, height: u32) -> Option<Self> {
        Self::with_dpr(width, height, 1.0)
    }

    /// Creates a pixmap with DPR support.
    pub fn with_dpr(width: u32, height: u32, dpr: f32) -> Option<Self> {
        let pixmap = tiny_skia::Pixmap::new(width.max(1), height.max(1))?;
        Some(Self { pixmap, dpr })
    }

    /// Resizes the pixmap buffer with the current DPR if dimensions differ.
    ///
    /// Returns `true` if a reallocation occurred, or `false` if the existing buffer
    /// already matches the requested dimensions.
    pub fn resize(&mut self, width: u32, height: u32) -> bool {
        self.resize_with_dpr(width, height, self.dpr)
    }

    /// Resizes the pixmap buffer with the specified DPR if dimensions or DPR differ.
    ///
    /// Returns `true` if a reallocation occurred, or `false` if the existing buffer
    /// already matches the requested physical dimensions and DPR.
    pub fn resize_with_dpr(&mut self, width: u32, height: u32, dpr: f32) -> bool {
        let width = width.max(1);
        let height = height.max(1);
        if self.physical_width() == width
            && self.physical_height() == height
            && (self.dpr - dpr).abs() < 0.001
        {
            return false;
        }
        if let Some(new_pixmap) = Self::with_dpr(width, height, dpr) {
            *self = new_pixmap;
            true
        } else {
            false
        }
    }

    /// Physical width in pixels.
    #[inline]
    pub fn physical_width(&self) -> u32 {
        self.pixmap.width()
    }

    /// Physical height in pixels.
    #[inline]
    pub fn physical_height(&self) -> u32 {
        self.pixmap.height()
    }

    /// Logical width considering DPR.
    #[inline]
    pub fn logical_width(&self) -> f32 {
        self.pixmap.width() as f32 / self.dpr
    }

    /// Logical height considering DPR.
    #[inline]
    pub fn logical_height(&self) -> f32 {
        self.pixmap.height() as f32 / self.dpr
    }

    /// Decodes an encoded image (PNG, BMP, PPM or ICO; an ICO yields its largest frame) like
    /// `QPixmap::loadFromData`. `None` when the data cannot be decoded.
    pub fn load_from_data(bytes: &[u8]) -> Option<Self> {
        let image = crate::image::ImageReader::read_from_memory(bytes).ok()?;
        Self::from_image(&image)
    }

    /// Creates a Pixmap from a canonical CPU `Image`.
    pub fn from_image(image: &crate::image::Image) -> Option<Self> {
        if image.is_null() {
            return None;
        }
        let rgba_image = if image.format() == crate::image::ImageFormat::Rgba8888Premultiplied {
            image.clone()
        } else {
            image.converted_to(crate::image::ImageFormat::Rgba8888Premultiplied)
        };
        let mut pm = Self::with_dpr(
            rgba_image.width(),
            rgba_image.height(),
            rgba_image.device_pixel_ratio(),
        )?;
        let row_bytes = rgba_image.width() as usize * 4;
        let stride = rgba_image.bytes_per_line();
        if stride == row_bytes {
            pm.data_mut().copy_from_slice(rgba_image.data());
        } else {
            let src = rgba_image.data();
            for (y, dst_row) in pm.data_mut().chunks_exact_mut(row_bytes).enumerate() {
                dst_row.copy_from_slice(&src[y * stride..y * stride + row_bytes]);
            }
        }
        Some(pm)
    }

    /// Converts this platform Pixmap back into a canonical CPU `Image`.
    pub fn to_image(&self) -> crate::image::Image {
        let mut img = crate::image::Image::with_dpr(
            self.pixmap.width(),
            self.pixmap.height(),
            crate::image::ImageFormat::Rgba8888Premultiplied,
            self.dpr,
        );
        img.data_mut().copy_from_slice(self.data());
        img
    }

    /// Fills the surface with the specified color.
    pub fn fill(&mut self, color: tiny_skia::Color) {
        self.pixmap.fill(color);
    }

    /// Clears a sub-rectangle of the pixmap to transparent (all zeroes).
    /// `rect` is expressed in physical pixel coordinates.
    pub fn clear_rect(&mut self, rect: Rect) {
        if rect.is_empty() || rect.width <= 0 || rect.height <= 0 {
            return;
        }

        // i64 so `x + width` of a huge rect cannot overflow i32.
        let p_w = self.pixmap.width() as i64;
        let p_h = self.pixmap.height() as i64;

        let x0 = (rect.x as i64).clamp(0, p_w) as usize;
        let y0 = (rect.y as i64).clamp(0, p_h) as usize;
        let x1 = (rect.x as i64 + rect.width as i64).clamp(0, p_w) as usize;
        let y1 = (rect.y as i64 + rect.height as i64).clamp(0, p_h) as usize;

        if x1 <= x0 || y1 <= y0 {
            return;
        }

        let stride = (self.pixmap.width() * 4) as usize;
        let byte_start = x0 * 4;
        let byte_len = (x1 - x0) * 4;
        let data = self.pixmap.data_mut();

        for y in y0..y1 {
            let row_offset = y * stride + byte_start;
            data[row_offset..row_offset + byte_len].fill(0);
        }
    }

    /// Clears multiple sub-rectangles of the pixmap to transparent.
    pub fn clear_rects(&mut self, rects: &[Rect]) {
        for &r in rects {
            self.clear_rect(r);
        }
    }

    /// Returns the raw RGBA byte slice.
    pub fn data(&self) -> &[u8] {
        self.pixmap.data()
    }

    /// Returns the mutable RGBA byte slice.
    pub fn data_mut(&mut self) -> &mut [u8] {
        self.pixmap.data_mut()
    }
    /// Gets the color of pixel at (x, y).
    pub fn pixel(&self, x: u32, y: u32) -> Option<tiny_skia::PremultipliedColorU8> {
        self.pixmap.pixel(x, y)
    }

    /// Returns a reference to the underlying tiny_skia::Pixmap.
    pub fn as_tiny_skia(&self) -> &tiny_skia::Pixmap {
        &self.pixmap
    }

    /// Returns a mutable reference to the underlying tiny_skia::Pixmap.
    pub fn as_tiny_skia_mut(&mut self) -> &mut tiny_skia::Pixmap {
        &mut self.pixmap
    }

    /// Converts RGBA pixels to BGRA format in-place for Windows UpdateLayeredWindow.
    #[inline]
    pub fn convert_to_bgra_in_place(&mut self) {
        let data = self.pixmap.data_mut();
        for chunk in data.as_chunks_mut::<4>().0 {
            chunk.swap(0, 2);
        }
    }

    /// Clones and converts the pixel buffer to BGRA byte vector.
    pub fn to_bgra_vec(&self) -> Vec<u8> {
        let mut bytes = self.pixmap.data().to_vec();
        for chunk in bytes.as_chunks_mut::<4>().0 {
            chunk.swap(0, 2);
        }
        bytes
    }

    /// Saves the pixmap content to a PNG file.
    pub fn save_png(&self, path: &Path) -> Result<(), png::EncodingError> {
        self.pixmap.save_png(path)
    }
}

impl PaintDevice for Pixmap {
    fn physical_width(&self) -> u32 {
        self.pixmap.width()
    }

    fn physical_height(&self) -> u32 {
        self.pixmap.height()
    }

    fn device_pixel_ratio(&self) -> f32 {
        self.dpr
    }

    fn as_pixmap_mut(&mut self) -> tiny_skia::PixmapMut<'_> {
        self.pixmap.as_mut()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::primitives::Size;

    #[test]
    fn test_pixmap_creation_and_paint_device_impl() {
        let mut pm = Pixmap::with_dpr(200, 100, 2.0).expect("valid pixmap");

        assert_eq!(pm.physical_width(), 200);
        assert_eq!(pm.physical_height(), 100);
        assert_eq!(pm.device_pixel_ratio(), 2.0);
        assert_eq!(pm.width(), 100.0);
        assert_eq!(pm.height(), 50.0);
        assert_eq!(pm.size(), Size::new(100, 50));

        let white = tiny_skia::Color::WHITE;
        pm.fill(white);

        let data = pm.data();
        assert_eq!(data[0], 255);
        assert_eq!(data[1], 255);
        assert_eq!(data[2], 255);
        assert_eq!(data[3], 255);

        let data_mut = pm.data_mut();
        data_mut[0] = 128;
        assert_eq!(pm.data()[0], 128);

        let pixmap_mut = pm.as_pixmap_mut();
        assert_eq!(pixmap_mut.width(), 200);
        assert_eq!(pixmap_mut.height(), 100);
    }

    #[test]
    fn test_pixmap_bgra_conversion() {
        let mut pm = Pixmap::new(2, 2).expect("valid pixmap");
        let data = pm.data_mut();
        data[0] = 200; // R
        data[1] = 100; // G
        data[2] = 50; // B
        data[3] = 128; // A

        let bgra_vec = pm.to_bgra_vec();
        assert_eq!(bgra_vec[0], 50); // B
        assert_eq!(bgra_vec[1], 100); // G
        assert_eq!(bgra_vec[2], 200); // R
        assert_eq!(bgra_vec[3], 128); // A

        pm.convert_to_bgra_in_place();
        assert_eq!(pm.data()[0], 50); // B
        assert_eq!(pm.data()[1], 100); // G
        assert_eq!(pm.data()[2], 200); // R
        assert_eq!(pm.data()[3], 128); // A

        pm.convert_to_bgra_in_place();
        assert_eq!(pm.data()[0], 200);
        assert_eq!(pm.data()[1], 100);
        assert_eq!(pm.data()[2], 50);
        assert_eq!(pm.data()[3], 128);
    }

    #[test]
    fn test_pixmap_fill_and_raw_bytes() {
        let mut pm = Pixmap::new(2, 2).expect("allocation failed");

        let red = tiny_skia::Color::from_rgba8(255, 0, 0, 255);
        pm.fill(red);

        let data = pm.data();
        assert_eq!(data.len(), 2 * 2 * 4);
        assert_eq!(data[0], 255); // R
        assert_eq!(data[1], 0); // G
        assert_eq!(data[2], 0); // B
        assert_eq!(data[3], 255); // A
    }

    #[test]
    fn test_win32_bgra_conversion() {
        let mut pm = Pixmap::new(1, 1).expect("allocation failed");
        pm.fill(tiny_skia::Color::from_rgba8(255, 0, 0, 255));

        pm.convert_to_bgra_in_place();

        let data = pm.data();
        assert_eq!(data[0], 0); // B
        assert_eq!(data[1], 0); // G
        assert_eq!(data[2], 255); // R
        assert_eq!(data[3], 255); // A
    }

    #[test]
    fn test_pixmap_clear_rect() {
        let mut pm = Pixmap::new(10, 10).expect("allocation failed");
        pm.fill(tiny_skia::Color::from_rgba8(255, 255, 255, 255));

        // Clear a 4x4 rect in the center: (3, 3) to (7, 7)
        pm.clear_rect(Rect::new(3, 3, 4, 4));

        // Check that outside pixels remain white
        assert_eq!(pm.pixel(0, 0).unwrap().red(), 255);
        assert_eq!(pm.pixel(2, 2).unwrap().red(), 255);
        assert_eq!(pm.pixel(7, 7).unwrap().red(), 255);

        // Check that inside pixels are transparent (zeroed)
        assert_eq!(pm.pixel(3, 3).unwrap().alpha(), 0);
        assert_eq!(pm.pixel(3, 3).unwrap().red(), 0);
        assert_eq!(pm.pixel(6, 6).unwrap().alpha(), 0);
        assert_eq!(pm.pixel(6, 6).unwrap().red(), 0);
    }

    #[test]
    fn test_save_png_visual_verification() {
        let mut pm = Pixmap::new(100, 100).unwrap();
        pm.fill(tiny_skia::Color::from_rgba8(0, 128, 255, 200));

        let out_dir = std::path::Path::new("target");
        if !out_dir.exists() {
            let _ = std::fs::create_dir_all(out_dir);
        }
        let out_path = out_dir.join("test_device_output.png");

        pm.save_png(&out_path).expect("failed to save PNG");
        assert!(out_path.exists());
    }
}
