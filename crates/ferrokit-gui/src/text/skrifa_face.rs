// SPDX-License-Identifier: LGPL-3.0-only
//! Linux/Unix glyph backend: Qt's FreeType engine (`QFontEngineFT`, qfontengine_ft.cpp) rebuilt
//! from `skrifa` hinted outlines and a port of FreeType's rasterizer ([`ftgrays`]).
//!
//! A glyph goes through the steps `QFontEngineFT::loadGlyph` takes it through:
//!
//! * `FT_Set_Char_Size(face, xsize, ysize, 0, 0)` with `ysize = qRound(pixelSize * 64)`
//!   (`QFreetypeFace::computeSize`);
//! * `FT_Load_Glyph` with Qt's load flags (`QFontEngineFT::loadFlags`): `FT_LOAD_NO_HINTING` for
//!   [`HintStyle::HintNone`], else `FT_LOAD_TARGET_LIGHT` for [`HintStyle::HintLight`] and
//!   `FT_LOAD_TARGET_NORMAL` for the other two. FreeType then hints a TrueType glyph in light mode
//!   with its auto-hinter (the TrueType driver does not hint lightly) and a CFF glyph with the CFF
//!   driver; in normal mode a TrueType font with no `fpgm` and a tiny `prep` goes to the
//!   auto-hinter too, and a tricky font never does (`FT_Load_Glyph`, ftobjs.c). skrifa follows the
//!   same rules (`Engine::Auto`, `Engine::AutoFallback`, `require_interpreter`). Its outline
//!   points are FreeType's 26.6 points;
//! * `FT_Set_Transform(face, &matrix, &delta)`: the device scale (glyphs are then loaded
//!   unhinted, `loadGlyphFor`) and the sub-pixel position the raster engine asks for, applied to
//!   the outline after hinting;
//! * `FT_Render_Glyph` in `FT_RENDER_MODE_LIGHT` or `FT_RENDER_MODE_NORMAL` (both render alike),
//!   which [`ftgrays::render`] ports, bitmap box included.
//!
//! The advance a glyph is laid out with is `recalcAdvances`': the linear advance
//! (`linearHoriAdvance >> 10`) when the engine uses design metrics (hint style none or light),
//! else `TRUNC(ROUND(advance.x))` of the hinted glyph. Character map, line metrics, average width
//! and outlines for paths come from [`OutlineFace`], as they did before this backend existed.
//!
//! Not covered: LCD glyphs (Qt renders them only for `Format_A32`, which its raster engine asks for
//! only when painting a widget, never for a `QImage`), embedded bitmaps and colour glyphs (left to
//! the painter's colour glyph path), fontconfig's per-font `hintstyle` and `autohint` (the hint
//! style is the process default or the font's hinting preference), synthetic bold and oblique,
//! and the switch to auto-hinting Qt makes after `FT_Err_Execution_Too_Long`.
//!
//! # Vendored crates
//!
//! `skrifa` and `read-fonts` come from `third_party/` (see the `README.ferrokit.md` of each):
//! the pen learns which points are implied ones, so `SlotPen` rebuilds FreeType's point array;
//! the TrueType interpreter uses FreeType 2.14's move arithmetic and its choice of the
//! auto-hinter; and CFF stem darkening, which Qt switches on (`qt_getFreetypeData`), is ported.
//! Qt's FreeType engine also blends the grey-scale glyphs of CFF fonts with the text gamma
//! (`expectsGammaCorrectedBlending`), which [`GlyphFace::gamma_corrected_blending`] tells the painter.
//! With them, against FreeType 2.14.2 on 24 TrueType and 33 CFF faces (Latin-1 text, 9 to 24 px)
//! outline point arrays and bitmaps are identical for every TrueType glyph in normal mode
//! (73,344 renders) and unhinted (293,376), for every CFF glyph in all three modes (907,632), and
//! for 253,048 of 293,376 TrueType glyphs in light mode before the vertical separation
//! adjustment below was ported.
//!
//! # Light hinting of TrueType accents and dots
//!
//! FreeType 2.14.0 added a vertical separation adjustment to its auto-hinter (`afadjust.c`,
//! commit `7099b09e9`, "[autofit] Implement vertical separation adjustment"). It moves the dot of
//! `i` and `j` and the accents of accented letters vertically so that they keep a gap to the
//! letter below. The vendored skrifa auto-hinter has it (see `third_party/skrifa/README.ferrokit.md`
//! and `adjust/mod.rs`, with the FreeType line of every ported function). It includes what the
//! HarfBuzz build of FreeType adds: the reverse character map of the adjustment is extended
//! through the GSUB single and alternate substitutions (`add_substitute`, `adjust/gsub.rs`), the
//! blue strings of Indic, Khmer, Myanmar and USE scripts measure the dotted circle that HarfBuzz
//! inserts before a lone dependent sign and hide the default ignorable characters, the
//! alternate substitutions of a feature style (Verdana's `c2sc`) are applied, and the style
//! ranges are those of FreeType 2.14.2's `afranges.c`. Measured over 40 TrueType fonts, about
//! 1,270 characters each (Latin-1, accented Latin, Greek, Cyrillic), 8 to 24 px: 3,050,616 of
//! 3,050,616 light renders equal FreeType 2.14.2's (system library), 762,654 of 762,654
//! `QRawFont::alphaMapForGlyph` records equal Qt 6.12.0's, 330,224 of 330,224 single glyph
//! `drawText` renders equal Qt's. By glyph index (every glyph of the 40 fonts, 9, 12, 16 and
//! 20 px, Normal and Light with a sub-pixel offset): 1,364,418 of 1,364,424 equal FreeType's and
//! 454,808 of 454,808 equal Qt's. The target is the Ubuntu build of FreeType 2.14.2, which loads
//! HarfBuzz at run time; other builds of FreeType may differ.
//!
//! # Known differences
//!
//! - 6 of the by-glyph-index records differ: Normal mode (`FT_LOAD_TARGET_NORMAL`) at 9 px for
//!   U+05D8 and U+FB38 of DejaVu Sans Bold and U+266B in the four Liberation Mono faces. Qt's
//!   default (light) hinting is not affected; the cause is not established.
//! - FreeType's coverage computation (`af_shaper_get_coverage_hb`) leaves out the glyphs that
//!   the GPOS table of a feature covers; the vendored skrifa does not read GPOS. No difference
//!   was measured in the 40 fonts.
//! - The style class of Nyiakeng Puachue Hmong, which FreeType 2.14.2 does not have, is still in
//!   the table of the vendored skrifa, without characters.
//!
//! CFF fonts (hinted by the CFF engine, which has no such adjustment) and every other hint style
//! are not affected.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use crate::text::font::SharedFontData;
use crate::text::font_database::HintStyle;
use crate::text::ftgrays::{self, Outline, Vector};
use crate::text::glyph_face::{GlyphFace, GlyphMetrics, LineMetrics, subpixel_bucket_offset};
use crate::text::outline_face::OutlineFace;
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{
    DrawSettings, Engine, GlyphStyles, HintingInstance, HintingOptions, OutlineGlyphCollection,
    OutlineGlyphFormat, OutlinePen, SmoothMode, Target,
};
use skrifa::{FontRef, GlyphId, MetadataProvider};

/// Most hinting instances (sizes) and hinted advances a face keeps before it starts over.
const INSTANCE_CACHE_LIMIT: usize = 32;
const ADVANCE_CACHE_LIMIT: usize = 8192;

/// The glyph slot `FT_Load_Glyph` fills: the outline in 26.6 pixels (y up, origin on the pen
/// position) and `advance.x`.
struct Slot {
    outline: Outline,
    advance: i64,
}

/// Collects a skrifa outline as the points, tags and contour ends of an `FT_Outline`.
///
/// skrifa draws a glyph as FreeType's `FT_Outline_Decompose` walks it: contours start at the same
/// point and two consecutive off-curve points have an implied on-curve point between them. skrifa
/// hands that point to the pen as an explicit one, rounded to 1/64 pixel, but FreeType's point
/// array does not have it and its rasterizer works at the exact midpoint (`gray_conic_to`). The
/// vendored read-fonts (`third_party/read-fonts`) tells the pen about such a point
/// ([`OutlinePen::implied_point`], called before the command that ends on it): it is dropped, and
/// the contour keeps the off-curve points around it, as in FreeType. That includes the start of a
/// contour that begins with an off-curve point and the return to the start that closes a curve.
///
/// A CFF contour (`cff`) does not repeat its first point at the end (`cff_builder_close_contour`).
#[derive(Default)]
struct SlotPen {
    outline: Outline,
    open: bool,
    /// The outline comes from the CFF driver.
    cff: bool,
    /// The end point of the next command is an implied point.
    implied: bool,
    /// Index in `outline.points` of the open contour's first point.
    first: usize,
}

impl SlotPen {
    fn new(cff: bool) -> Self {
        Self {
            cff,
            ..Self::default()
        }
    }

    fn push(&mut self, x: f32, y: f32, tag: u8) {
        // skrifa's coordinates are 26.6 (TrueType, auto-hinter) or 16.16 (CFF) values as `f32`;
        // FreeType keeps 26.6.
        self.outline.points.push(Vector::new(
            (f64::from(x) * 64.0).round() as i64,
            (f64::from(y) * 64.0).round() as i64,
        ));
        self.outline.tags.push(tag);
    }

    /// The on-curve end point of a command, unless it is an implied one.
    fn push_end(&mut self, x: f32, y: f32) {
        if !std::mem::take(&mut self.implied) {
            self.push(x, y, ftgrays::TAG_ON);
        }
    }

    fn end_contour(&mut self) {
        if !self.open {
            return;
        }
        self.open = false;
        let n = self.outline.points.len() - self.first;
        if self.cff
            && n > 1
            && self.outline.tags.last() == Some(&ftgrays::TAG_ON)
            && self.outline.points.last() == self.outline.points.get(self.first)
        {
            // `cff_builder_close_contour`: a contour does not repeat its first point.
            self.outline.points.pop();
            self.outline.tags.pop();
        }
        if self.outline.points.len() > self.first {
            let last = self.outline.points.len() - 1;
            self.outline
                .contours
                .push(last.min(usize::from(u16::MAX)) as u16);
        }
    }

    fn finish(mut self) -> Outline {
        self.end_contour();
        self.outline
    }
}

impl OutlinePen for SlotPen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.end_contour();
        self.open = true;
        self.first = self.outline.points.len();
        self.push_end(x, y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.push(x, y, ftgrays::TAG_ON);
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        self.push(cx, cy, ftgrays::TAG_CONIC);
        self.push_end(x, y);
    }
    fn curve_to(&mut self, c0x: f32, c0y: f32, c1x: f32, c1y: f32, x: f32, y: f32) {
        self.push(c0x, c0y, ftgrays::TAG_CUBIC);
        self.push(c1x, c1y, ftgrays::TAG_CUBIC);
        self.push_end(x, y);
    }
    fn close(&mut self) {
        self.end_contour();
    }
    fn implied_point(&mut self) {
        self.implied = true;
    }
}

/// Which hinter a glyph goes through (see the module documentation).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Hinter {
    /// TrueType or CFF instructions.
    Native,
    /// FreeType's auto-hinter.
    Auto,
}

/// Hinting instances by `ysize` and hinter; `None` where skrifa cannot make one.
type InstanceCache = HashMap<(i64, Hinter), Option<Arc<HintingInstance>>>;

/// A face rendered as Qt's FreeType engine renders it for one hint style, fixed when the face is
/// built (`QFontEngineFT::default_hint_style`).
pub struct SkrifaFace {
    base: OutlineFace,
    data: SharedFontData,
    face_index: u32,
    hint_style: HintStyle,
    units_per_em: u16,
    /// The outlines are CFF ones (not TrueType): see [`SlotPen`].
    cff: bool,
    /// The hinter of `FT_LOAD_TARGET_LIGHT` / `FT_LOAD_TARGET_NORMAL` for this font.
    light_hinter: Hinter,
    normal_hinter: Hinter,
    /// Auto-hinter glyph styles, computed once per font (`af_face_globals_new`).
    styles: OnceLock<GlyphStyles>,
    /// Hinting instances by `ysize` (`FT_Size` with its `prep` run) and hinter.
    instances: Mutex<InstanceCache>,
    /// `TRUNC(ROUND(advance.x))` of hinted glyphs by glyph and `ysize`.
    advances: Mutex<HashMap<(u16, i64), i64>>,
}

impl SkrifaFace {
    /// Builds the face `face_index` of `data` for `hint_style`; fails when skrifa cannot read
    /// the font's outlines (the caller then falls back to [`OutlineFace`]).
    pub fn new(
        data: SharedFontData,
        face_index: u32,
        hint_style: HintStyle,
    ) -> Result<Self, String> {
        let base = OutlineFace::new(data.clone(), face_index)?;
        let font = FontRef::from_index(data.as_slice(), face_index)
            .map_err(|e| format!("skrifa cannot read the font: {e}"))?;
        let outlines = font.outline_glyphs();
        let format = outlines
            .format()
            .ok_or_else(|| "the font has no scalable outlines".to_string())?;
        let units_per_em = font
            .metrics(Size::unscaled(), LocationRef::default())
            .units_per_em;
        if units_per_em == 0 {
            return Err("the font has no units per em".to_string());
        }
        let tricky = outlines.require_interpreter();
        let native_first = tricky || outlines.prefer_interpreter();
        let is_glyf = format == OutlineGlyphFormat::Glyf;
        let normal_hinter = if native_first {
            Hinter::Native
        } else {
            Hinter::Auto
        };
        // The CFF driver hints lightly; the TrueType driver does not, so FreeType auto-hints.
        let light_hinter = if tricky || (!is_glyf && native_first) {
            Hinter::Native
        } else {
            Hinter::Auto
        };
        Ok(Self {
            base,
            data,
            face_index,
            hint_style,
            units_per_em,
            cff: !is_glyf,
            light_hinter,
            normal_hinter,
            styles: OnceLock::new(),
            instances: Mutex::new(HashMap::new()),
            advances: Mutex::new(HashMap::new()),
        })
    }

    /// The hint style this face renders with.
    pub fn hint_style(&self) -> HintStyle {
        self.hint_style
    }

    fn font(&self) -> Option<FontRef<'_>> {
        FontRef::from_index(self.data.as_slice(), self.face_index).ok()
    }

    /// `FT_LOAD_TARGET_LIGHT` or `FT_LOAD_TARGET_NORMAL`, and the hinter FreeType picks for it;
    /// `None` for `FT_LOAD_NO_HINTING`.
    fn load_target(&self) -> Option<(SmoothMode, Hinter)> {
        match self.hint_style {
            HintStyle::HintNone => None,
            HintStyle::HintLight => Some((SmoothMode::Light, self.light_hinter)),
            HintStyle::HintMedium | HintStyle::HintFull => {
                Some((SmoothMode::Normal, self.normal_hinter))
            }
        }
    }

    /// The hinting instance of `FT_Size` at `ysize` (26.6) for `mode` and `hinter`.
    fn instance(
        &self,
        outlines: &OutlineGlyphCollection,
        ysize: i64,
        mode: SmoothMode,
        hinter: Hinter,
    ) -> Option<Arc<HintingInstance>> {
        let mut instances = self.instances.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(found) = instances.get(&(ysize, hinter)) {
            return found.clone();
        }
        let engine = match hinter {
            Hinter::Native => Engine::Interpreter,
            Hinter::Auto => Engine::Auto(Some(
                self.styles
                    .get_or_init(|| GlyphStyles::new(outlines))
                    .clone(),
            )),
        };
        let options = HintingOptions {
            engine,
            target: Target::Smooth {
                mode,
                symmetric_rendering: true,
                preserve_linear_metrics: false,
            },
            // Qt's FreeType engine turns CFF stem darkening on (`qt_getFreetypeData`)
            cff_stem_darkening: self.cff,
        };
        let instance = HintingInstance::new(
            outlines,
            Size::new(ysize as f32 / 64.0),
            LocationRef::default(),
            options,
        )
        .ok()
        .map(Arc::new);
        if instances.len() >= INSTANCE_CACHE_LIMIT {
            instances.clear();
        }
        instances.insert((ysize, hinter), instance.clone());
        instance
    }

    /// `FT_Load_Glyph` of `glyph_id` at `ysize` (26.6 pixels per em), hinted unless `unhinted`.
    /// `None` where FreeType fails to load the glyph (Qt then draws nothing and gives it no
    /// advance).
    fn load(&self, glyph_id: u16, ysize: i64, unhinted: bool) -> Option<Slot> {
        let font = self.font()?;
        let outlines = font.outline_glyphs();
        let glyph = outlines.get(GlyphId::new(u32::from(glyph_id)))?;
        let size = Size::new(ysize as f32 / 64.0);
        let target = if unhinted { None } else { self.load_target() };
        let draw = |settings: DrawSettings| {
            let mut pen = SlotPen::new(self.cff);
            let metrics = glyph.draw(settings, &mut pen).ok()?;
            let mut outline = pen.finish();
            if metrics.has_overlaps {
                outline.flags |= ftgrays::OUTLINE_OVERLAP;
            }
            Some((outline, metrics.advance_width))
        };
        let (outline, advance) = match target {
            None => draw(
                DrawSettings::unhinted(size, LocationRef::default())
                    .with_cff_stem_darkening(self.cff),
            )?,
            Some((mode, hinter)) => {
                let hinted = self
                    .instance(&outlines, ysize, mode, hinter)
                    .and_then(|instance| draw(DrawSettings::hinted(&instance, false)));
                match hinted {
                    Some(loaded) => loaded,
                    // `FT_Err_Too_Few_Arguments` and the like: Qt loads the glyph again with
                    // `FT_LOAD_FORCE_AUTOHINT`.
                    None if hinter != Hinter::Auto => {
                        let instance = self.instance(&outlines, ysize, mode, Hinter::Auto)?;
                        draw(DrawSettings::hinted(&instance, false))?
                    }
                    None => return None,
                }
            }
        };
        let advance = match advance {
            Some(advance) => (f64::from(advance) * 64.0).round() as i64,
            // A CFF glyph without a width of its own: the `hmtx` advance, as FreeType takes it.
            None => {
                let design = i64::from(self.base.glyph_hor_advance(glyph_id).unwrap_or(0));
                let scaled = ftgrays::mul_fix(design, self.x_scale(ysize));
                if target.is_some() {
                    (scaled + 32) & -64
                } else {
                    scaled
                }
            }
        };
        Some(Slot { outline, advance })
    }

    /// `size->metrics.x_scale` for `FT_Set_Char_Size(face, ysize, ysize, 0, 0)`.
    fn x_scale(&self, ysize: i64) -> i64 {
        ftgrays::div_fix(ysize, i64::from(self.units_per_em))
    }

    /// `qRound(pixelSize * 64)`: the 26.6 size `QFreetypeFace::computeSize` sets.
    fn ysize(px: f32) -> i64 {
        (f64::from(px) * 64.0 + 0.5).floor() as i64
    }

    /// The outline of `glyph_id` at `size` pixels per em drawn through the uniform device scale
    /// `scale`, moved right by `x_offset` 64ths of a device pixel: the glyph slot outline
    /// `QFontEngineFT::loadGlyph` hands to `FT_Render_Glyph`. `None` where FreeType fails to load
    /// the glyph.
    pub fn freetype_outline(
        &self,
        glyph_id: u16,
        size: f32,
        scale: f32,
        x_offset: i32,
    ) -> Option<Outline> {
        if !(size > 0.0 && scale > 0.0) {
            return None;
        }
        let ysize = Self::ysize(size);
        if ysize <= 0 {
            return None;
        }
        // `loadGlyphFor`: a scaled (not rotated) glyph is loaded without hinting.
        let transformed = scale != 1.0;
        let mut outline = self.load(glyph_id, ysize, transformed)?.outline;
        if transformed {
            // `QTransformToFTMatrix`: `FT_Fixed(m11 * 65536)`, truncated.
            let m = (f64::from(scale) * 65536.0) as i64;
            ftgrays::transform(&mut outline.points, m, 0, 0, m);
        }
        if x_offset != 0 {
            for v in &mut outline.points {
                v.x = v.x.wrapping_add(i64::from(x_offset));
            }
        }
        Some(outline)
    }

    /// `x_ppem` and `y_ppem` of the FreeType size for `size` pixels per em.
    fn ppem(size: f32) -> (u16, u16) {
        let ppem = ((Self::ysize(size) + 32) >> 6).clamp(0, i64::from(u16::MAX)) as u16;
        (ppem, ppem)
    }

    /// `QFontEngine::glyphBoundingBoxExceedsLimit` with Qt's default `QT_MAX_GLYPH_SIZE` of 4096:
    /// a glyph whose control box is beyond 16 times the pixel size (and 256 pixels) is not drawn.
    fn exceeds_qt_glyph_limit(outline: &Outline, size: f32, scale: f32) -> bool {
        const LIMIT: f64 = 4096.0;
        let cbox = ftgrays::control_box(&outline.points);
        let width = ((cbox.x_max - cbox.x_min) >> 6) as f64;
        let height = ((cbox.y_max - cbox.y_min) >> 6) as f64;
        let effective = f64::from(size) * f64::from(size) * f64::from(scale) * f64::from(scale);
        if effective > LIMIT * LIMIT {
            return true;
        }
        let max = (LIMIT * LIMIT).min((256.0f64 * 256.0).max(256.0 * effective));
        width * width > max || height * height > max
    }

    /// The advance `QFontEngineFT::recalcAdvances` lays the glyph out with, in 26.6 pixels.
    fn layout_advance_26_6(&self, glyph_id: u16, size: f32) -> i64 {
        let ysize = Self::ysize(size);
        if ysize <= 0 {
            return 0;
        }
        match self.hint_style {
            // `shouldUseDesignMetrics`: `QFixed::fromFixed(linearHoriAdvance >> 10)`, the linear
            // advance `FT_MulDiv(advance, x_scale, 64)` of `FT_Load_Glyph`.
            HintStyle::HintNone | HintStyle::HintLight => {
                if self.load(glyph_id, ysize, false).is_none() {
                    return 0;
                }
                let design = i64::from(self.base.glyph_hor_advance(glyph_id).unwrap_or(0));
                ftgrays::mul_div(design, self.x_scale(ysize), 64) >> 10
            }
            // `QFixed(g->advance)`: `TRUNC(ROUND(slot->advance.x))` of the hinted glyph.
            HintStyle::HintMedium | HintStyle::HintFull => {
                let key = (glyph_id, ysize);
                if let Some(&advance) = self
                    .advances
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&key)
                {
                    return advance;
                }
                let advance = self
                    .load(glyph_id, ysize, false)
                    .map_or(0, |slot| ((slot.advance + 32) >> 6) << 6);
                let mut advances = self.advances.lock().unwrap_or_else(|e| e.into_inner());
                if advances.len() >= ADVANCE_CACHE_LIMIT {
                    advances.clear();
                }
                advances.insert(key, advance);
                advance
            }
        }
    }

    /// `FT_Render_Glyph` of [`freetype_outline`](Self::freetype_outline), as glyph metrics and
    /// coverage.
    fn render(
        &self,
        glyph_id: u16,
        size: f32,
        scale: f32,
        x_offset: i32,
    ) -> (GlyphMetrics, Vec<u8>) {
        let advance_width = self.layout_advance_26_6(glyph_id, size) as f32 / 64.0;
        let empty = GlyphMetrics {
            advance_width,
            ..GlyphMetrics::default()
        };
        let Some(outline) = self.freetype_outline(glyph_id, size, scale, x_offset) else {
            return (empty, Vec::new());
        };
        if Self::exceeds_qt_glyph_limit(&outline, size, scale) {
            return (empty, Vec::new());
        }
        // Qt asks FreeType for bitmaps at the size of the font, not the device size.
        let Ok(bitmap) = ftgrays::render(&outline, Self::ppem(size)) else {
            return (empty, Vec::new());
        };
        let metrics = GlyphMetrics {
            xmin: bitmap.left,
            ymin: bitmap.top.wrapping_sub(bitmap.rows as i32),
            width: bitmap.width as usize,
            height: bitmap.rows as usize,
            advance_width,
            advance_height: 0.0,
        };
        (metrics, bitmap.buffer)
    }
}

impl GlyphFace for SkrifaFace {
    fn glyph_index(&self, ch: char) -> u16 {
        self.base.glyph_index(ch)
    }

    fn face_index(&self) -> u32 {
        self.face_index
    }

    /// The bitmap box of the glyph (`FT_Render_Glyph` without the coverage).
    fn metrics_indexed(&self, glyph_id: u16, px: f32) -> GlyphMetrics {
        let advance_width = self.layout_advance_26_6(glyph_id, px) as f32 / 64.0;
        let Some(outline) = self.freetype_outline(glyph_id, px, 1.0, 0) else {
            return GlyphMetrics {
                advance_width,
                ..GlyphMetrics::default()
            };
        };
        match ftgrays::preset_bitmap(&outline.points, Self::ppem(px)) {
            Some((left, top, width, rows)) if width > 0 && rows > 0 => GlyphMetrics {
                xmin: left,
                ymin: top.wrapping_sub(rows as i32),
                width: width as usize,
                height: rows as usize,
                advance_width,
                advance_height: 0.0,
            },
            _ => GlyphMetrics {
                advance_width,
                ..GlyphMetrics::default()
            },
        }
    }

    fn rasterize_indexed(&self, glyph_id: u16, px: f32) -> (GlyphMetrics, Vec<u8>) {
        self.render(glyph_id, px, 1.0, 0)
    }

    fn rasterize_scaled(&self, glyph_id: u16, size: f32, scale: f32) -> (GlyphMetrics, Vec<u8>) {
        self.render(glyph_id, size, scale, 0)
    }

    fn freetype_engine(&self) -> bool {
        true
    }

    /// `QFontEngineFT::supportsHorizontalSubPixelPositions`: hint style none or light.
    fn supports_horizontal_subpixel_positions(&self) -> bool {
        matches!(self.hint_style, HintStyle::HintNone | HintStyle::HintLight)
    }

    /// `QFontEngine::subPixelPositionFor` with the four positions per pixel of
    /// `m_subPixelPositionCount`.
    fn subpixel_position(&self, _glyph_id: u16, _size: f32, _scale: f32, x: i64) -> i32 {
        subpixel_bucket_offset(x, 4)
    }

    fn gamma_corrected_blending(&self) -> bool {
        self.cff
    }

    fn rasterize_subpixel(
        &self,
        glyph_id: u16,
        size: f32,
        scale: f32,
        subpixel: i32,
    ) -> (GlyphMetrics, Vec<u8>) {
        self.render(glyph_id, size, scale, subpixel)
    }

    fn line_metrics(&self, px: f32) -> Option<LineMetrics> {
        self.base.line_metrics(px)
    }

    fn average_char_width(&self, px: f32) -> Option<f32> {
        self.base.average_char_width(px)
    }

    fn cap_height(&self, px: f32) -> Option<f32> {
        self.base.cap_height(px)
    }

    fn glyph_outline(&self, glyph_id: u16, px: f32) -> Option<tiny_skia::Path> {
        self.base.glyph_outline(glyph_id, px)
    }

    fn layout_advance_width(&self, glyph_id: u16, size: f32, _direct_write: bool) -> Option<f32> {
        Some(self.layout_advance_26_6(glyph_id, size) as f32 / 64.0)
    }

    fn right_side_bearing(&self, glyph_id: u16, size: f32) -> Option<f32> {
        self.base.right_side_bearing(glyph_id, size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pts(o: &Outline) -> Vec<(i64, i64, u8)> {
        o.points
            .iter()
            .zip(&o.tags)
            .map(|(p, &t)| (p.x, p.y, t))
            .collect()
    }

    #[test]
    fn closing_curve_back_to_an_explicit_start_adds_no_point() {
        let mut pen = SlotPen::default();
        pen.move_to(0.0, 0.0);
        pen.line_to(1.0, 0.0);
        pen.line_to(1.0, 1.0);
        // skrifa closes the contour with the return to its start, which it announces.
        pen.implied_point();
        pen.quad_to(0.5, 1.5, 0.0, 0.0);
        pen.close();
        let o = pen.finish();
        assert_eq!(o.contours, [3]);
        assert_eq!(
            pts(&o),
            [
                (0, 0, ftgrays::TAG_ON),
                (64, 0, ftgrays::TAG_ON),
                (64, 64, ftgrays::TAG_ON),
                (32, 96, ftgrays::TAG_CONIC)
            ]
        );
    }

    #[test]
    fn implied_points_are_not_part_of_the_outline() {
        let mut pen = SlotPen::default();
        // A contour that starts off the curve: its start, and the points between two consecutive
        // off-curve points, are implied.
        pen.implied_point();
        pen.move_to(0.5, 0.0);
        pen.quad_to(1.0, 0.0, 1.0, 0.5);
        pen.implied_point();
        pen.quad_to(1.0, 1.0, 0.5, 0.0);
        pen.close();
        let o = pen.finish();
        assert_eq!(o.contours, [2]);
        assert_eq!(
            pts(&o),
            [
                (64, 0, ftgrays::TAG_CONIC),
                (64, 32, ftgrays::TAG_ON),
                (64, 64, ftgrays::TAG_CONIC)
            ]
        );
    }

    #[test]
    fn a_repeated_last_point_is_kept_in_truetype_outlines_and_dropped_in_cff_ones() {
        let draw = |cff| {
            let mut pen = SlotPen::new(cff);
            pen.move_to(0.0, 0.0);
            pen.line_to(1.0, 0.0);
            pen.line_to(1.0, 1.0);
            pen.line_to(0.0, 0.0);
            pen.close();
            pen.finish()
        };
        assert_eq!(draw(false).contours, [3]);
        assert_eq!(draw(true).contours, [2]);
    }

    #[test]
    fn contours_are_numbered_across_move_commands() {
        let mut pen = SlotPen::default();
        for y in [0.0, 2.0] {
            pen.move_to(0.0, y);
            pen.line_to(1.0, y);
            pen.line_to(1.0, y + 1.0);
            pen.close();
        }
        assert_eq!(pen.finish().contours, [2, 5]);
    }
}
