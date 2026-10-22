// SPDX-License-Identifier: LGPL-3.0-only
//! Gradients and Brush styling (`QGradient` and `QBrush` equivalents).
//!
//! Provides linear and radial gradients, gradient stops, spread modes, and texture patterns.

use crate::geometry::primitives::PointF;
use crate::paint::pixmap::Pixmap;
use std::sync::Arc;
use tiny_skia::{Color, Shader, SpreadMode, Transform};

/// Color stop in a gradient (position in [0.0, 1.0] and associated color).
#[derive(Debug, Clone, PartialEq)]
pub struct GradientStop {
    pub position: f32,
    pub color: Color,
}

impl GradientStop {
    pub fn new(position: f32, color: Color) -> Self {
        Self {
            position: position.clamp(0.0, 1.0),
            color,
        }
    }

    pub fn from_rgba8(position: f32, r: u8, g: u8, b: u8, a: u8) -> Self {
        Self::new(position, Color::from_rgba8(r, g, b, a))
    }
}

/// Inserts a stop keeping `stops` sorted by position; a stop at an existing position replaces
/// its color (`QGradient::setColorAt`, qgradient.cpp).
fn insert_stop(stops: &mut Vec<GradientStop>, position: f32, color: Color) {
    let stop = GradientStop::new(position, color);
    let index = stops.partition_point(|s| s.position < stop.position);
    match stops.get_mut(index) {
        Some(existing) if existing.position == stop.position => existing.color = stop.color,
        _ => stops.insert(index, stop),
    }
}

/// Converts to tiny-skia stops, scaling every stop's alpha by the painter opacity.
fn to_skia_stops(stops: &[GradientStop], opacity: f32) -> Vec<tiny_skia::GradientStop> {
    stops
        .iter()
        .map(|s| {
            let mut color = s.color;
            color.apply_opacity(opacity);
            tiny_skia::GradientStop::new(s.position, color)
        })
        .collect()
}

/// Linear gradient definition (`QLinearGradient`).
#[derive(Debug, Clone, PartialEq)]
pub struct LinearGradient {
    pub start: PointF,
    pub end: PointF,
    pub stops: Vec<GradientStop>,
    pub spread: SpreadMode,
}

impl LinearGradient {
    pub fn new(start: PointF, end: PointF) -> Self {
        Self {
            start,
            end,
            stops: Vec::new(),
            spread: SpreadMode::Pad,
        }
    }

    pub fn add_stop(&mut self, position: f32, color: Color) {
        insert_stop(&mut self.stops, position, color);
    }

    pub fn with_stop(mut self, position: f32, color: Color) -> Self {
        self.add_stop(position, color);
        self
    }

    pub fn set_spread(&mut self, spread: SpreadMode) {
        self.spread = spread;
    }

    /// Converts to tiny_skia Shader.
    pub fn to_shader(&self, transform: Transform, opacity: f32) -> Option<Shader<'static>> {
        if self.stops.len() < 2 {
            return None;
        }
        let skia_stops = to_skia_stops(&self.stops, opacity);

        LinearGradientBuilder::build(
            tiny_skia::Point::from_xy(self.start.x, self.start.y),
            tiny_skia::Point::from_xy(self.end.x, self.end.y),
            skia_stops,
            self.spread,
            transform,
        )
    }
}

/// Helper to wrap tiny_skia::LinearGradient::new
struct LinearGradientBuilder;
impl LinearGradientBuilder {
    fn build(
        start: tiny_skia::Point,
        end: tiny_skia::Point,
        stops: Vec<tiny_skia::GradientStop>,
        mode: SpreadMode,
        transform: Transform,
    ) -> Option<Shader<'static>> {
        tiny_skia::LinearGradient::new(start, end, stops, mode, transform)
    }
}

/// Radial gradient definition (`QRadialGradient`).
#[derive(Debug, Clone, PartialEq)]
pub struct RadialGradient {
    pub center: PointF,
    pub radius: f32,
    pub stops: Vec<GradientStop>,
    pub spread: SpreadMode,
}

impl RadialGradient {
    pub fn new(center: PointF, radius: f32) -> Self {
        Self {
            center,
            radius,
            stops: Vec::new(),
            spread: SpreadMode::Pad,
        }
    }

    pub fn add_stop(&mut self, position: f32, color: Color) {
        insert_stop(&mut self.stops, position, color);
    }

    pub fn with_stop(mut self, position: f32, color: Color) -> Self {
        self.add_stop(position, color);
        self
    }

    pub fn set_spread(&mut self, spread: SpreadMode) {
        self.spread = spread;
    }

    /// Converts to tiny_skia Shader.
    pub fn to_shader(&self, transform: Transform, opacity: f32) -> Option<Shader<'static>> {
        if self.stops.len() < 2 || self.radius <= 0.0 {
            return None;
        }
        let skia_stops = to_skia_stops(&self.stops, opacity);

        let pt = tiny_skia::Point::from_xy(self.center.x, self.center.y);
        tiny_skia::RadialGradient::new(pt, pt, self.radius, skia_stops, self.spread, transform)
    }
}

/// Texture brush pattern (`QBrush` with `QPixmap` texture).
#[derive(Debug, Clone)]
pub struct TexturePattern {
    pub pixmap: Arc<Pixmap>,
    pub transform: Transform,
}

impl PartialEq for TexturePattern {
    fn eq(&self, other: &Self) -> bool {
        self.transform == other.transform && Arc::ptr_eq(&self.pixmap, &other.pixmap)
    }
}

/// Brush styling (`QBrush` equivalent) supporting solid colors, patterns, and gradients.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Brush {
    /// Transparent fill (Qt::NoBrush).
    #[default]
    NoBrush,
    /// Solid color fill (Qt::SolidPattern).
    Color(Color),
    /// 45-degree diagonal hatched pattern.
    Hatched { color: Color },
    /// Linear gradient fill.
    LinearGradient(LinearGradient),
    /// Radial gradient fill.
    RadialGradient(RadialGradient),
    /// Texture image fill.
    Texture(TexturePattern),
}

impl Brush {
    /// Creates a solid color brush.
    pub fn from_color(color: Color) -> Self {
        Brush::Color(color)
    }

    /// Creates an RGBA8 solid color brush.
    pub fn from_rgba8(r: u8, g: u8, b: u8, a: u8) -> Self {
        Brush::Color(Color::from_rgba8(r, g, b, a))
    }

    /// Creates a hatched pattern brush.
    pub fn hatched(color: Color) -> Self {
        Brush::Hatched { color }
    }

    /// Creates a linear gradient brush.
    pub fn linear_gradient(gradient: LinearGradient) -> Self {
        Brush::LinearGradient(gradient)
    }

    /// Creates a radial gradient brush.
    pub fn radial_gradient(gradient: RadialGradient) -> Self {
        Brush::RadialGradient(gradient)
    }

    /// Creates a texture brush from an Arc<Pixmap>.
    pub fn texture(pixmap: Arc<Pixmap>) -> Self {
        Brush::Texture(TexturePattern {
            pixmap,
            transform: Transform::identity(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn positions(stops: &[GradientStop]) -> Vec<f32> {
        stops.iter().map(|s| s.position).collect()
    }

    #[test]
    fn stops_are_kept_sorted_regardless_of_call_order() {
        let mut g = LinearGradient::new(PointF::new(0.0, 0.0), PointF::new(10.0, 0.0));
        g.add_stop(1.0, Color::from_rgba8(255, 0, 0, 255));
        g.add_stop(0.0, Color::from_rgba8(0, 0, 255, 255));
        g.add_stop(0.5, Color::from_rgba8(0, 255, 0, 255));
        assert_eq!(positions(&g.stops), vec![0.0, 0.5, 1.0]);
        assert_eq!(g.stops[0].color, Color::from_rgba8(0, 0, 255, 255));
        let mut r = RadialGradient::new(PointF::new(5.0, 5.0), 5.0);
        r.add_stop(1.0, Color::WHITE);
        r.add_stop(0.25, Color::BLACK);
        assert_eq!(positions(&r.stops), vec![0.25, 1.0]);
    }

    #[test]
    fn stop_at_existing_position_replaces_color() {
        let mut g = LinearGradient::new(PointF::new(0.0, 0.0), PointF::new(10.0, 0.0));
        g.add_stop(0.5, Color::BLACK);
        g.add_stop(0.5, Color::WHITE);
        assert_eq!(g.stops.len(), 1);
        assert_eq!(g.stops[0].color, Color::WHITE);
    }

    #[test]
    fn opacity_scales_stop_alpha() {
        let mut c = Color::from_rgba8(255, 0, 0, 255);
        c.apply_opacity(0.5);
        assert!((c.alpha() - 0.5).abs() < 1e-6);
        let mut g = LinearGradient::new(PointF::new(0.0, 0.0), PointF::new(10.0, 0.0));
        g.add_stop(0.0, c);
        g.add_stop(1.0, c);
        assert!(g.to_shader(Transform::identity(), 0.5).is_some());
    }
}
