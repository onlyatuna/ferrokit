// SPDX-License-Identifier: LGPL-3.0-only
use ferrokit_gui::geometry::primitives::PointF;
use ferrokit_gui::paint::{Painter, Pen, Pixmap};
use ferrokit_gui::text::font::Font;
use ferrokit_gui::text::font_database::resolve_font_engines_for_text_global;
use ferrokit_gui::text::glyph_layout::GlyphLayout;
use ferrokit_gui::tiny_skia::Color;
use std::sync::Arc;

fn first_glyph(text: &str, font: &Font) -> (ferrokit_gui::text::glyph_layout::FontEngine, u16) {
    let engines = resolve_font_engines_for_text_global(font, text);
    assert!(!engines.is_empty(), "test font must be installed");
    let layout = GlyphLayout::shape_with_engines(text, font, &engines);
    let g = layout
        .glyphs
        .iter()
        .find(|g| g.glyph_id != 0)
        .expect("a glyph");
    (engines[g.font_index as usize].clone(), g.glyph_id)
}

#[test]
fn test_cached_glyph_bitmap_is_identical_to_direct_rasterization_and_reused() {
    let font = Font::new("Segoe UI", 13.0);
    let (engine, gid) = first_glyph("Wg", &font);
    let px = 16.25;

    let (direct_m, direct_bitmap) = engine.face.rasterize_indexed(gid, px);
    let (m1, b1) = engine.rasterize_glyph(gid, px, 1.0);
    assert_eq!(
        (m1.width, m1.height, m1.xmin, m1.ymin),
        (
            direct_m.width,
            direct_m.height,
            direct_m.xmin,
            direct_m.ymin
        )
    );
    assert_eq!(
        &*b1,
        direct_bitmap.as_slice(),
        "cached bitmap must equal the face's own output"
    );

    let (_, b2) = engine.rasterize_glyph(gid, px, 1.0);
    assert!(
        Arc::ptr_eq(&b1, &b2),
        "second request must reuse the cached bitmap"
    );

    // A size that differs only below 1/64 px is still a different raster: no key collisions.
    let (_, b3) = engine.rasterize_glyph(gid, px + 0.001, 1.0);
    assert!(!Arc::ptr_eq(&b1, &b3));
    assert_eq!(
        &*b3,
        engine.face.rasterize_indexed(gid, px + 0.001).1.as_slice()
    );
}

#[test]
fn test_glyph_cache_survives_separate_font_resolutions() {
    // draw_text resolves the engines on every call; the cache must outlive that.
    let font = Font::new("Segoe UI", 13.0);
    let (a, gid) = first_glyph("Wg", &font);
    let (b, _) = first_glyph("Wg", &font);
    let (_, ba) = a.rasterize_glyph(gid, 12.0, 1.25);
    let (_, bb) = b.rasterize_glyph(gid, 12.0, 1.25);
    assert!(
        Arc::ptr_eq(&ba, &bb),
        "engines resolved by two calls must share one glyph cache"
    );
}

#[test]
fn test_draw_text_pixels_do_not_change_once_glyphs_are_cached() {
    let mut font = Font::new("Segoe UI, Microsoft JhengHei", 12.0);
    font.size = 12.0;
    let render = || {
        let mut pm = Pixmap::with_dpr(300, 60, 1.25).unwrap();
        let mut p = Painter::begin(&mut pm);
        p.set_pen(Some(Pen::new(Color::from_rgba8(226, 232, 240, 255), 1.0)));
        p.draw_text(
            PointF::new(4.0, 18.0),
            "5h 42% resets in 2h 13m 重設於: --",
            &font,
        );
        drop(p);
        pm.data().to_vec()
    };
    let cold = render();
    let warm = render();
    assert!(cold.iter().any(|&b| b != 0), "text must have been drawn");
    assert_eq!(
        cold, warm,
        "a cache hit must paint exactly what a cache miss painted"
    );
}

#[test]
fn test_reusing_engines_does_not_change_how_a_family_string_shapes() {
    use ferrokit_gui::text::font_database::FontDatabase;
    // "'Segoe UI', sans-serif" and the plain "Segoe UI" load the same font, but the list string gets
    // no raw font data and so shapes through per-glyph face metrics, while the plain name shapes through rustybuzz.
    // An engine shared per *font* would hand the list string the plain name's engine as soon as
    // anything resolved "Segoe UI" first, silently switching its shaping.
    let list_font = Font::new("'Segoe UI', 'SF Pro Display', sans-serif", 12.0);
    let plain = Font::new("Segoe UI", 12.0);
    let text = "5h 42% resets 0123456789";

    let shape = |db: &mut FontDatabase| {
        let engines = db.resolve_font_engines_for_text(&list_font, text);
        assert!(!engines.is_empty(), "test font must be installed");
        (
            engines[0].raw_data.is_some(),
            GlyphLayout::shape_with_engines(text, &list_font, &engines),
        )
    };

    // Separate databases so the outcome does not depend on what other tests resolved.
    let mut fresh = FontDatabase::new();
    let expected = shape(&mut fresh);

    let mut after_plain = FontDatabase::new();
    let _ = after_plain.resolve_font_engines_for_text(&plain, text);
    let actual = shape(&mut after_plain);

    assert_eq!(
        expected.0, actual.0,
        "raw data of the list-string engine depends on resolution order"
    );
    assert_eq!(
        expected.1, actual.1,
        "shaping of the list string depends on resolution order"
    );
}
