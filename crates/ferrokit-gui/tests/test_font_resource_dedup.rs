// SPDX-License-Identifier: LGPL-3.0-only
#[cfg(target_os = "windows")]
use ferrokit_gui::text::font::Font;
use ferrokit_gui::text::font::SharedFontData;
use ferrokit_gui::text::font_database::FontDatabase;
#[cfg(target_os = "windows")]
use ferrokit_gui::text::glyph_layout::{FontEngine, GlyphLayout};
#[cfg(target_os = "windows")]
use std::path::Path;

#[test]
fn test_shared_font_data_pointer_equality() {
    let raw = vec![0x00, 0x01, 0x00, 0x00, 0x00, 0x03];
    let shared1 = SharedFontData::from_vec(raw);
    let shared2 = shared1.clone();

    assert!(shared1.ptr_eq(&shared2), "Cloned SharedFontData must share exact physical buffer");
    assert_eq!(shared1.strong_count(), 2);
    assert_eq!(shared1.as_slice(), shared2.as_slice());
    assert_eq!(shared1.len(), 6);
    assert!(!shared1.is_empty());
}

#[cfg(target_os = "windows")]
#[test]
fn test_font_database_file_cache_deduplication() {
    let mut db = FontDatabase::new();

    // 1. Query Segoe UI or Arial from Windows Fonts
    let font_path = Path::new("C:/Windows/Fonts/segoeui.ttf");
    if font_path.exists() {
        let data1 = db.get_raw_font_data_by_path(font_path).expect("load font data 1");
        let data2 = db.get_raw_font_data_by_path(font_path).expect("load font data 2");

        assert!(
            data1.ptr_eq(&data2),
            "Subsequent requests for the same font file must return identical SharedFontData Arc"
        );
        assert!(db.cached_files_count() >= 1);
        assert!(db.total_raw_bytes() > 0);
    }

    // Test TTC collection deduplication if Microsoft YaHei or JhengHei is present
    let msyh_path = Path::new("C:/Windows/Fonts/msyh.ttc");
    if msyh_path.exists() {
        let initial_files = db.cached_files_count();
        let ttc_data1 = db.get_raw_font_data_by_path(msyh_path).expect("load msyh 1");
        let ttc_data2 = db.get_raw_font_data_by_path(msyh_path).expect("load msyh 2");

        assert!(
            ttc_data1.ptr_eq(&ttc_data2),
            "Multiple queries for TTC file must share the single in-memory buffer without duplicating tens of megabytes"
        );
        assert_eq!(
            db.cached_files_count(),
            initial_files + 1,
            "File cache count must not increase for duplicate TTC queries"
        );
    }
}

#[cfg(target_os = "windows")]
#[test]
fn test_font_engine_shaping_with_shared_data() {
    let font_path = Path::new("C:/Windows/Fonts/segoeui.ttf");
    if font_path.exists() {
        let mut db = FontDatabase::new();
        let raw_data = db.get_raw_font_data_by_path(font_path).expect("load raw data");
        let face = ferrokit_gui::text::glyph_face::parse_face(&raw_data, 0).expect("parse face");

        let engine = FontEngine::new(face)
            .with_raw_data(raw_data.clone())
            .with_face_index(0);

        let font = Font::new("Segoe UI", 14.0);
        let layout = GlyphLayout::shape_with_engines("Claude HUD Zero-Copy Text", &font, &[engine]);

        assert!(!layout.glyphs.is_empty(), "Glyphs must be shaped");
        assert!(layout.width > 0.0, "Shaped advance width must be non-zero");
    }
}

#[test]
fn test_font_database_cleanup_unused_fonts() {
    let mut db = FontDatabase::new();
    let initial_count = db.cached_files_count();

    // Verify cleanup does not crash and returns non-negative freed byte count
    let _freed = db.cleanup_unused_fonts();
    assert!(db.cached_files_count() <= initial_count);
}
