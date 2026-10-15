// SPDX-License-Identifier: LGPL-3.0-only
//! Color system and color management (`QColorSpace`, `QColorTransform`, `QPixelFormat`, HDR).
//!
//! Provides modern pixel format definitions, color space representation (sRGB, Display P3,
//! Adobe RGB, BT.2020, BT.2100 HDR), transformation pipelines with chromatic adaptation,
//! extended dynamic range floating point colors (EDR/HDR), and ICC profile handling.

pub mod color_space;
pub mod color_transform;
pub mod hdr;
pub mod icc;
pub mod pixel_format;
pub mod qcolor_ops;

pub use color_space::*;
pub use color_transform::*;
pub use hdr::*;
pub use icc::*;
pub use pixel_format::*;
pub use qcolor_ops::{darker, lighter};

// --- Qt Canonical Aliases ---
pub type QPixelFormat = PixelFormat;
pub type QColorSpace = ColorSpace;
pub type QColorTransform = ColorTransform;
pub type QRgbaFloat32 = HdrColor;
pub type QIccProfile = IccProfile;
