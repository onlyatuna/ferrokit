// SPDX-License-Identifier: LGPL-3.0-only
//! Color Space representation (`QColorSpace` equivalent).
//!
//! Encapsulates chromaticity primaries (Red, Green, Blue, White Point),
//! transfer functions (gamma, sRGB, linear, PQ, HLG), and color transformations.

use super::color_transform::ColorTransform;
use crate::geometry::PointF;

/// Predefined standard color spaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NamedColorSpace {
    Srgb = 1,
    LinearSrgb = 2,
    AdobeRgb = 3,
    DisplayP3 = 4,
    ProPhotoRgb = 5,
    Bt2020 = 6,
    Bt2100Pq = 7,
    Bt2100Hlg = 8,
}

/// Chromaticity primary sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Primaries {
    Custom = 0,
    Srgb = 1,
    AdobeRgb = 2,
    DciP3D65 = 3,
    DisplayP3 = 6,
    ProPhotoRgb = 4,
    Bt2020 = 5,
}

/// Optical-electro / electro-optical transfer functions (EOTF / OETF).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TransferFunction {
    Custom,
    Linear,
    Gamma(f32),
    Srgb,
    Bt2020,
    St2084, // Perceptual Quantizer (PQ, HDR10)
    Hlg,    // Hybrid Log-Gamma
}

/// CIE 1931 xy chromaticity coordinates for primaries and reference white point.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PrimaryPoints {
    pub white_point: PointF,
    pub red_point: PointF,
    pub green_point: PointF,
    pub blue_point: PointF,
}

impl PrimaryPoints {
    /// Standard CIE D65 white point (x: 0.3127, y: 0.3290).
    pub const D65: PointF = PointF {
        x: 0.3127,
        y: 0.3290,
    };
    /// Standard CIE D50 white point (x: 0.3457, y: 0.3585).
    pub const D50: PointF = PointF {
        x: 0.3457,
        y: 0.3585,
    };

    /// Returns the primary points for standard primaries.
    pub fn from_primaries(primaries: Primaries) -> Self {
        match primaries {
            Primaries::Srgb => Self {
                white_point: Self::D65,
                red_point: PointF { x: 0.640, y: 0.330 },
                green_point: PointF { x: 0.300, y: 0.600 },
                blue_point: PointF { x: 0.150, y: 0.060 },
            },
            Primaries::DisplayP3 | Primaries::DciP3D65 => Self {
                white_point: Self::D65,
                red_point: PointF { x: 0.680, y: 0.320 },
                green_point: PointF { x: 0.265, y: 0.690 },
                blue_point: PointF { x: 0.150, y: 0.060 },
            },
            Primaries::AdobeRgb => Self {
                white_point: Self::D65,
                red_point: PointF { x: 0.640, y: 0.330 },
                green_point: PointF { x: 0.210, y: 0.710 },
                blue_point: PointF { x: 0.150, y: 0.060 },
            },
            Primaries::ProPhotoRgb => Self {
                white_point: Self::D50,
                red_point: PointF {
                    x: 0.7347,
                    y: 0.2653,
                },
                green_point: PointF {
                    x: 0.1596,
                    y: 0.8404,
                },
                blue_point: PointF {
                    x: 0.0366,
                    y: 0.0001,
                },
            },
            Primaries::Bt2020 => Self {
                white_point: Self::D65,
                red_point: PointF { x: 0.708, y: 0.292 },
                green_point: PointF { x: 0.170, y: 0.797 },
                blue_point: PointF { x: 0.131, y: 0.046 },
            },
            Primaries::Custom => Self::from_primaries(Primaries::Srgb),
        }
    }
}

/// Color space defining gamut and tonal response curve (`QColorSpace`).
#[derive(Debug, Clone, PartialEq)]
pub struct ColorSpace {
    primaries: Primaries,
    primary_points: PrimaryPoints,
    transfer_function: TransferFunction,
    description: String,
}

impl Default for ColorSpace {
    fn default() -> Self {
        Self::srgb()
    }
}

impl ColorSpace {
    /// Creates a standard sRGB color space.
    pub fn srgb() -> Self {
        Self {
            primaries: Primaries::Srgb,
            primary_points: PrimaryPoints::from_primaries(Primaries::Srgb),
            transfer_function: TransferFunction::Srgb,
            description: "sRGB IEC61966-2.1".to_string(),
        }
    }

    /// Creates a Linear sRGB color space (gamma 1.0).
    pub fn linear_srgb() -> Self {
        Self {
            primaries: Primaries::Srgb,
            primary_points: PrimaryPoints::from_primaries(Primaries::Srgb),
            transfer_function: TransferFunction::Linear,
            description: "Linear sRGB".to_string(),
        }
    }

    /// Creates an Apple Display P3 color space.
    pub fn display_p3() -> Self {
        Self {
            primaries: Primaries::DciP3D65,
            primary_points: PrimaryPoints::from_primaries(Primaries::DciP3D65),
            transfer_function: TransferFunction::Srgb,
            description: "Display P3".to_string(),
        }
    }

    /// Creates an Adobe RGB (1998) color space.
    pub fn adobe_rgb() -> Self {
        Self {
            primaries: Primaries::AdobeRgb,
            primary_points: PrimaryPoints::from_primaries(Primaries::AdobeRgb),
            transfer_function: TransferFunction::Gamma(2.2),
            description: "Adobe RGB (1998)".to_string(),
        }
    }

    /// Creates a BT.2020 wide color gamut space.
    pub fn bt2020() -> Self {
        Self {
            primaries: Primaries::Bt2020,
            primary_points: PrimaryPoints::from_primaries(Primaries::Bt2020),
            transfer_function: TransferFunction::Bt2020,
            description: "ITU-R BT.2020".to_string(),
        }
    }

    /// Creates a BT.2100 HDR space with Perceptual Quantizer (PQ, ST 2084).
    pub fn bt2100_pq() -> Self {
        Self {
            primaries: Primaries::Bt2020,
            primary_points: PrimaryPoints::from_primaries(Primaries::Bt2020),
            transfer_function: TransferFunction::St2084,
            description: "ITU-R BT.2100-PQ".to_string(),
        }
    }

    /// Creates a BT.2100 HDR space with Hybrid Log-Gamma (HLG).
    pub fn bt2100_hlg() -> Self {
        Self {
            primaries: Primaries::Bt2020,
            primary_points: PrimaryPoints::from_primaries(Primaries::Bt2020),
            transfer_function: TransferFunction::Hlg,
            description: "ITU-R BT.2100-HLG".to_string(),
        }
    }

    /// Constructs from a named color space.
    pub fn from_named(named: NamedColorSpace) -> Self {
        match named {
            NamedColorSpace::Srgb => Self::srgb(),
            NamedColorSpace::LinearSrgb => Self::linear_srgb(),
            NamedColorSpace::AdobeRgb => Self::adobe_rgb(),
            NamedColorSpace::DisplayP3 => Self::display_p3(),
            NamedColorSpace::ProPhotoRgb => Self {
                primaries: Primaries::ProPhotoRgb,
                primary_points: PrimaryPoints::from_primaries(Primaries::ProPhotoRgb),
                transfer_function: TransferFunction::Gamma(1.8),
                description: "ProPhoto RGB".to_string(),
            },
            NamedColorSpace::Bt2020 => Self::bt2020(),
            NamedColorSpace::Bt2100Pq => Self::bt2100_pq(),
            NamedColorSpace::Bt2100Hlg => Self::bt2100_hlg(),
        }
    }

    /// Returns the primaries.
    pub fn primaries(&self) -> Primaries {
        self.primaries
    }

    /// Returns the primary points.
    pub fn primary_points(&self) -> PrimaryPoints {
        self.primary_points
    }

    /// Returns the transfer function.
    pub fn transfer_function(&self) -> TransferFunction {
        self.transfer_function
    }

    /// Returns the human-readable description.
    pub fn description(&self) -> &str {
        &self.description
    }

    /// Returns whether this color space is linear (transfer function is identity).
    pub fn is_linear(&self) -> bool {
        matches!(self.transfer_function, TransferFunction::Linear)
    }

    /// Creates a clone with a modified transfer function.
    pub fn with_transfer_function(&self, tf: TransferFunction) -> Self {
        let mut copy = self.clone();
        copy.transfer_function = tf;
        copy
    }

    /// Decodes an electro-optically encoded non-linear value [0.0, 1.0] to linear light.
    pub fn to_linear(&self, encoded: f32) -> f32 {
        if encoded <= 0.0 {
            return 0.0;
        }
        match self.transfer_function {
            TransferFunction::Linear => encoded,
            TransferFunction::Gamma(g) => {
                if g > 0.0 {
                    encoded.powf(g)
                } else {
                    encoded
                }
            }
            TransferFunction::Srgb => {
                if encoded <= 0.04045 {
                    encoded / 12.92
                } else {
                    ((encoded + 0.055) / 1.055).powf(2.4)
                }
            }
            TransferFunction::Bt2020 => {
                let alpha = 1.099;
                let beta = 0.018;
                if encoded < beta * 4.5 {
                    encoded / 4.5
                } else {
                    ((encoded + (alpha - 1.0)) / alpha).powf(1.0 / 0.45)
                }
            }
            TransferFunction::St2084 => {
                // SMPTE ST 2084 Perceptual Quantizer (PQ) inverse
                let m1 = 2610.0 / 16384.0;
                let m2 = 2523.0 / 4096.0 * 128.0;
                let c1 = 3424.0 / 4096.0;
                let c2 = 2413.0 / 4096.0 * 32.0;
                let c3 = 2392.0 / 4096.0 * 32.0;
                let vp = encoded.powf(1.0 / m2);
                let num = (vp - c1).max(0.0);
                let den = c2 - c3 * vp;
                if den <= 0.0 {
                    0.0
                } else {
                    (num / den).powf(1.0 / m1)
                }
            }
            TransferFunction::Hlg => {
                // ARIB STD-B67 (HLG) inverse
                let a = 0.17883277f32;
                let b = 1.0f32 - 4.0f32 * a;
                let c = 0.5f32 - a * (4.0f32 * a).ln();
                if encoded <= 0.5 {
                    (encoded * encoded) / 3.0
                } else {
                    (((encoded - c) / a).exp() + b) / 12.0
                }
            }
            TransferFunction::Custom => encoded,
        }
    }

    /// Encodes linear light value to non-linear signal representation.
    pub fn to_encoded(&self, linear: f32) -> f32 {
        if linear <= 0.0 {
            return 0.0;
        }
        match self.transfer_function {
            TransferFunction::Linear => linear,
            TransferFunction::Gamma(g) => {
                if g > 0.0 {
                    linear.powf(1.0 / g)
                } else {
                    linear
                }
            }
            TransferFunction::Srgb => {
                if linear <= 0.0031308 {
                    linear * 12.92
                } else {
                    1.055 * linear.powf(1.0 / 2.4) - 0.055
                }
            }
            TransferFunction::Bt2020 => {
                let alpha = 1.099;
                let beta = 0.018;
                if linear < beta {
                    linear * 4.5
                } else {
                    alpha * linear.powf(0.45) - (alpha - 1.0)
                }
            }
            TransferFunction::St2084 => {
                // SMPTE ST 2084 PQ forward
                let m1 = 2610.0 / 16384.0;
                let m2 = 2523.0 / 4096.0 * 128.0;
                let c1 = 3424.0 / 4096.0;
                let c2 = 2413.0 / 4096.0 * 32.0;
                let c3 = 2392.0 / 4096.0 * 32.0;
                let y = linear.powf(m1);
                ((c1 + c2 * y) / (1.0 + c3 * y)).powf(m2)
            }
            TransferFunction::Hlg => {
                let a = 0.17883277f32;
                let b = 1.0f32 - 4.0f32 * a;
                let c = 0.5f32 - a * (4.0f32 * a).ln();
                if linear <= 1.0 / 12.0 {
                    (3.0 * linear).sqrt()
                } else {
                    a * (12.0 * linear - b).ln() + c
                }
            }
            TransferFunction::Custom => linear,
        }
    }

    /// Computes the 3x3 RGB-to-XYZ conversion matrix based on primaries and white point.
    pub fn rgb_to_xyz_matrix(&self) -> [[f32; 3]; 3] {
        let pts = self.primary_points;
        let xr = pts.red_point.x;
        let yr = pts.red_point.y;
        let zr = 1.0 - xr - yr;

        let xg = pts.green_point.x;
        let yg = pts.green_point.y;
        let zg = 1.0 - xg - yg;

        let xb = pts.blue_point.x;
        let yb = pts.blue_point.y;
        let zb = 1.0 - xb - yb;

        let xw = pts.white_point.x;
        let yw = pts.white_point.y;
        let zw = 1.0 - xw - yw;

        // White point in XYZ with Y=1
        let xw_norm = xw / yw;
        let yw_norm = 1.0;
        let zw_norm = zw / yw;

        // Invert [xr xg xb; yr yg yb; zr zg zb] to solve for scaling coefficients (Sr, Sg, Sb)
        let det = xr * (yg * zb - zg * yb) - xg * (yr * zb - zr * yb) + xb * (yr * zg - zr * yg);
        if det.abs() < 1e-7 {
            return [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        }
        let inv_det = 1.0 / det;

        let i11 = (yg * zb - zg * yb) * inv_det;
        let i12 = (xb * zg - xg * zb) * inv_det;
        let i13 = (xg * yb - xb * yg) * inv_det;

        let i21 = (zg * yr - zr * yg) * inv_det;
        let i22 = (xr * zb - xb * zr) * inv_det;
        let i23 = (xb * yr - xr * yb) * inv_det;

        let i31 = (yr * zg - zr * yg) * inv_det;
        let i32 = (xg * zr - xr * zg) * inv_det;
        let i33 = (xr * yg - xg * yr) * inv_det;

        let sr = i11 * xw_norm + i12 * yw_norm + i13 * zw_norm;
        let sg = i21 * xw_norm + i22 * yw_norm + i23 * zw_norm;
        let sb = i31 * xw_norm + i32 * yw_norm + i33 * zw_norm;

        [
            [sr * xr, sg * xg, sb * xb],
            [sr * yr, sg * yg, sb * yb],
            [sr * zr, sg * zg, sb * zb],
        ]
    }

    /// Computes the transformation pipeline to convert colors from this space to `target`.
    pub fn transformation_to_color_space(&self, target: &ColorSpace) -> ColorTransform {
        ColorTransform::new(self, target)
    }
}
