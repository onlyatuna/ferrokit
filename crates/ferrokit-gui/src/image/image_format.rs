// SPDX-License-Identifier: LGPL-3.0-only
//! Image format definitions matching Qt 6 `QImage::Format`.

/// Supported pixel formats for `Image` matching Qt `QImage::Format`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ImageFormat {
    /// Invalid format.
    Invalid,
    /// 1-bit monochrome, MSB (most significant bit) first.
    Mono,
    /// 1-bit monochrome, LSB (least significant bit) first.
    MonoLsb,
    /// 8-bit per pixel indexed into a palette (color table).
    Indexed8,
    /// 24-bit RGB format (8-8-8, 3 bytes per pixel).
    Rgb888,
    /// 32-bit RGB format (0xFF, R, G, B, 4 bytes per pixel, alpha ignored/opaque).
    Rgb32,
    /// 32-bit ARGB format (A, R, G, B, 4 bytes per pixel).
    Argb32,
    /// 32-bit ARGB format with premultiplied alpha (A, R, G, B).
    Argb32Premultiplied,
    /// 32-bit RGBA format (R, G, B, A, 4 bytes per pixel).
    #[default]
    Rgba8888,
    /// 32-bit RGBA format with premultiplied alpha (R, G, B, A).
    Rgba8888Premultiplied,
    /// 32-bit BGRA format (B, G, R, A, 4 bytes per pixel, native to Windows DIB/GDI).
    Bgra8888,
    /// 8-bit grayscale format.
    Grayscale8,
    /// 8-bit alpha mask format.
    Alpha8,
}

impl ImageFormat {
    /// Returns the number of bits per pixel for this format.
    #[inline]
    pub const fn bits_per_pixel(&self) -> usize {
        match self {
            Self::Invalid => 0,
            Self::Mono | Self::MonoLsb => 1,
            Self::Indexed8 | Self::Grayscale8 | Self::Alpha8 => 8,
            Self::Rgb888 => 24,
            Self::Rgb32
            | Self::Argb32
            | Self::Argb32Premultiplied
            | Self::Rgba8888
            | Self::Rgba8888Premultiplied
            | Self::Bgra8888 => 32,
        }
    }

    /// Returns the number of bytes per pixel for this format.
    ///
    /// For sub-byte formats like `Mono`, this returns 1 (representing a single byte holding up to 8 pixels).
    #[inline]
    pub const fn bytes_per_pixel(&self) -> usize {
        match self {
            Self::Invalid => 0,
            Self::Mono | Self::MonoLsb | Self::Indexed8 | Self::Grayscale8 | Self::Alpha8 => 1,
            Self::Rgb888 => 3,
            Self::Rgb32
            | Self::Argb32
            | Self::Argb32Premultiplied
            | Self::Rgba8888
            | Self::Rgba8888Premultiplied
            | Self::Bgra8888 => 4,
        }
    }

    /// Returns whether this format supports an alpha channel.
    #[inline]
    pub const fn has_alpha_channel(&self) -> bool {
        matches!(
            self,
            Self::Argb32
                | Self::Argb32Premultiplied
                | Self::Rgba8888
                | Self::Rgba8888Premultiplied
                | Self::Bgra8888
                | Self::Alpha8
        )
    }

    /// Returns whether this format uses premultiplied alpha.
    #[inline]
    pub const fn is_premultiplied(&self) -> bool {
        matches!(
            self,
            Self::Argb32Premultiplied | Self::Rgba8888Premultiplied
        )
    }

    /// Returns whether this format is always opaque.
    #[inline]
    pub const fn is_opaque(&self) -> bool {
        !self.has_alpha_channel()
    }

    /// Calculates the minimum scanline byte stride for a given width, optionally aligned.
    ///
    /// Qt aligns scanlines to 4-byte (32-bit) boundaries by default.
    #[inline]
    pub fn bytes_per_line(&self, width: u32, alignment: usize) -> usize {
        if *self == Self::Invalid || width == 0 {
            return 0;
        }

        let raw_bytes = match self {
            Self::Mono | Self::MonoLsb => (width as usize).div_ceil(8),
            Self::Indexed8 | Self::Grayscale8 | Self::Alpha8 => width as usize,
            Self::Rgb888 => width as usize * 3,
            Self::Rgb32
            | Self::Argb32
            | Self::Argb32Premultiplied
            | Self::Rgba8888
            | Self::Rgba8888Premultiplied
            | Self::Bgra8888 => width as usize * 4,
            Self::Invalid => 0,
        };

        if alignment <= 1 {
            raw_bytes
        } else {
            (raw_bytes + alignment - 1) & !(alignment - 1)
        }
    }
}
