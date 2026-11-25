// SPDX-License-Identifier: LGPL-3.0-only
use super::*;

impl<'a> Painter<'a> {
    // -------------------------------------------------------------------------
    // Text rendering
    // -------------------------------------------------------------------------

    // Returns a mutable reference to the active state.
    //
    // Raster helpers
    // Sets opacity (0.0 to 1.0).
    /// `QPainter::drawText(QPointF, QString)` (qpainter.cpp:5698): `pos` is the baseline origin of
    /// the line. The text is itemized by bidi level and tabs, each item shaped in its own
    /// direction, and the items are drawn from `pos` in their visual order, each as wide as its
    /// advance (`text::line_items`). The base direction is the painter's
    /// [`layout_direction`](Self::layout_direction).
    pub fn draw_text(&mut self, pos: PointF, text: &str, font: &Font) {
        let option = TextOption {
            text_direction: self.state.layout_direction,
            ..TextOption::default()
        };
        let items = LineItems::new(text, font, &option);
        // The pen position is a `QFixed` in Qt, 1/64 px.
        let x = qfixed_from_real(pos.x);
        for (offset, item) in items.visual() {
            if let ItemContent::Glyphs { engines, glyphs } = &item.content {
                self.draw_glyph_run(PointF::new(x + offset, pos.y), glyphs, engines, font);
            }
        }
    }

    /// `QPainter::layoutDirection`.
    pub fn layout_direction(&self) -> LayoutDirection {
        self.state.layout_direction
    }

    /// `QPainter::setLayoutDirection`: the base direction of the text `draw_text` draws.
    pub fn set_layout_direction(&mut self, direction: LayoutDirection) {
        self.state.layout_direction = direction;
    }

    /// Draws glyphs already shaped and placed relative to `pos` (the baseline origin), in the
    /// pen colour: the glyph half of [`draw_text`](Self::draw_text). `engines` is the list the
    /// glyphs' `font_index` points into. Callers that shaped a whole paragraph once (see
    /// `TextLayout`) draw its pieces through this so ligatures and joined forms survive.
    pub(crate) fn draw_glyph_run(
        &mut self,
        pos: PointF,
        glyphs: &[PositionedGlyph],
        engines: &[FontEngine],
        font: &Font,
    ) {
        if glyphs.is_empty() || engines.is_empty() {
            return;
        }

        // Under rotation, scale or skew the translate-only glyph path below cannot place the
        // glyphs, but Qt draws the transformed glyphs (qpaintengine_raster.cpp:3094-3128). Fill the
        // glyph outlines with the pen colour instead; this is not pixel-identical to Qt's glyph cache.
        if !self.text_transform_is_translate_only() {
            if let Some(path) = Self::glyph_path(pos, glyphs, engines, font) {
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
        let a8_lut = gamma_lut(1.7);
        let dpr = self.device.device_pixel_ratio();
        let transform = self.state.transform;
        // A rotated clip is not a pixel box, so the glyph blits read its coverage from the mask.
        self.update_clip_mask_if_needed();
        let clip_cov = if self.state.clip_quads.is_empty() {
            None
        } else {
            self.cached_mask
                .as_ref()
                .map(|m| (m.data(), m.width() as usize))
        };
        let mut pixmap = self.device.as_pixmap_mut();
        let pw = pixmap.width();
        let ph = pixmap.height();
        let pix_data = pixmap.data_mut();

        for glyph in glyphs.iter().copied() {
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
                let start_x = glyph_pixel(px + color_metrics.xmin as f32);
                let start_y =
                    glyph_pixel(py - color_metrics.ymin as f32 - color_metrics.height as f32);
                let c_w = color_metrics.width;
                let c_h = color_metrics.height;
                let color_data = color_pixmap.data();

                if self.state.composition_mode != CompositionMode::SourceOver {
                    // Non-source-over: composite through the active mode, weighted by the
                    // clip coverage and the glyph's own alpha.
                    let cov = &mut self.glyph_cov;
                    cov.clear();
                    cov.resize(c_w * c_h, 0);
                    for gy in 0..c_h {
                        for gx in 0..c_w {
                            if color_data[(gy * c_w + gx) * 4 + 3] != 0 {
                                cov[gy * c_w + gx] = glyph_clip_coverage(
                                    self.state.clip_rect,
                                    dpr,
                                    clip_cov,
                                    (pw, ph),
                                    (start_x + gx as i32, start_y + gy as i32),
                                );
                            }
                        }
                    }
                    composite_masked(
                        pix_data,
                        (pw, ph),
                        (start_x, start_y),
                        (c_w as u32, c_h as u32),
                        cov,
                        MaskedSource::Image(color_data, self.state.opacity.clamp(0.0, 1.0)),
                        self.state.composition_mode.into(),
                        &mut self.blit_scratch,
                    );
                    continue;
                }

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

            // An engine with horizontal sub-pixel positions (Qt's CoreText engine, and its
            // DirectWrite engine at a device pixel ratio other than 1) draws the glyph at the
            // fractional position of its pen, kept in 26.6 pixels (`QFixed::fromReal`), and blits
            // it at the floor of that position (`QRasterPaintEngine::drawCachedGlyphs`). Qt's
            // Windows engines without sub-pixel positions blit at that floor too.
            let pen_x = (f64::from(px) * 64.0).trunc() as i64;
            // Qt draws LCD masks when ClearType is on and the face has them
            // (`QRasterPaintEngine::drawCachedGlyphs`, `alphaRGBMapForGlyph`).
            let lcd_glyph = lcd_lut
                .as_ref()
                .and_then(|_| engine.rasterize_lcd_glyph_at(glyph.glyph_id, font.size, dpr, pen_x));
            // A face of Qt's FreeType engine is placed at the glyph's 26.6 position as
            // `QFontEngine::getGlyphPositions` computes it: the translation truncated to 26.6, plus
            // the laid-out advances, unless the transform scales.
            let placement = engine
                .face
                .freetype_engine()
                .then_some(())
                .filter(|_| lcd_glyph.is_none())
                .map(|()| {
                    let (x, y) = if transform.sx == 1.0
                        && transform.sy == 1.0
                        && transform.kx == 0.0
                        && transform.ky == 0.0
                    {
                        // The laid-out positions are 26.6 values already.
                        (
                            qt_fixed(f64::from(transform.tx) + f64::from(pos.x))
                                .saturating_add((f64::from(glyph.x) * 64.0).round() as i64),
                            qt_fixed(f64::from(transform.ty) + f64::from(pos.y))
                                .saturating_add((f64::from(glyph.y) * 64.0).round() as i64),
                        )
                    } else {
                        (qt_fixed(f64::from(px)), qt_fixed(f64::from(py)))
                    };
                    qt_glyph_placement(x, y)
                });
            let pen_x = placement.map_or(pen_x, |p| p.pen_x);
            let subpixel_glyph =
                lcd_glyph.is_none() && engine.supports_horizontal_subpixel_positions();
            let (metrics, bitmap) = match &lcd_glyph {
                Some((metrics, bitmap)) => (*metrics, bitmap.clone()),
                None if subpixel_glyph => {
                    engine.rasterize_glyph_at(glyph.glyph_id, font.size, dpr, pen_x)
                }
                None => engine.rasterize_glyph(glyph.glyph_id, font.size, dpr),
            };
            if metrics.width == 0 || metrics.height == 0 {
                continue;
            }

            let (start_x, start_y) = match placement {
                Some(p) => (
                    p.x.saturating_add(metrics.xmin),
                    p.y.saturating_sub(metrics.ymin.saturating_add(metrics.height as i32)),
                ),
                None if subpixel_glyph || cfg!(windows) => (
                    (pen_x >> 6).clamp(-1_000_000_000, 1_000_000_000) as i32 + metrics.xmin,
                    glyph_pixel(py - metrics.ymin as f32 - metrics.height as f32),
                ),
                None => (
                    glyph_pixel(px + metrics.xmin as f32),
                    glyph_pixel(py - metrics.ymin as f32 - metrics.height as f32),
                ),
            };

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

            if self.state.composition_mode != CompositionMode::SourceOver {
                // Qt's mask blits composite the pen colour through the composition mode; an LCD
                // mask is reduced to one coverage as for a non-opaque destination.
                let cov = &mut self.glyph_cov;
                cov.clear();
                cov.resize(metrics.width * metrics.height, 0);
                for gy in 0..metrics.height {
                    for gx in 0..metrics.width {
                        let glyph_cov = match (&lcd_glyph, &lcd_lut) {
                            (Some(_), Some(_)) => {
                                let i = (gy * metrics.width + gx) * 3;
                                rgb_average([bitmap[i], bitmap[i + 1], bitmap[i + 2]])
                            }
                            _ => bitmap[gy * metrics.width + gx],
                        };
                        if glyph_cov != 0 {
                            let clip = glyph_clip_coverage(
                                self.state.clip_rect,
                                dpr,
                                clip_cov,
                                (pw, ph),
                                (start_x + gx as i32, start_y + gy as i32),
                            );
                            cov[gy * metrics.width + gx] =
                                (glyph_cov as u32 * clip as u32 / 255) as u8;
                        }
                    }
                }
                let mut color = text_color;
                color.set_alpha(base_alpha);
                composite_masked(
                    pix_data,
                    (pw, ph),
                    (start_x, start_y),
                    (metrics.width as u32, metrics.height as u32),
                    cov,
                    MaskedSource::Solid(color),
                    self.state.composition_mode.into(),
                    &mut self.blit_scratch,
                );
                continue;
            }

            // `QFontEngine::expectsGammaCorrectedBlending` (the text gamma of `colorProfileForA8Text`
            // is 1.7 where the platform integration does not say otherwise).
            let a8_gamma = lcd_glyph.is_none() && engine.face.gamma_corrected_blending();
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
                                if a8_gamma {
                                    blend_grey_gamma(px, glyph_alpha, base_alpha, target, &a8_lut);
                                } else {
                                    blend_grey(px, glyph_alpha, base_alpha, target);
                                }
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
        // `QPainterPath::addText` lays the text out with a `QTextLayout` (a default `QTextOption`)
        // and puts the items from `point.x()` in visual order, as `drawText` does.
        let items = LineItems::new(text, font, &TextOption::default());
        let mut builder = PathBuilder::new();
        for (x, item) in items.visual() {
            if let ItemContent::Glyphs { engines, glyphs } = &item.content
                && let Some(path) =
                    Self::glyph_path(PointF::new(pos.x + x, pos.y), glyphs, engines, font)
            {
                builder.push_path(&path);
            }
        }
        builder.finish()
    }

    /// The outline of shaped `glyphs` placed relative to `pos`.
    pub(super) fn glyph_path(
        pos: PointF,
        glyphs: &[PositionedGlyph],
        engines: &[FontEngine],
        font: &Font,
    ) -> Option<Path> {
        let mut builder = tiny_skia::PathBuilder::new();
        for glyph in glyphs {
            let engine = engines
                .get(glyph.font_index as usize)
                .unwrap_or(&engines[0]);
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

    /// [`draw_glyph_run`](Self::draw_glyph_run) in a specific colour.
    pub(crate) fn draw_glyph_run_colored(
        &mut self,
        pos: PointF,
        glyphs: &[PositionedGlyph],
        engines: &[FontEngine],
        font: &Font,
        color: Color,
    ) {
        let old_pen = self.state.pen.clone();
        self.set_pen(Pen::new(color, 1.0));
        self.draw_glyph_run(pos, glyphs, engines, font);
        self.state.pen = old_pen;
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
