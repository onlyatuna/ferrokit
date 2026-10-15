// SPDX-License-Identifier: LGPL-3.0-only
//! 3D Vector (`QVector3D` equivalent).

use super::super::primitives::{Point, PointF};
use super::vector2d::Vector2D;
use std::ops::{Add, AddAssign, Div, DivAssign, Mul, MulAssign, Neg, Sub, SubAssign};

/// 3D Vector (`QVector3D`).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Vector3D {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Vector3D {
    /// Constructs a 3D vector from x, y, and z coordinates.
    #[inline]
    pub const fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }

    /// Constructs from a `Vector2D` and z coordinate.
    #[inline]
    pub const fn from_vector2d(v: Vector2D, z: f32) -> Self {
        Self { x: v.x, y: v.y, z }
    }

    /// Constructs from a `Point`.
    #[inline]
    pub fn from_point(p: Point) -> Self {
        Self {
            x: p.x as f32,
            y: p.y as f32,
            z: 0.0,
        }
    }

    /// Constructs from a `PointF`.
    #[inline]
    pub fn from_point_f(p: PointF) -> Self {
        Self {
            x: p.x,
            y: p.y,
            z: 0.0,
        }
    }

    /// Returns whether all components are zero.
    #[inline]
    pub fn is_null(&self) -> bool {
        self.x.abs() < f32::EPSILON && self.y.abs() < f32::EPSILON && self.z.abs() < f32::EPSILON
    }

    /// Length / Euclidean norm of the vector.
    #[inline]
    pub fn length(&self) -> f32 {
        (self.x * self.x + self.y * self.y + self.z * self.z).sqrt()
    }

    /// Squared length (avoids square root).
    #[inline]
    pub fn length_squared(&self) -> f32 {
        self.x * self.x + self.y * self.y + self.z * self.z
    }

    /// Returns normalized unit vector (length 1.0).
    pub fn normalized(&self) -> Self {
        let len = self.length();
        if len > 1e-7 {
            let inv = 1.0 / len;
            Self {
                x: self.x * inv,
                y: self.y * inv,
                z: self.z * inv,
            }
        } else {
            Self::default()
        }
    }

    /// Normalizes this vector in place.
    pub fn normalize(&mut self) {
        *self = self.normalized();
    }

    /// Dot product of two vectors.
    #[inline]
    pub fn dot_product(v1: Self, v2: Self) -> f32 {
        v1.x * v2.x + v1.y * v2.y + v1.z * v2.z
    }

    /// Cross product of two vectors.
    #[inline]
    pub fn cross_product(v1: Self, v2: Self) -> Self {
        Self {
            x: v1.y * v2.z - v1.z * v2.y,
            y: v1.z * v2.x - v1.x * v2.z,
            z: v1.x * v2.y - v1.y * v2.x,
        }
    }

    /// Distance to another vector/point.
    #[inline]
    pub fn distance_to_point(&self, point: Self) -> f32 {
        (*self - point).length()
    }

    /// Converts to 2D vector dropping the z coordinate.
    #[inline]
    pub const fn to_vector2d(&self) -> Vector2D {
        Vector2D {
            x: self.x,
            y: self.y,
        }
    }

    /// Converts to integer `Point` (x, y).
    #[inline]
    pub fn to_point(&self) -> Point {
        Point::new(self.x.round() as i32, self.y.round() as i32)
    }

    /// Converts to floating-point `PointF` (x, y).
    #[inline]
    pub fn to_point_f(&self) -> PointF {
        PointF::new(self.x, self.y)
    }
}

impl Add for Vector3D {
    type Output = Self;
    #[inline]
    fn add(self, rhs: Self) -> Self::Output {
        Self {
            x: self.x + rhs.x,
            y: self.y + rhs.y,
            z: self.z + rhs.z,
        }
    }
}

impl AddAssign for Vector3D {
    #[inline]
    fn add_assign(&mut self, rhs: Self) {
        self.x += rhs.x;
        self.y += rhs.y;
        self.z += rhs.z;
    }
}

impl Sub for Vector3D {
    type Output = Self;
    #[inline]
    fn sub(self, rhs: Self) -> Self::Output {
        Self {
            x: self.x - rhs.x,
            y: self.y - rhs.y,
            z: self.z - rhs.z,
        }
    }
}

impl SubAssign for Vector3D {
    #[inline]
    fn sub_assign(&mut self, rhs: Self) {
        self.x -= rhs.x;
        self.y -= rhs.y;
        self.z -= rhs.z;
    }
}

impl Mul<f32> for Vector3D {
    type Output = Self;
    #[inline]
    fn mul(self, rhs: f32) -> Self::Output {
        Self {
            x: self.x * rhs,
            y: self.y * rhs,
            z: self.z * rhs,
        }
    }
}

impl MulAssign<f32> for Vector3D {
    #[inline]
    fn mul_assign(&mut self, rhs: f32) {
        self.x *= rhs;
        self.y *= rhs;
        self.z *= rhs;
    }
}

impl Div<f32> for Vector3D {
    type Output = Self;
    #[inline]
    fn div(self, rhs: f32) -> Self::Output {
        let inv = 1.0 / rhs;
        Self {
            x: self.x * inv,
            y: self.y * inv,
            z: self.z * inv,
        }
    }
}

impl DivAssign<f32> for Vector3D {
    #[inline]
    fn div_assign(&mut self, rhs: f32) {
        let inv = 1.0 / rhs;
        self.x *= inv;
        self.y *= inv;
        self.z *= inv;
    }
}

impl Neg for Vector3D {
    type Output = Self;
    #[inline]
    fn neg(self) -> Self::Output {
        Self {
            x: -self.x,
            y: -self.y,
            z: -self.z,
        }
    }
}
