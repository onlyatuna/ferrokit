// SPDX-License-Identifier: LGPL-3.0-only
//! Composition Modes for rendering (`QPainter::CompositionMode` equivalent).
//!
//! Provides Porter-Duff and advanced blend modes compatible with Qt and SVG compositing specs.

use tiny_skia::BlendMode;

/// Composition mode specifying how source pixels are blended with destination pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompositionMode {
    #[default]
    SourceOver,
    DestinationOver,
    Clear,
    Source,
    Destination,
    SourceIn,
    DestinationIn,
    SourceOut,
    DestinationOut,
    SourceAtop,
    DestinationAtop,
    Xor,
    Plus,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    HardLight,
    SoftLight,
    Difference,
    Exclusion,
}

impl From<CompositionMode> for BlendMode {
    fn from(mode: CompositionMode) -> Self {
        match mode {
            CompositionMode::SourceOver => BlendMode::SourceOver,
            CompositionMode::DestinationOver => BlendMode::DestinationOver,
            CompositionMode::Clear => BlendMode::Clear,
            CompositionMode::Source => BlendMode::Source,
            CompositionMode::Destination => BlendMode::Destination,
            CompositionMode::SourceIn => BlendMode::SourceIn,
            CompositionMode::DestinationIn => BlendMode::DestinationIn,
            CompositionMode::SourceOut => BlendMode::SourceOut,
            CompositionMode::DestinationOut => BlendMode::DestinationOut,
            CompositionMode::SourceAtop => BlendMode::SourceAtop,
            CompositionMode::DestinationAtop => BlendMode::DestinationAtop,
            CompositionMode::Xor => BlendMode::Xor,
            CompositionMode::Plus => BlendMode::Plus,
            CompositionMode::Multiply => BlendMode::Multiply,
            CompositionMode::Screen => BlendMode::Screen,
            CompositionMode::Overlay => BlendMode::Overlay,
            CompositionMode::Darken => BlendMode::Darken,
            CompositionMode::Lighten => BlendMode::Lighten,
            CompositionMode::ColorDodge => BlendMode::ColorDodge,
            CompositionMode::ColorBurn => BlendMode::ColorBurn,
            CompositionMode::HardLight => BlendMode::HardLight,
            CompositionMode::SoftLight => BlendMode::SoftLight,
            CompositionMode::Difference => BlendMode::Difference,
            CompositionMode::Exclusion => BlendMode::Exclusion,
        }
    }
}
