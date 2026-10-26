// SPDX-License-Identifier: LGPL-3.0-only
//! `QPainterPath::arcTo` / `arcMoveTo`, `QPainterPathStroker` and `QPainter::drawPolygon` of
//! `ferrokit_gui::paint` against Qt 6.11.2.
//!
//! `tests/data/qpath_oracle.txt` holds measurements of PySide6 6.11.2's real `QPainterPath`,
//! `QPainterPathStroker` and `QPainter` (regenerate it with `tests/data/gen_qpath_oracle.py`,
//! which also documents the line formats): the element lists of ~900 arcs, ~390 strokes of random
//! and hand-picked paths over every cap, join, miter limit and dash pattern, the HUD dial's
//! hatch stroke with its `controlPointRect`, area and fill-polygon bounds, and anti-aliased
//! renderings of `drawPolygon`, of the dial's pie wedge and of the ring hatch.
//!
//! Qt computes in `double`; ferrokit computes in `f64` too but stores `f32` in `PainterPath`, so
//! a coordinate is compared to Qt's within `1e-6` plus the relative rounding error of an `f32`
//! (`6e-8`). The double-precision stage itself is checked against the same file to `1e-9` by the
//! unit tests of `paint/qpath_geom.rs` (arcs) and `paint/stroker.rs` (strokes).

use ferrokit_gui::geometry::primitives::{PointF, RectF};
use ferrokit_gui::paint::path::{FillRule, PathElement};
use ferrokit_gui::paint::pixmap::Pixmap;
use ferrokit_gui::paint::{
    Brush, Painter, PainterPath, PainterPathStroker, Pen, PenCapStyle, PenJoinStyle, RenderHint,
};
use tiny_skia::{Color, LineCap, LineJoin};

const ORACLE: &str = include_str!("data/qpath_oracle.txt");

/// Under Miri one oracle line in `STRIDE` is checked: the file has ~1 400 lines and Miri runs
/// the whole of it about 1 000 times slower than native code.
const STRIDE: usize = if cfg!(miri) { 16 } else { 1 };

fn lines(kind: &'static str) -> impl Iterator<Item = Vec<&'static str>> {
    ORACLE
        .lines()
        .filter(move |l| l.split(' ').next() == Some(kind))
        .map(|l| l.split(' ').skip(1).collect())
}

/// Reads the tokens of an oracle line in order.
struct Tokens<'a>(std::slice::Iter<'a, &'static str>);

impl<'a> Tokens<'a> {
    fn new(t: &'a [&'static str]) -> Self {
        Tokens(t.iter())
    }
    fn s(&mut self) -> &'static str {
        self.0.next().expect("oracle line too short")
    }
    fn f(&mut self) -> f64 {
        f64::from_bits(u64::from_str_radix(self.s(), 16).unwrap())
    }
    fn f32(&mut self) -> f32 {
        let v = self.f();
        assert!(
            v.is_nan() || f64::from(v as f32) == v,
            "oracle input {v} is not an f32"
        );
        v as f32
    }
    fn n(&mut self) -> usize {
        self.s().parse().unwrap()
    }
}

/// Qt's `(type, x, y)` element list of a path of moves, lines and cubics.
fn qt_elements(path: &PainterPath) -> Vec<(u8, f64, f64)> {
    let mut out = Vec::new();
    for e in path.elements() {
        match e {
            PathElement::MoveTo(p) => out.push((0, f64::from(p.x), f64::from(p.y))),
            PathElement::LineTo(p) => out.push((1, f64::from(p.x), f64::from(p.y))),
            PathElement::CubicTo { ctrl1, ctrl2, to } => {
                out.push((2, f64::from(ctrl1.x), f64::from(ctrl1.y)));
                out.push((3, f64::from(ctrl2.x), f64::from(ctrl2.y)));
                out.push((3, f64::from(to.x), f64::from(to.y)));
            }
            other => panic!("unexpected element {other:?}"),
        }
    }
    out
}

/// The `N (T X Y)*` tail of an oracle line.
fn oracle_elements(t: &mut Tokens) -> Vec<(u8, f64, f64)> {
    let n = t.n();
    (0..n)
        .map(|_| (t.s().parse().unwrap(), t.f(), t.f()))
        .collect()
}

/// Allowed distance of an `f32` coordinate from Qt's `double`.
fn tolerance(qt: f64) -> f64 {
    1e-6 + qt.abs() * 6e-8
}

fn assert_same_elements(ours: &[(u8, f64, f64)], qt: &[(u8, f64, f64)], what: &str) {
    assert_eq!(ours.len(), qt.len(), "{what}: element count");
    for (i, (a, b)) in ours.iter().zip(qt).enumerate() {
        assert_eq!(a.0, b.0, "{what}: element {i} type");
        assert!(
            (a.1 - b.1).abs() <= tolerance(b.1) && (a.2 - b.2).abs() <= tolerance(b.2),
            "{what}: element {i} is ({}, {}), Qt has ({}, {})",
            a.1,
            a.2,
            b.1,
            b.2
        );
    }
}

#[test]
fn arc_to_and_arc_move_to_match_qt_element_lists() {
    let mut cases = 0;
    for (n, line) in lines("arc").enumerate() {
        if n % STRIDE != 0 {
            continue;
        }
        let mut t = Tokens::new(&line);
        let mode = t.n();
        let rect = RectF::new(t.f32(), t.f32(), t.f32(), t.f32());
        let (start, sweep) = (t.f32(), t.f32());
        let qt = oracle_elements(&mut t);

        let mut path = PainterPath::new();
        match mode {
            0 => {}
            1 => path.arc_move_to(rect, start),
            _ => {
                path.move_to(1.5, 2.5);
                path.line_to(3.0, 4.0);
            }
        }
        path.arc_to(rect, start, sweep);
        assert_same_elements(
            &qt_elements(&path),
            &qt,
            &format!("mode {mode} {rect:?} start {start} sweep {sweep}"),
        );
        cases += 1;
    }
    assert!(cases * STRIDE > 900, "{cases} arc cases");
}

#[test]
fn arc_to_makes_the_curves_of_a_quarter_circle_like_add_ellipse() {
    // A 90 degree arc at an axis is the prototype quadrant of `qt_curves_for_arc`.
    let rect = RectF::new(0.0, 0.0, 100.0, 100.0);
    let mut path = PainterPath::new();
    path.arc_move_to(rect, 0.0);
    path.arc_to(rect, 0.0, 90.0);
    let e = qt_elements(&path);
    assert_eq!(e.len(), 4, "move and one cubic: {e:?}");
    assert_eq!((e[0].1, e[0].2), (100.0, 50.0));
    assert!(
        (e[3].1 - 50.0).abs() < 1e-4 && e[3].2.abs() < 1e-4,
        "ends at 12 o'clock: {e:?}"
    );
    // The control points are KAPPA of the radius from the ends.
    assert!(
        (e[1].2 - (50.0 - 50.0 * 0.552_284_749_8)).abs() < 1e-4,
        "{e:?}"
    );
}

#[test]
fn arc_move_to_replaces_a_preceding_move_and_invalid_arguments_are_ignored() {
    let rect = RectF::new(0.0, 0.0, 10.0, 10.0);
    let mut path = PainterPath::new();
    path.arc_move_to(rect, 0.0);
    path.arc_move_to(rect, 90.0);
    assert_eq!(
        path.elements().len(),
        1,
        "a move after a move replaces it (QPainterPath::moveTo)"
    );

    let mut path = PainterPath::new();
    path.arc_to(RectF::new(0.0, 0.0, 0.0, 0.0), 0.0, 90.0);
    path.arc_to(rect, f32::NAN, 90.0);
    path.arc_to(rect, 0.0, f32::INFINITY);
    path.arc_move_to(rect, f32::NAN);
    assert!(
        path.elements().is_empty(),
        "ignored without touching the path"
    );
    let p = PainterPath::with_start_point(PointF::new(3.0, 4.0));
    assert_eq!(p.current_position(), PointF::new(3.0, 4.0));
}

#[test]
fn stroker_matches_qt_element_lists() {
    let mut cases = 0;
    for (n, line) in lines("stroke").enumerate() {
        if n % STRIDE != 0 {
            continue;
        }
        let mut t = Tokens::new(&line);
        let width = t.f();
        let cap = [PenCapStyle::Flat, PenCapStyle::Square, PenCapStyle::Round][t.n()];
        let join = [
            PenJoinStyle::Miter,
            PenJoinStyle::Bevel,
            PenJoinStyle::Round,
            PenJoinStyle::SvgMiter,
        ][t.n()];
        let (miter, offset, threshold) = (t.f(), t.f(), t.f());
        let dashes: Vec<f64> = (0..t.n()).map(|_| t.f()).collect();
        let mut path = PainterPath::new();
        for _ in 0..t.n() {
            match t.s() {
                "M" => path.move_to(t.f32(), t.f32()),
                "L" => path.line_to(t.f32(), t.f32()),
                "Q" => path.quad_to(t.f32(), t.f32(), t.f32(), t.f32()),
                "C" => path.cubic_to(t.f32(), t.f32(), t.f32(), t.f32(), t.f32(), t.f32()),
                "Z" => path.close_subpath(),
                op => panic!("unknown op {op}"),
            }
        }
        let qt = oracle_elements(&mut t);

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
        let stroke = stroker.create_stroke(&path);
        if stroke.fill_rule() != FillRule::Winding {
            assert_eq!(stroke.fill_rule(), path.fill_rule());
            assert!(qt.len() <= 1 && qt.first().map_or(true, |e| e.0 == 0));
        }
        assert_same_elements(
            &qt_elements(&stroke),
            &qt,
            &format!("{cases}: width {width} {cap:?} {join:?} miter {miter} dashes {dashes:?}"),
        );
        cases += 1;
    }
    assert!(cases * STRIDE > 390, "{cases} stroke cases");
}

#[test]
fn stroker_width_rules_are_qts() {
    let mut cases = 0;
    for line in lines("width") {
        let mut t = Tokens::new(&line);
        let (requested, stored, threshold) = (t.f(), t.f(), t.f());
        let mut stroker = PainterPathStroker::new();
        stroker.set_width(requested);
        assert_eq!(stroker.width(), stored, "setWidth({requested})");
        assert!(
            (stroker.curve_threshold() - threshold).abs() < 1e-15,
            "curveThreshold after setWidth({requested})"
        );
        cases += 1;
    }
    assert_eq!(cases, 7);

    let d = PainterPathStroker::new();
    assert_eq!(d.width(), 1.0);
    assert_eq!(d.cap_style(), PenCapStyle::Square);
    assert_eq!(d.join_style(), PenJoinStyle::Bevel);
    assert_eq!(d.miter_limit(), 2.0);
    assert_eq!(d.curve_threshold(), 0.25);
    assert!(d.dash_pattern().is_empty());
    assert_eq!(d.dash_offset(), 0.0);
}

/// Shoelace area of a path of lines and cubics, each cubic sampled `steps` times.
fn flat_points(path: &PainterPath, steps: usize) -> Vec<(f64, f64)> {
    let mut pts: Vec<(f64, f64)> = Vec::new();
    for e in path.elements() {
        match e {
            PathElement::MoveTo(p) | PathElement::LineTo(p) => {
                pts.push((f64::from(p.x), f64::from(p.y)))
            }
            PathElement::CubicTo { ctrl1, ctrl2, to } => {
                let p0 = *pts.last().unwrap();
                let c = |p: &PointF| (f64::from(p.x), f64::from(p.y));
                let (c1, c2, p3) = (c(ctrl1), c(ctrl2), c(to));
                for i in 1..=steps {
                    let t = i as f64 / steps as f64;
                    let m = 1.0 - t;
                    let (a, b, cc, d) = (m * m * m, 3.0 * m * m * t, 3.0 * m * t * t, t * t * t);
                    pts.push((
                        a * p0.0 + b * c1.0 + cc * c2.0 + d * p3.0,
                        a * p0.1 + b * c1.1 + cc * c2.1 + d * p3.1,
                    ));
                }
            }
            other => panic!("unexpected element {other:?}"),
        }
    }
    pts
}

fn shoelace(pts: &[(f64, f64)]) -> f64 {
    let n = pts.len();
    (0..n)
        .map(|i| {
            let (p, q) = (pts[i], pts[(i + 1) % n]);
            p.0 * q.1 - q.0 * p.1
        })
        .sum::<f64>()
        / 2.0
}

#[test]
fn hud_donut_arc_stroke_matches_qt_bounds_and_area() {
    let mut cases = 0;
    for (n, line) in lines("hud").enumerate() {
        if n % STRIDE.min(4) != 0 {
            continue;
        }
        let mut t = Tokens::new(&line);
        let rect = RectF::new(t.f32(), t.f32(), t.f32(), t.f32());
        let (start, sweep, ring) = (t.f32(), t.f32(), t.f());
        let count = t.n();
        let (cpx, cpy, cpw, cph) = (t.f(), t.f(), t.f(), t.f());
        let (area, px, py, pw, ph) = (t.f(), t.f(), t.f(), t.f(), t.f());

        // `usage_table.py`: `arc.arcMoveTo(r, a); arc.arcTo(r, a, s)`, then a flat-cap stroker
        // of the ring width and `fillPath(stroker.createStroke(arc), hatch)`.
        let mut arc = PainterPath::new();
        arc.arc_move_to(rect, start);
        arc.arc_to(rect, start, sweep);
        let mut stroker = PainterPathStroker::new();
        stroker.set_width(ring);
        stroker.set_cap_style(PenCapStyle::Flat);
        let outline = stroker.create_stroke(&arc);
        assert_eq!(
            qt_elements(&outline).len(),
            count,
            "element count of {rect:?}"
        );

        // `PainterPath` stores `f32`, so the arc is rounded before it is stroked, which Qt's
        // `double` arc is not; the offset curves magnify that to about 5e-5 at worst (the
        // `stroker_matches_qt_element_lists` strokes of `f32` inputs agree to the `f32` rounding).
        let cp = outline.control_point_rect().unwrap();
        for (ours, qt, what) in [
            (cp.x, cpx, "controlPointRect.x"),
            (cp.y, cpy, "controlPointRect.y"),
            (cp.width, cpw, "controlPointRect.width"),
            (cp.height, cph, "controlPointRect.height"),
        ] {
            assert!(
                (f64::from(ours) - qt).abs() <= 2e-4,
                "{what}: {ours} vs Qt {qt} for {rect:?} {start} {sweep}"
            );
        }

        // Qt's `toFillPolygon` flattens coarsely (to polygons inscribed in the curves), so its
        // area is a little under the exact one; the finely flattened outline agrees with it to 2%.
        let pts = flat_points(&outline, 64);
        let ours = shoelace(&pts).abs();
        assert!(
            (ours - area.abs()).abs() <= area.abs() * 0.02,
            "area {ours} vs Qt {area} for {rect:?} {start} {sweep}"
        );
        let (min_x, max_x) = pts
            .iter()
            .fold((f64::MAX, f64::MIN), |a, p| (a.0.min(p.0), a.1.max(p.0)));
        let (min_y, max_y) = pts
            .iter()
            .fold((f64::MAX, f64::MIN), |a, p| (a.0.min(p.1), a.1.max(p.1)));
        for (ours, qt, what) in [
            (min_x, px, "bounds.x"),
            (min_y, py, "bounds.y"),
            (max_x - min_x, pw, "bounds.width"),
            (max_y - min_y, ph, "bounds.height"),
        ] {
            assert!(
                (ours - qt).abs() <= 0.1,
                "{what}: {ours} vs Qt {qt} for {rect:?}"
            );
        }
        // The stroke is a ring segment of width `ring`: its area is the annulus sector, up to
        // the accuracy of Qt's offset curves (`curveThreshold()` is 1/width, here 0.2, so
        // `QBezier::shifted` accepts offsets a few percent off; the element lists above are
        // Qt's own, so the shortfall is Qt's).
        let r_mid = f64::from(rect.width) / 2.0;
        let sector = f64::from(sweep).abs().to_radians() / 2.0
            * ((r_mid + ring / 2.0).powi(2) - (r_mid - ring / 2.0).powi(2));
        assert!(
            (ours - sector).abs() <= sector * 0.04,
            "annulus sector {sector} vs {ours}"
        );
        cases += 1;
    }
    assert!(cases * STRIDE.min(4) >= 38, "{cases} HUD cases");
}

/// Renders what `QPainter::drawPolygon` is asked to in an oracle `poly` line.
fn draw_polygon_alpha(
    w: u32,
    h: u32,
    rule: FillRule,
    pen: f32,
    brush: bool,
    pts: &[PointF],
) -> Vec<u8> {
    let mut pixmap = Pixmap::new(w, h).unwrap();
    {
        let mut painter = Painter::begin(&mut pixmap);
        painter.set_render_hint(RenderHint::Antialiasing, true);
        painter.set_brush(if brush {
            Brush::Color(Color::WHITE)
        } else {
            Brush::NoBrush
        });
        painter.set_pen(if pen > 0.0 {
            Some(
                Pen::new(Color::WHITE, pen)
                    .with_cap(LineCap::Square)
                    .with_join(LineJoin::Bevel),
            )
        } else {
            None
        });
        painter.draw_polygon_points(pts, rule);
    }
    pixmap.data().chunks_exact(4).map(|px| px[3]).collect()
}

#[test]
fn draw_polygon_renders_like_qt() {
    let mut cases = 0;
    for line in lines("poly") {
        let mut t = Tokens::new(&line);
        let (w, h) = (t.n() as u32, t.n() as u32);
        let rule = if t.n() == 1 {
            FillRule::Winding
        } else {
            FillRule::OddEven
        };
        let pen = t.f32();
        let brush = t.n() == 1;
        let pts: Vec<PointF> = (0..t.n()).map(|_| PointF::new(t.f32(), t.f32())).collect();
        let qt = hex_bytes(t.s());
        assert_eq!(qt.len(), (w * h) as usize);

        let ours = draw_polygon_alpha(w, h, rule, pen, brush, &pts);
        // Both rasterisers anti-alias with their own coverage arithmetic (Qt integrates exact
        // areas, tiny-skia supersamples), so edge pixels differ a little and pixels at acute
        // corners, such as where the pentagram's edges cross, by more. The shape, the fill rule,
        // the outline and the total coverage must not differ. The HUD's alert triangle (the
        // first two cases) agrees to 20 of 255.
        let (mut worst, mut big, mut sum_ours, mut sum_qt) = (0i32, 0usize, 0i64, 0i64);
        for (a, b) in ours.iter().zip(&qt) {
            let d = (i32::from(*a) - i32::from(*b)).abs();
            worst = worst.max(d);
            big += usize::from(d > 40);
            sum_ours += i64::from(*a);
            sum_qt += i64::from(*b);
        }
        assert!(
            worst <= if cases < 2 { 20 } else { 128 },
            "case {cases}: worst difference {worst}"
        );
        assert!(
            big * 100 <= (w * h) as usize,
            "case {cases}: {big} pixels differ by more than 40"
        );
        let drift = (sum_ours - sum_qt).abs() as f64;
        assert!(
            drift <= (sum_qt as f64) * 0.02 + 255.0,
            "case {cases}: coverage {sum_ours} vs Qt {sum_qt}"
        );
        cases += 1;
    }
    assert_eq!(cases, 12);
}

fn hex_bytes(s: &str) -> Vec<u8> {
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap())
        .collect()
}

#[test]
fn draw_polygon_fill_rules_and_degenerate_polygons() {
    let star: Vec<PointF> = (0..5)
        .map(|k| {
            let a = std::f32::consts::TAU * (k * 2) as f32 / 5.0;
            PointF::new(20.0 + 17.0 * a.sin(), 20.0 - 17.0 * a.cos())
        })
        .collect();
    let centre = 20 * 40 + 20;
    let winding = draw_polygon_alpha(40, 40, FillRule::Winding, 0.0, true, &star);
    let odd_even = draw_polygon_alpha(40, 40, FillRule::OddEven, 0.0, true, &star);
    assert_eq!(
        winding[centre], 255,
        "the pentagram's centre is inside under the winding rule"
    );
    assert_eq!(odd_even[centre], 0, "and a hole under the odd-even rule");

    for pts in [vec![], vec![PointF::new(5.0, 5.0)]] {
        let drawn = draw_polygon_alpha(40, 40, FillRule::Winding, 2.0, true, &pts);
        assert!(
            drawn.iter().all(|&a| a == 0),
            "fewer than two points draw nothing"
        );
    }
    // A QPolygonF goes the same way as its points.
    let mut a = Pixmap::new(40, 40).unwrap();
    let mut b = Pixmap::new(40, 40).unwrap();
    let poly = ferrokit_gui::geometry::QPolygonF::from_points(star.clone());
    for (pixmap, as_polygon) in [(&mut a, true), (&mut b, false)] {
        let mut p = Painter::begin(pixmap);
        p.set_brush(Brush::Color(Color::WHITE));
        p.set_pen(None);
        if as_polygon {
            p.draw_polygon(&poly, FillRule::OddEven);
        } else {
            p.draw_polygon_points(&star, FillRule::OddEven);
        }
    }
    assert_eq!(a.data(), b.data());
}

#[test]
fn pie_wedge_built_with_a_start_point_fills_like_a_pie() {
    // `usage_table.py`: `wedge = QPainterPath(center); wedge.arcTo(inner, a, s);
    // wedge.closeSubpath(); p.fillPath(wedge, hatch)`.
    let inner = RectF::new(4.5, 4.5, 51.0, 51.0);
    let (start, sweep) = (90.0 - 100.0, -150.0);
    let centre = PointF::new(30.0, 30.0);
    let mut wedge = PainterPath::with_start_point(centre);
    wedge.arc_to(inner, start, sweep);
    wedge.close_subpath();
    let elements = qt_elements_with_close(&wedge);
    assert_eq!(elements.first(), Some(&(0, 30.0, 30.0)), "{elements:?}");
    assert_eq!(
        elements[1].0, 1,
        "a line from the centre to the arc's start: {elements:?}"
    );
    assert_eq!(
        elements.last().map(|e| e.0),
        Some(4),
        "closed: {elements:?}"
    );

    let ours = fill_alpha(60, 60, &wedge);
    let reference = ferrokit_gui::paint::painter::create_pie_path(inner, start, sweep)
        .map(|pie| {
            let mut pixmap = Pixmap::new(60, 60).unwrap();
            {
                let mut painter = Painter::begin(&mut pixmap);
                painter.set_render_hint(RenderHint::Antialiasing, true);
                painter.set_pen(None);
                painter.set_brush(Brush::Color(Color::WHITE));
                painter.fill_path(&pie);
            }
            pixmap
                .data()
                .chunks_exact(4)
                .map(|p| p[3])
                .collect::<Vec<u8>>()
        })
        .unwrap();
    // The two build their arcs differently (Qt's quadrant curves against 45 degree ones) and
    // tiny-skia flattens a long cubic more coarsely, so edge pixels differ by up to ~50 of 255.
    let worst = ours
        .iter()
        .zip(&reference)
        .map(|(a, b)| (i32::from(*a) - i32::from(*b)).abs())
        .max()
        .unwrap();
    assert!(worst <= 56, "worst pixel difference {worst}");
    assert!(ours.iter().any(|&a| a == 255));
}

/// The alpha of `path` filled with a white brush, anti-aliased.
fn fill_alpha(w: u32, h: u32, path: &PainterPath) -> Vec<u8> {
    let mut pixmap = Pixmap::new(w, h).unwrap();
    {
        let mut painter = Painter::begin(&mut pixmap);
        painter.set_render_hint(RenderHint::Antialiasing, true);
        painter.set_pen(None);
        painter.set_brush(Brush::Color(Color::WHITE));
        painter.fill_painter_path(path);
    }
    pixmap.data().chunks_exact(4).map(|p| p[3]).collect()
}

/// Pixels of `ours` against Qt's: the rasterisers differ at the edges (see
/// `draw_polygon_renders_like_qt`), the shape must not.
fn assert_renders_like_qt(ours: &[u8], qt: &[u8], what: &str) {
    assert_eq!(ours.len(), qt.len());
    let mut worst = 0;
    let (mut sum_ours, mut sum_qt) = (0i64, 0i64);
    for (a, b) in ours.iter().zip(qt) {
        worst = worst.max((i32::from(*a) - i32::from(*b)).abs());
        sum_ours += i64::from(*a);
        sum_qt += i64::from(*b);
    }
    assert!(worst <= 64, "{what}: worst pixel difference {worst}");
    assert!(
        (sum_ours - sum_qt).abs() as f64 <= sum_qt as f64 * 0.01 + 255.0,
        "{what}: coverage {sum_ours} vs Qt {sum_qt}"
    );
}

#[test]
fn hud_pie_wedge_and_ring_hatch_fill_like_qt() {
    let mut cases = 0;
    for line in lines("wedge") {
        let mut t = Tokens::new(&line);
        let (w, h) = (t.n() as u32, t.n() as u32);
        let centre = PointF::new(t.f32(), t.f32());
        let (x, y, side) = (t.f32(), t.f32(), t.f32());
        let rect = RectF::new(x, y, side, side);
        let (start, sweep) = (t.f32(), t.f32());
        let qt = hex_bytes(t.s());
        let mut wedge = PainterPath::with_start_point(centre);
        wedge.arc_to(rect, start, sweep);
        wedge.close_subpath();
        assert_renders_like_qt(
            &fill_alpha(w, h, &wedge),
            &qt,
            &format!("wedge {start} {sweep}"),
        );
        cases += 1;
    }
    assert_eq!(cases, 4);

    for line in lines("ringfill") {
        let mut t = Tokens::new(&line);
        let (w, h) = (t.n() as u32, t.n() as u32);
        let (x, y, side) = (t.f32(), t.f32(), t.f32());
        let side = RectF::new(x, y, side, side);
        let (start, sweep, ring) = (t.f32(), t.f32(), t.f());
        let qt = hex_bytes(t.s());
        let mut arc = PainterPath::new();
        arc.arc_move_to(side, start);
        arc.arc_to(side, start, sweep);
        let mut stroker = PainterPathStroker::new();
        stroker.set_width(ring);
        stroker.set_cap_style(PenCapStyle::Flat);
        let outline = stroker.create_stroke(&arc);
        assert_renders_like_qt(
            &fill_alpha(w, h, &outline),
            &qt,
            &format!("ring {start} {sweep}"),
        );
        cases += 1;
    }
    assert_eq!(cases, 7);
}

/// Like `qt_elements`, with `Close` as type 4.
fn qt_elements_with_close(path: &PainterPath) -> Vec<(u8, f64, f64)> {
    let mut out = Vec::new();
    for e in path.elements() {
        match e {
            PathElement::Close => out.push((4, 0.0, 0.0)),
            other => {
                let mut one = PainterPath::new();
                match other {
                    PathElement::MoveTo(p) => one.move_to(p.x, p.y),
                    PathElement::LineTo(p) => one.line_to(p.x, p.y),
                    PathElement::CubicTo { ctrl1, ctrl2, to } => {
                        one.cubic_to(ctrl1.x, ctrl1.y, ctrl2.x, ctrl2.y, to.x, to.y)
                    }
                    _ => unreachable!(),
                }
                out.extend(qt_elements(&one));
            }
        }
    }
    out
}

#[test]
fn arcs_ignore_angles_qt_overflows_on() {
    // Qt keeps the start quadrant in an `int`: beyond ~1.9e11 degrees it is undefined behaviour.
    // ferrokit ignores start angles beyond 1e9 degrees, 1e9 itself being drawn (the oracle has it).
    let rect = RectF::new(0.0, 0.0, 10.0, 10.0);
    for angle in [
        1.0e9 * 1.01,
        -2.0e9,
        1.0e12,
        f32::MAX,
        f32::MIN,
        f32::INFINITY,
        f32::NAN,
    ] {
        let mut path = PainterPath::new();
        path.arc_move_to(rect, angle);
        path.arc_to(rect, angle, 90.0);
        path.arc_to(rect, angle, f32::MAX);
        assert!(path.elements().is_empty(), "{angle}");
    }
    let mut path = PainterPath::new();
    path.arc_to(rect, 1.0e9, 90.0);
    assert!(!path.elements().is_empty());
    // A sweep of any size is a full turn at most.
    let mut path = PainterPath::new();
    path.arc_to(rect, 0.0, f32::MAX);
    assert_eq!(
        path.elements().len(),
        1 + 1 + 4,
        "move, line to the start, four quadrants"
    );
}

#[test]
fn create_stroke_does_not_hang_or_panic_on_hostile_settings() {
    let mut curve = PainterPath::new();
    curve.move_to(0.0, 0.0);
    curve.cubic_to(100.0, 0.0, 0.0, 0.01, 100.0, 50.0);
    curve.line_to(-30.0, 40.0);
    let mut line = PainterPath::new();
    line.move_to(0.0, 0.0);
    line.line_to(200.0, 0.0);
    // Under Miri a few of each (the whole product is 1 500 strokes).
    let few = if cfg!(miri) { 2 } else { usize::MAX };
    for width in [f64::NAN, f64::INFINITY, 0.0, 1e-300, 1e300]
        .into_iter()
        .take(few)
    {
        for threshold in [f64::NAN, 0.0, -1.0, 1e-300, f64::INFINITY]
            .into_iter()
            .take(few)
        {
            for dashes in [
                vec![],
                vec![0.0, 0.0, 1.0],
                vec![f64::NAN],
                vec![1e-300, 1e-300],
                vec![1e300, 1e300],
                vec![1.0, 2.0, 1e-9],
            ]
            .into_iter()
            .take(few.saturating_add(2))
            {
                for offset in [0.0, f64::NAN, f64::INFINITY, -1e300, 5.5]
                    .into_iter()
                    .take(few)
                {
                    let mut s = PainterPathStroker::new();
                    s.set_width(width);
                    s.set_curve_threshold(threshold);
                    s.set_dash_pattern(&dashes);
                    s.set_dash_offset(offset);
                    s.set_cap_style(PenCapStyle::Round);
                    s.set_join_style(PenJoinStyle::Round);
                    for path in [&curve, &line] {
                        let stroke = s.create_stroke(path);
                        assert!(stroke.elements().len() <= PainterPathStroker::MAX_ELEMENTS);
                    }
                }
            }
        }
    }
}

#[test]
fn create_stroke_checked_reports_truncation_and_survives_lost_precision() {
    let mut line = PainterPath::new();
    line.move_to(0.0, 0.0);
    line.line_to(100.0, 0.0);
    let mut stroker = PainterPathStroker::new();
    let (stroke, truncated) = stroker.create_stroke_checked(&line);
    assert!(!truncated);
    assert_eq!(stroke.elements(), stroker.create_stroke(&line).elements());
    assert!(!stroker.create_stroke_checked(&PainterPath::new()).1);

    // Dashes below the spacing of `f64`s: Qt's dash loop never ends, ferrokit's draws solid.
    let mut far = PainterPath::new();
    far.move_to(0.0, 0.0);
    far.line_to(1.0e17, 0.0);
    far.line_to(1.0e17, 5000.0);
    stroker.set_dash_pattern(&[1.0, 1.0]);
    let (stroke, truncated) = stroker.create_stroke_checked(&far);
    assert!(!truncated && stroke.elements().len() > 2);
}
