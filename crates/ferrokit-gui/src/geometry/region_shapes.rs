// SPDX-License-Identifier: LGPL-3.0-only
//! Curved and polygonal [`Region`] constructors using cubic-Bezier flattening and integer scanline
//! polygon rasterization.

use super::polygon::{FillRule, Polygon};
use super::primitives::{Point, Rect};
use super::region::Region;
use std::collections::BTreeMap;

/// Shape type for [`Region::from_rect_type`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionType {
    /// The region is the rectangle itself.
    Rectangle,
    /// The region is the ellipse inscribed in the rectangle.
    Ellipse,
}

impl Region {
    /// Builds a region from `r` as a rectangle or inscribed ellipse.
    pub fn from_rect_type(r: Rect, ty: RegionType) -> Self {
        if r.width <= 0 || r.height <= 0 {
            return Self::new();
        }
        match ty {
            RegionType::Rectangle => Self::from_rect(r),
            RegionType::Ellipse => {
                let mut poly = Vec::new();
                flatten_ellipse(
                    f64::from(r.x),
                    f64::from(r.y),
                    f64::from(r.width),
                    f64::from(r.height),
                    &mut poly,
                );
                let rounded = round_polygon(&poly);
                scan_convert_polygon(&rounded, FillRule::OddEven).unwrap_or_default()
            }
        }
    }

    /// Scan-converts `polygon` using `fill_rule`.
    ///
    /// Fewer than three points, or a polygon taller than 100,000 rows, yields an empty region.
    pub fn from_polygon(polygon: &Polygon, fill_rule: FillRule) -> Self {
        if polygon.len() > 2 {
            scan_convert_polygon(polygon.points(), fill_rule).unwrap_or_default()
        } else {
            Self::new()
        }
    }

    /// Builds a region covering the rounded rectangle `(rect, x_radius, y_radius)`.
    pub fn from_rounded_rect(rect: Rect, x_radius: f64, y_radius: f64) -> Self {
        if rect.width <= 0 || rect.height <= 0 {
            return Self::new();
        }
        let mut poly = Vec::new();
        flatten_rounded_rect(
            f64::from(rect.x),
            f64::from(rect.y),
            f64::from(rect.width),
            f64::from(rect.height),
            x_radius,
            y_radius,
            &mut poly,
        );
        let rounded = round_polygon(&poly);
        if rounded.len() > 2 {
            scan_convert_polygon(&rounded, FillRule::OddEven).unwrap_or_default()
        } else {
            Self::new()
        }
    }
}

/// Round half away from zero, matching integer coordinate rounding.
fn round_half_away(d: f64) -> i32 {
    (d + 0.5f64.copysign(d)) as i32
}

fn round_polygon(pts: &[(f64, f64)]) -> Vec<Point> {
    pts.iter()
        .map(|&(x, y)| Point::new(round_half_away(x), round_half_away(y)))
        .collect()
}

/// Cubic Bezier control-point factor for a 90-degree circular arc (`4 * (sqrt(2) - 1) / 3`).
const KAPPA: f64 = 0.552_284_749_8;

type Pt = (f64, f64);

#[derive(Clone, Copy)]
struct Cubic {
    p0: Pt,
    p1: Pt,
    p2: Pt,
    p3: Pt,
}

impl Cubic {
    fn split(self) -> (Self, Self) {
        let mid = |a: Pt, b: Pt| ((a.0 + b.0) * 0.5, (a.1 + b.1) * 0.5);
        let m01 = mid(self.p0, self.p1);
        let m12 = mid(self.p1, self.p2);
        let m23 = mid(self.p2, self.p3);
        let m012 = mid(m01, m12);
        let m123 = mid(m12, m23);
        let m0123 = mid(m012, m123);
        (
            Self {
                p0: self.p0,
                p1: m01,
                p2: m012,
                p3: m0123,
            },
            Self {
                p0: m0123,
                p1: m123,
                p2: m23,
                p3: self.p3,
            },
        )
    }

    /// Iterative de Casteljau subdivision with flatness threshold `0.5` and max depth `9`.
    fn flatten(self, out: &mut Vec<Pt>) {
        const THRESHOLD: f64 = 0.5;
        let mut stack = [self; 10];
        let mut depth = [0u8; 10];
        depth[0] = 9;
        let mut top: isize = 0;
        while top >= 0 {
            let idx = top as usize;
            let c = stack[idx];
            let dx = c.p3.0 - c.p0.0;
            let dy = c.p3.1 - c.p0.1;
            let mut span = dx.abs() + dy.abs();
            let err = if span > 1.0 {
                (dx * (c.p0.1 - c.p1.1) - dy * (c.p0.0 - c.p1.0)).abs()
                    + (dx * (c.p0.1 - c.p2.1) - dy * (c.p0.0 - c.p2.0)).abs()
            } else {
                span = 1.0;
                (c.p0.0 - c.p1.0).abs()
                    + (c.p0.1 - c.p1.1).abs()
                    + (c.p0.0 - c.p2.0).abs()
                    + (c.p0.1 - c.p2.1).abs()
            };
            if err < THRESHOLD * span || depth[idx] == 0 {
                out.push(c.p3);
                top -= 1;
            } else {
                let (left, right) = c.split();
                let next_depth = depth[idx] - 1;
                stack[idx] = right;
                stack[idx + 1] = left;
                depth[idx] = next_depth;
                depth[idx + 1] = next_depth;
                top += 1;
            }
        }
    }
}

fn ellipse_quadrant_points(x: f64, y: f64, w: f64, h: f64) -> [Pt; 13] {
    let rx = w * 0.5;
    let ry = h * 0.5;
    let kx = rx * KAPPA;
    let ky = ry * KAPPA;
    let cx = x + rx;
    let cy = y + ry;
    [
        (x + w, cy),
        (x + w, cy + ky),
        (cx + kx, y + h),
        (cx, y + h),
        (cx - kx, y + h),
        (x, cy + ky),
        (x, cy),
        (x, cy - ky),
        (cx - kx, y),
        (cx, y),
        (cx + kx, y),
        (x + w, cy - ky),
        (x + w, cy),
    ]
}

fn flatten_ellipse(x: f64, y: f64, w: f64, h: f64, out: &mut Vec<Pt>) {
    let pts = ellipse_quadrant_points(x, y, w, h);
    out.push(pts[0]);
    for q in 0..4 {
        Cubic {
            p0: pts[3 * q],
            p1: pts[3 * q + 1],
            p2: pts[3 * q + 2],
            p3: pts[3 * q + 3],
        }
        .flatten(out);
    }
}

fn flatten_rounded_rect(
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    x_radius: f64,
    y_radius: f64,
    out: &mut Vec<Pt>,
) {
    let rx = x_radius.min(w * 0.5);
    let ry = y_radius.min(h * 0.5);
    if rx <= 0.0 || ry <= 0.0 {
        out.extend_from_slice(&[(x, y), (x + w, y), (x + w, y + h), (x, y + h), (x, y)]);
        return;
    }
    let rw = rx * 2.0;
    let rh = ry * 2.0;
    let tl = ellipse_quadrant_points(x, y, rw, rh);
    let tr = ellipse_quadrant_points(x + w - rw, y, rw, rh);
    let br = ellipse_quadrant_points(x + w - rw, y + h - rh, rw, rh);
    let bl = ellipse_quadrant_points(x, y + h - rh, rw, rh);

    // Start at left-top arc start (x, y + ry) and walk clockwise around the 4 corner arcs.
    out.push(tl[6]);
    Cubic {
        p0: tl[6],
        p1: tl[7],
        p2: tl[8],
        p3: tl[9],
    }
    .flatten(out);
    if tr[9] != tl[9] {
        out.push(tr[9]);
    }
    Cubic {
        p0: tr[9],
        p1: tr[10],
        p2: tr[11],
        p3: tr[12],
    }
    .flatten(out);
    if br[0] != tr[12] {
        out.push(br[0]);
    }
    Cubic {
        p0: br[0],
        p1: br[1],
        p2: br[2],
        p3: br[3],
    }
    .flatten(out);
    if bl[3] != br[3] {
        out.push(bl[3]);
    }
    Cubic {
        p0: bl[3],
        p1: bl[4],
        p2: bl[5],
        p3: bl[6],
    }
    .flatten(out);
    if out.first() != out.last()
        && let Some(&first) = out.first()
    {
        out.push(first);
    }
}

const MAX_ROWS: i64 = 100_000;

#[derive(Clone, Copy)]
struct ScanEdge {
    y_top: i64,
    y_bottom: i64,
    x_top: i64,
    dx: i64,
    dy: i64,
    down: bool,
}

impl ScanEdge {
    fn x_at(&self, y: i64) -> i64 {
        let k = i128::from(y - self.y_top);
        let dx = i128::from(self.dx);
        let dy = i128::from(self.dy);
        if dx >= 0 {
            self.x_top + ((k * dx + dy - 1) / dy) as i64
        } else {
            self.x_top - ((k * (-dx)) / dy) as i64
        }
    }
}

fn try_rectangle_fast_path(pts: &[Point]) -> Option<Region> {
    let count = pts.len();
    let p = |i: usize| (i64::from(pts[i].x), i64::from(pts[i].y));
    if !((count == 4 || (count == 5 && p(4) == p(0)))
        && ((p(0).1 == p(1).1 && p(1).0 == p(2).0 && p(2).1 == p(3).1 && p(3).0 == p(0).0)
            || (p(0).0 == p(1).0 && p(1).1 == p(2).1 && p(2).0 == p(3).0 && p(3).1 == p(0).1)))
    {
        return None;
    }
    let x = p(0).0.min(p(2).0);
    let y = p(0).1.min(p(2).1);
    let w = p(0).0.max(p(2).0) - x;
    let h = p(0).1.max(p(2).1) - y;
    if w >= 1 && h >= 1 && w <= i64::from(i32::MAX) && h <= i64::from(i32::MAX) {
        Some(Region::from_rect(Rect::new(
            x as i32, y as i32, w as i32, h as i32,
        )))
    } else {
        Some(Region::new())
    }
}

fn scan_convert_polygon(pts: &[Point], rule: FillRule) -> Option<Region> {
    if pts.len() < 2 {
        return Some(Region::new());
    }
    if let Some(rect_region) = try_rectangle_fast_path(pts) {
        return Some(rect_region);
    }

    let mut buckets: BTreeMap<i64, Vec<ScanEdge>> = BTreeMap::new();
    let (mut ymin, mut ymax) = (i64::from(i32::MAX), i64::from(i32::MIN));
    let mut prev = (
        i64::from(pts[pts.len() - 1].x),
        i64::from(pts[pts.len() - 1].y),
    );

    for pt in pts {
        let curr = (i64::from(pt.x), i64::from(pt.y));
        if prev.1 != curr.1 {
            let (top, bottom, down) = if prev.1 < curr.1 {
                (prev, curr, true)
            } else {
                (curr, prev, false)
            };
            let edge = ScanEdge {
                y_top: top.1,
                y_bottom: bottom.1,
                x_top: top.0,
                dx: bottom.0 - top.0,
                dy: bottom.1 - top.1,
                down,
            };
            buckets.entry(top.1).or_default().push(edge);
            ymin = ymin.min(top.1);
            ymax = ymax.max(bottom.1);
        }
        prev = curr;
    }

    if ymin >= ymax {
        return Some(Region::new());
    }
    if ymax - ymin > MAX_ROWS {
        return None;
    }

    let mut active: Vec<ScanEdge> = Vec::new();
    let mut rects: Vec<Rect> = Vec::new();
    let mut spans: Vec<(i32, i32)> = Vec::new();

    for y in ymin..ymax {
        active.retain(|e| e.y_bottom > y);
        if let Some(new_edges) = buckets.remove(&y) {
            active.extend(new_edges);
        }
        if active.is_empty() {
            continue;
        }
        active.sort_by_key(|e| e.x_at(y));
        spans.clear();

        match rule {
            FillRule::OddEven => {
                for pair in active.as_chunks::<2>().0 {
                    let x0 = pair[0].x_at(y) as i32;
                    let x1 = pair[1].x_at(y) as i32;
                    if x1 > x0 {
                        match spans.last_mut() {
                            Some(last) if last.1 == x0 => last.1 = x1,
                            _ => spans.push((x0, x1)),
                        }
                    }
                }
            }
            FillRule::Winding => {
                let mut winding = 0i32;
                let mut span_start: Option<i32> = None;
                for edge in &active {
                    let x = edge.x_at(y) as i32;
                    let prev_w = winding;
                    winding += if edge.down { 1 } else { -1 };
                    if prev_w == 0 && winding != 0 {
                        span_start = Some(x);
                    } else if prev_w != 0
                        && winding == 0
                        && let Some(x0) = span_start.take()
                        && x > x0
                    {
                        match spans.last_mut() {
                            Some(last) if last.1 == x0 => last.1 = x,
                            _ => spans.push((x0, x)),
                        }
                    }
                }
            }
        }

        rects.extend(
            spans
                .iter()
                .map(|&(x0, x1)| Rect::new(x0, y as i32, x1 - x0, 1)),
        );
    }

    Some(Region::from_rects(&rects))
}
