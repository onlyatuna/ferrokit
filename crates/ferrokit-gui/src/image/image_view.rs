// SPDX-License-Identifier: LGPL-3.0-only
//! Zero-copy pixel buffer views matching `QImageView` / `ImageSpan`.

use crate::geometry::primitives::Rect;
use crate::image::image_format::ImageFormat;

/// Non-owning, read-only view of a 2D pixel buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageView<'a> {
    data: &'a [u8],
    width: u32,
    height: u32,
    bytes_per_line: usize,
    format: ImageFormat,
}

impl<'a> ImageView<'a> {
    /// Creates a new `ImageView` from raw pixel slice, dimensions, stride, and format.
    pub fn new(
        data: &'a [u8],
        width: u32,
        height: u32,
        bytes_per_line: usize,
        format: ImageFormat,
    ) -> Option<Self> {
        if width == 0 || height == 0 || format == ImageFormat::Invalid {
            return None;
        }

        if bytes_per_line < format.bytes_per_line(width, 1) {
            return None;
        }
        let required_len = bytes_per_line.checked_mul(height as usize)?;
        if data.len() < required_len {
            return None;
        }

        Some(Self {
            data,
            width,
            height,
            bytes_per_line,
            format,
        })
    }

    /// Width in pixels.
    #[inline]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Height in pixels.
    #[inline]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// Pixel format.
    #[inline]
    pub const fn format(&self) -> ImageFormat {
        self.format
    }

    /// Number of bytes per scanline.
    #[inline]
    pub const fn bytes_per_line(&self) -> usize {
        self.bytes_per_line
    }

    /// Underlying raw byte slice.
    #[inline]
    pub const fn data(&self) -> &'a [u8] {
        self.data
    }

    /// Returns the byte slice for the scanline at index `y`.
    #[inline]
    pub fn scan_line(&self, y: u32) -> Option<&'a [u8]> {
        if y >= self.height {
            return None;
        }
        let start = (y as usize) * self.bytes_per_line;
        let end = start + self.format.bytes_per_line(self.width, 1);
        self.data.get(start..end)
    }

    /// Returns the pixel bytes at coordinate `(x, y)`.
    #[inline]
    pub fn pixel_bytes(&self, x: u32, y: u32) -> Option<&'a [u8]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let bpp = self.format.bytes_per_pixel();
        let start = (y as usize) * self.bytes_per_line + (x as usize) * bpp;
        let end = start + bpp;
        self.data.get(start..end)
    }

    /// Creates a sub-view within the specified sub-rectangle.
    pub fn sub_view(&self, rect: Rect) -> Option<ImageView<'a>> {
        if rect.x < 0 || rect.y < 0 || rect.width <= 0 || rect.height <= 0 {
            return None;
        }
        let rx = rect.x as u32;
        let ry = rect.y as u32;
        let rw = rect.width as u32;
        let rh = rect.height as u32;

        if rx + rw > self.width || ry + rh > self.height {
            return None;
        }

        let bits = self.format.bits_per_pixel();
        // A view cannot carry a sub-byte start offset, so reject unaligned columns.
        if (rx as usize).checked_mul(bits)? % 8 != 0 {
            return None;
        }
        let offset = (ry as usize)
            .checked_mul(self.bytes_per_line)?
            .checked_add((rx as usize) * bits / 8)?;
        let sub_data = self.data.get(offset..)?;

        Some(ImageView {
            data: sub_data,
            width: rw,
            height: rh,
            bytes_per_line: self.bytes_per_line,
            format: self.format,
        })
    }
}

/// Non-owning, mutable view of a 2D pixel buffer.
#[derive(Debug)]
pub struct ImageViewMut<'a> {
    data: &'a mut [u8],
    width: u32,
    height: u32,
    bytes_per_line: usize,
    format: ImageFormat,
}

impl<'a> ImageViewMut<'a> {
    /// Creates a new `ImageViewMut` from mutable raw pixel slice, dimensions, stride, and format.
    pub fn new(
        data: &'a mut [u8],
        width: u32,
        height: u32,
        bytes_per_line: usize,
        format: ImageFormat,
    ) -> Option<Self> {
        if width == 0 || height == 0 || format == ImageFormat::Invalid {
            return None;
        }

        if bytes_per_line < format.bytes_per_line(width, 1) {
            return None;
        }
        let required_len = bytes_per_line.checked_mul(height as usize)?;
        if data.len() < required_len {
            return None;
        }

        Some(Self {
            data,
            width,
            height,
            bytes_per_line,
            format,
        })
    }

    /// Width in pixels.
    #[inline]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Height in pixels.
    #[inline]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// Pixel format.
    #[inline]
    pub const fn format(&self) -> ImageFormat {
        self.format
    }

    /// Number of bytes per scanline.
    #[inline]
    pub const fn bytes_per_line(&self) -> usize {
        self.bytes_per_line
    }

    /// Underlying raw byte slice.
    #[inline]
    pub fn data(&self) -> &[u8] {
        self.data
    }

    /// Underlying mutable raw byte slice.
    #[inline]
    pub fn data_mut(&mut self) -> &mut [u8] {
        self.data
    }

    /// Returns the byte slice for the scanline at index `y`.
    #[inline]
    pub fn scan_line(&self, y: u32) -> Option<&[u8]> {
        if y >= self.height {
            return None;
        }
        let start = (y as usize) * self.bytes_per_line;
        let end = start + self.format.bytes_per_line(self.width, 1);
        self.data.get(start..end)
    }

    /// Returns the mutable byte slice for the scanline at index `y`.
    #[inline]
    pub fn scan_line_mut(&mut self, y: u32) -> Option<&mut [u8]> {
        if y >= self.height {
            return None;
        }
        let start = (y as usize) * self.bytes_per_line;
        let end = start + self.format.bytes_per_line(self.width, 1);
        self.data.get_mut(start..end)
    }

    /// Reborrows as an immutable `ImageView`.
    #[inline]
    pub fn as_view(&self) -> ImageView<'_> {
        ImageView {
            data: self.data,
            width: self.width,
            height: self.height,
            bytes_per_line: self.bytes_per_line,
            format: self.format,
        }
    }
}

#[cfg(test)]
mod stride_tests {
    use super::*;

    #[test]
    fn view_mut_rejects_stride_smaller_than_a_row() {
        let mut buf = [0u8; 64];
        // 4 px * 4 bytes = 16 bytes per row minimum.
        assert!(ImageViewMut::new(&mut buf, 4, 2, 15, ImageFormat::Rgba8888).is_none());
        assert!(ImageViewMut::new(&mut buf, 4, 2, 16, ImageFormat::Rgba8888).is_some());
        assert!(ImageView::new(&buf, 4, 2, 15, ImageFormat::Rgba8888).is_none());
    }

    #[test]
    fn view_mut_rejects_short_buffers() {
        let mut buf = [0u8; 31];
        assert!(ImageViewMut::new(&mut buf, 4, 2, 16, ImageFormat::Rgba8888).is_none());
        assert!(ImageViewMut::new(&mut buf, 4, 2, usize::MAX, ImageFormat::Rgba8888).is_none());
    }
}
