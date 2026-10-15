// SPDX-License-Identifier: LGPL-3.0-only
//! Dumps glyph coverage bitmaps of the active backend, for comparison against Qt.
//!
//! Usage: glyph_dump <font file> <logical size> <dpr> <chars>   (prints one JSON object per char)
use ferrokit_gui::text::font::SharedFontData;
use ferrokit_gui::text::glyph_face::parse_face;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (file, size, dpr, chars) = (&a[1], a[2].parse::<f32>().unwrap(), a[3].parse::<f32>().unwrap(), &a[4]);
    let data = SharedFontData::from_vec(std::fs::read(file).expect("font file"));
    let face = parse_face(&data, 0).expect("face");
    for ch in chars.chars() {
        let gid = face.glyph_index(ch);
        let (m, bitmap) = face.rasterize_scaled(gid, size, dpr);
        let hex: String = bitmap.iter().map(|b| format!("{b:02x}")).collect();
        println!(
            "{{\"cp\":{},\"gid\":{gid},\"xmin\":{},\"ymin\":{},\"w\":{},\"h\":{},\"adv\":{:.4},\"bits\":\"{hex}\"}}",
            ch as u32,
            m.xmin,
            m.ymin,
            m.width,
            m.height,
            m.advance_width
        );
    }
}
