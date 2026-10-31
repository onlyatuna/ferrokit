// SPDX-License-Identifier: LGPL-3.0-only
//! Color Transform pipeline (`QColorTransform` equivalent).
//!
//! Manages full gamut transformation between source and destination color spaces,
//! combining EOTF decoding, 3x3 chromatic matrix transformation with chromatic adaptation,
//! and target OETF encoding.

use super::color_space::ColorSpace;
use crate::geometry::transform::is_singular;

/// Determinant of a 3x3 matrix and the sum of the absolute values of its six terms, evaluated
/// in `f64`. The sum is the scale reference for [`is_singular`].
pub(crate) fn det3_with_magnitude(m: &[[f32; 3]; 3]) -> (f64, f64) {
    let a = |r: usize, c: usize| m[r][c] as f64;
    let t = [
        a(0, 0) * a(1, 1) * a(2, 2),
        a(0, 1) * a(1, 2) * a(2, 0),
        a(0, 2) * a(1, 0) * a(2, 1),
        -a(0, 2) * a(1, 1) * a(2, 0),
        -a(0, 0) * a(1, 2) * a(2, 1),
        -a(0, 1) * a(1, 0) * a(2, 2),
    ];
    (t.iter().sum(), t.iter().map(|v| v.abs()).sum())
}

/// Helper function to invert a 3x3 matrix.
///
/// Singularity is judged relative to the magnitude of the determinant's terms (as in
/// `Transform::inverted` / `Matrix4x4::inverted`), so uniformly small but well-conditioned
/// matrices invert.
fn invert_3x3(m: &[[f32; 3]; 3]) -> Option<[[f32; 3]; 3]> {
    let (det, magnitude) = det3_with_magnitude(m);
    if is_singular(det, magnitude) {
        return None;
    }
    let det = det as f32;
    if det == 0.0 || !det.is_finite() {
        return None;
    }
    let inv_det = 1.0 / det;

    Some([
        [
            (m[1][1] * m[2][2] - m[1][2] * m[2][1]) * inv_det,
            (m[0][2] * m[2][1] - m[0][1] * m[2][2]) * inv_det,
            (m[0][1] * m[1][2] - m[0][2] * m[1][1]) * inv_det,
        ],
        [
            (m[1][2] * m[2][0] - m[1][0] * m[2][2]) * inv_det,
            (m[0][0] * m[2][2] - m[0][2] * m[2][0]) * inv_det,
            (m[0][2] * m[1][0] - m[0][0] * m[1][2]) * inv_det,
        ],
        [
            (m[1][0] * m[2][1] - m[1][1] * m[2][0]) * inv_det,
            (m[0][1] * m[2][0] - m[0][0] * m[2][1]) * inv_det,
            (m[0][0] * m[1][1] - m[0][1] * m[1][0]) * inv_det,
        ],
    ])
}

/// Helper function to multiply two 3x3 matrices: A * B.
fn mul_3x3(a: &[[f32; 3]; 3], b: &[[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let mut res = [[0.0f32; 3]; 3];
    for r in 0..3 {
        for c in 0..3 {
            res[r][c] = a[r][0] * b[0][c] + a[r][1] * b[1][c] + a[r][2] * b[2][c];
        }
    }
    res
}

/// Color transformation pipeline between two `ColorSpace` instances (`QColorTransform`).
#[derive(Debug, Clone, PartialEq)]
pub struct ColorTransform {
    src_space: ColorSpace,
    dst_space: ColorSpace,
    matrix: [[f32; 3]; 3],
    is_identity: bool,
}

impl ColorTransform {
    /// Creates an identity color transform.
    pub fn identity() -> Self {
        let cs = ColorSpace::srgb();
        Self {
            src_space: cs.clone(),
            dst_space: cs,
            matrix: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            is_identity: true,
        }
    }

    /// Creates a transformation pipeline from `src` to `dst` color space.
    pub fn new(src: &ColorSpace, dst: &ColorSpace) -> Self {
        if src == dst {
            return Self {
                src_space: src.clone(),
                dst_space: dst.clone(),
                matrix: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
                is_identity: true,
            };
        }

        let m_src_to_xyz = src.rgb_to_xyz_matrix();
        let m_dst_to_xyz = dst.rgb_to_xyz_matrix();
        let m_xyz_to_dst = invert_3x3(&m_dst_to_xyz).unwrap_or([
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
        ]);

        // Chromatic adaptation (Bradford) if white points differ
        let src_wp = src.primary_points().white_point;
        let dst_wp = dst.primary_points().white_point;
        let matrix = if (src_wp.x - dst_wp.x).abs() > 1e-4 || (src_wp.y - dst_wp.y).abs() > 1e-4 {
            // Bradford adaptation matrix
            let bradford = [
                [0.8951, 0.2664, -0.1614],
                [-0.7502, 1.7135, 0.0367],
                [0.0389, -0.0685, 1.0296],
            ];
            let inv_bradford = invert_3x3(&bradford).unwrap_or(bradford);

            let src_x = src_wp.x / src_wp.y;
            let src_z = (1.0 - src_wp.x - src_wp.y) / src_wp.y;
            let dst_x = dst_wp.x / dst_wp.y;
            let dst_z = (1.0 - dst_wp.x - dst_wp.y) / dst_wp.y;

            let src_cone = [
                bradford[0][0] * src_x + bradford[0][1] * 1.0 + bradford[0][2] * src_z,
                bradford[1][0] * src_x + bradford[1][1] * 1.0 + bradford[1][2] * src_z,
                bradford[2][0] * src_x + bradford[2][1] * 1.0 + bradford[2][2] * src_z,
            ];

            let dst_cone = [
                bradford[0][0] * dst_x + bradford[0][1] * 1.0 + bradford[0][2] * dst_z,
                bradford[1][0] * dst_x + bradford[1][1] * 1.0 + bradford[1][2] * dst_z,
                bradford[2][0] * dst_x + bradford[2][1] * 1.0 + bradford[2][2] * dst_z,
            ];

            let scale = [
                dst_cone[0] / src_cone[0].max(1e-7),
                dst_cone[1] / src_cone[1].max(1e-7),
                dst_cone[2] / src_cone[2].max(1e-7),
            ];

            let adapt = [
                [
                    scale[0] * bradford[0][0],
                    scale[0] * bradford[0][1],
                    scale[0] * bradford[0][2],
                ],
                [
                    scale[1] * bradford[1][0],
                    scale[1] * bradford[1][1],
                    scale[1] * bradford[1][2],
                ],
                [
                    scale[2] * bradford[2][0],
                    scale[2] * bradford[2][1],
                    scale[2] * bradford[2][2],
                ],
            ];

            let m_adapt = mul_3x3(&inv_bradford, &adapt);
            let m_src_adapted = mul_3x3(&m_adapt, &m_src_to_xyz);
            mul_3x3(&m_xyz_to_dst, &m_src_adapted)
        } else {
            mul_3x3(&m_xyz_to_dst, &m_src_to_xyz)
        };

        Self {
            src_space: src.clone(),
            dst_space: dst.clone(),
            matrix,
            is_identity: false,
        }
    }

    /// Whether this transformation performs no operations.
    #[inline]
    pub fn is_identity(&self) -> bool {
        self.is_identity
    }

    /// Source color space.
    pub fn source_color_space(&self) -> &ColorSpace {
        &self.src_space
    }

    /// Destination color space.
    pub fn destination_color_space(&self) -> &ColorSpace {
        &self.dst_space
    }

    /// Maps normalized floating-point RGB components [0.0, 1.0] (or HDR > 1.0).
    pub fn map_rgb(&self, r: f32, g: f32, b: f32) -> (f32, f32, f32) {
        if self.is_identity {
            return (r, g, b);
        }

        // 1. Decode to linear light
        let lr = self.src_space.to_linear_channel(0, r);
        let lg = self.src_space.to_linear_channel(1, g);
        let lb = self.src_space.to_linear_channel(2, b);

        // 2. Matrix conversion
        let m = self.matrix;
        let mut tr = m[0][0] * lr + m[0][1] * lg + m[0][2] * lb;
        let mut tg = m[1][0] * lr + m[1][1] * lg + m[1][2] * lb;
        let mut tb = m[2][0] * lr + m[2][1] * lg + m[2][2] * lb;

        // Clamp negative linear light
        tr = tr.max(0.0);
        tg = tg.max(0.0);
        tb = tb.max(0.0);

        // 3. Encode to destination transfer function
        (
            self.dst_space.to_encoded_channel(0, tr),
            self.dst_space.to_encoded_channel(1, tg),
            self.dst_space.to_encoded_channel(2, tb),
        )
    }

    /// Maps normalized RGBA components. Alpha passes through untouched.
    #[inline]
    pub fn map_rgba(&self, r: f32, g: f32, b: f32, a: f32) -> (f32, f32, f32, f32) {
        let (or, og, ob) = self.map_rgb(r, g, b);
        (or, og, ob, a)
    }

    /// Maps 8-bit integer RGBA components [0..255].
    pub fn map_rgba_u8(&self, r: u8, g: u8, b: u8, a: u8) -> (u8, u8, u8, u8) {
        if self.is_identity {
            return (r, g, b, a);
        }
        let inv255 = 1.0 / 255.0;
        let (or, og, ob) = self.map_rgb(r as f32 * inv255, g as f32 * inv255, b as f32 * inv255);
        (
            (or * 255.0).round().clamp(0.0, 255.0) as u8,
            (og * 255.0).round().clamp(0.0, 255.0) as u8,
            (ob * 255.0).round().clamp(0.0, 255.0) as u8,
            a,
        )
    }
}

#[cfg(test)]
mod singularity_tests {
    use super::*;

    #[test]
    fn uniformly_small_matrix_is_invertible() {
        // det = 1e-12 would have been rejected by the old absolute 1e-8 threshold.
        let s = 1e-4f32;
        let m = [[s, 0.0, 0.0], [0.0, s, 0.0], [0.0, 0.0, s]];
        let inv = invert_3x3(&m).expect("well-conditioned at a small scale");
        for (i, row) in inv.iter().enumerate() {
            assert!((row[i] - 1.0 / s).abs() / (1.0 / s) < 1e-4);
        }
    }

    #[test]
    fn rank_deficient_matrix_is_rejected_at_any_scale() {
        for s in [1e-4f32, 1.0, 1e4] {
            let m = [
                [s, 2.0 * s, 3.0 * s],
                [2.0 * s, 4.0 * s, 6.0 * s],
                [s, 0.0, s],
            ];
            assert!(invert_3x3(&m).is_none(), "scale {s}");
        }
    }
}
