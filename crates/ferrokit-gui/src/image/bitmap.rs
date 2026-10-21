// SPDX-License-Identifier: LGPL-3.0-only
//! 1-bit monochrome bitmap surface matching Qt 6 `QBitmap`.

use std::ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign, BitXor, BitXorAssign, Not};

use crate::geometry::primitives::Size;
use crate::image::image::Image;
use crate::image::image_format::ImageFormat;

/// 1-bit monochrome bitmap used for masks, cursors, and clipping boundaries matching `QBitmap`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bitmap {
    width: u32,
    height: u32,
    data: Vec<u8>,
    bytes_per_line: usize,
}

impl Default for Bitmap {
    fn default() -> Self {
        Self::null()
    }
}

impl Bitmap {
    /// Creates a null (empty) bitmap.
    pub const fn null() -> Self {
        Self {
            width: 0,
            height: 0,
            data: Vec::new(),
            bytes_per_line: 0,
        }
    }

    /// Creates a new monochrome bitmap with specified dimensions.
    pub fn new(width: u32, height: u32) -> Self {
        if width == 0 || height == 0 {
            return Self::null();
        }
        let bytes_per_line = (width as usize).div_ceil(8);
        let total = bytes_per_line * (height as usize);
        Self {
            width,
            height,
            data: vec![0u8; total],
            bytes_per_line,
        }
    }

    /// Creates a monochrome bitmap initialized with all bits set (1) or cleared (0).
    pub fn with_fill(width: u32, height: u32, fill_bit: bool) -> Self {
        let mut bm = Self::new(width, height);
        if fill_bit {
            bm.data.fill(0xFF);
        }
        bm
    }

    /// Converts from a canonical `Image` by thresholding (luminance >= 128 is 1, else 0).
    pub fn from_image(image: &Image) -> Self {
        if image.is_null() {
            return Self::null();
        }

        let mut bm = Self::new(image.width(), image.height());
        for y in 0..image.height() {
            for x in 0..image.width() {
                if let Some(c) = image.pixel_color(x, y) {
                    let u = c.to_color_u8();
                    let lum =
                        (u.red() as u32 * 299 + u.green() as u32 * 587 + u.blue() as u32 * 114)
                            / 1000;
                    let bit = lum >= 128 && u.alpha() >= 128;
                    bm.set_bit(x, y, bit);
                }
            }
        }
        bm
    }

    /// Converts this monochrome bitmap into a canonical `Image` of `ImageFormat::Mono`.
    pub fn to_image(&self) -> Image {
        if self.is_null() {
            return Image::null();
        }
        let mut img = Image::new(self.width, self.height, ImageFormat::Mono);
        for y in 0..self.height {
            let start = y as usize * self.bytes_per_line;
            let end = start + self.bytes_per_line;
            if let (Some(src), Some(dst)) = (
                self.data.get(start..end),
                img.scan_line_mut(y),
            ) {
                dst.copy_from_slice(src);
            }
        }
        img
    }

    /// Returns `true` if this bitmap is empty.
    #[inline]
    pub const fn is_null(&self) -> bool {
        self.width == 0 || self.height == 0
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

    /// Dimensions as `Size`.
    #[inline]
    pub fn size(&self) -> Size {
        Size::new(self.width as i32, self.height as i32)
    }

    /// Clears all bits to 0.
    pub fn clear(&mut self) {
        self.data.fill(0);
    }

    /// Sets all bits to 1.
    pub fn set_all(&mut self) {
        self.data.fill(0xFF);
    }

    /// Gets the bit value at `(x, y)`.
    #[inline]
    pub fn bit(&self, x: u32, y: u32) -> bool {
        if x >= self.width || y >= self.height {
            return false;
        }
        let byte_idx = (y as usize) * self.bytes_per_line + (x as usize / 8);
        let bit_idx = 7 - (x % 8);
        (self.data[byte_idx] & (1 << bit_idx)) != 0
    }

    /// Sets the bit value at `(x, y)`.
    #[inline]
    pub fn set_bit(&mut self, x: u32, y: u32, value: bool) {
        if x >= self.width || y >= self.height {
            return;
        }
        let byte_idx = (y as usize) * self.bytes_per_line + (x as usize / 8);
        let bit_idx = 7 - (x % 8);
        if value {
            self.data[byte_idx] |= 1 << bit_idx;
        } else {
            self.data[byte_idx] &= !(1 << bit_idx);
        }
    }

    /// Toggles the bit value at `(x, y)`.
    #[inline]
    pub fn toggle_bit(&mut self, x: u32, y: u32) {
        let current = self.bit(x, y);
        self.set_bit(x, y, !current);
    }
}

// =============================================================================
// Bitwise operations matching Qt QBitmap
// =============================================================================

impl BitAnd for Bitmap {
    type Output = Self;

    fn bitand(mut self, rhs: Self) -> Self::Output {
        self &= rhs;
        self
    }
}

impl BitAndAssign for Bitmap {
    fn bitand_assign(&mut self, rhs: Self) {
        let len = self.data.len().min(rhs.data.len());
        for i in 0..len {
            self.data[i] &= rhs.data[i];
        }
    }
}

impl BitOr for Bitmap {
    type Output = Self;

    fn bitor(mut self, rhs: Self) -> Self::Output {
        self |= rhs;
        self
    }
}

impl BitOrAssign for Bitmap {
    fn bitor_assign(&mut self, rhs: Self) {
        let len = self.data.len().min(rhs.data.len());
        for i in 0..len {
            self.data[i] |= rhs.data[i];
        }
    }
}

impl BitXor for Bitmap {
    type Output = Self;

    fn bitxor(mut self, rhs: Self) -> Self::Output {
        self ^= rhs;
        self
    }
}

impl BitXorAssign for Bitmap {
    fn bitxor_assign(&mut self, rhs: Self) {
        let len = self.data.len().min(rhs.data.len());
        for i in 0..len {
            self.data[i] ^= rhs.data[i];
        }
    }
}

impl Not for Bitmap {
    type Output = Self;

    fn not(mut self) -> Self::Output {
        for b in &mut self.data {
            *b = !*b;
        }
        self
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use tiny_skia::Color;

    #[test]
    fn bitmap_image_preserves_mono_bits() {
        let mut bitmap = Bitmap::new(9, 1);
        bitmap.set_bit(0, 0, true);
        bitmap.set_bit(8, 0, true);
        let image = bitmap.to_image();
        assert_eq!(image.pixel_color(0, 0), Some(Color::WHITE));
        assert_eq!(image.pixel_color(1, 0), Some(Color::BLACK));
        assert_eq!(image.pixel_color(8, 0), Some(Color::WHITE));
    }
}
