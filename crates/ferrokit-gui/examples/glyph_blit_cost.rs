// SPDX-License-Identifier: LGPL-3.0-only
//! Cost and heap allocations of drawing a frame of text through every glyph blit path of
//! `Painter::draw_glyph_run`: the source-over fast path, and the `composite_masked` path that
//! non-source-over composition modes take (grey masks, LCD masks and colour emoji).
//!
//! Run in release mode: `cargo run --release -p ferrokit-gui --example glyph_blit_cost`.
//! Per case it prints the median and minimum time of a frame over 15 runs of 40 frames, the heap allocations and bytes
//! of one frame (a counting global allocator), and an FNV-1a checksum of the resulting pixels, so
//! a change to the blit code can be checked to leave the pixels alone. Shaping allocates too and is
//! the same in every case; the source-over rows are the floor the other rows are read against.
use ferrokit_gui::geometry::primitives::{PointF, RectF};
use ferrokit_gui::paint::{CompositionMode, Painter, Pen, Pixmap};
use ferrokit_gui::text::font::Font;
use ferrokit_gui::text::font_database::resolve_font_engines_for_text_global;
use ferrokit_gui::text::glyph_layout::GlyphLayout;
use ferrokit_gui::text::smoothing::TextSmoothing;
use ferrokit_gui::tiny_skia::Color;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

struct Counting;

static ALLOCS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);

// SAFETY: every method forwards to `System` unchanged; the counters are only bookkeeping.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        System.alloc(layout)
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        System.alloc_zeroed(layout)
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout)
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(new_size as u64, Ordering::Relaxed);
        System.realloc(ptr, layout, new_size)
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

const TEXT: &[&str] = &[
    "Claude HUD Monitor",
    "5h 42%  resets in 2h 13m",
    "7d 18%  resets in 4d 02h",
    "Opus  31%   Sonnet  12%",
    "Codex  68%   18:45",
    "12,345 tokens   $1.28",
    "0123456789 % Session  Weekly  Extra",
];

const EMOJI: &[&str] = &[
    "\u{1F600} \u{1F525} \u{2728}",
    "ok \u{1F680} done \u{1F44D}",
];

fn font() -> Font {
    let mut f = Font::default();
    f.family = "Segoe UI, Microsoft JhengHei, Segoe UI Emoji".to_string();
    f.size = 12.0;
    f
}

fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
        (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

struct Case {
    label: &'static str,
    lines: &'static [&'static str],
    mode: CompositionMode,
    lcd: bool,
    clipped: bool,
}

fn draw_frame(pm: &mut Pixmap, f: &Font, case: &Case) {
    let mut p = Painter::begin(pm);
    p.set_text_smoothing(if case.lcd {
        TextSmoothing {
            cleartype: true,
            gamma: 1.4,
        }
    } else {
        TextSmoothing::OFF
    });
    p.set_pen(Some(Pen::new(Color::from_rgba8(226, 232, 240, 255), 1.0)));
    p.set_composition_mode(case.mode);
    if case.clipped {
        // Cuts through glyphs on every side.
        p.set_clip_rect(RectF::new(13.0, 9.0, 150.0, 100.0));
    }
    for (i, l) in case.lines.iter().enumerate() {
        p.draw_text(PointF::new(10.0, 16.0 + 20.0 * i as f32), l, f);
    }
}

fn backdrop(dpr: f32) -> Pixmap {
    let mut pm = Pixmap::with_dpr(420, 180, dpr).unwrap();
    pm.fill(Color::from_rgba8(22, 25, 32, 255));
    pm
}

fn main() {
    let dpr = 1.25f32;
    let f = font();
    let glyphs_of = |lines: &[&str]| -> usize {
        lines
            .iter()
            .map(|l| {
                let engines = resolve_font_engines_for_text_global(&f, l);
                GlyphLayout::shape_with_engines(l, &f, &engines)
                    .glyphs
                    .len()
            })
            .sum()
    };
    let text_glyphs = glyphs_of(TEXT);
    let emoji_glyphs = glyphs_of(EMOJI);
    println!("text glyphs/frame {text_glyphs}, emoji glyphs/frame {emoji_glyphs}, dpr {dpr}");

    use CompositionMode::*;
    let cases = [
        Case {
            label: "text grey   SourceOver",
            lines: TEXT,
            mode: SourceOver,
            lcd: false,
            clipped: false,
        },
        Case {
            label: "text LCD    SourceOver",
            lines: TEXT,
            mode: SourceOver,
            lcd: true,
            clipped: false,
        },
        Case {
            label: "text grey   Plus",
            lines: TEXT,
            mode: Plus,
            lcd: false,
            clipped: false,
        },
        Case {
            label: "text grey   Multiply",
            lines: TEXT,
            mode: Multiply,
            lcd: false,
            clipped: false,
        },
        Case {
            label: "text grey   DestinationOver",
            lines: TEXT,
            mode: DestinationOver,
            lcd: false,
            clipped: false,
        },
        Case {
            label: "text LCD    Plus",
            lines: TEXT,
            mode: Plus,
            lcd: true,
            clipped: false,
        },
        Case {
            label: "text grey   Plus, clipped",
            lines: TEXT,
            mode: Plus,
            lcd: false,
            clipped: true,
        },
        Case {
            label: "emoji       SourceOver",
            lines: EMOJI,
            mode: SourceOver,
            lcd: false,
            clipped: false,
        },
        Case {
            label: "emoji       Plus",
            lines: EMOJI,
            mode: Plus,
            lcd: false,
            clipped: false,
        },
        Case {
            label: "emoji       Multiply, clipped",
            lines: EMOJI,
            mode: Multiply,
            lcd: false,
            clipped: true,
        },
    ];

    println!(
        "{:<30} {:>10} {:>10} {:>12} {:>10}  {:<16}",
        "case", "median ms", "min ms", "allocs/frame", "KiB/frame", "pixel fnv1a"
    );
    for case in &cases {
        let mut pm = backdrop(dpr);
        for _ in 0..5 {
            draw_frame(&mut pm, &f, case);
        }

        // Allocations of one frame on a warm cache, drawn onto the backdrop so the checksum is
        // independent of how often the frame was drawn before.
        let mut fresh = backdrop(dpr);
        let (a0, b0) = (
            ALLOCS.load(Ordering::Relaxed),
            BYTES.load(Ordering::Relaxed),
        );
        draw_frame(&mut fresh, &f, case);
        let allocs = ALLOCS.load(Ordering::Relaxed) - a0;
        let bytes = BYTES.load(Ordering::Relaxed) - b0;
        let hash = fnv(fresh.data());

        let n = 40;
        let mut runs: Vec<f64> = (0..15)
            .map(|_| {
                let t = Instant::now();
                for _ in 0..n {
                    draw_frame(&mut pm, &f, case);
                }
                t.elapsed().as_secs_f64() * 1e3 / n as f64
            })
            .collect();
        runs.sort_by(|a, b| a.partial_cmp(b).unwrap());
        println!(
            "{:<30} {:>10.3} {:>10.3} {:>12} {:>10.1}  {:016x}",
            case.label,
            runs[runs.len() / 2],
            runs[0],
            allocs,
            bytes as f64 / 1024.0,
            hash
        );
    }
}
