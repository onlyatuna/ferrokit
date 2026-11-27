// SPDX-License-Identifier: LGPL-3.0-only
//! Double-precision geometry helpers for elliptic arcs and path coordinate validation.

use std::ops::{Add, Mul, Neg, Sub};

/// Cubic Bezier control-point factor for a quarter circle (`4 * (sqrt(2) - 1) / 3`).
#[allow(dead_code)]
pub(crate) const KAPPA: f64 = 0.552_284_749_8;

/// Relative tolerance comparison (`1e-12`) for non-zero `f64` values.
pub(crate) fn fuzzy_compare(a: f64, b: f64) -> bool {
    (a - b).abs() * 1_000_000_000_000.0 <= a.abs().min(b.abs())
}

/// Near-zero check (`|d| <= 1e-12`).
pub(crate) fn fuzzy_is_null(d: f64) -> bool {
    d.abs() <= 0.000_000_000_001
}

/// Zero-aware `1e-12` tolerance comparison.
fn fuzzy_compare_any(a: f64, b: f64) -> bool {
    if a == 0.0 || b == 0.0 {
        fuzzy_is_null(a - b)
    } else {
        fuzzy_compare(a, b)
    }
}

/// Double-precision 2D point.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct P {
    pub x: f64,
    pub y: f64,
}

impl P {
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    #[allow(dead_code)]
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

/// Zero-aware `1e-12` point equality.
pub(crate) fn point_eq(a: P, b: P) -> bool {
    fuzzy_compare_any(a.x, b.x) && fuzzy_compare_any(a.y, b.y)
}

/// Double-precision rectangle.
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

    pub fn is_null(&self) -> bool {
        self.w == 0.0 && self.h == 0.0
    }

    pub fn center(&self) -> P {
        P::new(self.x + self.w * 0.5, self.y + self.h * 0.5)
    }
}

/// Whether `c` is finite and within path coordinate bounds.
pub(crate) fn is_valid_coord(c: f64) -> bool {
    c.is_finite() && c.abs() < 1e128
}

/// Largest start angle in degrees accepted for arc construction.
pub(crate) const MAX_ARC_ANGLE: f64 = 1e9;

/// Whether `a` is finite and within [`MAX_ARC_ANGLE`].
pub(crate) fn is_valid_angle(a: f64) -> bool {
    a.abs() <= MAX_ARC_ANGLE
}

/// Whether all four rectangle coordinates are valid.
pub(crate) fn rect_has_valid_coords(r: &Rc) -> bool {
    is_valid_coord(r.x) && is_valid_coord(r.y) && is_valid_coord(r.w) && is_valid_coord(r.h)
}

/// Computes `(cos(deg), sin(deg))` with exact values at 90-degree multiples.
fn cos_sin_deg(deg: f64) -> (f64, f64) {
    let rem = deg.rem_euclid(360.0);
    if rem == 0.0 {
        (1.0, 0.0)
    } else if rem == 90.0 {
        (0.0, 1.0)
    } else if rem == 180.0 {
        (-1.0, 0.0)
    } else if rem == 270.0 {
        (0.0, -1.0)
    } else {
        let rad = deg.to_radians();
        (rad.cos(), rad.sin())
    }
}

fn ellipse_point(r: &Rc, deg: f64) -> P {
    let c = r.center();
    let (rx, ry) = (r.w * 0.5, r.h * 0.5);
    let (cos_a, sin_a) = cos_sin_deg(deg);
    P::new(c.x + rx * cos_a, c.y - ry * sin_a)
}

/// Returns the start and end points on the ellipse inscribed in `r` at `angle` and
/// `angle + length` degrees (counter-clockwise from +x in y-down screen coordinates).
pub(crate) fn find_ellipse_coords(r: &Rc, angle: f64, length: f64) -> (P, P) {
    (ellipse_point(r, angle), ellipse_point(r, angle + length))
}

/// Approximates an elliptic arc inscribed in `rect` from `start_angle` spanning `sweep_length`
/// degrees (CCW from +x) as `(start_point, [c1, c2, end, ...])` cubic Bezier segments split at
/// 90-degree quadrant boundaries.
pub(crate) fn curves_for_arc(rect: &Rc, start_angle: f64, sweep_length: f64) -> (P, Vec<P>) {
    let sweep = sweep_length.clamp(-360.0, 360.0);
    let start = ellipse_point(rect, start_angle);
    if sweep == 0.0 {
        return (start, Vec::new());
    }
    let c = rect.center();
    let (rx, ry) = (rect.w * 0.5, rect.h * 0.5);
    let end = start_angle + sweep;
    let mut cur = start_angle;
    let mut out = Vec::with_capacity(15);

    for _ in 0..6 {
        if (sweep > 0.0 && cur >= end - 1e-12) || (sweep < 0.0 && cur <= end + 1e-12) {
            break;
        }
        let next = if sweep > 0.0 {
            (((cur / 90.0).floor() + 1.0) * 90.0).min(end)
        } else {
            (((cur / 90.0).ceil() - 1.0) * 90.0).max(end)
        };
        let delta = (next - cur).to_radians();
        let alpha = (4.0 / 3.0) * (delta * 0.25).tan();
        let (cos0, sin0) = cos_sin_deg(cur);
        let (cos1, sin1) = cos_sin_deg(next);
        let p0 = P::new(c.x + rx * cos0, c.y - ry * sin0);
        let p3 = P::new(c.x + rx * cos1, c.y - ry * sin1);
        let t0 = P::new(-rx * sin0, -ry * cos0);
        let t1 = P::new(-rx * sin1, -ry * cos1);
        out.push(p0 + t0 * alpha);
        out.push(p3 - t1 * alpha);
        out.push(p3);
        cur = next;
    }
    (start, out)
}
