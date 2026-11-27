// SPDX-License-Identifier: LGPL-3.0-only
//! [`PainterPathStroker`]: converts a [`PainterPath`] into a fillable outline of its stroke via
//! `tiny_skia::Path::stroke`.

use super::path::{FillRule, PainterPath, PathElement};
use super::qpath_geom::P;
use super::qpath_geom::point_eq;
use tiny_skia::{LineCap, LineJoin, PathSegment, Stroke, StrokeDash};

/// Pen cap style at open subpath endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PenCapStyle {
    /// Stroke ends flush at the endpoint.
    Flat,
    /// Stroke extends half the pen width past the endpoint.
    #[default]
    Square,
    /// Semicircular cap with diameter equal to the pen width.
    Round,
}

/// Pen join style at vertices between adjacent segments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PenJoinStyle {
    /// Outer edges extend until they meet, clipped by the miter limit.
    Miter,
    /// Outer edges are connected by a straight bevel segment.
    #[default]
    Bevel,
    /// Outer edges are connected by a circular arc.
    Round,
    /// Miter join that falls back to bevel when the miter limit is exceeded.
    SvgMiter,
}

/// Predefined dash patterns for [`PainterPathStroker::set_dash_style`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PenStyle {
    /// Solid continuous stroke.
    Solid,
    /// Dashed stroke (`[4, 2]`).
    Dash,
    /// Dotted stroke (`[1, 2]`).
    Dot,
    /// Alternating dash-dot (`[4, 2, 1, 2]`).
    DashDot,
    /// Alternating dash-dot-dot (`[4, 2, 1, 2, 1, 2]`).
    DashDotDot,
}

/// Maximum number of path elements emitted by a single stroke operation.
pub(crate) const MAX_STROKE_ELEMENTS: usize = 1 << 20;

const MIN_CURVE_THRESHOLD: f64 = 0.00025;

/// Converts a [`PainterPath`] into a fillable outline of its stroke.
#[derive(Debug, Clone, PartialEq)]
pub struct PainterPathStroker {
    width: f64,
    cap: PenCapStyle,
    join: PenJoinStyle,
    miter_limit: f64,
    curve_threshold: f64,
    dash_pattern: Vec<f64>,
    dash_offset: f64,
}

impl Default for PainterPathStroker {
    fn default() -> Self {
        Self::new()
    }
}

impl PainterPathStroker {
    /// Element budget enforced by [`create_stroke`](Self::create_stroke).
    pub const MAX_ELEMENTS: usize = MAX_STROKE_ELEMENTS;

    /// Creates a stroker with default width `1.0`, `Square` caps, `Bevel` joins, and miter limit
    /// `2.0`.
    pub fn new() -> Self {
        Self {
            width: 1.0,
            cap: PenCapStyle::Square,
            join: PenJoinStyle::Bevel,
            miter_limit: 2.0,
            curve_threshold: 0.25,
            dash_pattern: Vec::new(),
            dash_offset: 0.0,
        }
    }

    /// Sets the stroke width (`<= 0` or non-finite defaults to `1.0`) and resets the curve
    /// threshold to `clamp(1 / width, 0.00025, 0.25)`.
    pub fn set_width(&mut self, width: f64) {
        let width = if width <= 0.0 || !width.is_finite() {
            1.0
        } else {
            width
        };
        self.width = width;
        self.curve_threshold = (1.0 / width).clamp(MIN_CURVE_THRESHOLD, 0.25);
    }

    pub fn width(&self) -> f64 {
        self.width
    }

    pub fn set_cap_style(&mut self, cap: PenCapStyle) {
        self.cap = cap;
    }

    pub fn cap_style(&self) -> PenCapStyle {
        self.cap
    }

    pub fn set_join_style(&mut self, join: PenJoinStyle) {
        self.join = join;
    }

    pub fn join_style(&self) -> PenJoinStyle {
        self.join
    }

    pub fn set_miter_limit(&mut self, limit: f64) {
        self.miter_limit = limit;
    }

    pub fn miter_limit(&self) -> f64 {
        self.miter_limit
    }

    /// Sets the curve approximation threshold (`<= 0` or `NaN` defaults to `0.00025`).
    pub fn set_curve_threshold(&mut self, threshold: f64) {
        self.curve_threshold = if threshold > 0.0 {
            threshold
        } else {
            MIN_CURVE_THRESHOLD
        };
    }

    pub fn curve_threshold(&self) -> f64 {
        self.curve_threshold
    }

    pub fn set_dash_style(&mut self, style: PenStyle) {
        const SPACE: f64 = 2.0;
        const DOT: f64 = 1.0;
        const DASH: f64 = 4.0;
        self.dash_pattern = match style {
            PenStyle::Solid => Vec::new(),
            PenStyle::Dash => vec![DASH, SPACE],
            PenStyle::Dot => vec![DOT, SPACE],
            PenStyle::DashDot => vec![DASH, SPACE, DOT, SPACE],
            PenStyle::DashDotDot => vec![DASH, SPACE, DOT, SPACE, DOT, SPACE],
        };
    }

    pub fn set_dash_pattern(&mut self, pattern: &[f64]) {
        self.dash_pattern = pattern.to_vec();
    }

    pub fn dash_pattern(&self) -> &[f64] {
        &self.dash_pattern
    }

    pub fn set_dash_offset(&mut self, offset: f64) {
        self.dash_offset = offset;
    }

    pub fn dash_offset(&self) -> f64 {
        self.dash_offset
    }

    /// Generates the stroke outline of `path` with [`FillRule::Winding`] (or returns an empty
    /// copy preserving `path.fill_rule()` when `path` has no drawable segments).
    pub fn create_stroke(&self, path: &PainterPath) -> PainterPath {
        self.create_stroke_checked(path).0
    }

    /// Generates the stroke outline of `path`, also returning whether the output was truncated at
    /// [`MAX_STROKE_ELEMENTS`].
    pub fn create_stroke_checked(&self, path: &PainterPath) -> (PainterPath, bool) {
        let has_segments = path
            .elements()
            .iter()
            .any(|e| !matches!(e, PathElement::MoveTo(_)));
        if !has_segments {
            return (path.clone(), false);
        }

        let Some(skia_path) = path.to_skia_path() else {
            return (path.clone(), false);
        };

        let dash = match self.resolve_dash(path) {
            DashPlan::Solid => None,
            DashPlan::Empty => {
                return (empty_winding(), false);
            }
            DashPlan::Pattern(d) => Some(d),
        };

        let stroke = Stroke {
            width: self.width as f32,
            miter_limit: if self.miter_limit.is_finite() {
                (self.miter_limit * 2.0).max(0.0) as f32
            } else {
                4.0
            },
            line_cap: match self.cap {
                PenCapStyle::Flat => LineCap::Butt,
                PenCapStyle::Square => LineCap::Square,
                PenCapStyle::Round => LineCap::Round,
            },
            line_join: match self.join {
                PenJoinStyle::Bevel => LineJoin::Bevel,
                PenJoinStyle::Round => LineJoin::Round,
                PenJoinStyle::Miter | PenJoinStyle::SvgMiter => LineJoin::Miter,
            },
            dash,
        };

        let res_scale = (0.25 / self.curve_threshold).clamp(0.25, 16.0) as f32;
        let Some(stroked) = skia_path.stroke(&stroke, res_scale) else {
            return (empty_winding(), false);
        };

        convert_skia_path(&stroked, MAX_STROKE_ELEMENTS)
    }

    fn resolve_dash(&self, path: &PainterPath) -> DashPlan {
        if self.dash_pattern.is_empty()
            || self.dash_pattern.iter().any(|&d| !d.is_finite() || d < 0.0)
        {
            return DashPlan::Solid;
        }
        let clamped_len = self.dash_pattern.len().min(32);
        let used = if clamped_len == 1 {
            1
        } else {
            clamped_len & !1
        };
        let scaled: Vec<f64> = self.dash_pattern[..used]
            .iter()
            .map(|&d| d * self.width)
            .collect();
        let cycle: f64 = scaled.iter().sum();
        if cycle <= 0.0 || !cycle.is_finite() {
            return DashPlan::Empty;
        }

        // If the path is so long that dashing would exceed ~10,000 cycles (or lose f64 precision),
        // fall back to a solid stroke to avoid unbounded dash iteration.
        let approx_len = estimate_path_length(path);
        if !approx_len.is_finite() || approx_len / cycle > 10_000.0 {
            return DashPlan::Solid;
        }

        let offset = if self.dash_offset.is_finite() {
            (self.dash_offset * self.width).rem_euclid(cycle)
        } else {
            0.0
        };
        let intervals: Vec<f32> = if scaled.len() == 1 {
            vec![scaled[0] as f32, scaled[0] as f32]
        } else {
            scaled.into_iter().map(|v| v as f32).collect()
        };
        if intervals.iter().any(|&v| !v.is_finite()) {
            return DashPlan::Empty;
        }
        match StrokeDash::new(intervals, offset as f32) {
            Some(dash) => DashPlan::Pattern(dash),
            None => DashPlan::Solid,
        }
    }
}

enum DashPlan {
    Solid,
    Empty,
    Pattern(StrokeDash),
}

fn estimate_path_length(path: &PainterPath) -> f64 {
    let mut total = 0.0f64;
    let mut start = P::new(0.0, 0.0);
    let mut cur = P::new(0.0, 0.0);
    for elem in path.elements() {
        match *elem {
            PathElement::MoveTo(p) => {
                start = P::new(f64::from(p.x), f64::from(p.y));
                cur = start;
            }
            PathElement::LineTo(p) => {
                let next = P::new(f64::from(p.x), f64::from(p.y));
                total += (next.x - cur.x).hypot(next.y - cur.y);
                cur = next;
            }
            PathElement::QuadTo { ctrl, to } => {
                let c = P::new(f64::from(ctrl.x), f64::from(ctrl.y));
                let next = P::new(f64::from(to.x), f64::from(to.y));
                total += (c.x - cur.x).hypot(c.y - cur.y) + (next.x - c.x).hypot(next.y - c.y);
                cur = next;
            }
            PathElement::CubicTo { ctrl1, ctrl2, to } => {
                let c1 = P::new(f64::from(ctrl1.x), f64::from(ctrl1.y));
                let c2 = P::new(f64::from(ctrl2.x), f64::from(ctrl2.y));
                let next = P::new(f64::from(to.x), f64::from(to.y));
                total += (c1.x - cur.x).hypot(c1.y - cur.y)
                    + (c2.x - c1.x).hypot(c2.y - c1.y)
                    + (next.x - c2.x).hypot(next.y - c2.y);
                cur = next;
            }
            PathElement::Close => {
                total += (start.x - cur.x).hypot(start.y - cur.y);
                cur = start;
            }
        }
    }
    total
}

fn empty_winding() -> PainterPath {
    let mut path = PainterPath::new();
    path.set_fill_rule(FillRule::Winding);
    path
}

fn convert_skia_path(skia_path: &tiny_skia::Path, max_elements: usize) -> (PainterPath, bool) {
    let mut out = empty_winding();
    let mut truncated = false;
    let mut subpath_start = P::new(0.0, 0.0);
    let mut cur = P::new(0.0, 0.0);

    for seg in skia_path.segments() {
        if out.elements().len() + 3 > max_elements {
            truncated = true;
            break;
        }
        match seg {
            PathSegment::MoveTo(p) => {
                out.move_to(p.x, p.y);
                subpath_start = P::new(f64::from(p.x), f64::from(p.y));
                cur = subpath_start;
            }
            PathSegment::LineTo(p) => {
                let next = P::new(f64::from(p.x), f64::from(p.y));
                if !point_eq(cur, next) {
                    out.line_to(p.x, p.y);
                    cur = next;
                }
            }
            PathSegment::QuadTo(c, p) => {
                // Degree-elevate quadratic Bezier to cubic so the output contains only move/line/cubic.
                let c1x = cur.x as f32 + (2.0 / 3.0) * (c.x - cur.x as f32);
                let c1y = cur.y as f32 + (2.0 / 3.0) * (c.y - cur.y as f32);
                let c2x = p.x + (2.0 / 3.0) * (c.x - p.x);
                let c2y = p.y + (2.0 / 3.0) * (c.y - p.y);
                out.cubic_to(c1x, c1y, c2x, c2y, p.x, p.y);
                cur = P::new(f64::from(p.x), f64::from(p.y));
            }
            PathSegment::CubicTo(c1, c2, p) => {
                out.cubic_to(c1.x, c1.y, c2.x, c2.y, p.x, p.y);
                cur = P::new(f64::from(p.x), f64::from(p.y));
            }
            PathSegment::Close => {
                if !point_eq(cur, subpath_start) {
                    out.line_to(subpath_start.x as f32, subpath_start.y as f32);
                    cur = subpath_start;
                }
            }
        }
    }

    (out, truncated)
}
