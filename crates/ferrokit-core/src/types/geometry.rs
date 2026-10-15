// SPDX-License-Identifier: LGPL-3.0-only
//! Core geometric types (`QSize`, `QSizeF`, `QLine`, `QLineF`, `QMargins`, `QMarginsF` equivalents).

use std::fmt;

/// 2D integer size (`QSize` equivalent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Size {
    pub width: i32,
    pub height: i32,
}

impl Size {
    pub const fn new(width: i32, height: i32) -> Self {
        Self { width, height }
    }

    pub const fn is_null(&self) -> bool {
        self.width == 0 && self.height == 0
    }

    pub const fn is_empty(&self) -> bool {
        self.width <= 0 || self.height <= 0
    }

    pub const fn is_valid(&self) -> bool {
        self.width >= 0 && self.height >= 0
    }

    pub fn expanded_to(&self, other: Self) -> Self {
        Self {
            width: self.width.max(other.width),
            height: self.height.max(other.height),
        }
    }

    pub fn bounded_to(&self, other: Self) -> Self {
        Self {
            width: self.width.min(other.width),
            height: self.height.min(other.height),
        }
    }
}

impl fmt::Display for Size {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "QSize({}, {})", self.width, self.height)
    }
}

impl From<(i32, i32)> for Size {
    fn from((w, h): (i32, i32)) -> Self {
        Self::new(w, h)
    }
}

/// 2D floating-point size (`QSizeF` equivalent).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SizeF {
    pub width: f32,
    pub height: f32,
}

impl SizeF {
    pub const fn new(width: f32, height: f32) -> Self {
        Self { width, height }
    }

    pub fn is_null(&self) -> bool {
        self.width.abs() < f32::EPSILON && self.height.abs() < f32::EPSILON
    }

    pub fn is_empty(&self) -> bool {
        self.width <= 0.0 || self.height <= 0.0
    }

    pub fn is_valid(&self) -> bool {
        self.width >= 0.0 && self.height >= 0.0
    }

    pub fn expanded_to(&self, other: Self) -> Self {
        Self {
            width: self.width.max(other.width),
            height: self.height.max(other.height),
        }
    }

    pub fn bounded_to(&self, other: Self) -> Self {
        Self {
            width: self.width.min(other.width),
            height: self.height.min(other.height),
        }
    }
}

impl fmt::Display for SizeF {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "QSizeF({}, {})", self.width, self.height)
    }
}

impl From<(f32, f32)> for SizeF {
    fn from((w, h): (f32, f32)) -> Self {
        Self::new(w, h)
    }
}

/// 2D integer line segment (`QLine` equivalent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Line {
    pub x1: i32,
    pub y1: i32,
    pub x2: i32,
    pub y2: i32,
}

impl Line {
    pub const fn new(x1: i32, y1: i32, x2: i32, y2: i32) -> Self {
        Self { x1, y1, x2, y2 }
    }

    pub const fn is_null(&self) -> bool {
        self.x1 == self.x2 && self.y1 == self.y2
    }

    pub const fn dx(&self) -> i32 {
        self.x2 - self.x1
    }

    pub const fn dy(&self) -> i32 {
        self.y2 - self.y1
    }

    pub fn length(&self) -> f64 {
        let dx = self.dx() as f64;
        let dy = self.dy() as f64;
        (dx * dx + dy * dy).sqrt()
    }
}

impl fmt::Display for Line {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "QLine({}, {}, {}, {})",
            self.x1, self.y1, self.x2, self.y2
        )
    }
}

impl From<(i32, i32, i32, i32)> for Line {
    fn from((x1, y1, x2, y2): (i32, i32, i32, i32)) -> Self {
        Self::new(x1, y1, x2, y2)
    }
}

/// 2D floating-point line segment (`QLineF` equivalent).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LineF {
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
}

impl LineF {
    pub const fn new(x1: f32, y1: f32, x2: f32, y2: f32) -> Self {
        Self { x1, y1, x2, y2 }
    }

    pub fn is_null(&self) -> bool {
        (self.x1 - self.x2).abs() < f32::EPSILON && (self.y1 - self.y2).abs() < f32::EPSILON
    }

    pub const fn dx(&self) -> f32 {
        self.x2 - self.x1
    }

    pub const fn dy(&self) -> f32 {
        self.y2 - self.y1
    }

    pub fn length(&self) -> f32 {
        let dx = self.dx();
        let dy = self.dy();
        (dx * dx + dy * dy).sqrt()
    }
}

impl fmt::Display for LineF {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "QLineF({}, {}, {}, {})",
            self.x1, self.y1, self.x2, self.y2
        )
    }
}

impl From<(f32, f32, f32, f32)> for LineF {
    fn from((x1, y1, x2, y2): (f32, f32, f32, f32)) -> Self {
        Self::new(x1, y1, x2, y2)
    }
}

/// 4-side integer margins (`QMargins` equivalent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Margins {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Margins {
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    pub const fn is_null(&self) -> bool {
        self.left == 0 && self.top == 0 && self.right == 0 && self.bottom == 0
    }
}

impl fmt::Display for Margins {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "QMargins({}, {}, {}, {})",
            self.left, self.top, self.right, self.bottom
        )
    }
}

impl From<(i32, i32, i32, i32)> for Margins {
    fn from((left, top, right, bottom): (i32, i32, i32, i32)) -> Self {
        Self::new(left, top, right, bottom)
    }
}

/// 4-side floating-point margins (`QMarginsF` equivalent).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct MarginsF {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl MarginsF {
    pub const fn new(left: f32, top: f32, right: f32, bottom: f32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    pub fn is_null(&self) -> bool {
        self.left.abs() < f32::EPSILON
            && self.top.abs() < f32::EPSILON
            && self.right.abs() < f32::EPSILON
            && self.bottom.abs() < f32::EPSILON
    }
}

impl fmt::Display for MarginsF {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "QMarginsF({}, {}, {}, {})",
            self.left, self.top, self.right, self.bottom
        )
    }
}

impl From<(f32, f32, f32, f32)> for MarginsF {
    fn from((left, top, right, bottom): (f32, f32, f32, f32)) -> Self {
        Self::new(left, top, right, bottom)
    }
}
