// SPDX-License-Identifier: LGPL-3.0-only
//! ICC Color Profile representation and parser (`QIccProfile` equivalent).
//!
//! Parses the standard 128-byte ICC profile header (ICC.1:2010 specification),
//! extracting magic signature ('acsp'), profile class, color space signature,
//! and connection space.

use super::color_space::ColorSpace;
use super::pixel_format::ColorModel;

/// Parsed ICC Profile header metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IccHeader {
    pub profile_size: u32,
    pub cmm_type: [u8; 4],
    pub version_major: u8,
    pub version_minor: u8,
    pub device_class: [u8; 4],
    pub color_space: [u8; 4],
    pub pcs: [u8; 4],
    pub platform: [u8; 4],
}

/// Standard ICC Profile (`QIccProfile`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IccProfile {
    data: Vec<u8>,
    header: Option<IccHeader>,
}

impl Default for IccProfile {
    fn default() -> Self {
        Self::new()
    }
}

impl IccProfile {
    /// Magic signature 'acsp' at offset 36..40.
    pub const MAGIC: [u8; 4] = *b"acsp";

    /// Creates an empty invalid ICC profile.
    pub const fn new() -> Self {
        Self {
            data: Vec::new(),
            header: None,
        }
    }

    /// Parses an ICC profile from raw binary bytes.
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < 128 {
            return None;
        }

        // Validate 'acsp' magic
        if data[36..40] != Self::MAGIC {
            return None;
        }

        let profile_size = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        let cmm_type = [data[4], data[5], data[6], data[7]];
        let version_major = data[8];
        let version_minor = data[9];
        let device_class = [data[12], data[13], data[14], data[15]];
        let color_space = [data[16], data[17], data[18], data[19]];
        let pcs = [data[20], data[21], data[22], data[23]];
        let platform = [data[40], data[41], data[42], data[43]];

        let header = IccHeader {
            profile_size,
            cmm_type,
            version_major,
            version_minor,
            device_class,
            color_space,
            pcs,
            platform,
        };

        Some(Self {
            data: data.to_vec(),
            header: Some(header),
        })
    }

    /// Whether this profile is parsed and valid.
    pub fn is_valid(&self) -> bool {
        self.header.is_some()
    }

    /// Parsed header metadata, if valid.
    pub fn header(&self) -> Option<&IccHeader> {
        self.header.as_ref()
    }

    /// Raw profile binary data.
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Extracted color model signature.
    pub fn color_model(&self) -> Option<ColorModel> {
        let hdr = self.header.as_ref()?;
        match &hdr.color_space {
            b"RGB " => Some(ColorModel::Rgb),
            b"GRAY" => Some(ColorModel::Grayscale),
            b"CMYK" => Some(ColorModel::Cmyk),
            _ => None,
        }
    }

    /// Attempts to convert this profile to a known `ColorSpace`.
    pub fn to_color_space(&self) -> Option<ColorSpace> {
        match self.color_model()? {
            ColorModel::Rgb => Some(ColorSpace::srgb()),
            ColorModel::Grayscale => Some(ColorSpace::linear_srgb()),
            _ => None,
        }
    }
}
