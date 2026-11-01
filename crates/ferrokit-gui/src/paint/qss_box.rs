// SPDX-License-Identifier: LGPL-3.0-only
//! Style-sheet box painting: a rule's background and solid border (`QRenderRule::drawBackground`,
//! `QRenderRule::drawBorder` -> `qDrawBorder`).
//!
//! Qt does not stroke the border with a pen centred on the rectangle edge. `qDrawBorder`
//! (`gui/painting/qcssutil.cpp:313-369`) fills each edge as a rectangle (or trapezoid) that lies inside
//! the border box, and draws the rounded corners as 45-degree arcs stroked with a square-capped pen of
//! the border width. The background of a rounded box is a path through the middle of the border
//! (`QRenderRule::borderClip`, `widgets/styles/qstylesheetstyle.cpp:1288-1324`), filled anti-aliased.
//!
//! Only what the style sheets here use is covered: a uniform width, a solid style, one colour for all four
//! edges and one circular radius for all four corners.

use crate::geometry::primitives::RectF;
use crate::paint::brush::Brush;
use crate::paint::painter::{Painter, Pen, RenderHint};
use tiny_skia::{Color, LineCap, PathBuilder};

/// Border description of a style-sheet box.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QssBorder {
    /// Width of all four edges, in logical pixels (`> 0`).
    pub width: f32,
    pub color: Color,
}

const KAPPA: f32 = 0.552_284_8;

/// `qNormalizeRadii` (`qcssutil.cpp:279-294`) for one radius on all corners: a radius whose two
/// corners do not fit along a side is dropped (not clamped).
fn normalize_radius(rect: RectF, radius: f32) -> f32 {
    let r = radius.max(0.0);
    if 2.0 * r > rect.width || 2.0 * r > rect.height {
        0.0
    } else {
        r
    }
}

/// Draws the background and border of a style-sheet box over `rect` (the border box).
///
/// `background` is `QRenderRule::drawBackground`, `border` is `QRenderRule::drawBorder`.
pub fn draw_qss_box(
    painter: &mut Painter,
    rect: RectF,
    background: Option<Color>,
    border: Option<QssBorder>,
    radius: f32,
) {
    let radius = normalize_radius(rect, radius);
    painter.save();
    if let Some(color) = background {
        draw_background(
            painter,
            rect,
            color,
            border.map_or(0.0, |b| b.width),
            radius,
        );
    }
    if let Some(border) = border
        && border.width > 0.0
    {
        draw_border(painter, rect, border, radius);
    }
    painter.restore();
}

/// `QRenderRule::drawBackground`: a rounded box is filled (anti-aliased) along the middle of the border,
/// a square one is a plain `fillRect` with the painter's current anti-aliasing (off by default).
fn draw_background(
    painter: &mut Painter,
    rect: RectF,
    color: Color,
    border_width: f32,
    radius: f32,
) {
    painter.set_pen(None);
    painter.set_brush(Brush::Color(color));
    if radius <= 0.0 {
        painter.set_render_hint(RenderHint::Antialiasing, false);
        painter.draw_rect(rect);
        return;
    }
    let half = border_width / 2.0;
    let arc = (radius - half).max(0.0);
    let k = KAPPA * arc;
    let (left, top, right, bottom) = (rect.x, rect.y, rect.x + rect.width, rect.y + rect.height);
    let mut pb = PathBuilder::new();
    pb.move_to(left + radius, top + half);
    pb.line_to(right - radius, top + half);
    let (cx, cy) = (right - radius, top + radius);
    pb.cubic_to(cx + k, cy - arc, cx + arc, cy - k, cx + arc, cy);
    pb.line_to(right - half, bottom - radius);
    let (cx, cy) = (right - radius, bottom - radius);
    pb.cubic_to(cx + arc, cy + k, cx + k, cy + arc, cx, cy + arc);
    pb.line_to(left + radius, bottom - half);
    let (cx, cy) = (left + radius, bottom - radius);
    pb.cubic_to(cx - k, cy + arc, cx - arc, cy + k, cx - arc, cy);
    pb.line_to(left + half, top + radius);
    let (cx, cy) = (left + radius, top + radius);
    pb.cubic_to(cx - arc, cy - k, cx - k, cy - arc, cx, cy - arc);
    pb.close();
    if let Some(path) = pb.finish() {
        painter.set_render_hint(RenderHint::Antialiasing, true);
        painter.fill_path(&path);
    }
}

#[derive(Clone, Copy)]
enum Edge {
    Top,
    Bottom,
    Left,
    Right,
}

/// `QRenderRule::drawBorder` -> `qDrawBorder` for a uniform solid border of one colour.
fn draw_border(painter: &mut Painter, rect: RectF, border: QssBorder, radius: f32) {
    let bw = border.width;
    let (left, top) = (rect.x, rect.y);
    let (right, bottom) = (rect.x + rect.width, rect.y + rect.height);
    // `paintsOver` (`qcssutil.cpp:297-311`) for two solid edges of the same colour: only an opaque colour
    // lets one edge draw over the other, otherwise the corner is split by a mitre (trapezoids).
    let opaque = border.color.alpha() >= 1.0;
    let dw = if radius > 0.0 || opaque { 0.0 } else { bw };
    painter.set_render_hint(RenderHint::Antialiasing, true);
    // Drawn in increasing order of precedence: bottom, right, left, top.
    draw_edge(
        painter,
        border.color,
        Edge::Bottom,
        left + radius,
        bottom - bw,
        right - radius,
        bottom,
        dw,
        dw,
    );
    draw_edge(
        painter,
        border.color,
        Edge::Right,
        right - bw,
        top + radius,
        right,
        bottom - radius,
        dw,
        dw,
    );
    draw_edge(
        painter,
        border.color,
        Edge::Left,
        left,
        top + radius,
        left + bw,
        bottom - radius,
        dw,
        dw,
    );
    draw_edge(
        painter,
        border.color,
        Edge::Top,
        left + radius,
        top,
        right - radius,
        top + bw,
        dw,
        dw,
    );
    if radius > 0.0 {
        let (r, pw) = (radius, bw);
        let h = pw / 2.0;
        let d = 2.0 * r - pw;
        let pen = Pen::new(border.color, pw).with_cap(LineCap::Square);
        painter.set_brush(Brush::Color(Color::TRANSPARENT));
        painter.set_pen(pen);
        let (x1, x2) = (left + radius, right - radius);
        let (y1, y2) = (top + radius, bottom - radius);
        let a = |painter: &mut Painter, x: f32, y: f32, start: f32, span: f32| {
            painter.draw_arc(RectF::new(x, y, d, d), start, span);
        };
        // qDrawRoundedCorners (`qcssutil.cpp:109-141`), `edge` x `r1`/`r2`.
        // Top
        a(painter, x1 - r + h, top + h, 135.0, -45.0);
        a(painter, x2 - r + h, top + h, 45.0, 45.0);
        // Bottom
        a(painter, x1 - r + h, bottom - 2.0 * r + h, -90.0, -45.0);
        a(painter, x2 - r + h, bottom - 2.0 * r + h, -90.0, 45.0);
        // Left
        a(painter, left + h, y1 - r + h, 135.0, 45.0);
        a(painter, left + h, y2 - r + h, 180.0, 45.0);
        // Right
        a(painter, right - 2.0 * r + h, y1 - r + h, 45.0, -45.0);
        a(painter, right - 2.0 * r + h, y2 - r + h, 315.0, 45.0);
    }
}

/// `qDrawEdge` (`qcssutil.cpp:149-277`), solid style.
#[allow(clippy::too_many_arguments)]
fn draw_edge(
    painter: &mut Painter,
    color: Color,
    edge: Edge,
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    dw1: f32,
    dw2: f32,
) {
    painter.set_pen(None);
    painter.set_brush(Brush::Color(color));
    let width = match edge {
        Edge::Top | Edge::Bottom => y2 - y1,
        Edge::Left | Edge::Right => x2 - x1,
    };
    if width == 1.0 || (dw1 == 0.0 && dw2 == 0.0) {
        painter.draw_rect(RectF::new(x1, y1, x2 - x1, y2 - y1));
        return;
    }
    let quad = match edge {
        Edge::Top => [(x1, y1), (x1 + dw1, y2), (x2 - dw2, y2), (x2, y1)],
        Edge::Bottom => [(x1 + dw1, y1), (x1, y2), (x2, y2), (x2 - dw2, y1)],
        Edge::Left => [(x1, y1), (x1, y2), (x2, y2 - dw2), (x2, y1 + dw1)],
        Edge::Right => [(x1, y1 + dw1), (x1, y2 - dw2), (x2, y2), (x2, y1)],
    };
    let mut pb = PathBuilder::new();
    pb.move_to(quad[0].0, quad[0].1);
    for p in &quad[1..] {
        pb.line_to(p.0, p.1);
    }
    pb.close();
    if let Some(path) = pb.finish() {
        painter.fill_path(&path);
    }
}
