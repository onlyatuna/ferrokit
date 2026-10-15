// SPDX-License-Identifier: LGPL-3.0-only
//! 4x4 Matrix for 3D transformations, projections, and viewports (`QMatrix4x4` equivalent).

use super::super::primitives::PointF;
use super::vector3d::Vector3D;
use super::vector4d::Vector4D;
use std::f32::consts::PI;
use std::ops::{Mul, MulAssign};

/// 4x4 Matrix with row-major representation (`QMatrix4x4`).
///
/// Elements are indexed `m[row][column]`, the first index running down the rows.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Matrix4x4 {
    pub m: [[f32; 4]; 4],
}

impl Default for Matrix4x4 {
    #[inline]
    fn default() -> Self {
        Self::identity()
    }
}

impl Matrix4x4 {
    /// Constructs an identity 4x4 matrix.
    pub const fn identity() -> Self {
        Self {
            m: [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
        }
    }

    /// Constructs from a flat 16-element array in row-major order.
    pub const fn from_array(values: [f32; 16]) -> Self {
        Self {
            m: [
                [values[0], values[1], values[2], values[3]],
                [values[4], values[5], values[6], values[7]],
                [values[8], values[9], values[10], values[11]],
                [values[12], values[13], values[14], values[15]],
            ],
        }
    }

    /// Returns a flat array of 16 elements in row-major order.
    pub fn to_array(&self) -> [f32; 16] {
        [
            self.m[0][0],
            self.m[0][1],
            self.m[0][2],
            self.m[0][3],
            self.m[1][0],
            self.m[1][1],
            self.m[1][2],
            self.m[1][3],
            self.m[2][0],
            self.m[2][1],
            self.m[2][2],
            self.m[2][3],
            self.m[3][0],
            self.m[3][1],
            self.m[3][2],
            self.m[3][3],
        ]
    }

    /// Multiplies this matrix by a translation matrix on the right.
    pub fn translate(&mut self, x: f32, y: f32, z: f32) {
        let mut t = Self::identity();
        t.m[0][3] = x;
        t.m[1][3] = y;
        t.m[2][3] = z;
        *self *= t;
    }

    /// Multiplies this matrix by a translation vector on the right.
    pub fn translate_vector(&mut self, v: Vector3D) {
        self.translate(v.x, v.y, v.z);
    }

    /// Multiplies this matrix by a scale matrix on the right.
    pub fn scale(&mut self, x: f32, y: f32, z: f32) {
        let mut s = Self::identity();
        s.m[0][0] = x;
        s.m[1][1] = y;
        s.m[2][2] = z;
        *self *= s;
    }

    /// Multiplies by uniform scale factor.
    pub fn scale_uniform(&mut self, factor: f32) {
        self.scale(factor, factor, factor);
    }

    /// Multiplies this matrix by a rotation matrix on the right.
    pub fn rotate(&mut self, angle_degrees: f32, x: f32, y: f32, z: f32) {
        let len = (x * x + y * y + z * z).sqrt();
        if len < 1e-7 {
            return;
        }
        let inv_len = 1.0 / len;
        let x = x * inv_len;
        let y = y * inv_len;
        let z = z * inv_len;

        let rad = angle_degrees * PI / 180.0;
        let c = rad.cos();
        let s = rad.sin();
        let omc = 1.0 - c;

        let rot = Self {
            m: [
                [
                    x * x * omc + c,
                    x * y * omc - z * s,
                    x * z * omc + y * s,
                    0.0,
                ],
                [
                    y * x * omc + z * s,
                    y * y * omc + c,
                    y * z * omc - x * s,
                    0.0,
                ],
                [
                    x * z * omc - y * s,
                    y * z * omc + x * s,
                    z * z * omc + c,
                    0.0,
                ],
                [0.0, 0.0, 0.0, 1.0],
            ],
        };
        *self *= rot;
    }

    /// Orthographic projection (`QMatrix4x4::ortho`).
    pub fn ortho(&mut self, left: f32, right: f32, bottom: f32, top: f32, near: f32, far: f32) {
        let dx = right - left;
        let dy = top - bottom;
        let dz = far - near;

        if dx.abs() < 1e-7 || dy.abs() < 1e-7 || dz.abs() < 1e-7 {
            return;
        }

        let m_ortho = Self {
            m: [
                [2.0 / dx, 0.0, 0.0, -(right + left) / dx],
                [0.0, 2.0 / dy, 0.0, -(top + bottom) / dy],
                [0.0, 0.0, -2.0 / dz, -(far + near) / dz],
                [0.0, 0.0, 0.0, 1.0],
            ],
        };
        *self *= m_ortho;
    }

    /// Perspective projection matrix (`QMatrix4x4::perspective`).
    pub fn perspective(&mut self, vertical_angle_deg: f32, aspect_ratio: f32, near: f32, far: f32) {
        let rad = vertical_angle_deg * 0.5 * PI / 180.0;
        let tan_half_fov = rad.tan();

        if tan_half_fov.abs() < 1e-7 || aspect_ratio.abs() < 1e-7 || (far - near).abs() < 1e-7 {
            return;
        }

        let m_persp = Self {
            m: [
                [1.0 / (aspect_ratio * tan_half_fov), 0.0, 0.0, 0.0],
                [0.0, 1.0 / tan_half_fov, 0.0, 0.0],
                [
                    0.0,
                    0.0,
                    -(far + near) / (far - near),
                    -(2.0 * far * near) / (far - near),
                ],
                [0.0, 0.0, -1.0, 0.0],
            ],
        };
        *self *= m_persp;
    }

    /// Look-at view matrix (`QMatrix4x4::lookAt`).
    pub fn look_at(&mut self, eye: Vector3D, center: Vector3D, up: Vector3D) {
        let f = (center - eye).normalized();
        let s = Vector3D::cross_product(f, up).normalized();
        let u = Vector3D::cross_product(s, f);

        let m_look = Self {
            m: [
                [s.x, s.y, s.z, -Vector3D::dot_product(s, eye)],
                [u.x, u.y, u.z, -Vector3D::dot_product(u, eye)],
                [-f.x, -f.y, -f.z, Vector3D::dot_product(f, eye)],
                [0.0, 0.0, 0.0, 1.0],
            ],
        };
        *self *= m_look;
    }

    /// Transposed matrix.
    pub fn transposed(&self) -> Self {
        Self {
            m: [
                [self.m[0][0], self.m[1][0], self.m[2][0], self.m[3][0]],
                [self.m[0][1], self.m[1][1], self.m[2][1], self.m[3][1]],
                [self.m[0][2], self.m[1][2], self.m[2][2], self.m[3][2]],
                [self.m[0][3], self.m[1][3], self.m[2][3], self.m[3][3]],
            ],
        }
    }

    /// Transforms a 4D vector.
    pub fn map_vector4d(&self, v: Vector4D) -> Vector4D {
        Vector4D {
            x: self.m[0][0] * v.x + self.m[0][1] * v.y + self.m[0][2] * v.z + self.m[0][3] * v.w,
            y: self.m[1][0] * v.x + self.m[1][1] * v.y + self.m[1][2] * v.z + self.m[1][3] * v.w,
            z: self.m[2][0] * v.x + self.m[2][1] * v.y + self.m[2][2] * v.z + self.m[2][3] * v.w,
            w: self.m[3][0] * v.x + self.m[3][1] * v.y + self.m[3][2] * v.z + self.m[3][3] * v.w,
        }
    }

    /// Transforms a 3D point (w = 1.0, affine division).
    pub fn map_vector3d(&self, v: Vector3D) -> Vector3D {
        self.map_vector4d(Vector4D::from_vector3d(v, 1.0))
            .to_vector3d_affine()
    }

    /// Transforms a 2D point (`PointF`).
    pub fn map_point_f(&self, p: PointF) -> PointF {
        let v = self.map_vector3d(Vector3D::new(p.x, p.y, 0.0));
        PointF::new(v.x, v.y)
    }

    /// Computes matrix determinant.
    pub fn determinant(&self) -> f32 {
        let a = &self.m;
        a[0][0]
            * (a[1][1] * (a[2][2] * a[3][3] - a[2][3] * a[3][2])
                - a[1][2] * (a[2][1] * a[3][3] - a[2][3] * a[3][1])
                + a[1][3] * (a[2][1] * a[3][2] - a[2][2] * a[3][1]))
            - a[0][1]
                * (a[1][0] * (a[2][2] * a[3][3] - a[2][3] * a[3][2])
                    - a[1][2] * (a[2][0] * a[3][3] - a[2][3] * a[3][0])
                    + a[1][3] * (a[2][0] * a[3][2] - a[2][2] * a[3][0]))
            + a[0][2]
                * (a[1][0] * (a[2][1] * a[3][3] - a[2][3] * a[3][1])
                    - a[1][1] * (a[2][0] * a[3][3] - a[2][3] * a[3][0])
                    + a[1][3] * (a[2][0] * a[3][1] - a[2][1] * a[3][0]))
            - a[0][3]
                * (a[1][0] * (a[2][1] * a[3][2] - a[2][2] * a[3][1])
                    - a[1][1] * (a[2][0] * a[3][2] - a[2][2] * a[3][0])
                    + a[1][2] * (a[2][0] * a[3][1] - a[2][1] * a[3][0]))
    }

    /// Inverts the matrix using Gaussian elimination with partial pivoting.
    #[allow(clippy::needless_range_loop)]
    pub fn inverted(&self) -> Option<Self> {
        let mut a = [
            [
                self.m[0][0],
                self.m[0][1],
                self.m[0][2],
                self.m[0][3],
                1.0,
                0.0,
                0.0,
                0.0,
            ],
            [
                self.m[1][0],
                self.m[1][1],
                self.m[1][2],
                self.m[1][3],
                0.0,
                1.0,
                0.0,
                0.0,
            ],
            [
                self.m[2][0],
                self.m[2][1],
                self.m[2][2],
                self.m[2][3],
                0.0,
                0.0,
                1.0,
                0.0,
            ],
            [
                self.m[3][0],
                self.m[3][1],
                self.m[3][2],
                self.m[3][3],
                0.0,
                0.0,
                0.0,
                1.0,
            ],
        ];

        for i in 0..4 {
            let mut max_row = i;
            for k in (i + 1)..4 {
                if a[k][i].abs() > a[max_row][i].abs() {
                    max_row = k;
                }
            }
            if a[max_row][i].abs() < 1e-9 {
                return None;
            }
            a.swap(i, max_row);

            let pivot = a[i][i];
            let inv_pivot = 1.0 / pivot;
            for val in &mut a[i] {
                *val *= inv_pivot;
            }

            for k in 0..4 {
                if k != i {
                    let factor = a[k][i];
                    for j in 0..8 {
                        a[k][j] -= factor * a[i][j];
                    }
                }
            }
        }

        Some(Self {
            m: [
                [a[0][4], a[0][5], a[0][6], a[0][7]],
                [a[1][4], a[1][5], a[1][6], a[1][7]],
                [a[2][4], a[2][5], a[2][6], a[2][7]],
                [a[3][4], a[3][5], a[3][6], a[3][7]],
            ],
        })
    }
}

impl Mul for Matrix4x4 {
    type Output = Self;
    #[allow(clippy::needless_range_loop)]
    fn mul(self, rhs: Self) -> Self::Output {
        let mut res = [[0.0f32; 4]; 4];
        for r in 0..4 {
            for c in 0..4 {
                res[r][c] = self.m[r][0] * rhs.m[0][c]
                    + self.m[r][1] * rhs.m[1][c]
                    + self.m[r][2] * rhs.m[2][c]
                    + self.m[r][3] * rhs.m[3][c];
            }
        }
        Self { m: res }
    }
}

impl MulAssign for Matrix4x4 {
    #[inline]
    fn mul_assign(&mut self, rhs: Self) {
        *self = *self * rhs;
    }
}
