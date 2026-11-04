// SPDX-License-Identifier: LGPL-3.0-only
//! Which pixel `Painter::draw_text` blits a glyph at when the pen has a fraction.
//!
//! Qt blits at `qFloor(position.x)` for engines that position glyphs at sub-pixel offsets, for the
//! Windows engines and for its FreeType engine (`QFontEngine::getGlyphPositions`,
//! `QRasterPaintEngine::drawCachedGlyphs`); ferrokit's FreeType-style face (`SkrifaFace`, Linux and
//! the other Unix systems but macOS) places glyphs that way. Fully hinted, it has no sub-pixel
//! positions, so a glyph sits at the floor of its pen. Windows is compared against Qt in
//! `tests/test_directwrite_placement.rs`; here it only has to agree with the floor.
//!
//! One glyph of the synthetic `tests/data/oracle_text.ttf` (a box) is drawn, so the advance and the
//! shaping do not enter.
#![cfg(not(target_os = "macos"))]

use ferrokit_gui::geometry::primitives::PointF;
use ferrokit_gui::paint::{Painter, Pen, Pixmap};
use ferrokit_gui::text::font::{Font, HintingPreference, SharedFontData};
use ferrokit_gui::text::font_database::set_application_device_pixel_ratio;
use ferrokit_gui::text::smoothing::TextSmoothing;
use ferrokit_gui::tiny_skia::Color;

const FONT: &[u8] = include_bytes!("data/oracle_text.ttf");

fn draw_h_at(x: f32) -> Vec<u8> {
    let font = Font::new("FerrokitOracleText", 16.0)
        .with_font_data(SharedFontData::from_vec(FONT.to_vec()))
        .with_hinting_preference(HintingPreference::PreferFullHinting);
    let mut pixmap = Pixmap::with_dpr(48, 32, 1.0).expect("pixmap");
    pixmap.fill(Color::WHITE);
    {
        let mut painter = Painter::begin(&mut pixmap);
        painter.set_text_smoothing(TextSmoothing {
            cleartype: false,
            gamma: 1.0,
        });
        painter.set_pen(Some(Pen::new(Color::BLACK, 1.0)));
        painter.draw_text(PointF::new(x, 22.0), "H", &font);
    }
    pixmap.data().to_vec()
}

#[test]
fn a_glyph_is_blitted_at_the_floored_pen_position() {
    set_application_device_pixel_ratio(1.0);
    let whole = draw_h_at(6.0);
    let next = draw_h_at(7.0);
    assert_ne!(whole, next, "the glyph must move with its pen");
    for x in [6.25, 6.5, 6.75] {
        let drawn = draw_h_at(x);
        assert_eq!(drawn, whole, "pen at x = {x}");
    }
}
