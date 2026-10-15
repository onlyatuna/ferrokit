// SPDX-License-Identifier: LGPL-3.0-only
//! PainterPath: Composite 2D vector path representation (`QPainterPath` equivalent).
//!
//! Supports subpaths, line/quad/cubic bezier curves, arcs, rectangles, ellipses,
//! polygon primitives, and configurable winding/even-odd fill rules.

use crate::geometry::primitives::{PointF, RectF};
use tiny_skia::{FillRule as SkiaFillRule, Path, PathBuilder};

/// Fill rule specifying how the interior of a path is determined.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FillRule {
    /// Non-zero winding rule (Qt::WindingFill).
    #[default]
    Winding,
    /// Odd-even filling rule (Qt::OddEvenFill).
    OddEven,
}

impl From<FillRule> for SkiaFillRule {
    fn from(rule: FillRule) -> Self {
        match rule {
            FillRule::Winding => SkiaFillRule::Winding,
            FillRule::OddEven => SkiaFillRule::EvenOdd,
        }
    }
}

/// A path segment/element type.
#[derive(Debug, Clone, PartialEq)]
pub enum PathElement {
    MoveTo(PointF),
    LineTo(PointF),
    QuadTo {
        ctrl: PointF,
        to: PointF,
    },
    CubicTo {
        ctrl1: PointF,
        ctrl2: PointF,
        to: PointF,
    },
    Close,
}

/// QPainterPath equivalent: provides container and manipulation for vector paths.
#[derive(Debug, Clone, Default)]
pub struct PainterPath {
    elements: Vec<PathElement>,
    current_pos: PointF,
    start_pos: PointF,
    fill_rule: FillRule,
}

impl PainterPath {
    /// Creates a new empty painter path.
    pub fn new() -> Self {
        Self {
            elements: Vec::new(),
            current_pos: PointF::new(0.0, 0.0),
            start_pos: PointF::new(0.0, 0.0),
            fill_rule: FillRule::Winding,
        }
    }

    /// Sets the fill rule.
    pub fn set_fill_rule(&mut self, rule: FillRule) {
        self.fill_rule = rule;
    }

    /// Returns current fill rule.
    pub fn fill_rule(&self) -> FillRule {
        self.fill_rule
    }

    /// Moves the current position to `(x, y)` without drawing.
    pub fn move_to(&mut self, x: f32, y: f32) {
        let pt = PointF::new(x, y);
        self.current_pos = pt;
        self.start_pos = pt;
        self.elements.push(PathElement::MoveTo(pt));
    }

    /// Moves the current point to `p`.
    pub fn move_to_point(&mut self, p: PointF) {
        self.move_to(p.x, p.y);
    }

    /// Appends a line segment that ends at `(x, y)`.
    pub fn line_to(&mut self, x: f32, y: f32) {
        let pt = PointF::new(x, y);
        self.current_pos = pt;
        self.elements.push(PathElement::LineTo(pt));
    }

    /// Adds a line to `p`.
    pub fn line_to_point(&mut self, p: PointF) {
        self.line_to(p.x, p.y);
    }

    /// Adds a quadratic Bezier curve between current position and `(x, y)` with control point `(cx, cy)`.
    pub fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        let ctrl = PointF::new(cx, cy);
        let to = PointF::new(x, y);
        self.current_pos = to;
        self.elements.push(PathElement::QuadTo { ctrl, to });
    }

    /// Adds a cubic Bezier curve between current position and `(x, y)` with control points `(c1x, c1y)` and `(c2x, c2y)`.
    pub fn cubic_to(&mut self, c1x: f32, c1y: f32, c2x: f32, c2y: f32, x: f32, y: f32) {
        let ctrl1 = PointF::new(c1x, c1y);
        let ctrl2 = PointF::new(c2x, c2y);
        let to = PointF::new(x, y);
        self.current_pos = to;
        self.elements
            .push(PathElement::CubicTo { ctrl1, ctrl2, to });
    }

    /// Closes current subpath.
    pub fn close_subpath(&mut self) {
        if !self.elements.is_empty() {
            self.elements.push(PathElement::Close);
            self.current_pos = self.start_pos;
        }
    }

    /// Returns the current position.
    pub fn current_position(&self) -> PointF {
        self.current_pos
    }

    /// Returns true if path contains no elements.
    pub fn is_empty(&self) -> bool {
        self.elements.is_empty()
    }

    /// Clears all elements from the path.
    pub fn clear(&mut self) {
        self.elements.clear();
        self.current_pos = PointF::new(0.0, 0.0);
        self.start_pos = PointF::new(0.0, 0.0);
    }

    /// Elements of the path.
    pub fn elements(&self) -> &[PathElement] {
        &self.elements
    }

    /// Adds a rectangle to the path.
    pub fn add_rect(&mut self, rect: RectF) {
        self.move_to(rect.x, rect.y);
        self.line_to(rect.x + rect.width, rect.y);
        self.line_to(rect.x + rect.width, rect.y + rect.height);
        self.line_to(rect.x, rect.y + rect.height);
        self.close_subpath();
    }

    /// Adds a rounded rectangle to the path.
    pub fn add_rounded_rect(&mut self, rect: RectF, rx: f32, ry: f32) {
        let rx = rx.min(rect.width * 0.5);
        let ry = ry.min(rect.height * 0.5);
        if rx <= 0.0 || ry <= 0.0 {
            self.add_rect(rect);
            return;
        }

        // 4 cubic approximation constant for quarter circles: k = 4 * (sqrt(2) - 1) / 3 ≈ 0.55228475
        let kx = rx * 0.552_284_8;
        let ky = ry * 0.552_284_8;

        self.move_to(rect.x + rx, rect.y);
        self.line_to(rect.right() - rx, rect.y);
        self.cubic_to(
            rect.right() - rx + kx,
            rect.y,
            rect.right(),
            rect.y + ry - ky,
            rect.right(),
            rect.y + ry,
        );
        self.line_to(rect.right(), rect.bottom() - ry);
        self.cubic_to(
            rect.right(),
            rect.bottom() - ry + ky,
            rect.right() - rx + kx,
            rect.bottom(),
            rect.right() - rx,
            rect.bottom(),
        );
        self.line_to(rect.x + rx, rect.bottom());
        self.cubic_to(
            rect.x + rx - kx,
            rect.bottom(),
            rect.x,
            rect.bottom() - ry + ky,
            rect.x,
            rect.bottom() - ry,
        );
        self.line_to(rect.x, rect.y + ry);
        self.cubic_to(
            rect.x,
            rect.y + ry - ky,
            rect.x + rx - kx,
            rect.y,
            rect.x + rx,
            rect.y,
        );
        self.close_subpath();
    }

    /// Adds an ellipse bounding `rect` to the path.
    pub fn add_ellipse(&mut self, rect: RectF) {
        let rx = rect.width * 0.5;
        let ry = rect.height * 0.5;
        let cx = rect.x + rx;
        let cy = rect.y + ry;

        let kx = rx * 0.552_284_8;
        let ky = ry * 0.552_284_8;

        self.move_to(cx, cy - ry);
        self.cubic_to(cx + kx, cy - ry, cx + rx, cy - ky, cx + rx, cy);
        self.cubic_to(cx + rx, cy + ky, cx + kx, cy + ry, cx, cy + ry);
        self.cubic_to(cx - kx, cy + ry, cx - rx, cy + ky, cx - rx, cy);
        self.cubic_to(cx - rx, cy - ky, cx - kx, cy - ry, cx, cy - ry);
        self.close_subpath();
    }

    /// Appends another `PainterPath` to this path.
    pub fn add_path(&mut self, other: &PainterPath) {
        for elem in &other.elements {
            match elem {
                PathElement::MoveTo(p) => self.move_to(p.x, p.y),
                PathElement::LineTo(p) => self.line_to(p.x, p.y),
                PathElement::QuadTo { ctrl, to } => self.quad_to(ctrl.x, ctrl.y, to.x, to.y),
                PathElement::CubicTo { ctrl1, ctrl2, to } => {
                    self.cubic_to(ctrl1.x, ctrl1.y, ctrl2.x, ctrl2.y, to.x, to.y)
                }
                PathElement::Close => self.close_subpath(),
            }
        }
    }

    /// Calculates tight bounding box of all anchor points in the path.
    pub fn control_point_rect(&self) -> Option<RectF> {
        if self.elements.is_empty() {
            return None;
        }

        let mut min_x = f32::MAX;
        let mut min_y = f32::MAX;
        let mut max_x = f32::MIN;
        let mut max_y = f32::MIN;

        let mut update = |p: PointF| {
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
        };

        for elem in &self.elements {
            match elem {
                PathElement::MoveTo(p) | PathElement::LineTo(p) => update(*p),
                PathElement::QuadTo { ctrl, to } => {
                    update(*ctrl);
                    update(*to);
                }
                PathElement::CubicTo { ctrl1, ctrl2, to } => {
                    update(*ctrl1);
                    update(*ctrl2);
                    update(*to);
                }
                PathElement::Close => {}
            }
        }

        if min_x > max_x || min_y > max_y {
            None
        } else {
            Some(RectF::new(min_x, min_y, max_x - min_x, max_y - min_y))
        }
    }

    /// Converts to `tiny_skia::Path`.
    pub fn to_skia_path(&self) -> Option<Path> {
        if self.elements.is_empty() {
            return None;
        }
        let mut builder = PathBuilder::new();
        for elem in &self.elements {
            match elem {
                PathElement::MoveTo(p) => builder.move_to(p.x, p.y),
                PathElement::LineTo(p) => builder.line_to(p.x, p.y),
                PathElement::QuadTo { ctrl, to } => builder.quad_to(ctrl.x, ctrl.y, to.x, to.y),
                PathElement::CubicTo { ctrl1, ctrl2, to } => {
                    builder.cubic_to(ctrl1.x, ctrl1.y, ctrl2.x, ctrl2.y, to.x, to.y)
                }
                PathElement::Close => builder.close(),
            }
        }
        builder.finish()
    }
}
