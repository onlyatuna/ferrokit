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
use crate::text::glyph_face::{exceeds_glyph_budget, GlyphFace, GlyphMetrics, LineMetrics};
use ab_glyph_rasterizer::{point, Point, Rasterizer};
use rustybuzz::ttf_parser::{Face, GlyphId, OutlineBuilder, Tag};

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

    /// `QFontEngine::initializeHeightMetrics` (qfontengine.cpp:419-448) from the font's own tables,
    /// which Qt 6 uses on every platform instead of the system's values: the `hhea` ascender,
    /// descender and line gap, replaced by the OS/2 typographic ones when `fsSelection` has
    /// `USE_TYPO_METRICS` (bit 7) and by the OS/2 Windows ascent and descent (with no leading)
    /// otherwise. Invalid values (both of a pair zero) leave the earlier ones. Each is
    /// `QFixed::fromReal(value * px / unitsPerEm)`, truncated to 1/64 px. Read straight from the
    /// table bytes, as Qt does, so the result does not depend on a platform shaper.
    ///
    /// A font with embedded bitmaps (`EBLC`, `CBLC` or `bdat`) keeps its platform's values in Qt;
    /// those are the `hhea` ones here (CoreText's and FreeType's, the DirectWrite ones differ).
    /// macOS has no override at all: CoreText reports the `hhea` values (`CTFontGetAscent`,
    /// `CTFontGetDescent`, `CTFontGetLeading`, qfontengine_coretext.mm:401-411) unless the
    /// application asks for `QFont::PreferTypoLineMetrics`.
    ///
    /// `None` when neither pair is usable.
    pub(crate) fn line_metrics(&self, px: f32) -> Option<LineMetrics> {
        let raw = self.face.raw_face();
        let upem = f64::from(self.units_per_em);
        let px = f64::from(px);
        // `QFixed::fromReal`: truncated toward zero to 1/64.
        let fixed = |design: f64| ((design * px / upem) * 64.0).trunc() / 64.0;
        // Qt gives up on values that do not fit a `QFixed` (`limitForQFixed`).
        let limit = f64::from(i32::MAX >> 6) / px;
        let fits = |design: i16| f64::from(design) <= limit;
        let (mut ascent, mut descent, mut leading) = (0.0f64, 0.0f64, 0.0f64);

        if let Some(hhea) = raw
            .table(Tag::from_bytes(b"hhea"))
            .filter(|t| t.len() >= 10)
        {
            let (a, d, l) = (be_i16(hhea, 4), be_i16(hhea, 6), be_i16(hhea, 8));
            if !(a == 0 && d == 0) && fits(a) && fits(d) && fits(l) {
                ascent = fixed(f64::from(a));
                descent = -fixed(f64::from(d));
                leading = fixed(f64::from(l));
            }
        }
        let bitmaps = [b"EBLC", b"CBLC", b"bdat"]
            .iter()
            .any(|tag| raw.table(Tag::from_bytes(tag)).is_some());
        if !cfg!(target_os = "macos") && !bitmaps {
            if let Some(os2) = raw
                .table(Tag::from_bytes(b"OS/2"))
                .filter(|t| t.len() >= 78)
            {
                const USE_TYPO_METRICS: u16 = 0x80;
                if be_u16(os2, 62) & USE_TYPO_METRICS != 0 {
                    let (a, d, l) = (be_i16(os2, 68), be_i16(os2, 70), be_i16(os2, 72));
                    if !(a == 0 && d == 0) && fits(a) && fits(d) && fits(l) {
                        ascent = fixed(f64::from(a));
                        descent = -fixed(f64::from(d));
                        leading = fixed(f64::from(l));
                    }
                } else {
                    let (a, d) = (be_u16(os2, 74), be_u16(os2, 76));
                    if !(a == 0 && d == 0) && f64::from(a) <= limit && f64::from(d) <= limit {
                        ascent = fixed(f64::from(a));
                        descent = fixed(f64::from(d));
                        leading = 0.0;
                    }
                }
            }
        }
        (ascent != 0.0 || descent != 0.0).then_some(LineMetrics {
            ascent: ascent as f32,
            descent: descent as f32,
            leading: leading as f32,
        })
    }

    /// The advance of the glyph for `x` as DirectWrite reports it (`GetDesignGlyphMetrics`), in
    /// pixels at `px` per em converted as Qt's `DESIGN_TO_LOGICAL` does: a 26.6 value truncated
    /// toward zero. This is `QFontEngine::averageCharWidth` for Qt's Windows engines
    /// (`boundingBox('x').xoff`, qfontengine.cpp:246-251; measured with PySide6 6.11.2 as the
    /// `QFontMetricsF::averageCharWidth` of every font tried, hinted and unhinted), and the
    /// fallback of the other platforms' engines for a font with no OS/2 average width.
    pub(crate) fn x_advance(&self, px: f32) -> Option<f32> {
        let gid = self.face.glyph_index('x').unwrap_or(GlyphId(0));
        let design = f64::from(self.face.glyph_hor_advance(gid)?);
        let pixels = design / f64::from(self.units_per_em) * f64::from(px);
        Some(((pixels * 64.0).trunc() / 64.0) as f32)
    }

    /// The OS/2 `xAvgCharWidth` at `px` per em as Qt's FreeType and CoreText engines scale it
    /// (`QFontEngineFT::averageCharWidth`, qfontengine_ft.cpp:2167-2178;
    /// `QCoreTextFontEngine`, qfontengine_coretext.mm:240-251, 427-430), or the advance of `x`
    /// when the font has none. FreeType: `QFixed(xAvgCharWidth * x_ppem) / emSquareSize()`, a
    /// 26.6 quotient rounded half up (`x_ppem` being the pixel size rounded, which FreeType
    /// holds for the strike). CoreText: `abs(xAvgCharWidth) * pixelSize / unitsPerEm`,
    /// truncated to 26.6 (`abs` for fonts like Lucida Grande that store a negative width).
    fn os2_average_char_width(&self, px: f32) -> Option<f32> {
        let raw = self.face.raw_face();
        let avg = raw
            .table(Tag::from_bytes(b"OS/2"))
            .filter(|t| t.len() >= 10)
            .map_or(0, |t| be_i16(t, 2));
        if avg == 0 {
            return self.x_advance(px);
        }
        let upem = i64::from(self.face.units_per_em());
        if cfg!(target_os = "macos") {
            let width = f64::from(avg.unsigned_abs());
            return Some(((width * f64::from(px) / upem as f64 * 64.0).trunc() / 64.0) as f32);
        }
        // `QFixed(i) / QFixed(j)`: ((i * 64) << 6 + (j * 64) / 2) / (j * 64), in 1/64 units.
        let ppem = f64::from(px).round() as i64;
        let (a, b) = (i64::from(avg) * ppem * 64, upem * 64);
        Some((((a << 6) + (b >> 1)) / b) as f32 / 64.0)
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

/// Big-endian `i16` at `offset` of a table whose length the caller has checked.
fn be_i16(table: &[u8], offset: usize) -> i16 {
    i16::from_be_bytes([table[offset], table[offset + 1]])
}

/// Big-endian `u16` at `offset` of a table whose length the caller has checked.
fn be_u16(table: &[u8], offset: usize) -> u16 {
    u16::from_be_bytes([table[offset], table[offset + 1]])
}

impl GlyphFace for OutlineFace {
    fn glyph_index(&self, ch: char) -> u16 {
        self.face.glyph_index(ch).map_or(0, |g| g.0)
    }

    /// FreeType's and CoreText's view: the font's own tables (see [`OutlineFace::line_metrics`]).
    fn line_metrics(&self, px: f32) -> Option<LineMetrics> {
        OutlineFace::line_metrics(self, px)
    }

    fn average_char_width(&self, px: f32) -> Option<f32> {
        self.os2_average_char_width(px)
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
