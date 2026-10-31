// SPDX-License-Identifier: LGPL-3.0-only
//! The DirectWrite backend rasterizes with the OS, but its bitmaps must sit where the outline backend's
//! do (same pen origin and y convention) and carry the same amount of ink; only edge treatment
//! (hinting, ClearType filtering) may differ. Pixel-level agreement with Qt is measured by
//! a separate comparison script, which needs PySide6 and so cannot run here.
#![cfg(windows)]

use ferrokit_gui::text::directwrite_face::{is_available, DirectWriteFace};
use ferrokit_gui::text::font::SharedFontData;
use ferrokit_gui::text::glyph_face::GlyphFace;
use ferrokit_gui::text::outline_face::OutlineFace;

fn faces(file: &str) -> Option<(DirectWriteFace, OutlineFace)> {
    if !is_available() {
        return None;
    }
    let data = SharedFontData::from_vec(std::fs::read(format!("C:/Windows/Fonts/{file}")).ok()?);
    Some((
        DirectWriteFace::new(data.clone(), 0).ok()?,
        OutlineFace::new(data, 0).ok()?,
    ))
}

fn ink(bitmap: &[u8]) -> u64 {
    bitmap.iter().map(|&b| b as u64).sum()
}

#[test]
fn directwrite_bitmaps_are_placed_like_the_outline_backends() {
    for (file, text) in [
        ("segoeui.ttf", "Claude HUD 5h 42% gjpQ@"),
        ("msjh.ttc", "重設於立即新整理"),
    ] {
        let Some((dw, outline)) = faces(file) else {
            return;
        };
        for ch in text.chars().filter(|c| !c.is_whitespace()) {
            let gid = outline.glyph_index(ch);
            assert_eq!(dw.glyph_index(ch), gid, "{file}: glyph index of {ch:?}");
            for (size, scale) in [(12.0f32, 1.25f32), (14.0, 1.5), (18.0, 1.25)] {
                let ctx = format!("{file}: {ch:?} {size}px x{scale}");
                let (dm, db) = dw.rasterize_scaled(gid, size, scale);
                let (om, ob) = outline.rasterize_scaled(gid, size, scale);
                assert_eq!(
                    db.len(),
                    dm.width * dm.height,
                    "{ctx}: bitmap length matches its metrics"
                );
                // Hinting may move an edge by a pixel, never more; the baseline convention must agree.
                for (name, a, b) in [
                    ("xmin", dm.xmin, om.xmin),
                    ("ymin", dm.ymin, om.ymin),
                    ("width", dm.width as i32, om.width as i32),
                    ("height", dm.height as i32, om.height as i32),
                ] {
                    assert!((a - b).abs() <= 2, "{ctx}: {name} {a} vs outline {b}");
                }
                // Hinting and ClearType filtering add or remove ink at small sizes: measured
                // DirectWrite/outline ink ratios span 0.74-1.36 over these glyphs. The bound only has to
                // catch a glyph that is missing, doubled or from the wrong font.
                let (di, oi) = (ink(&db) as f64, ink(&ob) as f64);
                assert!(
                    (0.6 * oi..=1.6 * oi).contains(&di),
                    "{ctx}: ink {di} vs outline {oi} is outside 0.6x-1.6x"
                );
                assert_eq!(
                    dm.advance_width, om.advance_width,
                    "{ctx}: advance comes from the font file"
                );
            }
        }
    }
}

#[test]
fn directwrite_blank_glyphs_have_no_bitmap() {
    let Some((dw, outline)) = faces("segoeui.ttf") else {
        return;
    };
    let space = outline.glyph_index(' ');
    let (m, bitmap) = dw.rasterize_scaled(space, 12.0, 1.25);
    assert!(
        bitmap.is_empty() && m.width == 0 && m.height == 0,
        "a space draws nothing"
    );
    assert!(m.advance_width > 0.0, "but still advances the pen");
}
