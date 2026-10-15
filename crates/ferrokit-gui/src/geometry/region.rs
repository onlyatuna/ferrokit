// SPDX-License-Identifier: LGPL-3.0-only
//! 2D Region Algebra (`QRegion` equivalent).
//!
//! Represents arbitrary 2D geometric areas composed of non-overlapping rectangles.
//! Essential for window clipping, dirty regions, partial repainting, and expose events.
//! Provides full boolean algebra: Union (`|`, `+`), Intersection (`&`), Subtraction (`-`), and Xor (`^`).

use super::primitives::{Point, Rect};
use std::ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign, BitXor, BitXorAssign, Sub, SubAssign};

/// Subtracts rectangle `b` from rectangle `a`, writing 0 to 4 disjoint remainder rectangles into `out`.
fn subtract_rect(a: &Rect, b: &Rect, out: &mut Vec<Rect>) {
    if a.is_empty() {
        return;
    }
    if b.is_empty() || !a.intersects(b) {
        out.push(*a);
        return;
    }

    let a_left = a.x;
    let a_top = a.y;
    let a_right = a.x + a.width;
    let a_bottom = a.y + a.height;

    let b_left = b.x;
    let b_top = b.y;
    let b_right = b.x + b.width;
    let b_bottom = b.y + b.height;

    // If b completely covers a, remainder is empty
    if b_left <= a_left && b_right >= a_right && b_top <= a_top && b_bottom >= a_bottom {
        return;
    }

    // Top slice
    if b_top > a_top {
        out.push(Rect::new(a_left, a_top, a.width, b_top - a_top));
    }

    // Bottom slice
    if b_bottom < a_bottom {
        out.push(Rect::new(a_left, b_bottom, a.width, a_bottom - b_bottom));
    }

    // Middle vertical span (clamped to a's top and bottom)
    let mid_top = a_top.max(b_top);
    let mid_bottom = a_bottom.min(b_bottom);
    let mid_height = mid_bottom - mid_top;

    if mid_height > 0 {
        // Left slice
        if b_left > a_left {
            out.push(Rect::new(a_left, mid_top, b_left - a_left, mid_height));
        }

        // Right slice
        if b_right < a_right {
            out.push(Rect::new(b_right, mid_top, a_right - b_right, mid_height));
        }
    }
}

/// Coalesces adjacent rectangles sharing the same x/width or y/height.
fn coalesce_rects(rects: &mut Vec<Rect>) {
    if rects.len() <= 1 {
        return;
    }

    let mut changed = true;
    while changed {
        changed = false;
        let mut i = 0;
        while i < rects.len() {
            let mut j = i + 1;
            while j < rects.len() {
                let r1 = rects[i];
                let r2 = rects[j];

                // Merge vertically if same x and width and touching
                if r1.x == r2.x && r1.width == r2.width {
                    if r1.y + r1.height == r2.y {
                        rects[i].height += r2.height;
                        rects.swap_remove(j);
                        changed = true;
                        continue;
                    } else if r2.y + r2.height == r1.y {
                        rects[i].y = r2.y;
                        rects[i].height += r2.height;
                        rects.swap_remove(j);
                        changed = true;
                        continue;
                    }
                }

                // Merge horizontally if same y and height and touching
                if r1.y == r2.y && r1.height == r2.height {
                    if r1.x + r1.width == r2.x {
                        rects[i].width += r2.width;
                        rects.swap_remove(j);
                        changed = true;
                        continue;
                    } else if r2.x + r2.width == r1.x {
                        rects[i].x = r2.x;
                        rects[i].width += r2.width;
                        rects.swap_remove(j);
                        changed = true;
                        continue;
                    }
                }

                j += 1;
            }
            i += 1;
        }
    }
}

/// 2D Region composed of non-overlapping rectangles (`QRegion`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Region {
    rects: Vec<Rect>,
}

impl Region {
    /// Constructs an empty region.
    pub const fn new() -> Self {
        Self { rects: Vec::new() }
    }

    /// Constructs a region from a single rectangle.
    pub fn from_rect(r: Rect) -> Self {
        if r.is_empty() || r.width <= 0 || r.height <= 0 {
            Self::new()
        } else {
            Self { rects: vec![r] }
        }
    }

    /// Constructs a region from coordinate scalars.
    pub fn from_coords(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self::from_rect(Rect::new(x, y, width, height))
    }

    /// Constructs a region by uniting multiple rectangles.
    pub fn from_rects(rects: &[Rect]) -> Self {
        let mut r = Self::new();
        for &rect in rects {
            r = r.united_rect(&rect);
        }
        r
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

        let mut min_x = self.rects[0].x;
        let mut min_y = self.rects[0].y;
        let mut max_x = self.rects[0].x + self.rects[0].width;
        let mut max_y = self.rects[0].y + self.rects[0].height;

        for r in &self.rects[1..] {
            if r.x < min_x {
                min_x = r.x;
            }
            if r.y < min_y {
                min_y = r.y;
            }
            let right = r.x + r.width;
            let bottom = r.y + r.height;
            if right > max_x {
                max_x = right;
            }
            if bottom > max_y {
                max_y = bottom;
            }
        }

        Rect::new(min_x, min_y, max_x - min_x, max_y - min_y)
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
        for r1 in &self.rects {
            for r2 in &other.rects {
                if r1.intersects(r2) {
                    return true;
                }
            }
        }
        false
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
        for r in &mut self.rects {
            r.x += dx;
            r.y += dy;
        }
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
        if self.is_empty() || other.is_empty() {
            return self.clone();
        }

        let mut result = Vec::new();
        for r in &self.rects {
            subtract_rect(r, other, &mut result);
        }
        coalesce_rects(&mut result);
        Self { rects: result }
    }

    /// Subtracts `other` region from this region (`self - other`).
    pub fn subtracted(&self, other: &Region) -> Self {
        if self.is_empty() || other.is_empty() {
            return self.clone();
        }

        let mut current = self.clone();
        for r in &other.rects {
            current = current.subtracted_rect(r);
            if current.is_empty() {
                break;
            }
        }
        current
    }

    /// Intersects this region with a rectangle (`self & other`).
    pub fn intersected_rect(&self, other: &Rect) -> Self {
        if self.is_empty() || other.is_empty() {
            return Self::new();
        }

        let mut result = Vec::new();
        for r in &self.rects {
            let inter = r.intersected(other);
            if !inter.is_empty() && inter.width > 0 && inter.height > 0 {
                result.push(inter);
            }
        }
        coalesce_rects(&mut result);
        Self { rects: result }
    }

    /// Intersects this region with another region (`self & other`).
    pub fn intersected(&self, other: &Region) -> Self {
        if self.is_empty() || other.is_empty() {
            return Self::new();
        }

        let mut result = Vec::new();
        for r1 in &self.rects {
            for r2 in &other.rects {
                let inter = r1.intersected(r2);
                if !inter.is_empty() && inter.width > 0 && inter.height > 0 {
                    result.push(inter);
                }
            }
        }
        coalesce_rects(&mut result);
        Self { rects: result }
    }

    /// Unites this region with a rectangle (`self | other`).
    pub fn united_rect(&self, other: &Rect) -> Self {
        if other.is_empty() || other.width <= 0 || other.height <= 0 {
            return self.clone();
        }
        if self.is_empty() {
            return Self::from_rect(*other);
        }

        // other - self gives the disjoint additions
        let addition = Region::from_rect(*other).subtracted(self);
        let mut new_rects = self.rects.clone();
        new_rects.extend(addition.rects);
        coalesce_rects(&mut new_rects);
        Self { rects: new_rects }
    }

    /// Unites this region with another region (`self | other`).
    pub fn united(&self, other: &Region) -> Self {
        if other.is_empty() {
            return self.clone();
        }
        if self.is_empty() {
            return other.clone();
        }

        let addition = other.subtracted(self);
        let mut new_rects = self.rects.clone();
        new_rects.extend(addition.rects);
        coalesce_rects(&mut new_rects);
        Self { rects: new_rects }
    }

    /// Symmetric difference / XOR of two regions (`self ^ other`).
    pub fn xored(&self, other: &Region) -> Self {
        let diff1 = self.subtracted(other);
        let diff2 = other.subtracted(self);
        diff1.united(&diff2)
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
