// SPDX-License-Identifier: LGPL-3.0-only
use ferrokit_gui::text::font::Font;
use ferrokit_gui::text::font_metrics::FontMetrics;
use std::time::Instant;
fn main() {
    let t0 = Instant::now();
    let mut font = Font::default();
    font.size = 12.0;
    let m = FontMetrics::from_font(&font);
    // Cold: first query loads the font database and the face.
    let t = Instant::now();
    let _ = m.horizontal_advance_exact("Claude HUD", &font);
    println!(
        "cold first call (default font): {:.1} ms",
        t.elapsed().as_secs_f64() * 1e3
    );
    for fam in ["Segoe UI", "Consolas", "Microsoft JhengHei"] {
        let mut f = font.clone();
        f.family = fam.to_string();
        let t = Instant::now();
        let _ = m.horizontal_advance_exact("Claude HUD 你好", &f);
        println!(
            "first call, family {:<20}: {:.1} ms",
            fam,
            t.elapsed().as_secs_f64() * 1e3
        );
    }
    for text in ["Claude HUD", "5h 42% resets in 2h 13m"] {
        let n = 200;
        let t = Instant::now();
        for _ in 0..n {
            std::hint::black_box(m.horizontal_advance_exact(text, &font));
        }
        println!(
            "warm {:>26}: {:.2} us/call",
            text,
            t.elapsed().as_secs_f64() * 1e6 / n as f64
        );
    }
    println!("total {:.1} ms", t0.elapsed().as_secs_f64() * 1e3);
}
