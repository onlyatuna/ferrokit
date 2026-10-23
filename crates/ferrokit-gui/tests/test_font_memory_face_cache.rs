// SPDX-License-Identifier: LGPL-3.0-only
//! The in-memory face cache must not keep a font blob alive after every user of it is gone.

use ferrokit_gui::text::font::{Font, SharedFontData};
use ferrokit_gui::text::font_database::FontDatabase;
use std::path::Path;
use std::sync::Arc;

const FONT_FILES: [&str; 4] = [
    "C:/Windows/Fonts/segoeui.ttf",
    "/System/Library/Fonts/Supplemental/Arial.ttf",
    "/System/Library/Fonts/Geneva.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
];

#[test]
#[cfg_attr(miri, ignore = "reads a system font file and parses it")]
fn test_memory_face_cache_does_not_pin_font_data() {
    let path = FONT_FILES
        .iter()
        .map(Path::new)
        .find(|p| p.exists())
        .expect("one of the test font files must be installed");
    let bytes = Arc::new(std::fs::read(path).unwrap());
    let mut db = FontDatabase::new();
    {
        let font = Font::new("Anything", 14.0).with_font_data(SharedFontData::from_arc(bytes.clone()));
        let engines = db.resolve_font_engines(&font);
        assert!(!engines.is_empty());
        let again = db.resolve_font_engines(&font);
        assert!(!again.is_empty());
    }
    assert_eq!(
        Arc::strong_count(&bytes),
        1,
        "the database kept the font data alive after the font and its engines were dropped"
    );
}
