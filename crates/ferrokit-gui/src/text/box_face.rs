// SPDX-License-Identifier: LGPL-3.0-only
//! Fallback box-glyph face for scripts with no available font: maps every code point to a
//! missing-glyph outline box whose width and height equal the integer pixel size.

use crate::text::font::SharedFontData;
use crate::text::glyph_face::{GlyphFace, GlyphMetrics, LineMetrics};
use std::sync::LazyLock;

/// Minimal OpenType sfnt font for shaping missing-glyph runs: maps all Unicode scalar values via a
/// `cmap` format 13 subtable to glyph 1 with `units_per_em = 1000` and uniform advance `1000`.
pub(crate) fn shaping_font() -> SharedFontData {
    static DATA: LazyLock<SharedFontData> =
        LazyLock::new(|| SharedFontData::from_vec(build_tofu_sfnt()));
    DATA.clone()
}

fn push_be_u16(out: &mut Vec<u8>, values: &[u16]) {
    for &v in values {
        out.extend_from_slice(&v.to_be_bytes());
    }
}

fn push_be_u32(out: &mut Vec<u8>, values: &[u32]) {
    for &v in values {
        out.extend_from_slice(&v.to_be_bytes());
    }
}

fn build_tofu_sfnt() -> Vec<u8> {
    const UPM: u16 = 1000;

    // `cmap`: header (version 0, 1 encoding record) -> platform 3, encoding 10 -> format 13 subtable.
    let mut cmap = Vec::new();
    push_be_u16(&mut cmap, &[0, 1, 3, 10]);
    push_be_u32(&mut cmap, &[12]);
    push_be_u16(&mut cmap, &[13, 0]);
    push_be_u32(&mut cmap, &[28, 0, 1, 0, 0x10_FFFF, 1]);

    // `head`: OpenType font header with 1000 units per em.
    let mut head = Vec::new();
    push_be_u32(&mut head, &[0x0001_0000, 0x0001_0000, 0, 0x5F0F_3CF5]);
    push_be_u16(&mut head, &[0, UPM]);
    head.extend_from_slice(&[0u8; 16]);
    push_be_u16(&mut head, &[0, 0, UPM, UPM, 0, 8, 2, 0, 0]);

    // `hhea`: horizontal header (ascent 1000, descent 0, 2 hmtx metrics).
    let mut hhea = Vec::new();
    push_be_u32(&mut hhea, &[0x0001_0000]);
    push_be_u16(&mut hhea, &[UPM, 0, 0, UPM, 0, 0, UPM, 1, 0, 0]);
    hhea.extend_from_slice(&[0u8; 8]);
    push_be_u16(&mut hhea, &[0, 2]);

    // `hmtx`: glyph 0 and glyph 1 both advance 1000 units with 0 lsb.
    let mut hmtx = Vec::new();
    push_be_u16(&mut hmtx, &[UPM, 0, UPM, 0]);

    // `maxp` version 0.5: 2 glyphs.
    let mut maxp = Vec::new();
    push_be_u32(&mut maxp, &[0x0000_5000]);
    push_be_u16(&mut maxp, &[2]);

    let tables: [(&[u8; 4], &[u8]); 5] = [
        (b"cmap", &cmap),
        (b"head", &head),
        (b"hhea", &hhea),
        (b"hmtx", &hmtx),
        (b"maxp", &maxp),
    ];

    let mut font = Vec::new();
    push_be_u32(&mut font, &[0x0001_0000]);
    push_be_u16(&mut font, &[tables.len() as u16, 64, 2, 16]);

    let mut offset = 12 + 16 * tables.len();
    for (tag, data) in &tables {
        font.extend_from_slice(*tag);
        push_be_u32(&mut font, &[0, offset as u32, data.len() as u32]);
        offset += data.len().div_ceil(4) * 4;
    }
    for (_, data) in &tables {
        font.extend_from_slice(data);
        font.resize(font.len().div_ceil(4) * 4, 0);
    }
    font
}

/// Fallback glyph face that renders a hollow square outline for every character.
pub(crate) struct BoxFace;

fn box_size(px: f32) -> i32 {
    px as i32
}

impl GlyphFace for BoxFace {
    fn glyph_index(&self, _ch: char) -> u16 {
        1
    }

    fn metrics_indexed(&self, _glyph_id: u16, px: f32) -> GlyphMetrics {
        let size = box_size(px);
        GlyphMetrics {
            xmin: 0,
            ymin: 0,
            width: size.max(0) as usize,
            height: size.max(0) as usize,
            advance_width: size as f32,
            advance_height: 0.0,
        }
    }

    fn rasterize_indexed(&self, glyph_id: u16, px: f32) -> (GlyphMetrics, Vec<u8>) {
        let metrics = self.metrics_indexed(glyph_id, px);
        let size = box_size(px).max(0) as usize;
        let mut bitmap = vec![0u8; size * size];
        if size >= 4 {
            let min = 2;
            let max = size - 3;
            for i in min..=max {
                bitmap[min * size + i] = 255;
                bitmap[max * size + i] = 255;
                bitmap[i * size + min] = 255;
                bitmap[i * size + max] = 255;
            }
        }
        (metrics, bitmap)
    }

    fn line_metrics(&self, px: f32) -> Option<LineMetrics> {
        let size = box_size(px) as f32;
        Some(LineMetrics {
            ascent: size,
            descent: 0.0,
            leading: ((size * 0.15 * 64.0).trunc() / 64.0).ceil(),
            unrounded: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shaping_font_maps_everything_to_glyph_one() {
        use rustybuzz::ttf_parser::{Face, GlyphId};
        let data = shaping_font();
        let face = Face::parse(data.as_slice(), 0).expect("parses");
        assert_eq!(face.units_per_em(), 1000);
        for ch in ['a', '\u{928}', '\u{1F600}', '\u{10FFFF}'] {
            assert_eq!(face.glyph_index(ch), Some(GlyphId(1)), "{ch:?}");
        }
        assert_eq!(face.glyph_hor_advance(GlyphId(1)), Some(1000));
    }

    #[test]
    fn a_box_is_one_pixel_size_wide_and_drawn_as_a_square_outline() {
        let face = BoxFace;
        assert_eq!(face.glyph_index('x'), 1);
        assert_eq!(face.glyph_index('\u{928}'), 1);
        let (metrics, bitmap) = face.rasterize_indexed(1, 16.0);
        assert_eq!(metrics.advance_width, 16.0);
        assert_eq!((metrics.width, metrics.height), (16, 16));
        assert_eq!(bitmap.len(), 256);
        let on: Vec<usize> = (0..256).filter(|&i| bitmap[i] == 255).collect();
        assert_eq!(on.len(), 4 * 12 - 4);
        assert!(on.contains(&(2 * 16 + 2)) && on.contains(&(13 * 16 + 13)));
        assert!(!on.contains(&(3 * 16 + 3)));
        assert_eq!(face.metrics_indexed(1, 13.7).advance_width, 13.0);
        assert_eq!(face.line_metrics(20.0).map(|m| m.leading), Some(3.0));
    }
}
