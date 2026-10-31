// SPDX-License-Identifier: LGPL-3.0-only
//! PainterPath: Composite 2D vector path representation (`QPainterPath` equivalent).
//!
//! Supports subpaths, line/quad/cubic bezier curves, arcs, rectangles, ellipses,
//! polygon primitives, and configurable winding/even-odd fill rules.

use super::qpath_geom::{
    curves_for_arc, find_ellipse_coords, fuzzy_compare, is_valid_angle, is_valid_coord, point_eq,
    rect_has_valid_coords, Rc, P,
};
use crate::geometry::primitives::{PointF, RectF};
use tiny_skia::{FillRule as SkiaFillRule, Path, PathBuilder};

/// Fill rule specifying how the interior of a path is determined.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FillRule {
    /// Odd-even filling rule (`Qt::OddEvenFill`, Qt's default for `QPainterPath`).
    #[default]
    OddEven,
    /// Non-zero winding rule (`Qt::WindingFill`).
    Winding,
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

/// `QPainterPath::ElementType`. [`PainterPath`] keeps `Close` and quadratic segments as such; Qt's
/// own element list has neither, and the algorithms transcribed from Qt work on that list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PathElementType {
    MoveTo,
    LineTo,
    CurveTo,
    CurveToData,
}

/// `QPainterPath::Element`, in Qt's `qreal` (`f64`) precision.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PathElementPoint {
    pub kind: PathElementType,
    pub x: f64,
    pub y: f64,
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
    /// Creates a new empty painter path (`QPainterPath::QPainterPath()`, default fill rule
    /// [`FillRule::OddEven`]).
    pub fn new() -> Self {
        Self {
            elements: Vec::new(),
            current_pos: PointF::new(0.0, 0.0),
            start_pos: PointF::new(0.0, 0.0),
            fill_rule: FillRule::OddEven,
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

    /// `QPainterPath(const QPointF &startPoint)`: a path whose current position is `p`.
    pub fn with_start_point(p: PointF) -> Self {
        let mut path = Self::new();
        path.move_to(p.x, p.y);
        path
    }

    /// `QPainterPath::arcMoveTo(rect, angle)`: moves to the point of the ellipse in `rect` at
    /// `angle` degrees (counter-clockwise from 3 o'clock, negative is clockwise).
    ///
    /// Like Qt, a call that follows a bare move replaces that move, and an invalid (NaN,
    /// infinite) argument or a null rectangle (zero width and height) is ignored. Qt hangs/UB
    /// here; ferrokit also ignores an `angle` beyond 1e9 degrees (see [`arc_to`](Self::arc_to)).
    ///
    /// Mirrors `QPainterPath::arcMoveTo` with `qt_find_ellipse_coords` (qtbase v6.11.2,
    /// `qpainterpath.cpp`). The point is computed in `f64` as Qt does and stored as `f32`.
    pub fn arc_move_to(&mut self, rect: RectF, angle: f32) {
        let r = Rc::new(
            f64::from(rect.x),
            f64::from(rect.y),
            f64::from(rect.width),
            f64::from(rect.height),
        );
        if !rect_has_valid_coords(&r) || !is_valid_angle(f64::from(angle)) {
            return;
        }
        if r.is_null() {
            return;
        }
        let (pt, _) = find_ellipse_coords(&r, f64::from(angle), 0.0);
        self.qt_move_to(PointF::new(pt.x as f32, pt.y as f32));
    }

    /// `QPainterPath::arcTo(rect, startAngle, sweepLength)`: appends the arc of the ellipse in
    /// `rect` that starts at `start_angle` degrees and extends `sweep_length` degrees
    /// (counter-clockwise from 3 o'clock; a negative sweep runs clockwise, `|sweep| > 360` is
    /// clamped to a full turn).
    ///
    /// As in Qt, a straight line from the current position to the arc's start is added first (it
    /// is dropped when the two coincide, e.g. right after [`arc_move_to`](Self::arc_move_to) with
    /// the same angle), and the arc is made of at most five cubic Beziers: whole quadrants use
    /// the `QT_PATH_KAPPA` circle approximation and the partial quadrants at both ends are cut
    /// out of it with `QBezier::bezierOnInterval` at the parameter `qt_t_for_arc_angle` finds.
    /// Invalid arguments and a null rectangle are ignored. A path without elements starts at
    /// `(0, 0)` like Qt's.
    ///
    /// Qt hangs/UB here; ferrokit ignores the call when `|start_angle|` exceeds 1e9 degrees:
    /// Qt's `qt_curves_for_arc` keeps the number of the start quadrant in an `int`, which a
    /// larger angle overflows.
    ///
    /// Mirrors `QPainterPath::arcTo`, `qt_curves_for_arc` and `qt_t_for_arc_angle` (qtbase
    /// v6.11.2, `qpainterpath.cpp`, `qstroker.cpp`). Qt computes in `double`; so does this, and
    /// only the resulting points are narrowed to the `f32` this path stores.
    pub fn arc_to(&mut self, rect: RectF, start_angle: f32, sweep_length: f32) {
        let r = Rc::new(
            f64::from(rect.x),
            f64::from(rect.y),
            f64::from(rect.width),
            f64::from(rect.height),
        );
        let (start_angle, sweep_length) = (f64::from(start_angle), f64::from(sweep_length));
        if !rect_has_valid_coords(&r)
            || !is_valid_angle(start_angle)
            || !is_valid_coord(sweep_length)
        {
            return;
        }
        if r.is_null() {
            return;
        }
        let (start, curves) = curves_for_arc(&r, start_angle, sweep_length);
        self.qt_line_to(narrow(start));
        // Qt's `cubicTo` drops a curve whose points all equal the last element; that last element
        // is the arc's start (or, after a dropped line, equal to it) in `double`.
        let mut last = start;
        for c in curves.as_chunks::<3>().0 {
            if point_eq(last, c[0]) && point_eq(c[0], c[1]) && point_eq(c[1], c[2]) {
                continue;
            }
            let (c1, c2, e) = (narrow(c[0]), narrow(c[1]), narrow(c[2]));
            self.cubic_to(c1.x, c1.y, c2.x, c2.y, e.x, e.y);
            last = c[2];
        }
    }

    /// The first element of an empty path, which Qt's `ensureData` creates: a move to `(0, 0)`.
    pub(crate) fn ensure_qt_data(&mut self) {
        if self.elements.is_empty() {
            self.current_pos = PointF::new(0.0, 0.0);
            self.start_pos = PointF::new(0.0, 0.0);
            self.elements.push(PathElement::MoveTo(self.current_pos));
        }
    }

    /// `QPainterPath::moveTo`: a move that follows a move replaces it.
    fn qt_move_to(&mut self, p: PointF) {
        self.ensure_qt_data();
        if let Some(PathElement::MoveTo(last)) = self.elements.last_mut() {
            *last = p;
            self.current_pos = p;
            self.start_pos = p;
        } else {
            self.move_to(p.x, p.y);
        }
    }

    /// `QPainterPath::lineTo`: a line to the current position is dropped.
    fn qt_line_to(&mut self, p: PointF) {
        self.ensure_qt_data();
        if point_eq(point_p(self.current_pos), point_p(p)) {
            return;
        }
        self.line_to(p.x, p.y);
    }

    /// The path as Qt's `QPainterPath` would hold it: only move, line and cubic elements, a
    /// `Close` being the line back to the subpath's start (`QPainterPathPrivate::close`), a
    /// quadratic segment its cubic equivalent (`QPainterPath::quadTo`), and a segment that
    /// follows a close starting with a move to the closing point (`maybeMoveTo`). This is what
    /// `QPainterPathStroker` consumes.
    pub(crate) fn qt_elements(&self) -> Vec<PathElementPoint> {
        use PathElementType::{CurveTo, CurveToData, LineTo, MoveTo};
        let el = |kind, x: f64, y: f64| PathElementPoint { kind, x, y };
        let mut out: Vec<PathElementPoint> = Vec::with_capacity(self.elements.len() + 1);
        let mut require_move_to = false;
        let mut c_start = 0usize;

        fn last_p(out: &[PathElementPoint]) -> P {
            let l = out[out.len() - 1];
            P::new(l.x, l.y)
        }
        // `ensureData`, then `maybeMoveTo`
        fn prepare(out: &mut Vec<PathElementPoint>, require_move_to: &mut bool) {
            if out.is_empty() {
                out.push(PathElementPoint {
                    kind: PathElementType::MoveTo,
                    x: 0.0,
                    y: 0.0,
                });
            }
            if *require_move_to {
                let l = out[out.len() - 1];
                out.push(PathElementPoint {
                    kind: PathElementType::MoveTo,
                    ..l
                });
                *require_move_to = false;
            }
        }

        for e in &self.elements {
            match *e {
                PathElement::MoveTo(p) => {
                    if out.is_empty() {
                        out.push(el(MoveTo, 0.0, 0.0));
                    }
                    require_move_to = false;
                    let n = out.len();
                    if out[n - 1].kind == MoveTo {
                        out[n - 1].x = f64::from(p.x);
                        out[n - 1].y = f64::from(p.y);
                    } else {
                        out.push(el(MoveTo, f64::from(p.x), f64::from(p.y)));
                    }
                    c_start = out.len() - 1;
                }
                PathElement::LineTo(p) => {
                    prepare(&mut out, &mut require_move_to);
                    let q = P::new(f64::from(p.x), f64::from(p.y));
                    if point_eq(q, last_p(&out)) {
                        continue;
                    }
                    out.push(el(LineTo, q.x, q.y));
                }
                PathElement::QuadTo { ctrl, to } => {
                    let c = P::new(f64::from(ctrl.x), f64::from(ctrl.y));
                    let t = P::new(f64::from(to.x), f64::from(to.y));
                    if out.is_empty() {
                        out.push(el(MoveTo, 0.0, 0.0));
                    }
                    let prev = last_p(&out);
                    if point_eq(prev, c) && point_eq(c, t) {
                        continue;
                    }
                    let c1 = P::new((prev.x + 2.0 * c.x) / 3.0, (prev.y + 2.0 * c.y) / 3.0);
                    let c2 = P::new((t.x + 2.0 * c.x) / 3.0, (t.y + 2.0 * c.y) / 3.0);
                    prepare(&mut out, &mut require_move_to);
                    out.push(el(CurveTo, c1.x, c1.y));
                    out.push(el(CurveToData, c2.x, c2.y));
                    out.push(el(CurveToData, t.x, t.y));
                }
                PathElement::CubicTo { ctrl1, ctrl2, to } => {
                    let c1 = P::new(f64::from(ctrl1.x), f64::from(ctrl1.y));
                    let c2 = P::new(f64::from(ctrl2.x), f64::from(ctrl2.y));
                    let t = P::new(f64::from(to.x), f64::from(to.y));
                    if out.is_empty() {
                        out.push(el(MoveTo, 0.0, 0.0));
                    }
                    if point_eq(last_p(&out), c1) && point_eq(c1, c2) && point_eq(c2, t) {
                        continue;
                    }
                    prepare(&mut out, &mut require_move_to);
                    out.push(el(CurveTo, c1.x, c1.y));
                    out.push(el(CurveToData, c2.x, c2.y));
                    out.push(el(CurveToData, t.x, t.y));
                }
                PathElement::Close => {
                    // `QPainterPath::closeSubpath` does nothing on an empty path.
                    if out.len() <= 1 && out.first().is_none_or(|f| f.kind == MoveTo) {
                        continue;
                    }
                    require_move_to = true;
                    let first = out[c_start];
                    let n = out.len();
                    if first.x != out[n - 1].x || first.y != out[n - 1].y {
                        if fuzzy_compare(first.x, out[n - 1].x)
                            && fuzzy_compare(first.y, out[n - 1].y)
                        {
                            out[n - 1].x = first.x;
                            out[n - 1].y = first.y;
                        } else {
                            out.push(el(LineTo, first.x, first.y));
                        }
                    }
                }
            }
        }
        out
    }

    /// A path of move, line and cubic elements in Qt's element layout (a cubic is its
    /// `CurveTo` and two `CurveToData` elements), narrowed to `f32`.
    pub(crate) fn from_qt_elements(elements: &[PathElementPoint]) -> Self {
        let mut path = Self::new();
        let mut i = 0;
        while i < elements.len() {
            let e = elements[i];
            match e.kind {
                PathElementType::MoveTo => path.move_to(e.x as f32, e.y as f32),
                PathElementType::LineTo => path.line_to(e.x as f32, e.y as f32),
                PathElementType::CurveTo => {
                    let (c2, end) = (elements[i + 1], elements[i + 2]);
                    path.cubic_to(
                        e.x as f32,
                        e.y as f32,
                        c2.x as f32,
                        c2.y as f32,
                        end.x as f32,
                        end.y as f32,
                    );
                    i += 2;
                }
                PathElementType::CurveToData => {}
            }
            i += 1;
        }
        path
    }
}

/// A `qreal` point of an `f32` one.
fn point_p(p: PointF) -> P {
    P::new(f64::from(p.x), f64::from(p.y))
}

/// The `f32` point a `qreal` one is stored as.
fn narrow(p: P) -> PointF {
    PointF::new(p.x as f32, p.y as f32)
}
