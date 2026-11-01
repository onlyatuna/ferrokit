// SPDX-License-Identifier: LGPL-3.0-only
//! 1-bit monochrome bitmap surface matching Qt 6 `QBitmap`.

use std::ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign, BitXor, BitXorAssign, Not};

use crate::geometry::primitives::Size;
use crate::image::image::Image;
use crate::image::image_format::{ImageFormat, q_gray};

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
    ///
    /// Returns a null bitmap if the dimensions are zero or the buffer size cannot be represented
    /// or allocated. Padding bits (the unused trailing bits of each row) are always 0.
    pub fn new(width: u32, height: u32) -> Self {
        if width == 0 || height == 0 {
            return Self::null();
        }
        let bytes_per_line = (width as usize).div_ceil(8);
        let Some(total) = bytes_per_line.checked_mul(height as usize) else {
            return Self::null();
        };
        let mut data = Vec::new();
        if data.try_reserve_exact(total).is_err() {
            return Self::null();
        }
        data.resize(total, 0u8);
        Self {
            width,
            height,
            data,
            bytes_per_line,
        }
    }

    /// Creates a monochrome bitmap initialized with all bits set (1) or cleared (0).
    pub fn with_fill(width: u32, height: u32, fill_bit: bool) -> Self {
        let mut bm = Self::new(width, height);
        if fill_bit {
            bm.set_all();
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
                    let lum = q_gray(u.red(), u.green(), u.blue());
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
            if let (Some(src), Some(dst)) = (self.data.get(start..end), img.scan_line_mut(y)) {
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

    /// Sets all bits to 1 (padding bits stay 0).
    pub fn set_all(&mut self) {
        self.data.fill(0xFF);
        self.clear_padding();
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

    /// Zeroes the unused trailing bits of every row so `PartialEq` and bitwise operations only
    /// ever see pixel bits.
    fn clear_padding(&mut self) {
        let used = self.width % 8;
        if used == 0 || self.bytes_per_line == 0 {
            return;
        }
        let mask = 0xFFu8 << (8 - used);
        for row in self.data.chunks_exact_mut(self.bytes_per_line) {
            if let Some(last) = row.last_mut() {
                *last &= mask;
            }
        }
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
        self.clear_padding();
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
        self.clear_padding();
    }
}

impl Not for Bitmap {
    type Output = Self;

    fn not(mut self) -> Self::Output {
        for b in &mut self.data {
            *b = !*b;
        }
        self.clear_padding();
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

    #[test]
    fn padding_bits_stay_zero_and_do_not_affect_equality() {
        // Width 5: 3 trailing padding bits per row.
        let filled = Bitmap::with_fill(5, 2, true);
        assert_eq!(filled.data, vec![0b1111_1000, 0b1111_1000]);
        let mut set = Bitmap::new(5, 2);
        for y in 0..2 {
            for x in 0..5 {
                set.set_bit(x, y, true);
            }
        }
        assert_eq!(filled, set);

        let inverted = !Bitmap::new(5, 2);
        assert_eq!(inverted.data, vec![0b1111_1000, 0b1111_1000]);
        assert_eq!(inverted, filled);
        assert_eq!(!inverted, Bitmap::new(5, 2));

        let mut all = Bitmap::new(5, 2);
        all.set_all();
        assert_eq!(all, filled);
        // Out-of-range writes cannot reach the padding bits.
        all.set_bit(5, 0, false);
        all.set_bit(7, 1, true);
        assert_eq!(all, filled);
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "attempts a multi-exabyte allocation, which Miri cannot refuse gracefully"
    )]
    fn huge_dimensions_yield_a_null_bitmap_instead_of_overflowing() {
        assert!(Bitmap::new(u32::MAX, u32::MAX).is_null());
        assert!(Bitmap::with_fill(u32::MAX, u32::MAX, true).is_null());
    }
}
