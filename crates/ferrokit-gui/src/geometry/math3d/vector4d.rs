// SPDX-License-Identifier: LGPL-3.0-only
//! 4D Vector / Homogeneous Coordinates (`QVector4D` equivalent).

use super::vector2d::Vector2D;
use super::vector3d::Vector3D;
use std::ops::{Add, AddAssign, Div, DivAssign, Mul, MulAssign, Neg, Sub, SubAssign};

/// 4D Vector (`QVector4D`).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Vector4D {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub w: f32,
}

impl Vector4D {
    /// Constructs a 4D vector from x, y, z, and w coordinates.
    #[inline]
    pub const fn new(x: f32, y: f32, z: f32, w: f32) -> Self {
        Self { x, y, z, w }
    }

    /// Constructs from a `Vector3D` and w coordinate.
    #[inline]
    pub const fn from_vector3d(v: Vector3D, w: f32) -> Self {
        Self {
            x: v.x,
            y: v.y,
            z: v.z,
            w,
        }
    }

    /// Constructs from a `Vector2D` and z, w coordinates.
    #[inline]
    pub const fn from_vector2d(v: Vector2D, z: f32, w: f32) -> Self {
        Self {
            x: v.x,
            y: v.y,
            z,
            w,
        }
    }

    /// Returns whether all components are zero.
    #[inline]
    pub fn is_null(&self) -> bool {
        self.x.abs() < f32::EPSILON
            && self.y.abs() < f32::EPSILON
            && self.z.abs() < f32::EPSILON
            && self.w.abs() < f32::EPSILON
    }

    /// Euclidean length in 4D space.
    #[inline]
    pub fn length(&self) -> f32 {
        (self.x * self.x + self.y * self.y + self.z * self.z + self.w * self.w).sqrt()
    }

    /// Squared length in 4D space.
    #[inline]
    pub fn length_squared(&self) -> f32 {
        self.x * self.x + self.y * self.y + self.z * self.z + self.w * self.w
    }

    /// Normalized unit vector.
    pub fn normalized(&self) -> Self {
        let len = self.length();
        if len > 1e-7 {
            let inv = 1.0 / len;
            Self {
                x: self.x * inv,
                y: self.y * inv,
                z: self.z * inv,
                w: self.w * inv,
            }
        } else {
            Self::default()
        }
    }

    /// Normalizes this vector in place.
    pub fn normalize(&mut self) {
        *self = self.normalized();
    }

    /// Dot product of two 4D vectors.
    #[inline]
    pub fn dot_product(v1: Self, v2: Self) -> f32 {
        v1.x * v2.x + v1.y * v2.y + v1.z * v2.z + v1.w * v2.w
    }

    /// Converts to 3D vector by dropping w.
    #[inline]
    pub const fn to_vector3d(&self) -> Vector3D {
        Vector3D {
            x: self.x,
            y: self.y,
            z: self.z,
        }
    }

    /// Converts homogeneous coordinates to Cartesian 3D coordinates (dividing by w).
    pub fn to_vector3d_affine(&self) -> Vector3D {
        if self.w.abs() > 1e-7 {
            let inv_w = 1.0 / self.w;
            Vector3D {
                x: self.x * inv_w,
                y: self.y * inv_w,
                z: self.z * inv_w,
            }
        } else {
            Vector3D {
                x: self.x,
                y: self.y,
                z: self.z,
            }
        }
    }

    /// Converts to 2D vector by dropping z and w.
    #[inline]
    pub const fn to_vector2d(&self) -> Vector2D {
        Vector2D {
            x: self.x,
            y: self.y,
        }
    }
}

impl Add for Vector4D {
    type Output = Self;
    #[inline]
    fn add(self, rhs: Self) -> Self::Output {
        Self {
            x: self.x + rhs.x,
            y: self.y + rhs.y,
            z: self.z + rhs.z,
            w: self.w + rhs.w,
        }
    }
}

impl AddAssign for Vector4D {
    #[inline]
    fn add_assign(&mut self, rhs: Self) {
        self.x += rhs.x;
        self.y += rhs.y;
        self.z += rhs.z;
        self.w += rhs.w;
    }
}

impl Sub for Vector4D {
    type Output = Self;
    #[inline]
    fn sub(self, rhs: Self) -> Self::Output {
        Self {
            x: self.x - rhs.x,
            y: self.y - rhs.y,
            z: self.z - rhs.z,
            w: self.w - rhs.w,
        }
    }
}

impl SubAssign for Vector4D {
    #[inline]
    fn sub_assign(&mut self, rhs: Self) {
        self.x -= rhs.x;
        self.y -= rhs.y;
        self.z -= rhs.z;
        self.w -= rhs.w;
    }
}

impl Mul<f32> for Vector4D {
    type Output = Self;
    #[inline]
    fn mul(self, rhs: f32) -> Self::Output {
        Self {
            x: self.x * rhs,
            y: self.y * rhs,
            z: self.z * rhs,
            w: self.w * rhs,
        }
    }
}

impl MulAssign<f32> for Vector4D {
    #[inline]
    fn mul_assign(&mut self, rhs: f32) {
        self.x *= rhs;
        self.y *= rhs;
        self.z *= rhs;
        self.w *= rhs;
    }
}

impl Div<f32> for Vector4D {
    type Output = Self;
    #[inline]
    fn div(self, rhs: f32) -> Self::Output {
        let inv = 1.0 / rhs;
        Self {
            x: self.x * inv,
            y: self.y * inv,
            z: self.z * inv,
            w: self.w * inv,
        }
    }
}

impl DivAssign<f32> for Vector4D {
    #[inline]
    fn div_assign(&mut self, rhs: f32) {
        let inv = 1.0 / rhs;
        self.x *= inv;
        self.y *= inv;
        self.z *= inv;
        self.w *= inv;
    }
}

impl Neg for Vector4D {
    type Output = Self;
    #[inline]
    fn neg(self) -> Self::Output {
        Self {
            x: -self.x,
            y: -self.y,
            z: -self.z,
            w: -self.w,
        }
    }
}
