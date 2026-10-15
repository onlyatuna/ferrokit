// SPDX-License-Identifier: LGPL-3.0-only
//! Modern Pixel Format abstraction (`QPixelFormat` equivalent).
//!
//! Compact 64-bit integer representation encoding color model, channel bit depths,
//! alpha position and usage, premultiplication, type interpretation, and endianness.

/// Color model of the pixel format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ColorModel {
    Rgb = 0,
    Bgr = 1,
    Indexed = 2,
    Grayscale = 3,
    Cmyk = 4,
    Hsl = 5,
    Hsv = 6,
    Yuv = 7,
    Alpha = 8,
}

/// Indicates whether alpha channel is present and used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum AlphaUsage {
    UsesAlpha = 0,
    IgnoresAlpha = 1,
}

/// Position of the alpha channel in the pixel memory layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum AlphaPosition {
    AtBeginning = 0,
    AtEnd = 1,
}

/// Alpha premultiplication state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum AlphaPremultiplied {
    NotPremultiplied = 0,
    Premultiplied = 1,
}

/// Data type interpretation for pixel components.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum TypeInterpretation {
    UnsignedByte = 0,
    UnsignedShort = 1,
    UnsignedInteger = 2,
    FloatingPoint = 3,
}

/// Byte order / endianness for multi-byte pixel components.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ByteOrder {
    LittleEndian = 0,
    BigEndian = 1,
    CurrentSystemEndian = 2,
}

/// YUV sub-sampling layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum YuvLayout {
    Yuv444 = 0,
    Yuv422 = 1,
    Yuv420p = 2,
    Nv12 = 3,
    Nv21 = 4,
}

/// 64-bit compact pixel format representation (`QPixelFormat`).
///
/// Bit layout (total 64 bits):
/// - `0..3`:   ColorModel (4 bits)
/// - `4..9`:   First channel size (6 bits)
/// - `10..15`: Second channel size (6 bits)
/// - `16..21`: Third channel size (6 bits)
/// - `22..27`: Fourth channel size (6 bits)
/// - `28..33`: Fifth channel size (6 bits)
/// - `34..39`: Alpha channel size (6 bits)
/// - `40`:     AlphaUsage (1 bit)
/// - `41`:     AlphaPosition (1 bit)
/// - `42`:     AlphaPremultiplied (1 bit)
/// - `43..46`: TypeInterpretation (4 bits)
/// - `47..48`: ByteOrder (2 bits)
/// - `49..54`: SubEnum / YuvLayout (6 bits)
/// - `55..63`: Reserved (9 bits)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct PixelFormat {
    data: u64,
}

impl PixelFormat {
    const MODEL_SHIFT: u32 = 0;
    const MODEL_MASK: u64 = 0xF;

    const FIRST_SHIFT: u32 = 4;
    const FIRST_MASK: u64 = 0x3F;

    const SECOND_SHIFT: u32 = 10;
    const SECOND_MASK: u64 = 0x3F;

    const THIRD_SHIFT: u32 = 16;
    const THIRD_MASK: u64 = 0x3F;

    const FOURTH_SHIFT: u32 = 22;
    const FOURTH_MASK: u64 = 0x3F;

    const FIFTH_SHIFT: u32 = 28;
    const FIFTH_MASK: u64 = 0x3F;

    const ALPHA_SHIFT: u32 = 34;
    const ALPHA_MASK: u64 = 0x3F;

    const ALPHA_USAGE_SHIFT: u32 = 40;
    const ALPHA_USAGE_MASK: u64 = 0x1;

    const ALPHA_POS_SHIFT: u32 = 41;
    const ALPHA_POS_MASK: u64 = 0x1;

    const PREMUL_SHIFT: u32 = 42;
    const PREMUL_MASK: u64 = 0x1;

    const TYPE_INTERP_SHIFT: u32 = 43;
    const TYPE_INTERP_MASK: u64 = 0xF;

    const BYTE_ORDER_SHIFT: u32 = 47;
    const BYTE_ORDER_MASK: u64 = 0x3;

    const SUB_ENUM_SHIFT: u32 = 49;
    const SUB_ENUM_MASK: u64 = 0x3F;

    /// Creates an empty/invalid pixel format.
    pub const fn new() -> Self {
        Self { data: 0 }
    }

    /// Full constructor for `PixelFormat`.
    #[allow(clippy::too_many_arguments)]
    pub const fn from_raw_parts(
        color_model: ColorModel,
        first_size: u8,
        second_size: u8,
        third_size: u8,
        fourth_size: u8,
        fifth_size: u8,
        alpha_size: u8,
        alpha_usage: AlphaUsage,
        alpha_position: AlphaPosition,
        premultiplied: AlphaPremultiplied,
        type_interpretation: TypeInterpretation,
        byte_order: ByteOrder,
        sub_enum: u8,
    ) -> Self {
        let raw = ((color_model as u64) & Self::MODEL_MASK) << Self::MODEL_SHIFT
            | (((first_size as u64) & Self::FIRST_MASK) << Self::FIRST_SHIFT)
            | (((second_size as u64) & Self::SECOND_MASK) << Self::SECOND_SHIFT)
            | (((third_size as u64) & Self::THIRD_MASK) << Self::THIRD_SHIFT)
            | (((fourth_size as u64) & Self::FOURTH_MASK) << Self::FOURTH_SHIFT)
            | (((fifth_size as u64) & Self::FIFTH_MASK) << Self::FIFTH_SHIFT)
            | (((alpha_size as u64) & Self::ALPHA_MASK) << Self::ALPHA_SHIFT)
            | (((alpha_usage as u64) & Self::ALPHA_USAGE_MASK) << Self::ALPHA_USAGE_SHIFT)
            | (((alpha_position as u64) & Self::ALPHA_POS_MASK) << Self::ALPHA_POS_SHIFT)
            | (((premultiplied as u64) & Self::PREMUL_MASK) << Self::PREMUL_SHIFT)
            | (((type_interpretation as u64) & Self::TYPE_INTERP_MASK) << Self::TYPE_INTERP_SHIFT)
            | (((byte_order as u64) & Self::BYTE_ORDER_MASK) << Self::BYTE_ORDER_SHIFT)
            | (((sub_enum as u64) & Self::SUB_ENUM_MASK) << Self::SUB_ENUM_SHIFT);

        Self { data: raw }
    }

    /// Color model.
    #[inline]
    pub const fn color_model(&self) -> ColorModel {
        match (self.data >> Self::MODEL_SHIFT) & Self::MODEL_MASK {
            1 => ColorModel::Bgr,
            2 => ColorModel::Indexed,
            3 => ColorModel::Grayscale,
            4 => ColorModel::Cmyk,
            5 => ColorModel::Hsl,
            6 => ColorModel::Hsv,
            7 => ColorModel::Yuv,
            8 => ColorModel::Alpha,
            _ => ColorModel::Rgb,
        }
    }

    /// Bits per pixel.
    #[inline]
    pub const fn bits_per_pixel(&self) -> u8 {
        self.first_size()
            + self.second_size()
            + self.third_size()
            + self.fourth_size()
            + self.fifth_size()
            + self.alpha_size()
    }

    /// Bytes per pixel (rounded up).
    #[inline]
    pub const fn bytes_per_pixel(&self) -> usize {
        (self.bits_per_pixel() as usize).div_ceil(8)
    }

    /// Total number of active channels.
    #[inline]
    pub const fn channel_count(&self) -> u8 {
        (if self.first_size() > 0 { 1 } else { 0 })
            + (if self.second_size() > 0 { 1 } else { 0 })
            + (if self.third_size() > 0 { 1 } else { 0 })
            + (if self.fourth_size() > 0 { 1 } else { 0 })
            + (if self.fifth_size() > 0 { 1 } else { 0 })
            + (if self.alpha_size() > 0 { 1 } else { 0 })
    }

    #[inline]
    pub const fn first_size(&self) -> u8 {
        ((self.data >> Self::FIRST_SHIFT) & Self::FIRST_MASK) as u8
    }

    #[inline]
    pub const fn second_size(&self) -> u8 {
        ((self.data >> Self::SECOND_SHIFT) & Self::SECOND_MASK) as u8
    }

    #[inline]
    pub const fn third_size(&self) -> u8 {
        ((self.data >> Self::THIRD_SHIFT) & Self::THIRD_MASK) as u8
    }

    #[inline]
    pub const fn fourth_size(&self) -> u8 {
        ((self.data >> Self::FOURTH_SHIFT) & Self::FOURTH_MASK) as u8
    }

    #[inline]
    pub const fn fifth_size(&self) -> u8 {
        ((self.data >> Self::FIFTH_SHIFT) & Self::FIFTH_MASK) as u8
    }

    #[inline]
    pub const fn alpha_size(&self) -> u8 {
        ((self.data >> Self::ALPHA_SHIFT) & Self::ALPHA_MASK) as u8
    }

    #[inline]
    pub const fn red_size(&self) -> u8 {
        match self.color_model() {
            ColorModel::Rgb => self.first_size(),
            ColorModel::Bgr => self.third_size(),
            _ => 0,
        }
    }

    #[inline]
    pub const fn green_size(&self) -> u8 {
        self.second_size()
    }

    #[inline]
    pub const fn blue_size(&self) -> u8 {
        match self.color_model() {
            ColorModel::Rgb => self.third_size(),
            ColorModel::Bgr => self.first_size(),
            _ => 0,
        }
    }

    #[inline]
    pub const fn alpha_usage(&self) -> AlphaUsage {
        if ((self.data >> Self::ALPHA_USAGE_SHIFT) & Self::ALPHA_USAGE_MASK) == 1 {
            AlphaUsage::IgnoresAlpha
        } else {
            AlphaUsage::UsesAlpha
        }
    }

    #[inline]
    pub const fn alpha_position(&self) -> AlphaPosition {
        if ((self.data >> Self::ALPHA_POS_SHIFT) & Self::ALPHA_POS_MASK) == 1 {
            AlphaPosition::AtEnd
        } else {
            AlphaPosition::AtBeginning
        }
    }

    #[inline]
    pub const fn premultiplied(&self) -> AlphaPremultiplied {
        if ((self.data >> Self::PREMUL_SHIFT) & Self::PREMUL_MASK) == 1 {
            AlphaPremultiplied::Premultiplied
        } else {
            AlphaPremultiplied::NotPremultiplied
        }
    }

    #[inline]
    pub const fn is_premultiplied(&self) -> bool {
        matches!(self.premultiplied(), AlphaPremultiplied::Premultiplied)
    }

    #[inline]
    pub const fn has_alpha(&self) -> bool {
        self.alpha_size() > 0 && matches!(self.alpha_usage(), AlphaUsage::UsesAlpha)
    }

    #[inline]
    pub const fn type_interpretation(&self) -> TypeInterpretation {
        match (self.data >> Self::TYPE_INTERP_SHIFT) & Self::TYPE_INTERP_MASK {
            1 => TypeInterpretation::UnsignedShort,
            2 => TypeInterpretation::UnsignedInteger,
            3 => TypeInterpretation::FloatingPoint,
            _ => TypeInterpretation::UnsignedByte,
        }
    }

    #[inline]
    pub const fn byte_order(&self) -> ByteOrder {
        match (self.data >> Self::BYTE_ORDER_SHIFT) & Self::BYTE_ORDER_MASK {
            1 => ByteOrder::BigEndian,
            2 => ByteOrder::CurrentSystemEndian,
            _ => ByteOrder::LittleEndian,
        }
    }

    // --- Standard Preset Constructors ---

    /// RGBA 8-8-8-8 format (non-premultiplied).
    pub const fn rgba8888() -> Self {
        Self::from_raw_parts(
            ColorModel::Rgb,
            8,
            8,
            8,
            0,
            0,
            8,
            AlphaUsage::UsesAlpha,
            AlphaPosition::AtEnd,
            AlphaPremultiplied::NotPremultiplied,
            TypeInterpretation::UnsignedByte,
            ByteOrder::CurrentSystemEndian,
            0,
        )
    }

    /// RGBA 8-8-8-8 format with premultiplied alpha.
    pub const fn rgba8888_premultiplied() -> Self {
        Self::from_raw_parts(
            ColorModel::Rgb,
            8,
            8,
            8,
            0,
            0,
            8,
            AlphaUsage::UsesAlpha,
            AlphaPosition::AtEnd,
            AlphaPremultiplied::Premultiplied,
            TypeInterpretation::UnsignedByte,
            ByteOrder::CurrentSystemEndian,
            0,
        )
    }

    /// ARGB 8-8-8-8 format.
    pub const fn argb8888() -> Self {
        Self::from_raw_parts(
            ColorModel::Rgb,
            8,
            8,
            8,
            0,
            0,
            8,
            AlphaUsage::UsesAlpha,
            AlphaPosition::AtBeginning,
            AlphaPremultiplied::NotPremultiplied,
            TypeInterpretation::UnsignedByte,
            ByteOrder::CurrentSystemEndian,
            0,
        )
    }

    /// BGRA 8-8-8-8 format (Windows GDI / DXGI native).
    pub const fn bgra8888() -> Self {
        Self::from_raw_parts(
            ColorModel::Bgr,
            8,
            8,
            8,
            0,
            0,
            8,
            AlphaUsage::UsesAlpha,
            AlphaPosition::AtEnd,
            AlphaPremultiplied::NotPremultiplied,
            TypeInterpretation::UnsignedByte,
            ByteOrder::CurrentSystemEndian,
            0,
        )
    }

    /// RGB 8-8-8 24-bit format.
    pub const fn rgb888() -> Self {
        Self::from_raw_parts(
            ColorModel::Rgb,
            8,
            8,
            8,
            0,
            0,
            0,
            AlphaUsage::IgnoresAlpha,
            AlphaPosition::AtEnd,
            AlphaPremultiplied::NotPremultiplied,
            TypeInterpretation::UnsignedByte,
            ByteOrder::CurrentSystemEndian,
            0,
        )
    }

    /// Grayscale 8-bit format.
    pub const fn grayscale8() -> Self {
        Self::from_raw_parts(
            ColorModel::Grayscale,
            8,
            0,
            0,
            0,
            0,
            0,
            AlphaUsage::IgnoresAlpha,
            AlphaPosition::AtEnd,
            AlphaPremultiplied::NotPremultiplied,
            TypeInterpretation::UnsignedByte,
            ByteOrder::CurrentSystemEndian,
            0,
        )
    }

    /// 8-bit Alpha-only mask format.
    pub const fn alpha8() -> Self {
        Self::from_raw_parts(
            ColorModel::Alpha,
            0,
            0,
            0,
            0,
            0,
            8,
            AlphaUsage::UsesAlpha,
            AlphaPosition::AtBeginning,
            AlphaPremultiplied::NotPremultiplied,
            TypeInterpretation::UnsignedByte,
            ByteOrder::CurrentSystemEndian,
            0,
        )
    }

    /// High Dynamic Range (HDR) RGBA 32-bit floating point format.
    pub const fn rgba32f() -> Self {
        Self::from_raw_parts(
            ColorModel::Rgb,
            32,
            32,
            32,
            0,
            0,
            32,
            AlphaUsage::UsesAlpha,
            AlphaPosition::AtEnd,
            AlphaPremultiplied::NotPremultiplied,
            TypeInterpretation::FloatingPoint,
            ByteOrder::CurrentSystemEndian,
            0,
        )
    }

    /// HDR RGBA 16-bit half-float format.
    pub const fn rgba16f() -> Self {
        Self::from_raw_parts(
            ColorModel::Rgb,
            16,
            16,
            16,
            0,
            0,
            16,
            AlphaUsage::UsesAlpha,
            AlphaPosition::AtEnd,
            AlphaPremultiplied::NotPremultiplied,
            TypeInterpretation::FloatingPoint,
            ByteOrder::CurrentSystemEndian,
            0,
        )
    }
}
