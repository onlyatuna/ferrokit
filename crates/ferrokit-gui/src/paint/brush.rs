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
        self.stops.push(GradientStop::new(position, color));
    }

    pub fn with_stop(mut self, position: f32, color: Color) -> Self {
        self.add_stop(position, color);
        self
    }

    pub fn set_spread(&mut self, spread: SpreadMode) {
        self.spread = spread;
    }

    /// Converts to tiny_skia Shader.
    pub fn to_shader(&self, transform: Transform) -> Option<Shader<'static>> {
        if self.stops.len() < 2 {
            return None;
        }
        let skia_stops: Vec<tiny_skia::GradientStop> = self
            .stops
            .iter()
            .map(|s| tiny_skia::GradientStop::new(s.position, s.color))
            .collect();

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
        self.stops.push(GradientStop::new(position, color));
    }

    pub fn with_stop(mut self, position: f32, color: Color) -> Self {
        self.add_stop(position, color);
        self
    }

    pub fn set_spread(&mut self, spread: SpreadMode) {
        self.spread = spread;
    }

    /// Converts to tiny_skia Shader.
    pub fn to_shader(&self, transform: Transform) -> Option<Shader<'static>> {
        if self.stops.len() < 2 || self.radius <= 0.0 {
            return None;
        }
        let skia_stops: Vec<tiny_skia::GradientStop> = self
            .stops
            .iter()
            .map(|s| tiny_skia::GradientStop::new(s.position, s.color))
            .collect();

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
