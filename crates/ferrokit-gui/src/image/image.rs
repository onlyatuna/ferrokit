// SPDX-License-Identifier: LGPL-3.0-only
//! CPU-side canonical image buffer matching Qt 6 `QImage`.

use crate::geometry::primitives::{Rect, Size};
use crate::image::image_format::ImageFormat;
use crate::image::image_view::{ImageView, ImageViewMut};
use tiny_skia::Color;

/// Scaling aspect ratio mode matching `Qt::AspectRatioMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AspectRatioMode {
    /// Disregards aspect ratio, scales to target dimensions.
    #[default]
    IgnoreAspectRatio,
    /// Keeps aspect ratio, scales to maximum size inside target dimensions.
    KeepAspectRatio,
    /// Keeps aspect ratio, scales to minimum size expanding target dimensions.
    KeepAspectRatioByExpanding,
}

/// Image transformation quality matching `Qt::TransformationMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TransformationMode {
    /// Fast nearest-neighbor filtering.
    #[default]
    FastTransformation,
    /// Bilinear smooth filtering.
    SmoothTransformation,
}

/// CPU-side canonical 2D pixel buffer with format support and transformation methods.
#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    data: Vec<u8>,
    width: u32,
    height: u32,
    bytes_per_line: usize,
    format: ImageFormat,
    color_table: Vec<u32>,
    dpr: f32,
}

impl Default for Image {
    fn default() -> Self {
        Self::null()
    }
}

impl Image {
    /// Creates a null (empty) image.
    pub fn null() -> Self {
        Self {
            data: Vec::new(),
            width: 0,
            height: 0,
            bytes_per_line: 0,
            format: ImageFormat::Invalid,
            color_table: Vec::new(),
            dpr: 1.0,
        }
    }

    /// Creates an allocated image with the specified dimensions and pixel format.
    pub fn new(width: u32, height: u32, format: ImageFormat) -> Self {
        Self::with_dpr(width, height, format, 1.0)
    }

    /// Creates an image with device pixel ratio.
    pub fn with_dpr(width: u32, height: u32, format: ImageFormat, dpr: f32) -> Self {
        if width == 0 || height == 0 || format == ImageFormat::Invalid {
            return Self::null();
        }

        let bytes_per_line = format.bytes_per_line(width, 4);
        let total_bytes = bytes_per_line * (height as usize);
        let data = vec![0u8; total_bytes];

        Self {
            data,
            width,
            height,
            bytes_per_line,
            format,
            color_table: Vec::new(),
            dpr: dpr.max(1.0),
        }
    }

    /// Creates an image from an existing pixel data buffer.
    pub fn from_data(
        data: Vec<u8>,
        width: u32,
        height: u32,
        bytes_per_line: usize,
        format: ImageFormat,
    ) -> Option<Self> {
        if width == 0 || height == 0 || format == ImageFormat::Invalid {
            return None;
        }

        let required = bytes_per_line.checked_mul(height as usize)?;
        if data.len() < required {
            return None;
        }

        Some(Self {
            data,
            width,
            height,
            bytes_per_line,
            format,
            color_table: Vec::new(),
            dpr: 1.0,
        })
    }

    /// Returns `true` if the image is empty/null.
    #[inline]
    pub fn is_null(&self) -> bool {
        self.width == 0 || self.height == 0 || self.format == ImageFormat::Invalid
    }

    /// Width in physical pixels.
    #[inline]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Height in physical pixels.
    #[inline]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// Logical width taking DPR into account.
    #[inline]
    pub fn logical_width(&self) -> f32 {
        self.width as f32 / self.dpr
    }

    /// Logical height taking DPR into account.
    #[inline]
    pub fn logical_height(&self) -> f32 {
        self.height as f32 / self.dpr
    }

    /// Image dimensions as a `Size`.
    #[inline]
    pub fn size(&self) -> Size {
        Size::new(self.width as i32, self.height as i32)
    }

    /// Image rectangle `Rect(0, 0, width, height)`.
    #[inline]
    pub fn rect(&self) -> Rect {
        Rect::new(0, 0, self.width as i32, self.height as i32)
    }

    /// Pixel format.
    #[inline]
    pub const fn format(&self) -> ImageFormat {
        self.format
    }

    /// Device pixel ratio.
    #[inline]
    pub const fn device_pixel_ratio(&self) -> f32 {
        self.dpr
    }

    /// Sets the device pixel ratio.
    #[inline]
    pub fn set_device_pixel_ratio(&mut self, dpr: f32) {
        self.dpr = dpr.max(1.0);
    }

    /// Number of bytes per scanline.
    #[inline]
    pub const fn bytes_per_line(&self) -> usize {
        self.bytes_per_line
    }

    /// Total byte size of the image pixel buffer.
    #[inline]
    pub fn size_in_bytes(&self) -> usize {
        self.data.len()
    }

    /// Immutable reference to the raw pixel buffer.
    #[inline]
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Mutable reference to the raw pixel buffer.
    #[inline]
    pub fn data_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }

    /// Reference to the palette / color table for indexed formats.
    #[inline]
    pub fn color_table(&self) -> &[u32] {
        &self.color_table
    }

    /// Sets the color table for indexed formats.
    pub fn set_color_table(&mut self, table: Vec<u32>) {
        self.color_table = table;
    }

    /// Returns a zero-copy read-only view of this image.
    pub fn as_view(&self) -> ImageView<'_> {
        ImageView::new(
            &self.data,
            self.width,
            self.height,
            self.bytes_per_line,
            self.format,
        )
        .expect("image invariants ensure valid view")
    }

    /// Returns a zero-copy mutable view of this image.
    pub fn as_view_mut(&mut self) -> ImageViewMut<'_> {
        ImageViewMut::new(
            &mut self.data,
            self.width,
            self.height,
            self.bytes_per_line,
            self.format,
        )
        .expect("image invariants ensure valid view")
    }

    /// Gets a reference to the scanline at index `y`.
    #[inline]
    pub fn scan_line(&self, y: u32) -> Option<&[u8]> {
        if y >= self.height {
            return None;
        }
        let start = (y as usize) * self.bytes_per_line;
        let end = start + self.format.bytes_per_line(self.width, 1);
        self.data.get(start..end)
    }

    /// Gets a mutable reference to the scanline at index `y`.
    #[inline]
    pub fn scan_line_mut(&mut self, y: u32) -> Option<&mut [u8]> {
        if y >= self.height {
            return None;
        }
        let start = (y as usize) * self.bytes_per_line;
        let end = start + self.format.bytes_per_line(self.width, 1);
        self.data.get_mut(start..end)
    }

    /// Reads the pixel color at `(x, y)`.
    pub fn pixel_color(&self, x: u32, y: u32) -> Option<Color> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let line = self.scan_line(y)?;
        match self.format {
            ImageFormat::Rgba8888 => {
                let off = (x as usize) * 4;
                Some(Color::from_rgba8(
                    line[off],
                    line[off + 1],
                    line[off + 2],
                    line[off + 3],
                ))
            }
            ImageFormat::Rgba8888Premultiplied => {
                let off = (x as usize) * 4;
                let a = line[off + 3];
                if a == 0 {
                    Some(Color::from_rgba8(0, 0, 0, 0))
                } else {
                    let r = ((line[off] as u32 * 255) / a as u32).min(255) as u8;
                    let g = ((line[off + 1] as u32 * 255) / a as u32).min(255) as u8;
                    let b = ((line[off + 2] as u32 * 255) / a as u32).min(255) as u8;
                    Some(Color::from_rgba8(r, g, b, a))
                }
            }
            ImageFormat::Bgra8888 => {
                let off = (x as usize) * 4;
                Some(Color::from_rgba8(
                    line[off + 2],
                    line[off + 1],
                    line[off],
                    line[off + 3],
                ))
            }
            ImageFormat::Argb32 => {
                let off = (x as usize) * 4;
                Some(Color::from_rgba8(
                    line[off + 1],
                    line[off + 2],
                    line[off + 3],
                    line[off],
                ))
            }
            ImageFormat::Argb32Premultiplied => {
                let off = (x as usize) * 4;
                let a = line[off];
                if a == 0 {
                    Some(Color::from_rgba8(0, 0, 0, 0))
                } else {
                    let r = ((line[off + 1] as u32 * 255) / a as u32).min(255) as u8;
                    let g = ((line[off + 2] as u32 * 255) / a as u32).min(255) as u8;
                    let b = ((line[off + 3] as u32 * 255) / a as u32).min(255) as u8;
                    Some(Color::from_rgba8(r, g, b, a))
                }
            }
            ImageFormat::Rgb32 => {
                let off = (x as usize) * 4;
                Some(Color::from_rgba8(
                    line[off + 1],
                    line[off + 2],
                    line[off + 3],
                    255,
                ))
            }
            ImageFormat::Rgb888 => {
                let off = (x as usize) * 3;
                Some(Color::from_rgba8(
                    line[off],
                    line[off + 1],
                    line[off + 2],
                    255,
                ))
            }
            ImageFormat::Grayscale8 => {
                let v = line[x as usize];
                Some(Color::from_rgba8(v, v, v, 255))
            }
            ImageFormat::Alpha8 => {
                let a = line[x as usize];
                Some(Color::from_rgba8(255, 255, 255, a))
            }
            ImageFormat::Indexed8 => {
                let idx = line[x as usize] as usize;
                let argb = self.color_table.get(idx).copied().unwrap_or(0);
                let a = ((argb >> 24) & 0xFF) as u8;
                let r = ((argb >> 16) & 0xFF) as u8;
                let g = ((argb >> 8) & 0xFF) as u8;
                let b = (argb & 0xFF) as u8;
                Some(Color::from_rgba8(r, g, b, a))
            }
            ImageFormat::Mono => {
                let byte = line[(x / 8) as usize];
                let bit = (byte >> (7 - (x % 8))) & 1;
                if bit == 1 {
                    Some(Color::WHITE)
                } else {
                    Some(Color::BLACK)
                }
            }
            ImageFormat::MonoLsb => {
                let byte = line[(x / 8) as usize];
                let bit = (byte >> (x % 8)) & 1;
                if bit == 1 {
                    Some(Color::WHITE)
                } else {
                    Some(Color::BLACK)
                }
            }
            ImageFormat::Invalid => None,
        }
    }

    /// Sets the pixel color at `(x, y)`.
    pub fn set_pixel_color(&mut self, x: u32, y: u32, color: Color) -> bool {
        if x >= self.width || y >= self.height {
            return false;
        }
        let bpl = self.bytes_per_line;
        let line = match self.data.get_mut((y as usize) * bpl..) {
            Some(s) => s,
            None => return false,
        };

        match self.format {
            ImageFormat::Rgba8888 => {
                let u = color.to_color_u8();
                let off = (x as usize) * 4;
                line[off] = u.red();
                line[off + 1] = u.green();
                line[off + 2] = u.blue();
                line[off + 3] = u.alpha();
                true
            }
            ImageFormat::Rgba8888Premultiplied => {
                let u = color.to_color_u8();
                let off = (x as usize) * 4;
                let a = u.alpha() as u32;
                line[off] = ((u.red() as u32 * a + 127) / 255) as u8;
                line[off + 1] = ((u.green() as u32 * a + 127) / 255) as u8;
                line[off + 2] = ((u.blue() as u32 * a + 127) / 255) as u8;
                line[off + 3] = u.alpha();
                true
            }
            ImageFormat::Bgra8888 => {
                let u = color.to_color_u8();
                let off = (x as usize) * 4;
                line[off] = u.blue();
                line[off + 1] = u.green();
                line[off + 2] = u.red();
                line[off + 3] = u.alpha();
                true
            }
            ImageFormat::Argb32 => {
                let u = color.to_color_u8();
                let off = (x as usize) * 4;
                line[off] = u.alpha();
                line[off + 1] = u.red();
                line[off + 2] = u.green();
                line[off + 3] = u.blue();
                true
            }
            ImageFormat::Argb32Premultiplied => {
                let u = color.to_color_u8();
                let off = (x as usize) * 4;
                let a = u.alpha() as u32;
                line[off] = u.alpha();
                line[off + 1] = ((u.red() as u32 * a + 127) / 255) as u8;
                line[off + 2] = ((u.green() as u32 * a + 127) / 255) as u8;
                line[off + 3] = ((u.blue() as u32 * a + 127) / 255) as u8;
                true
            }
            ImageFormat::Rgb32 => {
                let u = color.to_color_u8();
                let off = (x as usize) * 4;
                line[off] = 0xFF;
                line[off + 1] = u.red();
                line[off + 2] = u.green();
                line[off + 3] = u.blue();
                true
            }
            ImageFormat::Rgb888 => {
                let u = color.to_color_u8();
                let off = (x as usize) * 3;
                line[off] = u.red();
                line[off + 1] = u.green();
                line[off + 2] = u.blue();
                true
            }
            ImageFormat::Grayscale8 => {
                let u = color.to_color_u8();
                let lum =
                    (u.red() as u32 * 299 + u.green() as u32 * 587 + u.blue() as u32 * 114) / 1000;
                line[x as usize] = lum as u8;
                true
            }
            ImageFormat::Alpha8 => {
                let u = color.to_color_u8();
                line[x as usize] = u.alpha();
                true
            }
            ImageFormat::Mono | ImageFormat::MonoLsb => {
                let u = color.to_color_u8();
                let lum =
                    (u.red() as u32 * 299 + u.green() as u32 * 587 + u.blue() as u32 * 114) / 1000;
                let bit = lum >= 128;
                let bit_index = (x % 8) as u8;
                let mask = match self.format {
                    ImageFormat::Mono => 1 << (7 - bit_index),
                    _ => 1 << bit_index,
                };
                let byte = &mut line[(x / 8) as usize];
                if bit {
                    *byte |= mask;
                } else {
                    *byte &= !mask;
                }
                true
            }
            ImageFormat::Indexed8 => {
                let u = color.to_color_u8();
                if self.color_table.is_empty() {
                    line[x as usize] = if u.red() as u32 * 299
                        + u.green() as u32 * 587
                        + u.blue() as u32 * 114
                        >= 128_000
                    {
                        1
                    } else {
                        0
                    };
                } else {
                    let argb = ((u.alpha() as u32) << 24)
                        | ((u.red() as u32) << 16)
                        | ((u.green() as u32) << 8)
                        | u.blue() as u32;
                    let nearest = self
                        .color_table
                        .iter()
                        .enumerate()
                        .min_by_key(|(_, candidate)| {
                            let dr = ((argb >> 16) & 255) as i32 - ((*candidate >> 16) & 255) as i32;
                            let dg = ((argb >> 8) & 255) as i32 - ((*candidate >> 8) & 255) as i32;
                            let db = (argb & 255) as i32 - (*candidate & 255) as i32;
                            dr * dr + dg * dg + db * db
                        })
                        .map(|(index, _)| index)
                        .unwrap_or(0);
                    line[x as usize] = nearest.min(255) as u8;
                }
                true
            }
            _ => false,
        }
    }

    /// Fills the entire image with the specified color.
    pub fn fill(&mut self, color: Color) {
        for y in 0..self.height {
            for x in 0..self.width {
                self.set_pixel_color(x, y, color);
            }
        }
    }

    /// Converts this image to a target format.
    pub fn converted_to(&self, target_format: ImageFormat) -> Image {
        if self.is_null() || target_format == ImageFormat::Invalid {
            return Image::null();
        }
        if self.format == target_format {
            return self.clone();
        }

        let mut dest = Image::with_dpr(self.width, self.height, target_format, self.dpr);
        for y in 0..self.height {
            for x in 0..self.width {
                if let Some(c) = self.pixel_color(x, y) {
                    dest.set_pixel_color(x, y, c);
                }
            }
        }
        dest
    }

    /// Returns a mirrored copy along horizontal and/or vertical axes.
    pub fn mirrored(&self, horizontal: bool, vertical: bool) -> Image {
        if self.is_null() || (!horizontal && !vertical) {
            return self.clone();
        }

        let mut dest = Image::with_dpr(self.width, self.height, self.format, self.dpr);
        for y in 0..self.height {
            let src_y = if vertical { self.height - 1 - y } else { y };
            for x in 0..self.width {
                let src_x = if horizontal { self.width - 1 - x } else { x };
                if let Some(c) = self.pixel_color(src_x, src_y) {
                    dest.set_pixel_color(x, y, c);
                }
            }
        }
        dest
    }

    /// Returns a cropped sub-image defined by `rect`.
    pub fn copy_rect(&self, rect: Rect) -> Image {
        if self.is_null() || rect.width <= 0 || rect.height <= 0 {
            return Image::null();
        }

        let intersect = self.rect().intersected(&rect);
        if intersect.is_empty() || intersect.width <= 0 || intersect.height <= 0 {
            return Image::null();
        }

        let mut dest = Image::with_dpr(
            intersect.width as u32,
            intersect.height as u32,
            self.format,
            self.dpr,
        );
        for y in 0..intersect.height as u32 {
            let src_y = (intersect.y as u32) + y;
            for x in 0..intersect.width as u32 {
                let src_x = (intersect.x as u32) + x;
                if let Some(c) = self.pixel_color(src_x, src_y) {
                    dest.set_pixel_color(x, y, c);
                }
            }
        }
        dest
    }

    /// Scales the image to target dimensions according to aspect ratio and transformation modes.
    pub fn scaled(
        &self,
        target_width: u32,
        target_height: u32,
        aspect_ratio: AspectRatioMode,
        _mode: TransformationMode,
    ) -> Image {
        if self.is_null() || target_width == 0 || target_height == 0 {
            return Image::null();
        }

        let (final_w, final_h) = match aspect_ratio {
            AspectRatioMode::IgnoreAspectRatio => (target_width, target_height),
            AspectRatioMode::KeepAspectRatio => {
                let w_ratio = target_width as f64 / self.width as f64;
                let h_ratio = target_height as f64 / self.height as f64;
                let ratio = w_ratio.min(h_ratio);
                (
                    ((self.width as f64 * ratio).round() as u32).max(1),
                    ((self.height as f64 * ratio).round() as u32).max(1),
                )
            }
            AspectRatioMode::KeepAspectRatioByExpanding => {
                let w_ratio = target_width as f64 / self.width as f64;
                let h_ratio = target_height as f64 / self.height as f64;
                let ratio = w_ratio.max(h_ratio);
                (
                    ((self.width as f64 * ratio).round() as u32).max(1),
                    ((self.height as f64 * ratio).round() as u32).max(1),
                )
            }
        };

        let mut dest = Image::with_dpr(final_w, final_h, self.format, self.dpr);
        // Nearest-neighbor resampling
        for y in 0..final_h {
            let src_y = ((y as u64 * self.height as u64) / final_h as u64) as u32;
            for x in 0..final_w {
                let src_x = ((x as u64 * self.width as u64) / final_w as u64) as u32;
                if let Some(c) = self.pixel_color(src_x, src_y) {
                    dest.set_pixel_color(x, y, c);
                }
            }
        }
        dest
    }
}
