// SPDX-License-Identifier: LGPL-3.0-only
//! 2D Vector in 3D/graphics space (`QVector2D` equivalent).

use super::super::primitives::{Point, PointF};
use std::ops::{Add, AddAssign, Div, DivAssign, Mul, MulAssign, Neg, Sub, SubAssign};

/// 2D Vector (`QVector2D`).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Vector2D {
    pub x: f32,
    pub y: f32,
}

impl Vector2D {
    /// Constructs a 2D vector from x and y coordinates.
    #[inline]
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    /// Constructs from a `Point`.
    #[inline]
    pub fn from_point(p: Point) -> Self {
        Self {
            x: p.x as f32,
            y: p.y as f32,
        }
    }

    /// Constructs from a `PointF`.
    #[inline]
    pub fn from_point_f(p: PointF) -> Self {
        Self { x: p.x, y: p.y }
    }

    /// Returns whether both components are zero.
    #[inline]
    pub fn is_null(&self) -> bool {
        self.x.abs() < f32::EPSILON && self.y.abs() < f32::EPSILON
    }

    /// Length / Euclidean norm of the vector.
    #[inline]
    pub fn length(&self) -> f32 {
        (self.x * self.x + self.y * self.y).sqrt()
    }

    /// Squared length (avoids square root).
    #[inline]
    pub fn length_squared(&self) -> f32 {
        self.x * self.x + self.y * self.y
    }

    /// Returns normalized unit vector (length 1.0).
    pub fn normalized(&self) -> Self {
        let len = self.length();
        if len > 1e-7 {
            let inv = 1.0 / len;
            Self {
                x: self.x * inv,
                y: self.y * inv,
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
        v1.x * v2.x + v1.y * v2.y
    }

    /// Distance to another vector/point.
    #[inline]
    pub fn distance_to_point(&self, point: Self) -> f32 {
        (*self - point).length()
    }

    /// Converts to integer `Point`.
    #[inline]
    pub fn to_point(&self) -> Point {
        Point::new(self.x.round() as i32, self.y.round() as i32)
    }

    /// Converts to floating-point `PointF`.
    #[inline]
    pub fn to_point_f(&self) -> PointF {
        PointF::new(self.x, self.y)
    }
}

impl Add for Vector2D {
    type Output = Self;
    #[inline]
    fn add(self, rhs: Self) -> Self::Output {
        Self {
            x: self.x + rhs.x,
            y: self.y + rhs.y,
        }
    }
}

impl AddAssign for Vector2D {
    #[inline]
    fn add_assign(&mut self, rhs: Self) {
        self.x += rhs.x;
        self.y += rhs.y;
    }
}

impl Sub for Vector2D {
    type Output = Self;
    #[inline]
    fn sub(self, rhs: Self) -> Self::Output {
        Self {
            x: self.x - rhs.x,
            y: self.y - rhs.y,
        }
    }
}

impl SubAssign for Vector2D {
    #[inline]
    fn sub_assign(&mut self, rhs: Self) {
        self.x -= rhs.x;
        self.y -= rhs.y;
    }
}

impl Mul<f32> for Vector2D {
    type Output = Self;
    #[inline]
    fn mul(self, rhs: f32) -> Self::Output {
        Self {
            x: self.x * rhs,
            y: self.y * rhs,
        }
    }
}

impl MulAssign<f32> for Vector2D {
    #[inline]
    fn mul_assign(&mut self, rhs: f32) {
        self.x *= rhs;
        self.y *= rhs;
    }
}

impl Div<f32> for Vector2D {
    type Output = Self;
    #[inline]
    fn div(self, rhs: f32) -> Self::Output {
        let inv = 1.0 / rhs;
        Self {
            x: self.x * inv,
            y: self.y * inv,
        }
    }
}

impl DivAssign<f32> for Vector2D {
    #[inline]
    fn div_assign(&mut self, rhs: f32) {
        let inv = 1.0 / rhs;
        self.x *= inv;
        self.y *= inv;
    }
}

impl Neg for Vector2D {
    type Output = Self;
    #[inline]
    fn neg(self) -> Self::Output {
        Self {
            x: -self.x,
            y: -self.y,
        }
    }
}
