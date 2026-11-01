// SPDX-License-Identifier: LGPL-3.0-only
//! fontdue (reference) vs OutlineFace (ttf-parser + exact-coverage rasterizer): metrics, pixel
//! differences, cost, memory.
use ferrokit_gui::text::font::SharedFontData;
use ferrokit_gui::text::glyph_face::{GlyphFace, GlyphMetrics};
use ferrokit_gui::text::outline_face::OutlineFace;
use std::time::Instant;

/// The reference rasterizer behind the same interface (fontdue is a dev-dependency only).
struct Fontdue(fontdue::Font);
impl GlyphFace for Fontdue {
    fn glyph_index(&self, ch: char) -> u16 {
        self.0.lookup_glyph_index(ch)
    }
    fn metrics_indexed(&self, glyph_id: u16, px: f32) -> GlyphMetrics {
        let m = self.0.metrics_indexed(glyph_id, px);
        GlyphMetrics {
            xmin: m.xmin,
            ymin: m.ymin,
            width: m.width,
            height: m.height,
            advance_width: m.advance_width,
            advance_height: m.advance_height,
        }
    }
    fn rasterize_indexed(&self, glyph_id: u16, px: f32) -> (GlyphMetrics, Vec<u8>) {
        let (m, b) = self.0.rasterize_indexed(glyph_id, px);
        (
            GlyphMetrics {
                xmin: m.xmin,
                ymin: m.ymin,
                width: m.width,
                height: m.height,
                advance_width: m.advance_width,
                advance_height: m.advance_height,
            },
            b,
        )
    }
}

#[cfg(windows)]
#[repr(C)]
struct Pmc {
    cb: u32,
    page_faults: u32,
    peak_ws: usize,
    ws: usize,
    qpp: usize,
    qp: usize,
    qpnp: usize,
    qnp: usize,
    pagefile: usize,
    peak_pagefile: usize,
}
#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetCurrentProcess() -> isize;
    fn K32GetProcessMemoryInfo(p: isize, c: *mut Pmc, cb: u32) -> i32;
}
#[cfg(windows)]
fn commit_mb() -> f64 {
    // SAFETY: `Pmc` consists only of integers, for which the all-zero bit pattern is valid.
    let mut m: Pmc = unsafe { std::mem::zeroed() };
    m.cb = std::mem::size_of::<Pmc>() as u32;
    // SAFETY: `GetCurrentProcess` takes no input and returns the pseudo-handle of this process,
    // which needs no closing. `&mut m` points to a live `Pmc`, a `#[repr(C)]` mirror of
    // `PROCESS_MEMORY_COUNTERS` (two `u32` then eight `usize`), and `m.cb` is its size as the
    // API requires, so the callee writes only inside `m`.
    unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut m, m.cb) };
    m.pagefile as f64 / 1048576.0
}
/// Non-Windows: resident set from /proc/self/statm (no commit equivalent).
#[cfg(not(windows))]
fn commit_mb() -> f64 {
    let statm = std::fs::read_to_string("/proc/self/statm").unwrap_or_default();
    let pages: f64 = statm
        .split_whitespace()
        .nth(1)
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.0);
    pages * 4096.0 / 1048576.0
}

const FONTS: &[(&str, &str)] = &[
    ("Segoe UI", "segoeui.ttf"),
    ("Consolas", "consola.ttf"),
    ("Microsoft JhengHei", "msjh.ttc"),
    ("Segoe UI Symbol", "seguisym.ttf"),
];
const TEXT: &str = "Claude HUD Monitor 5h 42% 7d 18% resets in 2h 13m 0123456789 $1.28 12,345 tokens Opus Sonnet Refresh All 重設於: -- 立即重新整理所有 AI 視窗透明度 更新頻率 開機自動啟動 結束程式 ✔ ⏱ ⚠";
const SIZES: &[f32] = &[11.0, 12.0, 15.0, 16.25, 18.75, 24.0];

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let only_mem = args.get(1).map(|s| s.as_str());
    println!("baseline commit {:.1} MB", commit_mb());
    for (name, file) in FONTS {
        if only_mem.is_some_and(|f| f != *file) {
            continue;
        }
        let path = format!("C:/Windows/Fonts/{file}");
        let Ok(bytes) = std::fs::read(&path) else {
            println!("{name}: not installed");
            continue;
        };
        let data = SharedFontData::from_vec(bytes);
        let before = commit_mb();
        let outline = OutlineFace::new(data.clone(), 0).expect("outline");
        let after_outline = commit_mb();
        let fd = Fontdue(
            fontdue::Font::from_bytes(data.as_slice(), fontdue::FontSettings::default())
                .expect("fontdue"),
        );
        let after_fd = commit_mb();
        println!(
            "\n== {name} ({} KB file)  commit: outline +{:.1} MB, fontdue +{:.1} MB",
            data.len() / 1024,
            after_outline - before,
            after_fd - after_outline
        );

        let chars: Vec<char> = {
            let mut v: Vec<char> = TEXT.chars().filter(|c| !c.is_whitespace()).collect();
            v.sort();
            v.dedup();
            v
        };
        let (mut n, mut gid_diff, mut metric_diff, mut size_diff) = (0, 0, 0, 0);
        let (mut px_total, mut px_diff, mut sum_abs, mut max_abs) = (0u64, 0u64, 0u64, 0i32);
        let mut worst = (0.0f32, ' ', 0i32);
        let mut adv_max = 0.0f32;
        for &ch in &chars {
            let ga = fd.glyph_index(ch);
            let gb = outline.glyph_index(ch);
            if ga != gb {
                gid_diff += 1;
            }
            if ga == 0 {
                continue;
            }
            for &px in SIZES {
                let (ma, ba) = fd.rasterize_indexed(ga, px);
                let (mb, bb) = outline.rasterize_indexed(ga, px);
                n += 1;
                adv_max = adv_max.max((ma.advance_width - mb.advance_width).abs());
                if (ma.xmin, ma.ymin) != (mb.xmin, mb.ymin) {
                    metric_diff += 1;
                }
                if (ma.width, ma.height) != (mb.width, mb.height) {
                    size_diff += 1;
                    continue;
                }
                for (a, b) in ba.iter().zip(bb.iter()) {
                    let d = (*a as i32 - *b as i32).abs();
                    px_total += 1;
                    if d > 0 {
                        px_diff += 1;
                    }
                    sum_abs += d as u64;
                    if d > max_abs {
                        max_abs = d;
                    }
                }
                let glyph_sum: i32 = ba
                    .iter()
                    .zip(bb.iter())
                    .map(|(a, b)| (*a as i32 - *b as i32).abs())
                    .sum();
                let mean = glyph_sum as f32 / ba.len().max(1) as f32;
                if mean > worst.0 {
                    worst = (mean, ch, px as i32);
                }
            }
        }
        println!(
            "glyphs x sizes {n}: glyph-id mismatches {gid_diff}, origin (xmin,ymin) mismatches {metric_diff}, bitmap size mismatches {size_diff}, max advance diff {adv_max:.4}"
        );
        println!(
            "pixels compared {px_total}: differing {} ({:.1}%), mean |diff| {:.2}/255, max |diff| {max_abs}; worst glyph {:?}@{}px mean {:.1}",
            px_diff,
            100.0 * px_diff as f64 / px_total.max(1) as f64,
            sum_abs as f64 / px_total.max(1) as f64,
            worst.1,
            worst.2,
            worst.0
        );

        // cost per glyph (cold = first rasterization, no cache)
        let gids: Vec<u16> = chars
            .iter()
            .map(|&c| fd.glyph_index(c))
            .filter(|&g| g != 0)
            .collect();
        for (label, face) in [
            ("fontdue", &fd as &dyn GlyphFace),
            ("outline", &outline as &dyn GlyphFace),
        ] {
            let reps = 200;
            let t = Instant::now();
            let mut cnt = 0u64;
            for _ in 0..reps {
                for &g in &gids {
                    std::hint::black_box(face.rasterize_indexed(g, 15.0));
                    cnt += 1;
                }
            }
            let rast = t.elapsed().as_secs_f64() * 1e6 / cnt as f64;
            let t = Instant::now();
            let mut cnt = 0u64;
            for _ in 0..reps {
                for &c in &chars {
                    std::hint::black_box(face.metrics(c, 15.0));
                    cnt += 1;
                }
            }
            let met = t.elapsed().as_secs_f64() * 1e6 / cnt as f64;
            println!("  {label}: rasterize {rast:.2} us/glyph, metrics(char) {met:.3} us");
        }
    }
    println!("\nfinal commit {:.1} MB", commit_mb());
}
