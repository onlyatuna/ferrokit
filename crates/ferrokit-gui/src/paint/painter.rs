// SPDX-License-Identifier: LGPL-3.0-only
//! Painter primitives and 2D vector painting subsystem (`QPainter` equivalent).
//!
//! Controls stroke styling, fill patterns (gradients, textures, solid), transform stack,
//! composition modes, clipping, and text rendering.

use crate::geometry::primitives::{PointF, RectF};
use crate::geometry::transform::Transform2D;
pub use crate::paint::brush::Brush;
pub use crate::paint::composition::CompositionMode;
use crate::paint::paint_device::PaintDevice;
use crate::paint::path::PainterPath;
use crate::paint::pixmap::Pixmap;
use crate::text::document::TextDocument;
use crate::text::font::Font;
use crate::text::font_database::resolve_font_engines_for_text_global;
use crate::paint::text_blend::{blend_grey, blend_lcd};
use crate::text::glyph_layout::GlyphLayout;
use crate::text::smoothing::{gamma_lut, text_smoothing, TextSmoothing, TrcLut};
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
        if let Some(dash) = &self.dash_pattern {
            if let Some(dash_stroke) = tiny_skia::StrokeDash::new(dash.clone(), 0.0) {
                stroke.dash = Some(dash_stroke);
            }
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
    pub transform: Transform,
    pub opacity: f32,
    pub render_hints: RenderHints,
    pub composition_mode: CompositionMode,
    /// Bounding box of the clip in root logical coordinates; exact unless `clip_quads` is
    /// non-empty.
    pub clip_rect: Option<tiny_skia::Rect>,
    /// Rotated rect clips intersected with `clip_rect`; empty while every clip is axis-aligned.
    pub clip_quads: Vec<ClipQuad>,
}

impl Default for PainterState {
    fn default() -> Self {
        Self {
            pen: Some(Pen::new(Color::BLACK, 1.0)),
            brush: Brush::NoBrush,
            transform: Transform::identity(),
            opacity: 1.0,
            render_hints: RenderHints::default(),
            composition_mode: CompositionMode::SourceOver,
            clip_rect: None,
            clip_quads: Vec::new(),
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
        let v = if floor { v.floor() as i64 - pad } else { v.ceil() as i64 + pad };
        v.clamp(0, max as i64) as u32
    };
    (clamp(l, true, w), clamp(t, true, h), clamp(r, false, w), clamp(b, false, h))
}

/// Zeroes `bounds` of `mask`, clamped to the mask.
fn zero_mask_region(mask: &mut Mask, bounds: PixelBounds) {
    let (w, h) = (mask.width(), mask.height());
    let (x0, y0, x1, y1) = (bounds.0.min(w), bounds.1.min(h), bounds.2.min(w), bounds.3.min(h));
    if x0 >= x1 {
        return;
    }
    let data = mask.data_mut();
    for y in y0..y1 {
        let row = (y * w) as usize;
        data[row + x0 as usize..row + x1 as usize].fill(0);
    }
}

/// 2D vector painter (`QPainter` equivalent).
///
/// Returns a mutable reference to the active state.
pub struct Painter<'a> {
    device: &'a mut dyn PaintDevice,
    state: PainterState,
    saved_states: Vec<PainterState>,
    cached_mask: Option<Mask>,
    cached_clip_rect: Option<tiny_skia::Rect>,
    cached_clip_quads: Vec<ClipQuad>,
    /// Mask buffer kept while no clip is active, so a clip change reuses it instead of
    /// allocating a device-sized one.
    spare_mask: Option<Mask>,
    /// Second device-sized buffer the quads are rasterised into before being multiplied in.
    scratch_mask: Option<Mask>,
    /// Pixels of `cached_mask` / `spare_mask` that may be non-zero.
    mask_dirty: PixelBounds,
    /// Gamma table of LCD (ClearType) text; `None` draws grey-scale text. Fixed when painting
    /// starts, like `QRasterPaintEngine::begin` fixes the glyph cache format.
    lcd_lut: Option<Arc<TrcLut>>,
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
    fn user_rect_to_root_clip(&self, rect: RectF) -> (tiny_skia::Rect, Option<ClipQuad>) {
        let dpr = self.device.device_pixel_ratio();
        let t = self.state.transform;
        let mapped = (|| {
            let r = tiny_skia::Rect::from_xywh(rect.x, rect.y, rect.width, rect.height)?;
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
    fn empty_clip() -> tiny_skia::Rect {
        tiny_skia::Rect::from_xywh(0.0, 0.0, 0.0, 0.0)
            .expect("a zero-area rect at the origin is finite")
    }

    // True when the transform is the device-pixel-ratio scale plus a translation, i.e. the glyph
    // cache's per-glyph origin mapping is exact.
    fn text_transform_is_translate_only(&self) -> bool {
        let t = self.state.transform;
        let dpr = self.device.device_pixel_ratio();
        const EPS: f32 = 1e-6;
        (t.sx - dpr).abs() < EPS && (t.sy - dpr).abs() < EPS && t.kx.abs() < EPS && t.ky.abs() < EPS
    }

    // -------------------------------------------------------------------------

    // -------------------------------------------------------------------------

    // Updates or clears the cached clip mask based on current state. The mask buffer is reused
    // across clip changes: only the pixels the previous clip painted are zeroed.
    fn update_clip_mask_if_needed(&mut self) {
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
                    let paint = Paint {
                        shader: Pattern::new(
                            tile.as_ref(),
                            SpreadMode::Repeat,
                            filter,
                            opacity,
                            Transform::identity(),
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
                let paint = Paint {
                    shader: Pattern::new(
                        pattern.pixmap.as_tiny_skia().as_ref(),
                        SpreadMode::Repeat,
                        filter,
                        opacity,
                        pattern.transform,
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
    // Basic shapes and gauge geometry
    // -------------------------------------------------------------------------

    pub fn draw_line(&mut self, p1: PointF, p2: PointF) {
        let mut pb = PathBuilder::new();
        pb.move_to(p1.x, p1.y);
        pb.line_to(p2.x, p2.y);
        if let Some(path) = pb.finish() {
            self.stroke_path(&path);
            self.fill_aliased_line_end_pixels(&path, p1, p2);
        }
    }

    /// Qt's aliased thin-line endpoint rule: with pen width <= 1 and a non-flat cap, the pixel
    /// containing each endpoint is drawn too. Qt builds these caps in
    /// `QCosmeticStroker::drawLine` (qcosmeticstroker.cpp:263 `drawCaps`, :342-356 CapBegin|CapEnd)
    /// and the raster engine takes that path for width <= 1 (qpaintengine_raster.cpp:655-658).
    /// tiny-skia's Round cap and the Square cap on a diagonal miss the endpoint pixel centre, so
    /// the pixel is filled explicitly in device space. A pixel the stroke already covers is not
    /// filled again, since Qt blends each pixel once and a translucent pen would otherwise double
    /// up. Stroke coverage comes from rasterising the same outline into a small local mask.
    fn fill_aliased_line_end_pixels(&mut self, path: &Path, p1: PointF, p2: PointF) {
        let pen = match &self.state.pen {
            Some(pen) => pen.clone(),
            None => return,
        };
        if self.state.render_hints.contains(RenderHint::Antialiasing)
            || pen.width > 1.0
            || pen.cap == LineCap::Butt
        {
            return;
        }
        let transform = self.state.transform;
        let res_scale = tiny_skia::PathStroker::compute_resolution_scale(&transform);
        let Some(outline) = path.stroke(&pen.to_stroke(), res_scale) else {
            return;
        };
        let mut d1 = tiny_skia::Point::from_xy(p1.x, p1.y);
        let mut d2 = tiny_skia::Point::from_xy(p2.x, p2.y);
        transform.map_point(&mut d1);
        transform.map_point(&mut d2);
        // Local coverage spans both endpoint pixels plus the cap extent (half the pen width in
        // device space) and a 2 px margin.
        let scale = transform
            .sx
            .abs()
            .max(transform.sy.abs())
            .max(transform.kx.abs())
            .max(transform.ky.abs());
        let margin = 2 + (pen.width * scale).ceil() as i32;
        let x0 = d1.x.min(d2.x).floor() as i32 - margin;
        let y0 = d1.y.min(d2.y).floor() as i32 - margin;
        let x1 = d1.x.max(d2.x).ceil() as i32 + margin;
        let y1 = d1.y.max(d2.y).ceil() as i32 + margin;
        let width = (x1 - x0) as u32;
        let Some(mut coverage) = Mask::new(width, (y1 - y0) as u32) else {
            return;
        };
        coverage.fill_path(
            &outline,
            tiny_skia::FillRule::Winding,
            false,
            transform.post_translate(-x0 as f32, -y0 as f32),
        );
        let mut c = pen.color;
        c.apply_opacity(self.state.opacity);
        let paint = Paint {
            shader: Shader::SolidColor(c),
            blend_mode: self.state.composition_mode.into(),
            anti_alias: false,
            ..Default::default()
        };
        self.update_clip_mask_if_needed();
        let mask_ref = self.cached_mask.as_ref();
        for d in [d1, d2] {
            let (px, py) = (d.x.floor() as i32, d.y.floor() as i32);
            let local = (py - y0) as u32 * width + (px - x0) as u32;
            if coverage.data()[local as usize] != 0 {
                continue;
            }
            if let Some(rect) = tiny_skia::Rect::from_xywh(px as f32, py as f32, 1.0, 1.0) {
                let path = PathBuilder::from_rect(rect);
                self.device.as_pixmap_mut().fill_path(
                    &path,
                    &paint,
                    tiny_skia::FillRule::Winding,
                    Transform::identity(),
                    mask_ref,
                );
            }
        }
    }

    // Draws a rectangle using current brush and pen.
    pub fn draw_rect(&mut self, rect: RectF) {
        if let Some(r) = tiny_skia::Rect::from_xywh(rect.x, rect.y, rect.width, rect.height) {
            let path = PathBuilder::from_rect(r);
            self.fill_path(&path);
            self.stroke_path(&path);
        }
    }
    /// Fills a rectangle with a solid color, bypassing active brush and pen.
    pub fn fill_rect(&mut self, rect: RectF, color: Color) {
        let old_brush = self.state.brush.clone();
        let old_pen = self.state.pen.clone();
        self.set_brush(Brush::from_color(color));
        self.set_pen(None);
        if let Some(r) = tiny_skia::Rect::from_xywh(rect.x, rect.y, rect.width, rect.height) {
            let path = PathBuilder::from_rect(r);
            self.fill_path(&path);
        }
        self.state.brush = old_brush;
        self.state.pen = old_pen;
    }

    // Draws a rounded rectangle.
    pub fn draw_rounded_rect(&mut self, rect: RectF, rx: f32, ry: f32) {
        let rx = rx.min(rect.width / 2.0).max(0.0);
        let ry = ry.min(rect.height / 2.0).max(0.0);
        if rx <= 0.0 || ry <= 0.0 {
            self.draw_rect(rect);
            return;
        }
        let mut pb = PathBuilder::new();
        let x = rect.x;
        let y = rect.y;
        let w = rect.width;
        let h = rect.height;
        let k = 0.552_284_8; // Bezier circle approximation constant
        let kx = rx * k;
        let ky = ry * k;

        pb.move_to(x + rx, y);
        pb.line_to(x + w - rx, y);
        pb.cubic_to(x + w - rx + kx, y, x + w, y + ry - ky, x + w, y + ry);
        pb.line_to(x + w, y + h - ry);
        pb.cubic_to(
            x + w,
            y + h - ry + ky,
            x + w - rx + kx,
            y + h,
            x + w - rx,
            y + h,
        );
        pb.line_to(x + rx, y + h);
        pb.cubic_to(x + rx - kx, y + h, x, y + h - ry + ky, x, y + h - ry);
        pb.line_to(x, y + ry);
        pb.cubic_to(x, y + ry - ky, x + rx - kx, y, x + rx, y);
        pb.close();

        if let Some(path) = pb.finish() {
            self.fill_path(&path);
            self.stroke_path(&path);
        }
    }

    // Draws an ellipse.
    pub fn draw_ellipse(&mut self, rect: RectF) {
        if let Some(r) = tiny_skia::Rect::from_xywh(rect.x, rect.y, rect.width, rect.height) {
            if let Some(path) = PathBuilder::from_oval(r) {
                self.fill_path(&path);
                self.stroke_path(&path);
            }
        }
    }

    pub fn draw_polyline(&mut self, points: &[PointF]) {
        if points.len() < 2 {
            return;
        }
        let mut pb = PathBuilder::new();
        pb.move_to(points[0].x, points[0].y);
        for p in &points[1..] {
            pb.line_to(p.x, p.y);
        }
        if let Some(path) = pb.finish() {
            self.stroke_path(&path);
        }
    }

    // -------------------------------------------------------------------------
    // Returns a mutable reference to the active state.
    // -------------------------------------------------------------------------

    // Draws an arc.
    //
    // `start_deg`: start angle in degrees (90 degrees is top).
    // `span_deg`: sweep angle in degrees (negative for clockwise).
    pub fn draw_arc(&mut self, rect: RectF, start_deg: f32, span_deg: f32) {
        if let Some(path) = create_arc_path(rect, start_deg, span_deg) {
            self.stroke_path(&path);
        }
    }

    // Draws a pie slice.
    //

    pub fn draw_pie(&mut self, rect: RectF, start_deg: f32, span_deg: f32) {
        if let Some(path) = create_pie_path(rect, start_deg, span_deg) {
            self.fill_path(&path);
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
            anti_alias: self.state.render_hints.contains(RenderHint::Antialiasing),
            ..Default::default()
        };

        if let Some(target_r) =
            tiny_skia::Rect::from_xywh(target.x, target.y, target.width, target.height)
        {
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

    // -------------------------------------------------------------------------
    // Text rendering
    // -------------------------------------------------------------------------

    // Returns a mutable reference to the active state.
    //
    // Raster helpers
    // Sets opacity (0.0 to 1.0).
    pub fn draw_text(&mut self, pos: PointF, text: &str, font: &Font) {
        if text.is_empty() {
            return;
        }
        let engines = resolve_font_engines_for_text_global(font, text);
        if engines.is_empty() {
            return;
        }

        // Under rotation, scale or skew the translate-only glyph path below cannot place the
        // glyphs, but Qt draws the transformed glyphs (qpaintengine_raster.cpp:3094-3128). Fill the
        // glyph outlines with the pen colour instead; this is not pixel-identical to Qt's glyph cache.
        if !self.text_transform_is_translate_only() {
            if let Some(path) = Self::text_path(pos, text, font) {
                let color = self
                    .state
                    .pen
                    .as_ref()
                    .map(|p| p.color)
                    .unwrap_or(Color::BLACK);
                let brush = std::mem::replace(&mut self.state.brush, Brush::Color(color));
                self.fill_path_with_rule(&path, tiny_skia::FillRule::Winding);
                self.state.brush = brush;
            }
            return;
        }

        // Compute glyph layout using Qt 6 Run-based OpenType fallback
        let layout = GlyphLayout::shape_with_engines(text, font, &engines);

        let text_color = self
            .state
            .pen
            .as_ref()
            .map(|p| p.color)
            .unwrap_or(Color::BLACK);
        let base_alpha = (text_color.alpha() * self.state.opacity).clamp(0.0, 1.0);
        let target_r = (text_color.red() * 255.0).round() as u32;
        let target_g = (text_color.green() * 255.0).round() as u32;
        let target_b = (text_color.blue() * 255.0).round() as u32;

        let target = [target_r, target_g, target_b];
        let lcd_lut = self.lcd_lut.clone();
        let dpr = self.device.device_pixel_ratio();
        let transform = self.state.transform;
        // A rotated clip is not a pixel box, so the glyph blits read its coverage from the mask.
        self.update_clip_mask_if_needed();
        let clip_cov = if self.state.clip_quads.is_empty() {
            None
        } else {
            self.cached_mask.as_ref().map(|m| (m.data(), m.width() as usize))
        };
        let mut pixmap = self.device.as_pixmap_mut();
        let pw = pixmap.width();
        let ph = pixmap.height();
        let pix_data = pixmap.data_mut();

        for glyph in layout.glyphs {
            let lx = pos.x + glyph.x;
            let ly = pos.y + glyph.y;

            let px = transform.sx * lx + transform.kx * ly + transform.tx;
            let py = transform.ky * lx + transform.sy * ly + transform.ty;

            let engine = match engines.get(glyph.font_index as usize) {
                Some(e) => e,
                None => &engines[0],
            };

            // Qt 6 parity (QWindowsFontEngineDirectWrite::alphaRGBMapForGlyph):
            // Check if this glyph is an OpenType COLRv0/CPAL color glyph (e.g. Segoe UI Emoji)
            if let Some((color_metrics, color_pixmap)) =
                engine.rasterize_color_glyph(glyph.glyph_id, font.size * dpr)
            {
                let start_x = (px + color_metrics.xmin as f32).round() as i32;
                let start_y =
                    (py - color_metrics.ymin as f32 - color_metrics.height as f32).round() as i32;
                let c_w = color_metrics.width;
                let c_h = color_metrics.height;
                let color_data = color_pixmap.data();

                for gy in 0..c_h {
                    let dst_y = start_y + gy as i32;
                    if dst_y < 0 || dst_y >= ph as i32 {
                        continue;
                    }
                    if let Some(clip) = self.state.clip_rect {
                        let cy0 = (clip.top() * dpr).floor() as i32;
                        let cy1 = (clip.bottom() * dpr).ceil() as i32;
                        if dst_y < cy0 || dst_y >= cy1 {
                            continue;
                        }
                    }
                    for gx in 0..c_w {
                        let dst_x = start_x + gx as i32;
                        if dst_x < 0 || dst_x >= pw as i32 {
                            continue;
                        }
                        if let Some(clip) = self.state.clip_rect {
                            let cx0 = (clip.left() * dpr).floor() as i32;
                            let cx1 = (clip.right() * dpr).ceil() as i32;
                            if dst_x < cx0 || dst_x >= cx1 {
                                continue;
                            }
                        }
                        let cf = match clip_cov {
                            Some((cov, mw)) => cov[dst_y as usize * mw + dst_x as usize],
                            None => 255,
                        };
                        if cf == 0 {
                            continue;
                        }

                        let src_idx = (gy * c_w + gx) * 4;
                        let src_raw_r = color_data[src_idx] as u32;
                        let src_raw_g = color_data[src_idx + 1] as u32;
                        let src_raw_b = color_data[src_idx + 2] as u32;
                        let src_raw_a = color_data[src_idx + 3] as u32;
                        if src_raw_a == 0 {
                            continue;
                        }

                        let op = self.state.opacity.clamp(0.0, 1.0) * cf as f32 / 255.0;
                        let a_factor = (src_raw_a as f32 / 255.0) * op;
                        let src_a = ((a_factor * 255.0).round() as u32).min(255);
                        let inv_a = 255 - src_a;

                        // tiny-skia Pixmap is premultiplied RGBA
                        let src_pr = ((src_raw_r as f32 * op).round() as u32).min(255);
                        let src_pg = ((src_raw_g as f32 * op).round() as u32).min(255);
                        let src_pb = ((src_raw_b as f32 * op).round() as u32).min(255);

                        let dst_idx = ((dst_y as usize) * (pw as usize) + (dst_x as usize)) * 4;
                        let cur_r = pix_data[dst_idx] as u32;
                        let cur_g = pix_data[dst_idx + 1] as u32;
                        let cur_b = pix_data[dst_idx + 2] as u32;
                        let cur_a = pix_data[dst_idx + 3] as u32;

                        pix_data[dst_idx] = ((src_pr + (cur_r * inv_a) / 255).min(255)) as u8;
                        pix_data[dst_idx + 1] = ((src_pg + (cur_g * inv_a) / 255).min(255)) as u8;
                        pix_data[dst_idx + 2] = ((src_pb + (cur_b * inv_a) / 255).min(255)) as u8;
                        pix_data[dst_idx + 3] = ((src_a + (cur_a * inv_a) / 255).min(255)) as u8;
                    }
                }
                continue;
            }

            // Qt draws LCD masks when ClearType is on and the face has them
            // (`QRasterPaintEngine::drawCachedGlyphs`, `alphaRGBMapForGlyph`).
            let lcd_glyph = lcd_lut
                .as_ref()
                .and_then(|_| engine.rasterize_lcd_glyph(glyph.glyph_id, font.size, dpr));
            let (metrics, bitmap) = match &lcd_glyph {
                Some((metrics, bitmap)) => (*metrics, bitmap.clone()),
                None => engine.rasterize_glyph(glyph.glyph_id, font.size, dpr),
            };
            if metrics.width == 0 || metrics.height == 0 {
                continue;
            }

            let start_x = (px + metrics.xmin as f32).round() as i32;
            let start_y = (py - metrics.ymin as f32 - metrics.height as f32).round() as i32;

            if let Some(clip) = self.state.clip_rect {
                let cx0 = (clip.left() * dpr).floor() as i32;
                let cy0 = (clip.top() * dpr).floor() as i32;
                let cx1 = (clip.right() * dpr).ceil() as i32;
                let cy1 = (clip.bottom() * dpr).ceil() as i32;
                let glyph_x1 = start_x + metrics.width as i32;
                let glyph_y1 = start_y + metrics.height as i32;
                if glyph_x1 <= cx0 || start_x >= cx1 || glyph_y1 <= cy0 || start_y >= cy1 {
                    continue;
                }
            }

            for gy in 0..metrics.height {
                let dst_y = start_y + gy as i32;
                if dst_y < 0 || dst_y >= ph as i32 {
                    continue;
                }
                if let Some(clip) = self.state.clip_rect {
                    let cy0 = (clip.top() * dpr).floor() as i32;
                    let cy1 = (clip.bottom() * dpr).ceil() as i32;
                    if dst_y < cy0 || dst_y >= cy1 {
                        continue;
                    }
                }
                for gx in 0..metrics.width {
                    let dst_x = start_x + gx as i32;
                    if dst_x < 0 || dst_x >= pw as i32 {
                        continue;
                    }
                    if let Some(clip) = self.state.clip_rect {
                        let cx0 = (clip.left() * dpr).floor() as i32;
                        let cx1 = (clip.right() * dpr).ceil() as i32;
                        if dst_x < cx0 || dst_x >= cx1 {
                            continue;
                        }
                    }
                    let cf = match clip_cov {
                        Some((cov, mw)) => cov[dst_y as usize * mw + dst_x as usize],
                        None => 255,
                    };
                    if cf == 0 {
                        continue;
                    }
                    let base_alpha = base_alpha * cf as f32 / 255.0;

                    let dst_idx = ((dst_y as usize) * (pw as usize) + (dst_x as usize)) * 4;
                    let px = &mut pix_data[dst_idx..dst_idx + 4];
                    match (&lcd_glyph, &lcd_lut) {
                        (Some(_), Some(lut)) => {
                            let i = (gy * metrics.width + gx) * 3;
                            let cov = [bitmap[i], bitmap[i + 1], bitmap[i + 2]];
                            blend_lcd(px, cov, base_alpha, target, lut);
                        }
                        _ => {
                            let glyph_alpha = bitmap[gy * metrics.width + gx];
                            if glyph_alpha != 0 {
                                blend_grey(px, glyph_alpha, base_alpha, target);
                            }
                        }
                    }
                }
            }
        }
    }

    /// The outline of `text` with its baseline origin at `pos` (`QPainterPath::addText`): the
    /// unhinted glyph outlines at the positions the text layout gives them, ready for
    /// [`fill_path`](Self::fill_path) and [`stroke_path`](Self::stroke_path). `None` when the text
    /// has no outlines (empty, or a backend that cannot produce them).
    pub fn text_path(pos: PointF, text: &str, font: &Font) -> Option<Path> {
        if text.is_empty() {
            return None;
        }
        let engines = resolve_font_engines_for_text_global(font, text);
        if engines.is_empty() {
            return None;
        }
        let layout = GlyphLayout::shape_with_engines(text, font, &engines);
        let mut builder = tiny_skia::PathBuilder::new();
        for glyph in layout.glyphs {
            let engine = engines.get(glyph.font_index as usize).unwrap_or(&engines[0]);
            let Some(outline) = engine.face.glyph_outline(glyph.glyph_id, font.size) else {
                continue;
            };
            let placed = outline.transform(tiny_skia::Transform::from_translate(
                pos.x + glyph.x,
                pos.y + glyph.y,
            ));
            if let Some(placed) = placed {
                builder.push_path(&placed);
            }
        }
        builder.finish()
    }
    /// Draws text with a specific color.
    pub fn draw_text_colored(&mut self, pos: PointF, text: &str, font: &Font, color: Color) {
        let old_pen = self.state.pen.clone();
        self.set_pen(Pen::new(color, 1.0));
        self.draw_text(pos, text, font);
        self.state.pen = old_pen;
    }

    /// Renders a multi-line formatted `TextDocument` at the specified position.
    pub fn draw_text_document(&mut self, pos: PointF, doc: &TextDocument) {
        doc.draw(self, pos);
    }
}

pub fn create_arc_path(rect: RectF, start_deg: f32, span_deg: f32) -> Option<Path> {
    if span_deg.abs() < 1e-4 || rect.width <= 0.0 || rect.height <= 0.0 {
        return None;
    }
    let rx = rect.width / 2.0;
    let ry = rect.height / 2.0;
    let cx = rect.x + rx;
    let cy = rect.y + ry;

    let mut pb = PathBuilder::new();
    let num_segments = (span_deg.abs() / 45.0).ceil().max(1.0) as usize;
    let step_deg = span_deg / num_segments as f32;

    for i in 0..num_segments {
        let a1_deg = start_deg + i as f32 * step_deg;
        let a2_deg = a1_deg + step_deg;
        // Returns a mutable reference to the active state.
        let a1 = a1_deg.to_radians();
        let a2 = a2_deg.to_radians();

        let p1 = (cx + rx * a1.cos(), cy - ry * a1.sin());
        let p2 = (cx + rx * a2.cos(), cy - ry * a2.sin());

        let delta = (a2 - a1) / 2.0;
        let k = (4.0 / 3.0) * (delta.sin() / (1.0 + delta.cos()));

        let c1x = p1.0 - k * rx * a1.sin();
        let c1y = p1.1 - k * ry * a1.cos();
        let c2x = p2.0 + k * rx * a2.sin();
        let c2y = p2.1 + k * ry * a2.cos();

        if i == 0 {
            pb.move_to(p1.0, p1.1);
        }
        pb.cubic_to(c1x, c1y, c2x, c2y, p2.0, p2.1);
    }

    pb.finish()
}

pub fn create_pie_path(rect: RectF, start_deg: f32, span_deg: f32) -> Option<Path> {
    if span_deg.abs() < 1e-4 || rect.width <= 0.0 || rect.height <= 0.0 {
        return None;
    }
    let rx = rect.width / 2.0;
    let ry = rect.height / 2.0;
    let cx = rect.x + rx;
    let cy = rect.y + ry;

    let mut pb = PathBuilder::new();
    // Start from center
    pb.move_to(cx, cy);

    let num_segments = (span_deg.abs() / 45.0).ceil().max(1.0) as usize;
    let step_deg = span_deg / num_segments as f32;

    for i in 0..num_segments {
        let a1_deg = start_deg + i as f32 * step_deg;
        let a2_deg = a1_deg + step_deg;
        let a1 = a1_deg.to_radians();
        let a2 = a2_deg.to_radians();

        let p1 = (cx + rx * a1.cos(), cy - ry * a1.sin());
        let p2 = (cx + rx * a2.cos(), cy - ry * a2.sin());

        let delta = (a2 - a1) / 2.0;
        let k = (4.0 / 3.0) * (delta.sin() / (1.0 + delta.cos()));

        let c1x = p1.0 - k * rx * a1.sin();
        let c1y = p1.1 - k * ry * a1.cos();
        let c2x = p2.0 + k * rx * a2.sin();
        let c2y = p2.1 + k * ry * a2.cos();

        if i == 0 {
            pb.line_to(p1.0, p1.1);
        }
        pb.cubic_to(c1x, c1y, c2x, c2y, p2.0, p2.1);
    }

    // Close back to center
    pb.close();
    pb.finish()
}
pub fn create_donut_arc_path(
    rect: RectF,
    ring_width: f32,
    start_deg: f32,
    span_deg: f32,
) -> Option<Path> {
    if span_deg.abs() < 1e-4 || rect.width <= 0.0 || rect.height <= 0.0 {
        return None;
    }
    let r_out_x = rect.width / 2.0;
    let r_out_y = rect.height / 2.0;
    let r_in_x = (r_out_x - ring_width).max(0.0);
    let r_in_y = (r_out_y - ring_width).max(0.0);
    let cx = rect.x + r_out_x;
    let cy = rect.y + r_out_y;

    let mut pb = PathBuilder::new();
    let num_segments = (span_deg.abs() / 45.0).ceil().max(1.0) as usize;
    let step_deg = span_deg / num_segments as f32;

    // Outer arc forward
    for i in 0..num_segments {
        let a1_deg = start_deg + i as f32 * step_deg;
        let a2_deg = a1_deg + step_deg;
        let a1 = a1_deg.to_radians();
        let a2 = a2_deg.to_radians();

        let p1 = (cx + r_out_x * a1.cos(), cy - r_out_y * a1.sin());
        let p2 = (cx + r_out_x * a2.cos(), cy - r_out_y * a2.sin());

        let delta = (a2 - a1) / 2.0;
        let k = (4.0 / 3.0) * (delta.sin() / (1.0 + delta.cos()));

        let c1x = p1.0 - k * r_out_x * a1.sin();
        let c1y = p1.1 - k * r_out_y * a1.cos();
        let c2x = p2.0 + k * r_out_x * a2.sin();
        let c2y = p2.1 + k * r_out_y * a2.cos();

        if i == 0 {
            pb.move_to(p1.0, p1.1);
        }
        pb.cubic_to(c1x, c1y, c2x, c2y, p2.0, p2.1);
    }

    // Inner arc backward
    for i in (0..num_segments).rev() {
        let a1_deg = start_deg + (i + 1) as f32 * step_deg;
        let a2_deg = a1_deg - step_deg;
        let a1 = a1_deg.to_radians();
        let a2 = a2_deg.to_radians();

        let p1 = (cx + r_in_x * a1.cos(), cy - r_in_y * a1.sin());
        let p2 = (cx + r_in_x * a2.cos(), cy - r_in_y * a2.sin());

        let delta = (a2 - a1) / 2.0;
        let k = (4.0 / 3.0) * (delta.sin() / (1.0 + delta.cos()));

        let c1x = p1.0 - k * r_in_x * a1.sin();
        let c1y = p1.1 - k * r_in_y * a1.cos();
        let c2x = p2.0 + k * r_in_x * a2.sin();
        let c2y = p2.1 + k * r_in_y * a2.cos();

        pb.line_to(p1.0, p1.1);
        pb.cubic_to(c1x, c1y, c2x, c2y, p2.0, p2.1);
    }

    pb.close();
    pb.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pen_creation_and_customization() {
        let pen = Pen::new(Color::from_rgba8(255, 0, 0, 255), 2.5);
        assert_eq!(pen.width, 2.5);
        assert_eq!(pen.cap, LineCap::Square);
        assert_eq!(pen.join, LineJoin::Bevel);
        assert!(pen.dash_pattern.is_none());

        let customized = pen
            .with_cap(LineCap::Square)
            .with_join(LineJoin::Miter)
            .with_dash_pattern(vec![5.0, 5.0]);

        assert_eq!(customized.cap, LineCap::Square);
        assert_eq!(customized.join, LineJoin::Miter);
        assert_eq!(customized.dash_pattern, Some(vec![5.0, 5.0]));

        let stroke = customized.to_stroke();
        assert_eq!(stroke.width, 2.5);
        assert_eq!(stroke.line_cap, LineCap::Square);
        assert_eq!(stroke.line_join, LineJoin::Miter);
        assert!(stroke.dash.is_some());
    }

    #[test]
    fn gradient_fills_honor_painter_opacity() {
        use crate::paint::brush::{LinearGradient, RadialGradient};
        let red = Color::from_rgba8(255, 0, 0, 255);
        let mut lin = LinearGradient::new(PointF::new(0.0, 0.0), PointF::new(20.0, 0.0));
        lin.add_stop(0.0, red);
        lin.add_stop(1.0, red);
        let mut rad = RadialGradient::new(PointF::new(10.0, 10.0), 30.0);
        rad.add_stop(0.0, red);
        rad.add_stop(1.0, red);
        for brush in [Brush::linear_gradient(lin), Brush::radial_gradient(rad)] {
            let mut pm = crate::paint::pixmap::Pixmap::new(20, 20).unwrap();
            {
                let mut p = Painter::begin(&mut pm);
                p.set_pen(None);
                p.set_brush(brush);
                p.set_opacity(0.5);
                p.draw_rect(RectF::new(0.0, 0.0, 20.0, 20.0));
            }
            let a = pm.pixel(10, 10).unwrap().alpha();
            assert!((126..=129).contains(&a), "alpha {a}");
        }
    }

    #[test]
    fn test_brush_variants_and_default() {
        assert_eq!(Brush::default(), Brush::NoBrush);

        let solid = Brush::from_rgba8(10, 20, 30, 255);
        assert_eq!(solid, Brush::Color(Color::from_rgba8(10, 20, 30, 255)));

        let hatched = Brush::hatched(Color::from_rgba8(255, 255, 0, 180));
        assert_eq!(
            hatched,
            Brush::Hatched {
                color: Color::from_rgba8(255, 255, 0, 180)
            }
        );
    }

    #[test]
    fn test_painter_state_default_and_clone() {
        let state = PainterState::default();
        assert!(state.pen.is_some());
        assert_eq!(state.brush, Brush::NoBrush);
        assert_eq!(state.opacity, 1.0);
        // Qt `QPainter::begin` leaves only TextAntialiasing set (`qpainter.cpp:1847`).
        assert_eq!(state.render_hints, RenderHints::DEFAULT);
        assert!(!state.render_hints.contains(RenderHint::Antialiasing));
        assert!(state.clip_rect.is_none());
        assert_eq!(state.transform, tiny_skia::Transform::identity());

        let mut cloned = state.clone();
        cloned.opacity = 0.5;
        cloned.pen = None;
        cloned.brush = Brush::from_rgba8(255, 0, 0, 255);
        assert_ne!(state, cloned);
        assert_eq!(cloned.opacity, 0.5);
        assert!(cloned.pen.is_none());
    }

    #[test]
    fn test_painter_begin_dpr_transform() {
        use crate::paint::pixmap::Pixmap;

        // DPR 1.0
        let mut pm1 = Pixmap::new(100, 100).unwrap();
        let painter1 = Painter::begin(&mut pm1);
        assert_eq!(painter1.state().transform, tiny_skia::Transform::identity());

        // DPR 2.0
        let mut pm2 = Pixmap::with_dpr(200, 100, 2.0).unwrap();
        let painter2 = Painter::begin(&mut pm2);
        assert_eq!(
            painter2.state().transform,
            tiny_skia::Transform::from_scale(2.0, 2.0)
        );
    }

    #[test]
    fn test_painter_transforms_and_state_stack() {
        let mut pm = Pixmap::new(100, 100).unwrap();
        let mut p = Painter::begin(&mut pm);

        p.set_opacity(0.8);
        p.translate(10.0, 20.0);
        p.save();

        p.scale(2.0, 2.0);
        p.set_opacity(0.4);
        assert_eq!(p.state().opacity, 0.4);

        p.restore();
        assert_eq!(p.state().opacity, 0.8);

        // Returns a mutable reference to the active state.
    }

    #[test]
    fn test_painter_draw_primitives_headless() {
        let mut pm = Pixmap::new(200, 200).unwrap();
        let mut p = Painter::begin(&mut pm);

        // Line
        p.set_pen(Pen::from_rgba8(255, 0, 0, 255, 2.0));
        p.draw_line(PointF::new(0.0, 0.0), PointF::new(50.0, 50.0));

        // Rect
        p.set_brush(Brush::from_rgba8(0, 255, 0, 255));
        p.draw_rect(RectF::new(10.0, 10.0, 40.0, 40.0));

        // Rounded rect
        p.draw_rounded_rect(RectF::new(60.0, 10.0, 40.0, 40.0), 5.0, 5.0);

        // Ellipse
        p.draw_ellipse(RectF::new(10.0, 60.0, 40.0, 40.0));

        let points = [
            PointF::new(0.0, 0.0),
            PointF::new(10.0, 20.0),
            PointF::new(20.0, 10.0),
        ];
        p.draw_polyline(&points);

        assert!(pm.data().iter().any(|&b| b > 0));
    }

    #[test]
    fn test_painter_draw_arc_and_pie() {
        let mut pm = Pixmap::new(200, 200).unwrap();
        let mut p = Painter::begin(&mut pm);

        p.set_pen(Pen::from_rgba8(255, 255, 0, 255, 3.0));
        p.set_brush(Brush::from_rgba8(0, 0, 255, 128));

        // Arc (outer ring of a gauge)
        p.draw_arc(RectF::new(20.0, 20.0, 100.0, 100.0), 90.0, -180.0);

        p.draw_pie(RectF::new(20.0, 20.0, 100.0, 100.0), 0.0, 90.0);

        assert!(pm.data().iter().any(|&b| b > 0));
    }

    #[test]
    fn test_painter_draw_pixmap_and_clip() {
        let mut dest = Pixmap::new(100, 100).unwrap();
        let mut src = Pixmap::new(50, 50).unwrap();
        src.fill(Color::from_rgba8(255, 0, 0, 255));

        let mut p = Painter::begin(&mut dest);
        p.set_clip_rect(RectF::new(10.0, 10.0, 80.0, 80.0));
        p.draw_pixmap(RectF::new(0.0, 0.0, 50.0, 50.0), &src, None);

        assert!(dest.data().iter().any(|&b| b > 0));
    }

    #[test]
    fn clip_change_reuses_the_mask_buffer() {
        let mut dest = Pixmap::new(64, 64).unwrap();
        let mut p = Painter::begin(&mut dest);
        let fill = |p: &mut Painter| p.fill_rect(RectF::new(0.0, 0.0, 64.0, 64.0), Color::WHITE);
        p.set_clip_rect(RectF::new(0.0, 0.0, 32.0, 32.0));
        fill(&mut p);
        let first = p.cached_mask.as_ref().unwrap().data().as_ptr();
        p.set_clip_rect(RectF::new(16.0, 16.0, 32.0, 32.0));
        fill(&mut p);
        let second = p.cached_mask.as_ref().unwrap().data().as_ptr();
        assert_eq!(first, second, "a clip change must reuse the device-sized mask");
        // Dropping the clip and setting another still reuses the same buffer.
        p.state_mut().clip_rect = None;
        fill(&mut p);
        assert!(p.cached_mask.is_none());
        p.set_clip_rect(RectF::new(8.0, 8.0, 8.0, 8.0));
        fill(&mut p);
        assert_eq!(first, p.cached_mask.as_ref().unwrap().data().as_ptr());
        // Only the new clip's pixels are non-zero: the buffer was cleared where it was dirty.
        let mask = p.cached_mask.as_ref().unwrap();
        let covered = mask.data().iter().filter(|&&a| a != 0).count();
        assert_eq!(covered, 8 * 8);
    }

    // -------------------------------------------------------------------------
    // Returns a mutable reference to the active state.
    // -------------------------------------------------------------------------

    #[test]
    fn test_painter_draw_rect_and_fill() {
        let mut surface = Pixmap::new(100, 100).unwrap();
        // Fill with white
        surface.fill(tiny_skia::Color::WHITE);

        {
            let mut painter = Painter::begin(&mut surface);
            painter.set_pen(None);
            painter.set_brush(Brush::Color(tiny_skia::Color::from_rgba8(255, 0, 0, 255)));

            painter.draw_rect(RectF::new(40.0, 40.0, 20.0, 20.0));
        }

        let data = surface.data();

        let get_pixel = |x: usize, y: usize| -> (u8, u8, u8, u8) {
            let idx = (y * 100 + x) * 4;
            (data[idx], data[idx + 1], data[idx + 2], data[idx + 3])
        };

        assert_eq!(get_pixel(10, 10), (255, 255, 255, 255));

        assert_eq!(get_pixel(50, 50), (255, 0, 0, 255));
    }

    // -------------------------------------------------------------------------

    // -------------------------------------------------------------------------

    #[test]
    fn test_render_dial_to_png() {
        let mut surface = Pixmap::new(300, 300).unwrap();
        surface.fill(tiny_skia::Color::TRANSPARENT);

        {
            let mut painter = Painter::begin(&mut surface);

            // Returns a mutable reference to the active state.
            let bounds = RectF::new(20.0, 20.0, 260.0, 260.0);
            painter.set_pen(Pen::new(
                tiny_skia::Color::from_rgba8(40, 40, 40, 255),
                14.0,
            ));
            painter.set_brush(Brush::NoBrush);
            painter.draw_ellipse(bounds);

            painter.set_pen(Pen::new(
                tiny_skia::Color::from_rgba8(50, 205, 50, 255),
                14.0,
            ));
            painter.draw_arc(bounds, 90.0, -270.0);

            let inner_bounds = RectF::new(60.0, 60.0, 180.0, 180.0);
            painter.set_pen(None);
            painter.set_brush(Brush::Color(tiny_skia::Color::from_rgba8(255, 140, 0, 200)));
            painter.draw_pie(inner_bounds, 90.0, -120.0);

            let font = Font::new("Arial", 18.0)
                .with_weight(crate::text::font::FontWeight::Bold)
                .with_feature(crate::text::font::FontTag::TNUM, 1);
            painter.set_pen(Pen::from_rgba8(255, 255, 255, 255, 1.0));
            painter.draw_text(PointF::new(105.0, 155.0), "75%", &font);
        }

        // Save PNG
        let out_dir = std::path::Path::new("target");
        if !out_dir.exists() {
            let _ = std::fs::create_dir_all(out_dir);
        }
        let output_path = out_dir.join("test_dial.png");
        surface.save_png(&output_path).expect("failed to save PNG");
        assert!(output_path.exists());
    }

    #[test]
    fn test_painter_draw_text_headless_probe() {
        let mut surface = Pixmap::new(100, 100).unwrap();
        surface.fill(tiny_skia::Color::TRANSPARENT);

        {
            let mut painter = Painter::begin(&mut surface);
            painter.set_pen(Pen::from_rgba8(0, 255, 0, 255, 1.0));
            let font = Font::new("Arial", 16.0);
            painter.draw_text(PointF::new(20.0, 50.0), "HUD", &font);
        }

        // Verify rendered text contains green pixels
        let data = surface.data();
        let has_green = data
            .as_chunks::<4>()
            .0
            .iter()
            .any(|p| p[1] > 50 && p[3] > 0);
        assert!(has_green, "Canvas must contain rendered green text pixels");
    }

    // Qt's fresh QPainter has renderHints()==TextAntialiasing only, so shapes are aliased.
    // QPainter probe (PySide6 6.11.2): 30.3px ellipse with no hint has 0 partial-alpha pixels;
    // with Antialiasing it has 114.
    #[test]
    fn g10_4d_default_shapes_are_aliased_like_qt() {
        let mut pm = Pixmap::new(40, 40).unwrap();
        pm.fill(tiny_skia::Color::TRANSPARENT);
        {
            let mut p = Painter::begin(&mut pm);
            p.set_pen(None);
            p.set_brush(Brush::from_rgba8(255, 0, 0, 255));
            p.draw_ellipse(RectF::new(2.5, 2.5, 30.3, 30.3));
        }
        let partial = pm
            .data()
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|px| px[3] > 0 && px[3] < 255)
            .count();
        assert_eq!(
            partial, 0,
            "fresh QPainter has no Antialiasing hint (Qt renderHints()==TextAntialiasing only): \
             ellipse edge must be aliased, found {partial} partial-alpha pixels"
        );
    }

    // draw_pixmap samples nearest-neighbour unless SmoothPixmapTransform is set (Qt
    // default). Source: left half red, right half blue, 4x4 px. Target: 10x10 px, scale 2.5.
    // Nearest yields only pure red or pure blue opaque pixels; bilinear mixes at the seam.
    #[test]
    fn g10_4d_draw_pixmap_is_nearest_without_smooth_pixmap_transform() {
        let mut src = Pixmap::new(4, 4).unwrap();
        {
            let mut sp = Painter::begin(&mut src);
            sp.set_pen(None);
            sp.set_brush(Brush::from_rgba8(255, 0, 0, 255));
            sp.draw_rect(RectF::new(0.0, 0.0, 2.0, 4.0));
            sp.set_brush(Brush::from_rgba8(0, 0, 255, 255));
            sp.draw_rect(RectF::new(2.0, 0.0, 2.0, 4.0));
        }
        let mut dest = Pixmap::new(10, 10).unwrap();
        {
            let mut p = Painter::begin(&mut dest);
            p.draw_pixmap(RectF::new(0.0, 0.0, 10.0, 10.0), &src, None);
        }
        let stride = 10 * 4;
        for y in 0..10usize {
            for x in 0..10usize {
                let i = y * stride + x * 4;
                let px = &dest.data()[i..i + 4];
                let (r, b) = (px[0], px[2]);
                assert_eq!(px[3], 255, "pixel ({x},{y}) must be opaque");
                let pure_red = r == 255 && b == 0;
                let pure_blue = r == 0 && b == 255;
                assert!(
                    pure_red || pure_blue,
                    "pixel ({x},{y}) is a mixed colour rgba=({},{},{},{}); Qt nearest sampling gives pure colours",
                    px[0], px[1], px[2], px[3]
                );
            }
        }
    }

    // An aliased cosmetic line with a non-flat cap covers both end pixels. Qt's default
    // pen is SquareCap (qpen.cpp:203); QCosmeticStroker::drawLine adds caps unless FlatCap
    // (qcosmeticstroker.cpp:263, CapBegin|CapEnd at :356). PySide6 6.11.2 probe, 1px black on
    // white, Antialiasing off, same result for Square and Round: H (8,36)-(258,36) is black at
    // x=8 and x=258 and white at x=7 and x=259; V (36,8)-(36,258) and D (8,8)-(258,158) likewise.
    // FlatCap: the far end pixel (258) stays white.
    #[test]
    fn g10_4d_aliased_line_includes_endpoint_pixels_like_qt() {
        const W: u32 = 300;
        let red = |pm: &Pixmap, x: usize, y: usize| pm.data()[(y * W as usize + x) * 4];
        let draw = |p1: (f32, f32), p2: (f32, f32), cap: LineCap| {
            let mut pm = Pixmap::new(W, W).unwrap();
            pm.fill(tiny_skia::Color::WHITE);
            {
                let mut p = Painter::begin(&mut pm);
                p.set_pen(Pen::new(tiny_skia::Color::BLACK, 1.0).with_cap(cap));
                p.draw_line(PointF::new(p1.0, p1.1), PointF::new(p2.0, p2.1));
            }
            pm
        };
        // (start, end, pixel before start along the line, pixel after end along the line)
        let lines = [
            (
                (8.0, 36.0),
                (258.0, 36.0),
                (8, 36),
                (258, 36),
                (7, 36),
                (259, 36),
            ),
            (
                (36.0, 8.0),
                (36.0, 258.0),
                (36, 8),
                (36, 258),
                (36, 7),
                (36, 259),
            ),
            (
                (8.0, 8.0),
                (258.0, 158.0),
                (8, 8),
                (258, 158),
                (7, 7),
                (259, 159),
            ),
        ];
        for cap in [LineCap::Square, LineCap::Round] {
            for (p1, p2, start, end, before, after) in lines {
                let pm = draw(p1, p2, cap);
                let msg = format!("{cap:?} {p1:?}->{p2:?}");
                assert_eq!(
                    red(&pm, start.0, start.1),
                    0,
                    "{msg}: start pixel must be drawn"
                );
                assert_eq!(red(&pm, end.0, end.1), 0, "{msg}: end pixel must be drawn");
                assert_eq!(
                    red(&pm, before.0, before.1),
                    255,
                    "{msg}: pixel before start must stay white"
                );
                assert_eq!(
                    red(&pm, after.0, after.1),
                    255,
                    "{msg}: pixel after end must stay white"
                );
            }
        }
        for (p1, p2, start, end, _, _) in lines {
            let pm = draw(p1, p2, LineCap::Butt);
            let msg = format!("FlatCap {p1:?}->{p2:?}");
            assert_eq!(
                red(&pm, start.0, start.1),
                0,
                "{msg}: start pixel must be drawn"
            );
            assert_eq!(
                red(&pm, end.0, end.1),
                255,
                "{msg}: FlatCap must not draw the end pixel"
            );
        }
    }

    // The endpoint pixel is blended once. Qt draws each pixel of an aliased cosmetic
    // line once (QCosmeticStroker::drawLine, qcosmeticstroker.cpp:263/:356). A menu separator
    // (menu.rs paint_item): white alpha 31 over (22,25,32) from x=9.0 to x=258.0 at y=36.5.
    // PySide6 6.11.2 probe, Antialiasing off: x=8 and x=259 stay (22,25,32); x=9..=258 are all
    // (50,53,59), with x=9 and x=258 identical to the interior.
    #[test]
    fn g10_4d_aliased_line_endpoint_is_blended_once() {
        const W: u32 = 300;
        let bg = [22u8, 25, 32];
        let mut pm = Pixmap::new(W, W).unwrap();
        pm.fill(tiny_skia::Color::from_rgba8(bg[0], bg[1], bg[2], 255));
        {
            let mut p = Painter::begin(&mut pm);
            p.set_render_hint(RenderHint::Antialiasing, false);
            p.set_pen(Pen::new(
                tiny_skia::Color::from_rgba8(255, 255, 255, 31),
                1.0,
            ));
            p.draw_line(PointF::new(9.0, 36.5), PointF::new(258.0, 36.5));
        }
        let px = |x: usize, y: usize| {
            let i = (y * W as usize + x) * 4;
            [pm.data()[i], pm.data()[i + 1], pm.data()[i + 2]]
        };
        assert_eq!(px(8, 36), bg, "pixel before the start must stay background");
        assert_eq!(px(259, 36), bg, "pixel after the end must stay background");
        let interior = px(100, 36);
        for x in 9..=258usize {
            assert_eq!(
                px(x, 36),
                interior,
                "x={x}: each pixel of the line must be blended exactly once"
            );
        }
    }
}
