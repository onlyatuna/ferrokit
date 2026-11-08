// SPDX-License-Identifier: LGPL-3.0-only
//! Qt's box font engine (`QFontEngineBox`, qfontengine.cpp:1800-1965): the engine a script item gets
//! when no font can shape it, which draws every character as an empty box one pixel size wide.

use crate::text::font::SharedFontData;
use crate::text::glyph_face::{GlyphFace, GlyphMetrics, LineMetrics};
use std::sync::LazyLock;

/// The font HarfBuzz shapes the box engine's text with: Qt hands it the engine itself, whose glyph
/// for every character is 1 and whose advance is the size (`hb_qt_font_get_nominal_glyph`,
/// `hb_qt_get_glyph_h_advance`), with no layout tables. This is the same as an sfnt with a
/// many-to-one `cmap` (format 13) from every code point to glyph 1, 1000 units per em, an advance
/// of 1000 for both glyphs and no `GSUB`, `GPOS` or `GDEF`, so the shaper's clusters, reordering,
/// dotted circles and zeroed marks come out as they do in Qt.
pub(crate) fn shaping_font() -> SharedFontData {
    static DATA: LazyLock<SharedFontData> =
        LazyLock::new(|| SharedFontData::from_vec(build_font()));
    DATA.clone()
}

fn build_font() -> Vec<u8> {
    fn u16s(out: &mut Vec<u8>, values: &[u16]) {
        for v in values {
            out.extend_from_slice(&v.to_be_bytes());
        }
    }
    fn u32s(out: &mut Vec<u8>, values: &[u32]) {
        for v in values {
            out.extend_from_slice(&v.to_be_bytes());
        }
    }
    // cmap: one Windows Unicode full repertoire subtable, format 13, one group.
    let mut cmap = Vec::new();
    u16s(&mut cmap, &[0, 1, 3, 10]);
    u32s(&mut cmap, &[12]);
    u16s(&mut cmap, &[13, 0]);
    u32s(&mut cmap, &[16 + 12, 0, 1, 0, 0x10_FFFF, 1]);
    // head
    let mut head = Vec::new();
    u32s(&mut head, &[0x0001_0000, 0x0001_0000, 0, 0x5F0F_3CF5]);
    u16s(&mut head, &[0, 1000]);
    head.extend_from_slice(&[0u8; 16]); // created, modified
    // xMin, yMin, xMax, yMax, macStyle, lowestRecPPEM, fontDirectionHint, indexToLocFormat,
    // glyphDataFormat.
    u16s(&mut head, &[0, 0, 1000, 1000, 0, 8, 2, 0, 0]);
    // hhea
    let mut hhea = Vec::new();
    u32s(&mut hhea, &[0x0001_0000]);
    // ascender, descender, lineGap, advanceWidthMax, minLeftSideBearing, minRightSideBearing,
    // xMaxExtent, caretSlopeRise, caretSlopeRun, caretOffset.
    u16s(&mut hhea, &[1000, 0, 0, 1000, 0, 0, 1000, 1, 0, 0]);
    hhea.extend_from_slice(&[0u8; 8]); // reserved
    u16s(&mut hhea, &[0, 2]);
    // hmtx: two long metrics
    let mut hmtx = Vec::new();
    u16s(&mut hmtx, &[1000, 0, 1000, 0]);
    // maxp 0.5
    let mut maxp = Vec::new();
    u32s(&mut maxp, &[0x0000_5000]);
    u16s(&mut maxp, &[2]);
    let tables: [(&[u8; 4], &Vec<u8>); 5] = [
        (b"cmap", &cmap),
        (b"head", &head),
        (b"hhea", &hhea),
        (b"hmtx", &hmtx),
        (b"maxp", &maxp),
    ];
    let mut font = Vec::new();
    u32s(&mut font, &[0x0001_0000]);
    u16s(&mut font, &[tables.len() as u16, 64, 2, 16]);
    let mut offset = 12 + 16 * tables.len();
    for (tag, data) in &tables {
        font.extend_from_slice(*tag);
        u32s(&mut font, &[0, offset as u32, data.len() as u32]);
        offset += data.len().div_ceil(4) * 4;
    }
    for (_, data) in &tables {
        font.extend_from_slice(data);
        font.resize(font.len().div_ceil(4) * 4, 0);
    }
    font
}

/// The box engine. Its size is the request's pixel size, which Qt passes as an `int` (the
/// fraction of a size like 13.5 is dropped).
pub(crate) struct BoxFace;
/// `QFontEngineBox::_size`.
fn size_of(px: f32) -> i32 {
    px as i32
}

impl GlyphFace for BoxFace {
    /// `QFontEngineBox::glyphIndex`: every character has the glyph 1.
    fn glyph_index(&self, _ch: char) -> u16 {
        1
    }

    /// `QFontEngineBox::boundingBox(glyph_t)`: `glyph_metrics_t(0, -_size, _size, _size, _size, 0)`.
    fn metrics_indexed(&self, _glyph_id: u16, px: f32) -> GlyphMetrics {
        let size = size_of(px);
        GlyphMetrics {
            xmin: 0,
            ymin: 0,
            width: size.max(0) as usize,
            height: size.max(0) as usize,
            advance_width: size as f32,
            advance_height: 0.0,
        }
    }

    /// `QFontEngineBox::alphaMapForGlyph`: the outline of a square inset by two pixels at the
    /// left and top and three at the right and bottom.
    fn rasterize_indexed(&self, glyph_id: u16, px: f32) -> (GlyphMetrics, Vec<u8>) {
        let metrics = self.metrics_indexed(glyph_id, px);
        let size = size_of(px).max(0) as usize;
        let mut bitmap = vec![0u8; size * size];
        if size >= 4 {
            for i in 2..=size - 3 {
                bitmap[i + 2 * size] = 255;
                bitmap[i + (size - 3) * size] = 255;
                bitmap[2 + i * size] = 255;
                bitmap[size - 3 + i * size] = 255;
            }
        }
        (metrics, bitmap)
    }

    /// `QFontEngineBox::ascent`, `descent` and `leading` (15 % of the size, rounded up).
    fn line_metrics(&self, px: f32) -> Option<LineMetrics> {
        let size = size_of(px) as f32;
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

    /// The font the shaper gets parses, maps every code point to glyph 1 and gives both glyphs
    /// the advance of the size.
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
        // Rows 2 and 13, columns 2 and 13, from 2 to 13 inclusive.
        assert_eq!(on.len(), 4 * 12 - 4);
        assert!(on.contains(&(2 * 16 + 2)) && on.contains(&(13 * 16 + 13)));
        assert!(!on.contains(&(3 * 16 + 3)));
        assert_eq!(face.metrics_indexed(1, 13.7).advance_width, 13.0);
        assert_eq!(face.line_metrics(20.0).map(|m| m.leading), Some(3.0));
    }
}
