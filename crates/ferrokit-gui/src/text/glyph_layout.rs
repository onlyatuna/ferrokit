// SPDX-License-Identifier: LGPL-3.0-only
use crate::text::font::{Font, FontTag, SharedFontData};
use crate::text::glyph_face::{GlyphFace, GlyphMetrics, SharedGlyphFace};
use rustybuzz::ttf_parser::kern as ttf_kern;
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
    /// Byte offset in the shaped text of the first character of the cluster this glyph belongs
    /// to (`hb_glyph_info_t::cluster`, made absolute across font runs).
    pub cluster: usize,
    /// Pen advance after this glyph in points, including the letter spacing Qt adds after the last
    /// glyph of a cluster. The advances of all glyphs add up to [`GlyphLayout::width`].
    pub advance: f32,
}

/// A single font engine instance combining the glyph source (metrics, coverage bitmaps and
/// character mapping, see [`GlyphFace`]) and optional raw binary data for OpenType layout
/// (`rustybuzz`). Mirrors Qt's `QFontEngine`.
type ColorGlyphCache = Arc<
    std::sync::Mutex<
        std::collections::HashMap<(u16, u32), Option<(GlyphMetrics, Arc<tiny_skia::Pixmap>)>>,
    >,
>;
type MonoGlyphCache = Arc<
    std::sync::Mutex<std::collections::HashMap<(u16, u32, u32, i32), (GlyphMetrics, Arc<[u8]>)>>,
>;
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
        // No points per em on purpose: rustybuzz then applies no `trak` tracking (what it adds is
        // not what the HarfBuzz 14 bundled with Qt 6.12 adds, see `hb_tracking`); where Qt applies
        // tracking (macOS) `qt_glyph_metrics` does.
        Some(Self {
            face,
            _data: data.clone(),
        })
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
            crate::text::color_glyph::parse_colr_v0_layers(
                raw.as_slice(),
                self.face_index,
                glyph_id,
            )
            .is_some()
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
        if let Ok(guard) = self.color_cache.lock()
            && let Some(entry) = guard.get(&key)
        {
            return entry.clone();
        }

        let raw = self.raw_data.as_ref()?;
        let rendered = crate::text::color_glyph::rasterize_color_glyph(
            raw.as_slice(),
            self.face_index,
            glyph_id,
            self.face.as_ref(),
            px_size,
        )
        .map(|(m, p)| (m, Arc::new(p)));

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
    pub fn rasterize_glyph(
        &self,
        glyph_id: u16,
        size: f32,
        scale: f32,
    ) -> (GlyphMetrics, Arc<[u8]>) {
        self.cached_mono_glyph(glyph_id, size, scale, 0, || {
            self.face.rasterize_scaled(glyph_id, size, scale)
        })
    }

    /// Whether glyphs are drawn at the fractional x position of their pen
    /// ([`GlyphFace::supports_horizontal_subpixel_positions`]): the caller then blits them at the
    /// floor of that position, `x >> 6` for a pen at `x` (26.6).
    pub fn supports_horizontal_subpixel_positions(&self) -> bool {
        self.face.supports_horizontal_subpixel_positions()
    }

    /// [`rasterize_glyph`](Self::rasterize_glyph) for a glyph whose pen is at `x` (26.6 device
    /// pixels): the bitmap Qt's glyph cache draws for that sub-pixel position, cached per position.
    /// Same as `rasterize_glyph` when the face has no sub-pixel positions.
    pub fn rasterize_glyph_at(
        &self,
        glyph_id: u16,
        size: f32,
        scale: f32,
        x: i64,
    ) -> (GlyphMetrics, Arc<[u8]>) {
        if !self.supports_horizontal_subpixel_positions() {
            return self.rasterize_glyph(glyph_id, size, scale);
        }
        let subpixel = self.face.subpixel_position(glyph_id, size, scale, x);
        self.cached_mono_glyph(glyph_id, size, scale, subpixel, || {
            self.face
                .rasterize_subpixel(glyph_id, size, scale, subpixel)
        })
    }

    fn cached_mono_glyph(
        &self,
        glyph_id: u16,
        size: f32,
        scale: f32,
        subpixel: i32,
        rasterize: impl FnOnce() -> (GlyphMetrics, Vec<u8>),
    ) -> (GlyphMetrics, Arc<[u8]>) {
        let key = (glyph_id, size.to_bits(), scale.to_bits(), subpixel);
        if let Ok(guard) = self.mono_cache.lock()
            && let Some((metrics, bitmap)) = guard.get(&key)
        {
            return (*metrics, bitmap.clone());
        }
        let (metrics, bitmap) = rasterize();
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
        if let Ok(guard) = self.lcd_cache.lock()
            && let Some(entry) = guard.get(&key)
        {
            return entry.clone();
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

/// Qt's `fontDef.pointSize` for a font of `pixel_size` pixels (`QFontDatabasePrivate::load`:
/// `pixelSize * 72 / dpi`), which `hb_qt_font_create` hands HarfBuzz as the font's points per em.
/// `dpi` is `qt_defaultDpi()`, the primary screen's logical DPI: 72 on macOS, the base DPI of
/// `QCocoaScreen` (`logicalBaseDpi`), as PySide6 6.12 reports it under the `cocoa` plugin. The
/// other platforms' screens differ (96 on Windows at 100 %, whatever the display says on Linux),
/// so there this is not applied, and neither is tracking.
#[cfg(target_os = "macos")]
fn qt_point_size(pixel_size: f32) -> f32 {
    const QT_DEFAULT_DPI: f64 = 72.0;
    (f64::from(pixel_size) * 72.0 / QT_DEFAULT_DPI) as f32
}

/// `AAT::TrackTableEntry::get_value` (HarfBuzz 14, hb-aat-layout-trak-table.hh): the tracking in
/// font units of one track at `ptem`, from the sizes the table lists. The value of the first size
/// at or above `ptem` is used, interpolated from the size before it; below the first size and
/// above the last one the nearest value stands (rustybuzz 0.20 extrapolates there). Single
/// precision, as HarfBuzz computes it.
#[cfg(any(target_os = "macos", test))]
fn hb_trak_value(sizes: &[f32], values: &[i16], ptem: f32) -> f32 {
    let n = sizes.len().min(values.len());
    if n == 0 {
        return 0.0;
    }
    if n == 1 {
        return f32::from(values[0]);
    }
    let Some(i) = sizes[..n].iter().position(|&s| s >= ptem) else {
        return f32::from(values[n - 1]);
    };
    if i == 0 {
        return f32::from(values[0]);
    }
    if sizes[i] == ptem {
        return f32::from(values[i]);
    }
    let (mut s0, mut s1) = (sizes[i - 1], sizes[i]);
    let (mut v0, mut v1) = (i32::from(values[i - 1]), i32::from(values[i]));
    // Deal with font bugs.
    if s1 < s0 {
        std::mem::swap(&mut s0, &mut s1);
        std::mem::swap(&mut v0, &mut v1);
    }
    if ptem < s0 {
        return v0 as f32;
    }
    if ptem > s1 {
        return v1 as f32;
    }
    if s0 == s1 {
        return (v0 + v1) as f32 * 0.5;
    }
    let t = (ptem - s0) / (s1 - s0);
    v0 as f32 + t * (v1 - v0) as f32
}

/// `AAT::TrackData::get_tracking` for the default track (0): the tracking of a horizontal `trak`
/// table at `ptem`, in font units. A table without a track 0 interpolates between the tracks
/// around it.
#[cfg(any(target_os = "macos", test))]
fn hb_tracking_units(data: &rustybuzz::ttf_parser::trak::TrackData, ptem: f32) -> f32 {
    let sizes: Vec<f32> = data.sizes.into_iter().map(|size| size.0).collect();
    let tracks: Vec<(f32, f32)> = data
        .tracks
        .into_iter()
        .map(|track| {
            let values: Vec<i16> = track.values.into_iter().collect();
            (track.value, hb_trak_value(&sizes, &values, ptem))
        })
        .collect();
    let count = tracks.len();
    match count {
        0 => return 0.0,
        1 => return tracks[0].1,
        _ => {}
    }
    const TRACK: f32 = 0.0;
    let mut i = 0;
    let mut j = count - 1;
    while i + 1 < count && tracks[i + 1].0 <= TRACK {
        i += 1;
    }
    while j > 0 && tracks[j - 1].0 >= TRACK {
        j -= 1;
    }
    if i == j {
        return tracks[i].1;
    }
    let ((t0, a), (t1, b)) = (tracks[i], tracks[j]);
    let t = (TRACK - t0) / (t1 - t0);
    a + t * (b - a)
}

/// `hb_font_t::em_scalef_x`: `units` font units at `size` pixels per em in 26.6 pixels, rounded
/// half away from zero. HarfBuzz scales with `x_scale / upem`, `x_scale` being Qt's
/// `QFixed::fromReal(size)` (`hb_qt_font_create`), in single precision.
#[cfg(any(target_os = "macos", test))]
fn hb_scale_tracking(units: f32, size: f32, units_per_em: i32) -> i64 {
    let x_scale = (f64::from(size) * 64.0) as i32;
    let multiplier = x_scale as f32 / units_per_em as f32;
    (units * multiplier).round() as i64
}

/// What `AAT::trak::apply` adds to the advance of the first glyph of every grapheme (26.6 pixels):
/// the `trak` table's tracking at `ptem`. HarfBuzz 14, which Qt 6.12 bundles, applies it to fonts
/// with a `trak` and a `STAT` table ("modern fonts", `hb-ot-shape.cc`) regardless of what the
/// shaping features say. 0 for any other font.
#[cfg(any(target_os = "macos", test))]
fn hb_tracking(face: &rustybuzz::Face, ptem: f32, size: f32) -> i64 {
    let stat = rustybuzz::ttf_parser::Tag::from_bytes(b"STAT");
    let has_stat = face
        .raw_face()
        .table(stat)
        .is_some_and(|table| table.len() >= 4 && table[..4] != [0; 4]);
    let Some(trak) = face.tables().trak else {
        return 0;
    };
    if !has_stat || face.units_per_em() <= 0 {
        return 0;
    }
    // `HB_CORETEXT_DEFAULT_FONT_SIZE` for a font without points per em.
    let ptem = if ptem > 0.0 { ptem } else { 12.0 };
    hb_scale_tracking(
        hb_tracking_units(&trak.horizontal, ptem),
        size,
        face.units_per_em(),
    )
}

/// The tracking Qt's HarfBuzz adds to every grapheme's advance in a font of `size` pixels:
/// [`hb_tracking`] with the points per em Qt gives the font, on macOS only (off it the
/// screen-dependent points per em of the other platforms are not modelled and ferrokit has always
/// laid text out without tracking).
fn layout_tracking(face: &rustybuzz::Face, size: f32) -> i64 {
    #[cfg(target_os = "macos")]
    {
        hb_tracking(face, qt_point_size(size), size)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (face, size);
        0
    }
}

/// Whether `glyph_id` is a mark in the font's GDEF table, which HarfBuzz never gives an advance.
fn is_mark_glyph(face: &rustybuzz::Face, glyph_id: u32) -> bool {
    face.tables().gdef.is_some_and(|gdef| {
        gdef.glyph_class(rustybuzz::ttf_parser::GlyphId(glyph_id as u16))
            == Some(rustybuzz::ttf_parser::gdef::GlyphClass::Mark)
    })
}

/// A HarfBuzz feature switching `tag` to `value` over the whole run (`HB_FEATURE_GLOBAL_START`
/// to `HB_FEATURE_GLOBAL_END`, as `QTextEngine::shapeTextWithHarfbuzzNG` builds them).
fn feature(tag: FontTag, value: u32) -> rustybuzz::Feature {
    rustybuzz::Feature::new(
        rustybuzz::ttf_parser::Tag::from_bytes(&tag.to_bytes()),
        value,
        ..,
    )
}

/// The features `QFont::setFeature` set, in tag order.
fn user_features(font: &Font) -> Vec<rustybuzz::Feature> {
    font.features()
        .iter()
        .map(|(&tag, &value)| feature(tag, value))
        .collect()
}

/// The features Qt hands HarfBuzz for `font` (`QTextEngine::shapeTextWithHarfbuzzNG`): the
/// explicit ones, then `kern` and, with letter spacing, the cosmetic ligatures, each only when
/// the font has not set it itself (`QFlatMap::insert` keeps the first value, so
/// `setFeature` takes precedence over what Qt would pick).
///
/// `kern` goes out as 0 when `legacy_kern` says the pairs of the font's `kern` table are applied
/// by hand afterwards (see `legacy_kern_table`); a font that set `kern` to 0 itself keeps both
/// off.
fn shaping_features(font: &Font, legacy_kern: bool) -> Vec<rustybuzz::Feature> {
    let mut features = user_features(font);
    if !font.is_feature_set(FontTag::KERN) {
        features.push(feature(FontTag::KERN, u32::from(!legacy_kern)));
    } else if legacy_kern && font.kerning_enabled() {
        // The explicit non-zero `kern` would make HarfBuzz apply the table too.
        features.retain(|f| f.tag != rustybuzz::ttf_parser::Tag::from_bytes(b"kern"));
        features.push(feature(FontTag::KERN, 0));
    }
    // Qt passes `letterSpacing != 0` to disable ligatures.
    if font.letter_spacing != 0.0 {
        for tag in [FontTag::LIGA, FontTag::CLIG] {
            if !font.is_feature_set(tag) {
                features.push(feature(tag, 0));
            }
        }
    }
    features
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
    if tables.gpos.is_some_and(|gpos| {
        gpos.features
            .into_iter()
            .any(|feature| feature.tag == kern_tag)
    }) {
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
    subpixel: bool,
    legacy_kern: Option<&Vec<ttf_kern::Subtable>>,
) -> Option<Vec<(i64, i64)>> {
    let mult = hb_font_multiplier(size, face.units_per_em());
    let mut metrics = Vec::with_capacity(infos.len());
    for (info, pos) in infos.iter().zip(positions) {
        let glyph = rustybuzz::ttf_parser::GlyphId(info.glyph_id as u16);
        let engine_advance = engine.layout_advance_width(glyph.0, size, subpixel)?;
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
        let is_mark = |info: &rustybuzz::GlyphInfo| is_mark_glyph(face, info.glyph_id);
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
    // `AAT::trak::apply` comes last in `hb_ot_shape_plan_t::position`: it widens the first glyph of
    // every grapheme (not a mark, whose advance HarfBuzz zeroes afterwards, and not a glyph
    // without an advance).
    let tracking = layout_tracking(face, size);
    if tracking != 0 {
        for (info, (advance, _)) in infos.iter().zip(&mut metrics) {
            if *advance != 0 && !is_mark_glyph(face, info.glyph_id) {
                *advance += tracking;
            }
        }
    }
    if !subpixel {
        // `g.advances[i].round()` and `g.offsets[i].x.round()` for engines without subpixel positions.
        for (advance, x_offset) in &mut metrics {
            *advance = (*advance + 32) & -64;
            *x_offset = (*x_offset + 32) & -64;
        }
    }
    Some(metrics)
}

/// Whether `c` has no width in Qt in any font, whatever glyph a font has for it (measured with
/// PySide6 6.11.2, oracle font and Arial: `QFontMetricsF::horizontalAdvance` of each is 0 and
/// `"a" + c + "b"` is as wide as `"ab"`): the default ignorable characters (the soft hyphen, the
/// Arabic letter mark, the zero width space and joiners, the direction marks, embeddings,
/// overrides and isolates, the word joiner and invisible operators, the variation selectors, the
/// byte order mark) and the line and paragraph separators.
pub(crate) fn is_zero_width(c: char) -> bool {
    matches!(
        c,
        '\u{ad}'
            | '\u{61c}'
            | '\u{200b}'..='\u{200f}'
            | '\u{2028}'..='\u{202e}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{fe00}'..='\u{fe0f}'
            | '\u{feff}'
    )
}

impl GlyphLayout {
    /// Creates an empty layout.
    pub fn empty() -> Self {
        Self {
            glyphs: Vec::new(),
            width: 0.0,
        }
    }

    /// The advance of every grapheme of the shaped text.
    ///
    /// `starts` holds the byte offset at which each grapheme of the shaped text starts, followed
    /// by the byte length of the text. Glyphs are folded into graphemes through their cluster:
    /// a cluster's advance (kerning included, as the first glyph of a pair carries it) goes to
    /// the graphemes the cluster covers, split evenly when one glyph stands for several of them
    /// (`QTextEngine::positionInLigature` divides a ligature's width evenly between its
    /// characters). The result sums to [`GlyphLayout::width`] and is in logical order, so
    /// cursor positions stay on grapheme boundaries.
    pub fn grapheme_advances(&self, starts: &[usize]) -> Vec<f32> {
        let count = starts.len().saturating_sub(1);
        let mut advances = vec![0.0f32; count];
        if count == 0 {
            return advances;
        }
        let text_len = starts[count];

        // Glyph order is visual (right to left runs come reversed); clusters are logical.
        let mut by_cluster: Vec<&PositionedGlyph> = self.glyphs.iter().collect();
        by_cluster.sort_by_key(|g| g.cluster);
        let mut clusters: Vec<(usize, f32)> = Vec::with_capacity(by_cluster.len());
        for glyph in by_cluster {
            match clusters.last_mut() {
                Some((cluster, advance)) if *cluster == glyph.cluster => *advance += glyph.advance,
                _ => clusters.push((glyph.cluster, glyph.advance)),
            }
        }

        // The grapheme containing byte `byte`.
        let grapheme_at = |byte: usize| {
            (starts[..count].partition_point(|&s| s <= byte).max(1) - 1).min(count - 1)
        };
        for (i, &(cluster, advance)) in clusters.iter().enumerate() {
            let end = clusters.get(i + 1).map_or(text_len, |next| next.0);
            let first = grapheme_at(cluster);
            let last = grapheme_at(end.saturating_sub(1).max(cluster)).max(first);
            let share = advance / (last - first + 1) as f32;
            for slot in &mut advances[first..=last] {
                *slot += share;
            }
        }
        advances
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

        let mut features = user_features(font);
        if !font.is_feature_set(FontTag::KERN) {
            features.push(feature(FontTag::KERN, 1));
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
            let advance = (pos.x_advance as f32) * scale;
            glyphs.push(PositionedGlyph {
                glyph_id: info.glyph_id as u16,
                font_index: 0,
                x: current_x + (pos.x_offset as f32) * scale,
                y: current_y + (pos.y_offset as f32) * scale,
                cluster: info.cluster as usize,
                advance,
            });
            current_x += advance;
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
                if idx == 0
                    && let Some(data) = &font.font_data
                {
                    engine.raw_data = Some(data.clone());
                }
                engine
            })
            .collect();

        Self::shape_with_engines(text, font, &engines)
    }

    /// Whether `text` holds a sequence that Qt shapes in an emoji font as one item even when the
    /// primary font has a glyph for every character of it (the keycap `1` U+FE0F U+20E3 is
    /// all Arial characters): `QTextEngine::itemize` splits emoji sequences off with its emoji
    /// segmenter. Such text needs the fallback fonts loaded. Only real emoji sequences count: a
    /// ZWJ next to a pictograph (a skin tone or variation selector may sit between them), U+FE0F
    /// after a character that has an emoji form, a keycap U+20E3 after `0`-`9`, `#` or `*`, a
    /// pair of regional indicators (a flag) and a pictograph followed by a tag character (the
    /// subdivision flags, U+1F3F4 and tags U+E0020..U+E007F). The ZWJ of Arabic, Persian or Indic
    /// text is not one.
    pub(crate) fn has_emoji_sequence(text: &str) -> bool {
        // The last character that is neither a variation selector nor a skin tone modifier: what
        // a following selector, keycap, tag or ZWJ attaches to.
        let mut base: Option<char> = None;
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            let sequence = match c {
                '\u{200d}' => {
                    base.is_some_and(Self::is_pictographic)
                        || chars.peek().copied().is_some_and(Self::is_pictographic)
                }
                '\u{fe0f}' => base.is_some_and(Self::is_emoji_base),
                '\u{20e3}' => base.is_some_and(|b| matches!(b, '0'..='9' | '#' | '*')),
                '\u{e0020}'..='\u{e007f}' => base.is_some_and(Self::is_pictographic),
                c if Self::is_regional_indicator(c) => chars
                    .peek()
                    .copied()
                    .is_some_and(Self::is_regional_indicator),
                _ => false,
            };
            if sequence {
                return true;
            }
            if !matches!(c, '\u{fe00}'..='\u{fe0f}' | '\u{1f3fb}'..='\u{1f3ff}' | '\u{e0020}'..='\u{e007f}')
            {
                base = Some(c);
            }
        }
        false
    }

    /// Whether `c` is a regional indicator letter, two of which make a flag.
    fn is_regional_indicator(c: char) -> bool {
        ('\u{1f1e6}'..='\u{1f1ff}').contains(&c)
    }

    /// Whether `c` is in the blocks of the `Extended_Pictographic` characters (an approximation
    /// without the Unicode emoji tables: the pictograph, symbol and arrow blocks and the few
    /// pictographs outside them; regional indicators and skin tone modifiers are not).
    fn is_pictographic(c: char) -> bool {
        matches!(
            c as u32,
            0xA9 | 0xAE
                | 0x203C
                | 0x2049
                | 0x2122
                | 0x2139
                | 0x2194..=0x2199
                | 0x21A9..=0x21AA
                | 0x231A..=0x23FF
                | 0x24C2
                | 0x25A0..=0x27BF
                | 0x2934..=0x2935
                | 0x2B05..=0x2B55
                | 0x3030
                | 0x303D
                | 0x3297
                | 0x3299
                | 0x1F000..=0x1F1E5
                | 0x1F200..=0x1F3FA
                | 0x1F400..=0x1FAFF
                | 0x1FC00..=0x1FFFD
        )
    }

    /// Whether `c` can start an emoji sequence: a pictograph or a keycap base.
    fn is_emoji_base(c: char) -> bool {
        Self::is_pictographic(c) || matches!(c, '0'..='9' | '#' | '*')
    }

    /// Qt shapes an emoji sequence (a ZWJ family, a keycap, a character with the emoji variation
    /// selector, a flag) as one item in an emoji font. A per-character choice would cut it at
    /// every character the primary font happens to cover (U+200D, U+FE0F, the keycap base), so
    /// each grapheme cluster that is such a sequence gets the first fallback font with emoji
    /// glyphs that covers all of it.
    fn keep_emoji_sequences_in_one_font(
        text: &str,
        engines: &[FontEngine],
        resolved: &mut [(char, usize)],
    ) {
        use unicode_segmentation::UnicodeSegmentation;
        if engines.len() < 2 || !Self::has_emoji_sequence(text) {
            return;
        }
        // Characters that need no glyph of their own in the font that shapes the sequence.
        let joiner =
            |c: char| matches!(c, '\u{200d}' | '\u{fe00}'..='\u{fe0f}' | '\u{e0020}'..='\u{e007f}');
        let mut at = 0;
        for cluster in text.graphemes(true) {
            let n = cluster.chars().count();
            let chars = &mut resolved[at..at + n];
            at += n;
            if n < 2 || !Self::has_emoji_sequence(cluster) {
                continue;
            }
            let font = engines.iter().enumerate().skip(1).find(|(_, e)| {
                e.face.glyph_index('\u{1F600}') != 0
                    && cluster
                        .chars()
                        .all(|c| joiner(c) || e.face.glyph_index(c) != 0)
            });
            if let Some((index, _)) = font {
                for entry in chars {
                    entry.1 = index;
                }
            }
        }
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
            // A character of no width never takes a fallback font: whatever glyph one has for it
            // is not shown (Qt gives it no advance in any font), and switching fonts would cut
            // the run (and its kerning) around it. It stays with the font before it.
            if is_zero_width(ch) {
                let previous = char_engine_indices.last().map_or(0, |&(_, idx)| idx);
                char_engine_indices.push((ch, previous));
                continue;
            }
            let mut resolved_idx = 0usize;
            let gid = primary.glyph_index(ch);

            // Qt parity: if primary engine has no glyph (0) and character is not whitespace/ignorable,
            // query fallback engines.
            if gid == 0 && ch != ' ' && ch != '\t' && ch != '\n' && ch != '\r' {
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
        Self::keep_emoji_sequences_in_one_font(text, engines, &mut char_engine_indices);

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
        Self::shape_with_engines_directed(text, font, engines, None)
    }

    /// [`shape_with_engines`](Self::shape_with_engines) with the direction of the text fixed:
    /// `Some(true)` shapes right to left (the glyphs come out in visual order), `Some(false)`
    /// left to right, `None` lets the shaper guess it from the characters. A bidi run takes its
    /// direction from its embedding level, never from what its characters happen to be.
    pub fn shape_with_engines_directed(
        text: &str,
        font: &Font,
        engines: &[FontEngine],
        right_to_left: Option<bool>,
    ) -> Self {
        if text.is_empty() || engines.is_empty() {
            return Self::empty();
        }

        // Qt sets the HarfBuzz font's ppem to the whole pixel size (`hb_font_set_ppem(int(ppem))`),
        // which selects the GPOS device-table adjustments.
        let ppem = font.size as u16;

        let mut runs = Self::partition_into_runs(text, engines);
        // Right to left text is laid out from its right end: the first font run in the text is the
        // rightmost, so the runs go in visual order.
        if right_to_left == Some(true) {
            runs.reverse();
        }
        let mut glyphs = Vec::with_capacity(text.len());
        let mut current_x = 0.0;
        let mut current_y = 0.0;
        let subpixel = crate::text::font_database::layout_has_subpixel_advances();

        for run in runs {
            // `run.text` is a subslice of `text`, so the pointer difference is its byte offset.
            let run_offset = run.text.as_ptr() as usize - text.as_ptr() as usize;
            let engine = &engines[run.engine_index];
            let shaping_face = engine.shaping_face(ppem);

            if let Some(rb_face) = shaping_face.as_deref().map(ShapingFace::face) {
                // Qt `shapeTextWithHarfbuzzNG` path:
                let mut buffer = rustybuzz::UnicodeBuffer::new();
                buffer.push_str(run.text);
                if let Some(rtl) = right_to_left {
                    buffer.set_direction(if rtl {
                        rustybuzz::Direction::RightToLeft
                    } else {
                        rustybuzz::Direction::LeftToRight
                    });
                }

                // HarfBuzz applies a `kern` table itself when the font has no GPOS kerning; the pairs
                // are then applied below, scaled as HarfBuzz scales them (see `legacy_kern_table`).
                let legacy_kern = legacy_kern_table(rb_face);
                let features = shaping_features(font, legacy_kern.is_some());
                let legacy_kern = legacy_kern.filter(|_| font.kerning_enabled());

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
                    subpixel,
                    legacy_kern.as_ref(),
                );

                // A character of no width is hidden as HarfBuzz hides default ignorables: the
                // space glyph, no advance, whatever glyph or advance the font has for it.
                let hidden_glyph = rb_face.glyph_index(' ').map_or(0, |g| g.0);
                for (i, (info, pos)) in infos.iter().zip(positions.iter()).enumerate() {
                    let hidden = text[run_offset + info.cluster as usize..]
                        .chars()
                        .next()
                        .is_some_and(is_zero_width);
                    // `engine_metrics` is Qt's own advance and x offset in 26.6 pixels; without a
                    // font engine advance HarfBuzz's design values stand.
                    let (adv, x_offset) = match &engine_metrics {
                        _ if hidden => (0.0, 0.0),
                        Some(m) => (m[i].0 as f32 / 64.0, m[i].1 as f32 / 64.0),
                        None => (
                            (pos.x_advance as f32) * scale,
                            (pos.x_offset as f32) * scale,
                        ),
                    };
                    let x = current_x + x_offset;
                    let y = current_y + (pos.y_offset as f32) * scale;
                    let mut advance = adv;
                    current_x += adv;
                    // `QTextEngine::shapeText`: spacing goes after the last glyph of every cluster.
                    if infos
                        .get(i + 1)
                        .is_none_or(|next| next.cluster != info.cluster)
                    {
                        let spacing = qt_letter_spacing(font.letter_spacing);
                        current_x += spacing;
                        advance += spacing;
                    }
                    glyphs.push(PositionedGlyph {
                        glyph_id: if hidden {
                            hidden_glyph
                        } else {
                            info.glyph_id as u16
                        },
                        font_index: run.engine_index as u8,
                        x,
                        y,
                        cluster: run_offset + info.cluster as usize,
                        advance,
                    });
                    current_y += (pos.y_advance as f32) * scale;
                }
            } else {
                // Fallback path (per-glyph face metrics) for runs without raw binary font data:
                let tnum_width = if font.feature_value(FontTag::TNUM) != 0 {
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

                let hidden_glyph = engine.face.glyph_index(' ');
                for (byte, ch) in run.text.char_indices() {
                    let hidden = is_zero_width(ch);
                    let gid = if hidden {
                        hidden_glyph
                    } else {
                        engine.face.glyph_index(ch)
                    };
                    let metrics = engine.face.metrics(ch, font.size);
                    let adv_x = if hidden {
                        0.0
                    } else if ch.is_ascii_digit() && run.engine_index == 0 {
                        tnum_width.unwrap_or(metrics.advance_width)
                    } else {
                        engine
                            .face
                            .layout_advance_width(gid, font.size, subpixel)
                            .unwrap_or(metrics.advance_width)
                    };

                    let advance = adv_x + qt_letter_spacing(font.letter_spacing);
                    glyphs.push(PositionedGlyph {
                        glyph_id: gid,
                        font_index: run.engine_index as u8,
                        x: current_x,
                        y: current_y,
                        cluster: run_offset + byte,
                        advance,
                    });
                    current_x += advance;
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
    use crate::text::glyph_face::parse_face;
    use std::sync::Arc;

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

    /// A face without OpenType data (the macOS system fallback faces) is measured glyph by glyph
    /// from its own metrics; a character of no width must still get no advance and no visible
    /// glyph, whatever glyph the face has for it (Qt: 0 in every font).
    #[test]
    fn zero_width_characters_have_no_advance_without_opentype_data() {
        let data =
            SharedFontData::from_vec(include_bytes!("../../tests/data/oracle_text.ttf").to_vec());
        let face = parse_face(&data, 0).unwrap();
        // No font data on the font: the engine has no OpenType data to shape with.
        let font = Font::new("FerrokitOracleText", 16.0);
        let ab = GlyphLayout::shape_with_face("ab", &font, Arc::clone(&face)).width;
        let space = face.glyph_index(' ');
        let mut wrong = Vec::new();
        for c in [
            '\u{ad}', '\u{61c}', '\u{202e}', '\u{2066}', '\u{fe0f}', '\u{2029}', '\u{feff}',
        ] {
            let layout = GlyphLayout::shape_with_face(&format!("a{c}b"), &font, Arc::clone(&face));
            let hidden = &layout.glyphs[1];
            if layout.width != ab || hidden.advance != 0.0 || hidden.glyph_id != space {
                wrong.push(format!("{c:?}: {layout:?}"));
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    #[test]
    fn only_real_emoji_sequences_need_the_emoji_fallback() {
        for text in [
            "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}", // family
            "\u{1F469}\u{1F3FD}\u{200D}\u{1F4BB}",         // skin tone before the ZWJ
            "\u{2764}\u{FE0F}\u{200D}\u{1F525}",           // VS16 before the ZWJ
            "x\u{200D}\u{1F525}",                          // ZWJ before a pictograph
            "\u{2764}\u{FE0F}",                            // emoji presentation
            "1\u{FE0F}\u{20E3}",                           // keycap
            "#\u{20E3}",                                   // keycap without VS16
            "a \u{2600}\u{FE0F} b",
            "\u{1F1F9}\u{1F1FC}", // a flag: two regional indicators
            "\u{1F3F4}\u{E0067}\u{E0062}\u{E0073}\u{E0063}\u{E0074}\u{E007F}", // tag sequence
        ] {
            assert!(GlyphLayout::has_emoji_sequence(text), "{text:?}");
        }
        for text in [
            "plain",
            "\u{0645}\u{200D}",                 // Arabic joining
            "\u{0645}\u{06CC}\u{200C}\u{200D}", // Persian
            "\u{0915}\u{094D}\u{200D}\u{0937}", // Devanagari half form
            "a\u{FE0F}",                        // no emoji form
            "x\u{20E3}",                        // not a keycap base
            "\u{1F3FB}\u{200D}a",               // a lone modifier is no pictograph
            "\u{1F1F9} x",                      // a lone regional indicator
            "a\u{E0067}\u{E007F}",              // tags after a letter
        ] {
            assert!(!GlyphLayout::has_emoji_sequence(text), "{text:?}");
        }
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
            .with_feature(FontTag::TNUM, 1)
            .with_font_data(data);

        // Single glyphs: Arial kerns the pair `11` (HarfBuzz applies it whatever `tnum` says), so a
        // run of ones is narrower than a run of eights in fonts that carry that pair.
        let layout_1 = GlyphLayout::shape("1", &font_tnum, font_face.clone());
        let layout_8 = GlyphLayout::shape("8", &font_tnum, font_face);

        assert_eq!(layout_1.glyphs.len(), 1);
        assert_eq!(layout_8.glyphs.len(), 1);

        assert!((layout_1.width - layout_8.width).abs() < 1.0);
    }

    /// A glyph of `cluster` (a byte offset) with pen advance `advance`.
    fn glyph(cluster: usize, advance: f32) -> PositionedGlyph {
        PositionedGlyph {
            glyph_id: 1,
            font_index: 0,
            x: 0.0,
            y: 0.0,
            cluster,
            advance,
        }
    }

    fn layout_of(glyphs: Vec<PositionedGlyph>) -> GlyphLayout {
        let width = glyphs.iter().map(|g| g.advance).sum();
        GlyphLayout { glyphs, width }
    }

    #[test]
    fn grapheme_advances_divide_a_ligature_evenly() {
        // "ffi!": one `ffi` ligature glyph over three graphemes, then `!`.
        let layout = layout_of(vec![glyph(0, 9.0), glyph(3, 2.0)]);
        assert_eq!(
            layout.grapheme_advances(&[0, 1, 2, 3, 4]),
            vec![3.0, 3.0, 3.0, 2.0]
        );
    }

    #[test]
    fn grapheme_advances_keep_kerning_on_the_first_glyph_of_the_pair() {
        // "AV": the pair kern shortens the advance of `A`; measured one grapheme at a time `A`
        // would be 8 wide.
        let layout = layout_of(vec![glyph(0, 7.0), glyph(1, 8.0)]);
        let advances = layout.grapheme_advances(&[0, 1, 2]);
        assert_eq!(advances, vec![7.0, 8.0]);
        assert_eq!(advances.iter().sum::<f32>(), layout.width);
    }

    #[test]
    fn grapheme_advances_are_in_logical_order_for_right_to_left_glyphs() {
        // Three Hebrew letters (two bytes each); rustybuzz returns them visually, last first.
        let layout = layout_of(vec![glyph(4, 3.0), glyph(2, 2.0), glyph(0, 1.0)]);
        assert_eq!(layout.grapheme_advances(&[0, 2, 4, 6]), vec![1.0, 2.0, 3.0]);
    }

    #[test]
    fn grapheme_advances_merge_marks_into_their_base_grapheme() {
        // "e" + U+0301 + "x": the mark's glyph shares the base's cluster and has no advance.
        let layout = layout_of(vec![glyph(0, 5.0), glyph(0, 0.0), glyph(3, 4.0)]);
        assert_eq!(layout.grapheme_advances(&[0, 3, 4]), vec![5.0, 4.0]);
    }

    #[test]
    fn grapheme_advances_of_empty_text_are_empty() {
        assert!(GlyphLayout::empty().grapheme_advances(&[0]).is_empty());
        assert!(GlyphLayout::empty().grapheme_advances(&[]).is_empty());
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
            assert_eq!(
                layout.glyphs[0].font_index, 1,
                "Emoji should resolve to fallback engine 1"
            );
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
            let engines = vec![FontEngine::new(font_face), FontEngine::new(emoji_font)];

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

    /// Hebrew and an emoji in one right to left run come from two fonts: the glyphs are laid out
    /// from the right end, so the first character of the text has the largest x.
    #[cfg(windows)]
    #[test]
    fn right_to_left_font_runs_are_laid_out_from_the_right() {
        let font = Font::new("Arial", 16.0);
        let text = "\u{5e9}\u{5dc}\u{5d5}\u{5dd} \u{1F600} \u{5e2}\u{5d5}\u{5dc}\u{5dd}";
        let engines = crate::text::font_database::resolve_font_engines_for_text_global(&font, text);
        assert!(
            engines.len() >= 2,
            "the test needs Arial and a fallback font with the emoji"
        );
        let layout = GlyphLayout::shape_with_engines_directed(text, &font, &engines, Some(true));
        assert!(
            layout.glyphs.iter().any(|g| g.font_index > 0),
            "the emoji comes from a fallback"
        );
        for pair in layout.glyphs.windows(2) {
            assert!(pair[0].x <= pair[1].x, "glyphs go left to right: {pair:?}");
            assert!(
                pair[0].cluster >= pair[1].cluster,
                "right to left: later text is further left: {pair:?}"
            );
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
                assert!(
                    w[1].x >= w[0].x,
                    "Glyph coordinates must advance monotonically"
                );
            }

            // Locate the emoji glyph and verify it has engine_index = 1
            let emoji_glyph = layout.glyphs.iter().find(|g| g.font_index == 1);
            assert!(
                emoji_glyph.is_some(),
                "Emoji should have been shaped with engine 1"
            );
        }
    }

    /// The horizontal tracking (track 0) SFNS.ttf, the macOS system UI font, lists on macOS 26:
    /// sizes in points, the value of each track in font units (2048 per em).
    const SF_TRAK_SIZES: [f32; 21] = [
        6.0, 9.0, 10.0, 11.0, 12.0, 13.0, 14.0, 15.0, 16.0, 17.0, 20.0, 22.0, 24.0, 28.0, 32.0,
        36.0, 50.0, 64.0, 80.0, 100.0, 138.0,
    ];
    const SF_TRAK_TRACKS: [(f32, [i16; 21]); 3] = [
        (
            -1.0,
            [
                54, 10, -4, -16, -28, -40, -50, -60, -68, -80, -76, -58, -34, 0, -2, -7, -14, -21,
                -28, -28, -28,
            ],
        ),
        (
            0.0,
            [
                82, 38, 24, 12, 0, -12, -22, -32, -40, -52, -46, -24, 6, 28, 26, 21, 14, 7, 0, 0, 0,
            ],
        ),
        (
            1.0,
            [
                110, 66, 52, 40, 28, 16, 6, -4, -12, -24, -18, 4, 34, 56, 54, 49, 42, 35, 28, 28,
                28,
            ],
        ),
    ];

    /// A `trak` table with only horizontal data.
    fn trak_table(sizes: &[f32], tracks: &[(f32, &[i16])]) -> Vec<u8> {
        let fixed = |v: f32| ((v * 65536.0) as i32).to_be_bytes();
        let sizes_offset = 12 + 8 + 8 * tracks.len();
        let values_offset = sizes_offset + 4 * sizes.len();
        let mut out = Vec::new();
        out.extend(0x0001_0000u32.to_be_bytes());
        out.extend([0, 0, 0, 12, 0, 0, 0, 0]); // format, horizontal at 12, no vertical, reserved
        out.extend((tracks.len() as u16).to_be_bytes());
        out.extend((sizes.len() as u16).to_be_bytes());
        out.extend((sizes_offset as u32).to_be_bytes());
        for (i, (value, _)) in tracks.iter().enumerate() {
            out.extend(fixed(*value));
            out.extend([0, 0]); // name index
            out.extend(((values_offset + 2 * sizes.len() * i) as u16).to_be_bytes());
        }
        for size in sizes {
            out.extend(fixed(*size));
        }
        for (_, values) in tracks {
            for value in *values {
                out.extend(value.to_be_bytes());
            }
        }
        out
    }

    /// `font` with `extra` tables added, the table directory rebuilt in tag order.
    fn with_tables(font: &[u8], extra: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
        let be16 = |at: usize| usize::from(u16::from_be_bytes([font[at], font[at + 1]]));
        let be32 = |at: usize| {
            u32::from_be_bytes([font[at], font[at + 1], font[at + 2], font[at + 3]]) as usize
        };
        let mut tables: Vec<([u8; 4], Vec<u8>)> = (0..be16(4))
            .map(|i| {
                let record = 12 + 16 * i;
                let (offset, length) = (be32(record + 8), be32(record + 12));
                let tag = [
                    font[record],
                    font[record + 1],
                    font[record + 2],
                    font[record + 3],
                ];
                (tag, font[offset..offset + length].to_vec())
            })
            .collect();
        tables.extend(extra.iter().cloned());
        tables.sort_by_key(|(tag, _)| *tag);
        let mut out = font[..4].to_vec();
        out.extend((tables.len() as u16).to_be_bytes());
        out.extend([0; 6]); // search range, entry selector and range shift: unused by the readers
        let mut body = Vec::new();
        for (tag, data) in &tables {
            out.extend(tag);
            out.extend([0; 4]); // checksum: unchecked
            out.extend(((12 + 16 * tables.len() + body.len()) as u32).to_be_bytes());
            out.extend((data.len() as u32).to_be_bytes());
            body.extend(data);
            while body.len() % 4 != 0 {
                body.push(0);
            }
        }
        out.extend(body);
        out
    }

    fn oracle_with_trak(stat: bool) -> Vec<u8> {
        let tracks: Vec<(f32, &[i16])> = SF_TRAK_TRACKS
            .iter()
            .map(|(value, values)| (*value, values.as_slice()))
            .collect();
        let mut extra = vec![(*b"trak", trak_table(&SF_TRAK_SIZES, &tracks))];
        if stat {
            extra.push((*b"STAT", vec![0, 1, 0, 2, 0, 0, 0, 0]));
        }
        with_tables(include_bytes!("../../tests/data/oracle_text.ttf"), &extra)
    }

    /// HarfBuzz reads the value of the first size at or above the points per em and interpolates
    /// from the size before it; rustybuzz 0.20 extrapolates beyond the table's ends, HarfBuzz
    /// keeps the nearest value.
    #[test]
    fn trak_values_follow_hb_aat_layout_trak_table() {
        let font = oracle_with_trak(true);
        let face = rustybuzz::Face::from_slice(&font, 0).unwrap();
        let trak = face.tables().trak.expect("trak table");
        let units = |ptem: f32| hb_tracking_units(&trak.horizontal, ptem);
        // The listed sizes.
        assert_eq!(units(12.0), 0.0);
        assert_eq!(units(9.0), 38.0);
        assert_eq!(units(20.0), -46.0);
        // Between two sizes: linear.
        assert!((units(18.0) + 50.0).abs() < 1e-4);
        assert_eq!(units(12.5), -6.0);
        // Outside the table: the nearest value, not an extrapolation.
        assert_eq!(units(4.0), 82.0);
        assert_eq!(units(500.0), 0.0);
    }

    /// The tracking Qt's HarfBuzz added to every glyph of the system UI font on macOS 26
    /// (PySide6 6.12, `cocoa` plugin, `ptem == px`): `QFontMetricsF::horizontalAdvance` of one
    /// character minus `QRawFont::advancesForGlyphIndexes`, in 1/64 px, at each size.
    #[test]
    fn tracking_is_scaled_and_rounded_as_qt_measured() {
        for (px, expected) in [(9.0, 11), (12.0, 0), (16.0, -20), (20.0, -29), (24.0, 5)] {
            let units = hb_trak_value(&SF_TRAK_SIZES, &SF_TRAK_TRACKS[1].1, px);
            assert_eq!(hb_scale_tracking(units, px, 2048), expected, "{px}px");
        }
    }

    /// HarfBuzz tracks only fonts with both a `trak` and a `STAT` table.
    #[test]
    fn tracking_needs_a_stat_table() {
        let with_stat = oracle_with_trak(true);
        let face = rustybuzz::Face::from_slice(&with_stat, 0).unwrap();
        let upem = face.units_per_em();
        assert_eq!(
            hb_tracking(&face, 20.0, 20.0),
            hb_scale_tracking(-46.0, 20.0, upem)
        );
        assert_ne!(hb_tracking(&face, 20.0, 20.0), 0);
        // No points per em: HarfBuzz tracks at 12 pt.
        assert_eq!(hb_tracking(&face, 0.0, 20.0), 0);

        let without_stat = oracle_with_trak(false);
        let face = rustybuzz::Face::from_slice(&without_stat, 0).unwrap();
        assert!(face.tables().trak.is_some());
        assert_eq!(hb_tracking(&face, 20.0, 20.0), 0);
    }
}
