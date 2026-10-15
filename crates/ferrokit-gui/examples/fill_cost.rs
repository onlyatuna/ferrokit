// SPDX-License-Identifier: LGPL-3.0-only
use ferrokit_gui::geometry::primitives::RectF;
use ferrokit_gui::paint::{Brush, Painter, Pen, Pixmap};
use ferrokit_gui::tiny_skia::Color;
use std::time::Instant;

fn run(label: &str, w: u32, h: u32, clip: bool, border: bool, fresh: bool, pace_ms: u64) {
    let dpr = 1.25f32;
    let mut pm = Pixmap::with_dpr(w, h, dpr).unwrap();
    let n = 20;
    let mut total = std::time::Duration::ZERO;
    for _ in 0..n {
        if pace_ms > 0 { std::thread::sleep(std::time::Duration::from_millis(pace_ms)); }
        let t = Instant::now();
        if fresh { pm = Pixmap::with_dpr(w, h, dpr).unwrap(); }
        pm.clear_rect(ferrokit_gui::geometry::primitives::Rect::new(0, 0, w as i32, h as i32));
        let mut p = Painter::begin(&mut pm);
        let lw = w as f32 / dpr;
        let lh = h as f32 / dpr;
        if clip {
            p.set_clip_rect(RectF::new(0.0, 0.0, lw, lh));
        }
        p.set_brush(Brush::Color(Color::from_rgba8(14, 17, 23, 240)));
        if border { p.set_pen(Pen::new(Color::from_rgba8(255, 255, 255, 36), 1.0)); } else { p.set_pen(None); }
        p.draw_rounded_rect(RectF::new(0.5, 0.5, lw - 1.0, lh - 1.0), 10.0, 10.0);
        drop(p);
        total += t.elapsed();
    }
    println!("{label:<40} {w}x{h}: {:.2} ms", total.as_secs_f64() * 1e3 / n as f64);
}
fn main() {
    for (w, h) in [(1100, 470), (1900, 1000)] {
        run("back-to-back, fresh pixmap", w, h, true, true, true, 0);
        run("paced 15 ms apart (like a drag), fresh", w, h, true, true, true, 15);
    }
}
