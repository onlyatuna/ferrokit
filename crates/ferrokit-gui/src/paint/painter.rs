// SPDX-License-Identifier: LGPL-3.0-only
//! Painter primitives and 2D vector painting subsystem (`QPainter` equivalent).
//!
//! Controls stroke styling, fill patterns (gradients, textures, solid), transform stack,
//! composition modes, clipping, and text rendering.

use crate::application::LayoutDirection;
use crate::geometry::QPolygonF;
use crate::geometry::primitives::{PointF, RectF};
use crate::geometry::transform::Transform2D;
pub use crate::paint::brush::Brush;
pub use crate::paint::composition::CompositionMode;
use crate::paint::paint_device::PaintDevice;
use crate::paint::path::{FillRule, PainterPath};
use crate::paint::pixmap::Pixmap;
use crate::paint::text_blend::{
    MaskedSource, blend_grey, blend_grey_gamma, blend_lcd, composite_masked, rgb_average,
};
use crate::text::document::TextDocument;
use crate::text::font::Font;
use crate::text::glyph_layout::{FontEngine, PositionedGlyph};
use crate::text::line_items::{ItemContent, LineItems, TextOption, qfixed_from_real};
use crate::text::smoothing::{TextSmoothing, TrcLut, gamma_lut, text_smoothing};
use std::sync::Arc;
use tiny_skia::{
    Color, FilterQuality, LineCap, LineJoin, Mask, Paint, Path, PathBuilder, Pattern, Shader,
    SpreadMode, Stroke, Transform,
};

/// Pen styling (`QPen` equivalent).
#[derive(Debug, Clone, PartialEq)]
pub struct Pen {
    pub color: Color,
    pub width: f32,
    pub cap: LineCap,                   // Butt, Round, Square (Qt::PenCapStyle)
    pub join: LineJoin,                 // Miter, Round, Bevel (Qt::PenJoinStyle)
    pub dash_pattern: Option<Vec<f32>>, // Dash/dot pattern
}

impl Pen {
    // Creates a Pen with color and width; like `QPen`, caps default to square and joins to bevel.
    pub fn new(color: Color, width: f32) -> Self {
        Self {
            color,
            width,
            cap: LineCap::Square,
            join: LineJoin::Bevel,
            dash_pattern: None,
        }
    }

    // Creates a Pen with RGBA8 color.
    pub fn from_rgba8(r: u8, g: u8, b: u8, a: u8, width: f32) -> Self {
        Self::new(Color::from_rgba8(r, g, b, a), width)
    }

    // Sets line cap style.
    pub fn with_cap(mut self, cap: LineCap) -> Self {
        self.cap = cap;
        self
    }

    // Sets line join style.
    pub fn with_join(mut self, join: LineJoin) -> Self {
        self.join = join;
        self
    }

    // Sets dash pattern.
    pub fn with_dash_pattern(mut self, pattern: Vec<f32>) -> Self {
        self.dash_pattern = Some(pattern);
        self
    }

    // Converts to tiny_skia Stroke configuration.
    pub fn to_stroke(&self) -> Stroke {
        let mut stroke = Stroke {
            width: self.width,
            line_cap: self.cap,
            line_join: self.join,
            ..Default::default()
        };
        if let Some(dash) = &self.dash_pattern
            && let Some(dash_stroke) = tiny_skia::StrokeDash::new(dash.clone(), 0.0)
        {
            stroke.dash = Some(dash_stroke);
        }
        stroke
    }
}

impl Default for Pen {
    fn default() -> Self {
        Self::new(Color::BLACK, 1.0)
    }
}

/// Render hint (`QPainter::RenderHint`, `qpainter.h`); bit values match Qt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RenderHint {
    /// Anti-aliased shape edges (`QPainter::Antialiasing`).
    Antialiasing = 0x01,
    /// Anti-aliased text (`QPainter::TextAntialiasing`). Stored only; text drawing is always
    /// anti-aliased.
    TextAntialiasing = 0x02,
    /// Bilinear sampling of pixmaps and textures (`QPainter::SmoothPixmapTransform`).
    SmoothPixmapTransform = 0x04,
}

/// Set of render hints (`QPainter::RenderHints`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderHints(u8);

impl RenderHints {
    /// `QPainter::begin` default: `TextAntialiasing` only (`qpainter.cpp:1847`).
    pub const DEFAULT: Self = Self(RenderHint::TextAntialiasing as u8);

    /// `QPainter::testRenderHint`.
    pub fn contains(self, hint: RenderHint) -> bool {
        self.0 & hint as u8 != 0
    }

    /// `QPainter::setRenderHint(hint, on)`.
    fn set(&mut self, hint: RenderHint, on: bool) {
        if on {
            self.0 |= hint as u8;
        } else {
            self.0 &= !(hint as u8);
        }
    }
}

impl Default for RenderHints {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// A rect clip taken through a rotating or shearing transform: a quadrilateral in root logical
/// coordinates (device pixels divided by the device-pixel ratio), the way Qt keeps it as a
/// transformed path clip (`QPainter::setClipRect` -> `QPaintEngine::clip(QVectorPath)`,
/// `qpainter.cpp:2744-2770`). `anti_alias` is the `Antialiasing` hint when the clip was set;
/// `QRasterPaintEngine::rasterize` takes it from the state at that time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClipQuad {
    pub points: [tiny_skia::Point; 4],
    pub anti_alias: bool,
}

/// Painter state snapshot (modeled after Qt `QPainter` state stack).
#[derive(Debug, Clone, PartialEq)]
pub struct PainterState {
    pub pen: Option<Pen>,
    pub brush: Brush,
    /// `QPainter::brushOrigin` in logical user coordinates (default `(0, 0)`).
    pub brush_origin: PointF,
    pub transform: Transform,
    pub opacity: f32,
    pub render_hints: RenderHints,
    pub composition_mode: CompositionMode,
    /// Bounding box of the clip in root logical coordinates; exact unless `clip_quads` is
    /// non-empty.
    pub clip_rect: Option<tiny_skia::Rect>,
    /// Rotated rect clips intersected with `clip_rect`; empty while every clip is axis-aligned.
    pub clip_quads: Vec<ClipQuad>,
    /// `QPainter::layoutDirection`: the base direction `draw_text` gives the text. A painter on an
    /// image or pixmap starts with `LayoutDirectionAuto`, the direction of the first strong
    /// character of the text (qpainter.cpp:1824).
    pub layout_direction: LayoutDirection,
}

impl Default for PainterState {
    fn default() -> Self {
        Self {
            pen: Some(Pen::new(Color::BLACK, 1.0)),
            brush: Brush::NoBrush,
            brush_origin: PointF::new(0.0, 0.0),
            transform: Transform::identity(),
            opacity: 1.0,
            render_hints: RenderHints::default(),
            composition_mode: CompositionMode::SourceOver,
            clip_rect: None,
            clip_quads: Vec::new(),
            layout_direction: LayoutDirection::LayoutDirectionAuto,
        }
    }
}

/// Half-open pixel rectangle `(x0, y0, x1, y1)` on the device.
type PixelBounds = (u32, u32, u32, u32);

/// Coverage multiplication matching `premultiply_u8` rounding.
fn mul_u8(a: u8, b: u8) -> u8 {
    ((a as u32 * b as u32 + 127) / 255) as u8
}

/// Device pixels a logical-space box touches, grown by `pad` and clamped to the device.
fn pixel_bounds([l, t, r, b]: [f32; 4], scale: f32, pad: i64, (w, h): (u32, u32)) -> PixelBounds {
    let clamp = |v: f32, floor: bool, max: u32| -> u32 {
        let v = v * scale;
        let v = if floor {
            v.floor() as i64 - pad
        } else {
            v.ceil() as i64 + pad
        };
        v.clamp(0, max as i64) as u32
    };
    (
        clamp(l, true, w),
        clamp(t, true, h),
        clamp(r, false, w),
        clamp(b, false, h),
    )
}

/// Zeroes `bounds` of `mask`, clamped to the mask.
fn zero_mask_region(mask: &mut Mask, bounds: PixelBounds) {
    let (w, h) = (mask.width(), mask.height());
    let (x0, y0, x1, y1) = (
        bounds.0.min(w),
        bounds.1.min(h),
        bounds.2.min(w),
        bounds.3.min(h),
    );
    if x0 >= x1 {
        return;
    }
    let data = mask.data_mut();
    for y in y0..y1 {
        let row = (y * w) as usize;
        data[row + x0 as usize..row + x1 as usize].fill(0);
    }
}

/// The integer pixel of a glyph's origin. A glyph that far outside any target is not drawn, but its
/// coordinates are still added to; keeping them within a billion keeps those sums inside an `i32`.
fn glyph_pixel(v: f32) -> i32 {
    v.round().clamp(-1.0e9, 1.0e9) as i32
}

/// Where `QRasterPaintEngine::drawCachedGlyphs` (qpaintengine_raster.cpp) puts a glyph of a face
/// of Qt's FreeType engine ([`GlyphFace::freetype_engine`]): the glyph's whole device pixel
/// (`qFloor(x)`, `qRound(y)`, before the bitmap's own left and top are applied) and the pen
/// position in 26.6, from which the glyph cache's sub-pixel position follows
/// (`QFontEngine::subPixelPositionFor`).
///
/// [`GlyphFace::freetype_engine`]: crate::text::glyph_face::GlyphFace::freetype_engine
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct QtGlyphPlacement {
    x: i32,
    y: i32,
    pen_x: i64,
}

/// `QFixed::fromReal`: truncated toward zero to 26.6, kept within a billion pixels (see
/// [`glyph_pixel`]).
fn qt_fixed(v: f64) -> i64 {
    (v * 64.0).clamp(-6.4e10, 6.4e10) as i64
}

/// [`QtGlyphPlacement`] of a glyph at the 26.6 device position `(x, y)`, which
/// `QFontEngine::getGlyphPositions` gives it.
fn qt_glyph_placement(x: i64, y: i64) -> QtGlyphPlacement {
    let clamp = |v: i64| v.clamp(-1_000_000_000, 1_000_000_000) as i32;
    QtGlyphPlacement {
        x: clamp(x >> 6),
        y: clamp(y.saturating_add(32) >> 6),
        pen_x: x,
    }
}

/// `qRound` (`qnumeric.h`): rounds half away from zero.
fn qt_round(v: f64) -> i32 {
    if v >= 0.0 {
        (v + 0.5).floor() as i32
    } else {
        (v - 0.5).ceil() as i32
    }
}

/// Pattern-space transform that makes `tiny-skia`'s nearest-neighbour `Repeat` pipeline step
/// through a `tile_w x tile_h` texture at the exact texels `QRasterPaintEngine` (`QSpanData::
/// setupMatrix` + `adjustSpanMethods` + `qBlendTexture`, `qpaintengine_raster.cpp` /
/// `qdrawhelper.cpp`) samples under `painter_transform` and `brush_origin`.
///
/// Only when BOTH axes have unit scale does Qt classify the matrix as pure translation
/// (`TxTranslate`) and round the device-space translation with `qRound`. Any other axis-aligned
/// scale (`TxScale`, such as `devicePixelRatio != 1.0` or `sx == 1.0, sy == 1.25`) sends BOTH axes
/// through the `TxScale` path, which wraps the inverse translation modulo the tile size (`fmod`)
/// before truncating to 16.16 fixed point and steps
/// `fx = (fdx >> 1) + int(fmod(inv_dx, tile_w) * 65536) - 1 + x * fdx`.
///
/// Rotation or skew (`kx != 0 || ky != 0`) and non-positive scales (`sx <= 0 || sy <= 0`)
/// intentionally keep the unadjusted `origin_ts` transform.
fn qt_nearest_pattern_transform(
    painter_transform: Transform,
    brush_origin: PointF,
    base_pattern_transform: Transform,
    tile_w: u32,
    tile_h: u32,
) -> Transform {
    let origin_ts = if brush_origin.x != 0.0 || brush_origin.y != 0.0 {
        base_pattern_transform.post_translate(brush_origin.x, brush_origin.y)
    } else {
        base_pattern_transform
    };
    let combined = origin_ts.post_concat(painter_transform);
    if combined.kx != 0.0 || combined.ky != 0.0 || combined.sx <= 0.0 || combined.sy <= 0.0 {
        return origin_ts;
    }
    let Some(painter_inv) = painter_transform.invert() else {
        return origin_ts;
    };
    let is_translate_only = (combined.sx - 1.0).abs() <= 1e-6 && (combined.sy - 1.0).abs() <= 1e-6;
    let axis = |scale: f32, trans: f32, tile: u32| -> (f32, f32) {
        let s = f64::from(scale);
        let t = f64::from(trans);
        if is_translate_only {
            let want_ts_tx = f64::from(-qt_round(t));
            (1.0, -want_ts_tx as f32)
        } else {
            let inv_s = 1.0 / s;
            let fdx = (inv_s * 65536.0) as i32;
            if fdx <= 0 {
                return (scale, trans);
            }
            let inv_dx = (-t * inv_s) % f64::from(tile);
            let m_dx = (inv_dx * 65536.0) as i32;
            let fx0 = (fdx >> 1) + m_dx - 1;
            let want_ts_sx = f64::from(fdx) / 65536.0;
            let want_ts_tx = (f64::from(fx0) - 0.5 * f64::from(fdx)) / 65536.0;
            ((1.0 / want_ts_sx) as f32, (-want_ts_tx / want_ts_sx) as f32)
        }
    };
    let (c_sx, c_tx) = axis(combined.sx, combined.tx, tile_w);
    let (c_sy, c_ty) = axis(combined.sy, combined.ty, tile_h);
    Transform::from_row(c_sx, 0.0, 0.0, c_sy, c_tx, c_ty).post_concat(painter_inv)
}

/// 2D vector painter (`QPainter` equivalent).
///
/// Returns a mutable reference to the active state.
pub struct Painter<'a> {
    pub(super) device: &'a mut dyn PaintDevice,
    pub(super) state: PainterState,
    pub(super) saved_states: Vec<PainterState>,
    pub(super) cached_mask: Option<Mask>,
    pub(super) cached_clip_rect: Option<tiny_skia::Rect>,
    pub(super) cached_clip_quads: Vec<ClipQuad>,
    /// Mask buffer kept while no clip is active, so a clip change reuses it instead of
    /// allocating a device-sized one.
    pub(super) spare_mask: Option<Mask>,
    /// Second device-sized buffer the quads are rasterised into before being multiplied in.
    pub(super) scratch_mask: Option<Mask>,
    /// Pixels of `cached_mask` / `spare_mask` that may be non-zero.
    pub(super) mask_dirty: PixelBounds,
    /// Gamma table of LCD (ClearType) text; `None` draws grey-scale text. Fixed when painting
    /// starts, like `QRasterPaintEngine::begin` fixes the glyph cache format.
    pub(super) lcd_lut: Option<Arc<TrcLut>>,
    /// Coverage of the glyph being blitted through `composite_masked`, kept between glyphs so a
    /// run of text does not allocate one per glyph.
    pub(super) glyph_cov: Vec<u8>,
    /// Scratch surface of `composite_masked`, kept for the same reason.
    pub(super) blit_scratch: Vec<u8>,
}
/// Clip coverage (0 = clipped away, 255 = inside) of the device pixel `(x, y)` for a glyph blit:
/// the clip box (logical, scaled by `dpr`), the device bounds, and a rotated clip's mask.
fn glyph_clip_coverage(
    clip_rect: Option<tiny_skia::Rect>,
    dpr: f32,
    clip_cov: Option<(&[u8], usize)>,
    (pw, ph): (u32, u32),
    (x, y): (i32, i32),
) -> u8 {
    if x < 0 || y < 0 || x >= pw as i32 || y >= ph as i32 {
        return 0;
    }
    if let Some(clip) = clip_rect {
        let inside = x >= (clip.left() * dpr).floor() as i32
            && x < (clip.right() * dpr).ceil() as i32
            && y >= (clip.top() * dpr).floor() as i32
            && y < (clip.bottom() * dpr).ceil() as i32;
        if !inside {
            return 0;
        }
    }
    match clip_cov {
        Some((cov, mw)) => cov[y as usize * mw + x as usize],
        None => 255,
    }
}

impl<'a> Painter<'a> {
    // Begins painting on the specified `PaintDevice`, configuring initial DPR transform.
    pub fn begin(device: &'a mut dyn PaintDevice) -> Self {
        let dpr = device.device_pixel_ratio();
        let mut state = PainterState::default();
        // Configure initial scale transform matching device pixel ratio
        if dpr != 1.0 {
            state.transform = tiny_skia::Transform::from_scale(dpr, dpr);
        }
        let smoothing = text_smoothing();
        Self {
            device,
            state,
            saved_states: Vec::new(),
            cached_mask: None,
            cached_clip_rect: None,
            cached_clip_quads: Vec::new(),
            spare_mask: None,
            scratch_mask: None,
            mask_dirty: (0, 0, 0, 0),
            lcd_lut: smoothing.cleartype.then(|| gamma_lut(smoothing.gamma)),
            glyph_cov: Vec::new(),
            blit_scratch: Vec::new(),
        }
    }

    /// Overrides the text smoothing taken from the platform when painting started.
    pub fn set_text_smoothing(&mut self, smoothing: TextSmoothing) {
        self.lcd_lut = smoothing.cleartype.then(|| gamma_lut(smoothing.gamma));
    }

    // Returns a reference to the active state.
    pub fn state(&self) -> &PainterState {
        &self.state
    }

    /// Sets or clears one render hint (`QPainter::setRenderHint`). Saved by `save()`.
    pub fn set_render_hint(&mut self, hint: RenderHint, on: bool) {
        self.state.render_hints.set(hint, on);
    }

    /// Active render hints (`QPainter::renderHints`).
    pub fn render_hints(&self) -> RenderHints {
        self.state.render_hints
    }

    /// Tests one render hint (`QPainter::testRenderHint`).
    pub fn test_render_hint(&self, hint: RenderHint) -> bool {
        self.state.render_hints.contains(hint)
    }

    // Returns a mutable reference to the active state.
    pub fn state_mut(&mut self) -> &mut PainterState {
        &mut self.state
    }

    // Returns a reference to the underlying PaintDevice.
    pub fn device(&self) -> &dyn PaintDevice {
        &*self.device
    }

    /// Returns the underlying `PaintDevice` mutably, for a caller that has to read or write the
    /// device's pixels around painting (the clip and state of the painter are unaffected).
    pub fn device_mut(&mut self) -> &mut dyn PaintDevice {
        &mut *self.device
    }

    // -------------------------------------------------------------------------
    // State and transform management
    // -------------------------------------------------------------------------

    // Saves the current painter state onto the stack.
    pub fn save(&mut self) {
        self.saved_states.push(self.state.clone());
    }

    // Restores the most recently saved painter state.
    pub fn restore(&mut self) {
        if let Some(prev) = self.saved_states.pop() {
            self.state = prev;
        }
    }

    // Translates coordinates.
    pub fn translate(&mut self, dx: f32, dy: f32) {
        self.state.transform = self.state.transform.pre_translate(dx, dy);
    }

    /// Device pixel ratio of the paint device (`QPaintDevice::devicePixelRatio`).
    pub fn device_pixel_ratio(&self) -> f32 {
        self.device.device_pixel_ratio()
    }

    /// Translates by whole device pixels, after the current transform (no dpr scaling), so the
    /// origin lands exactly on a pixel boundary. Qt gets this by giving every popup its own
    /// native window at a whole-pixel position; per-glyph rounding then never depends on how
    /// far the logical origin sits from the pixel grid.
    pub fn translate_device(&mut self, dx: i32, dy: i32) {
        self.state.transform = self.state.transform.post_translate(dx as f32, dy as f32);
    }

    // Scales coordinates.
    pub fn scale(&mut self, sx: f32, sy: f32) {
        self.state.transform = self.state.transform.pre_scale(sx, sy);
    }

    // Rotates coordinates (in degrees, clockwise).
    pub fn rotate(&mut self, degrees: f32) {
        self.state.transform = self.state.transform.pre_rotate(degrees);
    }
    /// Shears coordinates horizontally by `shx` and vertically by `shy`.
    pub fn shear(&mut self, shx: f32, shy: f32) {
        let t = Transform2D::from_shear(shx, shy);
        self.state.transform = self.state.transform.post_concat(t.to_skia());
    }

    /// Sets the full 2D affine transformation matrix.
    pub fn set_transform(&mut self, transform: &Transform2D) {
        self.state.transform = transform.to_skia();
    }

    /// Returns the current 2D affine transformation matrix.
    pub fn transform(&self) -> Transform2D {
        Transform2D::from_skia(self.state.transform)
    }

    /// Resets the current transformation to identity (scaled by DPR).
    pub fn reset_transform(&mut self) {
        let dpr = self.device.device_pixel_ratio();
        if dpr != 1.0 {
            self.state.transform = Transform::from_scale(dpr, dpr);
        } else {
            self.state.transform = Transform::identity();
        }
    }

    /// Sets the composition / blend mode.
    pub fn set_composition_mode(&mut self, mode: CompositionMode) {
        self.state.composition_mode = mode;
    }

    /// Returns the active composition mode.
    pub fn composition_mode(&self) -> CompositionMode {
        self.state.composition_mode
    }

    pub fn set_pen(&mut self, pen: impl Into<Option<Pen>>) {
        self.state.pen = pen.into();
    }

    pub fn set_brush(&mut self, brush: Brush) {
        self.state.brush = brush;
    }

    /// Sets the brush origin in logical user coordinates (`QPainter::setBrushOrigin`).
    pub fn set_brush_origin(&mut self, origin: PointF) {
        self.state.brush_origin = origin;
    }

    /// Returns the current brush origin (`QPainter::brushOrigin`).
    pub fn brush_origin(&self) -> PointF {
        self.state.brush_origin
    }
    // Sets opacity (0.0 to 1.0).
    pub fn set_opacity(&mut self, opacity: f32) {
        self.state.opacity = opacity.clamp(0.0, 1.0);
    }

    // Replaces the clip with `rect` given in the current user space, like
    // `QPainter::setClipRect(QRectF)` (qpainter.cpp:2816,2828; qpainter.h:187): the rect is taken
    // through the current transform and the clip is replaced, not intersected. Under a rotating
    // or shearing transform the clip is the transformed rectangle, not its bounding box
    // (`QRasterPaintEngine::clip(QRect)` falls back to a path clip, qpaintengine_raster.cpp:1212).
    // An empty or unmappable rect clips everything.
    pub fn set_clip_rect(&mut self, rect: RectF) {
        let (bounds, quad) = self.user_rect_to_root_clip(rect);
        self.state.clip_rect = Some(bounds);
        self.state.clip_quads.clear();
        self.state.clip_quads.extend(quad);
    }

    // Intersects the clip with `rect` given in the current user space, like
    // `QPainter::setClipRect(rect, Qt::IntersectClip)` (qpainter.cpp:2798-2816): the rect is taken
    // through the current transform. An empty intersection clips everything.
    pub fn clip_to_rect(&mut self, rect: RectF) {
        let (new, quad) = self.user_rect_to_root_clip(rect);
        let clipped = match self.state.clip_rect {
            Some(current) => current.intersect(&new),
            None => Some(new),
        };
        match clipped {
            Some(bounds) => {
                self.state.clip_rect = Some(bounds);
                self.state.clip_quads.extend(quad);
            }
            None => {
                self.state.clip_rect = Some(Self::empty_clip());
                self.state.clip_quads.clear();
            }
        }
    }

    // Maps a user-space rect through the current transform into the root logical coordinates in
    // which the clip is stored (`update_clip_mask_if_needed` scales it by the device-pixel ratio
    // only, so the mapped rect is divided by that ratio). Returns the bounding box and, when the
    // transform rotates or shears, the exact quadrilateral. Unmappable rects become the empty
    // clip.
    pub(super) fn user_rect_to_root_clip(
        &self,
        rect: RectF,
    ) -> (tiny_skia::Rect, Option<ClipQuad>) {
        let dpr = self.device.device_pixel_ratio();
        let t = self.state.transform;
        let mapped = (|| {
            let r = rect.to_tiny_skia_rect()?;
            if t.kx == 0.0 && t.ky == 0.0 {
                let m = r.transform(t)?;
                let bounds = tiny_skia::Rect::from_ltrb(
                    m.left() / dpr,
                    m.top() / dpr,
                    m.right() / dpr,
                    m.bottom() / dpr,
                )?;
                return Some((bounds, None));
            }
            let mut points = [
                tiny_skia::Point::from_xy(r.left(), r.top()),
                tiny_skia::Point::from_xy(r.right(), r.top()),
                tiny_skia::Point::from_xy(r.right(), r.bottom()),
                tiny_skia::Point::from_xy(r.left(), r.bottom()),
            ];
            t.map_points(&mut points);
            for p in &mut points {
                p.x /= dpr;
                p.y /= dpr;
            }
            let min_x = points.iter().map(|p| p.x).fold(f32::INFINITY, f32::min);
            let min_y = points.iter().map(|p| p.y).fold(f32::INFINITY, f32::min);
            let max_x = points.iter().map(|p| p.x).fold(f32::NEG_INFINITY, f32::max);
            let max_y = points.iter().map(|p| p.y).fold(f32::NEG_INFINITY, f32::max);
            let bounds = tiny_skia::Rect::from_ltrb(min_x, min_y, max_x, max_y)?;
            let anti_alias = self.state.render_hints.contains(RenderHint::Antialiasing);
            Some((bounds, Some(ClipQuad { points, anti_alias })))
        })();
        mapped.unwrap_or_else(|| (Self::empty_clip(), None))
    }

    // A zero-area clip admits no pixels; it is the representation of an empty intersection.
    pub(super) fn empty_clip() -> tiny_skia::Rect {
        tiny_skia::Rect::from_xywh(0.0, 0.0, 0.0, 0.0)
            .expect("a zero-area rect at the origin is finite")
    }

    // True when the transform is the device-pixel-ratio scale plus a translation, i.e. the glyph
    // cache's per-glyph origin mapping is exact.
    pub(super) fn text_transform_is_translate_only(&self) -> bool {
        let t = self.state.transform;
        let dpr = self.device.device_pixel_ratio();
        const EPS: f32 = 1e-6;
        (t.sx - dpr).abs() < EPS && (t.sy - dpr).abs() < EPS && t.kx.abs() < EPS && t.ky.abs() < EPS
    }

    // -------------------------------------------------------------------------

    // -------------------------------------------------------------------------

    // Updates or clears the cached clip mask based on current state. The mask buffer is reused
    // across clip changes: only the pixels the previous clip painted are zeroed.
    pub(super) fn update_clip_mask_if_needed(&mut self) {
        let up_to_date = self.state.clip_rect == self.cached_clip_rect
            && self.state.clip_quads == self.cached_clip_quads
            && (self.cached_mask.is_some() || self.state.clip_rect.is_none());
        if up_to_date {
            return;
        }
        self.cached_clip_rect = self.state.clip_rect;
        self.cached_clip_quads.clone_from(&self.state.clip_quads);

        let mut buffer = self.cached_mask.take().or_else(|| self.spare_mask.take());
        if let Some(m) = buffer.as_mut() {
            zero_mask_region(m, self.mask_dirty);
        }
        self.mask_dirty = (0, 0, 0, 0);
        let Some(clip) = self.state.clip_rect else {
            self.spare_mask = buffer;
            return;
        };

        let (pw, ph) = (self.device.physical_width(), self.device.physical_height());
        let mut mask = match buffer {
            Some(m) if m.width() == pw && m.height() == ph => m,
            _ => match Mask::new(pw, ph) {
                Some(m) => m,
                None => return,
            },
        };
        let dpr = self.device.device_pixel_ratio();
        let scale = if dpr != 1.0 {
            tiny_skia::Transform::from_scale(dpr, dpr)
        } else {
            tiny_skia::Transform::identity()
        };
        mask.fill_path(
            &PathBuilder::from_rect(clip),
            tiny_skia::FillRule::Winding,
            false,
            scale,
        );
        let clip_bounds = pixel_bounds(
            [clip.left(), clip.top(), clip.right(), clip.bottom()],
            dpr,
            0,
            (pw, ph),
        );
        self.mask_dirty = clip_bounds;

        // Each rotated rect is rasterised into the scratch mask and multiplied in.
        for quad in &self.state.clip_quads {
            let mut scratch = match self.scratch_mask.take() {
                Some(m) if m.width() == pw && m.height() == ph => m,
                _ => match Mask::new(pw, ph) {
                    Some(m) => m,
                    None => break,
                },
            };
            let mut builder = PathBuilder::new();
            builder.move_to(quad.points[0].x, quad.points[0].y);
            for p in &quad.points[1..] {
                builder.line_to(p.x, p.y);
            }
            builder.close();
            let xs = quad.points.iter().map(|p| p.x);
            let ys = quad.points.iter().map(|p| p.y);
            let quad_bounds = pixel_bounds(
                [
                    xs.clone().fold(f32::INFINITY, f32::min),
                    ys.clone().fold(f32::INFINITY, f32::min),
                    xs.fold(f32::NEG_INFINITY, f32::max),
                    ys.fold(f32::NEG_INFINITY, f32::max),
                ],
                dpr,
                1,
                (pw, ph),
            );
            if let Some(path) = builder.finish() {
                scratch.fill_path(&path, tiny_skia::FillRule::Winding, quad.anti_alias, scale);
            }
            let (x0, y0, x1, y1) = clip_bounds;
            if x0 < x1 {
                let w = pw as usize;
                let (dst, src) = (mask.data_mut(), scratch.data());
                for y in y0 as usize..y1 as usize {
                    for i in y * w + x0 as usize..y * w + x1 as usize {
                        dst[i] = mul_u8(dst[i], src[i]);
                    }
                }
            }
            zero_mask_region(&mut scratch, quad_bounds);
            self.scratch_mask = Some(scratch);
        }
        self.cached_mask = Some(mask);
    }

    pub fn fill_path(&mut self, path: &Path) {
        self.fill_path_with_rule(path, tiny_skia::FillRule::Winding);
    }

    pub fn fill_path_with_rule(&mut self, path: &Path, fill_rule: tiny_skia::FillRule) {
        let opacity = self.state.opacity;
        let transform = self.state.transform;
        let anti_alias = self.state.render_hints.contains(RenderHint::Antialiasing);
        // Qt samples a pixmap or texture brush with the nearest pixel unless
        // `SmoothPixmapTransform` is set (`QPainter::renderHints`, Qt default: off).
        let filter = if self
            .state
            .render_hints
            .contains(RenderHint::SmoothPixmapTransform)
        {
            FilterQuality::Bilinear
        } else {
            FilterQuality::Nearest
        };
        self.update_clip_mask_if_needed();
        let mask_ref = self.cached_mask.as_ref();
        let blend_mode = self.state.composition_mode.into();

        match &self.state.brush {
            Brush::NoBrush => {}
            Brush::Color(color) => {
                let mut c = *color;
                c.apply_opacity(opacity);
                if self.state.composition_mode == CompositionMode::SourceOver {
                    crate::paint::solid_fill::fill_path_source_over(
                        &mut self.device.as_pixmap_mut(),
                        path,
                        fill_rule,
                        anti_alias,
                        transform,
                        c.premultiply().to_color_u8(),
                        mask_ref,
                    );
                    return;
                }
                let paint = Paint {
                    shader: Shader::SolidColor(c),
                    blend_mode,
                    anti_alias,
                    ..Default::default()
                };
                self.device
                    .as_pixmap_mut()
                    .fill_path(path, &paint, fill_rule, transform, mask_ref);
            }
            Brush::Hatched { color } => {
                // Python `_hatch_brush`: a 6x6 tile with three antialiased 1.6px diagonals, which
                // make one continuous stripe once tiled. Qt samples such a texture with the
                // nearest pixel unless `SmoothPixmapTransform` is set (the dial sets only
                // `Antialiasing`), including when the paint transform scales it.
                const TILE_SIZE: u32 = 6;
                if let Some(mut tile) = tiny_skia::Pixmap::new(TILE_SIZE, TILE_SIZE) {
                    let mut ink = Paint::default();
                    ink.set_color(*color);
                    ink.anti_alias = true;
                    let stroke = tiny_skia::Stroke {
                        width: 1.6,
                        line_cap: LineCap::Square,
                        ..Default::default()
                    };
                    for ((x1, y1), (x2, y2)) in [
                        ((-1.0, 7.0), (7.0, -1.0)),
                        ((-1.0, 1.0), (1.0, -1.0)),
                        ((5.0, 7.0), (7.0, 5.0)),
                    ] {
                        let mut line = tiny_skia::PathBuilder::new();
                        line.move_to(x1, y1);
                        line.line_to(x2, y2);
                        if let Some(line) = line.finish() {
                            tile.stroke_path(&line, &ink, &stroke, Transform::identity(), None);
                        }
                    }
                    let pattern_ts = if filter == FilterQuality::Nearest {
                        qt_nearest_pattern_transform(
                            transform,
                            self.state.brush_origin,
                            Transform::identity(),
                            TILE_SIZE,
                            TILE_SIZE,
                        )
                    } else if self.state.brush_origin.x != 0.0 || self.state.brush_origin.y != 0.0 {
                        Transform::from_translate(
                            self.state.brush_origin.x,
                            self.state.brush_origin.y,
                        )
                    } else {
                        Transform::identity()
                    };
                    let paint = Paint {
                        shader: Pattern::new(
                            tile.as_ref(),
                            SpreadMode::Repeat,
                            filter,
                            opacity,
                            pattern_ts,
                        ),
                        blend_mode,
                        anti_alias,
                        ..Default::default()
                    };
                    self.device
                        .as_pixmap_mut()
                        .fill_path(path, &paint, fill_rule, transform, mask_ref);
                }
            }
            Brush::LinearGradient(gradient) => {
                if let Some(shader) = gradient.to_shader(Transform::identity(), opacity) {
                    let paint = Paint {
                        shader,
                        blend_mode,
                        anti_alias,
                        ..Default::default()
                    };
                    self.device
                        .as_pixmap_mut()
                        .fill_path(path, &paint, fill_rule, transform, mask_ref);
                }
            }
            Brush::RadialGradient(gradient) => {
                if let Some(shader) = gradient.to_shader(Transform::identity(), opacity) {
                    let paint = Paint {
                        shader,
                        blend_mode,
                        anti_alias,
                        ..Default::default()
                    };
                    self.device
                        .as_pixmap_mut()
                        .fill_path(path, &paint, fill_rule, transform, mask_ref);
                }
            }
            Brush::Texture(pattern) => {
                let pm = pattern.pixmap.as_tiny_skia();
                let pattern_ts = if filter == FilterQuality::Nearest {
                    qt_nearest_pattern_transform(
                        transform,
                        self.state.brush_origin,
                        pattern.transform,
                        pm.width(),
                        pm.height(),
                    )
                } else if self.state.brush_origin.x != 0.0 || self.state.brush_origin.y != 0.0 {
                    pattern
                        .transform
                        .post_translate(self.state.brush_origin.x, self.state.brush_origin.y)
                } else {
                    pattern.transform
                };
                let paint = Paint {
                    shader: Pattern::new(
                        pm.as_ref(),
                        SpreadMode::Repeat,
                        filter,
                        opacity,
                        pattern_ts,
                    ),
                    blend_mode,
                    anti_alias,
                    ..Default::default()
                };
                self.device
                    .as_pixmap_mut()
                    .fill_path(path, &paint, fill_rule, transform, mask_ref);
            }
        }
    }

    pub fn stroke_path(&mut self, path: &Path) {
        let pen = match &self.state.pen {
            Some(p) => p,
            None => return,
        };
        let mut c = pen.color;
        c.apply_opacity(self.state.opacity);
        let blend_mode = self.state.composition_mode.into();
        let paint = Paint {
            shader: Shader::SolidColor(c),
            blend_mode,
            anti_alias: self.state.render_hints.contains(RenderHint::Antialiasing),
            ..Default::default()
        };
        let stroke = pen.to_stroke();
        let transform = self.state.transform;
        self.update_clip_mask_if_needed();
        let mask_ref = self.cached_mask.as_ref();

        self.device
            .as_pixmap_mut()
            .stroke_path(path, &paint, &stroke, transform, mask_ref);
    }

    /// Draws a `PainterPath` (`QPainter::drawPath` equivalent) with filling and outline.
    pub fn draw_path(&mut self, painter_path: &PainterPath) {
        if let Some(path) = painter_path.to_skia_path() {
            self.fill_path_with_rule(&path, painter_path.fill_rule().into());
            self.stroke_path(&path);
        }
    }

    /// Fills a `PainterPath` with current brush and the path's fill rule.
    pub fn fill_painter_path(&mut self, painter_path: &PainterPath) {
        if let Some(path) = painter_path.to_skia_path() {
            self.fill_path_with_rule(&path, painter_path.fill_rule().into());
        }
    }

    /// Strokes a `PainterPath` with current pen.
    pub fn stroke_painter_path(&mut self, painter_path: &PainterPath) {
        if let Some(path) = painter_path.to_skia_path() {
            self.stroke_path(&path);
        }
    }

    // -------------------------------------------------------------------------
    // Pixmap drawing
    // -------------------------------------------------------------------------

    // Draws a pixmap with optional source rectangle cropping and scaling.
    pub fn draw_pixmap(&mut self, target: RectF, pixmap: &Pixmap, source: Option<RectF>) {
        let src_rect = source.unwrap_or_else(|| {
            RectF::new(
                0.0,
                0.0,
                pixmap.physical_width() as f32,
                pixmap.physical_height() as f32,
            )
        });
        if src_rect.width <= 0.0
            || src_rect.height <= 0.0
            || target.width <= 0.0
            || target.height <= 0.0
        {
            return;
        }
        let scale_x = target.width / src_rect.width;
        let scale_y = target.height / src_rect.height;

        let patt_transform = Transform::from_translate(target.x, target.y)
            .pre_scale(scale_x, scale_y)
            .pre_translate(-src_rect.x, -src_rect.y);

        // Qt `drawPixmap` samples nearest unless `SmoothPixmapTransform` is set.
        let filter = if self
            .state
            .render_hints
            .contains(RenderHint::SmoothPixmapTransform)
        {
            FilterQuality::Bilinear
        } else {
            FilterQuality::Nearest
        };
        let paint = Paint {
            shader: Pattern::new(
                pixmap.as_tiny_skia().as_ref(),
                SpreadMode::Pad,
                filter,
                self.state.opacity,
                patt_transform,
            ),
            blend_mode: self.state.composition_mode.into(),
            anti_alias: self.state.render_hints.contains(RenderHint::Antialiasing),
            ..Default::default()
        };

        if let Some(target_r) = target.to_tiny_skia_rect() {
            let path = PathBuilder::from_rect(target_r);
            let transform = self.state.transform;
            self.update_clip_mask_if_needed();
            let mask_ref = self.cached_mask.as_ref();
            self.device.as_pixmap_mut().fill_path(
                &path,
                &paint,
                tiny_skia::FillRule::Winding,
                transform,
                mask_ref,
            );
        }
    }
}

mod shapes;
#[cfg(test)]
mod tests;
mod text;

#[allow(unused_imports)]
use shapes::{clamped_span, is_rect_polygon};
pub use shapes::{create_arc_path, create_donut_arc_path, create_pie_path};
