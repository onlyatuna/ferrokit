// SPDX-License-Identifier: LGPL-3.0-only
//! Offscreen double-buffering surface (`QBackingStore` equivalent).
//!
//! Encapsulates the top-level window backing store, separating window geometry updates
//! from buffer reallocation, supporting high-DPI rasterization, dirty region painting,
//! and static content optimization matching Qt's `QBackingStore` and `QWidgetRepaintManager`.

use std::ops::{Deref, DerefMut};

use crate::geometry::primitives::{RectF, Size};
use crate::geometry::region::Region;
use crate::paint::painter::Painter;
use crate::paint::pixmap::Pixmap;

/// Offscreen double-buffering backing store for a top-level window (`QBackingStore`).
///
/// Encapsulates the offscreen pixel buffer (`Pixmap`), tracking logical size,
/// device pixel ratio (DPR), paint lifecycle state, and static content regions.
#[derive(Clone, Debug)]
pub struct BackingStore {
    size: Size,
    dpr: f32,
    pixmap: Pixmap,
    static_contents: Option<Region>,
    is_painting: bool,
}

impl BackingStore {
    /// Creates a new backing store with the specified logical size and DPR.
    pub fn new(size: Size, dpr: f32) -> Option<Self> {
        let dpr = if dpr <= 0.0 { 1.0 } else { dpr };
        let phys_w = ((size.width.max(1) as f32) * dpr).round() as u32;
        let phys_h = ((size.height.max(1) as f32) * dpr).round() as u32;
        let pixmap = Pixmap::with_dpr(phys_w, phys_h, dpr)?;

        Some(Self {
            size,
            dpr,
            pixmap,
            static_contents: None,
            is_painting: false,
        })
    }

    /// Creates a backing store with default DPR of 1.0.
    pub fn with_size(size: Size) -> Option<Self> {
        Self::new(size, 1.0)
    }

    /// Returns the current logical size of the backing store surface (`QBackingStore::size()`).
    #[inline]
    pub fn size(&self) -> Size {
        self.size
    }

    /// Updates the logical size without immediately reallocating the underlying pixel buffer.
    ///
    /// The actual buffer reallocation occurs lazily during the paint phase if needed,
    /// matching Qt's `QWidgetRepaintManager::paintAndFlush()` behavior.
    #[inline]
    pub fn set_size(&mut self, size: Size) {
        self.size = size;
    }

    /// Returns the device pixel ratio.
    #[inline]
    pub fn device_pixel_ratio(&self) -> f32 {
        self.dpr
    }

    /// Sets the device pixel ratio without immediately reallocating.
    #[inline]
    pub fn set_device_pixel_ratio(&mut self, dpr: f32) {
        self.dpr = if dpr <= 0.0 { 1.0 } else { dpr };
    }

    /// Returns the physical width in native pixels.
    #[inline]
    pub fn physical_width(&self) -> u32 {
        self.pixmap.physical_width()
    }

    /// Returns the physical height in native pixels.
    #[inline]
    pub fn physical_height(&self) -> u32 {
        self.pixmap.physical_height()
    }

    /// Returns the native dimensions `(width, height)` in physical pixels.
    #[inline]
    pub fn native_size(&self) -> (u32, u32) {
        (self.physical_width(), self.physical_height())
    }

    /// Synchronizes buffer dimensions with requested logical size and DPR (Lazy Resize).
    ///
    /// Returns `true` if the backing store buffer was reallocated, or `false` if the
    /// existing buffer already matched the required dimensions and DPR.
    pub fn resize(&mut self, size: Size, dpr: f32) -> bool {
        self.size = size;
        self.dpr = if dpr <= 0.0 { 1.0 } else { dpr };
        let phys_w = ((size.width.max(1) as f32) * self.dpr).round() as u32;
        let phys_h = ((size.height.max(1) as f32) * self.dpr).round() as u32;
        self.pixmap.resize_with_dpr(phys_w, phys_h, self.dpr)
    }

    /// Like [`resize`](Self::resize), but sizes the buffer to the window's real native size when
    /// the platform knows it.
    ///
    /// `round(logical * dpr)` can be 1 px off the native window at fractional scale factors; the
    /// native size is used when it is within 2 px of that estimate (otherwise the estimate wins,
    /// e.g. the window is mid-way through a move to another screen).
    pub fn resize_to_native(&mut self, size: Size, dpr: f32, native: Option<(u32, u32)>) -> bool {
        self.size = size;
        self.dpr = if dpr <= 0.0 { 1.0 } else { dpr };
        let est_w = ((size.width.max(1) as f32) * self.dpr).round() as u32;
        let est_h = ((size.height.max(1) as f32) * self.dpr).round() as u32;
        let (phys_w, phys_h) = match native {
            Some((w, h)) if w.abs_diff(est_w) <= 2 && h.abs_diff(est_h) <= 2 => (w, h),
            _ => (est_w, est_h),
        };
        self.pixmap.resize_with_dpr(phys_w, phys_h, self.dpr)
    }

    /// Returns a reference to the underlying raster pixmap.
    #[inline]
    pub fn pixmap(&self) -> &Pixmap {
        &self.pixmap
    }

    /// Returns a mutable reference to the underlying raster pixmap.
    #[inline]
    pub fn pixmap_mut(&mut self) -> &mut Pixmap {
        &mut self.pixmap
    }

    /// Returns the paint device for raster operations (`QBackingStore::paintDevice()`).
    #[inline]
    pub fn paint_device(&mut self) -> &mut Pixmap {
        &mut self.pixmap
    }

    /// Begins painting on the backing store for the specified dirty region (`QBackingStore::beginPaint`).
    ///
    /// The returned `Painter` is clipped to the bounding rectangle of the dirty region.
    pub fn begin_paint(&mut self, dirty_region: &Region) -> Painter<'_> {
        self.is_painting = true;
        let mut painter = Painter::begin(&mut self.pixmap);
        let bounds = dirty_region.bounding_rect();
        painter.set_clip_rect(RectF::new(
            bounds.x as f32,
            bounds.y as f32,
            bounds.width as f32,
            bounds.height as f32,
        ));
        painter
    }

    /// Ends the current painting session on the backing store (`QBackingStore::endPaint`).
    pub fn end_paint(&mut self) {
        self.is_painting = false;
    }

    /// Returns whether painting is currently in progress.
    #[inline]
    pub fn is_painting(&self) -> bool {
        self.is_painting
    }

    /// Returns the static contents region if set (`QBackingStore::staticContents()`).
    #[inline]
    pub fn static_contents(&self) -> Option<&Region> {
        self.static_contents.as_ref()
    }

    /// Sets the static contents region for optimization during window resize (`QBackingStore::setStaticContents()`).
    pub fn set_static_contents(&mut self, region: Option<Region>) {
        self.static_contents = region;
    }

    /// Returns `true` if static contents are defined and non-empty.
    #[inline]
    pub fn has_static_contents(&self) -> bool {
        self.static_contents
            .as_ref()
            .is_some_and(|r| !r.is_empty())
    }

    /// Scrolls the specified area by `(dx, dy)` (`QBackingStore::scroll()`).
    ///
    /// Returns `true` if pixels were scrolled (or the offset is zero), or `false` if nothing
    /// remained after clipping to the buffer and a full repaint is required.
    pub fn scroll(&mut self, area: &Region, dx: i32, dy: i32) -> bool {
        if dx == 0 && dy == 0 {
            return true;
        }
        // Pixel scrolling on the underlying buffer
        let rect = area.bounding_rect();
        let src_rect = crate::geometry::primitives::Rect::new(
            (rect.x as f32 * self.dpr).round() as i32,
            (rect.y as f32 * self.dpr).round() as i32,
            (rect.width as f32 * self.dpr).round() as i32,
            (rect.height as f32 * self.dpr).round() as i32,
        );
        let phys_dx = (dx as f32 * self.dpr).round() as i32;
        let phys_dy = (dy as f32 * self.dpr).round() as i32;

        // i64 so i32::MIN shifts and huge rects cannot overflow.
        let w = self.pixmap.physical_width() as i64;
        let h = self.pixmap.physical_height() as i64;
        let (phys_dx, phys_dy) = (phys_dx as i64, phys_dy as i64);

        let left = (src_rect.x as i64).max(0).max(-phys_dx);
        let top = (src_rect.y as i64).max(0).max(-phys_dy);
        let right = (src_rect.x as i64 + src_rect.width as i64).min(w).min(w - phys_dx);
        let bottom = (src_rect.y as i64 + src_rect.height as i64).min(h).min(h - phys_dy);
        if left >= right || top >= bottom {
            return false;
        }
        let width = right - left;
        let height = bottom - top;
        let dst_x = left + phys_dx;
        let dst_y = top + phys_dy;
        let (w, phys_dy) = (w as i32, phys_dy as i32);
        let (left, top, width, height, dst_x, dst_y) =
            (left as i32, top as i32, width as i32, height as i32, dst_x as i32, dst_y as i32);
        let dst_slice = self.pixmap.data_mut();
        let bpp = 4;
        let stride = w as usize * bpp;

        if phys_dy > 0 {
            for row in (0..height).rev() {
                let s_start = ((top + row) as usize) * stride + (left as usize) * bpp;
                let d_start = ((dst_y + row) as usize) * stride + (dst_x as usize) * bpp;
                let len = width as usize * bpp;
                dst_slice.copy_within(s_start..s_start + len, d_start);
            }
        } else {
            for row in 0..height {
                let s_start = ((top + row) as usize) * stride + (left as usize) * bpp;
                let d_start = ((dst_y + row) as usize) * stride + (dst_x as usize) * bpp;
                let len = width as usize * bpp;
                dst_slice.copy_within(s_start..s_start + len, d_start);
            }
        }

        true
    }
}

impl Deref for BackingStore {
    type Target = Pixmap;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.pixmap
    }
}

impl DerefMut for BackingStore {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.pixmap
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::primitives::Rect;

    #[test]
    fn resize_to_native_keeps_the_window_pixel_size_at_fractional_dpr() {
        // 125%: native 427 rows -> logical 342 -> round(342 * 1.25) = 428, 1 px too tall.
        let mut bs = BackingStore::new(Size::new(100, 100), 1.25).unwrap();
        assert!(bs.resize_to_native(Size::new(828, 342), 1.25, Some((1035, 427))));
        assert_eq!(bs.native_size(), (1035, 427));
        // Unknown native size: falls back to the estimate.
        assert!(bs.resize_to_native(Size::new(828, 342), 1.25, None));
        assert_eq!(bs.native_size(), (1035, 428));
        // A native size far from the estimate (e.g. mid screen change) is ignored.
        bs.resize_to_native(Size::new(828, 342), 1.25, Some((2000, 900)));
        assert_eq!(bs.native_size(), (1035, 428));
    }

    #[test]
    fn test_backing_store_creation_and_dpr() {
        let bs = BackingStore::new(Size::new(200, 100), 2.0).expect("failed to create BackingStore");
        assert_eq!(bs.size(), Size::new(200, 100));
        assert_eq!(bs.device_pixel_ratio(), 2.0);
        assert_eq!(bs.physical_width(), 400);
        assert_eq!(bs.physical_height(), 200);
        assert_eq!(bs.native_size(), (400, 200));
    }

    #[test]
    fn test_backing_store_lazy_resize() {
        let mut bs = BackingStore::new(Size::new(100, 100), 1.0).unwrap();
        assert_eq!(bs.physical_width(), 100);

        // Same size -> no reallocation
        assert!(!bs.resize(Size::new(100, 100), 1.0));

        // Different size -> reallocates
        assert!(bs.resize(Size::new(200, 150), 1.0));
        assert_eq!(bs.physical_width(), 200);
        assert_eq!(bs.physical_height(), 150);
        assert_eq!(bs.size(), Size::new(200, 150));
    }

    #[test]
    fn test_backing_store_paint_lifecycle() {
        let mut bs = BackingStore::new(Size::new(100, 100), 1.0).unwrap();
        assert!(!bs.is_painting());

        let dirty = Region::from_rect(Rect::new(10, 10, 50, 50));
        {
            let mut painter = bs.begin_paint(&dirty);
            painter.fill_rect(
                RectF::new(10.0, 10.0, 50.0, 50.0),
                tiny_skia::Color::from_rgba8(255, 0, 0, 255),
            );
        }
        assert!(bs.is_painting());
        bs.end_paint();
        assert!(!bs.is_painting());
    }

    #[test]
    fn test_backing_store_static_contents() {
        let mut bs = BackingStore::new(Size::new(100, 100), 1.0).unwrap();
        assert!(!bs.has_static_contents());

        let region = Region::from_rect(Rect::new(0, 0, 50, 50));
        bs.set_static_contents(Some(region));
        assert!(bs.has_static_contents());
    }

    #[test]
    fn test_backing_store_scroll() {
        let mut bs = BackingStore::new(Size::new(100, 100), 1.0).unwrap();
        let area = Region::from_rect(Rect::new(0, 0, 50, 50));
        assert!(bs.scroll(&area, 10, 10));
    }
    #[test]
    fn scroll_clips_negative_source_rect() {
        let mut bs = BackingStore::new(Size::new(100, 100), 1.0).unwrap();
        let area = Region::from_rect(Rect::new(-10, 0, 50, 50));
        assert!(bs.scroll(&area, 20, 0));
    }

    #[test]
    fn scroll_clips_source_rect_past_right_edge() {
        let mut bs = BackingStore::new(Size::new(100, 100), 1.0).unwrap();
        let area = Region::from_rect(Rect::new(80, 0, 50, 10));
        assert!(bs.scroll(&area, -40, 0));
    }

    /// Pixel (x, y) holds [x, y, 0, 255], so every source pixel is unique.
    fn coordinate_store(w: u32, h: u32, dpr: f32) -> BackingStore {
        let mut bs =
            BackingStore::new(Size::new((w as f32 / dpr) as i32, (h as f32 / dpr) as i32), dpr).unwrap();
        let stride = bs.physical_width() as usize * 4;
        for (i, px) in bs.data_mut().chunks_exact_mut(4).enumerate() {
            px.copy_from_slice(&[(i % (stride / 4)) as u8, (i / (stride / 4)) as u8, 0, 255]);
        }
        bs
    }

    fn px(bs: &BackingStore, x: u32, y: u32) -> [u8; 4] {
        let c = bs.pixel(x, y).unwrap();
        [c.red(), c.green(), c.blue(), c.alpha()]
    }

    #[test]
    #[cfg_attr(miri, ignore = "per-pixel loops over a full buffer take minutes under Miri; safe arithmetic only")]
    fn scroll_right_moves_pixels_and_leaves_the_rest() {
        let mut bs = coordinate_store(100, 100, 1.0);
        assert!(bs.scroll(&Region::from_rect(Rect::new(0, 0, 50, 50)), 10, 0));
        for y in 0..50 {
            for x in 10..60 {
                assert_eq!(px(&bs, x, y), [(x - 10) as u8, y as u8, 0, 255], "({x},{y})");
            }
            // Vacated strip and everything outside the destination keep their old content.
            assert_eq!(px(&bs, 5, y), [5, y as u8, 0, 255]);
            assert_eq!(px(&bs, 70, y), [70, y as u8, 0, 255]);
        }
        assert_eq!(px(&bs, 20, 60), [20, 60, 0, 255]);
    }

    #[test]
    #[cfg_attr(miri, ignore = "per-pixel loops over a full buffer take minutes under Miri; safe arithmetic only")]
    fn scroll_down_overlapping_copies_from_the_original() {
        let mut bs = coordinate_store(100, 100, 1.0);
        assert!(bs.scroll(&Region::from_rect(Rect::new(0, 0, 20, 50)), 0, 3));
        for y in 3..53 {
            assert_eq!(px(&bs, 7, y), [7, (y - 3) as u8, 0, 255], "y={y}");
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "per-pixel loops over a full buffer take minutes under Miri; safe arithmetic only")]
    fn scroll_up_and_left_overlapping_copies_from_the_original() {
        let mut bs = coordinate_store(100, 100, 1.0);
        assert!(bs.scroll(&Region::from_rect(Rect::new(10, 10, 40, 40)), -4, -3));
        for y in 7..47 {
            for x in 6..46 {
                assert_eq!(px(&bs, x, y), [(x + 4) as u8, (y + 3) as u8, 0, 255], "({x},{y})");
            }
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "per-pixel loops over a full buffer take minutes under Miri; safe arithmetic only")]
    fn scroll_with_negative_source_clips_to_the_buffer() {
        let mut bs = coordinate_store(100, 100, 1.0);
        assert!(bs.scroll(&Region::from_rect(Rect::new(-10, 0, 50, 50)), 20, 0));
        for x in 20..60 {
            assert_eq!(px(&bs, x, 3), [(x - 20) as u8, 3, 0, 255], "x={x}");
        }
        assert_eq!(px(&bs, 10, 3), [10, 3, 0, 255]);
    }

    #[test]
    #[cfg_attr(miri, ignore = "per-pixel loops over a full buffer take minutes under Miri; safe arithmetic only")]
    fn scroll_with_source_past_right_edge_clips_to_the_buffer() {
        let mut bs = coordinate_store(100, 100, 1.0);
        assert!(bs.scroll(&Region::from_rect(Rect::new(80, 0, 50, 10)), -40, 0));
        for y in 0..10 {
            for x in 40..60 {
                assert_eq!(px(&bs, x, y), [(x + 40) as u8, y as u8, 0, 255], "({x},{y})");
            }
        }
        // The next row's start must not have been pulled into the last row.
        assert_eq!(px(&bs, 60, 9), [60, 9, 0, 255]);
    }

    #[test]
    #[cfg_attr(miri, ignore = "per-pixel loops over a full buffer take minutes under Miri; safe arithmetic only")]
    fn scroll_scales_logical_offsets_by_dpr() {
        let mut bs = coordinate_store(200, 200, 2.0);
        assert!(bs.scroll(&Region::from_rect(Rect::new(0, 0, 25, 25)), 5, 0));
        // Logical (0,0,25,25) is physical (0,0,50,50); dx 5 is 10 physical px.
        for x in 10..60 {
            assert_eq!(px(&bs, x, 4), [(x - 10) as u8, 4, 0, 255], "x={x}");
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "per-pixel loops over a full buffer take minutes under Miri; safe arithmetic only")]
    fn scroll_entirely_outside_is_a_no_op() {
        let mut bs = coordinate_store(100, 100, 1.0);
        // Nothing survives clipping, so no pixels moved: report false.
        assert!(!bs.scroll(&Region::from_rect(Rect::new(0, 0, 10, 10)), 500, 0));
        assert_eq!(px(&bs, 3, 3), [3, 3, 0, 255]);
        assert_eq!(px(&bs, 99, 9), [99, 9, 0, 255]);
    }

    #[test]
    fn scroll_of_rect_outside_the_buffer_returns_false() {
        let mut bs = BackingStore::new(Size::new(100, 100), 1.0).unwrap();
        assert!(!bs.scroll(&Region::from_rect(Rect::new(200, 200, 10, 10)), 1, 1));
        assert!(!bs.scroll(&Region::from_rect(Rect::new(0, 0, 0, 0)), 1, 1));
        // A zero offset is trivially satisfied.
        assert!(bs.scroll(&Region::from_rect(Rect::new(200, 200, 10, 10)), 0, 0));
    }
}
