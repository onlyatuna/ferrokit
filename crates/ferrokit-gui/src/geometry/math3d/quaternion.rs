// SPDX-License-Identifier: LGPL-3.0-only
//! Quaternion for 3D rotations and spherical linear interpolation (`QQuaternion` equivalent).

use super::matrix4x4::Matrix4x4;
use super::vector3d::Vector3D;
use std::f32::consts::PI;
use std::ops::{Add, Mul, Neg, Sub};

/// Quaternion representing 3D spatial rotation (`QQuaternion`).
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Quaternion {
    pub scalar: f32, // w
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Default for Quaternion {
    #[inline]
    fn default() -> Self {
        Self::identity()
    }
}

impl Quaternion {
    /// Identity quaternion (no rotation).
    #[inline]
    pub const fn identity() -> Self {
        Self {
            scalar: 1.0,
            x: 0.0,
            y: 0.0,
            z: 0.0,
        }
    }

    /// Constructs a quaternion from scalar (w) and vector (x, y, z) components.
    #[inline]
    pub const fn new(scalar: f32, x: f32, y: f32, z: f32) -> Self {
        Self { scalar, x, y, z }
    }

    /// Constructs from an axis of rotation and an angle in degrees.
    pub fn from_axis_and_angle(axis: Vector3D, angle_degrees: f32) -> Self {
        let norm_axis = axis.normalized();
        let rad = angle_degrees * 0.5 * PI / 180.0;
        let s = rad.sin();
        let c = rad.cos();
        Self {
            scalar: c,
            x: norm_axis.x * s,
            y: norm_axis.y * s,
            z: norm_axis.z * s,
        }
    }

    /// Constructs from Euler angles (pitch, yaw, roll in degrees).
    pub fn from_euler_angles(pitch: f32, yaw: f32, roll: f32) -> Self {
        let p = pitch * 0.5 * PI / 180.0;
        let y = yaw * 0.5 * PI / 180.0;
        let r = roll * 0.5 * PI / 180.0;

        let sin_p = p.sin();
        let cos_p = p.cos();
        let sin_y = y.sin();
        let cos_y = y.cos();
        let sin_r = r.sin();
        let cos_r = r.cos();

        Self {
            scalar: cos_r * cos_p * cos_y + sin_r * sin_p * sin_y,
            x: cos_r * sin_p * cos_y + sin_r * cos_p * sin_y,
            y: cos_r * cos_p * sin_y - sin_r * sin_p * cos_y,
            z: sin_r * cos_p * cos_y - cos_r * sin_p * sin_y,
        }
    }

    /// Euclidean length / norm.
    #[inline]
    pub fn length(&self) -> f32 {
        (self.scalar * self.scalar + self.x * self.x + self.y * self.y + self.z * self.z).sqrt()
    }

    /// Squared length.
    #[inline]
    pub fn length_squared(&self) -> f32 {
        self.scalar * self.scalar + self.x * self.x + self.y * self.y + self.z * self.z
    }

    /// Returns normalized unit quaternion.
    pub fn normalized(&self) -> Self {
        let len = self.length();
        if len > 1e-7 {
            let inv = 1.0 / len;
            Self {
                scalar: self.scalar * inv,
                x: self.x * inv,
                y: self.y * inv,
                z: self.z * inv,
            }
        } else {
            Self::identity()
        }
    }

    /// Normalizes this quaternion in place.
    pub fn normalize(&mut self) {
        *self = self.normalized();
    }

    /// Conjugate of the quaternion (negates vector part).
    #[inline]
    pub const fn conjugate(&self) -> Self {
        Self {
            scalar: self.scalar,
            x: -self.x,
            y: -self.y,
            z: -self.z,
        }
    }

    /// Inverted quaternion (q^-1 = conjugate / |q|^2).
    pub fn inverted(&self) -> Self {
        let len_sq = self.length_squared();
        if len_sq > 1e-7 {
            let inv = 1.0 / len_sq;
            Self {
                scalar: self.scalar * inv,
                x: -self.x * inv,
                y: -self.y * inv,
                z: -self.z * inv,
            }
        } else {
            Self::identity()
        }
    }

    /// Rotates a 3D vector using this quaternion: q * v * q^-1.
    pub fn rotated_vector(&self, v: Vector3D) -> Vector3D {
        let q_vec = Vector3D::new(self.x, self.y, self.z);
        let uv = Vector3D::cross_product(q_vec, v);
        let uuv = Vector3D::cross_product(q_vec, uv);
        v + (uv * self.scalar + uuv) * 2.0
    }

    /// Converts quaternion to equivalent 4x4 rotation matrix.
    pub fn to_rotation_matrix(&self) -> Matrix4x4 {
        let q = self.normalized();
        let xx = q.x * q.x;
        let xy = q.x * q.y;
        let xz = q.x * q.z;
        let xw = q.x * q.scalar;

        let yy = q.y * q.y;
        let yz = q.y * q.z;
        let yw = q.y * q.scalar;

        let zz = q.z * q.z;
        let zw = q.z * q.scalar;

        Matrix4x4 {
            m: [
                [1.0 - 2.0 * (yy + zz), 2.0 * (xy - zw), 2.0 * (xz + yw), 0.0],
                [2.0 * (xy + zw), 1.0 - 2.0 * (xx + zz), 2.0 * (yz - xw), 0.0],
                [2.0 * (xz - yw), 2.0 * (yz + xw), 1.0 - 2.0 * (xx + yy), 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
        }
    }

    /// Spherical Linear Interpolation (slerp) between two quaternions.
    pub fn slerp(q1: Self, mut q2: Self, t: f32) -> Self {
        let mut dot = q1.scalar * q2.scalar + q1.x * q2.x + q1.y * q2.y + q1.z * q2.z;

        // Invert q2 if dot is negative to take shorter path
        if dot < 0.0 {
            q2 = -q2;
            dot = -dot;
        }

        if dot > 0.9995 {
            // Linear interpolation for very close orientations
            (q1 + (q2 - q1) * t).normalized()
        } else {
            let theta_0 = dot.acos();
            let theta = theta_0 * t;
            let sin_theta = theta.sin();
            let sin_theta_0 = theta_0.sin();

            let s1 = (theta_0 - theta).sin() / sin_theta_0;
            let s2 = sin_theta / sin_theta_0;

            q1 * s1 + q2 * s2
        }
    }

    /// Normalized Linear Interpolation (nlerp) - faster approximation of slerp.
    pub fn nlerp(q1: Self, mut q2: Self, t: f32) -> Self {
        let dot = q1.scalar * q2.scalar + q1.x * q2.x + q1.y * q2.y + q1.z * q2.z;
        if dot < 0.0 {
            q2 = -q2;
        }
        (q1 * (1.0 - t) + q2 * t).normalized()
    }
}

impl Mul for Quaternion {
    type Output = Self;
    fn mul(self, rhs: Self) -> Self::Output {
        Self {
            scalar: self.scalar * rhs.scalar - self.x * rhs.x - self.y * rhs.y - self.z * rhs.z,
            x: self.scalar * rhs.x + self.x * rhs.scalar + self.y * rhs.z - self.z * rhs.y,
            y: self.scalar * rhs.y - self.x * rhs.z + self.y * rhs.scalar + self.z * rhs.x,
            z: self.scalar * rhs.z + self.x * rhs.y - self.y * rhs.x + self.z * rhs.scalar,
        }
    }
}

impl Mul<f32> for Quaternion {
    type Output = Self;
    #[inline]
    fn mul(self, rhs: f32) -> Self::Output {
        Self {
            scalar: self.scalar * rhs,
            x: self.x * rhs,
            y: self.y * rhs,
            z: self.z * rhs,
        }
    }
}

impl Add for Quaternion {
    type Output = Self;
    #[inline]
    fn add(self, rhs: Self) -> Self::Output {
        Self {
            scalar: self.scalar + rhs.scalar,
            x: self.x + rhs.x,
            y: self.y + rhs.y,
            z: self.z + rhs.z,
        }
    }
}

impl Sub for Quaternion {
    type Output = Self;
    #[inline]
    fn sub(self, rhs: Self) -> Self::Output {
        Self {
            scalar: self.scalar - rhs.scalar,
            x: self.x - rhs.x,
            y: self.y - rhs.y,
            z: self.z - rhs.z,
        }
    }
}

impl Neg for Quaternion {
    type Output = Self;
    #[inline]
    fn neg(self) -> Self::Output {
        Self {
            scalar: -self.scalar,
            x: -self.x,
            y: -self.y,
            z: -self.z,
        }
    }
}
