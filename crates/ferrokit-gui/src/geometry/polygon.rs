// SPDX-License-Identifier: LGPL-3.0-only
//! 2D Polygon primitives (`QPolygon` and `QPolygonF` equivalent).
//!
//! Provides collections of points representing closed or open polygonal contours,
//! with bounding box computation, point containment testing (OddEven and Winding fill rules),
//! and affine translation.

use super::primitives::{Point, PointF, Rect, RectF};
pub use crate::paint::FillRule;

/// Polygon with integer coordinates (`QPolygon`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Polygon {
    points: Vec<Point>,
}

impl Polygon {
    /// Creates an empty polygon.
    pub const fn new() -> Self {
        Self { points: Vec::new() }
    }

    /// Creates a polygon from a slice or vector of points.
    pub fn from_points(points: Vec<Point>) -> Self {
        Self { points }
    }

    /// Creates a 4-point polygon from a rectangle.
    pub fn from_rect(r: &Rect) -> Self {
        Self {
            points: vec![
                r.top_left(),
                r.top_right(),
                r.bottom_right(),
                r.bottom_left(),
            ],
        }
    }

    #[inline]
    pub fn points(&self) -> &[Point] {
        &self.points
    }

    #[inline]
    pub fn points_mut(&mut self) -> &mut Vec<Point> {
        &mut self.points
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.points.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    #[inline]
    pub fn push(&mut self, p: Point) {
        self.points.push(p);
    }

    /// Computes the axis-aligned bounding box.
    pub fn bounding_rect(&self) -> Rect {
        if self.points.is_empty() {
            return Rect::default();
        }
        let mut min_x = self.points[0].x;
        let mut max_x = self.points[0].x;
        let mut min_y = self.points[0].y;
        let mut max_y = self.points[0].y;

        for p in &self.points[1..] {
            if p.x < min_x {
                min_x = p.x;
            }
            if p.x > max_x {
                max_x = p.x;
            }
            if p.y < min_y {
                min_y = p.y;
            }
            if p.y > max_y {
                max_y = p.y;
            }
        }

        Rect::new(min_x, min_y, max_x - min_x, max_y - min_y)
    }

    /// Tests if a point is inside the polygon using the specified fill rule.
    pub fn contains_point(&self, p: Point, fill_rule: FillRule) -> bool {
        self.to_polygon_f()
            .contains_point(p.to_point_f(), fill_rule)
    }

    /// Translates the polygon by (dx, dy).
    pub fn translate(&mut self, offset: Point) {
        for p in &mut self.points {
            *p += offset;
        }
    }

    /// Returns a translated copy.
    pub fn translated(&self, offset: Point) -> Self {
        let mut copy = self.clone();
        copy.translate(offset);
        copy
    }

    /// Converts to floating-point `PolygonF`.
    pub fn to_polygon_f(&self) -> PolygonF {
        PolygonF::from_points(self.points.iter().map(|p| (*p).to_point_f()).collect())
    }
}

/// Polygon with floating-point coordinates (`QPolygonF`).
#[derive(Debug, Clone, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PolygonF {
    points: Vec<PointF>,
}

impl PolygonF {
    /// Creates an empty floating-point polygon.
    pub const fn new() -> Self {
        Self { points: Vec::new() }
    }

    /// Creates from points.
    pub fn from_points(points: Vec<PointF>) -> Self {
        Self { points }
    }

    /// Creates a 4-point polygon from a rectangle.
    pub fn from_rect(r: &RectF) -> Self {
        Self {
            points: vec![
                r.top_left(),
                r.top_right(),
                r.bottom_right(),
                r.bottom_left(),
            ],
        }
    }

    #[inline]
    pub fn points(&self) -> &[PointF] {
        &self.points
    }

    #[inline]
    pub fn points_mut(&mut self) -> &mut Vec<PointF> {
        &mut self.points
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.points.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    #[inline]
    pub fn push(&mut self, p: PointF) {
        self.points.push(p);
    }

    /// Computes the axis-aligned bounding box.
    pub fn bounding_rect(&self) -> RectF {
        if self.points.is_empty() {
            return RectF::default();
        }
        let mut min_x = self.points[0].x;
        let mut max_x = self.points[0].x;
        let mut min_y = self.points[0].y;
        let mut max_y = self.points[0].y;

        for p in &self.points[1..] {
            if p.x < min_x {
                min_x = p.x;
            }
            if p.x > max_x {
                max_x = p.x;
            }
            if p.y < min_y {
                min_y = p.y;
            }
            if p.y > max_y {
                max_y = p.y;
            }
        }

        RectF::new(min_x, min_y, max_x - min_x, max_y - min_y)
    }

    /// Tests if a point is inside the polygon using ray casting and winding rules.
    pub fn contains_point(&self, p: PointF, fill_rule: FillRule) -> bool {
        let n = self.points.len();
        if n < 3 {
            return false;
        }

        match fill_rule {
            FillRule::OddEven => {
                let mut inside = false;
                let mut j = n - 1;
                for i in 0..n {
                    let pi = self.points[i];
                    let pj = self.points[j];
                    if ((pi.y > p.y) != (pj.y > p.y))
                        && (p.x < (pj.x - pi.x) * (p.y - pi.y) / (pj.y - pi.y) + pi.x)
                    {
                        inside = !inside;
                    }
                    j = i;
                }
                inside
            }
            FillRule::Winding => {
                let mut winding_number = 0i32;
                let mut j = n - 1;
                for i in 0..n {
                    let pi = self.points[i];
                    let pj = self.points[j];
                    if pi.y <= p.y {
                        if pj.y > p.y {
                            // Upward edge
                            let cross = (pj.x - pi.x) * (p.y - pi.y) - (p.x - pi.x) * (pj.y - pi.y);
                            if cross > 0.0 {
                                winding_number += 1;
                            }
                        }
                    } else if pj.y <= p.y {
                        // Downward edge
                        let cross = (pj.x - pi.x) * (p.y - pi.y) - (p.x - pi.x) * (pj.y - pi.y);
                        if cross < 0.0 {
                            winding_number -= 1;
                        }
                    }
                    j = i;
                }
                winding_number != 0
            }
        }
    }

    /// Translates the polygon by (dx, dy).
    pub fn translate(&mut self, offset: PointF) {
        for p in &mut self.points {
            *p += offset;
        }
    }

    /// Returns a translated copy.
    pub fn translated(&self, offset: PointF) -> Self {
        let mut copy = self.clone();
        copy.translate(offset);
        copy
    }

    /// Converts to rounded integer `Polygon`.
    pub fn to_polygon(&self) -> Polygon {
        Polygon::from_points(self.points.iter().map(|p| (*p).to_point()).collect())
    }
}
