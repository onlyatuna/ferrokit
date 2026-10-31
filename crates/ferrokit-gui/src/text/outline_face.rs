// SPDX-License-Identifier: LGPL-3.0-only
//! Generic, cross-platform glyph backend: `ttf-parser` outlines filled with exact area coverage.
//!
//! This plays the role of Qt's FreeType engine (`QFontEngineFT`): the font file is kept as
//! plain bytes, the character map and a glyph's outline are read from it only when asked for, and
//! a glyph is rasterized individually. Nothing is built per glyph of the font up front, unlike
//! `fontdue::Font::from_bytes`, which converts every glyph outline eagerly (hundreds of MB for a
//! CJK font).
//!
//! Metrics and placement follow `fontdue` exactly (same unhinted outline bounds, same pixel
//! rounding), so layout does not change when the backend does (checked against `fontdue` in
//! `tests/test_outline_face_parity.rs`).

use crate::text::font::SharedFontData;
use crate::text::glyph_face::{exceeds_glyph_budget, GlyphFace, GlyphMetrics};
use ab_glyph_rasterizer::{point, Point, Rasterizer};
use rustybuzz::ttf_parser::{Face, GlyphId, OutlineBuilder};

/// A font face read straight from the font file bytes.
pub struct OutlineFace {
    // Field order matters: `face` borrows from `_data` and must be dropped first.
    face: Face<'static>,
    units_per_em: f32,
    /// Design right side bearing, in font units, of every glyph asked for so far (see
    /// [`GlyphFace::right_side_bearing`]): reading it takes the glyph header, and a text asks for
    /// the same few glyphs again for every layout. At most one entry per glyph of the font.
    bearings: std::sync::Mutex<std::collections::HashMap<u16, f64>>,
    _data: SharedFontData,
}

impl OutlineFace {
    /// Parses face `face_index` of the font file `data` (reads the table directory only).
    pub fn new(data: SharedFontData, face_index: u32) -> Result<Self, String> {
        let bytes: &[u8] = data.as_slice();
        // SAFETY: `bytes` points into the heap buffer of the `Arc<Vec<u8>>` inside `data`. That
        // buffer is never mutated or reallocated while a clone of the `Arc` exists, and `_data`
        // keeps one alive for as long as `face` does. `face` is declared before `_data`, so it
        // is dropped first, and it is never handed out with the extended lifetime.
        let bytes: &'static [u8] = unsafe { std::mem::transmute::<&[u8], &'static [u8]>(bytes) };
        let face =
            Face::parse(bytes, face_index).map_err(|e| format!("Failed to parse font: {e}"))?;
        let units_per_em = face.units_per_em() as f32;
        if units_per_em <= 0.0 {
            return Err("Font has no units-per-em".to_string());
        }
        Ok(Self {
            face,
            units_per_em,
            bearings: std::sync::Mutex::new(std::collections::HashMap::new()),
            _data: data,
        })
    }

    /// Unrounded `(ascent, descent)` at `px`: DirectWrite's (`windows` = true) are the OS/2
    /// typographic or Windows metrics, FreeType's are the `hhea` ones.
    pub(crate) fn line_metrics(&self, px: f32, windows: bool) -> (f32, f32) {
        let (asc, desc) = match self.face.tables().os2 {
            Some(os2) if windows && os2.use_typographic_metrics() => {
                (os2.typographic_ascender(), os2.typographic_descender())
            }
            Some(os2) if windows => (os2.windows_ascender(), os2.windows_descender()),
            _ => (self.face.ascender(), self.face.descender()),
        };
        let scale = px / self.units_per_em;
        (asc as f32 * scale, (-(desc as f32)).max(0.0) * scale)
    }

    /// Placement of `glyph_id` at `px`, plus the sub-pixel offsets the outline is drawn with.
    fn layout(&self, glyph_id: u16, px: f32) -> Layout {
        let scale = px / self.units_per_em;
        let gid = GlyphId(glyph_id);
        let (bx, by, bw, bh) = match self.face.glyph_bounding_box(gid) {
            Some(r) => (
                r.x_min as f32 * scale,
                r.y_min as f32 * scale,
                (r.x_max as f32 - r.x_min as f32) * scale,
                (r.y_max as f32 - r.y_min as f32) * scale,
            ),
            None => (0.0, 0.0, 0.0, 0.0),
        };
        // fontdue's `metrics_raw`.
        let mut offset_x = fract(bx);
        let mut offset_y = fract(1.0 - fract(bh) - fract(by));
        if offset_x < 0.0 {
            offset_x += 1.0;
        }
        if offset_y < 0.0 {
            offset_y += 1.0;
        }
        let advance_width = self.face.glyph_hor_advance(gid).unwrap_or(0) as f32 * scale;
        let advance_height = self.face.glyph_ver_advance(gid).unwrap_or(0) as f32 * scale;
        Layout {
            metrics: GlyphMetrics {
                xmin: bx.floor() as i32,
                ymin: by.floor() as i32,
                width: (bw + offset_x).ceil() as i32 as usize,
                height: (bh + offset_y).ceil() as i32 as usize,
                advance_width,
                advance_height,
            },
            scale,
            origin_x: -bx + offset_x,
            origin_y: by + bh + offset_y,
        }
    }
}

struct Layout {
    metrics: GlyphMetrics,
    scale: f32,
    /// Pixel column of font-unit x = 0.
    origin_x: f32,
    /// Pixel row of font-unit y = 0 (rows grow downwards).
    origin_y: f32,
}

#[inline]
fn fract(x: f32) -> f32 {
    x - x.trunc()
}

impl GlyphFace for OutlineFace {
    fn glyph_index(&self, ch: char) -> u16 {
        self.face.glyph_index(ch).map_or(0, |g| g.0)
    }

    /// FreeType's view: the `hhea` ascender and descender.
    fn vertical_metrics(&self, px: f32) -> Option<(f32, f32)> {
        Some(self.line_metrics(px, false))
    }

    fn cap_height(&self, px: f32) -> Option<f32> {
        let cap = self
            .face
            .tables()
            .os2?
            .capital_height()
            .filter(|c| *c > 0)?;
        Some(cap as f32 * px / self.units_per_em)
    }

    fn glyph_outline(&self, glyph_id: u16, px: f32) -> Option<tiny_skia::Path> {
        let mut sink = crate::text::color_glyph::SkiaPathBuilder {
            builder: tiny_skia::PathBuilder::new(),
        };
        self.face.outline_glyph(GlyphId(glyph_id), &mut sink)?;
        let scale = px / self.units_per_em;
        sink.builder
            .finish()?
            .transform(tiny_skia::Transform::from_row(
                scale, 0.0, 0.0, -scale, 0.0, 0.0,
            ))
    }

    /// `rightSideBearing` of DirectWrite's `GetDesignGlyphMetrics` (advance minus the `hmtx` left
    /// side bearing minus the ink width, in font units), converted as Qt's `DESIGN_TO_LOGICAL`
    /// does: `design / unitsPerEm * size`, truncated toward zero to 1/64 px. Checked against Qt
    /// (PySide6 6.11.2, Windows) for every Latin-1 glyph of Arial, Arial Italic, Times New Roman
    /// (and italic), Segoe UI (and italic), Consolas, Calibri, Verdana and Georgia at 11, 13, 16,
    /// 24 and 31 px: 9450 glyphs, no difference. The advance minus the glyph box edge (`xMax`) is
    /// not the same where `hmtx` and `glyf` disagree on the left side bearing, and differs from Qt
    /// in 60 of them.
    fn right_side_bearing(&self, glyph_id: u16, size: f32) -> Option<f32> {
        let mut known = self.bearings.lock().unwrap_or_else(|e| e.into_inner());
        let design = match known.get(&glyph_id) {
            Some(&design) => design,
            None => {
                let gid = GlyphId(glyph_id);
                // A glyph without a contour has no ink to hang over anything.
                let design = match self.face.glyph_bounding_box(gid) {
                    None => 0.0,
                    Some(bbox) => {
                        let advance = f64::from(self.face.glyph_hor_advance(gid)?);
                        let left = f64::from(self.face.glyph_hor_side_bearing(gid)?);
                        advance - left - (f64::from(bbox.x_max) - f64::from(bbox.x_min))
                    }
                };
                known.insert(glyph_id, design);
                design
            }
        };
        let pixels = design / f64::from(self.units_per_em) * f64::from(size);
        Some(((pixels * 64.0).trunc() / 64.0) as f32)
    }

    fn metrics_indexed(&self, glyph_id: u16, px: f32) -> GlyphMetrics {
        self.layout(glyph_id, px).metrics
    }

    fn rasterize_indexed(&self, glyph_id: u16, px: f32) -> (GlyphMetrics, Vec<u8>) {
        if px <= 0.0 {
            return (GlyphMetrics::default(), Vec::new());
        }
        let layout = self.layout(glyph_id, px);
        let mut m = layout.metrics;
        if m.width == 0 || m.height == 0 {
            return (m, Vec::new());
        }
        if exceeds_glyph_budget(m.width, m.height) {
            // Crafted bounding box: drop the glyph instead of allocating gigabytes.
            m.width = 0;
            m.height = 0;
            return (m, Vec::new());
        }

        let mut sink = CoverageSink {
            raster: Rasterizer::new(m.width, m.height),
            scale: layout.scale,
            origin_x: layout.origin_x,
            origin_y: layout.origin_y,
            start: point(0.0, 0.0),
            current: point(0.0, 0.0),
        };
        if self
            .face
            .outline_glyph(GlyphId(glyph_id), &mut sink)
            .is_none()
        {
            return (m, vec![0; m.width * m.height]);
        }

        let mut bitmap = vec![0u8; m.width * m.height];
        sink.raster.for_each_pixel(|i, coverage| {
            bitmap[i] = (coverage.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        });
        (m, bitmap)
    }
}

/// Receives a glyph outline in font units and accumulates exact pixel coverage.
struct CoverageSink {
    raster: Rasterizer,
    scale: f32,
    origin_x: f32,
    origin_y: f32,
    start: Point,
    current: Point,
}

impl CoverageSink {
    /// Font units (y up) -> bitmap pixels (y down).
    #[inline]
    fn map(&self, x: f32, y: f32) -> Point {
        point(
            x * self.scale + self.origin_x,
            self.origin_y - y * self.scale,
        )
    }
}

/// Largest distance a flattened curve may stray from the true curve, in pixels.
const FLATTEN_TOLERANCE: f32 = 0.03;
const MAX_SEGMENTS: usize = 64;

/// Number of equal-parameter pieces so that `error_at_one_piece / n^2 <= FLATTEN_TOLERANCE`.
#[inline]
fn segments(error_at_one_piece: f32) -> usize {
    ((error_at_one_piece / FLATTEN_TOLERANCE).sqrt().ceil() as usize).clamp(1, MAX_SEGMENTS)
}

#[inline]
fn hypot(x: f32, y: f32) -> f32 {
    (x * x + y * y).sqrt()
}

#[inline]
fn lerp(t: f32, a: Point, b: Point) -> Point {
    point(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
}

impl OutlineBuilder for CoverageSink {
    fn move_to(&mut self, x: f32, y: f32) {
        self.start = self.map(x, y);
        self.current = self.start;
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.map(x, y);
        self.raster.draw_line(self.current, p);
        self.current = p;
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let (c, p) = (self.map(x1, y1), self.map(x, y));
        let p0 = self.current;
        // Chord error of a quadratic split into n pieces is |p0 - 2c + p| / (4 n^2).
        let dd = hypot(p0.x - 2.0 * c.x + p.x, p0.y - 2.0 * c.y + p.y);
        let n = segments(dd * 0.25);
        let mut prev = p0;
        for i in 1..n {
            let t = i as f32 / n as f32;
            let q = lerp(t, lerp(t, p0, c), lerp(t, c, p));
            self.raster.draw_line(prev, q);
            prev = q;
        }
        self.raster.draw_line(prev, p);
        self.current = p;
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let (c1, c2, p) = (self.map(x1, y1), self.map(x2, y2), self.map(x, y));
        let p0 = self.current;
        // Chord error of a cubic split into n pieces is at most 3/4 * max|second difference| / n^2.
        let d1 = hypot(p0.x - 2.0 * c1.x + c2.x, p0.y - 2.0 * c1.y + c2.y);
        let d2 = hypot(c1.x - 2.0 * c2.x + p.x, c1.y - 2.0 * c2.y + p.y);
        let n = segments(0.75 * d1.max(d2));
        let mut prev = p0;
        for i in 1..n {
            let t = i as f32 / n as f32;
            let a = lerp(t, lerp(t, p0, c1), lerp(t, c1, c2));
            let b = lerp(t, lerp(t, c1, c2), lerp(t, c2, p));
            let q = lerp(t, a, b);
            self.raster.draw_line(prev, q);
            prev = q;
        }
        self.raster.draw_line(prev, p);
        self.current = p;
    }
    fn close(&mut self) {
        // Fill needs closed contours: join the last point back to the start.
        self.raster.draw_line(self.current, self.start);
        self.current = self.start;
    }
}
