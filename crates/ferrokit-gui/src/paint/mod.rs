// SPDX-License-Identifier: LGPL-3.0-only
//! 2D painting subsystem.
//!
//! Provides canvas abstractions (`PaintDevice`), raster image backends (`Image`),
//! painter paths, brushes/gradients, composition modes, palettes, and vector painter.
pub mod backing_store;
pub mod brush;
pub mod composition;
pub mod paint_device;
pub mod painter;
pub mod palette;
pub mod path;
pub mod pixmap;
mod qpath_geom;
pub mod qss_box;
mod solid_fill;
pub mod stroker;
mod text_blend;

pub use backing_store::BackingStore;
pub use brush::*;
pub use composition::*;
pub use paint_device::PaintDevice;
pub use painter::{Painter, PainterState, Pen, RenderHint, RenderHints};
pub use palette::*;
pub use path::*;
pub use pixmap::Pixmap;
pub use stroker::{PainterPathStroker, PenCapStyle, PenJoinStyle, PenStyle};
