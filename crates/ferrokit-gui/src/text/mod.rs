// SPDX-License-Identifier: LGPL-3.0-only
mod bidi;
pub mod block;
pub mod block_data;
#[cfg(all(unix, not(target_os = "macos")))]
pub(crate) mod box_face;
pub mod color_glyph;
#[cfg(target_os = "macos")]
pub mod core_text_face;
pub mod cursor;
#[cfg(windows)]
pub(crate) mod directwrite_catalog;
#[cfg(windows)]
pub mod directwrite_face;
pub mod document;
pub mod document_fragment;
pub mod font;
pub mod font_database;
pub mod font_metrics;
#[cfg(all(unix, not(target_os = "macos")))]
pub(crate) mod fontconfig_catalog;
#[cfg(all(unix, not(target_os = "macos")))]
pub(crate) mod fontconfig_ffi;
pub mod format;
pub mod fragment;
pub mod ftgrays;
pub mod glyph_face;
pub mod glyph_layout;
pub mod highlighter;
pub mod layout;
mod line;
mod line_break;
pub(crate) mod line_items;
mod markup;
pub use markup::might_be_rich_text;
pub mod outline_face;
pub mod position;
pub mod qcssparser;
mod shaping_plan;
pub mod skrifa_face;
pub mod smoothing;

pub use block::*;
pub use block_data::*;
pub use color_glyph::*;
pub use cursor::*;
pub use document::*;
pub use document_fragment::*;
pub use font::*;
pub use font_database::*;
pub use font_metrics::*;
pub use format::*;
pub use fragment::*;
pub use glyph_face::*;
pub use glyph_layout::*;
pub use highlighter::*;
pub use layout::*;
pub use line::{CursorPosition, Edge, QTextLine, TextLine};
pub use line_items::TextOption;
pub use position::*;
pub use qcssparser::*;
pub use unicode_segmentation::UnicodeSegmentation;
