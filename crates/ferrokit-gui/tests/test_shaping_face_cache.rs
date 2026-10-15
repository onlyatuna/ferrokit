// SPDX-License-Identifier: LGPL-3.0-only
//! The HarfBuzz face of a font engine is built once and reused for every string it shapes.
//!
//! Qt keeps the `hb_face_t` on the `QFontEngine` (`hb_qt_face_get_for_engine`,
//! gui/text/qharfbuzzng.cpp:668-676) and the `hb_font_t` likewise (709), so shaping a
//! string does not parse the font's GSUB/GPOS tables again.

use ferrokit_gui::text::font::Font;
use ferrokit_gui::text::font_database::{resolve_font_engines_for_text_global, FontDatabase};
use ferrokit_gui::text::glyph_layout::{shaping_face_parse_count, FontEngine, GlyphLayout};
use std::path::Path;

/// A font file with OpenType tables on each CI platform.
const FONT_FILES: [&str; 4] = [
    "C:/Windows/Fonts/segoeui.ttf",
    "/System/Library/Fonts/Supplemental/Arial.ttf",
    "/System/Library/Fonts/Geneva.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
];

fn engine_from_file() -> FontEngine {
    let path = FONT_FILES
        .iter()
        .map(Path::new)
        .find(|p| p.exists())
        .expect("one of the test font files must be installed");
    let mut db = FontDatabase::new();
    let data = db.get_raw_font_data_by_path(path).expect("font data");
    let face = ferrokit_gui::text::glyph_face::parse_face(&data, 0).expect("font face");
    FontEngine::new(face).with_raw_data(data).with_face_index(0)
}

/// Shapes three strings after a warm-up and returns how many faces that built.
fn rebuilds(font: &Font, engines: impl Fn(&str) -> Vec<FontEngine>) -> usize {
    let warm = GlyphLayout::shape_with_engines("Claude HUD 42%", font, &engines("Claude HUD 42%"));
    let before = shaping_face_parse_count();
    let again = GlyphLayout::shape_with_engines("Claude HUD 42%", font, &engines("Claude HUD 42%"));
    GlyphLayout::shape_with_engines("Reset in 3h", font, &engines("Reset in 3h"));
    GlyphLayout::shape_with_engines("Claude HUD 42%", font, &engines("Claude HUD 42%"));
    assert_eq!(
        again.width, warm.width,
        "the cached face shapes the same way"
    );
    assert_eq!(again.glyphs, warm.glyphs);
    shaping_face_parse_count() - before
}

#[test]
fn test_shaping_with_an_engine_again_reuses_its_face() {
    // `draw_text` gets a clone of the database's engine on every call; clones share the face.
    let engine = engine_from_file();
    let font = Font::new("Segoe UI", 14.0);
    assert_eq!(
        rebuilds(&font, |_| vec![engine.clone()]),
        0,
        "shaping three more strings with the same engine rebuilt its face"
    );
}

#[test]
fn test_engines_resolved_from_the_font_database_keep_their_face() {
    let font = Font::new("Segoe UI", 13.0);
    let resolved = resolve_font_engines_for_text_global(&font, "Claude HUD 42%");
    if !resolved.iter().any(|e| e.raw_data.is_some()) {
        // macOS resolves system fonts without OpenType data; nothing is shaped by HarfBuzz there.
        return;
    }
    assert_eq!(
        rebuilds(&font, |text| resolve_font_engines_for_text_global(
            &font, text
        )),
        0,
        "each draw_text resolves the engines again; that must not rebuild their faces"
    );
}
