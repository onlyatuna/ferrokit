// SPDX-License-Identifier: LGPL-3.0-only
//! 2D affine transformation (`QTransform` equivalent).
//!
//! Represents a 3x3 matrix for 2D transformations: translation, scale, rotation, and shear.

use crate::geometry::primitives::{PointF, RectF};
use tiny_skia::Transform as SkiaTransform;

/// Relative tolerance below which a determinant counts as zero.
///
/// Qt's `QTransform::inverted` tests `qFuzzyIsNull(det)`, an absolute `1e-12` on `double`, and
/// `QMatrix4x4::inverted` tests `det == 0`; both wrongly reject tiny-scale matrices or accept
/// ill-scaled ones when mapped onto `f32` storage. Comparing against the summed magnitude of the
/// determinant's own terms is scale-invariant, so only genuine rank loss (cancellation to rounding
/// noise of the `f64` evaluation) is rejected.
pub(crate) const SINGULAR_REL_EPS: f64 = 1e-12;

/// `true` if `det` is zero relative to `magnitude` (the sum of absolute values of its terms) or not finite.
#[inline]
pub(crate) fn is_singular(det: f64, magnitude: f64) -> bool {
    !det.is_finite() || !magnitude.is_finite() || det.abs() <= SINGULAR_REL_EPS * magnitude
}

/// Narrows to `f32`, returning `None` if any element is not finite (the inverse overflows `f32`).
pub(crate) fn finite_f32<const N: usize>(v: &[f64; N]) -> Option<[f32; N]> {
    let mut out = [0.0f32; N];
    for (o, &x) in out.iter_mut().zip(v) {
        *o = x as f32;
        if !o.is_finite() {
            return None;
        }
    }
    Some(out)
}

/// 2D affine transformation matrix.
///
/// | sx  ky  0 |
/// | kx  sy  0 |
/// | tx  ty  1 |
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform2D {
    pub sx: f32, // scale x
    pub ky: f32, // shear y
    pub kx: f32, // shear x
    pub sy: f32, // scale y
    pub tx: f32, // translate x
    pub ty: f32, // translate y
}

impl Default for Transform2D {
    fn default() -> Self {
        Self::identity()
    }
}

impl Transform2D {
    /// Identity matrix.
    pub const fn identity() -> Self {
        Self {
            sx: 1.0,
            ky: 0.0,
            kx: 0.0,
            sy: 1.0,
            tx: 0.0,
            ty: 0.0,
        }
    }

    /// Translation transform.
    pub const fn from_translate(tx: f32, ty: f32) -> Self {
        Self {
            sx: 1.0,
            ky: 0.0,
            kx: 0.0,
            sy: 1.0,
            tx,
            ty,
        }
    }

    /// Scaling transform.
    pub const fn from_scale(sx: f32, sy: f32) -> Self {
        Self {
            sx,
            ky: 0.0,
            kx: 0.0,
            sy,
            tx: 0.0,
            ty: 0.0,
        }
    }

    /// Rotation transform (degrees clockwise).
    pub fn from_rotate(degrees: f32) -> Self {
        let rad = degrees.to_radians();
        let (sin, cos) = rad.sin_cos();
        Self {
            sx: cos,
            ky: sin,
            kx: -sin,
            sy: cos,
            tx: 0.0,
            ty: 0.0,
        }
    }

    /// Rotation around a pivot point `(cx, cy)` (degrees clockwise).
    pub fn from_rotate_at(degrees: f32, cx: f32, cy: f32) -> Self {
        Self::from_translate(cx, cy)
            .pre_rotate(degrees)
            .pre_translate(-cx, -cy)
    }

    /// Shear / Skew transform (horizontal `kx` and vertical `ky` factors).
    pub const fn from_shear(kx: f32, ky: f32) -> Self {
        Self {
            sx: 1.0,
            ky,
            kx,
            sy: 1.0,
            tx: 0.0,
            ty: 0.0,
        }
    }

    /// Custom 6-parameter affine matrix.
    pub const fn from_row_matrix(sx: f32, ky: f32, kx: f32, sy: f32, tx: f32, ty: f32) -> Self {
        Self {
            sx,
            ky,
            kx,
            sy,
            tx,
            ty,
        }
    }

    /// Concatenate (multiply) with another transform: `self * other`.
    pub fn post_concat(&self, other: &Self) -> Self {
        Self {
            sx: other.sx * self.sx + other.ky * self.kx,
            ky: other.sx * self.ky + other.ky * self.sy,
            kx: other.kx * self.sx + other.sy * self.kx,
            sy: other.kx * self.ky + other.sy * self.sy,
            tx: other.tx * self.sx + other.ty * self.kx + self.tx,
            ty: other.tx * self.ky + other.ty * self.sy + self.ty,
        }
    }

    /// Pre-multiply: `other * self`.
    pub fn pre_concat(&self, other: &Self) -> Self {
        other.post_concat(self)
    }

    /// Pre-translate.
    pub fn pre_translate(&self, dx: f32, dy: f32) -> Self {
        self.post_concat(&Self::from_translate(dx, dy))
    }

    /// Post-translate.
    pub fn post_translate(&self, dx: f32, dy: f32) -> Self {
        Self::from_translate(dx, dy).post_concat(self)
    }

    /// Pre-scale.
    pub fn pre_scale(&self, sx: f32, sy: f32) -> Self {
        self.post_concat(&Self::from_scale(sx, sy))
    }

    /// Pre-rotate (degrees clockwise).
    pub fn pre_rotate(&self, degrees: f32) -> Self {
        self.post_concat(&Self::from_rotate(degrees))
    }

    /// Pre-shear.
    pub fn pre_shear(&self, kx: f32, ky: f32) -> Self {
        self.post_concat(&Self::from_shear(kx, ky))
    }

    /// Maps a 2D point using this transform.
    pub fn map_point(&self, p: PointF) -> PointF {
        PointF::new(
            self.sx * p.x + self.kx * p.y + self.tx,
            self.ky * p.x + self.sy * p.y + self.ty,
        )
    }

    /// Maps a 2D rectangle (computes axis-aligned bounding box of transformed corners).
    pub fn map_rect(&self, r: RectF) -> RectF {
        let p1 = self.map_point(PointF::new(r.x, r.y));
        let p2 = self.map_point(PointF::new(r.right(), r.y));
        let p3 = self.map_point(PointF::new(r.right(), r.bottom()));
        let p4 = self.map_point(PointF::new(r.x, r.bottom()));

        let min_x = p1.x.min(p2.x).min(p3.x).min(p4.x);
        let max_x = p1.x.max(p2.x).max(p3.x).max(p4.x);
        let min_y = p1.y.min(p2.y).min(p3.y).min(p4.y);
        let max_y = p1.y.max(p2.y).max(p3.y).max(p4.y);

        RectF::new(min_x, min_y, max_x - min_x, max_y - min_y)
    }

    /// Inverts the affine transform if invertible (`QTransform::inverted`).
    ///
    /// Singularity is judged relative to the magnitude of the determinant's terms, so uniformly
    /// tiny (or huge) but well-conditioned transforms invert. The determinant and inverse are
    /// evaluated in `f64`, matching Qt's `qreal`.
    pub fn inverted(&self) -> Option<Self> {
        let (sx, ky, kx, sy, tx, ty) = (
            self.sx as f64,
            self.ky as f64,
            self.kx as f64,
            self.sy as f64,
            self.tx as f64,
            self.ty as f64,
        );
        let det = sx * sy - ky * kx;
        if is_singular(det, (sx * sy).abs() + (ky * kx).abs()) {
            return None;
        }
        let inv_det = 1.0 / det;
        finite_f32(&[
            sy * inv_det,
            -ky * inv_det,
            -kx * inv_det,
            sx * inv_det,
            (kx * ty - sy * tx) * inv_det,
            (ky * tx - sx * ty) * inv_det,
        ])
        .map(|v| Self {
            sx: v[0],
            ky: v[1],
            kx: v[2],
            sy: v[3],
            tx: v[4],
            ty: v[5],
        })
    }

    /// Converts to `tiny_skia::Transform`.
    pub fn to_skia(&self) -> SkiaTransform {
        SkiaTransform::from_row(self.sx, self.ky, self.kx, self.sy, self.tx, self.ty)
    }

    /// Creates from `tiny_skia::Transform`.
    pub fn from_skia(t: SkiaTransform) -> Self {
        Self {
            sx: t.sx,
            ky: t.ky,
            kx: t.kx,
            sy: t.sy,
            tx: t.tx,
            ty: t.ty,
        }
    }
}

/// Full 3x3 projective transformation matrix (`QTransform` equivalent).
///
/// Elements are named `m<row><column>`, from `m11` to `m33`.
///
/// Supports affine operations (scaling, translation, rotation, shear)
/// as well as true perspective/projective transformations (`m13`, `m23`, `m33`).
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Transform {
    pub m11: f32,
    pub m12: f32,
    pub m13: f32,
    pub m21: f32,
    pub m22: f32,
    pub m23: f32,
    pub m31: f32,
    pub m32: f32,
    pub m33: f32,
}

impl Default for Transform {
    #[inline]
    fn default() -> Self {
        Self::identity()
    }
}

impl Transform {
    /// Identity transformation.
    #[inline]
    pub const fn identity() -> Self {
        Self {
            m11: 1.0,
            m12: 0.0,
            m13: 0.0,
            m21: 0.0,
            m22: 1.0,
            m23: 0.0,
            m31: 0.0,
            m32: 0.0,
            m33: 1.0,
        }
    }

    /// Constructs from all 9 elements.
    #[allow(clippy::too_many_arguments)]
    #[inline]
    pub const fn from_elements(
        m11: f32,
        m12: f32,
        m13: f32,
        m21: f32,
        m22: f32,
        m23: f32,
        m31: f32,
        m32: f32,
        m33: f32,
    ) -> Self {
        Self {
            m11,
            m12,
            m13,
            m21,
            m22,
            m23,
            m31,
            m32,
            m33,
        }
    }

    /// Constructs from 2D affine `Transform2D`.
    #[inline]
    pub const fn from_affine(t: Transform2D) -> Self {
        Self {
            m11: t.sx,
            m12: t.ky,
            m13: 0.0,
            m21: t.kx,
            m22: t.sy,
            m23: 0.0,
            m31: t.tx,
            m32: t.ty,
            m33: 1.0,
        }
    }

    /// Returns `true` if this transform is identity.
    pub fn is_identity(&self) -> bool {
        *self == Self::identity()
    }

    /// Returns `true` if this transform has no perspective component (`m13 == 0, m23 == 0, m33 == 1`).
    pub fn is_affine(&self) -> bool {
        self.m13.abs() < 1e-7 && self.m23.abs() < 1e-7 && (self.m33 - 1.0).abs() < 1e-7
    }

    /// Converts to `Transform2D` if affine.
    pub fn to_affine(&self) -> Option<Transform2D> {
        if self.is_affine() {
            Some(Transform2D {
                sx: self.m11,
                ky: self.m12,
                kx: self.m21,
                sy: self.m22,
                tx: self.m31,
                ty: self.m32,
            })
        } else {
            None
        }
    }

    /// Translates by (dx, dy).
    pub fn translate(&mut self, dx: f32, dy: f32) {
        let t = Self {
            m11: 1.0,
            m12: 0.0,
            m13: 0.0,
            m21: 0.0,
            m22: 1.0,
            m23: 0.0,
            m31: dx,
            m32: dy,
            m33: 1.0,
        };
        // `QTransform::translate` computes `T * M`: the new operation applies first.
        *self = t * *self;
    }

    /// Scales by (sx, sy).
    pub fn scale(&mut self, sx: f32, sy: f32) {
        let s = Self {
            m11: sx,
            m12: 0.0,
            m13: 0.0,
            m21: 0.0,
            m22: sy,
            m23: 0.0,
            m31: 0.0,
            m32: 0.0,
            m33: 1.0,
        };
        *self = s * *self;
    }

    /// Rotates by angle in degrees.
    pub fn rotate(&mut self, angle_degrees: f32) {
        let rad = angle_degrees * std::f32::consts::PI / 180.0;
        let c = rad.cos();
        let s = rad.sin();
        let r = Self {
            m11: c,
            m12: s,
            m13: 0.0,
            m21: -s,
            m22: c,
            m23: 0.0,
            m31: 0.0,
            m32: 0.0,
            m33: 1.0,
        };
        *self = r * *self;
    }

    /// Maps a 2D point with projective division by `w = m13 * x + m23 * y + m33`.
    pub fn map_point(&self, p: PointF) -> PointF {
        let w = self.m13 * p.x + self.m23 * p.y + self.m33;
        if w.abs() > 1e-7 {
            let inv_w = 1.0 / w;
            PointF::new(
                (self.m11 * p.x + self.m21 * p.y + self.m31) * inv_w,
                (self.m12 * p.x + self.m22 * p.y + self.m32) * inv_w,
            )
        } else {
            PointF::new(
                self.m11 * p.x + self.m21 * p.y + self.m31,
                self.m12 * p.x + self.m22 * p.y + self.m32,
            )
        }
    }

    /// Computes determinant of the 3x3 matrix.
    pub fn determinant(&self) -> f32 {
        self.m11 * (self.m22 * self.m33 - self.m23 * self.m32)
            - self.m12 * (self.m21 * self.m33 - self.m23 * self.m31)
            + self.m13 * (self.m21 * self.m32 - self.m22 * self.m31)
    }

    /// Inverts the 3x3 matrix (`QTransform::inverted`).
    ///
    /// Singularity is judged relative to the magnitude of the determinant's terms (see
    /// [`Transform2D::inverted`]).
    pub fn inverted(&self) -> Option<Self> {
        let (m11, m12, m13) = (self.m11 as f64, self.m12 as f64, self.m13 as f64);
        let (m21, m22, m23) = (self.m21 as f64, self.m22 as f64, self.m23 as f64);
        let (m31, m32, m33) = (self.m31 as f64, self.m32 as f64, self.m33 as f64);

        let c1 = m22 * m33 - m23 * m32;
        let c2 = m21 * m33 - m23 * m31;
        let c3 = m21 * m32 - m22 * m31;
        let det = m11 * c1 - m12 * c2 + m13 * c3;
        let magnitude = m11.abs() * ((m22 * m33).abs() + (m23 * m32).abs())
            + m12.abs() * ((m21 * m33).abs() + (m23 * m31).abs())
            + m13.abs() * ((m21 * m32).abs() + (m22 * m31).abs());
        if is_singular(det, magnitude) {
            return None;
        }
        let inv = 1.0 / det;

        finite_f32(&[
            c1 * inv,
            (m13 * m32 - m12 * m33) * inv,
            (m12 * m23 - m13 * m22) * inv,
            (m23 * m31 - m21 * m33) * inv,
            (m11 * m33 - m13 * m31) * inv,
            (m13 * m21 - m11 * m23) * inv,
            c3 * inv,
            (m12 * m31 - m11 * m32) * inv,
            (m11 * m22 - m12 * m21) * inv,
        ])
        .map(|v| Self {
            m11: v[0],
            m12: v[1],
            m13: v[2],
            m21: v[3],
            m22: v[4],
            m23: v[5],
            m31: v[6],
            m32: v[7],
            m33: v[8],
        })
    }

    /// Projective mapping from unit square [0,1]x[0,1] to arbitrary quadrilateral (`QTransform::squareToQuad`).
    pub fn square_to_quad(q: [PointF; 4]) -> Option<Self> {
        let dx3 = q[0].x - q[1].x + q[2].x - q[3].x;
        let dy3 = q[0].y - q[1].y + q[2].y - q[3].y;

        if dx3.abs() < 1e-7 && dy3.abs() < 1e-7 {
            // Affine mapping
            Some(Self {
                m11: q[1].x - q[0].x,
                m12: q[1].y - q[0].y,
                m13: 0.0,
                m21: q[2].x - q[1].x,
                m22: q[2].y - q[1].y,
                m23: 0.0,
                m31: q[0].x,
                m32: q[0].y,
                m33: 1.0,
            })
        } else {
            let dx1 = q[1].x - q[2].x;
            let dy1 = q[1].y - q[2].y;
            let dx2 = q[3].x - q[2].x;
            let dy2 = q[3].y - q[2].y;

            let det = dx1 as f64 * dy2 as f64 - dx2 as f64 * dy1 as f64;
            let magnitude = (dx1 as f64 * dy2 as f64).abs() + (dx2 as f64 * dy1 as f64).abs();
            if is_singular(det, magnitude) {
                return None;
            }
            let det = det as f32;
            let g = (dx3 * dy2 - dx2 * dy3) / det;
            let h = (dx1 * dy3 - dx3 * dy1) / det;

            Some(Self {
                m11: q[1].x - q[0].x + g * q[1].x,
                m12: q[1].y - q[0].y + g * q[1].y,
                m13: g,
                m21: q[3].x - q[0].x + h * q[3].x,
                m22: q[3].y - q[0].y + h * q[3].y,
                m23: h,
                m31: q[0].x,
                m32: q[0].y,
                m33: 1.0,
            })
        }
    }
}

impl std::ops::Mul for Transform {
    type Output = Self;
    fn mul(self, rhs: Self) -> Self::Output {
        Self {
            m11: self.m11 * rhs.m11 + self.m12 * rhs.m21 + self.m13 * rhs.m31,
            m12: self.m11 * rhs.m12 + self.m12 * rhs.m22 + self.m13 * rhs.m32,
            m13: self.m11 * rhs.m13 + self.m12 * rhs.m23 + self.m13 * rhs.m33,

            m21: self.m21 * rhs.m11 + self.m22 * rhs.m21 + self.m23 * rhs.m31,
            m22: self.m21 * rhs.m12 + self.m22 * rhs.m22 + self.m23 * rhs.m32,
            m23: self.m21 * rhs.m13 + self.m22 * rhs.m23 + self.m23 * rhs.m33,

            m31: self.m31 * rhs.m11 + self.m32 * rhs.m21 + self.m33 * rhs.m31,
            m32: self.m31 * rhs.m12 + self.m32 * rhs.m22 + self.m33 * rhs.m32,
            m33: self.m31 * rhs.m13 + self.m32 * rhs.m23 + self.m33 * rhs.m33,
        }
    }
}

/// Canonical Qt alias.
pub type QTransform = Transform;

#[cfg(test)]
mod compose_order_tests {
    use super::*;

    fn close(a: PointF, x: f32, y: f32) {
        assert!((a.x - x).abs() < 1e-4 && (a.y - y).abs() < 1e-4, "{a:?} != ({x}, {y})");
    }

    // Hand-computed from QTransform semantics: each call pre-multiplies, so the
    // most recent operation is applied to the point first.
    #[test]
    fn scale_then_translate_applies_translate_first() {
        let mut t = Transform::identity();
        t.scale(2.0, 2.0);
        t.translate(10.0, 0.0);
        close(t.map_point(PointF::new(0.0, 0.0)), 20.0, 0.0);
    }

    #[test]
    fn translate_then_scale_applies_scale_first() {
        let mut t = Transform::identity();
        t.translate(10.0, 0.0);
        t.scale(2.0, 2.0);
        close(t.map_point(PointF::new(1.0, 1.0)), 12.0, 2.0);
    }

    #[test]
    fn translate_then_rotate_applies_rotate_first() {
        let mut t = Transform::identity();
        t.translate(10.0, 0.0);
        t.rotate(90.0);
        close(t.map_point(PointF::new(1.0, 0.0)), 10.0, 1.0);
    }

    #[test]
    fn rotate_then_translate_applies_translate_first() {
        let mut t = Transform::identity();
        t.rotate(90.0);
        t.translate(10.0, 0.0);
        close(t.map_point(PointF::new(0.0, 0.0)), 0.0, 10.0);
    }
}
