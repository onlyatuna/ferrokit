// SPDX-License-Identifier: LGPL-3.0-only
//! Curved and polygonal `QRegion` constructors.
//!
//! Mirrors Qt 6.11.2 (`github.com/qt/qtbase`, tag `v6.11.2`):
//!
//! - `QRegion::QRegion(const QPolygon &, Qt::FillRule)` and the X11-derived scan converter
//!   `PolygonRegion` / `CreateETandAET` / `loadAET` / `computeWAET` / `InsertionSort` /
//!   `PtsToRegion` (`src/gui/painting/qregion.cpp`), including the Bresenham edge stepping
//!   (`BRESINITPGON` / `BRESINCRPGON`), the single-rectangle fast path and the 100000-row guard.
//! - `QRegion::QRegion(const QRect &, QRegion::Ellipse)`: `QPainterPath::addEllipse`, then
//!   `toSubpathPolygons().at(0).toPolygon()`, then `PolygonRegion(.., EvenOddRule)`
//!   (`qregion.cpp`, `qpainterpath.cpp`).
//! - The path pieces they depend on, in `f64` like Qt's `qreal`: `qt_curves_for_arc`,
//!   `qt_find_ellipse_coords` (`qstroker.cpp`, `qpainterpath.cpp`),
//!   `QBezier::addToPolygon` with the default 0.5 threshold and `QBezier::split` (`qbezier.cpp`),
//!   `QPainterPath::toSubpathPolygons` / `toFillPolygon` / `addRoundedRect` / `closeSubpath`, and
//!   `QPolygonF::toPolygon` (`qRound`).
//!
//! The curve-to-polygon flattening and the scan conversion are plain software code in Qt, so the
//! result does not depend on the platform engine (unlike, say, the region a window system builds
//! from it). Verified on Windows against PySide 6.11.2 output; the code is platform independent and
//! has no OS-specific branches.
//!
//! Rect decomposition: the scan converter's output is passed through [`Region::from_rects`], which
//! produces the canonical banding (maximal vertical merging, ordered by y then x). Qt's own
//! `PtsToRegion` merges identical consecutive rows the same way, and for every oracle case in
//! `tests/test_qregion_shapes.rs` the rect list equals the one Qt reports from `QRegion::rects()`.
//! That equality is observed, not guaranteed: Qt's merge only joins directly adjacent identical
//! rows of one polygon, so a shape where it leaves a mergeable split would differ in banding (never
//! in covered pixels).
//!
//! Floating point: the curve flattening uses only `+ - * /`, `floor` and `copysign`, which are
//! exactly rounded or exact in IEEE 754, so results are bit-identical across platforms. The
//! `sin`/`cos` based `qt_t_for_arc_angle` (libm, may differ by an ulp between platforms) is never
//! reached because only quadrant-aligned arc angles are implemented, where Qt returns before it
//! evaluates them; it is therefore not ported.
//!
//! Only the arcs these constructors need are implemented: `qt_curves_for_arc` for a quadrant-aligned
//! start angle with a `-90` degree sweep (what `addRoundedRect` calls) and the full `-360` ellipse.

use super::polygon::{FillRule, Polygon};
use super::primitives::{Point, Rect};
use super::region::Region;
use std::collections::BTreeMap;

/// `QRegion::RegionType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionType {
    /// The region is the rectangle itself.
    Rectangle,
    /// The region is the ellipse inscribed in the rectangle.
    Ellipse,
}

impl Region {
    /// `QRegion(const QRect &, RegionType)`.
    ///
    /// An empty rectangle gives an empty region. `Ellipse` scan-converts the flattened
    /// `QPainterPath::addEllipse` outline with the even-odd rule, exactly as Qt does.
    pub fn from_rect_type(r: Rect, ty: RegionType) -> Self {
        if r.width <= 0 || r.height <= 0 {
            return Self::new();
        }
        match ty {
            RegionType::Rectangle => Self::from_rect(r),
            RegionType::Ellipse => {
                let mut path = Path::default();
                path.add_ellipse(
                    f64::from(r.x),
                    f64::from(r.y),
                    f64::from(r.width),
                    f64::from(r.height),
                );
                let poly = round_polygon(
                    path.subpath_polygons()
                        .first()
                        .map_or(&[][..], |p| p.as_slice()),
                );
                polygon_region(&poly, FillRule::OddEven).unwrap_or_default()
            }
        }
    }

    /// `QRegion(const QPolygon &, Qt::FillRule)`.
    ///
    /// Fewer than three points, or a polygon taller than 100000 rows, give an empty region (Qt
    /// returns the shared empty region for both).
    pub fn from_polygon(polygon: &Polygon, fill_rule: FillRule) -> Self {
        if polygon.len() > 2 {
            polygon_region(polygon.points(), fill_rule).unwrap_or_default()
        } else {
            Self::new()
        }
    }

    /// Region of a rounded rectangle as Qt computes it:
    /// `QRegion(path.toFillPolygon().toPolygon())` for a `QPainterPath::addRoundedRect(rect,
    /// x_radius, y_radius)` path (`Qt::AbsoluteSize`, default odd-even fill).
    ///
    /// This composite is not a single Qt constructor; it is the idiom Qt code uses to get a
    /// rounded-rectangle `QRegion` (Qt has no rounded `RegionType`). As in `addRoundedRect`, a
    /// radius of zero or less gives the plain rectangle, and radii are clamped to half the size.
    /// `rect` with a non-positive width or height gives an empty region.
    pub fn from_rounded_rect(rect: Rect, x_radius: f64, y_radius: f64) -> Self {
        if rect.width <= 0 || rect.height <= 0 {
            return Self::new();
        }
        let mut path = Path::default();
        path.add_rounded_rect(
            f64::from(rect.x),
            f64::from(rect.y),
            f64::from(rect.width),
            f64::from(rect.height),
            x_radius,
            y_radius,
        );
        let poly = round_polygon(&path.fill_polygon());
        if poly.len() > 2 {
            polygon_region(&poly, FillRule::OddEven).unwrap_or_default()
        } else {
            Self::new()
        }
    }
}

// --- QPolygonF::toPolygon ---------------------------------------------------------------------

/// `qRound(double)` (`qnumeric.h`, Qt 6.11): add 0.5 with the sign of `d` (round half away from
/// zero), then truncate toward zero (the checked FP-to-int conversion).
fn q_round(d: f64) -> i32 {
    (d + 0.5f64.copysign(d)) as i32
}

/// `QPolygonF::toPolygon`.
fn round_polygon(pts: &[(f64, f64)]) -> Vec<Point> {
    pts.iter()
        .map(|&(x, y)| Point::new(q_round(x), q_round(y)))
        .collect()
}

// --- QPainterPath subset, in f64 ----------------------------------------------------------------

/// `QT_PATH_KAPPA`.
const KAPPA: f64 = 0.552_284_749_8;

type Pt = (f64, f64);

/// `qFuzzyCompare(double, double)`.
fn fuzzy_compare(a: f64, b: f64) -> bool {
    (a - b).abs() * 1_000_000_000_000.0 <= a.abs().min(b.abs())
}

/// `QBezier` (`qbezier_p.h`).
#[derive(Clone, Copy)]
struct Bezier {
    x1: f64,
    y1: f64,
    x2: f64,
    y2: f64,
    x3: f64,
    y3: f64,
    x4: f64,
    y4: f64,
}

impl Bezier {
    fn from_points(p1: Pt, p2: Pt, p3: Pt, p4: Pt) -> Self {
        Self {
            x1: p1.0,
            y1: p1.1,
            x2: p2.0,
            y2: p2.1,
            x3: p3.0,
            y3: p3.1,
            x4: p4.0,
            y4: p4.1,
        }
    }

    /// `QBezier::coefficients`.
    fn coefficients(t: f64) -> (f64, f64, f64, f64) {
        let m_t = 1.0 - t;
        let mut b = m_t * m_t;
        let mut c = t * t;
        let d = c * t;
        let a = b * m_t;
        b *= 3.0 * t;
        c *= 3.0 * m_t;
        (a, b, c, d)
    }

    /// `QBezier::split`.
    fn split(&self) -> (Bezier, Bezier) {
        let mid = |l: Pt, r: Pt| ((l.0 + r.0) * 0.5, (l.1 + r.1) * 0.5);
        let (p1, p2, p3, p4) = (
            (self.x1, self.y1),
            (self.x2, self.y2),
            (self.x3, self.y3),
            (self.x4, self.y4),
        );
        let mid_12 = mid(p1, p2);
        let mid_23 = mid(p2, p3);
        let mid_34 = mid(p3, p4);
        let mid_12_23 = mid(mid_12, mid_23);
        let mid_23_34 = mid(mid_23, mid_34);
        let mid_all = mid(mid_12_23, mid_23_34);
        (
            Self::from_points(p1, mid_12, mid_12_23, mid_all),
            Self::from_points(mid_all, mid_23_34, mid_34, p4),
        )
    }

    /// `QBezier::addToPolygon` with the default `bezier_flattening_threshold` of 0.5.
    fn add_to_polygon(&self, polygon: &mut Vec<Pt>) {
        const THRESHOLD: f64 = 0.5;
        let mut beziers = [*self; 10];
        let mut levels = [0i32; 10];
        levels[0] = 9;
        let mut top: i32 = 0;
        while top >= 0 {
            let t = top as usize;
            let b = beziers[t];
            let y4y1 = b.y4 - b.y1;
            let x4x1 = b.x4 - b.x1;
            let mut l = x4x1.abs() + y4y1.abs();
            let d;
            if l > 1.0 {
                d = (x4x1 * (b.y1 - b.y2) - y4y1 * (b.x1 - b.x2)).abs()
                    + (x4x1 * (b.y1 - b.y3) - y4y1 * (b.x1 - b.x3)).abs();
            } else {
                d = (b.x1 - b.x2).abs()
                    + (b.y1 - b.y2).abs()
                    + (b.x1 - b.x3).abs()
                    + (b.y1 - b.y3).abs();
                l = 1.0;
            }
            if d < THRESHOLD * l || levels[t] == 0 {
                polygon.push((b.x4, b.y4));
                top -= 1;
            } else {
                // split, second half of the polygon goes lower into the stack
                let (first, second) = b.split();
                beziers[t + 1] = first;
                beziers[t] = second;
                levels[t] -= 1;
                levels[t + 1] = levels[t];
                top += 1;
            }
        }
    }
}

/// `qt_find_ellipse_coords` for one quadrant-aligned angle (`qpainterpath.cpp`).
///
/// Qt maps the fractional quadrant position through `qt_t_for_arc_angle`, which returns 0 for a zero
/// angle before it evaluates any `sin`/`cos`. Only aligned angles are supported here, so the
/// result never depends on the platform libm.
fn find_ellipse_coords(x: f64, y: f64, w: f64, h: f64, angle: f64) -> Pt {
    let (w2, h2) = (w / 2.0, h / 2.0);
    let theta = angle - 360.0 * (angle / 360.0).floor();
    debug_assert!(
        theta % 90.0 == 0.0,
        "only quadrant-aligned angles are supported"
    );
    let quadrant = (theta / 90.0) as i32;
    let mut t = 0.0;
    if quadrant & 1 != 0 {
        t = 1.0 - t;
    }
    let (a, b, c, d) = Bezier::coefficients(t);
    let mut p = (a + b + c * KAPPA, d + c + b * KAPPA);
    if quadrant == 1 || quadrant == 2 {
        p.0 = -p.0;
    }
    if quadrant == 0 || quadrant == 1 {
        p.1 = -p.1;
    }
    // QRectF::center() is (x + w / 2, y + h / 2).
    (x + w / 2.0 + w2 * p.0, y + h / 2.0 + h2 * p.1)
}

/// The 13 control points of the four quadrant beziers of `qt_curves_for_arc`'s ellipse table.
fn arc_points(x: f64, y: f64, w: f64, h: f64) -> [Pt; 13] {
    let w2 = w / 2.0;
    let w2k = w2 * KAPPA;
    let h2 = h / 2.0;
    let h2k = h2 * KAPPA;
    [
        (x + w, y + h2),
        (x + w, y + h2 + h2k),
        (x + w2 + w2k, y + h),
        (x + w2, y + h),
        (x + w2 - w2k, y + h),
        (x, y + h2 + h2k),
        (x, y + h2),
        (x, y + h2 - h2k),
        (x + w2 - w2k, y),
        (x + w2, y),
        (x + w2 + w2k, y),
        (x + w, y + h2 - h2k),
        (x + w, y + h2),
    ]
}

#[derive(Clone, Copy)]
enum Element {
    Move(Pt),
    Line(Pt),
    Curve(Pt, Pt, Pt),
}

impl Element {
    fn end(&self) -> Pt {
        match *self {
            Element::Move(p) | Element::Line(p) | Element::Curve(_, _, p) => p,
        }
    }
}

/// Minimal `QPainterPath`: the element list plus the operations the region constructors need.
#[derive(Default)]
struct Path {
    elements: Vec<Element>,
    /// Index of the current subpath's `MoveTo` (`cStart`).
    start: usize,
}

impl Path {
    /// `QPainterPath::moveTo`.
    fn move_to(&mut self, p: Pt) {
        if let Some(Element::Move(last)) = self.elements.last_mut() {
            *last = p;
        } else {
            self.elements.push(Element::Move(p));
        }
        self.start = self.elements.len() - 1;
    }

    /// `QPainterPath::lineTo`; a segment to the current point is dropped.
    fn line_to(&mut self, p: Pt) {
        if self.elements.last().is_some_and(|e| e.end() == p) {
            return;
        }
        self.elements.push(Element::Line(p));
    }

    /// `QPainterPath::cubicTo`.
    fn cubic_to(&mut self, c1: Pt, c2: Pt, e: Pt) {
        self.elements.push(Element::Curve(c1, c2, e));
    }

    /// `QPainterPath::closeSubpath` (`QPainterPathPrivate::close`).
    fn close(&mut self) {
        let Some(&last) = self.elements.last() else {
            return;
        };
        let first = self.elements[self.start].end();
        let last = last.end();
        if first != last {
            if fuzzy_compare(first.0, last.0) && fuzzy_compare(first.1, last.1) {
                // Snap the last point onto the start.
                match self.elements.last_mut() {
                    Some(Element::Move(p) | Element::Line(p) | Element::Curve(_, _, p)) => {
                        *p = first
                    }
                    None => {}
                }
            } else {
                self.elements.push(Element::Line(first));
            }
        }
    }

    /// `QPainterPath::addEllipse`: `qt_curves_for_arc(rect, 0, -360)`.
    fn add_ellipse(&mut self, x: f64, y: f64, w: f64, h: f64) {
        let pts = arc_points(x, y, w, h);
        self.move_to(pts[0]);
        for i in 0..4 {
            self.cubic_to(pts[1 + 3 * i], pts[2 + 3 * i], pts[3 + 3 * i]);
        }
    }

    /// `QPainterPath::arcMoveTo` for a quadrant-aligned angle.
    fn arc_move_to(&mut self, x: f64, y: f64, w: f64, h: f64, angle: f64) {
        self.move_to(find_ellipse_coords(x, y, w, h, angle));
    }

    /// `QPainterPath::arcTo(rect, start_angle, -90)` for a quadrant-aligned `start_angle`
    /// (`qt_curves_for_arc` reduces this to one full quadrant bezier whose end point is replaced by
    /// `qt_find_ellipse_coords`).
    fn arc_to_quadrant(&mut self, x: f64, y: f64, w: f64, h: f64, start_angle: i32) {
        debug_assert!(start_angle % 90 == 0);
        let pts = arc_points(x, y, w, h);
        let segment = start_angle.div_euclid(90) - 1;
        let quadrant = 3 - segment.rem_euclid(4);
        let j = 3 * quadrant as usize;
        let start = find_ellipse_coords(x, y, w, h, f64::from(start_angle));
        let end = find_ellipse_coords(x, y, w, h, f64::from(start_angle - 90));
        self.line_to(start);
        self.cubic_to(pts[j + 1], pts[j + 2], end);
    }

    /// `QPainterPath::addRect` (the zero-radius branch of `addRoundedRect`).
    fn add_rect(&mut self, x: f64, y: f64, w: f64, h: f64) {
        self.move_to((x, y));
        self.elements.push(Element::Line((x + w, y)));
        self.elements.push(Element::Line((x + w, y + h)));
        self.elements.push(Element::Line((x, y + h)));
        self.elements.push(Element::Line((x, y)));
    }

    /// `QPainterPath::addRoundedRect(rect, xr, yr, Qt::AbsoluteSize)` for an already normalized,
    /// non-null rectangle.
    fn add_rounded_rect(&mut self, x: f64, y: f64, w: f64, h: f64, x_radius: f64, y_radius: f64) {
        let half_w = w / 2.0;
        let half_h = h / 2.0;
        let x_radius = if half_w == 0.0 {
            0.0
        } else {
            100.0 * x_radius.min(half_w) / half_w
        };
        let y_radius = if half_h == 0.0 {
            0.0
        } else {
            100.0 * y_radius.min(half_h) / half_h
        };
        if x_radius <= 0.0 || y_radius <= 0.0 {
            self.add_rect(x, y, w, h);
            return;
        }
        let rxx2 = w * x_radius / 100.0;
        let ryy2 = h * y_radius / 100.0;
        self.arc_move_to(x, y, rxx2, ryy2, 180.0);
        self.arc_to_quadrant(x, y, rxx2, ryy2, 180);
        self.arc_to_quadrant(x + w - rxx2, y, rxx2, ryy2, 90);
        self.arc_to_quadrant(x + w - rxx2, y + h - ryy2, rxx2, ryy2, 0);
        self.arc_to_quadrant(x, y + h - ryy2, rxx2, ryy2, 270);
        self.close();
    }

    /// `QPainterPath::toSubpathPolygons` with the identity matrix.
    fn subpath_polygons(&self) -> Vec<Vec<Pt>> {
        let mut flat: Vec<Vec<Pt>> = Vec::new();
        let mut current: Vec<Pt> = Vec::new();
        let mut prev: Pt = (0.0, 0.0);
        for e in &self.elements {
            match *e {
                Element::Move(p) => {
                    if current.len() > 1 {
                        flat.push(std::mem::take(&mut current));
                    }
                    current.clear();
                    current.push(p);
                }
                Element::Line(p) => current.push(p),
                Element::Curve(c1, c2, p) => {
                    Bezier::from_points(prev, c1, c2, p).add_to_polygon(&mut current)
                }
            }
            prev = e.end();
        }
        if current.len() > 1 {
            flat.push(current);
        }
        flat
    }

    /// `QPainterPath::toFillPolygon` with the identity matrix.
    fn fill_polygon(&self) -> Vec<Pt> {
        let flats = self.subpath_polygons();
        let mut polygon = Vec::new();
        let Some(first) = flats.first().and_then(|f| f.first().copied()) else {
            return polygon;
        };
        for (i, sub) in flats.iter().enumerate() {
            polygon.extend_from_slice(sub);
            if sub.first() != sub.last() {
                polygon.push(sub[0]);
            }
            if i > 0 {
                polygon.push(first);
            }
        }
        polygon
    }
}

// --- PolygonRegion (X11 PolyReg.c as shipped in qregion.cpp) --------------------------------------

const NIL: usize = usize::MAX;
/// `PolygonRegion` refuses polygons taller than this many rows.
const MAX_ROWS: i64 = 100_000;

/// `EdgeTableEntry`; `BRESINFO` is inlined. All arithmetic is `i64` so degenerate coordinates
/// cannot overflow the way C `int` would.
#[derive(Clone, Copy)]
struct Edge {
    ymax: i64,
    clockwise: bool,
    minor_axis: i64,
    d: i64,
    m: i64,
    m1: i64,
    incr1: i64,
    incr2: i64,
    next: usize,
    back: usize,
    next_wete: usize,
}

impl Edge {
    const fn blank() -> Self {
        Self {
            ymax: 0,
            clockwise: false,
            minor_axis: 0,
            d: 0,
            m: 0,
            m1: 0,
            incr1: 0,
            incr2: 0,
            next: NIL,
            back: NIL,
            next_wete: NIL,
        }
    }

    /// `BRESINITPGON` for a non-horizontal edge.
    fn init(&mut self, dy: i64, x1: i64, x2: i64) {
        self.minor_axis = x1;
        let dx = x2 - x1;
        if dx < 0 {
            self.m = dx / dy;
            self.m1 = self.m - 1;
            self.incr1 = -2 * dx + 2 * dy * self.m1;
            self.incr2 = -2 * dx + 2 * dy * self.m;
            self.d = 2 * self.m * dy - 2 * dx - 2 * dy;
        } else {
            self.m = dx / dy;
            self.m1 = self.m + 1;
            self.incr1 = 2 * dx - 2 * dy * self.m1;
            self.incr2 = 2 * dx - 2 * dy * self.m;
            self.d = -2 * self.m * dy + 2 * dx;
        }
    }

    /// `BRESINCRPGON`.
    fn step(&mut self) {
        if self.m1 > 0 {
            if self.d > 0 {
                self.minor_axis += self.m1;
                self.d += self.incr1;
            } else {
                self.minor_axis += self.m;
                self.d += self.incr2;
            }
        } else if self.d >= 0 {
            self.minor_axis += self.m1;
            self.d += self.incr1;
        } else {
            self.minor_axis += self.m;
            self.d += self.incr2;
        }
    }
}

/// Active edge table: arena of edges, index 0 is the header node (`AET`).
struct Aet {
    e: Vec<Edge>,
}

impl Aet {
    /// `loadAET`: merge a sorted edge list into the active table.
    fn load(&mut self, mut ets: usize) {
        let mut prev = 0;
        let mut aet = self.e[0].next;
        while ets != NIL {
            while aet != NIL && self.e[aet].minor_axis < self.e[ets].minor_axis {
                prev = aet;
                aet = self.e[aet].next;
            }
            let tmp = self.e[ets].next;
            self.e[ets].next = aet;
            if aet != NIL {
                self.e[aet].back = ets;
            }
            self.e[ets].back = prev;
            self.e[prev].next = ets;
            prev = ets;
            ets = tmp;
        }
    }

    /// `computeWAET`.
    fn compute_waet(&mut self) {
        let mut inside = true;
        let mut is_inside = 0i32;
        self.e[0].next_wete = NIL;
        let mut wete = 0;
        let mut aet = self.e[0].next;
        while aet != NIL {
            if self.e[aet].clockwise {
                is_inside += 1;
            } else {
                is_inside -= 1;
            }
            if (!inside && is_inside == 0) || (inside && is_inside != 0) {
                self.e[wete].next_wete = aet;
                wete = aet;
                inside = !inside;
            }
            aet = self.e[aet].next;
        }
        self.e[wete].next_wete = NIL;
    }

    /// `InsertionSort`; returns whether anything moved.
    fn insertion_sort(&mut self) -> bool {
        let mut changed = false;
        let mut aet = self.e[0].next;
        while aet != NIL {
            let insert = aet;
            let mut chase = aet;
            while self.e[self.e[chase].back].minor_axis > self.e[aet].minor_axis {
                chase = self.e[chase].back;
            }
            aet = self.e[aet].next;
            if chase != insert {
                let chase_back = self.e[chase].back;
                let insert_back = self.e[insert].back;
                self.e[insert_back].next = aet;
                if aet != NIL {
                    self.e[aet].back = insert_back;
                }
                self.e[insert].next = chase;
                let cb = self.e[chase].back;
                self.e[cb].next = insert;
                self.e[chase].back = insert;
                self.e[insert].back = chase_back;
                changed = true;
            }
        }
        changed
    }

    /// `EVALUATEEDGEWINDING` / `EVALUATEEDGEEVENODD`: advance past `aet`, returning
    /// `(next aet, new prev, edge left)`.
    fn evaluate(&mut self, aet: usize, prev: usize, y: i64) -> (usize, usize, bool) {
        if self.e[aet].ymax == y {
            let next = self.e[aet].next;
            self.e[prev].next = next;
            if next != NIL {
                self.e[next].back = prev;
            }
            (next, prev, true)
        } else {
            self.e[aet].step();
            (self.e[aet].next, aet, false)
        }
    }
}

/// `PolygonRegion`: scan-convert `pts` into a region. `None` where Qt returns a null region.
fn polygon_region(pts: &[Point], rule: FillRule) -> Option<Region> {
    let count = pts.len();
    debug_assert!(count > 1);

    // Special case a rectangle.
    let p = |i: usize| (i64::from(pts[i].x), i64::from(pts[i].y));
    if (count == 4 || (count == 5 && p(4) == p(0)))
        && ((p(0).1 == p(1).1 && p(1).0 == p(2).0 && p(2).1 == p(3).1 && p(3).0 == p(0).0)
            || (p(0).0 == p(1).0 && p(1).1 == p(2).1 && p(2).0 == p(3).0 && p(3).1 == p(0).1))
    {
        let x = p(0).0.min(p(2).0);
        let y = p(0).1.min(p(2).1);
        let w = p(0).0.max(p(2).0) - x;
        let h = p(0).1.max(p(2).1) - y;
        // QRect left <= right && top <= bottom, i.e. a positive size.
        if w >= 1 && h >= 1 && w <= i64::from(i32::MAX) && h <= i64::from(i32::MAX) {
            return Some(Region::from_rect(Rect::new(
                x as i32, y as i32, w as i32, h as i32,
            )));
        }
        return Some(Region::new());
    }

    // CreateETandAET.
    let mut e = vec![Edge::blank(); 1 + count];
    e[0].minor_axis = i64::from(i32::MIN);
    let mut et: BTreeMap<i64, usize> = BTreeMap::new();
    let (mut ymax, mut ymin) = (i64::from(i32::MIN), i64::from(i32::MAX));
    let mut prev_pt = p(count - 1);
    let mut next_edge = 1;
    for i in 0..count {
        let curr = p(i);
        let (top, bottom, clockwise) = if prev_pt.1 > curr.1 {
            (curr, prev_pt, false)
        } else {
            (prev_pt, curr, true)
        };
        if bottom.1 != top.1 {
            let ete = next_edge;
            e[ete].clockwise = clockwise;
            e[ete].ymax = bottom.1 - 1; // -1 so we don't get the last scanline
            e[ete].init(bottom.1 - top.1, top.0, bottom.0);
            // InsertEdgeInET: sorted by minor axis, new edge before equal ones.
            let head = et.entry(top.1).or_insert(NIL);
            let (mut before, mut cur) = (NIL, *head);
            while cur != NIL && e[cur].minor_axis < e[ete].minor_axis {
                before = cur;
                cur = e[cur].next;
            }
            e[ete].next = cur;
            if before == NIL {
                *head = ete;
            } else {
                e[before].next = ete;
            }
            ymax = ymax.max(prev_pt.1);
            ymin = ymin.min(prev_pt.1);
            next_edge += 1;
        }
        prev_pt = curr;
    }

    if ymax - ymin > MAX_ROWS {
        return None;
    }

    let mut aet = Aet { e };
    let mut scan = et.into_iter().peekable();
    let mut out: Vec<(i32, i32)> = Vec::new(); // emitted (x, y) edge crossings
    let mut fix_waet = false;
    let mut y = ymin;
    while y < ymax {
        let mut loaded = false;
        if let Some(&(sl, head)) = scan.peek()
            && sl == y
        {
            aet.load(head);
            scan.next();
            loaded = true;
        }
        match rule {
            FillRule::OddEven => {
                let mut prev = 0;
                let mut cur = aet.e[0].next;
                while cur != NIL {
                    out.push((aet.e[cur].minor_axis as i32, y as i32));
                    let (n, pv, _) = aet.evaluate(cur, prev, y);
                    cur = n;
                    prev = pv;
                }
                aet.insertion_sort();
            }
            FillRule::Winding => {
                if loaded {
                    aet.compute_waet();
                }
                let mut prev = 0;
                let mut cur = aet.e[0].next;
                let mut wete = cur;
                while cur != NIL {
                    if wete == cur {
                        out.push((aet.e[cur].minor_axis as i32, y as i32));
                        wete = aet.e[wete].next_wete;
                    }
                    let (n, pv, left) = aet.evaluate(cur, prev, y);
                    fix_waet |= left;
                    cur = n;
                    prev = pv;
                }
                if aet.insertion_sort() || fix_waet {
                    aet.compute_waet();
                    fix_waet = false;
                }
            }
        }
        y += 1;
    }

    Some(points_to_region(&out))
}

/// `PtsToRegion`: pair up the emitted points into spans per scanline and build the region.
fn points_to_region(pts: &[(i32, i32)]) -> Region {
    let mut rects: Vec<Rect> = Vec::new();
    let mut row: Vec<(i32, i32)> = Vec::new();
    let pairs = pts.len() / 2;
    for i in 0..pairs {
        let (a, b) = (pts[2 * i], pts[2 * i + 1]);
        let width = b.0 - a.0;
        if width != 0 {
            match row.last_mut() {
                Some(last) if last.1 == a.0 => last.1 = b.0,
                _ => row.push((a.0, b.0)),
            }
        }
        if !row.is_empty() {
            let next_y = pts.get(2 * i + 2).map(|n| n.1);
            if next_y != Some(a.1) {
                rects.extend(row.drain(..).map(|(x1, x2)| Rect::new(x1, a.1, x2 - x1, 1)));
            }
        }
    }
    Region::from_rects(&rects)
}
