// SPDX-License-Identifier: LGPL-3.0-only
//! # Image System
//!
//! Provides the canonical CPU-side 2D image buffer, format conversions, zero-copy views,
//! 1-bit monochrome bitmaps, multi-resolution icons, and file I/O codecs matching Qt 6 `QtGui`.
//!
//! ## Core Architecture
//!
//! - [`Image`]: Canonical CPU-side 2D pixel buffer (`QImage`).
//! - [`ImageView`] / [`ImageViewMut`]: Zero-copy pixel slice views (`QImageView`).
//! - [`ImageFormat`]: Pixel format enumeration (RGBA8888, BGRA8888, Mono, Grayscale8, etc.).
//! - [`Bitmap`]: 1-bit monochrome mask for cursors, window masks, and clipping (`QBitmap`).
//! - [`Icon`]: Multi-resolution, multi-state icon management (`QIcon`).
//! - [`ImageReader`] / [`ImageWriter`]: Format detection and codecs (PNG, BMP, PPM; ICO read) (`QImageReader` / `QImageWriter`).
//! - [`Pixmap`]: Platform/display representation backed by rasterization buffer (`QPixmap`).

pub mod bitmap;
pub mod icon;
#[allow(clippy::module_inception)]
pub mod image;
pub mod image_format;
pub mod image_view;
pub mod io;

// Re-exports
pub use bitmap::Bitmap;
pub use icon::{Icon, IconMode, IconState};
pub use image::{AspectRatioMode, Image, TransformationMode};
pub use image_format::ImageFormat;
pub use image_view::{ImageView, ImageViewMut};
pub use io::{ImageFileFormat, ImageReader, ImageWriter};

// Re-export Pixmap from paint module
pub use crate::paint::pixmap::Pixmap;

// =============================================================================
// Canonical Qt 6 Type Aliases
// =============================================================================

/// Canonical Qt 6 alias for `Image`.
pub type QImage = Image;

/// Canonical Qt 6 alias for `Pixmap`.
pub type QPixmap = Pixmap;

/// Canonical Qt 6 alias for `Bitmap`.
pub type QBitmap = Bitmap;

/// Canonical Qt 6 alias for `Icon`.
pub type QIcon = Icon;

/// Canonical Qt 6 alias for `ImageFormat`.
pub type QImageFormat = ImageFormat;

/// Canonical Qt 6 alias for `ImageView`.
pub type QImageView<'a> = ImageView<'a>;

/// Canonical Qt 6 alias for `ImageReader`.
pub type QImageReader = ImageReader;

/// Canonical Qt 6 alias for `ImageWriter`.
pub type QImageWriter = ImageWriter;
