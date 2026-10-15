// SPDX-License-Identifier: LGPL-3.0-only
pub mod line;
pub mod math3d;
pub mod polygon;
pub mod primitives;
pub mod region;
pub mod transform;

pub use line::*;
pub use math3d::*;
pub use polygon::*;
pub use primitives::*;
pub use region::*;
pub use transform::*;

// --- Qt Canonical Aliases ---
pub type QLine = line::Line;
pub type QLineF = line::LineF;
pub type QPolygon = polygon::Polygon;
pub type QPolygonF = polygon::PolygonF;
pub type QRegion = region::Region;
