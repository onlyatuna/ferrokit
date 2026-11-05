// SPDX-License-Identifier: LGPL-3.0-only
//! Where `Painter::draw_text` puts DirectWrite glyphs, against real Qt 6.12 on Windows.
//!
//! `tests/data/directwrite_placement_golden.txt` holds the pixels of whole strings drawn by
//! `QPainter::drawText` into an `ARGB32_Premultiplied` image (ClearType and greyscale text, device
//! pixel ratios 1, 1.25, 1.5 and 2, black, light, coloured and translucent pens, fractional
//! origins, letter spacing), as a hash per image; `tests/data/gen_directwrite_placement_golden.py`
//! regenerates it. The font is the synthetic `tests/data/oracle_text.ttf`: its glyphs are boxes,
//! so one pixel in the wrong place or one sub-pixel offset too many shows.
//!
//! What this pins (Qt: `qpaintengine_raster.cpp` `drawCachedGlyphs`,
//! `qwindowsfontenginedirectwrite.cpp`, `qtextureglyphcache.cpp`):
//! * glyphs are blitted at `qFloor` of the pen position, at every ratio, not at its rounded value;
//! * at a ratio other than 1 the engine supports horizontal sub-pixel positions, so each glyph is
//!   rendered at the sub-pixel offset of its pen (`DWRITE_MATRIX.dx`, one of the buckets the first
//!   outlined glyph's twelve probe offsets give), for LCD masks as well as for greyscale ones.
//!
//! What it does not cover: the `kern` table check, which ferrokit does not do yet. The cases at a
//! ratio of 1 use a font without hinting (symmetric antialiasing, natural advances) and strings
//! whose advances are whole pixels; the `gasp`/`maxp` rendering mode and the advance rounding are
//! pinned by `tests/test_directwrite_modes.rs`, font simulations and the face of a font collection
//! by `tests/test_directwrite_faces.rs`.
//!
//! The ratio is process-wide state: one test walks every ratio.
#![cfg(windows)]

use ferrokit_gui::geometry::primitives::PointF;
use ferrokit_gui::paint::{Painter, Pen, Pixmap};
use ferrokit_gui::text::directwrite_face::is_available;
use ferrokit_gui::text::font::{Font, SharedFontData};
use ferrokit_gui::text::font_database::set_application_device_pixel_ratio;
use ferrokit_gui::text::smoothing::TextSmoothing;
use ferrokit_gui::tiny_skia::Color;

const GOLDEN: &str = include_str!("data/directwrite_placement_golden.txt");
const FONT: &[u8] = include_bytes!("data/oracle_text.ttf");
const TEXT_A: &str = "Hello Wor-ld 0123 AVTo";
const TEXT_B: &str = "ACEFHJKMOPRTUWY";

fn fnv1a64(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Background of the pen of the golden's `PENS` (white, or the colour that suits the pen).
fn background(pen: [u8; 4]) -> [u8; 4] {
    match pen {
        [226, 232, 240, 255] => [15, 23, 42, 255],
        [100, 160, 40, 200] => [240, 240, 200, 255],
        _ => [255, 255, 255, 255],
    }
}

fn color(c: [u8; 4]) -> Color {
    Color::from_rgba8(c[0], c[1], c[2], c[3])
}

struct Case {
    dpr: f32,
    cleartype: bool,
    px: u32,
    pen: [u8; 4],
    x_fraction: f32,
    letter_spacing: f32,
    text: &'static str,
    size: (u32, u32),
    hash: u64,
}

fn cases() -> Vec<Case> {
    GOLDEN
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|line| {
            let f: Vec<&str> = line.split(' ').collect();
            assert_eq!(f.len(), 10, "golden line {line:?}");
            let pen: Vec<u8> = f[3].split(',').map(|v| v.parse().expect("pen")).collect();
            Case {
                dpr: f[0].parse().expect("dpr"),
                cleartype: f[1] == "cleartype",
                px: f[2].parse().expect("px"),
                pen: [pen[0], pen[1], pen[2], pen[3]],
                x_fraction: f[4].parse().expect("x"),
                letter_spacing: f[5].parse().expect("ls"),
                text: if f[6] == "a" { TEXT_A } else { TEXT_B },
                size: (f[7].parse().expect("w"), f[8].parse().expect("h")),
                hash: u64::from_str_radix(f[9], 16).expect("hash"),
            }
        })
        .collect()
}

fn render(case: &Case) -> (u32, u32, u64) {
    let mut font = Font::new("FerrokitOracleText", case.px as f32)
        .with_font_data(SharedFontData::from_vec(FONT.to_vec()));
    if case.letter_spacing != 0.0 {
        font = font.with_letter_spacing(case.letter_spacing);
    }
    let mut pixmap =
        Pixmap::with_dpr(case.size.0, case.size.1, case.dpr).expect("pixmap of a golden case");
    pixmap.fill(color(background(case.pen)));
    {
        let mut painter = Painter::begin(&mut pixmap);
        painter.set_text_smoothing(TextSmoothing {
            cleartype: case.cleartype,
            gamma: 1.2,
        });
        painter.set_pen(Some(Pen::new(color(case.pen), 1.0)));
        painter.draw_text(
            PointF::new(6.0 + case.x_fraction, case.px as f32 + 6.0),
            case.text,
            &font,
        );
    }
    (
        pixmap.physical_width(),
        pixmap.physical_height(),
        fnv1a64(pixmap.data()),
    )
}

#[test]
fn whole_strings_are_pixel_identical_to_qt() {
    // DirectWrite's in-memory loader needs Windows 10 1703; on an older system the crate draws
    // with its outline backend, which these goldens do not describe.
    if !is_available() {
        return;
    }
    let mut checked = 0;
    let mut failures = Vec::new();
    for case in cases() {
        set_application_device_pixel_ratio(case.dpr);
        let (w, h, hash) = render(&case);
        // The golden stores the size of Qt's image; the pixmap of this test has the same.
        assert_eq!((w, h), case.size, "pixmap size at x{}", case.dpr);
        if hash != case.hash {
            failures.push(format!(
                "x{} {} {}px pen {:?} origin +{} spacing {}: {hash:016x}, Qt {:016x}",
                case.dpr,
                if case.cleartype {
                    "ClearType"
                } else {
                    "greyscale"
                },
                case.px,
                case.pen,
                case.x_fraction,
                case.letter_spacing,
                case.hash,
            ));
        }
        checked += 1;
    }
    set_application_device_pixel_ratio(1.0);
    assert!(checked >= 90, "only {checked} golden cases");
    assert!(
        failures.is_empty(),
        "{} of {checked} strings differ from Qt:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
