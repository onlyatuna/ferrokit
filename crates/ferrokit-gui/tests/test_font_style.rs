// SPDX-License-Identifier: LGPL-3.0-only
//! Weight/italic face selection (`QFontDatabase::bestStyle`) and `QFont` absolute letter spacing.
#![cfg(windows)]

use ferrokit_gui::text::font::{Font, FontStyle, FontWeight};
use ferrokit_gui::text::font_database::resolve_font_engines_for_text_global;
use ferrokit_gui::text::glyph_layout::{FontEngine, GlyphLayout};

fn primary(font: &Font, text: &str) -> FontEngine {
    let engines = resolve_font_engines_for_text_global(font, text);
    assert!(!engines.is_empty(), "{} must be installed", font.family);
    engines[0].clone()
}

fn file_len(engine: &FontEngine) -> usize {
    engine.raw_data.as_ref().expect("raw font data").as_slice().len()
}

fn width(font: &Font, text: &str) -> f32 {
    let engines = resolve_font_engines_for_text_global(font, text);
    GlyphLayout::shape_with_engines(text, font, &engines).width
}

#[test]
fn bold_weights_load_the_bold_face_and_lighter_weights_the_regular_one() {
    let regular = Font::new("Segoe UI", 16.0);
    let regular_len = file_len(&primary(&regular, "SESSION"));
    let bold_len = file_len(&primary(&regular.clone().with_weight(FontWeight::Bold), "SESSION"));
    assert_ne!(regular_len, bold_len, "Bold must not be served from the regular file");

    // Qt picks the nearest of the faces a family has: Segoe UI ships regular and bold only.
    for (weight, want_bold) in [
        (FontWeight::Light, false),
        (FontWeight::Medium, false),
        (FontWeight::SemiBold, true),
        (FontWeight::Black, true),
    ] {
        let len = file_len(&primary(&regular.clone().with_weight(weight), "SESSION"));
        assert_eq!(len == bold_len, want_bold, "{weight:?}");
    }

    // Bold glyphs are wider, so layout and painting see a different advance.
    let bold = regular.clone().with_weight(FontWeight::Bold);
    assert!(width(&bold, "SESSION 5H") > width(&regular, "SESSION 5H"));
}

#[test]
fn italic_loads_the_italic_face() {
    let regular = Font::new("Segoe UI", 16.0);
    let italic = regular.clone().with_style(FontStyle::Italic);
    assert_ne!(file_len(&primary(&regular, "abc")), file_len(&primary(&italic, "abc")));
}

#[test]
fn bold_text_falls_back_to_the_bold_cjk_face() {
    // The glyph chain keeps the weight: a CJK character in bold text comes from msjhbd.ttc.
    let bold = Font::new("Segoe UI", 16.0).with_weight(FontWeight::Bold);
    let regular = Font::new("Segoe UI", 16.0);
    let cjk = "重設於";
    let bold_engines = resolve_font_engines_for_text_global(&bold, cjk);
    let regular_engines = resolve_font_engines_for_text_global(&regular, cjk);
    assert!(bold_engines.len() >= 2 && regular_engines.len() >= 2, "CJK fallback must resolve");
    assert_ne!(file_len(&bold_engines[1]), file_len(&regular_engines[1]));
}

#[test]
fn family_without_a_bold_face_yields_its_regular_face() {
    // Segoe UI Emoji has a single face; asking for bold must still give text, not nothing.
    let bold = Font::new("Segoe UI Emoji", 16.0).with_weight(FontWeight::Bold);
    let regular = Font::new("Segoe UI Emoji", 16.0);
    assert_eq!(file_len(&primary(&bold, "😀")), file_len(&primary(&regular, "😀")));
}

#[test]
fn letter_spacing_is_added_after_every_glyph_including_the_last() {
    let plain = Font::new("Segoe UI", 14.0);
    let spaced = plain.clone().with_letter_spacing(1.5);
    let text = "SESSION 5H";
    let n = text.chars().count() as f32;
    assert!((width(&spaced, text) - (width(&plain, text) + 1.5 * n)).abs() < 1e-3);

    let engines = resolve_font_engines_for_text_global(&spaced, text);
    let a = GlyphLayout::shape_with_engines(text, &plain, &engines);
    let b = GlyphLayout::shape_with_engines(text, &spaced, &engines);
    for (i, (ga, gb)) in a.glyphs.iter().zip(&b.glyphs).enumerate() {
        assert!((gb.x - (ga.x + 1.5 * i as f32)).abs() < 1e-3, "glyph {i} offset");
    }
}
