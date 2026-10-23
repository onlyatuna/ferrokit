// SPDX-License-Identifier: LGPL-3.0-only
use std::ops::{Add, AddAssign, Div, DivAssign, Mul, MulAssign, Neg, Sub, SubAssign};

///
/// 2D integer point (`QPoint` equivalent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

///
/// 2D floating-point point (`QPointF` equivalent).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PointF {
    pub x: f32,
    pub y: f32,
}

impl Point {
    #[inline]
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }

    #[inline]
    pub const fn is_null(&self) -> bool {
        self.x == 0 && self.y == 0
    }

    //
    // Returns Manhattan length: |x| + |y| (`QPoint::manhattanLength`).

    #[inline]
    pub const fn manhattan_length(&self) -> i32 {
        self.x.abs() + self.y.abs()
    }

    //
    // Returns transposed coordinates (y, x).
    #[inline]
    pub const fn transposed(self) -> Self {
        Self {
            x: self.y,
            y: self.x,
        }
    }

    #[inline]
    pub const fn to_f32(self) -> PointF {
        PointF {
            x: self.x as f32,
            y: self.y as f32,
        }
    }

    #[inline]
    pub const fn to_point_f(self) -> PointF {
        self.to_f32()
    }
}

impl PointF {
    #[inline]
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    #[inline]
    pub fn is_null(&self) -> bool {
        self.x == 0.0 && self.y == 0.0
    }

    //
    // Returns Manhattan length: |x| + |y| (`QPoint::manhattanLength`).
    #[inline]
    pub fn manhattan_length(&self) -> f32 {
        self.x.abs() + self.y.abs()
    }

    //
    // Returns transposed coordinates (y, x).
    #[inline]
    pub const fn transposed(self) -> Self {
        Self {
            x: self.y,
            y: self.x,
        }
    }

    // 2D floating-point point (`QPointF` equivalent).
    #[inline]
    pub fn to_i32(self) -> Point {
        Point {
            x: self.x.round() as i32,
            y: self.y.round() as i32,
        }
    }

    #[inline]
    pub fn to_point(self) -> Point {
        self.to_i32()
    }

    #[inline]
    pub fn truncate_to_i32(self) -> Point {
        Point {
            x: self.x as i32,
            y: self.y as i32,
        }
    }
}

///
/// 2D integer margins (`QMargins` equivalent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Margins {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

///
/// 2D floating-point margins (`QMarginsF` equivalent).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MarginsF {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl Margins {
    #[inline]
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    #[inline]
    pub const fn is_null(&self) -> bool {
        self.left == 0 && self.top == 0 && self.right == 0 && self.bottom == 0
    }

    #[inline]
    pub const fn to_f32(self) -> MarginsF {
        MarginsF {
            left: self.left as f32,
            top: self.top as f32,
            right: self.right as f32,
            bottom: self.bottom as f32,
        }
    }
}

impl MarginsF {
    #[inline]
    pub const fn new(left: f32, top: f32, right: f32, bottom: f32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    #[inline]
    pub fn is_null(&self) -> bool {
        self.left == 0.0 && self.top == 0.0 && self.right == 0.0 && self.bottom == 0.0
    }

    #[inline]
    pub fn to_i32(self) -> Margins {
        Margins {
            left: self.left.round() as i32,
            top: self.top.round() as i32,
            right: self.right.round() as i32,
            bottom: self.bottom.round() as i32,
        }
    }
}

// -----------------------------------------------------------------------------

// -----------------------------------------------------------------------------

impl Add for Margins {
    type Output = Self;
    #[inline]
    fn add(self, rhs: Self) -> Self::Output {
        Self {
            left: self.left + rhs.left,
            top: self.top + rhs.top,
            right: self.right + rhs.right,
            bottom: self.bottom + rhs.bottom,
        }
    }
}

impl AddAssign for Margins {
    #[inline]
    fn add_assign(&mut self, rhs: Self) {
        self.left += rhs.left;
        self.top += rhs.top;
        self.right += rhs.right;
        self.bottom += rhs.bottom;
    }
}

impl Sub for Margins {
    type Output = Self;
    #[inline]
    fn sub(self, rhs: Self) -> Self::Output {
        Self {
            left: self.left - rhs.left,
            top: self.top - rhs.top,
            right: self.right - rhs.right,
            bottom: self.bottom - rhs.bottom,
        }
    }
}

impl SubAssign for Margins {
    #[inline]
    fn sub_assign(&mut self, rhs: Self) {
        self.left -= rhs.left;
        self.top -= rhs.top;
        self.right -= rhs.right;
        self.bottom -= rhs.bottom;
    }
}

// -----------------------------------------------------------------------------

// -----------------------------------------------------------------------------

impl Add for MarginsF {
    type Output = Self;
    #[inline]
    fn add(self, rhs: Self) -> Self::Output {
        Self {
            left: self.left + rhs.left,
            top: self.top + rhs.top,
            right: self.right + rhs.right,
            bottom: self.bottom + rhs.bottom,
        }
    }
}

impl AddAssign for MarginsF {
    #[inline]
    fn add_assign(&mut self, rhs: Self) {
        self.left += rhs.left;
        self.top += rhs.top;
        self.right += rhs.right;
        self.bottom += rhs.bottom;
    }
}

impl Sub for MarginsF {
    type Output = Self;
    #[inline]
    fn sub(self, rhs: Self) -> Self::Output {
        Self {
            left: self.left - rhs.left,
            top: self.top - rhs.top,
            right: self.right - rhs.right,
            bottom: self.bottom - rhs.bottom,
        }
    }
}

impl SubAssign for MarginsF {
    #[inline]
    fn sub_assign(&mut self, rhs: Self) {
        self.left -= rhs.left;
        self.top -= rhs.top;
        self.right -= rhs.right;
        self.bottom -= rhs.bottom;
    }
}

impl From<Margins> for MarginsF {
    #[inline]
    fn from(m: Margins) -> Self {
        m.to_f32()
    }
}

impl From<(i32, i32, i32, i32)> for Margins {
    #[inline]
    fn from((left, top, right, bottom): (i32, i32, i32, i32)) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }
}

impl From<(f32, f32, f32, f32)> for MarginsF {
    #[inline]
    fn from((left, top, right, bottom): (f32, f32, f32, f32)) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }
}

///
/// 2D integer size (`QSize` equivalent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Size {
    pub width: i32,
    pub height: i32,
}

///
/// 2D floating-point size (`QSizeF` equivalent).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SizeF {
    pub width: f32,
    pub height: f32,
}

impl Size {
    #[inline]
    pub const fn new(width: i32, height: i32) -> Self {
        Self { width, height }
    }

    // 2D integer size (`QSize` equivalent).
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.width <= 0 || self.height <= 0
    }

    // 2D integer size (`QSize` equivalent).
    #[inline]
    pub const fn is_valid(&self) -> bool {
        self.width >= 0 && self.height >= 0
    }

    #[inline]
    pub const fn is_null(&self) -> bool {
        self.width == 0 && self.height == 0
    }

    #[inline]
    pub const fn transposed(self) -> Self {
        Self {
            width: self.height,
            height: self.width,
        }
    }

    //
    // 2D integer size (`QSize` equivalent).
    #[inline]
    pub fn expanded_to(&self, other: Size) -> Size {
        Size {
            width: self.width.max(other.width),
            height: self.height.max(other.height),
        }
    }

    //
    // 2D integer size (`QSize` equivalent).
    #[inline]
    pub fn bounded_to(&self, other: Size) -> Size {
        Size {
            width: self.width.min(other.width),
            height: self.height.min(other.height),
        }
    }

    // 2D integer margins (`QMargins` equivalent).
    #[inline]
    pub const fn grown_by(&self, m: Margins) -> Size {
        Size {
            width: self.width + m.left + m.right,
            height: self.height + m.top + m.bottom,
        }
    }

    // 2D integer margins (`QMargins` equivalent).
    #[inline]
    pub const fn shrunk_by(&self, m: Margins) -> Size {
        Size {
            width: self.width - m.left - m.right,
            height: self.height - m.top - m.bottom,
        }
    }

    #[inline]
    pub const fn to_f32(self) -> SizeF {
        SizeF {
            width: self.width as f32,
            height: self.height as f32,
        }
    }
}

impl SizeF {
    #[inline]
    pub const fn new(width: f32, height: f32) -> Self {
        Self { width, height }
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.width <= 0.0 || self.height <= 0.0
    }

    #[inline]
    pub fn is_valid(&self) -> bool {
        self.width >= 0.0 && self.height >= 0.0
    }

    #[inline]
    pub fn is_null(&self) -> bool {
        self.width == 0.0 && self.height == 0.0
    }

    #[inline]
    pub const fn transposed(self) -> Self {
        Self {
            width: self.height,
            height: self.width,
        }
    }

    //
    // 2D floating-point size (`QSizeF` equivalent).
    #[inline]
    pub fn expanded_to(&self, other: SizeF) -> SizeF {
        SizeF {
            width: self.width.max(other.width),
            height: self.height.max(other.height),
        }
    }

    //
    // 2D floating-point size (`QSizeF` equivalent).
    #[inline]
    pub fn bounded_to(&self, other: SizeF) -> SizeF {
        SizeF {
            width: self.width.min(other.width),
            height: self.height.min(other.height),
        }
    }

    // 2D floating-point margins (`QMarginsF` equivalent).
    #[inline]
    pub fn grown_by(&self, m: MarginsF) -> SizeF {
        SizeF {
            width: self.width + m.left + m.right,
            height: self.height + m.top + m.bottom,
        }
    }

    // 2D floating-point margins (`QMarginsF` equivalent).
    #[inline]
    pub fn shrunk_by(&self, m: MarginsF) -> SizeF {
        SizeF {
            width: self.width - m.left - m.right,
            height: self.height - m.top - m.bottom,
        }
    }

    #[inline]
    pub fn to_i32(self) -> Size {
        Size {
            width: self.width.round() as i32,
            height: self.height.round() as i32,
        }
    }
}

///
/// 2D integer rectangle (`QRect` equivalent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

///
/// 2D floating-point rectangle (`QRectF` equivalent).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct RectF {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    #[inline]
    pub const fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    #[inline]
    pub const fn from_point_and_size(point: Point, size: Size) -> Self {
        Self {
            x: point.x,
            y: point.y,
            width: size.width,
            height: size.height,
        }
    }

    #[inline]
    pub fn from_points(p1: Point, p2: Point) -> Self {
        let x = p1.x.min(p2.x);
        let y = p1.y.min(p2.y);
        let width = (p1.x - p2.x).abs();
        let height = (p1.y - p2.y).abs();
        Self {
            x,
            y,
            width,
            height,
        }
    }

    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.width <= 0 || self.height <= 0
    }

    #[inline]
    pub const fn is_valid(&self) -> bool {
        self.width >= 0 && self.height >= 0
    }

    // Returns true if coordinates are at origin (0, 0).
    #[inline]
    pub const fn is_null(&self) -> bool {
        self.x == 0 && self.y == 0 && self.width == 0 && self.height == 0
    }

    #[inline]
    pub const fn left(&self) -> i32 {
        self.x
    }

    #[inline]
    pub const fn top(&self) -> i32 {
        self.y
    }

    /// Exclusive right edge: `x + width`, saturating at `i32::MAX`.
    ///
    /// This is NOT `QRect::right()`, which is the inclusive last column `x + width - 1`.
    /// The Qt-equivalent last column is `right() - 1`. `QRect` computes `x + w - 1` in plain
    /// `int` arithmetic, which is undefined on overflow; saturating keeps debug builds panic-free.
    #[inline]
    pub const fn right(&self) -> i32 {
        self.x.saturating_add(self.width)
    }

    /// Exclusive bottom edge: `y + height`, saturating at `i32::MAX`.
    ///
    /// This is NOT `QRect::bottom()`, which is the inclusive last row `y + height - 1`.
    /// The Qt-equivalent last row is `bottom() - 1`.
    #[inline]
    pub const fn bottom(&self) -> i32 {
        self.y.saturating_add(self.height)
    }

    // Center

    #[inline]
    pub const fn top_left(&self) -> Point {
        Point::new(self.left(), self.top())
    }

    #[inline]
    pub const fn top_right(&self) -> Point {
        Point::new(self.right(), self.top())
    }

    #[inline]
    pub const fn bottom_left(&self) -> Point {
        Point::new(self.left(), self.bottom())
    }

    #[inline]
    pub const fn bottom_right(&self) -> Point {
        Point::new(self.right(), self.bottom())
    }

    // Center
    /// Centre from exclusive corners: `(x + width / 2, y + height / 2)`.
    ///
    /// Differs from `QRect::center()`, which uses inclusive corners
    /// `((x1 + x2) / 2, (y1 + y2) / 2)` with `x2 = x + width - 1`; it can be one pixel off.
    /// `Rect` is a half-open (exclusive-edge) type by design.
    #[inline]
    pub const fn center(&self) -> Point {
        Point::new(
            self.x.saturating_add(self.width / 2),
            self.y.saturating_add(self.height / 2),
        )
    }

    #[inline]
    pub const fn size(&self) -> Size {
        Size::new(self.width, self.height)
    }

    // Checks if point is contained in rectangle (`[left, right)` x `[top, bottom)`).
    #[inline]
    pub fn contains(&self, point: Point) -> bool {
        if self.is_empty() {
            return false;
        }
        let l = self.left().min(self.right());
        let r = self.left().max(self.right());
        let t = self.top().min(self.bottom());
        let b = self.top().max(self.bottom());
        point.x >= l && point.x < r && point.y >= t && point.y < b
    }

    #[inline]
    pub fn intersects(&self, other: &Rect) -> bool {
        if self.is_empty() || other.is_empty() {
            return false;
        }
        let l1 = self.left().min(self.right());
        let r1 = self.left().max(self.right());
        let l2 = other.left().min(other.right());
        let r2 = other.left().max(other.right());

        if l1 >= r2 || l2 >= r1 {
            return false;
        }

        let t1 = self.top().min(self.bottom());
        let b1 = self.top().max(self.bottom());
        let t2 = other.top().min(other.bottom());
        let b2 = other.top().max(other.bottom());

        if t1 >= b2 || t2 >= b1 {
            return false;
        }

        true
    }

    // Returns intersection of two rectangles.
    #[inline]
    pub fn intersected(&self, other: &Rect) -> Rect {
        if !self.intersects(other) {
            return Rect::default();
        }
        let l = self.left().max(other.left());
        let r = self.right().min(other.right());
        let t = self.top().max(other.top());
        let b = self.bottom().min(other.bottom());
        Rect::new(l, t, r.saturating_sub(l), b.saturating_sub(t))
    }

    // Returns bounding union of two rectangles.
    #[inline]
    pub fn united(&self, other: &Rect) -> Rect {
        if self.is_empty() {
            return *other;
        }
        if other.is_empty() {
            return *self;
        }
        let l = self.left().min(other.left());
        let r = self.right().max(other.right());
        let t = self.top().min(other.top());
        let b = self.bottom().max(other.bottom());
        Rect::new(l, t, r.saturating_sub(l), b.saturating_sub(t))
    }

    // Adjusts rectangle coordinates by deltas.
    #[inline]
    pub const fn adjusted(&self, dx1: i32, dy1: i32, dx2: i32, dy2: i32) -> Rect {
        Rect {
            x: self.x + dx1,
            y: self.y + dy1,
            width: self.width + dx2 - dx1,
            height: self.height + dy2 - dy1,
        }
    }

    // 2D integer rectangle (`QRect` equivalent).
    #[inline]
    pub const fn margins_added(&self, m: Margins) -> Rect {
        Rect {
            x: self.x - m.left,
            y: self.y - m.top,
            width: self.width + m.left + m.right,
            height: self.height + m.top + m.bottom,
        }
    }

    // 2D integer rectangle (`QRect` equivalent).
    #[inline]
    pub const fn margins_removed(&self, m: Margins) -> Rect {
        Rect {
            x: self.x + m.left,
            y: self.y + m.top,
            width: self.width - m.left - m.right,
            height: self.height - m.top - m.bottom,
        }
    }

    #[inline]
    pub const fn translated(&self, dx: i32, dy: i32) -> Rect {
        Rect {
            x: self.x + dx,
            y: self.y + dy,
            width: self.width,
            height: self.height,
        }
    }

    #[inline]
    pub const fn to_f32(self) -> RectF {
        RectF {
            x: self.x as f32,
            y: self.y as f32,
            width: self.width as f32,
            height: self.height as f32,
        }
    }
}

impl RectF {
    #[inline]
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    #[inline]
    pub const fn from_point_and_size(point: PointF, size: SizeF) -> Self {
        Self {
            x: point.x,
            y: point.y,
            width: size.width,
            height: size.height,
        }
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.width <= 0.0 || self.height <= 0.0
    }

    #[inline]
    pub fn is_valid(&self) -> bool {
        self.width >= 0.0 && self.height >= 0.0
    }

    #[inline]
    pub fn is_null(&self) -> bool {
        self.x == 0.0 && self.y == 0.0 && self.width == 0.0 && self.height == 0.0
    }

    #[inline]
    pub const fn left(&self) -> f32 {
        self.x
    }

    #[inline]
    pub const fn top(&self) -> f32 {
        self.y
    }

    #[inline]
    pub fn right(&self) -> f32 {
        self.x + self.width
    }

    #[inline]
    pub fn bottom(&self) -> f32 {
        self.y + self.height
    }

    #[inline]
    pub fn top_left(&self) -> PointF {
        PointF::new(self.left(), self.top())
    }

    #[inline]
    pub fn top_right(&self) -> PointF {
        PointF::new(self.right(), self.top())
    }

    #[inline]
    pub fn bottom_left(&self) -> PointF {
        PointF::new(self.left(), self.bottom())
    }

    #[inline]
    pub fn bottom_right(&self) -> PointF {
        PointF::new(self.right(), self.bottom())
    }

    #[inline]
    pub fn center(&self) -> PointF {
        PointF::new(self.x + self.width / 2.0, self.y + self.height / 2.0)
    }

    #[inline]
    pub const fn size(&self) -> SizeF {
        SizeF::new(self.width, self.height)
    }

    // Checks if point is contained in rectangle (`[left, right)` x `[top, bottom)`).
    #[inline]
    pub fn contains(&self, point: PointF) -> bool {
        if self.is_empty() {
            return false;
        }
        let l = self.left().min(self.right());
        let r = self.left().max(self.right());
        let t = self.top().min(self.bottom());
        let b = self.top().max(self.bottom());
        point.x >= l && point.x < r && point.y >= t && point.y < b
    }

    #[inline]
    pub fn intersects(&self, other: &RectF) -> bool {
        if self.is_empty() || other.is_empty() {
            return false;
        }
        let l1 = self.left().min(self.right());
        let r1 = self.left().max(self.right());
        let l2 = other.left().min(other.right());
        let r2 = other.left().max(other.right());

        if l1 >= r2 || l2 >= r1 {
            return false;
        }

        let t1 = self.top().min(self.bottom());
        let b1 = self.top().max(self.bottom());
        let t2 = other.top().min(other.bottom());
        let b2 = other.top().max(other.bottom());

        if t1 >= b2 || t2 >= b1 {
            return false;
        }

        true
    }

    #[inline]
    pub fn intersected(&self, other: &RectF) -> RectF {
        if !self.intersects(other) {
            return RectF::default();
        }
        let l = self.left().max(other.left());
        let r = self.right().min(other.right());
        let t = self.top().max(other.top());
        let b = self.bottom().min(other.bottom());
        RectF::new(l, t, r - l, b - t)
    }

    #[inline]
    pub fn united(&self, other: &RectF) -> RectF {
        if self.is_empty() {
            return *other;
        }
        if other.is_empty() {
            return *self;
        }
        let l = self.left().min(other.left());
        let r = self.right().max(other.right());
        let t = self.top().min(other.top());
        let b = self.bottom().max(other.bottom());
        RectF::new(l, t, r - l, b - t)
    }

    #[inline]
    pub fn adjusted(&self, dx1: f32, dy1: f32, dx2: f32, dy2: f32) -> RectF {
        RectF {
            x: self.x + dx1,
            y: self.y + dy1,
            width: self.width + dx2 - dx1,
            height: self.height + dy2 - dy1,
        }
    }

    #[inline]
    pub fn margins_added(&self, m: MarginsF) -> RectF {
        RectF {
            x: self.x - m.left,
            y: self.y - m.top,
            width: self.width + m.left + m.right,
            height: self.height + m.top + m.bottom,
        }
    }

    #[inline]
    pub fn margins_removed(&self, m: MarginsF) -> RectF {
        RectF {
            x: self.x + m.left,
            y: self.y + m.top,
            width: self.width - m.left - m.right,
            height: self.height - m.top - m.bottom,
        }
    }

    #[inline]
    pub fn translated(&self, dx: f32, dy: f32) -> RectF {
        RectF {
            x: self.x + dx,
            y: self.y + dy,
            width: self.width,
            height: self.height,
        }
    }

    #[inline]
    pub fn to_i32(self) -> Rect {
        Rect {
            x: self.x.round() as i32,
            y: self.y.round() as i32,
            width: self.width.round() as i32,
            height: self.height.round() as i32,
        }
    }
}

impl From<Rect> for RectF {
    #[inline]
    fn from(r: Rect) -> Self {
        r.to_f32()
    }
}

// -----------------------------------------------------------------------------

// -----------------------------------------------------------------------------

impl Add for Size {
    type Output = Self;
    #[inline]
    fn add(self, rhs: Self) -> Self::Output {
        Self {
            width: self.width + rhs.width,
            height: self.height + rhs.height,
        }
    }
}

impl AddAssign for Size {
    #[inline]
    fn add_assign(&mut self, rhs: Self) {
        self.width += rhs.width;
        self.height += rhs.height;
    }
}

impl Sub for Size {
    type Output = Self;
    #[inline]
    fn sub(self, rhs: Self) -> Self::Output {
        Self {
            width: self.width - rhs.width,
            height: self.height - rhs.height,
        }
    }
}

impl SubAssign for Size {
    #[inline]
    fn sub_assign(&mut self, rhs: Self) {
        self.width -= rhs.width;
        self.height -= rhs.height;
    }
}

impl Mul<i32> for Size {
    type Output = Self;
    #[inline]
    fn mul(self, rhs: i32) -> Self::Output {
        Self {
            width: self.width * rhs,
            height: self.height * rhs,
        }
    }
}

impl Mul<f32> for Size {
    type Output = SizeF;
    #[inline]
    fn mul(self, rhs: f32) -> Self::Output {
        SizeF {
            width: self.width as f32 * rhs,
            height: self.height as f32 * rhs,
        }
    }
}

impl Div<i32> for Size {
    type Output = Self;
    #[inline]
    fn div(self, rhs: i32) -> Self::Output {
        Self {
            width: self.width / rhs,
            height: self.height / rhs,
        }
    }
}

impl Div<f32> for Size {
    type Output = SizeF;
    #[inline]
    fn div(self, rhs: f32) -> Self::Output {
        SizeF {
            width: self.width as f32 / rhs,
            height: self.height as f32 / rhs,
        }
    }
}
impl MulAssign<i32> for Size {
    #[inline]
    fn mul_assign(&mut self, rhs: i32) {
        self.width *= rhs;
        self.height *= rhs;
    }
}

impl DivAssign<i32> for Size {
    #[inline]
    fn div_assign(&mut self, rhs: i32) {
        self.width /= rhs;
        self.height /= rhs;
    }
}

// -----------------------------------------------------------------------------

// -----------------------------------------------------------------------------

impl Add for SizeF {
    type Output = Self;
    #[inline]
    fn add(self, rhs: Self) -> Self::Output {
        Self {
            width: self.width + rhs.width,
            height: self.height + rhs.height,
        }
    }
}

impl AddAssign for SizeF {
    #[inline]
    fn add_assign(&mut self, rhs: Self) {
        self.width += rhs.width;
        self.height += rhs.height;
    }
}

impl Sub for SizeF {
    type Output = Self;
    #[inline]
    fn sub(self, rhs: Self) -> Self::Output {
        Self {
            width: self.width - rhs.width,
            height: self.height - rhs.height,
        }
    }
}

impl SubAssign for SizeF {
    #[inline]
    fn sub_assign(&mut self, rhs: Self) {
        self.width -= rhs.width;
        self.height -= rhs.height;
    }
}

impl Mul<f32> for SizeF {
    type Output = Self;
    #[inline]
    fn mul(self, rhs: f32) -> Self::Output {
        Self {
            width: self.width * rhs,
            height: self.height * rhs,
        }
    }
}

impl Div<f32> for SizeF {
    type Output = Self;
    #[inline]
    fn div(self, rhs: f32) -> Self::Output {
        Self {
            width: self.width / rhs,
            height: self.height / rhs,
        }
    }
}
impl MulAssign<f32> for SizeF {
    #[inline]
    fn mul_assign(&mut self, rhs: f32) {
        self.width *= rhs;
        self.height *= rhs;
    }
}

impl DivAssign<f32> for SizeF {
    #[inline]
    fn div_assign(&mut self, rhs: f32) {
        self.width /= rhs;
        self.height /= rhs;
    }
}

impl From<(i32, i32)> for Size {
    #[inline]
    fn from((width, height): (i32, i32)) -> Self {
        Self { width, height }
    }
}

impl From<(f32, f32)> for SizeF {
    #[inline]
    fn from((width, height): (f32, f32)) -> Self {
        Self { width, height }
    }
}

impl From<Size> for SizeF {
    #[inline]
    fn from(s: Size) -> Self {
        s.to_f32()
    }
}
// -----------------------------------------------------------------------------

// -----------------------------------------------------------------------------

impl Add for Point {
    type Output = Self;
    #[inline]
    fn add(self, rhs: Self) -> Self::Output {
        Self {
            x: self.x + rhs.x,
            y: self.y + rhs.y,
        }
    }
}

impl AddAssign for Point {
    #[inline]
    fn add_assign(&mut self, rhs: Self) {
        self.x += rhs.x;
        self.y += rhs.y;
    }
}

impl Sub for Point {
    type Output = Self;
    #[inline]
    fn sub(self, rhs: Self) -> Self::Output {
        Self {
            x: self.x - rhs.x,
            y: self.y - rhs.y,
        }
    }
}

impl SubAssign for Point {
    #[inline]
    fn sub_assign(&mut self, rhs: Self) {
        self.x -= rhs.x;
        self.y -= rhs.y;
    }
}

impl Mul<i32> for Point {
    type Output = Self;
    #[inline]
    fn mul(self, rhs: i32) -> Self::Output {
        Self {
            x: self.x * rhs,
            y: self.y * rhs,
        }
    }
}

impl MulAssign<i32> for Point {
    #[inline]
    fn mul_assign(&mut self, rhs: i32) {
        self.x *= rhs;
        self.y *= rhs;
    }
}

impl Mul<f32> for Point {
    type Output = PointF;
    #[inline]
    fn mul(self, rhs: f32) -> Self::Output {
        PointF {
            x: self.x as f32 * rhs,
            y: self.y as f32 * rhs,
        }
    }
}

impl Div<i32> for Point {
    type Output = Self;
    #[inline]
    fn div(self, rhs: i32) -> Self::Output {
        Self {
            x: self.x / rhs,
            y: self.y / rhs,
        }
    }
}

impl DivAssign<i32> for Point {
    #[inline]
    fn div_assign(&mut self, rhs: i32) {
        self.x /= rhs;
        self.y /= rhs;
    }
}

impl Div<f32> for Point {
    type Output = PointF;
    #[inline]
    fn div(self, rhs: f32) -> Self::Output {
        PointF {
            x: self.x as f32 / rhs,
            y: self.y as f32 / rhs,
        }
    }
}

impl Neg for Point {
    type Output = Self;
    #[inline]
    fn neg(self) -> Self::Output {
        Self {
            x: -self.x,
            y: -self.y,
        }
    }
}

// -----------------------------------------------------------------------------

// -----------------------------------------------------------------------------

impl Add for PointF {
    type Output = Self;
    #[inline]
    fn add(self, rhs: Self) -> Self::Output {
        Self {
            x: self.x + rhs.x,
            y: self.y + rhs.y,
        }
    }
}

impl AddAssign for PointF {
    #[inline]
    fn add_assign(&mut self, rhs: Self) {
        self.x += rhs.x;
        self.y += rhs.y;
    }
}

impl Sub for PointF {
    type Output = Self;
    #[inline]
    fn sub(self, rhs: Self) -> Self::Output {
        Self {
            x: self.x - rhs.x,
            y: self.y - rhs.y,
        }
    }
}

impl SubAssign for PointF {
    #[inline]
    fn sub_assign(&mut self, rhs: Self) {
        self.x -= rhs.x;
        self.y -= rhs.y;
    }
}

impl Mul<f32> for PointF {
    type Output = Self;
    #[inline]
    fn mul(self, rhs: f32) -> Self::Output {
        Self {
            x: self.x * rhs,
            y: self.y * rhs,
        }
    }
}

impl MulAssign<f32> for PointF {
    #[inline]
    fn mul_assign(&mut self, rhs: f32) {
        self.x *= rhs;
        self.y *= rhs;
    }
}

impl Div<f32> for PointF {
    type Output = Self;
    #[inline]
    fn div(self, rhs: f32) -> Self::Output {
        Self {
            x: self.x / rhs,
            y: self.y / rhs,
        }
    }
}

impl DivAssign<f32> for PointF {
    #[inline]
    fn div_assign(&mut self, rhs: f32) {
        self.x /= rhs;
        self.y /= rhs;
    }
}

impl Neg for PointF {
    type Output = Self;
    #[inline]
    fn neg(self) -> Self::Output {
        Self {
            x: -self.x,
            y: -self.y,
        }
    }
}

// -----------------------------------------------------------------------------
// Center
// -----------------------------------------------------------------------------

impl From<(i32, i32)> for Point {
    #[inline]
    fn from((x, y): (i32, i32)) -> Self {
        Self { x, y }
    }
}

impl From<Point> for (i32, i32) {
    #[inline]
    fn from(p: Point) -> Self {
        (p.x, p.y)
    }
}

impl From<(f32, f32)> for PointF {
    #[inline]
    fn from((x, y): (f32, f32)) -> Self {
        Self { x, y }
    }
}

impl From<PointF> for (f32, f32) {
    #[inline]
    fn from(p: PointF) -> Self {
        (p.x, p.y)
    }
}

impl From<Point> for PointF {
    #[inline]
    fn from(p: Point) -> Self {
        p.to_f32()
    }
}

// -----------------------------------------------------------------------------
// Conversions with tiny-skia
// -----------------------------------------------------------------------------

impl From<RectF> for tiny_skia::Rect {
    fn from(r: RectF) -> Self {
        tiny_skia::Rect::from_xywh(r.x, r.y, r.width, r.height)
            .unwrap_or_else(|| tiny_skia::Rect::from_xywh(0.0, 0.0, 1.0, 1.0).unwrap())
    }
}

impl From<PointF> for tiny_skia::Point {
    fn from(p: PointF) -> Self {
        tiny_skia::Point::from_xy(p.x, p.y)
    }
}

impl From<Rect> for tiny_skia::Rect {
    fn from(r: Rect) -> Self {
        r.to_f32().into()
    }
}

impl From<Point> for tiny_skia::Point {
    fn from(p: Point) -> Self {
        p.to_f32().into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_point_basic_and_manhattan() {
        let p = Point::new(-10, 25);
        assert_eq!(p.x, -10);
        assert_eq!(p.y, 25);
        assert_eq!(p.manhattan_length(), 35);
        assert!(!p.is_null());
        assert!(Point::default().is_null());

        let t = p.transposed();
        assert_eq!(t, Point::new(25, -10));
    }

    #[test]
    fn test_point_operators() {
        let p1 = Point::new(10, 20);
        let p2 = Point::new(3, 4);

        assert_eq!(p1 + p2, Point::new(13, 24));
        assert_eq!(p1 - p2, Point::new(7, 16));
        assert_eq!(-p1, Point::new(-10, -20));
        assert_eq!(p1 * 2, Point::new(20, 40));
        assert_eq!(p1 / 2, Point::new(5, 10));

        let pf = p1 * 1.5f32;
        assert_eq!(pf, PointF::new(15.0, 30.0));
        let pf2 = p1 / 2.0f32;
        assert_eq!(pf2, PointF::new(5.0, 10.0));

        let mut p_assign = Point::new(5, 5);
        p_assign += Point::new(2, 3);
        assert_eq!(p_assign, Point::new(7, 8));
        p_assign -= Point::new(1, 2);
        assert_eq!(p_assign, Point::new(6, 6));
        p_assign *= 2;
        assert_eq!(p_assign, Point::new(12, 12));
        p_assign /= 3;
        assert_eq!(p_assign, Point::new(4, 4));
    }

    #[test]
    fn test_point_f_and_conversions() {
        let pf = PointF::new(10.6, -20.4);
        assert_eq!(pf.manhattan_length(), 31.0);
        assert_eq!(pf.transposed(), PointF::new(-20.4, 10.6));

        assert_eq!(pf.to_i32(), Point::new(11, -20));

        assert_eq!(pf.truncate_to_i32(), Point::new(10, -20));

        let p = Point::new(3, 7);
        let pf2: PointF = p.into();
        assert_eq!(pf2, PointF::new(3.0, 7.0));

        let mut pf_assign = PointF::new(1.0, 2.0);
        pf_assign += PointF::new(3.0, 4.0);
        assert_eq!(pf_assign, PointF::new(4.0, 6.0));
        pf_assign -= PointF::new(1.0, 1.0);
        assert_eq!(pf_assign, PointF::new(3.0, 5.0));
        pf_assign *= 2.0;
        assert_eq!(pf_assign, PointF::new(6.0, 10.0));
        pf_assign /= 2.0;
        assert_eq!(pf_assign, PointF::new(3.0, 5.0));
        assert_eq!(-pf_assign, PointF::new(-3.0, -5.0));
    }

    #[test]
    fn test_size_basic_and_predicates() {
        let s = Size::new(100, 50);
        assert_eq!(s.width, 100);
        assert_eq!(s.height, 50);
        assert!(!s.is_empty());
        assert!(s.is_valid());
        assert!(!s.is_null());

        let empty_s = Size::new(0, 50);
        assert!(empty_s.is_empty());
        assert!(empty_s.is_valid());

        let invalid_s = Size::new(-5, 20);
        assert!(invalid_s.is_empty());
        assert!(!invalid_s.is_valid());

        assert_eq!(s.transposed(), Size::new(50, 100));
    }

    #[test]
    fn test_size_expanded_and_bounded() {
        let s1 = Size::new(100, 30);
        let s2 = Size::new(60, 80);

        assert_eq!(s1.expanded_to(s2), Size::new(100, 80));
        assert_eq!(s1.bounded_to(s2), Size::new(60, 30));

        let sf1 = SizeF::new(100.5, 30.2);
        let sf2 = SizeF::new(60.1, 80.8);
        assert_eq!(sf1.expanded_to(sf2), SizeF::new(100.5, 80.8));
        assert_eq!(sf1.bounded_to(sf2), SizeF::new(60.1, 30.2));
    }

    #[test]
    fn test_size_margins_grown_and_shrunk() {
        let s = Size::new(200, 100);
        let m = Margins::new(10, 20, 30, 40);

        let grown = s.grown_by(m);

        assert_eq!(grown, Size::new(240, 160));

        let shrunk = grown.shrunk_by(m);
        assert_eq!(shrunk, s);

        let sf = SizeF::new(200.0, 100.0);
        let mf = MarginsF::new(5.5, 10.5, 5.5, 10.5);
        assert_eq!(sf.grown_by(mf), SizeF::new(211.0, 121.0));
        assert_eq!(sf.grown_by(mf).shrunk_by(mf), sf);
    }

    #[test]
    fn test_rect_edges_and_corners() {
        let r = Rect::new(10, 20, 100, 50);
        assert_eq!(r.left(), 10);
        assert_eq!(r.top(), 20);
        assert_eq!(r.right(), 110);
        assert_eq!(r.bottom(), 70);

        assert_eq!(r.top_left(), Point::new(10, 20));
        assert_eq!(r.top_right(), Point::new(110, 20));
        assert_eq!(r.bottom_left(), Point::new(10, 70));
        assert_eq!(r.bottom_right(), Point::new(110, 70));
        assert_eq!(r.center(), Point::new(60, 45));
    }

    #[test]
    fn test_rect_contains_and_intersects() {
        let r = Rect::new(10, 20, 100, 50);

        assert!(r.contains(Point::new(10, 20)));
        assert!(r.contains(Point::new(50, 40)));
        assert!(!r.contains(Point::new(110, 70)));
        assert!(!r.contains(Point::new(110, 40)));
        assert!(!r.contains(Point::new(50, 70)));
        assert!(!r.contains(Point::new(9, 20)));
        assert!(!r.contains(Point::new(111, 70)));

        let r_overlap = Rect::new(50, 40, 100, 100);
        assert!(r.intersects(&r_overlap));

        let r_disjoint = Rect::new(200, 200, 50, 50);
        assert!(!r.intersects(&r_disjoint));
    }

    #[test]
    fn test_rect_intersected_and_united() {
        let r1 = Rect::new(10, 10, 100, 100);
        let r2 = Rect::new(50, 50, 100, 100);

        let inter = r1.intersected(&r2);
        assert_eq!(inter, Rect::new(50, 50, 60, 60));

        let uni = r1.united(&r2);
        assert_eq!(uni, Rect::new(10, 10, 140, 140));

        let disjoint = Rect::new(300, 300, 50, 50);
        assert_eq!(r1.intersected(&disjoint), Rect::default());

        // Origin-anchored pair: overlap, disjoint emptiness and bounding union.
        let o1 = Rect::new(0, 0, 100, 100);
        assert_eq!(o1.intersected(&r2), Rect::new(50, 50, 50, 50));
        assert!(o1.intersects(&r2));
        let far = Rect::new(200, 200, 50, 50);
        assert!(!o1.intersects(&far));
        assert!(o1.intersected(&far).is_empty());
        assert_eq!(o1.united(&r2), Rect::new(0, 0, 150, 150));
    }

    #[test]
    fn test_rect_adjust_margins_translate() {
        let r = Rect::new(10, 10, 100, 50);
        let m = Margins::new(5, 10, 15, 20);

        let added = r.margins_added(m);
        assert_eq!(added, Rect::new(5, 0, 120, 80));
        assert_eq!(added.margins_removed(m), r);

        let moved = r.translated(5, -5);
        assert_eq!(moved, Rect::new(15, 5, 100, 50));

        let adj = r.adjusted(1, 2, -1, -2);
        assert_eq!(adj, Rect::new(11, 12, 98, 46));
    }

    #[test]
    fn test_rect_f_operations() {
        let rf1 = RectF::new(10.0, 10.0, 100.0, 100.0);
        let rf2 = RectF::new(50.0, 50.0, 100.0, 100.0);

        assert_eq!(rf1.intersected(&rf2), RectF::new(50.0, 50.0, 60.0, 60.0));
        assert_eq!(rf1.united(&rf2), RectF::new(10.0, 10.0, 140.0, 140.0));
        assert!(rf1.contains(PointF::new(50.0, 50.0)));
        assert_eq!(
            rf1.translated(10.5, 20.5).to_i32(),
            Rect::new(21, 31, 100, 100)
        );
    }

    #[test]
    fn test_size_operators() {
        let s1 = Size::new(10, 20);
        let s2 = Size::new(5, 5);

        assert_eq!(s1 + s2, Size::new(15, 25));
        assert_eq!(s1 - s2, Size::new(5, 15));
        assert_eq!(s1 * 3, Size::new(30, 60));
        assert_eq!(s1 / 2, Size::new(5, 10));

        let sf = s1 * 1.5f32;
        assert_eq!(sf, SizeF::new(15.0, 30.0));

        let mut s_assign = Size::new(10, 10);
        s_assign += Size::new(5, 5);
        assert_eq!(s_assign, Size::new(15, 15));
        s_assign -= Size::new(3, 3);
        assert_eq!(s_assign, Size::new(12, 12));

        let mut sf_assign = SizeF::new(10.0, 20.0);
        sf_assign += SizeF::new(2.0, 3.0);
        assert_eq!(sf_assign, SizeF::new(12.0, 23.0));
        sf_assign -= SizeF::new(2.0, 3.0);

        assert_eq!(sf_assign, SizeF::new(10.0, 20.0));
        sf_assign *= 2.0;
        assert_eq!(sf_assign, SizeF::new(20.0, 40.0));
        sf_assign /= 4.0;
        assert_eq!(sf_assign, SizeF::new(5.0, 10.0));
    }

    #[test]
    fn test_margins_operators_and_null() {
        let m1 = Margins::new(10, 20, 30, 40);
        let m2 = Margins::new(1, 2, 3, 4);

        assert!(!m1.is_null());
        assert!(Margins::default().is_null());
        assert!(Margins::new(0, 0, 0, 0).is_null());

        assert_eq!(m1 + m2, Margins::new(11, 22, 33, 44));
        assert_eq!(m1 - m2, Margins::new(9, 18, 27, 36));

        let mut m_assign = m1;
        m_assign += m2;
        assert_eq!(m_assign, Margins::new(11, 22, 33, 44));
        m_assign -= m2;
        assert_eq!(m_assign, m1);

        let mf1 = MarginsF::new(10.5, 20.5, 30.5, 40.5);
        let mf2 = MarginsF::new(0.5, 0.5, 0.5, 0.5);
        assert_eq!(mf1 + mf2, MarginsF::new(11.0, 21.0, 31.0, 41.0));
        assert_eq!(mf1 - mf2, MarginsF::new(10.0, 20.0, 30.0, 40.0));
        assert_eq!(mf1.to_i32(), Margins::new(11, 21, 31, 41));

        let p1 = Margins::new(5, 10, 5, 10);
        let p2 = Margins::new(2, 2, 2, 2);
        assert_eq!(p1 + p2, Margins::new(7, 12, 7, 12));
        assert_eq!(p1 - p2, Margins::new(3, 8, 3, 8));
        assert!(!p1.is_null());
    }

    // =========================================================================

    // =========================================================================

    #[test]
    fn test_point_math_and_manhattan() {
        let p1 = Point::new(10, -20);
        let p2 = Point::new(5, 10);

        assert_eq!(p1 + p2, Point::new(15, -10));
        assert_eq!(p1 - p2, Point::new(5, -30));

        assert_eq!(p1.manhattan_length(), 30);

        assert_eq!(p1.transposed(), Point::new(-20, 10));

        let pf = p1.to_f32();
        assert_eq!(pf, PointF::new(10.0, -20.0));
    }

    #[test]
    fn test_size_layout_bounds() {
        let s1 = Size::new(100, 200);
        let s2 = Size::new(150, 120);

        // Returns size expanded to maximum components (`expandedTo`).
        assert_eq!(s1.expanded_to(s2), Size::new(150, 200));

        // Returns size bounded to minimum components (`boundedTo`).
        assert_eq!(s1.bounded_to(s2), Size::new(100, 120));

        let m = Margins::new(10, 20, 10, 20);
        assert_eq!(s1.grown_by(m), Size::new(120, 240));
        assert_eq!(s1.shrunk_by(m), Size::new(80, 160));
    }

    #[test]
    fn test_rect_half_open_boundaries_and_contains() {
        let r = Rect::new(10, 10, 100, 50);

        // Half-open interval convention: right = x + w, bottom = y + h
        assert_eq!(r.left(), 10);
        assert_eq!(r.top(), 10);
        assert_eq!(r.right(), 110);
        assert_eq!(r.bottom(), 60);

        // Center
        assert_eq!(r.center(), Point::new(60, 35));

        assert!(
            r.contains(Point::new(10, 10)),
            "Top-left point must be contained"
        );
        assert!(
            r.contains(Point::new(50, 30)),
            "Interior point must be contained"
        );

        assert!(
            !r.contains(Point::new(110, 30)),
            "Right edge must be excluded in half-open interval"
        );
        assert!(
            !r.contains(Point::new(50, 60)),
            "Bottom edge must be excluded in half-open interval"
        );
        assert!(
            !r.contains(Point::new(9, 10)),
            "Exterior point must be excluded"
        );
    }

    #[test]
    fn test_tiny_skia_conversions() {
        let rf = RectF::new(10.0, 20.0, 100.0, 50.0);
        let skia_rect: tiny_skia::Rect = rf.into();
        assert_eq!(skia_rect.x(), 10.0);
        assert_eq!(skia_rect.y(), 20.0);
        assert_eq!(skia_rect.width(), 100.0);
        assert_eq!(skia_rect.height(), 50.0);

        let pf = PointF::new(15.5, 25.5);
        let skia_point: tiny_skia::Point = pf.into();
        assert_eq!(skia_point.x, 15.5);
        assert_eq!(skia_point.y, 25.5);

        let r = Rect::new(5, 10, 50, 30);
        let skia_r: tiny_skia::Rect = r.into();
        assert_eq!(skia_r.x(), 5.0);
        assert_eq!(skia_r.width(), 50.0);
    }

    #[test]
    fn test_point_operations_and_manhattan() {
        let p1 = Point::new(10, 20);
        let p2 = Point::new(5, -10);
        assert_eq!(p1 + p2, Point::new(15, 10));

        let p3 = Point::new(3, -4);
        assert_eq!(p3.manhattan_length(), 7);
    }

    #[test]
    fn test_rect_half_open_boundaries() {
        let r = Rect::new(10, 20, 30, 40);
        assert_eq!(r.left(), 10);
        assert_eq!(r.top(), 20);
        assert_eq!(r.right(), 40);
        assert_eq!(r.bottom(), 60); // 20 + 40 = 60

        // Checks if point is contained in rectangle (`[left, right)` x `[top, bottom)`).
        // Checks if point is contained in rectangle (`[left, right)` x `[top, bottom)`).
        // Checks if point is contained in rectangle (`[left, right)` x `[top, bottom)`).
    }

    #[test]
    fn test_rect_union_and_intersection() {
        let r1 = Rect::new(0, 0, 50, 50);
        let r2 = Rect::new(30, 30, 50, 50);

        // Intersection
        let inter = r1.intersected(&r2);
        assert_eq!(inter, Rect::new(30, 30, 20, 20));

        // Bounding union
        let union = r1.united(&r2);
        assert_eq!(union, Rect::new(0, 0, 80, 80));
    }
    #[cfg(feature = "serde")]
    #[test]
    fn test_primitives_serde_roundtrip() {
        let p = Point::new(10, -20);
        let pf = PointF::new(1.5, 2.5);
        let s = Size::new(800, 600);
        let sf = SizeF::new(1920.0, 1080.0);
        let r = Rect::new(5, 10, 100, 200);
        let rf = RectF::new(0.5, 1.5, 99.5, 199.5);
        let m = Margins::new(1, 2, 3, 4);
        let mf = MarginsF::new(1.1, 2.2, 3.3, 4.4);

        let p_json = serde_json::to_string(&p).expect("serialize Point");
        assert_eq!(serde_json::from_str::<Point>(&p_json).unwrap(), p);

        let pf_json = serde_json::to_string(&pf).expect("serialize PointF");
        assert_eq!(serde_json::from_str::<PointF>(&pf_json).unwrap(), pf);

        let s_json = serde_json::to_string(&s).expect("serialize Size");
        assert_eq!(serde_json::from_str::<Size>(&s_json).unwrap(), s);

        let sf_json = serde_json::to_string(&sf).expect("serialize SizeF");
        assert_eq!(serde_json::from_str::<SizeF>(&sf_json).unwrap(), sf);

        let r_json = serde_json::to_string(&r).expect("serialize Rect");
        assert_eq!(serde_json::from_str::<Rect>(&r_json).unwrap(), r);

        let rf_json = serde_json::to_string(&rf).expect("serialize RectF");
        assert_eq!(serde_json::from_str::<RectF>(&rf_json).unwrap(), rf);

        let m_json = serde_json::to_string(&m).expect("serialize Margins");
        assert_eq!(serde_json::from_str::<Margins>(&m_json).unwrap(), m);

        let mf_json = serde_json::to_string(&mf).expect("serialize MarginsF");
        assert_eq!(serde_json::from_str::<MarginsF>(&mf_json).unwrap(), mf);
    }
}
