// SPDX-License-Identifier: LGPL-3.0-only
use ferrokit_gui::text::font::Font;
use ferrokit_gui::text::font_database::resolve_font_engines_global;
use ferrokit_gui::text::font_metrics::FontMetrics;
use ferrokit_gui::text::glyph_layout::GlyphLayout;

#[test]
fn test_font_fallback_cjk_metrics_not_collapsed() {
    let font = Font::new("Segoe UI", 16.0);
    let metrics = FontMetrics::from_font(&font);

    let text_ascii = "5 ";
    let text_cjk = "5 小時";

    let advance_ascii = metrics.horizontal_advance_exact(text_ascii, &font);
    let advance_cjk = metrics.horizontal_advance_exact(text_cjk, &font);

    assert!(advance_ascii > 0.0, "ASCII text must have non-zero advance");
    // "5 " is ~12-16px. "5 小時" contains two full-width CJK ideographs (~16px each),
    // so total width must be substantially greater (at least ~35px) and not collapsed to ~14px.
    assert!(
        advance_cjk > advance_ascii * 2.0,
        "CJK characters in Segoe UI must resolve through fallback chain and not collapse to notdef: ascii={}, cjk={}",
        advance_ascii,
        advance_cjk
    );
}

#[test]
fn test_metrics_and_painter_shaping_exact_parity() {
    let font = Font::new("Segoe UI", 14.0);
    let metrics = FontMetrics::from_font(&font);

    let test_strings = [
        "Claude HUD Monitor",
        "5 小時",
        "重設",
        "剩餘",
        "Token: 12,450 (剩餘 85%)",
        "狀態: 🔄 運行中",
    ];

    let engines = resolve_font_engines_global(&font);
    assert!(
        !engines.is_empty(),
        "Font engines must resolve at least primary font"
    );

    for s in test_strings {
        let metrics_width = metrics.horizontal_advance_exact(s, &font);
        let layout = GlyphLayout::shape_with_engines(s, &font, &engines);

        // Verification of 100% parity between FontMetrics measurement and Painter layout width
        assert!(
            (metrics_width - layout.width).abs() < 1e-4,
            "Parity violation for string '{}': metrics={} vs layout={}",
            s,
            metrics_width,
            layout.width
        );
    }
}

#[test]
fn test_memory_font_retains_fallback_chain() {
    // Test with custom in-memory font buffer
    let mut font = Font::new("CustomIconFont", 14.0);
    font.font_data = Some(ferrokit_gui::text::font::SharedFontData::from_vec(vec![
        0u8;
        128
    ]));

    let engines = resolve_font_engines_global(&font);
    // On systems with CJK/Emoji system fonts, engines should contain fallback entries
    assert!(!engines.is_empty(), "Resolved engines must not be empty");
}

#[test]
fn test_fallback_chain_is_loaded_only_for_text_the_primary_cannot_cover() {
    use ferrokit_gui::text::font_database::resolve_font_engines_for_text_global;
    let font = Font::new("Segoe UI", 14.0);
    let full = resolve_font_engines_global(&font);
    if full.len() < 2 {
        return; // no system fallback fonts on this machine
    }

    let ascii = resolve_font_engines_for_text_global(&font, "Claude HUD 42% --");
    assert_eq!(
        ascii.len(),
        1,
        "ASCII text must not pull in CJK/emoji fallbacks"
    );

    if full.len() >= 3 {
        // CJK-only text stops after the CJK slot; the emoji font is not parsed for it.
        let cjk_only = resolve_font_engines_for_text_global(&font, "狀態");
        assert_eq!(cjk_only.len(), 2, "CJK text must not load the emoji slot");
    }

    let cjk = resolve_font_engines_for_text_global(&font, "狀態: 🔄");
    assert_eq!(
        cjk.len(),
        full.len(),
        "uncovered characters get the full fallback chain"
    );

    // Same shaping result as with the full chain, for covered and uncovered text alike.
    for s in ["Claude HUD 42%", "5 小時 🔄"] {
        let lazy = resolve_font_engines_for_text_global(&font, s);
        let a = GlyphLayout::shape_with_engines(s, &font, &lazy);
        let b = GlyphLayout::shape_with_engines(s, &font, &full);
        assert!(
            (a.width - b.width).abs() < 1e-4,
            "{s}: {} vs {}",
            a.width,
            b.width
        );
    }
}
