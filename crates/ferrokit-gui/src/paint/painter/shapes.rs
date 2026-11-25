// SPDX-License-Identifier: LGPL-3.0-only
use super::*;

impl<'a> Painter<'a> {
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
    pub(super) fn fill_aliased_line_end_pixels(&mut self, path: &Path, p1: PointF, p2: PointF) {
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
        let mut pixmap = self.device.as_pixmap_mut();
        let (pw, ph) = (pixmap.width() as f32, pixmap.height() as f32);
        for d in [d1, d2] {
            // A non-finite or off-pixmap endpoint has no pixel to fill; checking before the
            // `as i32` casts keeps huge coordinates from saturating or rasterising a giant box.
            if !(d.x >= 0.0 && d.x < pw && d.y >= 0.0 && d.y < ph) {
                continue;
            }
            let (px, py) = (d.x.floor() as i32, d.y.floor() as i32);
            // Coverage of the stroke at this pixel, from a 3x3 window centred on it.
            let Some(mut coverage) = Mask::new(3, 3) else {
                continue;
            };
            coverage.fill_path(
                &outline,
                tiny_skia::FillRule::Winding,
                false,
                transform.post_translate(-(px - 1) as f32, -(py - 1) as f32),
            );
            if coverage.data()[4] != 0 {
                continue;
            }
            if let Some(rect) = tiny_skia::Rect::from_xywh(px as f32, py as f32, 1.0, 1.0) {
                let path = PathBuilder::from_rect(rect);
                pixmap.fill_path(
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
        if let Some(r) = tiny_skia::Rect::from_xywh(rect.x, rect.y, rect.width, rect.height)
            && let Some(path) = PathBuilder::from_oval(r)
        {
            self.fill_path(&path);
            self.stroke_path(&path);
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

    /// `QPainter::drawPolygon(const QPolygonF &, Qt::FillRule)`: fills the polygon with the
    /// brush by `fill_rule` and outlines it, closed, with the pen. Fewer than two points draw
    /// nothing.
    pub fn draw_polygon(&mut self, polygon: &QPolygonF, fill_rule: FillRule) {
        self.draw_polygon_points(polygon.points(), fill_rule);
    }

    /// `QPainter::drawPolygon(const QPointF *, int, Qt::FillRule)`; see
    /// [`draw_polygon`](Self::draw_polygon).
    ///
    /// Qt's raster engine (`QRasterPaintEngine::drawPolygon`, qtbase v6.11.2) hands a polygon
    /// that is an axis-aligned rectangle (`QVectorPath::isRect`: four points, or five with the
    /// last repeating the first, running right, down, left from the top-left corner) to
    /// `drawRects`, and so does this. That shortcut is a property of Qt's raster paint engine
    /// (the OpenGL and other engines fill the polygon). Rendering is the one raster of this
    /// crate on every OS (tiny-skia); it was run on Windows only, and type-checked for macOS and
    /// Linux.
    pub fn draw_polygon_points(&mut self, points: &[PointF], fill_rule: FillRule) {
        if points.len() < 2 {
            return;
        }
        if is_rect_polygon(points) {
            self.draw_rect(RectF::new(
                points[0].x,
                points[0].y,
                points[2].x - points[0].x,
                points[2].y - points[0].y,
            ));
            return;
        }
        let mut pb = PathBuilder::new();
        pb.move_to(points[0].x, points[0].y);
        for p in &points[1..] {
            pb.line_to(p.x, p.y);
        }
        pb.close();
        if let Some(path) = pb.finish() {
            self.fill_path_with_rule(&path, fill_rule.into());
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
}

/// `QVectorPath::isRect` for a polygon's points (qtbase v6.11.2, `qvectorpath_p.h`).
pub(super) fn is_rect_polygon(pts: &[PointF]) -> bool {
    let n = pts.len();
    (n == 5 && pts[0] == pts[4] || n == 4)
        && pts[0].x == pts[3].x
        && pts[1].x == pts[2].x
        && pts[0].y == pts[1].y
        && pts[2].y == pts[3].y
        && pts[0].x < pts[2].x
        && pts[0].y < pts[2].y
}

/// Qt's `QPainterPath::arcTo` limits the sweep to one full turn. Returns `None` for a
/// negligible or non-finite sweep.
pub(super) fn clamped_span(span_deg: f32) -> Option<f32> {
    if !span_deg.is_finite() || span_deg.abs() < 1e-4 {
        return None;
    }
    Some(span_deg.clamp(-360.0, 360.0))
}

pub fn create_arc_path(rect: RectF, start_deg: f32, span_deg: f32) -> Option<Path> {
    let span_deg = clamped_span(span_deg)?;
    if rect.width <= 0.0 || rect.height <= 0.0 {
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
    let span_deg = clamped_span(span_deg)?;
    if rect.width <= 0.0 || rect.height <= 0.0 {
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
    let span_deg = clamped_span(span_deg)?;
    if rect.width <= 0.0 || rect.height <= 0.0 {
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
