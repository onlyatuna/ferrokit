// SPDX-License-Identifier: LGPL-3.0-only
//! `QPainterPathStroker`: turns a painter path into a fillable outline of its stroke.
//!
//! A transcription of Qt 6.11.2's `QStroker` and `QDashStroker` (`src/gui/painting/qstroker.cpp`,
//! `qstroker_p.h`) and of the `QPainterPathStroker` wrapper in `qpainterpath.cpp`
//! (`createStroke`, `setWidth`, `setDashPattern`, ...). The algorithms are Qt's own and not a
//! reinterpretation: the offset curves come from `QBezier::shifted`, joins from
//! `QStroker::joinPoints` and caps from the same function with the cap mode, round joins and caps
//! from `qt_curves_for_arc`. So the outline has the same elements as Qt's, for any width, cap,
//! join, miter limit and dash pattern.
//!
//! Differences from Qt:
//!
//! * Computation is in `f64` like Qt's `qreal`; the resulting [`PainterPath`] stores `f32`, so
//!   each coordinate differs from Qt's by at most half an `f32` ulp.
//! * `QDashStroker`'s clip-rect shortcut is omitted: `QPainterPathStroker` never sets a clip
//!   rectangle, so Qt never takes it from here.
//! * Qt reuses one `QStroker` between `createStroke` calls, which leaves two private
//!   "previous point" registers stale. Every call here starts from the initial state; the
//!   registers are always written before they are read, so the outline is the same.
//!
//! Platform: there is no OS-specific code, and Qt's stroker does not depend on the platform
//! engine either (it is the shared `QStroker` that every Qt raster and OpenGL paint engine uses).
//! What differs between platforms is only the last bit of `hypot`, `atan2`, `sin` and `cos` of the
//! system math library. The oracle tests ran on Windows. On macOS (`aarch64-apple-darwin`) and
//! Linux (`x86_64-unknown-linux-gnu`) the crate and its tests were type-checked only, not run;
//! the Linux target was also run under Miri (see `tools/miri_baseline_ferrokit_gui.json`), which
//! checks the code for undefined behaviour but is not a run on Linux.

use super::path::{FillRule, PainterPath, PathElementPoint, PathElementType};
use super::qpath_geom::{
    Bezier, Intersection, KAPPA, Line, P, Rc, curves_for_arc, fixed_eq, fuzzy_compare,
    fuzzy_is_null, point_eq,
};

/// `Qt::PenCapStyle`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PenCapStyle {
    /// `Qt::FlatCap`: the stroke ends at the end point.
    Flat,
    /// `Qt::SquareCap`: the stroke extends half the width past the end point (`QPen` default).
    #[default]
    Square,
    /// `Qt::RoundCap`.
    Round,
}

/// `Qt::PenJoinStyle`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PenJoinStyle {
    /// `Qt::MiterJoin`: the outer edges are extended to meet, cut off at the miter limit.
    Miter,
    /// `Qt::BevelJoin` (`QPen` default).
    #[default]
    Bevel,
    /// `Qt::RoundJoin`.
    Round,
    /// `Qt::SvgMiterJoin`: like `Miter`, but a join beyond the limit is beveled (SVG 1.1).
    SvgMiter,
}

/// The predefined dash patterns of `Qt::PenStyle`, for [`PainterPathStroker::set_dash_style`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PenStyle {
    /// `Qt::SolidLine`: no dashes.
    Solid,
    /// `Qt::DashLine`.
    Dash,
    /// `Qt::DotLine`.
    Dot,
    /// `Qt::DashDotLine`.
    DashDot,
    /// `Qt::DashDotDotLine`.
    DashDotDot,
}

/// `QStroker::LineJoinMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JoinMode {
    Flat,
    Square,
    Miter,
    Round,
    RoundCap,
    SvgMiter,
}

fn cap_mode(cap: PenCapStyle) -> JoinMode {
    match cap {
        PenCapStyle::Flat => JoinMode::Flat,
        PenCapStyle::Square => JoinMode::Square,
        PenCapStyle::Round => JoinMode::RoundCap,
    }
}

fn join_mode(join: PenJoinStyle) -> JoinMode {
    match join {
        PenJoinStyle::Bevel => JoinMode::Flat,
        PenJoinStyle::Miter => JoinMode::Miter,
        PenJoinStyle::SvgMiter => JoinMode::SvgMiter,
        PenJoinStyle::Round => JoinMode::Round,
    }
}

fn element(kind: PathElementType, x: f64, y: f64) -> PathElementPoint {
    PathElementPoint { kind, x, y }
}

/// The most elements (`QPainterPath::Element`s, a cubic being three) a stroke is made of; see
/// [`PainterPathStroker::create_stroke`].
pub(crate) const MAX_STROKE_ELEMENTS: usize = 1 << 20;

/// The threshold `set_curve_threshold` uses instead of one that is not positive.
const MIN_CURVE_THRESHOLD: f64 = 0.00025;

/// The output path of the strokers, with the `QPainterPath::moveTo` / `lineTo` / `cubicTo`
/// behaviour Qt's stroke hooks (`qt_path_stroke_move_to`, ...) get. Past `max_elements` nothing
/// more is added.
struct OutPath {
    elements: Vec<PathElementPoint>,
    max_elements: usize,
    /// Set when `full` was found true: something was left out.
    truncated: bool,
}

impl Default for OutPath {
    fn default() -> Self {
        Self {
            elements: Vec::new(),
            max_elements: MAX_STROKE_ELEMENTS,
            truncated: false,
        }
    }
}

impl OutPath {
    fn ensure_data(&mut self) {
        if self.elements.is_empty() {
            self.elements
                .push(element(PathElementType::MoveTo, 0.0, 0.0));
        }
    }

    /// Whether the limit is reached: the stroke is cut short here, which is remembered.
    fn full(&mut self) -> bool {
        let full = self.elements.len() + 3 > self.max_elements;
        self.truncated |= full;
        full
    }

    fn last(&self) -> P {
        let e = self.elements[self.elements.len() - 1];
        P::new(e.x, e.y)
    }

    /// `QPainterPath::moveTo`: a move after a move replaces it.
    fn move_to(&mut self, x: f64, y: f64) {
        if self.full() {
            return;
        }
        self.ensure_data();
        let n = self.elements.len();
        if self.elements[n - 1].kind == PathElementType::MoveTo {
            self.elements[n - 1].x = x;
            self.elements[n - 1].y = y;
        } else {
            self.elements.push(element(PathElementType::MoveTo, x, y));
        }
    }

    /// `QPainterPath::lineTo`: a line to the current point is dropped.
    fn line_to(&mut self, x: f64, y: f64) {
        if self.full() {
            return;
        }
        self.ensure_data();
        if point_eq(P::new(x, y), self.last()) {
            return;
        }
        self.elements.push(element(PathElementType::LineTo, x, y));
    }

    /// `QPainterPath::cubicTo`: a curve that is a single point is dropped.
    fn cubic_to(&mut self, c1: P, c2: P, e: P) {
        if self.full() {
            return;
        }
        self.ensure_data();
        let last = self.last();
        if point_eq(last, c1) && point_eq(c1, c2) && point_eq(c2, e) {
            return;
        }
        self.elements
            .push(element(PathElementType::CurveTo, c1.x, c1.y));
        self.elements
            .push(element(PathElementType::CurveToData, c2.x, c2.y));
        self.elements
            .push(element(PathElementType::CurveToData, e.x, e.y));
    }
}

/// `QSubpathBackwardIterator`: the elements of a subpath in reverse, with the types of the
/// reversed path.
fn backward(elements: &[PathElementPoint]) -> Vec<PathElementPoint> {
    let n = elements.len();
    let mut out = Vec::with_capacity(n);
    for pos in (0..n).rev() {
        let mut ce = elements[pos];
        if pos == n - 1 {
            ce.kind = PathElementType::MoveTo;
        } else {
            let pe = elements[pos + 1];
            ce.kind = match pe.kind {
                PathElementType::LineTo => PathElementType::LineTo,
                PathElementType::CurveToData => {
                    // First control point?
                    if ce.kind == PathElementType::CurveTo {
                        PathElementType::CurveToData
                    } else {
                        // Second control point then
                        PathElementType::CurveTo
                    }
                }
                PathElementType::CurveTo => PathElementType::CurveToData,
                PathElementType::MoveTo => ce.kind,
            };
        }
        out.push(ce);
    }
    out
}

/// `QStroker` (together with the `QStrokerOps` base that collects the subpath).
struct Stroker {
    elements: Vec<PathElementPoint>,
    curve_threshold: f64,
    stroke_width: f64,
    miter_limit: f64,
    cap: JoinMode,
    join: JoinMode,
    back1: P,
    back2: P,
    force_open: bool,
    out: OutPath,
}

impl Stroker {
    fn new() -> Self {
        Self {
            elements: Vec::new(),
            curve_threshold: 0.25,
            stroke_width: 1.0,
            miter_limit: 2.0,
            cap: JoinMode::Square,
            join: JoinMode::Flat,
            back1: P::default(),
            back2: P::default(),
            force_open: false,
            out: OutPath::default(),
        }
    }

    // ---- QStrokerOps ----

    fn begin(&mut self) {
        self.elements.clear();
    }

    fn end(&mut self) {
        if self.elements.len() > 1 {
            self.process_current_subpath();
        }
    }

    fn move_to(&mut self, x: f64, y: f64) {
        if self.elements.len() > 1 {
            self.process_current_subpath();
        }
        self.elements.clear();
        self.elements.push(element(PathElementType::MoveTo, x, y));
    }

    fn line_to(&mut self, x: f64, y: f64) {
        self.elements.push(element(PathElementType::LineTo, x, y));
    }

    fn cubic_to(&mut self, c1: P, c2: P, e: P) {
        self.elements
            .push(element(PathElementType::CurveTo, c1.x, c1.y));
        self.elements
            .push(element(PathElementType::CurveToData, c2.x, c2.y));
        self.elements
            .push(element(PathElementType::CurveToData, e.x, e.y));
    }

    /// `QStrokerOps::strokePath` with the identity matrix.
    fn stroke_path(&mut self, path: &[PathElementPoint]) {
        if path.is_empty() {
            return;
        }
        self.begin();
        let mut i = 0;
        while i < path.len() && !self.out.full() {
            let e = path[i];
            match e.kind {
                PathElementType::MoveTo => self.move_to(e.x, e.y),
                PathElementType::LineTo => self.line_to(e.x, e.y),
                PathElementType::CurveTo => {
                    let cp2 = path[i + 1];
                    let ep = path[i + 2];
                    i += 2;
                    self.cubic_to(P::new(e.x, e.y), P::new(cp2.x, cp2.y), P::new(ep.x, ep.y));
                }
                PathElementType::CurveToData => {}
            }
            i += 1;
        }
        self.end();
    }

    // ---- QStroker ----

    fn emit_move_to(&mut self, x: f64, y: f64) {
        self.back2 = self.back1;
        self.back1 = P::new(x, y);
        self.out.move_to(x, y);
    }

    fn emit_line_to(&mut self, x: f64, y: f64) {
        self.back2 = self.back1;
        self.back1 = P::new(x, y);
        self.out.line_to(x, y);
    }

    fn emit_cubic_to(&mut self, c1: P, c2: P, e: P) {
        if c2.x == e.x && c2.y == e.y {
            if c1.x == e.x && c1.y == e.y {
                self.back2 = self.back1;
            } else {
                self.back2 = c1;
            }
        } else {
            self.back2 = c2;
        }
        self.back1 = e;
        self.out.cubic_to(c1, c2, e);
    }

    /// `QStroker::processCurrentSubpath`.
    fn process_current_subpath(&mut self) {
        if self.out.full() {
            return;
        }
        let elements = std::mem::take(&mut self.elements);
        let fw = elements.clone();
        let bw = backward(&elements);

        let mut fw_start_tangent = Line::default();
        let mut bw_start_tangent = Line::default();

        let fwclosed = self.stroke_side(&fw, false, &mut fw_start_tangent);
        let bwclosed = self.stroke_side(&bw, !fwclosed, &mut bw_start_tangent);

        if !bwclosed && !fw_start_tangent.is_null() {
            self.join_points(elements[0].x, elements[0].y, &fw_start_tangent, self.cap);
        }
        self.elements = elements;
    }

    /// `qt_stroke_side`: strokes one side of the subpath `it` (in iteration order). Returns
    /// whether the side was closed. With `cap_first` the first segment is connected with a cap
    /// instead of starting a new subpath.
    fn stroke_side(
        &mut self,
        it: &[PathElementPoint],
        cap_first: bool,
        start_tangent: &mut Line,
    ) -> bool {
        // Used in CurveToElement section below.
        const MAX_OFFSET: usize = 16;
        let mut offset_curves = [Bezier::default(); MAX_OFFSET];

        let first_element = it[0];
        let mut pos = 1;
        let start = P::new(first_element.x, first_element.y);
        let mut prev = start;
        let mut first = true;
        let offset = self.stroke_width / 2.0;

        while pos < it.len() && !self.out.full() {
            let e = it[pos];
            pos += 1;

            if e.kind == PathElementType::LineTo {
                let mut line = Line::new(prev, P::new(e.x, e.y));
                if !point_eq(line.p1, line.p2) {
                    let mut normal = line.normal_vector();
                    normal.set_length(offset);
                    line.translate(P::new(normal.dx(), normal.dy()));

                    // If we are starting a new subpath, move to correct starting point.
                    if first {
                        if cap_first {
                            self.join_points(prev.x, prev.y, &line, self.cap);
                        } else {
                            self.emit_move_to(line.p1.x, line.p1.y);
                        }
                        *start_tangent = line;
                        first = false;
                    } else {
                        self.join_points(prev.x, prev.y, &line, self.join);
                    }

                    // Add the stroke for this line.
                    self.emit_line_to(line.p2.x, line.p2.y);
                    prev = P::new(e.x, e.y);
                }
            } else if e.kind == PathElementType::CurveTo {
                let cp2 = it[pos]; // control point 2
                let ep = it[pos + 1]; // end point
                pos += 2;

                let bezier = Bezier::from_points(
                    prev,
                    P::new(e.x, e.y),
                    P::new(cp2.x, cp2.y),
                    P::new(ep.x, ep.y),
                );
                let count = bezier.shifted(
                    &mut offset_curves,
                    MAX_OFFSET as i32,
                    offset,
                    self.curve_threshold as f32,
                );

                if count > 0 {
                    // If we are starting a new subpath, move to correct starting point
                    let mut tangent = bezier.start_tangent();
                    tangent.translate(offset_curves[0].pt(0) - bezier.pt(0));
                    if first {
                        let pt = offset_curves[0].pt(0);
                        if cap_first {
                            self.join_points(prev.x, prev.y, &tangent, self.cap);
                        } else {
                            self.emit_move_to(pt.x, pt.y);
                        }
                        *start_tangent = tangent;
                        first = false;
                    } else {
                        self.join_points(prev.x, prev.y, &tangent, self.join);
                    }

                    // Add these beziers
                    for curve in &offset_curves[..count] {
                        self.emit_cubic_to(curve.pt(1), curve.pt(2), curve.pt(3));
                    }
                }

                prev = P::new(ep.x, ep.y);
            }
        }

        if fixed_eq(start, prev) && !self.force_open {
            // closed subpath, join first and last point
            // don't join empty subpaths
            if !first {
                self.join_points(prev.x, prev.y, start_tangent, self.join);
            }
            true
        } else {
            false
        }
    }

    /// `QStroker::joinPoints`: connects the stroke so far to `next_line` around the focal point
    /// `(focal_x, focal_y)` with the join (or cap) `join`.
    fn join_points(&mut self, focal_x: f64, focal_y: f64, next_line: &Line, join: JoinMode) {
        // points connected already, don't join
        if fuzzy_compare(self.back1.x, next_line.p1.x)
            && fuzzy_compare(self.back1.y, next_line.p1.y)
        {
            return;
        }

        let prev_line = Line::new(self.back2, self.back1);
        let (ty, isect) = prev_line.intersects(next_line);

        if join == JoinMode::Flat {
            let short_cut = Line::new(prev_line.p2, next_line.p1);
            let angle = short_cut.angle_to(&prev_line);
            if ty == Intersection::Bounded || (angle > 90.0 && !fuzzy_compare(angle, 90.0)) {
                self.emit_line_to(focal_x, focal_y);
                self.emit_line_to(next_line.p1.x, next_line.p1.y);
                return;
            }
            self.emit_line_to(next_line.p1.x, next_line.p1.y);
        } else if join == JoinMode::Miter {
            let applied_miter_limit = self.stroke_width * self.miter_limit;

            // If we are on the inside, do the short cut...
            let short_cut = Line::new(prev_line.p2, next_line.p1);
            let angle = short_cut.angle_to(&prev_line);
            if ty == Intersection::Bounded || (angle > 90.0 && !fuzzy_compare(angle, 90.0)) {
                self.emit_line_to(focal_x, focal_y);
                self.emit_line_to(next_line.p1.x, next_line.p1.y);
                return;
            }
            let miter_line = Line::new(self.back1, isect);
            if ty == Intersection::No || miter_line.length() > applied_miter_limit {
                let mut l1 = prev_line;
                l1.set_length(applied_miter_limit);
                l1.translate(P::new(prev_line.dx(), prev_line.dy()));

                let mut l2 = *next_line;
                l2.set_length(applied_miter_limit);
                let d = P::new(-l2.dx(), -l2.dy());
                l2.translate(d);

                self.emit_line_to(l1.p2.x, l1.p2.y);
                self.emit_line_to(l2.p1.x, l2.p1.y);
                self.emit_line_to(next_line.p1.x, next_line.p1.y);
            } else {
                self.emit_line_to(isect.x, isect.y);
                self.emit_line_to(next_line.p1.x, next_line.p1.y);
            }
        } else if join == JoinMode::Square {
            let offset = self.stroke_width / 2.0;

            let mut l1 = prev_line;
            let dp =
                P::new(prev_line.dx(), prev_line.dy()).dot(P::new(next_line.dx(), next_line.dy()));
            if dp > 0.0 {
                // same direction, means that prevLine is from a bezier that has been "reversed"
                // by shifting
                l1 = Line::new(prev_line.p2, prev_line.p1);
            } else {
                let d = P::new(l1.dx(), l1.dy());
                l1.translate(d);
            }
            l1.set_length(offset);
            let mut l2 = Line::new(next_line.p2, next_line.p1);
            let d = P::new(l2.dx(), l2.dy());
            l2.translate(d);
            l2.set_length(offset);
            self.emit_line_to(l1.p2.x, l1.p2.y);
            self.emit_line_to(l2.p2.x, l2.p2.y);
            self.emit_line_to(l2.p1.x, l2.p1.y);
        } else if join == JoinMode::Round {
            let offset = self.stroke_width / 2.0;

            let short_cut = Line::new(prev_line.p2, next_line.p1);
            let angle = short_cut.angle_to(&prev_line);
            if (ty == Intersection::Bounded || angle > 90.01) && next_line.length() > offset {
                self.emit_line_to(focal_x, focal_y);
                self.emit_line_to(next_line.p1.x, next_line.p1.y);
                return;
            }
            // `adapted_angle_on_x`: QLineF::angleTo gives the angle between two lines respecting
            // the direction; here the line's angle direction on the unit circle is wanted.
            let x_axis = Line::from_xy(0.0, 0.0, 1.0, 0.0);
            let l1_on_x = x_axis.angle_to(&prev_line);
            let l2_on_x = x_axis.angle_to(next_line);

            let sweep_length = (l2_on_x - l1_on_x).abs();

            let (_curve_start, curves) = curves_for_arc(
                &Rc::new(
                    focal_x - offset,
                    focal_y - offset,
                    offset * 2.0,
                    offset * 2.0,
                ),
                l1_on_x + 90.0,
                -sweep_length,
            );

            for c in curves.as_chunks::<3>().0 {
                self.emit_cubic_to(c[0], c[1], c[2]);
            }

            // line to the end of the arc segment, (should also not be needed).
            self.emit_line_to(next_line.p1.x, next_line.p1.y);

        // Same as round join except we know its 180 degrees. Can also optimize this
        // later based on the addEllipse logic
        } else if join == JoinMode::RoundCap {
            let offset = self.stroke_width / 2.0;

            // first control line
            let mut l1 = prev_line;
            let dp =
                P::new(prev_line.dx(), prev_line.dy()).dot(P::new(next_line.dx(), next_line.dy()));
            if dp > 0.0 {
                // same direction, means that prevLine is from a bezier that has been "reversed"
                // by shifting
                l1 = Line::new(prev_line.p2, prev_line.p1);
            } else {
                let d = P::new(l1.dx(), l1.dy());
                l1.translate(d);
            }
            l1.set_length(KAPPA * offset);

            // second control line, find through normal between prevLine and focal.
            let mut l2 = Line::from_xy(focal_x, focal_y, prev_line.p2.x, prev_line.p2.y);
            let d = P::new(-l2.dy(), l2.dx());
            l2.translate(d);
            l2.set_length(KAPPA * offset);

            self.emit_cubic_to(l1.p2, l2.p2, l2.p1);

            // move so that it matches
            l2 = Line::from_xy(l2.p1.x, l2.p1.y, l2.p1.x - l2.dx(), l2.p1.y - l2.dy());

            // last line is parallel to l1 so just shift it down.
            let d = P::new(next_line.p1.x - l1.p1.x, next_line.p1.y - l1.p1.y);
            l1.translate(d);

            self.emit_cubic_to(l2.p2, l1.p2, l1.p1);
        } else if join == JoinMode::SvgMiter {
            let short_cut = Line::new(prev_line.p2, next_line.p1);
            let angle = short_cut.angle_to(&prev_line);
            if ty == Intersection::Bounded || (angle > 90.0 && !fuzzy_compare(angle, 90.0)) {
                self.emit_line_to(focal_x, focal_y);
                self.emit_line_to(next_line.p1.x, next_line.p1.y);
                return;
            }
            let miter_line = Line::new(P::new(focal_x, focal_y), isect);
            if ty == Intersection::No
                || miter_line.length() > self.stroke_width * self.miter_limit / 2.0
            {
                self.emit_line_to(next_line.p1.x, next_line.p1.y);
            } else {
                self.emit_line_to(isect.x, isect.y);
                self.emit_line_to(next_line.p1.x, next_line.p1.y);
            }
        }
    }
}

/// `QDashStroker`, feeding its dashes to a [`Stroker`].
struct DashStroker<'a> {
    stroker: &'a mut Stroker,
    elements: Vec<PathElementPoint>,
    dash_pattern: &'a [f64],
    dash_offset: f64,
    dash_threshold: f64,
}

/// `QDashStroker::repetitionLimit`.
const REPETITION_LIMIT: f64 = 10000.0;

/// The most dashes started on one line: 10 000 turns of at most 32 dashes, and some slack.
const MAX_TURNS_PER_LINE: u32 = 10_000 * 32 + 1000;

impl<'a> DashStroker<'a> {
    fn new(stroker: &'a mut Stroker, dash_pattern: &'a [f64], dash_offset: f64) -> Self {
        Self {
            stroker,
            elements: Vec::new(),
            dash_pattern,
            dash_offset,
            // `QStrokerOps::setCurveThresholdFromTransform(QTransform())` in `strokePath`:
            // 0.5 / scale with the identity's scale of 1.
            dash_threshold: 0.5,
        }
    }

    fn move_to(&mut self, x: f64, y: f64) {
        if self.elements.len() > 1 {
            self.process_current_subpath();
        }
        self.elements.clear();
        self.elements.push(element(PathElementType::MoveTo, x, y));
    }

    fn line_to(&mut self, x: f64, y: f64) {
        self.elements.push(element(PathElementType::LineTo, x, y));
    }

    fn cubic_to(&mut self, c1: P, c2: P, e: P) {
        self.elements
            .push(element(PathElementType::CurveTo, c1.x, c1.y));
        self.elements
            .push(element(PathElementType::CurveToData, c2.x, c2.y));
        self.elements
            .push(element(PathElementType::CurveToData, e.x, e.y));
    }

    /// `QStrokerOps::strokePath` of the dash stroker: `begin`, the elements, `end`.
    fn stroke_path(&mut self, path: &[PathElementPoint]) {
        if path.is_empty() {
            return;
        }
        self.stroker.begin();
        self.elements.clear();
        let mut i = 0;
        while i < path.len() && !self.stroker.out.full() {
            let e = path[i];
            match e.kind {
                PathElementType::MoveTo => self.move_to(e.x, e.y),
                PathElementType::LineTo => self.line_to(e.x, e.y),
                PathElementType::CurveTo => {
                    let cp2 = path[i + 1];
                    let ep = path[i + 2];
                    i += 2;
                    self.cubic_to(P::new(e.x, e.y), P::new(cp2.x, cp2.y), P::new(ep.x, ep.y));
                }
                PathElementType::CurveToData => {}
            }
            i += 1;
        }
        if self.elements.len() > 1 {
            self.process_current_subpath();
        }
        self.stroker.end();
    }

    /// `QSubpathFlatIterator`: the subpath with its curves flattened to lines.
    fn flatten(&self) -> Vec<PathElementPoint> {
        let m = &self.elements;
        let mut out = Vec::with_capacity(m.len());
        let mut pos = 0;
        while pos < m.len() {
            let mut e = m[pos];
            if e.kind == PathElementType::CurveTo {
                let curve = Bezier::from_points(
                    P::new(m[pos - 1].x, m[pos - 1].y),
                    P::new(e.x, e.y),
                    P::new(m[pos + 1].x, m[pos + 1].y),
                    P::new(m[pos + 2].x, m[pos + 2].y),
                )
                .to_polygon(self.dash_threshold);
                e.kind = PathElementType::LineTo;
                e.x = curve[0].x;
                e.y = curve[0].y;
                out.push(e);
                for p in &curve[1..] {
                    out.push(element(PathElementType::LineTo, p.x, p.y));
                }
                pos += 2;
            } else {
                out.push(e);
            }
            pos += 1;
        }
        out
    }

    /// `QDashStroker::processCurrentSubpath`.
    fn process_current_subpath(&mut self) {
        let dash_count_all = self.dash_pattern.len().min(32);
        let mut dashes = [0.0f64; 32];

        let stroke_width = self.stroker.stroke_width;

        let mut sum_length = 0.0;
        for (dash, &pattern) in dashes
            .iter_mut()
            .zip(self.dash_pattern)
            .take(dash_count_all)
        {
            *dash = pattern.max(0.0) * stroke_width;
            sum_length += *dash;
        }

        if fuzzy_is_null(sum_length) {
            return;
        }

        let dash_count = dash_count_all & !1usize; // Round down to even number

        // The dashes the loops below cycle through: `dash_count`, but a pattern of a single entry
        // has `dash_count == 0` and cycles through that one. Qt measures the cycle by the sum of
        // all entries, which is longer when the count is odd.
        let cycle_count = dash_count.max(1);
        let cycle_length: f64 = dashes[..cycle_count].iter().sum();
        // Qt hangs/UB here; ferrokit draws nothing. A cycle of zero length (`[0, 0, 1]`: the used
        // dashes are all zero and only the ignored odd entry is not) makes Qt's offset loop below
        // spin for ever; a sum beyond `f64` (width times entries overflows) makes its dash loop
        // spin on NaN positions.
        if fuzzy_is_null(cycle_length) || !sum_length.is_finite() {
            return;
        }
        let inv_sum_length = 1.0 / cycle_length;

        let mut idash = 0usize; // Index to current dash
        let mut pos = 0.0f64; // The position on the curve, 0 <= pos <= path.length
        let mut elen; // element length
        let mut doffset = self.dash_offset * stroke_width;
        // Qt hangs/UB here (`fmod(inf, x)` is NaN and the dash loop never ends on it); ferrokit
        // starts at the beginning of the pattern.
        if !doffset.is_finite() {
            doffset = 0.0;
        }

        // make sure doffset is in range [0..sumLength)
        doffset %= sum_length;
        if doffset < 0.0 {
            doffset += sum_length;
        }
        // Qt walks the dashes off `doffset` one by one, which for an odd count whose last entry
        // is long takes sum / cycle turns of the pattern; that is the remainder of the cycle.
        if cycle_length < sum_length {
            doffset %= cycle_length;
        }

        // At most two turns: after the remainder above, rounding can leave one more.
        for _ in 0..2 * cycle_count + 2 {
            if doffset < dashes[idash] {
                break;
            }
            doffset -= dashes[idash];
            idash += 1;
            if idash >= dash_count {
                idash = 0;
            }
        }
        if doffset >= dashes[idash] {
            doffset = 0.0;
        }

        let mut estart = 0.0f64; // The elements starting position
        let mut estop; // The element stop position

        let flat = self.flatten();
        let mut it = flat.iter();
        let first = it.next().expect("a subpath has its MoveTo");
        let mut prev = P::new(first.x, first.y);
        if !(prev.x.is_finite() && prev.y.is_finite()) {
            return;
        }

        let mut move_to_pos = prev;
        let mut has_move_to = false;
        for e in it {
            if self.stroker.out.full() {
                return;
            }
            if !(e.x.is_finite() && e.y.is_finite()) {
                continue;
            }

            let cline = Line::from_xy(prev.x, prev.y, e.x, e.y);
            elen = cline.length();

            estop = estart + elen;

            let mut done = pos >= estop;

            // Check if the entire line should be simplified
            let skip_dashing = elen * inv_sum_length > REPETITION_LIMIT;
            let mut max_dashes = dash_count as i32;
            if skip_dashing {
                // Cut away full dash sequences.
                elen -= (elen * inv_sum_length).floor() * cycle_length;
                // Update dash offset.
                while !done {
                    // parentheses to avoid float rounding issues: qreal(4) + 0.1 - 0.1 - 4 < 0
                    let dpos = (pos + dashes[idash]) - (doffset + estart);

                    if dpos > elen {
                        // dash extends this line
                        doffset = dashes[idash] - (dpos - elen); // subtract the part already used
                        pos = estop; // move pos to next path element
                        done = true;
                    } else {
                        // Dash is on this line
                        max_dashes -= 1;
                        pos = if max_dashes > 0 { dpos + estart } else { estop };
                        done = pos >= estop;
                        idash += 1;
                        if idash >= dash_count {
                            idash = 0;
                        }
                        doffset = 0.0; // full segment so no offset on next.
                    }
                }
                // skip costly dashing, just draw solid line
                if !has_move_to {
                    self.stroker.move_to(move_to_pos.x, move_to_pos.y);
                    has_move_to = true;
                }
                self.stroker.line_to(e.x, e.y);
                move_to_pos = P::new(e.x, e.y);
            }

            // Dash away...
            let mut stalled = 0u32;
            let mut turns = 0u32;
            while !done {
                if self.stroker.out.full() {
                    return;
                }
                let before = pos;
                let p2;

                let has_offset = doffset > 0.0;
                let even_dash = (idash & 1) == 0;
                // parentheses to avoid float rounding issues: qreal(4) + 0.1 - 0.1 - 4 < 0
                let dpos = (pos + dashes[idash]) - (doffset + estart);

                if dpos > elen {
                    // dash extends this line
                    doffset = dashes[idash] - (dpos - elen); // subtract the part already used
                    pos = estop; // move pos to next path element
                    done = true;
                    p2 = cline.p2;
                } else {
                    // Dash is on this line
                    p2 = cline.point_at(dpos / elen);
                    pos = dpos + estart;
                    done = pos >= estop;
                    idash += 1;
                    if idash >= dash_count {
                        idash = 0;
                    }
                    doffset = 0.0; // full segment so no offset on next.
                }

                if even_dash {
                    let line_to_pos = p2;

                    // If we have an offset, we're continuing a dash
                    // from a previous element and should only
                    // continue the current dash, without starting a
                    // new subpath.
                    if !has_offset || !has_move_to {
                        self.stroker.move_to(move_to_pos.x, move_to_pos.y);
                        has_move_to = true;
                    }

                    self.stroker.line_to(line_to_pos.x, line_to_pos.y);
                    move_to_pos = line_to_pos;
                } else {
                    move_to_pos = p2;
                }

                // Qt hangs here; ferrokit draws the rest of the line solid. Far from the origin
                // a dash is shorter than the spacing of `f64`s at `pos` (`1e17 + 1 == 1e17`), so
                // `pos` stops advancing and Qt turns the pattern over and over without emitting
                // anything. With every dash longer than 0, `pos` advances in each of
                // `dash_count` turns, so more turns than that without advancing (or more turns
                // than any line has, after the 10 000 turn limit) are such a stall. Dashes this
                // small are not visible, which is why the line is drawn solid, as Qt's own
                // shortcut for too many dashes does.
                turns += 1;
                stalled = if pos > before { 0 } else { stalled + 1 };
                if !done && (stalled > 2 * 32 + 2 || turns > MAX_TURNS_PER_LINE) {
                    if !has_move_to {
                        self.stroker.move_to(move_to_pos.x, move_to_pos.y);
                        has_move_to = true;
                    }
                    self.stroker.line_to(e.x, e.y);
                    move_to_pos = P::new(e.x, e.y);
                    pos = estop;
                    doffset = 0.0;
                    done = true;
                }
            }

            // Shuffle to the next cycle...
            estart = estop;
            prev = P::new(e.x, e.y);
        }
    }
}

/// `QPainterPathStroker`: generates fillable outlines of paths.
///
/// The defaults are those of a fresh `QPainterPathStroker`: width 1, square caps, bevel joins,
/// miter limit 2, curve threshold 0.25 and no dashes.
///
/// ```
/// use ferrokit_gui::geometry::primitives::RectF;
/// use ferrokit_gui::paint::{PainterPath, PainterPathStroker, PenCapStyle};
///
/// // The hatched stretch of the HUD dial's outer ring.
/// let ring = RectF::new(4.0, 4.0, 72.0, 72.0);
/// let mut arc = PainterPath::new();
/// arc.arc_move_to(ring, 90.0);
/// arc.arc_to(ring, 90.0, -120.0);
/// let mut stroker = PainterPathStroker::new();
/// stroker.set_width(6.0);
/// stroker.set_cap_style(PenCapStyle::Flat);
/// let outline = stroker.create_stroke(&arc);
/// assert!(!outline.is_empty());
/// ```
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
    /// `QPainterPathStroker()`.
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

    /// `QPainterPathStroker::setWidth`: a width of 0 or less is 1. As in Qt this also resets the
    /// curve threshold to `clamp(1 / width, 0.00025, 0.25)`.
    ///
    /// Qt hangs/UB here; ferrokit also takes a NaN or infinite width as 1 (Qt computes offsets
    /// from it that are NaN).
    pub fn set_width(&mut self, width: f64) {
        let width = if width <= 0.0 || !width.is_finite() {
            1.0
        } else {
            width
        };
        self.width = width;
        self.curve_threshold = (1.0 / width).clamp(0.00025, 0.25);
    }

    /// `QPainterPathStroker::width`.
    pub fn width(&self) -> f64 {
        self.width
    }

    /// `QPainterPathStroker::setCapStyle`.
    pub fn set_cap_style(&mut self, cap: PenCapStyle) {
        self.cap = cap;
    }

    /// `QPainterPathStroker::capStyle`.
    pub fn cap_style(&self) -> PenCapStyle {
        self.cap
    }

    /// `QPainterPathStroker::setJoinStyle`.
    pub fn set_join_style(&mut self, join: PenJoinStyle) {
        self.join = join;
    }

    /// `QPainterPathStroker::joinStyle`.
    pub fn join_style(&self) -> PenJoinStyle {
        self.join
    }

    /// `QPainterPathStroker::setMiterLimit`: the miter length limit in widths.
    pub fn set_miter_limit(&mut self, limit: f64) {
        self.miter_limit = limit;
    }

    /// `QPainterPathStroker::miterLimit`.
    pub fn miter_limit(&self) -> f64 {
        self.miter_limit
    }

    /// `QPainterPathStroker::setCurveThreshold`: the flatness at which offset curves are accepted.
    ///
    /// Qt hangs/UB here; ferrokit takes a threshold that is 0, negative or NaN as 0.00025, the
    /// smallest `setWidth` ever chooses. `QBezier::shifted` relaxes a threshold it cannot meet by
    /// a factor of 1.5 until it exceeds 2, which a threshold of 0 or NaN never does, so Qt's
    /// `createStroke` of any curve does not return.
    pub fn set_curve_threshold(&mut self, threshold: f64) {
        self.curve_threshold = if threshold > 0.0 {
            threshold
        } else {
            MIN_CURVE_THRESHOLD
        };
    }

    /// `QPainterPathStroker::curveThreshold`.
    pub fn curve_threshold(&self) -> f64 {
        self.curve_threshold
    }

    /// `QPainterPathStroker::setDashPattern(Qt::PenStyle)`.
    pub fn set_dash_style(&mut self, style: PenStyle) {
        // `QDashStroker::patternForStyle`
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

    /// `QPainterPathStroker::setDashPattern(QList<qreal>)`: alternating dash and gap lengths, in
    /// widths. An empty pattern is a solid stroke. Like Qt, at most 32 entries are used and a
    /// trailing odd entry is ignored.
    pub fn set_dash_pattern(&mut self, pattern: &[f64]) {
        self.dash_pattern = pattern.to_vec();
    }

    /// `QPainterPathStroker::dashPattern`.
    pub fn dash_pattern(&self) -> &[f64] {
        &self.dash_pattern
    }

    /// `QPainterPathStroker::setDashOffset`: in widths.
    pub fn set_dash_offset(&mut self, offset: f64) {
        self.dash_offset = offset;
    }

    /// `QPainterPathStroker::dashOffset`.
    pub fn dash_offset(&self) -> f64 {
        self.dash_offset
    }

    /// `QPainterPathStroker::createStroke`: the outline of the stroke of `path` as a path to be
    /// filled with the winding rule: the fill rule of a non-empty result is
    /// [`FillRule::Winding`]. An empty `path` (no elements, or only its initial move) is
    /// returned as a copy that keeps the input path's [`fill_rule`](PainterPath::fill_rule), as
    /// Qt returns its input unchanged.
    ///
    /// Qt hangs/UB here; ferrokit guards against input that makes Qt's strokers loop for ever or
    /// allocate without bound, and does not panic on any input:
    ///
    /// * A dash pattern with a NaN or infinite entry is ignored (the stroke is solid), and a
    ///   NaN or infinite dash offset is 0. Qt's dash loop never ends on NaN positions.
    /// * A dash pattern whose cycle has no length draws nothing: that is Qt's rule for a pattern
    ///   that sums to 0, extended to `[0, 0, 1]`, whose used entries are `0, 0` (a trailing odd
    ///   entry is not part of the cycle) and which makes Qt's offset loop spin. A pattern whose
    ///   length overflows `f64` when scaled by the width draws nothing as well.
    /// * The cycle of a dash pattern is the entries Qt actually cycles through: the even prefix,
    ///   or the single entry of a pattern with one. Qt measures its 10 000-turn limit (past which
    ///   a line is drawn solid) and the solid shortcut's cut-off by the sum of all entries, which
    ///   is longer for an odd count; there Qt's shortcut shifts the pattern and ferrokit's does
    ///   not. Qt's offset walk (one subtraction per dash) takes `sum / cycle` turns of the
    ///   pattern, up to 5e17 for `[1e-8, 1e-8, 1e10]`; ferrokit reduces it by the cycle.
    /// * A stroke has at most [`MAX_ELEMENTS`](Self::MAX_ELEMENTS) elements (a cubic is three):
    ///   what the stroker has produced by then is returned and the rest of the path is not
    ///   stroked, so a dashed stroke of a long path cannot allocate hundreds of megabytes. This
    ///   is silent; [`create_stroke_checked`](Self::create_stroke_checked) reports it.
    /// * Far from the origin the dashes of a long path can be shorter than the gap between two
    ///   neighbouring `f64`s at the position along the path (`1e17 + 1 == 1e17`), so the
    ///   position stops advancing and Qt's dash loop turns the pattern for ever without
    ///   emitting anything. ferrokit notices that the position does not advance and draws the
    ///   rest of that line solid, as Qt's own shortcut for more than 10 000 turns does.
    /// * The width ([`set_width`](Self::set_width)) and the curve threshold
    ///   ([`set_curve_threshold`](Self::set_curve_threshold)) cannot be set to values that
    ///   make Qt's offset-curve search endless.
    ///
    /// The cut at [`MAX_ELEMENTS`](Self::MAX_ELEMENTS) is silent here, as Qt has no such limit;
    /// [`create_stroke_checked`](Self::create_stroke_checked) tells whether it happened.
    pub fn create_stroke(&self, path: &PainterPath) -> PainterPath {
        self.create_stroke_checked(path).0
    }

    /// [`create_stroke`](Self::create_stroke), also telling whether the stroke was cut short at
    /// [`MAX_ELEMENTS`](Self::MAX_ELEMENTS) elements (`true`). Like the stroke, the answer is
    /// only about this call; the stroker keeps no state. It can be `true` for a stroke that
    /// needed just the elements it has, when the limit was reached with input left that turned
    /// out to add nothing.
    pub fn create_stroke_checked(&self, path: &PainterPath) -> (PainterPath, bool) {
        let input = path.qt_elements();
        // `QPainterPath::isEmpty`: no elements, or just the initial move.
        if input.is_empty() || (input.len() == 1 && input[0].kind == PathElementType::MoveTo) {
            // Qt returns its copy of the path, which has no zero-length lines and no `Close`.
            let mut same = PainterPath::from_qt_elements(&input);
            same.set_fill_rule(path.fill_rule());
            return (same, false);
        }
        let (elements, truncated) = self.stroke_elements_limited(&input, MAX_STROKE_ELEMENTS);
        let mut stroke = PainterPath::from_qt_elements(&elements);
        // `QPainterPath::setFillRule` creates the path data, which starts with a move to (0, 0):
        // a stroke with no outline is that single move, which `QPainterPath::isEmpty` counts empty.
        stroke.ensure_qt_data();
        stroke.set_fill_rule(FillRule::Winding);
        (stroke, truncated)
    }

    /// The most elements of a [`create_stroke`](Self::create_stroke) result (a cubic is three).
    pub const MAX_ELEMENTS: usize = MAX_STROKE_ELEMENTS;

    /// The elements `createStroke` produces for the elements of a non-empty path, in `f64`.
    #[cfg(test)]
    pub(crate) fn stroke_elements(&self, input: &[PathElementPoint]) -> Vec<PathElementPoint> {
        self.stroke_elements_limited(input, MAX_STROKE_ELEMENTS).0
    }

    /// `stroke_elements` with at most `max_elements` elements.
    pub(crate) fn stroke_elements_limited(
        &self,
        input: &[PathElementPoint],
        max_elements: usize,
    ) -> (Vec<PathElementPoint>, bool) {
        let mut stroker = Stroker::new();
        stroker.out.max_elements = max_elements;
        stroker.stroke_width = self.width;
        stroker.curve_threshold = self.curve_threshold;
        stroker.cap = cap_mode(self.cap);
        stroker.join = join_mode(self.join);
        stroker.miter_limit = self.miter_limit;

        if self.dash_pattern.is_empty() || !self.dash_pattern.iter().all(|d| d.is_finite()) {
            stroker.stroke_path(input);
        } else {
            let offset = if self.dash_offset.is_finite() {
                self.dash_offset
            } else {
                0.0
            };
            DashStroker::new(&mut stroker, &self.dash_pattern, offset).stroke_path(input);
        }
        (stroker.out.elements, stroker.out.truncated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORACLE: &str = include_str!("../../tests/data/qpath_oracle.txt");

    fn f(s: &str) -> f64 {
        f64::from_bits(u64::from_str_radix(s, 16).unwrap())
    }

    /// The double-precision stage of `QPainterPathStroker::createStroke` against Qt's doubles:
    /// the `f32` narrowing of `PainterPath` is what `tests/test_qpath_stroker.rs` tolerates, not
    /// something this stage may hide behind.
    #[test]
    fn stroke_elements_match_qt_to_1e_9() {
        // Under Miri one stroke in `STRIDE` is checked (see `tests/test_qpath_stroker.rs`).
        const STRIDE: usize = if cfg!(miri) { 16 } else { 1 };
        let mut cases = 0;
        for (n, line) in ORACLE
            .lines()
            .filter(|l| l.starts_with("stroke "))
            .enumerate()
        {
            if !n.is_multiple_of(STRIDE) {
                continue;
            }
            let t: Vec<&str> = line.split(' ').skip(1).collect();
            let mut i = 0;
            let mut next = || {
                i += 1;
                t[i - 1]
            };
            let width = f(next());
            let cap = [PenCapStyle::Flat, PenCapStyle::Square, PenCapStyle::Round]
                [next().parse::<usize>().unwrap()];
            let join = [
                PenJoinStyle::Miter,
                PenJoinStyle::Bevel,
                PenJoinStyle::Round,
                PenJoinStyle::SvgMiter,
            ][next().parse::<usize>().unwrap()];
            let (miter, offset, threshold) = (f(next()), f(next()), f(next()));
            let dashes: Vec<f64> = (0..next().parse::<usize>().unwrap())
                .map(|_| f(next()))
                .collect();
            let mut path = PainterPath::new();
            for _ in 0..next().parse::<usize>().unwrap() {
                let op = next();
                let mut a = |n: usize| -> Vec<f32> { (0..n).map(|_| f(next()) as f32).collect() };
                match op {
                    "M" => {
                        let v = a(2);
                        path.move_to(v[0], v[1]);
                    }
                    "L" => {
                        let v = a(2);
                        path.line_to(v[0], v[1]);
                    }
                    "Q" => {
                        let v = a(4);
                        path.quad_to(v[0], v[1], v[2], v[3]);
                    }
                    "C" => {
                        let v = a(6);
                        path.cubic_to(v[0], v[1], v[2], v[3], v[4], v[5]);
                    }
                    _ => path.close_subpath(),
                }
            }
            let n: usize = next().parse().unwrap();
            let qt: Vec<(u8, f64, f64)> = (0..n)
                .map(|_| (next().parse().unwrap(), f(next()), f(next())))
                .collect();

            let mut stroker = PainterPathStroker::new();
            stroker.set_width(width);
            stroker.set_cap_style(cap);
            stroker.set_join_style(join);
            stroker.set_miter_limit(miter);
            if threshold != 0.0 {
                stroker.set_curve_threshold(threshold);
            }
            stroker.set_dash_offset(offset);
            if !dashes.is_empty() {
                stroker.set_dash_pattern(&dashes);
            }
            let input = path.qt_elements();
            let ours = if input.len() < 2 {
                Vec::new()
            } else {
                let mut out = stroker.stroke_elements(&input);
                if out.is_empty() {
                    // `createStroke`'s `setFillRule` leaves a move to (0, 0).
                    out.push(PathElementPoint {
                        kind: PathElementType::MoveTo,
                        x: 0.0,
                        y: 0.0,
                    });
                }
                out
            };
            if input.len() >= 2 {
                assert_eq!(ours.len(), qt.len(), "case {cases}: element count");
                for (k, (a, b)) in ours.iter().zip(&qt).enumerate() {
                    let kind = match a.kind {
                        PathElementType::MoveTo => 0,
                        PathElementType::LineTo => 1,
                        PathElementType::CurveTo => 2,
                        PathElementType::CurveToData => 3,
                    };
                    assert_eq!(kind, b.0, "case {cases}: element {k} type");
                    assert!(
                        (a.x - b.1).abs() <= 1e-9 && (a.y - b.2).abs() <= 1e-9,
                        "case {cases}: element {k} is ({}, {}), Qt has ({}, {})",
                        a.x,
                        a.y,
                        b.1,
                        b.2
                    );
                }
            }
            cases += 1;
        }
        assert!(cases * STRIDE > 390, "{cases} stroke cases");
    }

    fn line(x: f64, y: f64) -> Vec<PathElementPoint> {
        vec![
            element(PathElementType::MoveTo, 0.0, 0.0),
            element(PathElementType::LineTo, x, y),
        ]
    }

    /// Qt hangs/UB: more elements than the limit are never made.
    #[test]
    fn the_element_limit_cuts_a_stroke_short() {
        // Dashes: 1e4 turns of the pattern per line, so a few lines make many elements.
        let mut dashed = PainterPathStroker::new();
        dashed.set_dash_pattern(&[1.0, 1.0]);
        let mut path = vec![element(PathElementType::MoveTo, 0.0, 0.0)];
        for i in 1..if cfg!(miri) { 4 } else { 40 } {
            path.push(element(
                PathElementType::LineTo,
                f64::from(i) * 1000.0,
                f64::from(i % 2) * 7.0,
            ));
        }
        let all = dashed.stroke_elements_limited(&path, usize::MAX).0;
        assert!(all.len() > 300, "{}", all.len());
        for limit in [3, 10, 100, 777] {
            let cut = dashed.stroke_elements_limited(&path, limit).0;
            assert!(
                !cut.is_empty() && cut.len() <= limit,
                "{limit}: {}",
                cut.len()
            );
            // What is produced is the start of the unlimited stroke.
            assert_eq!(cut[0], all[0]);
        }
        // The same without dashes: many segments of a subpath.
        let solid = PainterPathStroker::new();
        let mut zigzag = vec![element(PathElementType::MoveTo, 0.0, 0.0)];
        for i in 1..if cfg!(miri) { 200 } else { 2000 } {
            zigzag.push(element(
                PathElementType::LineTo,
                f64::from(i),
                f64::from(i % 2) * 5.0,
            ));
        }
        let all = solid.stroke_elements_limited(&zigzag, usize::MAX).0;
        assert!(all.len() > 600);
        let cut = solid.stroke_elements_limited(&zigzag, 500).0;
        assert!(!cut.is_empty() && cut.len() <= 500, "{}", cut.len());
        assert_eq!(PainterPathStroker::MAX_ELEMENTS, 1 << 20);
    }

    /// Qt hangs/UB: dash patterns and offsets that make its loops spin.
    #[test]
    fn degenerate_dash_patterns_and_offsets_terminate() {
        let path = line(40.0, 0.0);
        let solid = PainterPathStroker::new().stroke_elements(&path);
        let strokes = |pattern: &[f64], offset: f64, width: f64| {
            let mut s = PainterPathStroker::new();
            s.set_width(width);
            s.set_dash_pattern(pattern);
            s.set_dash_offset(offset);
            s.stroke_elements(&path)
        };
        // A cycle of no length (`[0, 0, 1]`, `[0, 0]`, `[0]`): nothing is drawn.
        for pattern in [
            &[0.0, 0.0, 1.0][..],
            &[0.0, 0.0],
            &[0.0],
            &[0.0, 0.0, 0.0, 0.0, 7.0],
        ] {
            assert!(strokes(pattern, 0.0, 1.0).is_empty(), "{pattern:?}");
        }
        // NaN and infinite entries: the pattern is ignored.
        for pattern in [
            &[f64::NAN, 1.0][..],
            &[1.0, f64::INFINITY],
            &[1.0, 1.0, f64::NEG_INFINITY],
        ] {
            assert_eq!(strokes(pattern, 0.0, 1.0), solid, "{pattern:?}");
        }
        // NaN and infinite offsets are 0, a huge finite one is reduced like any other.
        let plain = strokes(&[2.0, 1.0], 0.0, 1.0);
        for offset in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(strokes(&[2.0, 1.0], offset, 1.0), plain, "{offset}");
        }
        assert!(!strokes(&[2.0, 1.0], 1e300, 1.0).is_empty());
        assert!(!strokes(&[2.0, 1.0], -1e300, 1.0).is_empty());
        // Width times entries beyond `f64`: nothing is drawn; so is an offset that overflows to
        // infinity, which starts at the beginning of the pattern.
        assert!(strokes(&[1e308, 1e308], 0.0, 10.0).is_empty());
        assert_eq!(
            strokes(&[2.0, 1.0], 1e308, 10.0),
            strokes(&[2.0, 1.0], 0.0, 10.0)
        );
        // An odd pattern whose last entry is huge: the offset is reduced by the cycle, not walked
        // off one dash at a time (Qt turns the pattern sum / cycle = 5e17 times), and the line is
        // drawn solid after 10 000 cycles.
        let odd = strokes(&[1e-8, 1e-8, 1e10], 3.0e-9, 1.0);
        assert!(!odd.is_empty() && odd.len() < 100, "{}", odd.len());
        // The cycle of an odd pattern is its even prefix.
        assert_eq!(strokes(&[2.0, 1.0, 1e9], 0.0, 1.0), plain);
    }

    /// Qt hangs/UB: thresholds and widths that stop `QBezier::shifted`.
    #[test]
    fn unusable_curve_thresholds_and_widths_are_replaced() {
        let mut s = PainterPathStroker::new();
        for threshold in [0.0, -0.5, f64::NAN, f64::NEG_INFINITY] {
            s.set_curve_threshold(threshold);
            assert_eq!(s.curve_threshold(), 0.00025, "{threshold}");
        }
        s.set_curve_threshold(0.5);
        assert_eq!(s.curve_threshold(), 0.5);
        s.set_curve_threshold(f64::INFINITY);
        assert_eq!(s.curve_threshold(), f64::INFINITY);
        for width in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -3.0, 0.0] {
            s.set_width(width);
            assert_eq!((s.width(), s.curve_threshold()), (1.0, 0.25), "{width}");
        }

        let mut curve = PainterPath::new();
        curve.move_to(0.0, 0.0);
        curve.cubic_to(30.0, 0.0, 0.0, 20.0, 40.0, 25.0);
        let mut zero = PainterPathStroker::new();
        zero.set_width(3.0);
        zero.set_curve_threshold(0.0);
        let mut small = PainterPathStroker::new();
        small.set_width(3.0);
        small.set_curve_threshold(0.00025);
        assert_eq!(
            zero.create_stroke(&curve).elements(),
            small.create_stroke(&curve).elements()
        );
    }

    /// Qt hangs here: the position along the path stops advancing (dashes below the spacing of
    /// `f64`s at it), so the dash loop never ends and emits nothing.
    #[test]
    fn dashes_below_the_float_spacing_are_drawn_solid() {
        let line = |points: &[(f64, f64)]| -> Vec<PathElementPoint> {
            let mut v = vec![element(PathElementType::MoveTo, points[0].0, points[0].1)];
            for p in &points[1..] {
                v.push(element(PathElementType::LineTo, p.0, p.1));
            }
            v
        };
        let stroke = |width: f64, path: &[PathElementPoint]| {
            let mut s = PainterPathStroker::new();
            s.set_width(width);
            s.set_dash_pattern(&[1.0, 1.0]);
            s.stroke_elements_limited(path, MAX_STROKE_ELEMENTS)
        };
        // `1e17 + 1 == 1e17`: the second line is 5000 long, 2500 turns of the pattern.
        let (elements, truncated) = stroke(1.0, &line(&[(0.0, 0.0), (1e17, 0.0), (1e17, 5000.0)]));
        assert!(!truncated);
        assert!(
            elements.iter().any(|e| (e.y - 5000.0).abs() < 1.0),
            "{elements:?}"
        );
        // The same with small numbers: dashes of 1e-11 where `f64`s are 1.2e-10 apart.
        let (elements, _) = stroke(1e-11, &line(&[(0.0, 0.0), (1e6, 0.0), (1e6, 1e-7)]));
        assert!(
            elements.iter().any(|e| (e.x - 1e6).abs() < 1e-9),
            "{elements:?}"
        );
        // Lines a few dashes long at such a position are still dashed (nothing stalls).
        let (dashed, _) = stroke(1.0, &line(&[(0.0, 0.0), (1e5, 0.0), (1e5, 6.0)]));
        assert!(dashed.len() > 20, "{}", dashed.len());
    }

    #[test]
    fn truncation_is_reported() {
        let mut dashed = PainterPathStroker::new();
        dashed.set_dash_pattern(&[1.0, 1.0]);
        let path = line(if cfg!(miri) { 400.0 } else { 5000.0 }, 0.0);
        let (all, truncated) = dashed.stroke_elements_limited(&path, usize::MAX);
        assert!(all.len() > 200 && !truncated);
        let (cut, truncated) = dashed.stroke_elements_limited(&path, 100);
        assert!(cut.len() <= 100 && truncated);
        // Room for the elements needed (and one more cubic, which `full` keeps free) is not a
        // truncation.
        let (_, truncated) = dashed.stroke_elements_limited(&path, all.len() + 3);
        assert!(!truncated);
        let (_, truncated) = dashed.stroke_elements_limited(&path, all.len() - 1);
        assert!(truncated);
    }
}
