// SPDX-License-Identifier: LGPL-3.0-only
//! The generic outline backend must lay out and draw text like the `fontdue` rasterizer it replaced:
//! identical glyph mapping and metrics (so layout cannot move), and coverage within rounding.
use ferrokit_gui::text::font::SharedFontData;
use ferrokit_gui::text::glyph_face::GlyphFace;
use ferrokit_gui::text::outline_face::OutlineFace;

const TEXT: &str = "Claude HUD Monitor 5h 42% 7d 18% resets in 2h 13m 0123456789 $1.28 Opus Sonnet Refresh All g y Q @ & 重設於 立即重新整理所有 開機自動啟動";
const SIZES: &[f32] = &[11.0, 12.0, 15.0, 16.25, 18.75, 24.0];

fn compare(file: &str, ttc_index: u32) {
    let Ok(bytes) = std::fs::read(format!("C:/Windows/Fonts/{file}")) else {
        return; // font not installed on this machine
    };
    let data = SharedFontData::from_vec(bytes);
    let outline = OutlineFace::new(data.clone(), ttc_index).expect("outline face");
    let reference = fontdue::Font::from_bytes(
        data.as_slice(),
        fontdue::FontSettings { collection_index: ttc_index, ..Default::default() },
    )
    .expect("fontdue");

    let (mut compared, mut sum, mut worst) = (0u64, 0u64, 0i32);
    for ch in TEXT.chars().filter(|c| !c.is_whitespace()) {
        let gid = reference.lookup_glyph_index(ch);
        assert_eq!(outline.glyph_index(ch), gid, "{file}: glyph index of {ch:?}");
        if gid == 0 {
            continue;
        }
        for &px in SIZES {
            let (rm, rb) = reference.rasterize_indexed(gid, px);
            let (om, ob) = outline.rasterize_indexed(gid, px);
            let ctx = format!("{file}: {ch:?}@{px}");
            assert_eq!((om.xmin, om.ymin, om.width, om.height), (rm.xmin, rm.ymin, rm.width, rm.height), "{ctx}: placement");
            assert!((om.advance_width - rm.advance_width).abs() < 1e-3, "{ctx}: advance");
            assert_eq!(ob.len(), rb.len(), "{ctx}: bitmap length");
            assert_eq!(outline.metrics_indexed(gid, px), om, "{ctx}: metrics_indexed must match rasterize_indexed");
            for (a, b) in rb.iter().zip(ob.iter()) {
                let d = (*a as i32 - *b as i32).abs();
                sum += d as u64;
                worst = worst.max(d);
                compared += 1;
            }
        }
    }
    assert!(compared > 0, "{file}: nothing compared");
    let mean = sum as f64 / compared as f64;
    assert!(worst <= 16, "{file}: worst per-pixel coverage difference {worst}/255");
    assert!(mean < 1.0, "{file}: mean coverage difference {mean:.2}/255");
}

#[test]
fn outline_face_matches_fontdue_on_latin_fonts() {
    compare("segoeui.ttf", 0);
    compare("consola.ttf", 0);
}

#[test]
fn outline_face_matches_fontdue_on_cjk_collection() {
    compare("msjh.ttc", 0);
}
