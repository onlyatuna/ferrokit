// SPDX-License-Identifier: LGPL-3.0-only
//! Glyph source of a font engine: character -> glyph mapping, glyph metrics and coverage bitmaps.
//!
//! Mirrors the glyph half of Qt's `QFontEngine` interface. Qt keeps one backend per platform
//! (DirectWrite/GDI on Windows, CoreText on macOS, FreeType elsewhere) behind that interface and
//! asks it for one glyph at a time; [`parse_face`] is the single place that chooses the backend
//! (Qt's `QPlatformFontDatabase::fontEngine` / `createEngine`).

use crate::text::font::SharedFontData;
use crate::text::outline_face::OutlineFace;
use std::sync::Arc;

/// Placement and size of a rasterized glyph, in pixels at the requested size.
///
/// `xmin`/`ymin` are the offset of the bitmap's left edge and bottom edge from the pen position
/// (y grows upwards), so the bitmap's top-left lands at `(x + xmin, baseline - ymin - height)`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct GlyphMetrics {
    pub xmin: i32,
    pub ymin: i32,
    pub width: usize,
    pub height: usize,
    pub advance_width: f32,
    pub advance_height: f32,
}

/// One face of a font as seen by the rasterizer.
pub trait GlyphFace: Send + Sync {
    /// Glyph index for `ch`, or 0 when the face has no glyph for it.
    fn glyph_index(&self, ch: char) -> u16;

    /// Metrics of `glyph_id` at `px` pixels per em, without rasterizing.
    fn metrics_indexed(&self, glyph_id: u16, px: f32) -> GlyphMetrics;

    /// Metrics and 8-bit coverage bitmap (row-major, top row first) of `glyph_id` at `px`.
    fn rasterize_indexed(&self, glyph_id: u16, px: f32) -> (GlyphMetrics, Vec<u8>);

    /// Like [`rasterize_indexed`](Self::rasterize_indexed) for a font of `size` pixels drawn
    /// through a uniform scale (the device pixel ratio), as Qt does: the glyph run keeps its logical
    /// em size and the transform scales it. Backends that hint or pick a rendering mode from the
    /// logical size override this; the default simply rasterizes at `size * scale`.
    fn rasterize_scaled(&self, glyph_id: u16, size: f32, scale: f32) -> (GlyphMetrics, Vec<u8>) {
        self.rasterize_indexed(glyph_id, size * scale)
    }

    /// Sub-pixel (LCD) coverage of the glyph, as [`rasterize_scaled`](Self::rasterize_scaled)
    /// places it: three bytes (red, green, blue sub-pixel) per pixel, top row first. `None` when
    /// the backend only has grey-scale coverage (Qt's `alphaRGBMapForGlyph` fallback).
    fn rasterize_lcd_scaled(
        &self,
        _glyph_id: u16,
        _size: f32,
        _scale: f32,
    ) -> Option<(GlyphMetrics, Vec<u8>)> {
        None
    }

    /// Metrics of the glyph for `ch` at `px`.
    fn metrics(&self, ch: char, px: f32) -> GlyphMetrics {
        self.metrics_indexed(self.glyph_index(ch), px)
    }

    /// Unrounded `(ascent, descent)` in pixels at `px` per em, both positive, as the platform
    /// font engine reports them (Qt's `QFontEngine::ascent` / `descent` before rounding), or
    /// `None` when the backend has no such data.
    fn vertical_metrics(&self, _px: f32) -> Option<(f32, f32)> {
        None
    }

    /// Unrounded cap height in pixels at `px` per em (the OS/2 `sCapHeight` Qt's Windows engines
    /// report as `QFontEngine::capHeight`), or `None` when the face has none.
    fn cap_height(&self, _px: f32) -> Option<f32> {
        None
    }

    /// The unhinted outline of the glyph at `px` per em, in pixels with the origin on the baseline
    /// and y growing downwards (what `QFontEngine::addGlyphsToPath` adds to a `QPainterPath`), or
    /// `None` when the backend cannot produce outlines.
    fn glyph_outline(&self, _glyph_id: u16, _px: f32) -> Option<tiny_skia::Path> {
        None
    }

    /// The advance Qt's Windows font engine gives a glyph in layout (see
    /// `DirectWriteFace::layout_advance_width`); `direct_write` is
    /// [`uses_directwrite_engine`](crate::text::font_database::uses_directwrite_engine).
    /// `None` when the backend has no such metrics: the caller then keeps HarfBuzz's own advance.
    fn layout_advance_width(&self, _glyph_id: u16, _size: f32, _direct_write: bool) -> Option<f32> {
        None
    }

    /// The right side bearing of a glyph in pixels at `size` per em: the advance minus the left
    /// side bearing minus the ink width, so negative where the ink hangs over the advance. This is
    /// the `rightBearing` of the `glyph_metrics_t` Qt's Windows engines build from
    /// `GetDesignGlyphMetrics` (`QWindowsFontEngineDirectWrite::boundingBox(glyph_t)`), scaled with
    /// `DESIGN_TO_LOGICAL` (a 26.6 value truncated toward zero). `None` when the backend has no
    /// such metrics: the caller then counts no bearing.
    fn right_side_bearing(&self, _glyph_id: u16, _size: f32) -> Option<f32> {
        None
    }
}

/// Largest glyph bitmap (in pixels) any rasterizer backend will allocate.
pub(crate) const MAX_GLYPH_PIXELS: usize = 16 * 1024 * 1024;

/// Whether a `width` x `height` glyph bitmap is too large to allocate.
pub(crate) fn exceeds_glyph_budget(width: usize, height: usize) -> bool {
    width.checked_mul(height).map_or(true, |n| n > MAX_GLYPH_PIXELS)
}

#[cfg(test)]
mod budget_tests {
    use super::*;

    #[test]
    fn glyph_budget_rejects_crafted_bounding_boxes_without_allocating() {
        assert!(!exceeds_glyph_budget(64, 64));
        assert!(!exceeds_glyph_budget(4096, 4096));
        assert!(exceeds_glyph_budget(4097, 4096));
        assert!(exceeds_glyph_budget(49_152, 49_152));
        assert!(exceeds_glyph_budget(usize::MAX, 2));
    }

    #[test]
    fn glyph_budget_rejects_directwrite_texture_bounds_from_i32_extremes() {
        // DirectWrite texture bounds are i32 rectangles; the widest possible one must be refused.
        let span = (i32::MAX as i64 - i32::MIN as i64) as usize;
        assert!(exceeds_glyph_budget(span, 1));
        assert!(exceeds_glyph_budget(span, span));
    }
}

/// Shared handle to a face (Qt's ref-counted `QFontEngine*`).
pub type SharedGlyphFace = Arc<dyn GlyphFace>;

/// Which rasterizer backend [`parse_face`] uses (Qt's per-platform font engine choice).
///
/// Windows uses DirectWrite, like Qt; every other platform, and Windows without DirectWrite, uses
/// [`OutlineFace`] (Qt's FreeType role). `FERROKIT_FONT_ENGINE=outline` forces the generic backend
/// (Qt's `QT_QPA_PLATFORM=windows:fontengine=freetype`), for comparing the two.
#[cfg(windows)]
fn use_directwrite() -> bool {
    static CHOICE: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| {
        let forced_generic = std::env::var("FERROKIT_FONT_ENGINE").is_ok_and(|v| v.eq_ignore_ascii_case("outline"));
        !forced_generic && crate::text::directwrite_face::is_available()
    });
    *CHOICE
}

/// Builds the glyph face for face `face_index` of the font file `data`.
///
/// This is the only place that selects a rasterizer backend.
pub fn parse_face(data: &SharedFontData, face_index: u32) -> Result<SharedGlyphFace, String> {
    #[cfg(windows)]
    if use_directwrite() {
        match crate::text::directwrite_face::DirectWriteFace::new(data.clone(), face_index) {
            Ok(face) => return Ok(Arc::new(face)),
            // A font DirectWrite cannot open is still drawn by the generic backend.
            Err(_) => {}
        }
    }
    OutlineFace::new(data.clone(), face_index).map(|f| Arc::new(f) as SharedGlyphFace)
}
