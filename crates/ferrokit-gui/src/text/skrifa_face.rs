// SPDX-License-Identifier: LGPL-3.0-only
//! Linux/Unix glyph backend: `skrifa` hinted outlines rasterized with `ab_glyph_rasterizer`.
//!
//! A glyph goes through the steps FreeType glyph backend takes it through:
//!
//! * `FT_Set_Char_Size(face, xsize, ysize, 0, 0)` with `ysize = qRound(pixelSize * 64)`
//!   (FreeType symbol charmap);
//! * `FT_Load_Glyph` with Qt's load flags (FreeType glyph backend): `FT_LOAD_NO_HINTING` for
//!   [`HintStyle::HintNone`], else `FT_LOAD_TARGET_LIGHT` for [`HintStyle::HintLight`] and
//!   `FT_LOAD_TARGET_NORMAL` for the other two. FreeType then hints a TrueType glyph in light mode
//!   with its auto-hinter (the TrueType driver does not hint lightly) and a CFF glyph with the CFF
//!   driver; in normal mode a TrueType font with no `fpgm` and a tiny `prep` goes to the
//!   auto-hinter too, and a tricky font never does (`FT_Load_Glyph`, ftobjs.c). skrifa follows the
//!   same rules (`Engine::Auto`, `Engine::AutoFallback`, `require_interpreter`). Its outline
//!   points are 26.6 fixed-point values;
//! * `FT_Set_Transform(face, &matrix, &delta)`: the device scale (glyphs are then loaded
//!   unhinted, `loadGlyphFor`) and the sub-pixel position the raster engine asks for, applied to
//!   the outline after hinting;
//! * anti-aliased area-coverage rasterization via `ab_glyph_rasterizer`.
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
//! This backend uses upstream crates.io `skrifa` + `ab_glyph_rasterizer` (with CFF gamma-corrected
//! blending reported via [`GlyphFace::gamma_corrected_blending`]).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use crate::text::font::SharedFontData;
use crate::text::font_database::HintStyle;
use crate::text::glyph_face::{GlyphFace, GlyphMetrics, LineMetrics, subpixel_bucket_offset};
use crate::text::outline_face::OutlineFace;
use ab_glyph_rasterizer::{Point, Rasterizer, point};
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{
    DrawSettings, Engine, GlyphStyles, HintingInstance, HintingOptions, OutlineGlyphCollection,
    OutlineGlyphFormat, OutlinePen, SmoothMode, Target,
};
use skrifa::{FontRef, GlyphId, MetadataProvider};

/// Most hinting instances (sizes) and hinted advances a face keeps before it starts over.
const INSTANCE_CACHE_LIMIT: usize = 32;
const ADVANCE_CACHE_LIMIT: usize = 8192;

/// An outline drawing command in 26.6 fixed-point pixel coordinates (y up).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OutlineCommand {
    MoveTo {
        x: i64,
        y: i64,
    },
    LineTo {
        x: i64,
        y: i64,
    },
    QuadTo {
        cx: i64,
        cy: i64,
        x: i64,
        y: i64,
    },
    CubicTo {
        c0x: i64,
        c0y: i64,
        c1x: i64,
        c1y: i64,
        x: i64,
        y: i64,
    },
    Close,
}

/// A hinted or unhinted glyph outline in 26.6 fixed-point pixel coordinates (y up).
#[derive(Default, Clone, Debug)]
pub struct GlyphOutline {
    pub commands: Vec<OutlineCommand>,
    pub contours: usize,
    pub has_overlaps: bool,
}

#[derive(Clone, Copy, Debug)]
struct ControlBox {
    x_min: i64,
    y_min: i64,
    x_max: i64,
    y_max: i64,
}

fn mul_fix(a: i64, b: i64) -> i64 {
    let neg = (a < 0) ^ (b < 0);
    let prod = i128::from(a).abs() * i128::from(b).abs();
    let q = ((prod + 0x8000) >> 16) as i64;
    if neg { -q } else { q }
}

fn div_fix(a: i64, b: i64) -> i64 {
    if b == 0 {
        return if a < 0 { i64::MIN } else { i64::MAX };
    }
    let neg = (a < 0) ^ (b < 0);
    let num = i128::from(a).abs() << 16;
    let den = i128::from(b).abs();
    let q = ((num + (den >> 1)) / den) as i64;
    if neg { -q } else { q }
}

fn mul_div(a: i64, b: i64, c: i64) -> i64 {
    if c == 0 {
        return if (a ^ b) < 0 { i64::MIN } else { i64::MAX };
    }
    let neg = (a < 0) ^ (b < 0) ^ (c < 0);
    let num = i128::from(a).abs() * i128::from(b).abs();
    let den = i128::from(c).abs();
    let q = ((num + (den >> 1)) / den) as i64;
    if neg { -q } else { q }
}

fn to_26_6(v: f32) -> i64 {
    (f64::from(v) * 64.0).round() as i64
}

fn control_box(commands: &[OutlineCommand]) -> Option<ControlBox> {
    let mut x_min = i64::MAX;
    let mut y_min = i64::MAX;
    let mut x_max = i64::MIN;
    let mut y_max = i64::MIN;
    let mut any = false;
    let mut include = |x: i64, y: i64| {
        any = true;
        x_min = x_min.min(x);
        y_min = y_min.min(y);
        x_max = x_max.max(x);
        y_max = y_max.max(y);
    };
    for &cmd in commands {
        match cmd {
            OutlineCommand::MoveTo { x, y } | OutlineCommand::LineTo { x, y } => include(x, y),
            OutlineCommand::QuadTo { cx, cy, x, y } => {
                include(cx, cy);
                include(x, y);
            }
            OutlineCommand::CubicTo {
                c0x,
                c0y,
                c1x,
                c1y,
                x,
                y,
            } => {
                include(c0x, c0y);
                include(c1x, c1y);
                include(x, y);
            }
            OutlineCommand::Close => {}
        }
    }
    any.then_some(ControlBox {
        x_min,
        y_min,
        x_max,
        y_max,
    })
}

fn pixel_bounds(commands: &[OutlineCommand]) -> Option<(i32, i32, u32, u32)> {
    let cbox = control_box(commands)?;
    let x_min_px = (cbox.x_min >> 6) as i32;
    let y_min_px = (cbox.y_min >> 6) as i32;
    let x_max_px = ((cbox.x_max + 63) >> 6) as i32;
    let y_max_px = ((cbox.y_max + 63) >> 6) as i32;
    let width = x_max_px.saturating_sub(x_min_px);
    let height = y_max_px.saturating_sub(y_min_px);
    if width <= 0 || height <= 0 {
        return None;
    }
    Some((x_min_px, y_max_px, width as u32, height as u32))
}

fn rasterize_outline(outline: &GlyphOutline) -> Option<(i32, i32, u32, u32, Vec<u8>)> {
    let (left, top, width, rows) = pixel_bounds(&outline.commands)?;
    let mut ras = Rasterizer::new(width as usize, rows as usize);
    let to_ras = |x: i64, y: i64| -> Point {
        point(
            (x as f32 / 64.0) - left as f32,
            top as f32 - (y as f32 / 64.0),
        )
    };
    let mut start = point(0.0, 0.0);
    let mut cur = point(0.0, 0.0);
    let mut has_open = false;
    for &cmd in &outline.commands {
        match cmd {
            OutlineCommand::MoveTo { x, y } => {
                if has_open && cur != start {
                    ras.draw_line(cur, start);
                }
                let p = to_ras(x, y);
                start = p;
                cur = p;
                has_open = true;
            }
            OutlineCommand::LineTo { x, y } => {
                let p = to_ras(x, y);
                ras.draw_line(cur, p);
                cur = p;
            }
            OutlineCommand::QuadTo { cx, cy, x, y } => {
                let c = to_ras(cx, cy);
                let p = to_ras(x, y);
                ras.draw_quad(cur, c, p);
                cur = p;
            }
            OutlineCommand::CubicTo {
                c0x,
                c0y,
                c1x,
                c1y,
                x,
                y,
            } => {
                let c0 = to_ras(c0x, c0y);
                let c1 = to_ras(c1x, c1y);
                let p = to_ras(x, y);
                ras.draw_cubic(cur, c0, c1, p);
                cur = p;
            }
            OutlineCommand::Close => {
                if has_open && cur != start {
                    ras.draw_line(cur, start);
                }
                cur = start;
                has_open = false;
            }
        }
    }
    if has_open && cur != start {
        ras.draw_line(cur, start);
    }
    let mut buffer = vec![0u8; (width * rows) as usize];
    ras.for_each_pixel(|idx, alpha| {
        buffer[idx] = (alpha * 255.0).round().clamp(0.0, 255.0) as u8;
    });
    Some((left, top, width, rows, buffer))
}

/// The glyph slot `FT_Load_Glyph` fills: the outline in 26.6 pixels (y up, origin on the pen
/// position) and `advance.x`.
struct Slot {
    outline: GlyphOutline,
    advance: i64,
}

/// Collects a skrifa outline as [`OutlineCommand`]s in 26.6 pixels.
///
/// A CFF contour (`cff`) does not repeat its first point at the end (`cff_builder_close_contour`).
#[derive(Default)]
struct SlotPen {
    outline: GlyphOutline,
    open: bool,
    /// The outline comes from the CFF driver.
    cff: bool,
    /// Start point of the currently open contour.
    start: Option<(i64, i64)>,
    /// Index in `outline.commands` where the open contour began.
    first_cmd: usize,
}

impl SlotPen {
    fn new(cff: bool) -> Self {
        Self {
            cff,
            ..Self::default()
        }
    }

    fn end_contour(&mut self) {
        if !self.open {
            return;
        }
        self.open = false;
        let cmd_count = self.outline.commands.len() - self.first_cmd;
        if self.cff
            && cmd_count > 1
            && let (Some(OutlineCommand::LineTo { x, y }), Some((sx, sy))) =
                (self.outline.commands.last().copied(), self.start)
            && x == sx
            && y == sy
        {
            // A CFF contour does not repeat its first point with a closing line segment.
            self.outline.commands.pop();
        }
        if self.outline.commands.len() > self.first_cmd {
            self.outline.commands.push(OutlineCommand::Close);
            self.outline.contours += 1;
        }
    }

    fn finish(mut self) -> GlyphOutline {
        self.end_contour();
        self.outline
    }
}

impl OutlinePen for SlotPen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.end_contour();
        self.open = true;
        self.first_cmd = self.outline.commands.len();
        let (qx, qy) = (to_26_6(x), to_26_6(y));
        self.start = Some((qx, qy));
        self.outline
            .commands
            .push(OutlineCommand::MoveTo { x: qx, y: qy });
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.outline.commands.push(OutlineCommand::LineTo {
            x: to_26_6(x),
            y: to_26_6(y),
        });
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        self.outline.commands.push(OutlineCommand::QuadTo {
            cx: to_26_6(cx),
            cy: to_26_6(cy),
            x: to_26_6(x),
            y: to_26_6(y),
        });
    }
    fn curve_to(&mut self, c0x: f32, c0y: f32, c1x: f32, c1y: f32, x: f32, y: f32) {
        self.outline.commands.push(OutlineCommand::CubicTo {
            c0x: to_26_6(c0x),
            c0y: to_26_6(c0y),
            c1x: to_26_6(c1x),
            c1y: to_26_6(c1y),
            x: to_26_6(x),
            y: to_26_6(y),
        });
    }
    fn close(&mut self) {
        self.end_contour();
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
/// built (FreeType glyph backend).
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
            outline.has_overlaps = metrics.has_overlaps;
            Some((outline, metrics.advance_width))
        };
        let (outline, advance) = match target {
            None => draw(DrawSettings::unhinted(size, LocationRef::default()))?,
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
                let scaled = mul_fix(design, self.x_scale(ysize));
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
        div_fix(ysize, i64::from(self.units_per_em))
    }

    /// `qRound(pixelSize * 64)`: the 26.6 size FreeType symbol charmap sets.
    fn ysize(px: f32) -> i64 {
        (f64::from(px) * 64.0 + 0.5).floor() as i64
    }

    /// The outline of `glyph_id` at `size` pixels per em drawn through the uniform device scale
    /// `scale`, moved right by `x_offset` 64ths of a device pixel: the glyph slot outline
    /// FreeType glyph backend hands to the rasterizer. `None` where loading fails.
    pub fn freetype_outline(
        &self,
        glyph_id: u16,
        size: f32,
        scale: f32,
        x_offset: i32,
    ) -> Option<GlyphOutline> {
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
            for cmd in &mut outline.commands {
                match cmd {
                    OutlineCommand::MoveTo { x, y } | OutlineCommand::LineTo { x, y } => {
                        *x = mul_fix(*x, m);
                        *y = mul_fix(*y, m);
                    }
                    OutlineCommand::QuadTo { cx, cy, x, y } => {
                        *cx = mul_fix(*cx, m);
                        *cy = mul_fix(*cy, m);
                        *x = mul_fix(*x, m);
                        *y = mul_fix(*y, m);
                    }
                    OutlineCommand::CubicTo {
                        c0x,
                        c0y,
                        c1x,
                        c1y,
                        x,
                        y,
                    } => {
                        *c0x = mul_fix(*c0x, m);
                        *c0y = mul_fix(*c0y, m);
                        *c1x = mul_fix(*c1x, m);
                        *c1y = mul_fix(*c1y, m);
                        *x = mul_fix(*x, m);
                        *y = mul_fix(*y, m);
                    }
                    OutlineCommand::Close => {}
                }
            }
        }
        if x_offset != 0 {
            let dx = i64::from(x_offset);
            for cmd in &mut outline.commands {
                match cmd {
                    OutlineCommand::MoveTo { x, .. } | OutlineCommand::LineTo { x, .. } => {
                        *x = x.wrapping_add(dx);
                    }
                    OutlineCommand::QuadTo { cx, x, .. } => {
                        *cx = cx.wrapping_add(dx);
                        *x = x.wrapping_add(dx);
                    }
                    OutlineCommand::CubicTo { c0x, c1x, x, .. } => {
                        *c0x = c0x.wrapping_add(dx);
                        *c1x = c1x.wrapping_add(dx);
                        *x = x.wrapping_add(dx);
                    }
                    OutlineCommand::Close => {}
                }
            }
        }
        Some(outline)
    }

    /// Returns true if the glyph's control box exceeds the size limit (16x pixel size, capped at
    /// 4096 pixels).
    fn exceeds_glyph_size_limit(outline: &GlyphOutline, size: f32, scale: f32) -> bool {
        const LIMIT: f64 = 4096.0;
        let Some(cbox) = control_box(&outline.commands) else {
            return false;
        };
        let width = ((cbox.x_max - cbox.x_min) >> 6) as f64;
        let height = ((cbox.y_max - cbox.y_min) >> 6) as f64;
        let effective = f64::from(size) * f64::from(size) * f64::from(scale) * f64::from(scale);
        if effective > LIMIT * LIMIT {
            return true;
        }
        let max = (LIMIT * LIMIT).min((256.0f64 * 256.0).max(256.0 * effective));
        width * width > max || height * height > max
    }

    /// The advance FreeType glyph backend lays the glyph out with, in 26.6 pixels.
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
                mul_div(design, self.x_scale(ysize), 64) >> 10
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

    /// Rasterizes [`freetype_outline`](Self::freetype_outline) into glyph metrics and coverage.
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
        if Self::exceeds_glyph_size_limit(&outline, size, scale) {
            return (empty, Vec::new());
        }
        let Some((left, top, width, rows, buffer)) = rasterize_outline(&outline) else {
            return (empty, Vec::new());
        };
        let metrics = GlyphMetrics {
            xmin: left,
            ymin: top.wrapping_sub(rows as i32),
            width: width as usize,
            height: rows as usize,
            advance_width,
            advance_height: 0.0,
        };
        (metrics, buffer)
    }
}

impl GlyphFace for SkrifaFace {
    fn glyph_index(&self, ch: char) -> u16 {
        self.base.glyph_index(ch)
    }

    fn face_index(&self) -> u32 {
        self.face_index
    }

    /// The bitmap box of the glyph without rasterizing coverage.
    fn metrics_indexed(&self, glyph_id: u16, px: f32) -> GlyphMetrics {
        let advance_width = self.layout_advance_26_6(glyph_id, px) as f32 / 64.0;
        let Some(outline) = self.freetype_outline(glyph_id, px, 1.0, 0) else {
            return GlyphMetrics {
                advance_width,
                ..GlyphMetrics::default()
            };
        };
        if Self::exceeds_glyph_size_limit(&outline, px, 1.0) {
            return GlyphMetrics {
                advance_width,
                ..GlyphMetrics::default()
            };
        }
        match pixel_bounds(&outline.commands) {
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

    /// FreeType glyph backend: hint style none or light.
    fn supports_horizontal_subpixel_positions(&self) -> bool {
        matches!(self.hint_style, HintStyle::HintNone | HintStyle::HintLight)
    }

    /// the glyph backend with the four positions per pixel of
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

    #[test]
    fn closing_curve_back_to_an_explicit_start_adds_point() {
        let mut pen = SlotPen::default();
        pen.move_to(0.0, 0.0);
        pen.line_to(1.0, 0.0);
        pen.line_to(1.0, 1.0);
        pen.quad_to(0.5, 1.5, 0.0, 0.0);
        pen.close();
        let o = pen.finish();
        assert_eq!(o.contours, 1);
        assert_eq!(
            o.commands,
            [
                OutlineCommand::MoveTo { x: 0, y: 0 },
                OutlineCommand::LineTo { x: 64, y: 0 },
                OutlineCommand::LineTo { x: 64, y: 64 },
                OutlineCommand::QuadTo {
                    cx: 32,
                    cy: 96,
                    x: 0,
                    y: 0
                },
                OutlineCommand::Close,
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
        assert_eq!(draw(false).commands.len(), 5);
        assert_eq!(draw(true).commands.len(), 4);
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
        let o = pen.finish();
        assert_eq!(o.contours, 2);
    }
}
