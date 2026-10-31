// SPDX-License-Identifier: LGPL-3.0-only
//! Cost of drawing a typical frame of overlay text, and how much of it is glyph rasterisation.
use ferrokit_gui::geometry::primitives::PointF;
use ferrokit_gui::paint::{Painter, Pen, Pixmap};
use ferrokit_gui::text::font::Font;
use ferrokit_gui::text::font_database::resolve_font_engines_for_text_global;
use ferrokit_gui::text::glyph_layout::GlyphLayout;
use ferrokit_gui::tiny_skia::Color;
use std::collections::HashSet;
use std::time::Instant;

const LINES: &[&str] = &[
    "Claude HUD Monitor",
    "5h 42%  resets in 2h 13m",
    "7d 18%  resets in 4d 02h",
    "Opus  31%   Sonnet  12%",
    "重設於: --",
    "Codex  68%   18:45",
    "Refresh All   30 秒",
    "12,345 tokens   $1.28",
    "Updated 14:02:11",
    "100%",
    "0123456789 %",
    "Session  Weekly  Extra",
];

fn font() -> Font {
    let mut f = Font::default();
    f.family = "Segoe UI, Microsoft JhengHei, Segoe UI Emoji".to_string();
    f.size = 12.0;
    f
}

fn main() {
    let dpr = 1.25f32;
    let f = font();
    let mut pm = Pixmap::with_dpr(900, 300, dpr).unwrap();
    let draw_frame = |pm: &mut Pixmap| {
        let mut p = Painter::begin(pm);
        p.set_pen(Some(Pen::new(Color::from_rgba8(226, 232, 240, 255), 1.0)));
        for (i, l) in LINES.iter().enumerate() {
            p.draw_text(PointF::new(10.0, 16.0 + 20.0 * i as f32), l, &f);
        }
    };

    // warm fonts + shaping caches
    for _ in 0..5 {
        draw_frame(&mut pm);
    }

    let n = 200;
    let t = Instant::now();
    for _ in 0..n {
        draw_frame(&mut pm);
    }
    let per_frame = t.elapsed().as_secs_f64() * 1e3 / n as f64;

    // Same glyphs, rasterisation only (what a glyph cache would remove).
    let mut glyphs: Vec<(usize, u16, ferrokit_gui::text::glyph_face::SharedGlyphFace)> = Vec::new();
    let mut unique = HashSet::new();
    let mut total_glyphs = 0usize;
    for l in LINES {
        let engines = resolve_font_engines_for_text_global(&f, l);
        let layout = GlyphLayout::shape_with_engines(l, &f, &engines);
        for g in layout.glyphs {
            let e = engines.get(g.font_index as usize).unwrap_or(&engines[0]);
            total_glyphs += 1;
            unique.insert((g.font_index, g.glyph_id));
            glyphs.push((g.font_index as usize, g.glyph_id, e.face.clone()));
        }
    }
    let t = Instant::now();
    for _ in 0..n {
        for (_, gid, face) in &glyphs {
            std::hint::black_box(face.rasterize_indexed(*gid, f.size * dpr));
        }
    }
    let raster = t.elapsed().as_secs_f64() * 1e3 / n as f64;

    // Shaping only
    let t = Instant::now();
    for _ in 0..n {
        for l in LINES {
            let engines = resolve_font_engines_for_text_global(&f, l);
            std::hint::black_box(GlyphLayout::shape_with_engines(l, &f, &engines));
        }
    }
    let shape = t.elapsed().as_secs_f64() * 1e3 / n as f64;

    println!(
        "lines {}, glyphs {}, unique (font,glyph) {}",
        LINES.len(),
        total_glyphs,
        unique.len()
    );
    println!("draw_text, whole frame      : {per_frame:.3} ms");
    println!(
        "  of which rasterise_indexed: {raster:.3} ms ({:.0}%)",
        raster / per_frame * 100.0
    );
    println!(
        "  shaping (resolve+shape)   : {shape:.3} ms ({:.0}%)",
        shape / per_frame * 100.0
    );
}
