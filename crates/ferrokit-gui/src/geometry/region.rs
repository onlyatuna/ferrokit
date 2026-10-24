// SPDX-License-Identifier: LGPL-3.0-only
//! 2D Region Algebra (`QRegion` equivalent).
//!
//! Represents arbitrary 2D geometric areas composed of non-overlapping rectangles.
//! Essential for window clipping, dirty regions, partial repainting, and expose events.
//! Provides full boolean algebra: Union (`|`, `+`), Intersection (`&`), Subtraction (`-`), and Xor (`^`).

use super::primitives::{Point, Rect};
use std::ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign, BitXor, BitXorAssign, Sub, SubAssign};

// --- Band sweep -------------------------------------------------------------------------------
//
// Every boolean operation is a single scanline sweep. The y-edges of all input rectangles split
// the plane into horizontal bands; inside a band the inputs reduce to sorted x-spans, which are
// combined per operation. Adjacent bands with identical spans are merged, so the result is the
// canonical decomposition of the area (maximal horizontal runs, maximal vertical merging, ordered
// by y then x). Cost is `O(n log n)` for sorting plus the size of the swept bands, instead of the
// pairwise merge loops this replaced.
//
// Rectangles are interpreted through half-open edges that saturate at `i32::MAX`, so degenerate
// extents (`x + width > i32::MAX`) never overflow. A single output rect also cannot be wider than
// `i32::MAX`; a span that long (from `i32::MIN` to `i32::MAX`) is clipped to `i32::MAX` wide.

/// Per-band set operation.
#[derive(Clone, Copy)]
enum Op {
    Union,
    Intersect,
    Subtract,
    Xor,
}

impl Op {
    #[inline]
    fn apply(self, a: bool, b: bool) -> bool {
        match self {
            Op::Union => a || b,
            Op::Intersect => a && b,
            Op::Subtract => a && !b,
            Op::Xor => a != b,
        }
    }
}

/// Half-open edges `[x0, x1) x [y0, y1)` with `x0 < x1` and `y0 < y1`.
#[derive(Clone, Copy)]
struct Edges {
    x0: i32,
    x1: i32,
    y0: i32,
    y1: i32,
}

/// Edges of `r`, or `None` when it covers no pixel. Right/bottom saturate at `i32::MAX`.
fn edges(r: &Rect) -> Option<Edges> {
    if r.width <= 0 || r.height <= 0 {
        return None;
    }
    let x1 = r.x.saturating_add(r.width);
    let y1 = r.y.saturating_add(r.height);
    (x1 > r.x && y1 > r.y).then_some(Edges {
        x0: r.x,
        x1,
        y0: r.y,
        y1,
    })
}

/// Length of `[a, b)` clipped to `i32::MAX`.
#[inline]
fn span(a: i32, b: i32) -> i32 {
    (b as i64 - a as i64).min(i32::MAX as i64) as i32
}

#[inline]
fn rect_from_edges(x0: i32, x1: i32, y0: i32, y1: i32) -> Rect {
    Rect::new(x0, y0, span(x0, x1), span(y0, y1))
}

/// One input operand of the sweep: its rectangles sorted by top edge plus the currently active set.
struct Side {
    edges: Vec<Edges>,
    next: usize,
    active: Vec<Edges>,
}

impl Side {
    fn new(rects: &[Rect]) -> Self {
        let mut edges: Vec<Edges> = rects.iter().filter_map(edges).collect();
        edges.sort_unstable_by_key(|e| e.y0);
        Self {
            edges,
            next: 0,
            active: Vec::new(),
        }
    }

    /// Moves the sweep to the band starting at `y`.
    fn advance(&mut self, y: i32) {
        self.active.retain(|e| e.y1 > y);
        while self.next < self.edges.len() && self.edges[self.next].y0 <= y {
            self.active.push(self.edges[self.next]);
            self.next += 1;
        }
    }

    /// Writes the union of the active rects as sorted, disjoint, non-touching x-spans.
    fn spans(&mut self, out: &mut Vec<(i32, i32)>) {
        out.clear();
        self.active.sort_unstable_by_key(|e| e.x0);
        for e in &self.active {
            match out.last_mut() {
                Some(last) if e.x0 <= last.1 => last.1 = last.1.max(e.x1),
                _ => out.push((e.x0, e.x1)),
            }
        }
    }
}

/// Applies `op` to two span lists (each sorted, disjoint, non-touching), merging touching output spans.
fn combine_spans(a: &[(i32, i32)], b: &[(i32, i32)], op: Op, out: &mut Vec<(i32, i32)>) {
    #[inline]
    fn boundary(list: &[(i32, i32)], k: usize) -> Option<i32> {
        list.get(k / 2).map(|s| if k % 2 == 0 { s.0 } else { s.1 })
    }

    out.clear();
    let (mut ia, mut ib) = (0, 0);
    let (mut in_a, mut in_b) = (false, false);
    let mut start = None;
    loop {
        let (xa, xb) = (boundary(a, ia), boundary(b, ib));
        let x = match (xa, xb) {
            (None, None) => break,
            (Some(x), None) | (None, Some(x)) => x,
            (Some(p), Some(q)) => p.min(q),
        };
        if xa == Some(x) {
            in_a = !in_a;
            ia += 1;
        }
        if xb == Some(x) {
            in_b = !in_b;
            ib += 1;
        }
        match (start, op.apply(in_a, in_b)) {
            (None, true) => start = Some(x),
            (Some(s), false) => {
                out.push((s, x));
                start = None;
            }
            _ => {}
        }
    }
}

struct Band {
    y0: i32,
    y1: i32,
    spans: Vec<(i32, i32)>,
}

/// Canonical rectangles of `op(a, b)`. Operands may overlap internally (their union is used).
fn combine(a: &[Rect], b: &[Rect], op: Op) -> Vec<Rect> {
    let mut sa = Side::new(a);
    let mut sb = Side::new(b);
    if sa.edges.is_empty() && sb.edges.is_empty() {
        return Vec::new();
    }

    let mut ys: Vec<i32> = sa
        .edges
        .iter()
        .chain(&sb.edges)
        .flat_map(|e| [e.y0, e.y1])
        .collect();
    ys.sort_unstable();
    ys.dedup();

    let mut bands: Vec<Band> = Vec::new();
    let (mut spans_a, mut spans_b, mut spans_out) = (Vec::new(), Vec::new(), Vec::new());
    for w in ys.windows(2) {
        let (y0, y1) = (w[0], w[1]);
        sa.advance(y0);
        sb.advance(y0);
        if sa.active.is_empty() && sb.active.is_empty() {
            continue;
        }
        sa.spans(&mut spans_a);
        sb.spans(&mut spans_b);
        combine_spans(&spans_a, &spans_b, op, &mut spans_out);
        if spans_out.is_empty() {
            continue;
        }
        match bands.last_mut() {
            Some(last) if last.y1 == y0 && last.spans == spans_out => last.y1 = y1,
            _ => bands.push(Band {
                y0,
                y1,
                spans: std::mem::take(&mut spans_out),
            }),
        }
    }

    bands
        .iter()
        .flat_map(|band| {
            band.spans
                .iter()
                .map(move |&(x0, x1)| rect_from_edges(x0, x1, band.y0, band.y1))
        })
        .collect()
}

/// 2D Region composed of non-overlapping rectangles (`QRegion`).
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Region {
    rects: Vec<Rect>,
}

/// Regions are equal when they cover the same area, whatever their rect decomposition
/// (`QRegion::operator==`).
impl PartialEq for Region {
    fn eq(&self, other: &Self) -> bool {
        if self.rects == other.rects {
            return true;
        }
        // Rects within a region are disjoint, so equal area plus empty difference means equal sets.
        let area = |r: &Region| r.rects.iter().map(|r| r.width as i128 * r.height as i128).sum::<i128>();
        area(self) == area(other) && self.subtracted(other).is_empty()
    }
}

impl Eq for Region {}

#[cfg(test)]
mod eq_tests {
    use super::*;

    #[test]
    fn equal_area_different_decomposition_is_equal() {
        let a = Region { rects: vec![Rect::new(0, 0, 10, 5), Rect::new(0, 5, 10, 5)] };
        let b = Region { rects: vec![Rect::new(0, 0, 5, 10), Rect::new(5, 0, 5, 10)] };
        assert_ne!(a.rects(), b.rects());
        assert_eq!(a, b);
        assert_eq!(a, Region::from_rect(Rect::new(0, 0, 10, 10)));
    }

    #[test]
    fn different_area_is_not_equal() {
        let a = Region::from_rect(Rect::new(0, 0, 10, 10));
        let b = Region::from_rect(Rect::new(0, 0, 10, 11));
        let c = Region::from_rect(Rect::new(1, 0, 10, 10));
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_ne!(a, Region::new());
    }
}

impl Region {
    /// Constructs an empty region.
    pub const fn new() -> Self {
        Self { rects: Vec::new() }
    }

    /// Constructs a region from a single rectangle.
    pub fn from_rect(r: Rect) -> Self {
        match edges(&r) {
            Some(e) => Self {
                rects: vec![rect_from_edges(e.x0, e.x1, e.y0, e.y1)],
            },
            None => Self::new(),
        }
    }

    /// Constructs a region from coordinate scalars.
    pub fn from_coords(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self::from_rect(Rect::new(x, y, width, height))
    }

    /// Constructs a region by uniting multiple rectangles.
    pub fn from_rects(rects: &[Rect]) -> Self {
        Self {
            rects: combine(rects, &[], Op::Union),
        }
    }

    /// Returns `true` if the region contains no area.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.rects.is_empty()
    }

    /// Returns the number of disjoint rectangles composing this region.
    #[inline]
    pub fn rect_count(&self) -> usize {
        self.rects.len()
    }

    /// Returns the slice of disjoint rectangles.
    #[inline]
    pub fn rects(&self) -> &[Rect] {
        &self.rects
    }

    /// Computes the minimal bounding rectangle covering the entire region (`QRegion::boundingRect`).
    pub fn bounding_rect(&self) -> Rect {
        if self.rects.is_empty() {
            return Rect::default();
        }

        let (mut min_x, mut min_y, mut max_x, mut max_y) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
        for r in &self.rects {
            min_x = min_x.min(r.x);
            min_y = min_y.min(r.y);
            max_x = max_x.max(r.right());
            max_y = max_y.max(r.bottom());
        }

        Rect::new(min_x, min_y, span(min_x, max_x), span(min_y, max_y))
    }

    /// Returns whether the point is contained within the region.
    pub fn contains_point(&self, p: Point) -> bool {
        self.rects.iter().any(|r| r.contains(p))
    }

    /// Returns whether the rectangle is completely enclosed within the region.
    pub fn contains_rect(&self, r: &Rect) -> bool {
        if r.is_empty() {
            return true;
        }
        // r is contained in self if (r - self) is empty
        let reg = Region::from_rect(*r);
        reg.subtracted(self).is_empty()
    }

    /// Returns whether the region intersects with another region.
    pub fn intersects(&self, other: &Region) -> bool {
        !combine(&self.rects, &other.rects, Op::Intersect).is_empty()
    }

    /// Returns whether the region intersects with a rectangle.
    pub fn intersects_rect(&self, r: &Rect) -> bool {
        if r.is_empty() {
            return false;
        }
        self.rects.iter().any(|rect| rect.intersects(r))
    }

    /// Translates all rectangles by (dx, dy).
    pub fn translate(&mut self, dx: i32, dy: i32) {
        // Edges clamp to the i32 range; rects pushed entirely out of range vanish.
        let shift = |lo: i32, hi: i32, d: i32| {
            let clamp = |v: i64| v.clamp(i32::MIN as i64, i32::MAX as i64) as i32;
            (clamp(lo as i64 + d as i64), clamp(hi as i64 + d as i64))
        };
        self.rects.retain_mut(|r| {
            let Some(e) = edges(r) else { return false };
            let (x0, x1) = shift(e.x0, e.x1, dx);
            let (y0, y1) = shift(e.y0, e.y1, dy);
            if x1 <= x0 || y1 <= y0 {
                return false;
            }
            *r = rect_from_edges(x0, x1, y0, y1);
            true
        });
    }

    /// Returns a translated copy of the region.
    pub fn translated(&self, dx: i32, dy: i32) -> Self {
        let mut copy = self.clone();
        copy.translate(dx, dy);
        copy
    }

    // --- Region Algebra (Boolean Operations) ---

    /// Subtracts rectangle `other` from this region (`self - other`).
    pub fn subtracted_rect(&self, other: &Rect) -> Self {
        Self {
            rects: combine(&self.rects, std::slice::from_ref(other), Op::Subtract),
        }
    }

    /// Subtracts `other` region from this region (`self - other`).
    pub fn subtracted(&self, other: &Region) -> Self {
        Self {
            rects: combine(&self.rects, &other.rects, Op::Subtract),
        }
    }

    /// Intersects this region with a rectangle (`self & other`).
    pub fn intersected_rect(&self, other: &Rect) -> Self {
        Self {
            rects: combine(&self.rects, std::slice::from_ref(other), Op::Intersect),
        }
    }

    /// Intersects this region with another region (`self & other`).
    pub fn intersected(&self, other: &Region) -> Self {
        Self {
            rects: combine(&self.rects, &other.rects, Op::Intersect),
        }
    }

    /// Unites this region with a rectangle (`self | other`).
    pub fn united_rect(&self, other: &Rect) -> Self {
        Self {
            rects: combine(&self.rects, std::slice::from_ref(other), Op::Union),
        }
    }

    /// Unites this region with another region (`self | other`).
    pub fn united(&self, other: &Region) -> Self {
        Self {
            rects: combine(&self.rects, &other.rects, Op::Union),
        }
    }

    /// Symmetric difference / XOR of two regions (`self ^ other`).
    pub fn xored(&self, other: &Region) -> Self {
        Self {
            rects: combine(&self.rects, &other.rects, Op::Xor),
        }
    }
}

// --- Operator Overloads ---

impl BitOr for Region {
    type Output = Self;
    #[inline]
    fn bitor(self, rhs: Self) -> Self::Output {
        self.united(&rhs)
    }
}

impl BitOr<&Region> for &Region {
    type Output = Region;
    #[inline]
    fn bitor(self, rhs: &Region) -> Self::Output {
        self.united(rhs)
    }
}

impl BitOr<Rect> for Region {
    type Output = Self;
    #[inline]
    fn bitor(self, rhs: Rect) -> Self::Output {
        self.united_rect(&rhs)
    }
}

impl BitOrAssign for Region {
    #[inline]
    fn bitor_assign(&mut self, rhs: Self) {
        *self = self.united(&rhs);
    }
}

impl BitAnd for Region {
    type Output = Self;
    #[inline]
    fn bitand(self, rhs: Self) -> Self::Output {
        self.intersected(&rhs)
    }
}

impl BitAnd<&Region> for &Region {
    type Output = Region;
    #[inline]
    fn bitand(self, rhs: &Region) -> Self::Output {
        self.intersected(rhs)
    }
}

impl BitAnd<Rect> for Region {
    type Output = Self;
    #[inline]
    fn bitand(self, rhs: Rect) -> Self::Output {
        self.intersected_rect(&rhs)
    }
}

impl BitAndAssign for Region {
    #[inline]
    fn bitand_assign(&mut self, rhs: Self) {
        *self = self.intersected(&rhs);
    }
}

impl Sub for Region {
    type Output = Self;
    #[inline]
    fn sub(self, rhs: Self) -> Self::Output {
        self.subtracted(&rhs)
    }
}

impl Sub<&Region> for &Region {
    type Output = Region;
    #[inline]
    fn sub(self, rhs: &Region) -> Self::Output {
        self.subtracted(rhs)
    }
}

impl Sub<Rect> for Region {
    type Output = Self;
    #[inline]
    fn sub(self, rhs: Rect) -> Self::Output {
        self.subtracted_rect(&rhs)
    }
}

impl SubAssign for Region {
    #[inline]
    fn sub_assign(&mut self, rhs: Self) {
        *self = self.subtracted(&rhs);
    }
}

impl BitXor for Region {
    type Output = Self;
    #[inline]
    fn bitxor(self, rhs: Self) -> Self::Output {
        self.xored(&rhs)
    }
}

impl BitXor<&Region> for &Region {
    type Output = Region;
    #[inline]
    fn bitxor(self, rhs: &Region) -> Self::Output {
        self.xored(rhs)
    }
}

impl BitXorAssign for Region {
    #[inline]
    fn bitxor_assign(&mut self, rhs: Self) {
        *self = self.xored(&rhs);
    }
}
