// SPDX-License-Identifier: LGPL-3.0-only
//! 2D Line primitives (`QLine` and `QLineF` equivalent).
//!
//! Provides geometric line segments in integer (`Line`) and floating-point (`LineF`)
//! coordinates, with length, angle, intersection detection, unit vectors, and normal vectors.

use super::primitives::{Point, PointF};
use std::f32::consts::PI;

/// Intersection type between two lines (`QLineF::IntersectionType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IntersectionType {
    NoIntersection,
    BoundedIntersection,
    UnboundedIntersection,
}

/// 2D line segment with integer coordinates (`QLine`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Line {
    pub p1: Point,
    pub p2: Point,
}

impl Line {
    /// Constructs a line connecting (x1, y1) and (x2, y2).
    #[inline]
    pub const fn new(p1: Point, p2: Point) -> Self {
        Self { p1, p2 }
    }

    /// Constructs from coordinate scalars.
    #[inline]
    pub const fn from_coords(x1: i32, y1: i32, x2: i32, y2: i32) -> Self {
        Self {
            p1: Point::new(x1, y1),
            p2: Point::new(x2, y2),
        }
    }

    #[inline]
    pub const fn is_null(&self) -> bool {
        self.p1.x == self.p2.x && self.p1.y == self.p2.y
    }

    #[inline]
    pub const fn x1(&self) -> i32 {
        self.p1.x
    }

    #[inline]
    pub const fn y1(&self) -> i32 {
        self.p1.y
    }

    #[inline]
    pub const fn x2(&self) -> i32 {
        self.p2.x
    }

    #[inline]
    pub const fn y2(&self) -> i32 {
        self.p2.y
    }

    #[inline]
    pub const fn dx(&self) -> i32 {
        self.p2.x - self.p1.x
    }

    #[inline]
    pub const fn dy(&self) -> i32 {
        self.p2.y - self.p1.y
    }

    /// Translates by (dx, dy).
    #[inline]
    pub fn translate(&mut self, offset: Point) {
        self.p1 += offset;
        self.p2 += offset;
    }

    /// Returns a translated copy of the line.
    #[inline]
    pub fn translated(&self, offset: Point) -> Self {
        Self {
            p1: self.p1 + offset,
            p2: self.p2 + offset,
        }
    }

    /// Converts to floating-point `LineF`.
    #[inline]
    pub fn to_line_f(&self) -> LineF {
        LineF::new(self.p1.to_point_f(), self.p2.to_point_f())
    }
}

/// 2D line segment with floating-point coordinates (`QLineF`).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct LineF {
    pub p1: PointF,
    pub p2: PointF,
}

impl LineF {
    /// Constructs a floating point line connecting p1 and p2.
    #[inline]
    pub const fn new(p1: PointF, p2: PointF) -> Self {
        Self { p1, p2 }
    }

    /// Constructs from coordinate scalars.
    #[inline]
    pub const fn from_coords(x1: f32, y1: f32, x2: f32, y2: f32) -> Self {
        Self {
            p1: PointF::new(x1, y1),
            p2: PointF::new(x2, y2),
        }
    }

    #[inline]
    pub const fn is_null(&self) -> bool {
        (self.p1.x - self.p2.x).abs() < f32::EPSILON && (self.p1.y - self.p2.y).abs() < f32::EPSILON
    }

    #[inline]
    pub const fn x1(&self) -> f32 {
        self.p1.x
    }

    #[inline]
    pub const fn y1(&self) -> f32 {
        self.p1.y
    }

    #[inline]
    pub const fn x2(&self) -> f32 {
        self.p2.x
    }

    #[inline]
    pub const fn y2(&self) -> f32 {
        self.p2.y
    }

    #[inline]
    pub const fn dx(&self) -> f32 {
        self.p2.x - self.p1.x
    }

    #[inline]
    pub const fn dy(&self) -> f32 {
        self.p2.y - self.p1.y
    }

    /// Length of the line segment.
    #[inline]
    pub fn length(&self) -> f32 {
        let d_x = self.dx();
        let d_y = self.dy();
        (d_x * d_x + d_y * d_y).sqrt()
    }

    /// Resizes the line segment to the given length, preserving p1 and direction.
    pub fn set_length(&mut self, len: f32) {
        if self.is_null() {
            return;
        }
        let cur = self.length();
        if cur < 1e-6 {
            return;
        }
        let ratio = len / cur;
        self.p2 = PointF::new(self.p1.x + self.dx() * ratio, self.p1.y + self.dy() * ratio);
    }

    /// Angle in degrees in range [0, 360), counter-clockwise from positive x-axis.
    pub fn angle(&self) -> f32 {
        let rad = (-self.dy()).atan2(self.dx());
        let mut deg = rad * 180.0 / PI;
        if deg < 0.0 {
            deg += 360.0;
        }
        deg
    }

    /// Sets the angle of the line, keeping p1 and length constant.
    pub fn set_angle(&mut self, angle_degrees: f32) {
        let len = self.length();
        let rad = angle_degrees * PI / 180.0;
        self.p2 = PointF::new(self.p1.x + len * rad.cos(), self.p1.y - len * rad.sin());
    }

    /// Angle to another line in degrees [0, 360).
    pub fn angle_to(&self, other: &LineF) -> f32 {
        let mut a = other.angle() - self.angle();
        if a < 0.0 {
            a += 360.0;
        }
        a
    }

    /// Returns a unit vector with the same direction starting at p1.
    pub fn unit_vector(&self) -> Self {
        let mut u = *self;
        u.set_length(1.0);
        u
    }

    /// Returns a normal vector (rotated 90 degrees counter-clockwise).
    pub fn normal_vector(&self) -> Self {
        Self {
            p1: self.p1,
            p2: PointF::new(self.p1.x + self.dy(), self.p1.y - self.dx()),
        }
    }

    /// Interpolates along the line: t = 0.0 gives p1, t = 1.0 gives p2.
    #[inline]
    pub fn point_at(&self, t: f32) -> PointF {
        PointF::new(self.p1.x + self.dx() * t, self.p1.y + self.dy() * t)
    }

    /// Center point of the line.
    #[inline]
    pub fn center(&self) -> PointF {
        self.point_at(0.5)
    }

    /// Calculates intersection with another line (`QLineF::intersects`).
    pub fn intersects(&self, other: &LineF) -> (IntersectionType, Option<PointF>) {
        let x1 = self.x1();
        let y1 = self.y1();
        let x2 = self.x2();
        let y2 = self.y2();

        let x3 = other.x1();
        let y3 = other.y1();
        let x4 = other.x2();
        let y4 = other.y2();

        let denom = (y4 - y3) * (x2 - x1) - (x4 - x3) * (y2 - y1);
        if denom.abs() < 1e-9 {
            return (IntersectionType::NoIntersection, None);
        }

        let num_a = (x4 - x3) * (y1 - y3) - (y4 - y3) * (x1 - x3);
        let num_b = (x2 - x1) * (y1 - y3) - (y2 - y1) * (x1 - x3);

        let u_a = num_a / denom;
        let u_b = num_b / denom;

        let intersection_point = PointF::new(x1 + u_a * (x2 - x1), y1 + u_a * (y2 - y1));

        if (0.0..=1.0).contains(&u_a) && (0.0..=1.0).contains(&u_b) {
            (
                IntersectionType::BoundedIntersection,
                Some(intersection_point),
            )
        } else {
            (
                IntersectionType::UnboundedIntersection,
                Some(intersection_point),
            )
        }
    }

    /// Converts to rounded integer `Line`.
    pub fn to_line(&self) -> Line {
        Line::new(self.p1.to_point(), self.p2.to_point())
    }
}
