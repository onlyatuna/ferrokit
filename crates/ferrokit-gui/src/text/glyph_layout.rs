// SPDX-License-Identifier: LGPL-3.0-only
use crate::text::font::{Font, SharedFontData};
use crate::text::glyph_face::{GlyphFace, GlyphMetrics, SharedGlyphFace};
use rustybuzz::ttf_parser::kern as ttf_kern;
use std::str::FromStr;
use std::sync::Arc;

/// A single positioned glyph (`QGlyphRun` item equivalent).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PositionedGlyph {
    /// Glyph index in font.
    pub glyph_id: u16,
    /// Index into font engine multi list (0 is primary, >0 is fallback).
    pub font_index: u8,
    /// Horizontal offset in points.
    pub x: f32,
    /// Vertical offset in points.
    pub y: f32,
}

/// A single font engine instance combining the glyph source (metrics, coverage bitmaps and
/// character mapping, see [`GlyphFace`]) and optional raw binary data for OpenType layout
/// (`rustybuzz`). Mirrors Qt's `QFontEngine`.
type ColorGlyphCache = Arc<std::sync::Mutex<std::collections::HashMap<(u16, u32), Option<(GlyphMetrics, Arc<tiny_skia::Pixmap>)>>>>;
type MonoGlyphCache = Arc<std::sync::Mutex<std::collections::HashMap<(u16, u32, u32), (GlyphMetrics, Arc<[u8]>)>>>;
type LcdGlyphCache = Arc<
    std::sync::Mutex<std::collections::HashMap<(u16, u32, u32), Option<(GlyphMetrics, Arc<[u8]>)>>>,
>;
/// Upper bound on cached glyph bitmaps per engine (a CJK font could otherwise grow without limit).
const MONO_GLYPH_CACHE_LIMIT: usize = 4096;
type ShapingFaceCache =
    Arc<std::sync::Mutex<std::collections::HashMap<u16, Option<Arc<ShapingFace>>>>>;

thread_local! {
    static SHAPING_FACE_PARSES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many times this thread built a shaping face (`hb_face_t`) from font data. For tests that
/// check the face is built once per engine and size, not per shaped string.
#[doc(hidden)]
pub fn shaping_face_parse_count() -> usize {
    SHAPING_FACE_PARSES.with(|n| n.get())
}

/// The HarfBuzz face and font of an engine at one ppem (`hb_qt_face_get_for_engine` and
/// `hb_qt_font_get_for_engine`, qharfbuzzng.cpp:668-676, 709): parsing the GSUB/GPOS lookups is
/// most of the cost of shaping a short string, so it is done once and kept on the engine.
pub struct ShapingFace {
    // Declared before `_data` so it is dropped first, while the bytes it borrows are alive.
    face: rustybuzz::Face<'static>,
    _data: SharedFontData,
}

impl ShapingFace {
    fn new(data: &SharedFontData, face_index: u32, ppem: u16) -> Option<Self> {
        SHAPING_FACE_PARSES.with(|n| n.set(n.get() + 1));
        let bytes = data.as_slice();
        // SAFETY: `SharedFontData` is an `Arc` over font bytes that are never moved or written
        // while a clone exists (an owned `Vec` or a read-only file mapping). `_data` keeps one for
        // as long as `face` lives, `face` is dropped before it, and `face()` lends it out only for
        // the lifetime of `&self`.
        let bytes: &'static [u8] =
            unsafe { std::slice::from_raw_parts(bytes.as_ptr(), bytes.len()) };
        let mut face = rustybuzz::Face::from_slice(bytes, face_index)?;
        face.set_pixels_per_em(Some((ppem, ppem)));
        Some(Self { face, _data: data.clone() })
    }

    pub fn face(&self) -> &rustybuzz::Face<'_> {
        &self.face
    }
}

#[derive(Clone)]
pub struct FontEngine {
    /// Glyph source for rasterization, metrics, and character-to-glyph mapping.
    pub face: SharedGlyphFace,
    /// Optional binary font file data for HarfBuzz / rustybuzz OpenType shaping.
    pub raw_data: Option<SharedFontData>,
    /// Font face index within a font collection (.ttc / .otc).
    pub face_index: u32,
    /// Cached color glyphs: (glyph_id, px_size_key) -> (Metrics, Arc<Pixmap>)
    color_cache: ColorGlyphCache,
    /// Cached monochrome glyph bitmaps: (glyph_id, px_size bits) -> (Metrics, coverage bitmap)
    mono_cache: MonoGlyphCache,
    /// Cached LCD glyph bitmaps, three coverages per pixel; `None` when the face has none.
    lcd_cache: LcdGlyphCache,
    /// Shaping faces per ppem, built on first use; `None` when the font data does not parse.
    shaping_faces: ShapingFaceCache,
}

impl FontEngine {
    /// Creates a new engine backed by a glyph face.
    pub fn new(face: SharedGlyphFace) -> Self {
        Self {
            face,
            raw_data: None,
            face_index: 0,
            color_cache: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            mono_cache: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            lcd_cache: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            shaping_faces: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        }
    }

    /// Sets the raw binary font data for OpenType shaping.
    pub fn with_raw_data(mut self, raw_data: impl Into<SharedFontData>) -> Self {
        self.raw_data = Some(raw_data.into());
        // Faces built from other data (shared with clones of this engine) do not apply.
        self.shaping_faces = Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
        self
    }

    /// The HarfBuzz face for shaping at `ppem`, built once per engine and ppem; `None` without
    /// OpenType data.
    pub fn shaping_face(&self, ppem: u16) -> Option<Arc<ShapingFace>> {
        let data = self.raw_data.as_ref()?;
        let mut faces = self.shaping_faces.lock().unwrap_or_else(|e| e.into_inner());
        faces
            .entry(ppem)
            .or_insert_with(|| ShapingFace::new(data, self.face_index, ppem).map(Arc::new))
            .clone()
    }

    /// Sets the face index within a font collection file.
    pub fn with_face_index(mut self, face_index: u32) -> Self {
        self.face_index = face_index;
        self.shaping_faces = Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
        self
    }
    /// Checks whether `glyph_id` is an OpenType color glyph with COLRv0 layers.
    pub fn is_color_glyph(&self, glyph_id: u16) -> bool {
        if let Some(raw) = &self.raw_data {
            crate::text::color_glyph::parse_colr_v0_layers(raw.as_slice(), self.face_index, glyph_id).is_some()
        } else {
            false
        }
    }

    /// Rasterizes an OpenType color glyph into a 32-bit premultiplied ARGB/RGBA `Pixmap`.
    ///
    /// Mirrors Qt6's `QWindowsFontEngineDirectWrite::alphaRGBMapForGlyph` / `renderColr0GlyphRun`.
    pub fn rasterize_color_glyph(
        &self,
        glyph_id: u16,
        px_size: f32,
    ) -> Option<(GlyphMetrics, Arc<tiny_skia::Pixmap>)> {
        let key = (glyph_id, (px_size * 64.0).round() as u32);
        if let Ok(guard) = self.color_cache.lock() {
            if let Some(entry) = guard.get(&key) {
                return entry.clone();
            }
        }

        let raw = self.raw_data.as_ref()?;
        let rendered = crate::text::color_glyph::rasterize_color_glyph(
            raw.as_slice(),
            self.face_index,
            glyph_id,
            self.face.as_ref(),
            px_size,
        ).map(|(m, p)| (m, Arc::new(p)));

        if let Ok(mut guard) = self.color_cache.lock() {
            guard.insert(key, rendered.clone());
        }

        rendered
    }

    /// Rasterizes a monochrome (alpha coverage) glyph of a font of `size` pixels drawn at device
    /// pixel ratio `scale`, caching the bitmap per `(glyph, size, scale)`.
    ///
    /// Mirrors Qt's `QFontEngineGlyphCache`: a glyph is rasterized once per size and then reused by
    /// every later paint. The bitmap is identical to the face's own `rasterize_scaled`.
    /// The cache is shared by every clone of this engine.
    pub fn rasterize_glyph(&self, glyph_id: u16, size: f32, scale: f32) -> (GlyphMetrics, Arc<[u8]>) {
        let key = (glyph_id, size.to_bits(), scale.to_bits());
        if let Ok(guard) = self.mono_cache.lock() {
            if let Some((metrics, bitmap)) = guard.get(&key) {
                return (*metrics, bitmap.clone());
            }
        }
        let (metrics, bitmap) = self.face.rasterize_scaled(glyph_id, size, scale);
        let bitmap: Arc<[u8]> = bitmap.into();
        if let Ok(mut guard) = self.mono_cache.lock() {
            if guard.len() >= MONO_GLYPH_CACHE_LIMIT {
                guard.clear();
            }
            guard.insert(key, (metrics, bitmap.clone()));
        }
        (metrics, bitmap)
    }

    /// Like [`rasterize_glyph`](Self::rasterize_glyph) with three sub-pixel coverages per pixel,
    /// or `None` when the face cannot produce them.
    pub fn rasterize_lcd_glyph(
        &self,
        glyph_id: u16,
        size: f32,
        scale: f32,
    ) -> Option<(GlyphMetrics, Arc<[u8]>)> {
        let key = (glyph_id, size.to_bits(), scale.to_bits());
        if let Ok(guard) = self.lcd_cache.lock() {
            if let Some(entry) = guard.get(&key) {
                return entry.clone();
            }
        }
        let rendered = self
            .face
            .rasterize_lcd_scaled(glyph_id, size, scale)
            .map(|(metrics, bitmap)| (metrics, Arc::<[u8]>::from(bitmap)));
        if let Ok(mut guard) = self.lcd_cache.lock() {
            if guard.len() >= MONO_GLYPH_CACHE_LIMIT {
                guard.clear();
            }
            guard.insert(key, rendered.clone());
        }
        rendered
    }
}

impl From<SharedGlyphFace> for FontEngine {
    fn from(face: SharedGlyphFace) -> Self {
        Self::new(face)
    }
}

/// A contiguous slice of text associated with a single font engine.
/// Mirrors Qt's item boundaries in `QTextEngine::itemBoundaries`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FontRun<'a> {
    /// Text slice belonging to this run.
    pub text: &'a str,
    /// Index into the multi-engine list.
    pub engine_index: usize,
}

/// Shaped text layout result (`QGlyphLayout` / `QTextLayout` equivalent).
#[derive(Debug, Clone, PartialEq)]
pub struct GlyphLayout {
    /// List of ordered glyphs and their relative positions.
    pub glyphs: Vec<PositionedGlyph>,
    /// Total advance width of shaped text.
    pub width: f32,
}

/// The letter spacing Qt adds per glyph: `QFont::setLetterSpacing` stores `QFixed::fromReal(spacing)`
/// (`qfont.cpp`), which truncates to 26.6 fixed point (`(int)(r * 64) / 64`, `qfixed_p.h`), so
/// `0.4` px becomes 25/64 = 0.390625 px. Summed over a long text that is a pixel.
fn qt_letter_spacing(spacing: f32) -> f32 {
    ((spacing as f64 * 64.0) as i32) as f32 / 64.0
}

/// `hb_font_t::x_mult` of the font Qt hands HarfBuzz (`hb_font_set_scale(QFixed::fromReal(ppem))`):
/// the 16.16 factor taking font units to 26.6 pixels.
fn hb_font_multiplier(size: f32, units_per_em: i32) -> i64 {
    if units_per_em <= 0 {
        return 0;
    }
    let scale = (f64::from(size) * 64.0) as i64;
    (scale << 16) / i64::from(units_per_em)
}

/// `hb_font_t::em_mult`: font units to 26.6 pixels, rounded half up, as HarfBuzz scales what it
/// adds to the font engine's advance.
fn hb_scale_font_units(units: i64, mult: i64) -> i64 {
    (units * mult + 32768) >> 16
}

/// The plain horizontal subtables of the font's `kern` table when HarfBuzz applies them itself:
/// no `kerx`, and no GPOS `kern` feature (`apply_kern` in HarfBuzz's shape plan). `None` when the
/// font kerns through GPOS, or its `kern` table needs state machines or cross-stream kerning, which
/// stay with rustybuzz.
fn legacy_kern_table<'a>(face: &'a rustybuzz::Face<'a>) -> Option<Vec<ttf_kern::Subtable<'a>>> {
    let tables = face.tables();
    if tables.kerx.is_some() {
        return None;
    }
    let kern_tag = rustybuzz::ttf_parser::Tag::from_bytes(b"kern");
    if tables.gpos.is_some_and(|gpos| gpos.features.into_iter().any(|feature| feature.tag == kern_tag)) {
        return None;
    }
    let mut subtables = Vec::new();
    for subtable in tables.kern.as_ref()?.subtables {
        if subtable.variable || !subtable.horizontal {
            continue;
        }
        if subtable.has_cross_stream || subtable.has_state_machine {
            return None;
        }
        subtables.push(subtable);
    }
    (!subtables.is_empty()).then_some(subtables)
}

/// Advance and x offset of every shaped glyph in 26.6 pixels, as `QTextEngine::shapeTextWithHarfbuzzNG`
/// hands them on: the font engine's advance (`_hb_qt_font_get_glyph_h_advance`) plus what HarfBuzz
/// adds to it, scaled to `size * 64` per unit. A `kern` table is applied the way `hb_kern_machine_t`
/// does: the pair's kern is scaled first and only then split over both glyphs, the second one also
/// moving by its half. A font engine without subpixel positions (Qt's GDI engine) gets every
/// advance and offset rounded to whole pixels. `None` when the engine has no layout advance.
fn qt_glyph_metrics(
    face: &rustybuzz::Face,
    engine: &dyn GlyphFace,
    infos: &[rustybuzz::GlyphInfo],
    positions: &[rustybuzz::GlyphPosition],
    size: f32,
    direct_write: bool,
    legacy_kern: Option<&Vec<ttf_kern::Subtable>>,
) -> Option<Vec<(i64, i64)>> {
    let mult = hb_font_multiplier(size, face.units_per_em());
    let mut metrics = Vec::with_capacity(infos.len());
    for (info, pos) in infos.iter().zip(positions) {
        let glyph = rustybuzz::ttf_parser::GlyphId(info.glyph_id as u16);
        let engine_advance = engine.layout_advance_width(glyph.0, size, direct_write)?;
        let design = i64::from(face.glyph_hor_advance(glyph).unwrap_or(0));
        // HarfBuzz zeroes the advance of marks after the font callback; keep that zero.
        let advance = if pos.x_advance == 0 {
            0
        } else {
            (f64::from(engine_advance) * 64.0).round() as i64
                + hb_scale_font_units(i64::from(pos.x_advance) - design, mult)
        };
        metrics.push((advance, hb_scale_font_units(i64::from(pos.x_offset), mult)));
    }
    if let Some(subtables) = legacy_kern {
        let is_mark = |info: &rustybuzz::GlyphInfo| {
            face.tables().gdef.is_some_and(|gdef| {
                gdef.glyph_class(rustybuzz::ttf_parser::GlyphId(info.glyph_id as u16))
                    == Some(rustybuzz::ttf_parser::gdef::GlyphClass::Mark)
            })
        };
        for subtable in subtables {
            let mut i = 0;
            while i < infos.len() {
                // `IGNORE_MARKS` skipping iterator: the next glyph that is not a mark.
                let Some(j) = (i + 1..infos.len()).find(|&j| !is_mark(&infos[j])) else {
                    break;
                };
                let raw = subtable
                    .glyphs_kerning(
                        rustybuzz::ttf_parser::GlyphId(infos[i].glyph_id as u16),
                        rustybuzz::ttf_parser::GlyphId(infos[j].glyph_id as u16),
                    )
                    .map_or(0, i64::from);
                if raw != 0 {
                    let kern = hb_scale_font_units(raw, mult);
                    let first = kern >> 1;
                    let second = kern - first;
                    metrics[i].0 += first;
                    metrics[j].0 += second;
                    metrics[j].1 += second;
                }
                i = j;
            }
        }
    }
    if !direct_write {
        // `g.advances[i].round()` and `g.offsets[i].x.round()` for engines without subpixel positions.
        for (advance, x_offset) in &mut metrics {
            *advance = (*advance + 32) & -64;
            *x_offset = (*x_offset + 32) & -64;
        }
    }
    Some(metrics)
}

impl GlyphLayout {
    /// Creates an empty layout.
    pub fn empty() -> Self {
        Self {
            glyphs: Vec::new(),
            width: 0.0,
        }
    }

    /// Shapes and positions text using Qt 6 Run-based architecture:
    /// partitions text into font runs and uses `rustybuzz` with fallback to face metrics.
    pub fn shape(text: &str, font: &Font, face: SharedGlyphFace) -> Self {
        let mut engine = FontEngine::new(face).with_face_index(0);
        if let Some(raw) = &font.font_data {
            engine = engine.with_raw_data(raw.clone());
        }
        Self::shape_with_engines(text, font, &[engine])
    }

    /// Shapes text using rustybuzz (supporting tnum, kerning, and OpenType features).
    pub fn shape_with_rustybuzz(text: &str, font: &Font, rb_face: &rustybuzz::Face) -> Self {
        let mut buffer = rustybuzz::UnicodeBuffer::new();
        buffer.push_str(text);

        let mut features = Vec::new();
        if font.tabular_numbers {
            if let Ok(f) = rustybuzz::Feature::from_str("tnum") {
                features.push(f);
            }
        }
        if let Ok(f) = rustybuzz::Feature::from_str("kern") {
            features.push(f);
        }

        let glyph_buffer = rustybuzz::shape(rb_face, &features, buffer);

        let upem = rb_face.units_per_em() as f32;
        let scale = if upem > 0.0 { font.size / upem } else { 1.0 };

        let infos = glyph_buffer.glyph_infos();
        let positions = glyph_buffer.glyph_positions();

        let mut current_x = 0.0;
        let mut current_y = 0.0;
        let mut glyphs = Vec::with_capacity(infos.len());

        for (info, pos) in infos.iter().zip(positions.iter()) {
            glyphs.push(PositionedGlyph {
                glyph_id: info.glyph_id as u16,
                font_index: 0,
                x: current_x + (pos.x_offset as f32) * scale,
                y: current_y + (pos.y_offset as f32) * scale,
            });
            current_x += (pos.x_advance as f32) * scale;
            current_y += (pos.y_advance as f32) * scale;
        }

        Self {
            glyphs,
            width: current_x,
        }
    }

    /// Fallback shaping using per-glyph face metrics.
    pub fn shape_with_face(text: &str, font: &Font, face: SharedGlyphFace) -> Self {
        let engine = FontEngine::new(face);
        Self::shape_with_engines(text, font, &[engine])
    }

    /// True Qt `QFontEngineMulti` parity: shapes text using primary font engine,
    /// dynamically falling back to secondary font engines on a per-glyph basis.
    pub fn shape_with_faces(text: &str, font: &Font, faces: &[SharedGlyphFace]) -> Self {
        let engines: Vec<FontEngine> = faces
            .iter()
            .enumerate()
            .map(|(idx, f)| {
                let mut engine = FontEngine::new(Arc::clone(f));
                if idx == 0 {
                    if let Some(data) = &font.font_data {
                        engine.raw_data = Some(data.clone());
                    }
                }
                engine
            })
            .collect();

        Self::shape_with_engines(text, font, &engines)
    }

    /// Step 1 & 2 (Qt parity: `QFontEngineMulti::stringToCMap` + `QTextEngine::itemBoundaries`):
    /// Maps characters to font engines and partitions the string into contiguous runs sharing the same engine.
    pub fn partition_into_runs<'a>(text: &'a str, engines: &[FontEngine]) -> Vec<FontRun<'a>> {
        if text.is_empty() || engines.is_empty() {
            return Vec::new();
        }

        let primary = &engines[0].face;
        let mut char_engine_indices = Vec::with_capacity(text.len());
        let mut last_fallback_idx = 0usize;

        for ch in text.chars() {
            let mut resolved_idx = 0usize;
            let gid = primary.glyph_index(ch);

            // Qt parity: if primary engine has no glyph (0) and character is not whitespace/ignorable,
            // query fallback engines.
            if gid == 0 && ch != ' ' && ch != '\t' && ch != '\n' && ch != '\r' && ch != '\u{200b}' {
                let mut found = false;

                // Locality heuristic (Qt `lastFallback` in `stringToCMap`):
                // consecutive characters in multi-language text (CJK/Emoji) usually share the same font.
                if last_fallback_idx > 0
                    && last_fallback_idx < engines.len()
                    && engines[last_fallback_idx].face.glyph_index(ch) != 0
                {
                    resolved_idx = last_fallback_idx;
                    found = true;
                }

                if !found {
                    for (idx, fb_engine) in engines.iter().enumerate().skip(1) {
                        if idx == last_fallback_idx {
                            continue;
                        }
                        if fb_engine.face.glyph_index(ch) != 0 {
                            resolved_idx = idx;
                            last_fallback_idx = idx;
                            break;
                        }
                    }
                }
            }

            char_engine_indices.push((ch, resolved_idx));
        }

        // Qt parity (`QTextEngine::itemBoundaries`): group adjacent characters with the same engine into runs.
        let mut runs = Vec::new();
        let mut current_engine = char_engine_indices[0].1;
        let mut current_start = 0;
        let mut current_byte = 0;

        for &(ch, engine_idx) in &char_engine_indices {
            let ch_len = ch.len_utf8();
            if engine_idx != current_engine {
                runs.push(FontRun {
                    text: &text[current_start..current_byte],
                    engine_index: current_engine,
                });
                current_start = current_byte;
                current_engine = engine_idx;
            }
            current_byte += ch_len;
        }
        runs.push(FontRun {
            text: &text[current_start..text.len()],
            engine_index: current_engine,
        });

        runs
    }

    /// Full Qt 6 `QTextEngine::shapeTextWithHarfbuzzNG` parity:
    /// Shapes text across multiple font engines by partitioning into runs,
    /// batch-shaping each run with `rustybuzz` (with full OpenType GSUB/GPOS feature support),
    /// and falling back to the face's own per-glyph metrics when binary font data is unavailable.
    pub fn shape_with_engines(text: &str, font: &Font, engines: &[FontEngine]) -> Self {
        if text.is_empty() || engines.is_empty() {
            return Self::empty();
        }

        // Qt sets the HarfBuzz font's ppem to the whole pixel size (`hb_font_set_ppem(int(ppem))`),
        // which selects the GPOS device-table adjustments.
        let ppem = font.size as u16;

        let runs = Self::partition_into_runs(text, engines);
        let mut glyphs = Vec::with_capacity(text.len());
        let mut current_x = 0.0;
        let mut current_y = 0.0;
        let direct_write = crate::text::font_database::uses_directwrite_engine();

        for run in runs {
            let engine = &engines[run.engine_index];
            let shaping_face = engine.shaping_face(ppem);

            if let Some(rb_face) = shaping_face.as_deref().map(ShapingFace::face) {
                // Qt `shapeTextWithHarfbuzzNG` path:
                let mut buffer = rustybuzz::UnicodeBuffer::new();
                buffer.push_str(run.text);

                let mut features = Vec::new();
                if font.tabular_numbers {
                    if let Ok(f) = rustybuzz::Feature::from_str("tnum") {
                        features.push(f);
                    }
                }
                // HarfBuzz applies a `kern` table itself when the font has no GPOS kerning; the pairs
                // are then applied below, scaled as HarfBuzz scales them (see `legacy_kern_table`).
                let legacy_kern = legacy_kern_table(rb_face);
                if let Ok(f) = rustybuzz::Feature::from_str(if legacy_kern.is_some() { "kern=0" } else { "kern" }) {
                    features.push(f);
                }
                // Qt passes `letterSpacing != 0` to disable ligatures (`shapeTextWithHarfbuzzNG`).
                if font.letter_spacing != 0.0 {
                    for tag in ["liga=0", "clig=0"] {
                        if let Ok(f) = rustybuzz::Feature::from_str(tag) {
                            features.push(f);
                        }
                    }
                }

                let glyph_buffer = rustybuzz::shape(rb_face, &features, buffer);
                let upem = rb_face.units_per_em() as f32;
                let scale = if upem > 0.0 { font.size / upem } else { 1.0 };

                let infos = glyph_buffer.glyph_infos();
                let positions = glyph_buffer.glyph_positions();

                let engine_metrics = qt_glyph_metrics(
                    rb_face,
                    &*engine.face,
                    infos,
                    positions,
                    font.size,
                    direct_write,
                    legacy_kern.as_ref(),
                );

                for (i, (info, pos)) in infos.iter().zip(positions.iter()).enumerate() {
                    // `engine_metrics` is Qt's own advance and x offset in 26.6 pixels; without a
                    // font engine advance HarfBuzz's design values stand.
                    let (adv, x_offset) = match &engine_metrics {
                        Some(m) => (m[i].0 as f32 / 64.0, m[i].1 as f32 / 64.0),
                        None => ((pos.x_advance as f32) * scale, (pos.x_offset as f32) * scale),
                    };
                    glyphs.push(PositionedGlyph {
                        glyph_id: info.glyph_id as u16,
                        font_index: run.engine_index as u8,
                        x: current_x + x_offset,
                        y: current_y + (pos.y_offset as f32) * scale,
                    });
                    current_x += adv;
                    current_y += (pos.y_advance as f32) * scale;
                    // `QTextEngine::shapeText`: spacing goes after the last glyph of every cluster.
                    if infos.get(i + 1).is_none_or(|next| next.cluster != info.cluster) {
                        current_x += qt_letter_spacing(font.letter_spacing);
                    }
                }
            } else {
                // Fallback path (per-glyph face metrics) for runs without raw binary font data:
                let tnum_width = if font.tabular_numbers {
                    Some(
                        engine
                            .face
                            .metrics('0', font.size)
                            .advance_width
                            .max(engine.face.metrics('8', font.size).advance_width),
                    )
                } else {
                    None
                };

                for ch in run.text.chars() {
                    let gid = engine.face.glyph_index(ch);
                    let metrics = engine.face.metrics(ch, font.size);
                    let adv_x = if ch.is_ascii_digit() && run.engine_index == 0 {
                        tnum_width.unwrap_or(metrics.advance_width)
                    } else {
                        engine
                            .face
                            .layout_advance_width(gid, font.size, direct_write)
                            .unwrap_or(metrics.advance_width)
                    };

                    glyphs.push(PositionedGlyph {
                        glyph_id: gid,
                        font_index: run.engine_index as u8,
                        x: current_x,
                        y: current_y,
                    });
                    current_x += adv_x + qt_letter_spacing(font.letter_spacing);
                }
            }
        }

        Self {
            glyphs,
            width: current_x,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use crate::text::glyph_face::parse_face;

    /// Arial's regular face, looked up through the font database so every platform finds it
    /// where it is installed.
    fn get_test_font() -> (Arc<Vec<u8>>, SharedGlyphFace) {
        let raw = crate::text::font_database::with_global_font_database(|db| {
            db.get_raw_font_data("Arial")
        })
        .expect("Arial is installed");
        let data = raw.as_slice().to_vec();
        let face = parse_face(&SharedFontData::from_vec(data.clone()), 0).unwrap();
        (Arc::new(data), face)
    }

    #[test]
    fn test_glyph_layout_fontdue_shaping() {
        let (data, font_face) = get_test_font();
        let font = Font::new("Arial", 16.0).with_font_data(data);

        let layout = GlyphLayout::shape("HUD 100%", &font, font_face);
        assert!(!layout.glyphs.is_empty());
        assert!(layout.width > 0.0);
        assert_eq!(layout.glyphs[0].x, 0.0);
        assert!(layout.glyphs[1].x > 0.0);
    }

    #[test]
    fn test_glyph_layout_rustybuzz_tnum() {
        let (data, font_face) = get_test_font();
        let font_tnum = Font::new("Arial", 16.0)
            .with_tabular_numbers(true)
            .with_font_data(data);

        // Single glyphs: Arial kerns the pair `11` (HarfBuzz applies it whatever `tnum` says), so a
        // run of ones is narrower than a run of eights in fonts that carry that pair.
        let layout_1 = GlyphLayout::shape("1", &font_tnum, font_face.clone());
        let layout_8 = GlyphLayout::shape("8", &font_tnum, font_face);

        assert_eq!(layout_1.glyphs.len(), 1);
        assert_eq!(layout_8.glyphs.len(), 1);

        assert!((layout_1.width - layout_8.width).abs() < 1.0);
    }
    #[test]
    fn test_glyph_layout_emoji_fallback() {
        let (_data, font_face) = get_test_font();
        let font = Font::new("Arial", 16.0);
        let emoji_path = std::path::Path::new("C:/Windows/Fonts/seguiemj.ttf");
        if emoji_path.exists() {
            let emoji_data = std::fs::read(emoji_path).unwrap();
            let emoji_font = parse_face(&SharedFontData::from_vec(emoji_data), 0).unwrap();
            let fonts = vec![font_face, emoji_font];
            let layout = GlyphLayout::shape_with_faces("🔄 立即", &font, &fonts);
            assert_eq!(layout.glyphs[0].font_index, 1, "Emoji should resolve to fallback engine 1");
            let (metrics, bitmap) = fonts[1].rasterize_indexed(layout.glyphs[0].glyph_id, 16.0);
            assert!(metrics.width > 0);
            assert!(metrics.height > 0);
            assert!(!bitmap.is_empty());
        }
    }

    #[test]
    fn test_glyph_layout_empty_and_whitespace() {
        let (data, font_face) = get_test_font();
        let font = Font::new("Arial", 16.0).with_font_data(data);

        let layout_empty = GlyphLayout::shape("", &font, font_face.clone());
        assert!(layout_empty.glyphs.is_empty());
        assert_eq!(layout_empty.width, 0.0);

        let layout_space = GlyphLayout::shape("   ", &font, font_face);
        assert_eq!(layout_space.glyphs.len(), 3);
        assert!(layout_space.width > 0.0);
    }

    #[test]
    fn test_glyph_layout_partition_into_runs() {
        let (_data, font_face) = get_test_font();
        let emoji_path = std::path::Path::new("C:/Windows/Fonts/seguiemj.ttf");
        if emoji_path.exists() {
            let emoji_data = std::fs::read(emoji_path).unwrap();
            let emoji_font = parse_face(&SharedFontData::from_vec(emoji_data), 0).unwrap();
            let engines = vec![
                FontEngine::new(font_face),
                FontEngine::new(emoji_font),
            ];

            // "OK 🔄 OK" -> Run 0: "OK ", Run 1: "🔄", Run 2: " OK"
            let runs = GlyphLayout::partition_into_runs("OK 🔄 OK", &engines);
            assert_eq!(runs.len(), 3);
            assert_eq!(runs[0].text, "OK ");
            assert_eq!(runs[0].engine_index, 0);
            assert_eq!(runs[1].text, "🔄");
            assert_eq!(runs[1].engine_index, 1);
            assert_eq!(runs[2].text, " OK");
            assert_eq!(runs[2].engine_index, 0);
        }
    }

    #[test]
    fn test_glyph_layout_multi_engine_raw_opentype_shaping() {
        let (data, font_face) = get_test_font();
        let font = Font::new("Arial", 16.0);
        let emoji_path = std::path::Path::new("C:/Windows/Fonts/seguiemj.ttf");
        if emoji_path.exists() {
            let emoji_data = std::fs::read(emoji_path).unwrap();
            let emoji_font = parse_face(&SharedFontData::from_vec(emoji_data.clone()), 0).unwrap();
            let engines = vec![
                FontEngine::new(font_face).with_raw_data(data),
                FontEngine::new(emoji_font).with_raw_data(Arc::new(emoji_data)),
            ];

            let layout = GlyphLayout::shape_with_engines("Status: 🔄 Active", &font, &engines);
            assert!(!layout.glyphs.is_empty());
            assert!(layout.width > 0.0);

            // Verify that glyph coordinates advance monotonically
            for w in layout.glyphs.windows(2) {
                assert!(w[1].x >= w[0].x, "Glyph coordinates must advance monotonically");
            }

            // Locate the emoji glyph and verify it has engine_index = 1
            let emoji_glyph = layout.glyphs.iter().find(|g| g.font_index == 1);
            assert!(emoji_glyph.is_some(), "Emoji should have been shaped with engine 1");
        }
    }
}
