// SPDX-License-Identifier: LGPL-3.0-only
use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Transform};
use crate::text::glyph_face::{GlyphFace, GlyphMetrics};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColorLayer {
    pub glyph_id: u16,
    pub color: [u8; 4], // RGBA
}

/// Locates the SFNT table directory offset within a font or TTC collection.
fn get_sfnt_offset(font_data: &[u8], face_index: u32) -> Option<usize> {
    if font_data.len() < 12 {
        return None;
    }
    if &font_data[0..4] == b"ttcf" {
        let count = u32::from_be_bytes(font_data[8..12].try_into().ok()?) as usize;
        if (face_index as usize) >= count {
            return None;
        }
        let off_idx = 12 + (face_index as usize) * 4;
        if off_idx + 4 > font_data.len() {
            return None;
        }
        let offset = u32::from_be_bytes(font_data[off_idx..off_idx + 4].try_into().ok()?) as usize;
        Some(offset)
    } else {
        Some(0)
    }
}

/// Parses OpenType COLRv0 layers and CPAL palette colors for `glyph_id`.
pub fn parse_colr_v0_layers(font_data: &[u8], face_index: u32, glyph_id: u16) -> Option<Vec<ColorLayer>> {
    let sfnt_offset = get_sfnt_offset(font_data, face_index)?;
    if sfnt_offset + 12 > font_data.len() {
        return None;
    }

    let num_tables = u16::from_be_bytes([font_data[sfnt_offset + 4], font_data[sfnt_offset + 5]]) as usize;
    let mut colr_range: Option<(usize, usize)> = None;
    let mut cpal_range: Option<(usize, usize)> = None;

    for i in 0..num_tables {
        let entry_off = sfnt_offset + 12 + i * 16;
        if entry_off + 16 > font_data.len() {
            break;
        }
        let tag = &font_data[entry_off..entry_off + 4];
        let offset = u32::from_be_bytes(font_data[entry_off + 8..entry_off + 12].try_into().ok()?) as usize;
        let length = u32::from_be_bytes(font_data[entry_off + 12..entry_off + 16].try_into().ok()?) as usize;
        if tag == b"COLR" {
            colr_range = Some((offset, length));
        } else if tag == b"CPAL" {
            cpal_range = Some((offset, length));
        }
    }

    let (colr_off, colr_len) = colr_range?;
    let (cpal_off, cpal_len) = cpal_range?;
    let colr_end = colr_off.checked_add(colr_len)?;
    let cpal_end = cpal_off.checked_add(cpal_len)?;
    let colr = font_data.get(colr_off..colr_end)?;
    let cpal = font_data.get(cpal_off..cpal_end)?;
    if colr.len() < 14 || cpal.len() < 12 {
        return None;
    }
    let num_base = u16::from_be_bytes([colr[2], colr[3]]) as usize;
    let base_off = u32::from_be_bytes(colr[4..8].try_into().ok()?) as usize;
    let layer_off = u32::from_be_bytes(colr[8..12].try_into().ok()?) as usize;
    let num_layers = u16::from_be_bytes([colr[12], colr[13]]) as usize;

    if base_off + num_base * 6 > colr.len() || layer_off + num_layers * 4 > colr.len() {
        return None;
    }

    let base_records = &colr[base_off..base_off + num_base * 6];
    let mut left = 0;
    let mut right = num_base;
    let mut found_record: Option<(u16, u16)> = None;

    while left < right {
        let mid = (left + right) / 2;
        let off = mid * 6;
        let gid = u16::from_be_bytes([base_records[off], base_records[off + 1]]);
        if gid == glyph_id {
            let first_layer = u16::from_be_bytes([base_records[off + 2], base_records[off + 3]]);
            let n_layers = u16::from_be_bytes([base_records[off + 4], base_records[off + 5]]);
            found_record = Some((first_layer, n_layers));
            break;
        } else if gid < glyph_id {
            left = mid + 1;
        } else {
            right = mid;
        }
    }

    let (first_layer, n_layers) = found_record?;
    let first_layer = first_layer as usize;
    let n_layers = n_layers as usize;

    if first_layer + n_layers > num_layers {
        return None;
    }

    let cpal = &font_data[cpal_off..cpal_off + cpal_len];
    let n_palettes = u16::from_be_bytes([cpal[4], cpal[5]]) as usize;
    let n_colors = u16::from_be_bytes([cpal[6], cpal[7]]) as usize;
    let color_records_off = u32::from_be_bytes(cpal[8..12].try_into().ok()?) as usize;

    let palette_start = if n_palettes > 0 && 12 + 2 <= cpal.len() {
        u16::from_be_bytes([cpal[12], cpal[13]]) as usize
    } else {
        0
    };

    let layer_records = &colr[layer_off..layer_off + num_layers * 4];
    let mut result = Vec::with_capacity(n_layers);

    for i in 0..n_layers {
        let l_off = (first_layer + i) * 4;
        let layer_gid = u16::from_be_bytes([layer_records[l_off], layer_records[l_off + 1]]);
        let pal_idx = u16::from_be_bytes([layer_records[l_off + 2], layer_records[l_off + 3]]);

        let color = if pal_idx == 0xFFFF {
            [255, 255, 255, 255]
        } else {
            let actual_idx = palette_start + pal_idx as usize;
            if actual_idx < n_colors && color_records_off + actual_idx * 4 + 4 <= cpal.len() {
                let c_off = color_records_off + actual_idx * 4;
                let b = cpal[c_off];
                let g = cpal[c_off + 1];
                let r = cpal[c_off + 2];
                let a = cpal[c_off + 3];
                [r, g, b, a]
            } else {
                [255, 255, 255, 255]
            }
        };

        result.push(ColorLayer {
            glyph_id: layer_gid,
            color,
        });
    }

    Some(result)
}

pub(crate) struct SkiaPathBuilder {
    pub(crate) builder: PathBuilder,
}

impl rustybuzz::ttf_parser::OutlineBuilder for SkiaPathBuilder {
    fn move_to(&mut self, x: f32, y: f32) {
        self.builder.move_to(x, y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.builder.line_to(x, y);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.builder.quad_to(x1, y1, x, y);
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.builder.cubic_to(x1, y1, x2, y2, x, y);
    }
    fn close(&mut self) {
        self.builder.close();
    }
}

/// Rasterizes a multi-layer OpenType color glyph into a 32-bit ARGB/RGBA premultiplied `Pixmap`.
///
/// Mirrors Qt6's `QWindowsFontEngineDirectWrite::alphaRGBMapForGlyph` / `renderColr0GlyphRun`.
pub fn rasterize_color_glyph(
    font_data: &[u8],
    face_index: u32,
    glyph_id: u16,
    face_metrics: &dyn GlyphFace,
    px_size: f32,
) -> Option<(GlyphMetrics, Pixmap)> {
    let layers = parse_colr_v0_layers(font_data, face_index, glyph_id)?;
    if layers.is_empty() {
        return None;
    }

    let face = rustybuzz::ttf_parser::Face::parse(font_data, face_index).ok()?;
    let units_per_em = face.units_per_em() as f32;
    if units_per_em <= 0.0 {
        return None;
    }

    let metrics = face_metrics.metrics_indexed(glyph_id, px_size);

    let (width, height, xmin, ymin) = if metrics.width > 0 && metrics.height > 0 {
        (metrics.width, metrics.height, metrics.xmin, metrics.ymin)
    } else if let Some(bbox) = face.glyph_bounding_box(rustybuzz::ttf_parser::GlyphId(glyph_id)) {
        let scale = px_size / units_per_em;
        let xmin = (bbox.x_min as f32 * scale).floor() as i32;
        let xmax = (bbox.x_max as f32 * scale).ceil() as i32;
        let ymin = (bbox.y_min as f32 * scale).floor() as i32;
        let ymax = (bbox.y_max as f32 * scale).ceil() as i32;
        ((xmax - xmin).max(1) as usize, (ymax - ymin).max(1) as usize, xmin, ymin)
    } else {
        let advance = metrics.advance_width.round().max(px_size) as usize;
        (advance, px_size.round().max(1.0) as usize, 0, 0)
    };
    if crate::text::glyph_face::exceeds_glyph_budget(width, height) {
        return None;
    }

    let mut pixmap = Pixmap::new(width.max(1) as u32, height.max(1) as u32)?;
    pixmap.fill(Color::TRANSPARENT);

    let scale = px_size / units_per_em;
    let transform = Transform::from_scale(scale, -scale)
        .post_translate(-xmin as f32, (height as i32 + ymin) as f32);

    for layer in &layers {
        let mut skia_builder = SkiaPathBuilder {
            builder: PathBuilder::new(),
        };
        if let Some(_bbox) = face.outline_glyph(rustybuzz::ttf_parser::GlyphId(layer.glyph_id), &mut skia_builder) {
            if let Some(path) = skia_builder.builder.finish() {
                let mut paint = Paint::default();
                paint.set_color_rgba8(layer.color[0], layer.color[1], layer.color[2], layer.color[3]);
                paint.anti_alias = true;
                pixmap.fill_path(&path, &paint, FillRule::Winding, transform, None);
            }
        }
    }

    let effective_metrics = GlyphMetrics {
        xmin,
        ymin,
        width,
        height,
        advance_width: metrics.advance_width,
        advance_height: metrics.advance_height,
    };

    Some((effective_metrics, pixmap))
}

#[cfg(test)]
mod colr_bounds_tests {
    use super::*;

    fn be16(v: &mut Vec<u8>, x: u16) {
        v.extend_from_slice(&x.to_be_bytes());
    }
    fn be32(v: &mut Vec<u8>, x: u32) {
        v.extend_from_slice(&x.to_be_bytes());
    }

    /// COLR with one base glyph (id 5) -> one layer (glyph 9, palette entry 0).
    fn colr_table() -> Vec<u8> {
        let mut t = Vec::new();
        be16(&mut t, 0); // version
        be16(&mut t, 1); // numBaseGlyphRecords
        be32(&mut t, 14); // baseGlyphRecordsOffset
        be32(&mut t, 20); // layerRecordsOffset
        be16(&mut t, 1); // numLayerRecords
        be16(&mut t, 5); // base gid
        be16(&mut t, 0); // firstLayerIndex
        be16(&mut t, 1); // numLayers
        be16(&mut t, 9); // layer gid
        be16(&mut t, 0); // palette index
        t
    }

    /// CPAL with one palette and one BGRA color (blue 1, green 2, red 3, alpha 4).
    fn cpal_table() -> Vec<u8> {
        let mut t = Vec::new();
        be16(&mut t, 0);
        be16(&mut t, 1); // numPaletteEntries
        be16(&mut t, 1); // numPalettes
        be16(&mut t, 1); // numColorRecords
        be32(&mut t, 14); // colorRecordsArrayOffset
        be16(&mut t, 0); // colorRecordIndices[0]
        t.extend_from_slice(&[1, 2, 3, 4]);
        t
    }

    /// Builds an sfnt with COLR and CPAL, letting tests lie about the declared lengths.
    fn font(colr: &[u8], cpal: &[u8], colr_len: u32, cpal_len: u32) -> Vec<u8> {
        let colr_off = 12 + 2 * 16;
        let cpal_off = colr_off + colr.len();
        let mut f = Vec::new();
        f.extend_from_slice(&[0, 1, 0, 0]);
        be16(&mut f, 2);
        f.extend_from_slice(&[0; 6]);
        for (tag, off, len) in [(b"COLR", colr_off, colr_len), (b"CPAL", cpal_off, cpal_len)] {
            f.extend_from_slice(tag);
            be32(&mut f, 0);
            be32(&mut f, off as u32);
            be32(&mut f, len);
        }
        f.extend_from_slice(colr);
        f.extend_from_slice(cpal);
        f
    }

    #[test]
    fn well_formed_font_parses() {
        let (c, p) = (colr_table(), cpal_table());
        let f = font(&c, &p, c.len() as u32, p.len() as u32);
        let layers = parse_colr_v0_layers(&f, 0, 5).unwrap();
        assert_eq!(layers, vec![ColorLayer { glyph_id: 9, color: [3, 2, 1, 4] }]);
        assert!(parse_colr_v0_layers(&f, 0, 6).is_none());
    }

    #[test]
    fn declared_table_length_past_end_of_file_is_rejected() {
        let (c, p) = (colr_table(), cpal_table());
        assert!(parse_colr_v0_layers(&font(&c, &p, 10_000, p.len() as u32), 0, 5).is_none());
        assert!(parse_colr_v0_layers(&font(&c, &p, c.len() as u32, 10_000), 0, 5).is_none());
        assert!(parse_colr_v0_layers(&font(&c, &p, u32::MAX, p.len() as u32), 0, 5).is_none());
    }

    #[test]
    fn tables_shorter_than_their_headers_are_rejected() {
        let (c, p) = (colr_table(), cpal_table());
        assert!(parse_colr_v0_layers(&font(&c, &p, 13, p.len() as u32), 0, 5).is_none());
        assert!(parse_colr_v0_layers(&font(&c, &p, c.len() as u32, 11), 0, 5).is_none());
        assert!(parse_colr_v0_layers(&font(&c, &p, 0, 0), 0, 5).is_none());
    }

    #[test]
    fn record_offsets_past_the_table_are_rejected() {
        let p = cpal_table();
        let mut c = colr_table();
        c[4..8].copy_from_slice(&1000u32.to_be_bytes()); // base record offset
        assert!(parse_colr_v0_layers(&font(&c, &p, c.len() as u32, p.len() as u32), 0, 5).is_none());
        let mut c = colr_table();
        c[8..12].copy_from_slice(&1000u32.to_be_bytes()); // layer record offset
        assert!(parse_colr_v0_layers(&font(&c, &p, c.len() as u32, p.len() as u32), 0, 5).is_none());
    }

    #[test]
    fn truncated_directory_and_bad_ttc_index_are_rejected() {
        let (c, p) = (colr_table(), cpal_table());
        let f = font(&c, &p, c.len() as u32, p.len() as u32);
        assert!(parse_colr_v0_layers(&f[..30], 0, 5).is_none());
        assert!(parse_colr_v0_layers(&f[..8], 0, 5).is_none());
        let mut ttc = b"ttcf".to_vec();
        ttc.extend_from_slice(&[0, 1, 0, 0, 0, 0, 0, 1]);
        assert!(parse_colr_v0_layers(&ttc, 3, 5).is_none());
    }
}
