// SPDX-License-Identifier: LGPL-3.0-only
//! Whole-string advances and `drawText` pixels of real Qt 6.12.0: bidirectional text, tabs and
//! control characters.
//!
//! `tests/data/text_draw_golden.txt` is the output of `tests/data/gen_text_draw_golden.py`, run
//! once per platform (see the script for the record formats): `QFontMetricsF::horizontalAdvance`
//! (also with a `QTextOption`'s tab stop distance) and the pixels of
//! `QPainter::drawText(QPointF, text)` into an `ARGB32_Premultiplied` image, as a hash per image.
//! The strings mix Latin, Hebrew and Arabic, digits and neutrals, put tabs between and inside
//! runs of both directions, and use the explicit embedding, override and isolate characters, so
//! the itemization, the bidi reordering, the tab stops and the visual order of
//! `QPainter::drawText` (`qpainter.cpp`: `itemize`, `shapeLine`, `bidiReorder`) are all in play.
//!
//! Two kinds of font:
//! * `syn`: the synthetic `oracle_text.ttf`, 16 and 32 px, whose advances are exact in 26.6, so
//!   its advances are the same under every font backend (the generator was run on Linux with
//!   FreeType and on Windows with DirectWrite and the two files agree on every record). The
//!   advances are compared on Linux and Windows; the pixels are those of the backend of the
//!   platform.
//! * `sys`: an installed font with Arabic and Hebrew (DejaVu Sans on Linux, Arial on Windows).
//!   The record carries the size and hash of the font file and is compared only when the file of
//!   the machine is that one: the numbers describe that release of the font, and which fonts
//!   Qt falls back to is the machine's. A block that is not compared says so on stderr.
//!
//! macOS has no golden (Qt could not be run there): the test does nothing on it.
//!
//! Setting `FERROKIT_TEXT_DRAW_GOLDEN` to the output of the generator's `full` mode reports, for
//! every image that differs, how many pixels do (and `FERROKIT_TEXT_DRAW_ASCII` shows them).

use ferrokit_gui::application::LayoutDirection;
use ferrokit_gui::geometry::primitives::PointF;
use ferrokit_gui::paint::{Painter, Pen, Pixmap};
use ferrokit_gui::text::TextOption;
use ferrokit_gui::text::font::{Font, SharedFontData};
use ferrokit_gui::text::font_database::set_application_device_pixel_ratio;
use ferrokit_gui::text::font_metrics::FontMetrics;
use ferrokit_gui::text::smoothing::TextSmoothing;
use ferrokit_gui::tiny_skia::Color;
use std::collections::HashMap;

const GOLDEN: &str = include_str!("data/text_draw_golden.txt");
const SYNTHETIC_TTF: &[u8] = include_bytes!("data/oracle_text.ttf");

fn fnv1a64(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).expect("hex"))
        .collect()
}

fn unhex_text(s: &str) -> String {
    String::from_utf8(unhex(s)).expect("utf-8")
}

/// The platform letter of the records this machine compares (`None`: no golden).
fn platform() -> Option<char> {
    if cfg!(windows) {
        Some('W')
    } else if cfg!(target_os = "macos") {
        None
    } else {
        Some('L')
    }
}

/// The installed font file the `sys` records of this platform describe.
fn system_font_file() -> &'static str {
    if cfg!(windows) {
        "C:/Windows/Fonts/arial.ttf"
    } else {
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"
    }
}

struct GoldenFont {
    kind: String,
    family: String,
    px: f32,
    fingerprint: String,
}

impl GoldenFont {
    /// The ferrokit font of the record, or why this machine cannot compare it.
    fn font(&self) -> Result<Font, String> {
        if self.kind == "syn" {
            return Ok(Font::new("FerrokitOracleText", self.px)
                .with_font_data(SharedFontData::from_vec(SYNTHETIC_TTF.to_vec())));
        }
        let path = system_font_file();
        let bytes = std::fs::read(path).map_err(|e| format!("{path} is not readable ({e})"))?;
        let here = format!("{}:{:x}", bytes.len(), fnv1a64(&bytes));
        if here != self.fingerprint {
            return Err(format!(
                "{path} is {here}, the golden was measured with {}",
                self.fingerprint
            ));
        }
        Ok(Font::new(self.family.replace('_', " "), self.px))
    }
}

/// An image of the golden: its hash, or the whole image when the generator ran in `full` mode.
enum Image {
    Hash(u64),
    Pixels(Vec<u8>),
}

impl Image {
    fn parse(field: &str) -> Image {
        if let Some(hex) = field.strip_prefix('h') {
            return Image::Hash(u64::from_str_radix(hex, 16).expect("image hash"));
        }
        let mut bytes = Vec::new();
        for run in field.split(',') {
            let (px, n) = run.split_once('*').expect("run");
            let px = unhex(px);
            for _ in 0..n.parse::<usize>().expect("count") {
                bytes.extend_from_slice(&px);
            }
        }
        Image::Pixels(bytes)
    }
}

struct Draw {
    x: f32,
    y: f32,
    size: (u32, u32),
    text: String,
    image: Image,
}

/// What a font's records ask for.
#[derive(Default)]
struct Records {
    /// `(text, QFontMetricsF::horizontalAdvance)`
    advances: Vec<(String, f64)>,
    /// `(tab stop distance, text, QFontMetricsF::horizontalAdvance(text, option))`
    options: Vec<(f32, String, f64)>,
    draws: Vec<Draw>,
}

fn records() -> (HashMap<String, GoldenFont>, HashMap<String, Records>) {
    let (mut fonts, mut by_font) = (HashMap::new(), HashMap::<String, Records>::new());
    let golden = std::env::var("FERROKIT_TEXT_DRAW_GOLDEN")
        .ok()
        .map(|path| std::fs::read_to_string(path).expect("FERROKIT_TEXT_DRAW_GOLDEN"));
    for line in golden
        .as_deref()
        .unwrap_or(GOLDEN)
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
    {
        let f: Vec<&str> = line.split(' ').collect();
        if f[0] == "FONT" {
            fonts.insert(
                f[1].to_string(),
                GoldenFont {
                    kind: f[2].to_string(),
                    family: f[3].to_string(),
                    px: f[4].parse().expect("px"),
                    fingerprint: f[5].to_string(),
                },
            );
            continue;
        }
        let records = by_font.entry(f[1].to_string()).or_default();
        match f[0] {
            "A" => records
                .advances
                .push((unhex_text(f[2]), f[3].parse().expect("advance"))),
            "O" => records.options.push((
                f[2].parse().expect("tab stop"),
                unhex_text(f[3]),
                f[4].parse().expect("advance"),
            )),
            "D" => records.draws.push(Draw {
                x: f[2].parse().expect("x"),
                y: f[3].parse().expect("y"),
                size: (f[4].parse().expect("w"), f[5].parse().expect("h")),
                text: unhex_text(f[6]),
                image: Image::parse(f[7]),
            }),
            other => panic!("golden record {other:?}"),
        }
    }
    (fonts, by_font)
}

/// `QPainter::drawText` of `draw.text` with ferrokit into an image of the golden's size.
fn render(draw: &Draw, font: &Font, dpr: f32) -> Vec<u8> {
    // `Pixmap::with_dpr` takes the size in device pixels; the golden's is logical.
    let (w, h) = (
        (draw.size.0 as f32 * dpr).round() as u32,
        (draw.size.1 as f32 * dpr).round() as u32,
    );
    let mut pixmap = Pixmap::with_dpr(w, h, dpr).expect("pixmap");
    pixmap.fill(Color::WHITE);
    {
        let mut painter = Painter::begin(&mut pixmap);
        painter.set_text_smoothing(TextSmoothing {
            cleartype: false,
            gamma: 1.2,
        });
        painter.set_pen(Some(Pen::new(Color::BLACK, 1.0)));
        painter.draw_text(PointF::new(draw.x, draw.y), &draw.text, font);
    }
    pixmap.data().to_vec()
}

/// The red channel of `bytes` (RGBA, `width` pixels per row) as text, to see where two images
/// differ (`FERROKIT_TEXT_DRAW_ASCII` set prints the draws that differ, when the golden has the
/// whole images).
fn ascii(bytes: &[u8], width: usize) -> String {
    bytes
        .chunks(4 * width)
        .map(|row| {
            row.chunks(4)
                .map(|px| b" .:-=+*#%@"[9 - usize::from(px[0]) * 9 / 255] as char)
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// How many pixels of `got` differ from `want` (as a number of pixels, not bytes).
fn differing_pixels(got: &[u8], want: &[u8]) -> usize {
    got.chunks(4)
        .zip(want.chunks(4))
        .filter(|(a, b)| a != b)
        .count()
}

fn printable(text: &str) -> String {
    text.escape_default().to_string()
}

/// Characters that are never shown (default ignorables of Qt: the soft hyphen, joiners, direction
/// marks, embedding, override and isolate controls, the byte order mark).
fn is_hidden(c: char) -> bool {
    matches!(c, '\u{ad}' | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{2069}' | '\u{feff}')
}

#[test]
fn advances_and_pixels_equal_qts() {
    let Some(platform) = platform() else {
        eprintln!("skipping: no golden for this platform (Qt was not measured on it)");
        return;
    };
    // Windows: the golden was measured at a device pixel ratio of 2, which selects Qt's
    // DirectWrite engine with its design advances (see the generator).
    let dpr = if cfg!(windows) { 2.0 } else { 1.0 };
    set_application_device_pixel_ratio(dpr);
    let (fonts, mut by_font) = records();
    let mut ids: Vec<&String> = fonts.keys().filter(|id| id.starts_with(platform)).collect();
    ids.sort();
    let mut report = Vec::new();
    let (mut advances, mut images) = (0, 0);
    let system_font = fonts
        .iter()
        .filter(|(id, f)| id.starts_with(platform) && f.kind == "sys")
        .map(|(_, f)| f.font())
        .next();
    let system_font_ok = matches!(system_font, Some(Ok(_)));
    let system_font_why = match system_font {
        Some(Err(why)) => why,
        _ => "no installed font record".to_string(),
    };
    for id in ids {
        let golden_font = &fonts[id];
        let font = match golden_font.font() {
            Ok(font) => font,
            Err(why) => {
                eprintln!("skipping the records of {id}: {why}");
                continue;
            }
        };
        let records = by_font.remove(id).unwrap_or_default();
        let metrics = FontMetrics::from_font(&font);
        for (text, want) in &records.advances {
            let got = f64::from(metrics.horizontal_advance_exact(text, &font));
            advances += 1;
            if got != *want {
                report.push(format!(
                    "{id} advance of {:?}: {got}, Qt {want}",
                    printable(text)
                ));
            }
        }
        for (tab_stop, text, want) in &records.options {
            let option = TextOption {
                text_direction: LayoutDirection::LayoutDirectionAuto,
                tab_stop_distance: *tab_stop,
            };
            let got = f64::from(metrics.horizontal_advance_with_option(text, &font, &option));
            advances += 1;
            if got != *want {
                report.push(format!(
                    "{id} advance of {:?} with tab stops of {tab_stop}: {got}, Qt {want}",
                    printable(text)
                ));
            }
        }
        for draw in &records.draws {
            // A character the synthetic font lacks and that is never shown is drawn in the glyph
            // of a fallback font (blank in the measured DejaVu Sans and Tahoma); without the
            // measured machine's fonts the fallback is another one.
            if golden_font.kind == "syn" && draw.text.chars().any(is_hidden) && !system_font_ok {
                eprintln!(
                    "skipping the draw of {:?}: {system_font_why}",
                    printable(&draw.text)
                );
                continue;
            }
            let got = render(draw, &font, dpr);
            images += 1;
            let wrong = match &draw.image {
                Image::Hash(hash) => (fnv1a64(&got) != *hash).then(|| "differs".to_string()),
                Image::Pixels(want) => {
                    let n = differing_pixels(&got, want);
                    (n != 0).then(|| format!("{n} pixels differ"))
                }
            };
            if let Some(wrong) = wrong {
                report.push(format!(
                    "{id} drawText at ({}, {}) of {:?}: {wrong}",
                    draw.x,
                    draw.y,
                    printable(&draw.text)
                ));
                if let (Image::Pixels(want), Some(_)) =
                    (&draw.image, std::env::var_os("FERROKIT_TEXT_DRAW_ASCII"))
                {
                    let width = (draw.size.0 as f32 * dpr).round() as usize;
                    eprintln!(
                        "{id} {:?} at {}:\nferrokit:\n{}\nQt:\n{}",
                        printable(&draw.text),
                        draw.x,
                        ascii(&got, width),
                        ascii(want, width)
                    );
                }
            }
        }
    }
    set_application_device_pixel_ratio(1.0);
    eprintln!(
        "{advances} advances and {images} images compared, {} differ",
        report.len()
    );
    assert!(
        report.is_empty(),
        "{} of {} measurements differ from Qt:\n{}",
        report.len(),
        advances + images,
        report.join("\n")
    );
}
