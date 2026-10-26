// SPDX-License-Identifier: LGPL-3.0-only
//! Double-precision geometry shared by `QPainterPath::arcTo` and `QPainterPathStroker`.
//!
//! Qt computes arcs and strokes in `qreal` (`double`). [`PainterPath`](super::path::PainterPath)
//! stores `f32`, so the algorithms live here in `f64` and only their results are narrowed. Each
//! item names the Qt 6.11.2 function it transcribes (`qtbase` tag `v6.11.2`):
//!
//! * `QLineF`: `src/corelib/tools/qline.cpp`, `qline.h`
//! * `QBezier`: `src/gui/painting/qbezier.cpp`, `qbezier_p.h`
//! * `qt_t_for_arc_angle`, `qt_curves_for_arc`: `src/gui/painting/qstroker.cpp`
//! * `qt_find_ellipse_coords`: `src/gui/painting/qpainterpath.cpp`
//!
//! `QFuzzyCompare` / `QFuzzyIsNull` / `QPointF::operator==` are reproduced with their `double`
//! tolerances (`1e-12`), because the strokers branch on them.

use std::ops::{Add, Mul, Neg, Sub};

/// `QT_PATH_KAPPA` (`qstroker_p.h`): control-point length of a quarter circle, per unit radius.
pub(crate) const KAPPA: f64 = 0.552_284_749_8;

/// `qFuzzyCompare(double, double)`: only meaningful when neither argument is 0.
pub(crate) fn fuzzy_compare(a: f64, b: f64) -> bool {
    (a - b).abs() * 1_000_000_000_000.0 <= a.abs().min(b.abs())
}

/// `qFuzzyIsNull(double)`.
pub(crate) fn fuzzy_is_null(d: f64) -> bool {
    d.abs() <= 0.000_000_000_001
}

/// `QtPrivate::fuzzyCompare`: `qFuzzyCompare` that also handles an argument that is 0.
fn fuzzy_compare_any(a: f64, b: f64) -> bool {
    if a == 0.0 || b == 0.0 {
        fuzzy_is_null(a - b)
    } else {
        fuzzy_compare(a, b)
    }
}

/// A point in `qreal` precision (`QPointF`).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct P {
    pub x: f64,
    pub y: f64,
}

impl P {
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    pub fn dot(self, o: P) -> f64 {
        self.x * o.x + self.y * o.y
    }
}

impl Add for P {
    type Output = P;
    fn add(self, o: P) -> P {
        P::new(self.x + o.x, self.y + o.y)
    }
}

impl Sub for P {
    type Output = P;
    fn sub(self, o: P) -> P {
        P::new(self.x - o.x, self.y - o.y)
    }
}

impl Mul<f64> for P {
    type Output = P;
    fn mul(self, c: f64) -> P {
        P::new(self.x * c, self.y * c)
    }
}

impl Neg for P {
    type Output = P;
    fn neg(self) -> P {
        P::new(-self.x, -self.y)
    }
}

/// `QPointF::operator==` (Qt 6.8+): `qFuzzyCompare` per coordinate, zero-aware.
pub(crate) fn point_eq(a: P, b: P) -> bool {
    fuzzy_compare_any(a.x, b.x) && fuzzy_compare_any(a.y, b.y)
}

/// `qfixed2d::operator==` with `qfixed = qreal` (`qstroker_p.h`): plain `qFuzzyCompare`.
pub(crate) fn fixed_eq(a: P, b: P) -> bool {
    fuzzy_compare(a.x, b.x) && fuzzy_compare(a.y, b.y)
}

/// A rectangle in `qreal` precision (`QRectF`).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct Rc {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rc {
    pub const fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }

    /// `QRectF::isNull`.
    pub fn is_null(&self) -> bool {
        self.w == 0.0 && self.h == 0.0
    }

    /// `QRectF::center`.
    pub fn center(&self) -> P {
        P::new(self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
}

/// `isValidCoord` of `qpainterpath.cpp` (`qreal` is `double`).
pub(crate) fn is_valid_coord(c: f64) -> bool {
    c.is_finite() && c.abs() < 1e128
}

/// `hasValidCoords(QRectF)` of `qpainterpath.cpp`.
pub(crate) fn rect_has_valid_coords(r: &Rc) -> bool {
    is_valid_coord(r.x) && is_valid_coord(r.y) && is_valid_coord(r.w) && is_valid_coord(r.h)
}

/// `QLineF::IntersectionType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Intersection {
    No,
    Bounded,
    Unbounded,
}

/// `QLineF`.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Line {
    pub p1: P,
    pub p2: P,
}

impl Line {
    pub const fn new(p1: P, p2: P) -> Self {
        Self { p1, p2 }
    }

    pub const fn from_xy(x1: f64, y1: f64, x2: f64, y2: f64) -> Self {
        Self {
            p1: P::new(x1, y1),
            p2: P::new(x2, y2),
        }
    }

    pub fn dx(&self) -> f64 {
        self.p2.x - self.p1.x
    }

    pub fn dy(&self) -> f64 {
        self.p2.y - self.p1.y
    }

    /// `QLineF::length` (`qHypot`).
    pub fn length(&self) -> f64 {
        self.dx().hypot(self.dy())
    }

    /// `QLineF::isNull` (`qFuzzyCompare(pt1, pt2)`).
    pub fn is_null(&self) -> bool {
        point_eq(self.p1, self.p2)
    }

    /// `QLineF::normalVector`.
    pub fn normal_vector(&self) -> Line {
        Line::new(self.p1, self.p1 + P::new(self.dy(), -self.dx()))
    }

    /// `QLineF::unitVector`.
    pub fn unit_vector(&self) -> Line {
        let x = self.dx();
        let y = self.dy();
        let len = x.hypot(y);
        Line::new(self.p1, P::new(self.p1.x + x / len, self.p1.y + y / len))
    }

    /// `QLineF::translate`.
    pub fn translate(&mut self, d: P) {
        self.p1 = self.p1 + d;
        self.p2 = self.p2 + d;
    }

    /// `QLineF::setLength`.
    pub fn set_length(&mut self, len: f64) {
        let old = self.length();
        if old > 0.0 {
            let (dx, dy) = (self.dx(), self.dy());
            self.p2 = P::new(self.p1.x + len * (dx / old), self.p1.y + len * (dy / old));
        }
    }

    /// `QLineF::pointAt`.
    pub fn point_at(&self, t: f64) -> P {
        P::new(
            self.p1.x + (self.p2.x - self.p1.x) * t,
            self.p1.y + (self.p2.y - self.p1.y) * t,
        )
    }

    /// `QLineF::angle`: degrees counter-clockwise from the +x axis, `[0, 360)`.
    pub fn angle(&self) -> f64 {
        let theta = (-self.dy()).atan2(self.dx()).to_degrees();
        let normalized = if theta < 0.0 { theta + 360.0 } else { theta };
        if fuzzy_compare(normalized, 360.0) {
            0.0
        } else {
            normalized
        }
    }

    /// `QLineF::angleTo`.
    pub fn angle_to(&self, l: &Line) -> f64 {
        if self.is_null() || l.is_null() {
            return 0.0;
        }
        let a1 = self.angle();
        let a2 = l.angle();
        let delta = a2 - a1;
        let normalized = if delta < 0.0 { delta + 360.0 } else { delta };
        if fuzzy_compare(delta, 360.0) {
            0.0
        } else {
            normalized
        }
    }

    /// `QLineF::intersects`. The point is `(0, 0)` when the lines are parallel.
    pub fn intersects(&self, l: &Line) -> (Intersection, P) {
        let a = self.p2 - self.p1;
        let b = l.p1 - l.p2;
        let c = self.p1 - l.p1;
        let denominator = a.y * b.x - a.x * b.y;
        if denominator == 0.0 || !denominator.is_finite() {
            return (Intersection::No, P::default());
        }
        let reciprocal = 1.0 / denominator;
        let na = (b.y * c.x - b.x * c.y) * reciprocal;
        let isect = self.p1 + a * na;
        if !(0.0..=1.0).contains(&na) {
            return (Intersection::Unbounded, isect);
        }
        let nb = (a.x * c.y - a.y * c.x) * reciprocal;
        if !(0.0..=1.0).contains(&nb) {
            return (Intersection::Unbounded, isect);
        }
        (Intersection::Bounded, isect)
    }
}

/// Result of one `shift` step of `QBezier::shifted`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShiftResult {
    Ok,
    Discard,
    Split,
    Circle,
}

/// `QBezier`.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Bezier {
    pub x: [f64; 4],
    pub y: [f64; 4],
}

/// One coordinate of `QBezier::parameterSplitLeft`: returns the left part, `v` becomes the right.
fn split_left_1d(v: &mut [f64; 4], t: f64) -> [f64; 4] {
    let mut l = [0.0; 4];
    l[0] = v[0];
    l[1] = v[0] + t * (v[1] - v[0]);
    l[2] = v[1] + t * (v[2] - v[1]); // temporary holding spot
    v[2] += t * (v[3] - v[2]);
    v[1] = l[2] + t * (v[2] - l[2]);
    l[2] = l[1] + t * (l[2] - l[1]);
    l[3] = l[2] + t * (v[1] - l[2]);
    v[0] = l[3];
    l
}

impl Bezier {
    /// `QBezier::fromPoints`.
    pub fn from_points(p1: P, p2: P, p3: P, p4: P) -> Self {
        Self {
            x: [p1.x, p2.x, p3.x, p4.x],
            y: [p1.y, p2.y, p3.y, p4.y],
        }
    }

    pub fn pt(&self, i: usize) -> P {
        P::new(self.x[i], self.y[i])
    }

    /// `QBezier::coefficients`.
    pub fn coefficients(t: f64) -> (f64, f64, f64, f64) {
        let m_t = 1.0 - t;
        let mut b = m_t * m_t;
        let mut c = t * t;
        let d = c * t;
        let a = b * m_t;
        b *= 3.0 * t;
        c *= 3.0 * m_t;
        (a, b, c, d)
    }

    /// `QBezier::pointAt`.
    pub fn point_at(&self, t: f64) -> P {
        let m_t = 1.0 - t;
        let one = |v: &[f64; 4]| {
            let mut a = v[0] * m_t + v[1] * t;
            let mut b = v[1] * m_t + v[2] * t;
            let c = v[2] * m_t + v[3] * t;
            a = a * m_t + b * t;
            b = b * m_t + c * t;
            a * m_t + b * t
        };
        P::new(one(&self.x), one(&self.y))
    }

    /// `QBezier::normalVector`.
    pub fn normal_vector(&self, t: f64) -> P {
        let m_t = 1.0 - t;
        let a = m_t * m_t;
        let b = t * m_t;
        let c = t * t;
        P::new(
            (self.y[1] - self.y[0]) * a + (self.y[2] - self.y[1]) * b + (self.y[3] - self.y[2]) * c,
            -(self.x[1] - self.x[0]) * a
                - (self.x[2] - self.x[1]) * b
                - (self.x[3] - self.x[2]) * c,
        )
    }

    /// `QBezier::bounds`: `(xmin, ymin, width, height)` of the control polygon.
    pub fn bounds(&self) -> Rc {
        let range = |v: &[f64; 4]| {
            let mut min = v[0];
            let mut max = v[0];
            for &c in &v[1..] {
                if c < min {
                    min = c;
                } else if c > max {
                    max = c;
                }
            }
            (min, max)
        };
        let (xmin, xmax) = range(&self.x);
        let (ymin, ymax) = range(&self.y);
        Rc::new(xmin, ymin, xmax - xmin, ymax - ymin)
    }

    /// `QBezier::split`: `(first half, second half)`.
    pub fn split(&self) -> (Bezier, Bezier) {
        let mid = |l: P, r: P| (l + r) * 0.5;
        let mid_12 = mid(self.pt(0), self.pt(1));
        let mid_23 = mid(self.pt(1), self.pt(2));
        let mid_34 = mid(self.pt(2), self.pt(3));
        let mid_12_23 = mid(mid_12, mid_23);
        let mid_23_34 = mid(mid_23, mid_34);
        let mid_all = mid(mid_12_23, mid_23_34);
        (
            Bezier::from_points(self.pt(0), mid_12, mid_12_23, mid_all),
            Bezier::from_points(mid_all, mid_23_34, mid_34, self.pt(3)),
        )
    }

    /// `QBezier::parameterSplitLeft`: returns the part before `t`, `self` becomes the rest.
    pub fn parameter_split_left(&mut self, t: f64) -> Bezier {
        Bezier {
            x: split_left_1d(&mut self.x, t),
            y: split_left_1d(&mut self.y, t),
        }
    }

    /// `QBezier::bezierOnInterval`.
    pub fn bezier_on_interval(&self, t0: f64, t1: f64) -> Bezier {
        if t0 == 0.0 && t1 == 1.0 {
            return *self;
        }
        let mut bezier = *self;
        let _ = bezier.parameter_split_left(t0);
        let true_t = (t1 - t0) / (1.0 - t0);
        bezier.parameter_split_left(true_t)
    }

    /// `QBezier::startTangent`.
    pub fn start_tangent(&self) -> Line {
        let mut tangent = Line::new(self.pt(0), self.pt(1));
        if tangent.is_null() {
            tangent = Line::new(self.pt(0), self.pt(2));
        }
        if tangent.is_null() {
            tangent = Line::new(self.pt(0), self.pt(3));
        }
        tangent
    }

    /// `QBezier::toPolygon` (with `addToPolygon`): flattens the curve, first point included.
    pub fn to_polygon(&self, threshold: f64) -> Vec<P> {
        let mut polygon = vec![self.pt(0)];
        let mut beziers = [Bezier::default(); 10];
        let mut levels = [0i32; 10];
        beziers[0] = *self;
        levels[0] = 9;
        let mut top: isize = 0;
        while top >= 0 {
            let t = top as usize;
            let b = beziers[t];
            let y4y1 = b.y[3] - b.y[0];
            let x4x1 = b.x[3] - b.x[0];
            let mut l = x4x1.abs() + y4y1.abs();
            let d;
            if l > 1.0 {
                d = (x4x1 * (b.y[0] - b.y[1]) - y4y1 * (b.x[0] - b.x[1])).abs()
                    + (x4x1 * (b.y[0] - b.y[2]) - y4y1 * (b.x[0] - b.x[2])).abs();
            } else {
                d = (b.x[0] - b.x[1]).abs()
                    + (b.y[0] - b.y[1]).abs()
                    + (b.x[0] - b.x[2]).abs()
                    + (b.y[0] - b.y[2]).abs();
                l = 1.0;
            }
            if d < threshold * l || levels[t] == 0 {
                polygon.push(b.pt(3));
                top -= 1;
            } else {
                let (first, second) = b.split();
                beziers[t + 1] = first;
                beziers[t] = second;
                levels[t] -= 1;
                levels[t + 1] = levels[t];
                top += 1;
            }
        }
        polygon
    }

    /// `QBezier::shifted`: approximates the curve moved by `offset` along its normal with at most
    /// `max_segments - 1` curves written to `curve_segments`; returns how many. `threshold` is a
    /// `float` in Qt, and so is kept here.
    pub fn shifted(
        &self,
        curve_segments: &mut [Bezier],
        max_segments: i32,
        offset: f64,
        mut threshold: f32,
    ) -> usize {
        let degenerate = fuzzy_compare(self.x[0], self.x[1])
            && fuzzy_compare(self.x[0], self.x[2])
            && fuzzy_compare(self.x[0], self.x[3])
            && fuzzy_compare(self.y[0], self.y[1])
            && fuzzy_compare(self.y[0], self.y[2])
            && fuzzy_compare(self.y[0], self.y[3]);
        if degenerate {
            return 0;
        }

        let max_segments = max_segments - 1;
        let mut beziers = [Bezier::default(); 10];
        let mut b: i32;
        let mut o: i32;
        'redo: loop {
            beziers[0] = *self;
            b = 0;
            o = 0;
            while b >= 0 {
                let stack_segments = b + 1;
                if stack_segments == 10 || o == max_segments - stack_segments {
                    threshold = (f64::from(threshold) * 1.5) as f32;
                    if f64::from(threshold) > 2.0 {
                        break 'redo; // give_up
                    }
                    continue 'redo;
                }
                let res = shift(
                    &beziers[b as usize],
                    &mut curve_segments[o as usize],
                    offset,
                    f64::from(threshold),
                );
                if res == ShiftResult::Discard {
                    b -= 1;
                } else if res == ShiftResult::Ok {
                    o += 1;
                    b -= 1;
                } else if res == ShiftResult::Circle && max_segments - o >= 2 {
                    // add semi circle
                    let (head, tail) = curve_segments.split_at_mut(o as usize + 1);
                    if add_circle(
                        &beziers[b as usize],
                        offset,
                        &mut head[o as usize],
                        &mut tail[0],
                    ) {
                        o += 2;
                    }
                    b -= 1;
                } else {
                    let (first, second) = beziers[b as usize].split();
                    beziers[b as usize + 1] = first;
                    beziers[b as usize] = second;
                    b += 1;
                }
            }
            break;
        }

        // give_up:
        while b >= 0 {
            let res = shift(
                &beziers[b as usize],
                &mut curve_segments[o as usize],
                offset,
                f64::from(threshold),
            );
            // if res isn't Ok or Split then *o is undefined
            if res == ShiftResult::Ok || res == ShiftResult::Split {
                o += 1;
            }
            b -= 1;
        }
        o as usize
    }
}

/// `good_offset` of `qbezier.cpp`.
fn good_offset(b1: &Bezier, b2: &Bezier, offset: f64, threshold: f64) -> ShiftResult {
    let o2 = offset * offset;
    let max_dist_line = threshold * offset * offset;
    let max_dist_normal = threshold * offset;
    let divisions = 4;
    let spacing = 1.0 / f64::from(divisions);
    let mut t = spacing;
    for _ in 1..divisions {
        let p1 = b1.point_at(t);
        let p2 = b2.point_at(t);
        let mut d = (p1.x - p2.x) * (p1.x - p2.x) + (p1.y - p2.y) * (p1.y - p2.y);
        if (d - o2).abs() > max_dist_line {
            return ShiftResult::Split;
        }
        let normal_point = b1.normal_vector(t);
        let l = normal_point.x.abs() + normal_point.y.abs();
        if l != 0.0 {
            d = (normal_point.x * (p1.y - p2.y) - normal_point.y * (p1.x - p2.x)).abs() / l;
            if d > max_dist_normal {
                return ShiftResult::Split;
            }
        }
        t += spacing;
    }
    ShiftResult::Ok
}

/// `shift` of `qbezier.cpp`.
fn shift(orig: &Bezier, shifted: &mut Bezier, offset: f64, threshold: f64) -> ShiftResult {
    let mut map = [0usize; 4];
    let p1_p2_equal = fuzzy_compare(orig.x[0], orig.x[1]) && fuzzy_compare(orig.y[0], orig.y[1]);
    let p2_p3_equal = fuzzy_compare(orig.x[1], orig.x[2]) && fuzzy_compare(orig.y[1], orig.y[2]);
    let p3_p4_equal = fuzzy_compare(orig.x[2], orig.x[3]) && fuzzy_compare(orig.y[2], orig.y[3]);

    let mut points = [P::default(); 4];
    let mut np = 0usize;
    points[np] = orig.pt(0);
    map[0] = 0;
    np += 1;
    if !p1_p2_equal {
        points[np] = orig.pt(1);
        np += 1;
    }
    map[1] = np - 1;
    if !p2_p3_equal {
        points[np] = orig.pt(2);
        np += 1;
    }
    map[2] = np - 1;
    if !p3_p4_equal {
        points[np] = orig.pt(3);
        np += 1;
    }
    map[3] = np - 1;
    if np == 1 {
        return ShiftResult::Discard;
    }

    let b = orig.bounds();
    if np == 4 && b.w < 0.1 * offset && b.h < 0.1 * offset {
        // Transcribed with Qt's operator precedence, which is not what the comment suggests.
        let l = (orig.x[0] - orig.x[1]) * (orig.x[0] - orig.x[1])
            + (orig.y[0] - orig.y[1])
                * (orig.y[0] - orig.y[1])
                * (orig.x[2] - orig.x[3])
                * (orig.x[2] - orig.x[3])
            + (orig.y[2] - orig.y[3]) * (orig.y[2] - orig.y[3]);
        let dot = (orig.x[0] - orig.x[1]) * (orig.x[2] - orig.x[3])
            + (orig.y[0] - orig.y[1]) * (orig.y[2] - orig.y[3]);
        if dot < 0.0 && dot * dot < 0.8 * l {
            // the points are close and reverse direction. Approximate the whole
            // thing by a semi circle
            return ShiftResult::Circle;
        }
    }

    let mut points_shifted = [P::default(); 4];

    let prev = Line::new(P::default(), points[1] - points[0]);
    if prev.length() == 0.0 {
        return ShiftResult::Discard;
    }
    let mut prev_normal = prev.normal_vector().unit_vector().p2;

    points_shifted[0] = points[0] + prev_normal * offset;

    for i in 1..np - 1 {
        let next = Line::new(P::default(), points[i + 1] - points[i]);
        let next_normal = next.normal_vector().unit_vector().p2;

        let normal_sum = prev_normal + next_normal;

        let r = 1.0 + prev_normal.x * next_normal.x + prev_normal.y * next_normal.y;

        if fuzzy_is_null(r) {
            points_shifted[i] = points[i] + prev_normal * offset;
        } else {
            let k = offset / r;
            points_shifted[i] = points[i] + normal_sum * k;
        }

        prev_normal = next_normal;
    }

    points_shifted[np - 1] = points[np - 1] + prev_normal * offset;

    *shifted = Bezier::from_points(
        points_shifted[map[0]],
        points_shifted[map[1]],
        points_shifted[map[2]],
        points_shifted[map[3]],
    );

    if np > 2 {
        return good_offset(orig, shifted, offset, threshold);
    }
    ShiftResult::Ok
}

/// `addCircle` of `qbezier.cpp`: writes two curves approximating a semi circle to `o0`, `o1`.
fn add_circle(b: &Bezier, offset: f64, o0: &mut Bezier, o1: &mut Bezier) -> bool {
    let mut normals = [P::default(); 3];

    normals[0] = P::new(b.y[1] - b.y[0], b.x[0] - b.x[1]);
    let mut dist = (normals[0].x * normals[0].x + normals[0].y * normals[0].y).sqrt();
    if fuzzy_is_null(dist) {
        return false;
    }
    normals[0] = P::new(normals[0].x / dist, normals[0].y / dist);
    normals[2] = P::new(b.y[3] - b.y[2], b.x[2] - b.x[3]);
    dist = (normals[2].x * normals[2].x + normals[2].y * normals[2].y).sqrt();
    if fuzzy_is_null(dist) {
        return false;
    }
    normals[2] = P::new(normals[2].x / dist, normals[2].y / dist);

    normals[1] = P::new(
        b.x[0] - b.x[1] - b.x[2] + b.x[3],
        b.y[0] - b.y[1] - b.y[2] + b.y[3],
    );
    let n1_len = -1.0 * (normals[1].x * normals[1].x + normals[1].y * normals[1].y).sqrt();
    normals[1] = P::new(normals[1].x / n1_len, normals[1].y / n1_len);

    let mut angles = [0.0f64; 2];
    let mut sign = 1.0;
    for i in 0..2 {
        let mut cos_a = normals[i].x * normals[i + 1].x + normals[i].y * normals[i + 1].y;
        if cos_a > 1.0 {
            cos_a = 1.0;
        }
        if cos_a < -1.0 {
            cos_a = -1.0;
        }
        angles[i] = cos_a.acos() * std::f64::consts::FRAC_1_PI;
    }

    if angles[0] + angles[1] > 1.0 {
        // more than 180 degrees
        normals[1] = -normals[1];
        angles[0] = 1.0 - angles[0];
        angles[1] = 1.0 - angles[1];
        sign = -1.0;
    }

    let mut circle = [P::default(); 3];
    circle[0] = b.pt(0) + normals[0] * offset;
    circle[1] = P::new(0.5 * (b.x[0] + b.x[3]), 0.5 * (b.y[0] + b.y[3])) + normals[1] * offset;
    circle[2] = b.pt(3) + normals[2] * offset;

    for (i, o) in [o0, o1].into_iter().enumerate() {
        let kappa = 2.0 * KAPPA * sign * offset * angles[i];

        o.x[0] = circle[i].x;
        o.y[0] = circle[i].y;
        o.x[1] = circle[i].x - normals[i].y * kappa;
        o.y[1] = circle[i].y + normals[i].x * kappa;
        o.x[2] = circle[i + 1].x + normals[i + 1].y * kappa;
        o.y[2] = circle[i + 1].y - normals[i + 1].x * kappa;
        o.x[3] = circle[i + 1].x;
        o.y[3] = circle[i + 1].y;
    }
    true
}

/// `qt_t_for_arc_angle` (`qstroker.cpp`): the parameter `t` of the prototype quarter-circle
/// bezier whose point lies at `angle` degrees, `angle` in `[0, 90]`.
pub(crate) fn t_for_arc_angle(angle: f64) -> f64 {
    if fuzzy_is_null(angle) {
        return 0.0;
    }
    if fuzzy_compare(angle, 90.0) {
        return 1.0;
    }

    let radians = angle.to_radians();
    let cos_angle = radians.cos();
    let sin_angle = radians.sin();
    let k = KAPPA;

    // initial guess
    let mut tc = angle / 90.0;
    // do some iterations of newton's method to approximate cosAngle
    // finds the zero of the function b.pointAt(tc).x() - cosAngle
    tc -= ((((2.0 - 3.0 * k) * tc + 3.0 * (k - 1.0)) * tc) * tc + 1.0 - cos_angle) // value
        / (((6.0 - 9.0 * k) * tc + 6.0 * (k - 1.0)) * tc); // derivative
    tc -= ((((2.0 - 3.0 * k) * tc + 3.0 * (k - 1.0)) * tc) * tc + 1.0 - cos_angle) // value
        / (((6.0 - 9.0 * k) * tc + 6.0 * (k - 1.0)) * tc); // derivative

    // initial guess
    let mut ts = tc;
    // do some iterations of newton's method to approximate sinAngle
    // finds the zero of the function b.pointAt(tc).y() - sinAngle
    ts -= ((((3.0 * k - 2.0) * ts - 6.0 * k + 3.0) * ts + 3.0 * k) * ts - sin_angle)
        / (((9.0 * k - 6.0) * ts + 12.0 * k - 6.0) * ts + 3.0 * k);
    ts -= ((((3.0 * k - 2.0) * ts - 6.0 * k + 3.0) * ts + 3.0 * k) * ts - sin_angle)
        / (((9.0 * k - 6.0) * ts + 12.0 * k - 6.0) * ts + 3.0 * k);

    // use the average of the t that best approximates cosAngle
    // and the t that best approximates sinAngle
    0.5 * (tc + ts)
}

/// `qt_find_ellipse_coords` (`qpainterpath.cpp`): the points of the ellipse in `r` at `angle` and
/// at `angle + length` degrees (counter-clockwise from 3 o'clock). `(start, end)`.
pub(crate) fn find_ellipse_coords(r: &Rc, angle: f64, length: f64) -> (P, P) {
    if r.is_null() {
        return (P::default(), P::default());
    }

    let w2 = r.w / 2.0;
    let h2 = r.h / 2.0;

    let angles = [angle, angle + length];
    let mut out = [P::default(); 2];

    for i in 0..2 {
        let theta = angles[i] - 360.0 * (angles[i] / 360.0).floor();
        let mut t = theta / 90.0;
        // truncate
        let quadrant = t as i32;
        t -= f64::from(quadrant);

        t = t_for_arc_angle(90.0 * t);

        // swap x and y?
        if quadrant & 1 != 0 {
            t = 1.0 - t;
        }

        let (a, b, c, d) = Bezier::coefficients(t);
        let mut p = P::new(a + b + c * KAPPA, d + c + b * KAPPA);

        // left quadrants
        if quadrant == 1 || quadrant == 2 {
            p.x = -p.x;
        }

        // top quadrants
        if quadrant == 0 || quadrant == 1 {
            p.y = -p.y;
        }

        out[i] = r.center() + P::new(w2 * p.x, h2 * p.y);
    }
    (out[0], out[1])
}

/// `qt_curves_for_arc` (`qstroker.cpp`): the arc of the ellipse in `rect` from `start_angle`
/// over `sweep_length` degrees as `(start point, control points)`; the control points are
/// consecutive triples `(c1, c2, end)` of cubic segments.
pub(crate) fn curves_for_arc(rect: &Rc, start_angle: f64, sweep_length: f64) -> (P, Vec<P>) {
    let mut curves: Vec<P> = Vec::with_capacity(15);
    if rect.x.is_nan()
        || rect.y.is_nan()
        || rect.w.is_nan()
        || rect.h.is_nan()
        || start_angle.is_nan()
        || sweep_length.is_nan()
    {
        return (P::default(), curves);
    }

    if rect.is_null() {
        return (P::default(), curves);
    }

    let x = rect.x;
    let y = rect.y;

    let w = rect.w;
    let w2 = rect.w / 2.0;
    let w2k = w2 * KAPPA;

    let h = rect.h;
    let h2 = rect.h / 2.0;
    let h2k = h2 * KAPPA;

    let points = [
        // start point
        P::new(x + w, y + h2),
        // 0 -> 270 degrees
        P::new(x + w, y + h2 + h2k),
        P::new(x + w2 + w2k, y + h),
        P::new(x + w2, y + h),
        // 270 -> 180 degrees
        P::new(x + w2 - w2k, y + h),
        P::new(x, y + h2 + h2k),
        P::new(x, y + h2),
        // 180 -> 90 degrees
        P::new(x, y + h2 - h2k),
        P::new(x + w2 - w2k, y),
        P::new(x + w2, y),
        // 90 -> 0 degrees
        P::new(x + w2 + w2k, y),
        P::new(x + w, y + h2 - h2k),
        P::new(x + w, y + h2),
    ];

    let sweep_length = sweep_length.clamp(-360.0, 360.0);

    // Special case fast paths
    if start_angle == 0.0 {
        if sweep_length == 360.0 {
            for p in points[..=11].iter().rev() {
                curves.push(*p);
            }
            return (points[12], curves);
        } else if sweep_length == -360.0 {
            curves.extend_from_slice(&points[1..=12]);
            return (points[0], curves);
        }
    }

    let mut start_segment = (start_angle / 90.0).floor() as i32;
    let mut end_segment = ((start_angle + sweep_length) / 90.0).floor() as i32;

    let mut start_t = (start_angle - f64::from(start_segment) * 90.0) / 90.0;
    let mut end_t = (start_angle + sweep_length - f64::from(end_segment) * 90.0) / 90.0;

    let delta: i32 = if sweep_length > 0.0 { 1 } else { -1 };
    if delta < 0 {
        start_t = 1.0 - start_t;
        end_t = 1.0 - end_t;
    }

    // avoid empty start segment
    if fuzzy_is_null(start_t - 1.0) {
        start_t = 0.0;
        start_segment += delta;
    }

    // avoid empty end segment
    if fuzzy_is_null(end_t) {
        end_t = 1.0;
        end_segment -= delta;
    }

    start_t = t_for_arc_angle(start_t * 90.0);
    end_t = t_for_arc_angle(end_t * 90.0);

    let split_at_start = !fuzzy_is_null(start_t);
    let split_at_end = !fuzzy_is_null(end_t - 1.0);

    let end = end_segment + delta;

    // empty arc?
    if start_segment == end {
        let quadrant = 3 - ((start_segment % 4) + 4) % 4;
        let j = (3 * quadrant) as usize;
        return (if delta > 0 { points[j + 3] } else { points[j] }, curves);
    }

    let (start_point, end_point) = find_ellipse_coords(rect, start_angle, sweep_length);

    let mut i = start_segment;
    while i != end {
        let quadrant = 3 - ((i % 4) + 4) % 4;
        let j = (3 * quadrant) as usize;

        let mut b = if delta > 0 {
            Bezier::from_points(points[j + 3], points[j + 2], points[j + 1], points[j])
        } else {
            Bezier::from_points(points[j], points[j + 1], points[j + 2], points[j + 3])
        };

        // empty arc?
        if start_segment == end_segment && fuzzy_compare(start_t, end_t) {
            return (start_point, curves);
        }

        if i == start_segment {
            if i == end_segment && split_at_end {
                b = b.bezier_on_interval(start_t, end_t);
            } else if split_at_start {
                b = b.bezier_on_interval(start_t, 1.0);
            }
        } else if i == end_segment && split_at_end {
            b = b.bezier_on_interval(0.0, end_t);
        }

        // push control points
        curves.push(b.pt(1));
        curves.push(b.pt(2));
        curves.push(b.pt(3));
        i += delta;
    }

    let last = curves.len() - 1;
    curves[last] = end_point;

    (start_point, curves)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORACLE: &str = include_str!("../../tests/data/qpath_oracle.txt");

    fn f(s: &str) -> f64 {
        f64::from_bits(u64::from_str_radix(s, 16).unwrap())
    }

    /// `arcMoveTo` + `arcTo` of the oracle (mode 1), in `qreal`: the arc geometry against Qt's
    /// doubles to 1e-9, which is far tighter than the `f32` a `PainterPath` stores.
    #[test]
    fn arc_geometry_matches_qt_to_1e_9() {
        // Under Miri one arc in `STRIDE` is checked (see `tests/test_qpath_stroker.rs`).
        const STRIDE: usize = if cfg!(miri) { 16 } else { 1 };
        let mut cases = 0;
        for (n, line) in ORACLE
            .lines()
            .filter(|l| l.starts_with("arc 1 "))
            .enumerate()
        {
            if n % STRIDE != 0 {
                continue;
            }
            let t: Vec<&str> = line.split(' ').skip(2).collect();
            let rect = Rc::new(f(t[0]), f(t[1]), f(t[2]), f(t[3]));
            let (start_angle, sweep) = (f(t[4]), f(t[5]));
            let n: usize = t[6].parse().unwrap();
            if n == 0 || rect.is_null() {
                continue;
            }
            let el: Vec<(u8, f64, f64)> = (0..n)
                .map(|i| {
                    (
                        t[7 + 3 * i].parse().unwrap(),
                        f(t[8 + 3 * i]),
                        f(t[9 + 3 * i]),
                    )
                })
                .collect();
            let (move_to, _) = find_ellipse_coords(&rect, start_angle, 0.0);
            assert_eq!(el[0].0, 0);
            assert!((el[0].1 - move_to.x).abs() <= 1e-9 && (el[0].2 - move_to.y).abs() <= 1e-9);

            let (start, curves) = curves_for_arc(&rect, start_angle, sweep);
            assert!((start.x - move_to.x).abs() <= 1e-9 && (start.y - move_to.y).abs() <= 1e-9);
            // arcMoveTo's point is arcTo's start, so Qt's lineTo adds nothing. Its cubicTo drops
            // a curve whose points all equal the current one (a sweep of 1e-9 degrees at 270).
            let mut ours = vec![(0u8, start.x, start.y)];
            for chunk in curves.chunks_exact(3) {
                let last = ours[ours.len() - 1];
                let last = P::new(last.1, last.2);
                if point_eq(last, chunk[0])
                    && point_eq(chunk[0], chunk[1])
                    && point_eq(chunk[1], chunk[2])
                {
                    continue;
                }
                ours.push((2, chunk[0].x, chunk[0].y));
                ours.push((3, chunk[1].x, chunk[1].y));
                ours.push((3, chunk[2].x, chunk[2].y));
            }
            assert_eq!(el.len(), ours.len(), "{rect:?} {start_angle} {sweep}");
            for (k, (a, b)) in ours.iter().zip(&el).enumerate() {
                assert_eq!(a.0, b.0, "{rect:?} {start_angle} {sweep}: element {k} type");
                assert!(
                    (a.1 - b.1).abs() <= 1e-9 && (a.2 - b.2).abs() <= 1e-9,
                    "{rect:?} {start_angle} {sweep}: element {k} is ({}, {}), Qt has ({}, {})",
                    a.1,
                    a.2,
                    b.1,
                    b.2
                );
            }
            cases += 1;
        }
        assert!(cases * STRIDE > 250, "{cases} arcs");
    }

    #[test]
    fn t_for_arc_angle_spans_the_quadrant() {
        assert_eq!(t_for_arc_angle(0.0), 0.0);
        assert_eq!(t_for_arc_angle(90.0), 1.0);
        let mid = t_for_arc_angle(45.0);
        assert!((mid - 0.5).abs() < 1e-3, "{mid}");
    }

    #[test]
    fn bezier_split_and_sub_range_agree_with_point_at() {
        let b = Bezier::from_points(
            P::new(0.0, 0.0),
            P::new(10.0, 40.0),
            P::new(30.0, -20.0),
            P::new(50.0, 5.0),
        );
        let (l, r) = b.split();
        for t in [0.0, 0.25, 0.5, 1.0] {
            let a = b.point_at(t * 0.5);
            let c = l.point_at(t);
            assert!((a.x - c.x).abs() < 1e-9 && (a.y - c.y).abs() < 1e-9);
            let a = b.point_at(0.5 + t * 0.5);
            let c = r.point_at(t);
            assert!((a.x - c.x).abs() < 1e-9 && (a.y - c.y).abs() < 1e-9);
        }
        let s = b.bezier_on_interval(0.2, 0.7);
        for (t, u) in [(0.0, 0.2), (1.0, 0.7), (0.5, 0.45)] {
            let (a, c) = (s.point_at(t), b.point_at(u));
            assert!((a.x - c.x).abs() < 1e-9 && (a.y - c.y).abs() < 1e-9);
        }
    }

    #[test]
    fn line_angles_follow_qlinef() {
        let right = Line::from_xy(0.0, 0.0, 1.0, 0.0);
        let up = Line::from_xy(0.0, 0.0, 0.0, -1.0); // y grows downwards
        assert!((right.angle_to(&up) - 90.0).abs() < 1e-12);
        assert!((up.angle_to(&right) - 270.0).abs() < 1e-12);
        let (kind, p) =
            Line::from_xy(0.0, 0.0, 2.0, 2.0).intersects(&Line::from_xy(0.0, 2.0, 2.0, 0.0));
        assert_eq!(kind, Intersection::Bounded);
        assert!((p.x - 1.0).abs() < 1e-12 && (p.y - 1.0).abs() < 1e-12);
        let (kind, _) =
            Line::from_xy(0.0, 0.0, 1.0, 0.0).intersects(&Line::from_xy(0.0, 1.0, 1.0, 1.0));
        assert_eq!(kind, Intersection::No);
    }
}
