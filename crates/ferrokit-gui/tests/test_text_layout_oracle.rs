// SPDX-License-Identifier: LGPL-3.0-only
//! `TextLayout` / `TextLine` (`QTextLayout` / `QTextLine`) against Qt 6.11.2.
//!
//! `tests/data/text_layout_oracle.txt` holds measurements of PySide6 6.11.2's real `QTextLayout`
//! (regenerate it with `tests/data/gen_text_layout_oracle.py`, which also writes the font below):
//! `QTextLine::cursorToX` at every grapheme boundary of one unwrapped line, and the lines
//! (`textStart`, `textLength`, `naturalTextWidth`, `cursorToX` at the line end) of text wrapped at
//! several widths.
//!
//! `naturalTextWidth` is the sum of the advances plus the negative right bearing of the last glyph,
//! the ink that hangs over it; the `syn_bearing_*` and `arial_bearing_*` cases pin where Qt counts
//! it (the width of a line, whether a word fits, the end of a line a separator or a wrap ends, not
//! the end of a text that ends in white space) and where it does not (`cursorToX`, the alignment
//! of the line, which go by the advances).
//!
//! Two fonts:
//! * `tests/data/oracle_text.ttf`, a synthetic font with kerning, a `liga` ligature with a GDEF
//!   caret, combining marks, Hebrew letters and glyphs whose ink hangs over their advance (the
//!   bearings of 1.5, 0.375 and 1/64 px), built by the generator. Its units per em is 1024
//!   and the cases use 16 px, so every design value is a multiple of 1/64 px: DirectWrite's
//!   truncation to 26.6 changes nothing and the expected numbers are the same under every font
//!   backend. These tests run on every platform.
//! * the installed Arial, with the system's emoji and Hebrew fallbacks: Windows only, because the
//!   numbers were measured with Windows' Arial and DirectWrite and the font on a Linux runner is a
//!   different release. A missing font fails the test; it does not skip it.
//!
//! The fixture is measured with Qt's DirectWrite engine (device pixel ratio other than 1), so the
//! tests set ferrokit's application ratio to 2 (the ratio changes nothing but the engine).
//!
//! # Tolerance
//! A deviation larger than `TOLERANCE` (1/128 px) is a divergence. The synthetic font's values are
//! exact in 26.6, so any honest port agrees to `f32` rounding (the sums reach 120 px: 4e-6 px);
//! half of the 1/64 px quantum of Qt's DirectWrite advances is the smallest tolerance that cannot
//! be hit by a legitimate rounding choice and is still 96 times smaller than the smallest kerning
//! pair (`space A`, 0.75 px; the 2 px of `AV` is 256 times). It is applied to every case alike; no case has a looser
//! one. A case that does diverge is listed in `KNOWN_DIVERGENCES` with the reason and the test
//! asserts it still diverges, so fixing ferrokit makes the entry fail until it is removed.
//!
//! Text positions are compared in UTF-16 code units, as Qt reports them.

use ferrokit_gui::alignment::Alignment;
use ferrokit_gui::text::font::{Font, SharedFontData};
use ferrokit_gui::text::font_database::set_application_device_pixel_ratio;
#[cfg(windows)]
use ferrokit_gui::text::font_database::with_global_font_database;
use ferrokit_gui::text::layout::{TextLayout, WrapMode};
use ferrokit_gui::text::{CursorPosition, Edge};
use std::collections::BTreeMap;
use unicode_segmentation::UnicodeSegmentation;

const ORACLE: &str = include_str!("data/text_layout_oracle.txt");
const SYNTHETIC_TTF: &[u8] = include_bytes!("data/oracle_text.ttf");

/// See the module documentation.
const TOLERANCE: f64 = 1.0 / 128.0;

/// Cases where ferrokit differs from Qt: `(key, reason, bound)`. A key is `cursor/<case>`,
/// `cursor-trailing/<case>`, `hit/<case>`, `breaks/<case>/<width>`, `endx/<case>/<width>`, `natural/<case>/<width>` or
/// `align/<case>/<align>/<width>`; an entry ending in `/` stands for every key that
/// starts with it, and all of them must diverge. The cursor and natural-width entries are
/// differences of values; the `breaks` entries are different lines.
///
/// `bound` is the largest deviation, in px, the entry accepts: a difference of values that
/// grows past it fails, and so does one that turns into different lines or positions. `None`
/// is for entries that are about different positions (`hit/`). Every entry must name at least
/// one case (`every_known_divergence_names_at_least_one_case`), so a misspelt or stale key
/// cannot keep a test green.
const KNOWN_DIVERGENCES: &[(&str, &str, Option<f64>)] = &[
    // --- tabs ---
    (
        "cursor-trailing/syn_tab",
        "Qt's trailing edge of a tab is where the tab starts (its glyph has no advance of its \
         own); ferrokit's is the end of the tab, as for any other grapheme",
        Some(72.5),
    ),
    (
        "cursor-trailing/syn_tab_leading",
        "as cursor-trailing/syn_tab",
        Some(80.0),
    ),
    (
        "cursor-trailing/syn_tab_two",
        "as cursor-trailing/syn_tab",
        Some(80.0),
    ),
    (
        "cursor-trailing/syn_tab_long",
        "as cursor-trailing/syn_tab",
        Some(70.0),
    ),
    (
        "cursor-trailing/syn_tab_stop_20",
        "as cursor-trailing/syn_tab",
        Some(17.5),
    ),
    (
        "cursor-trailing/syn_tab_on_stop",
        "as cursor-trailing/syn_tab",
        Some(15.0),
    ),
    (
        "cursor-trailing/syn_tab_fraction",
        "as cursor-trailing/syn_tab",
        Some(14.8),
    ),
    (
        "cursor-trailing/syn_tab_small_gap",
        "as cursor-trailing/syn_tab",
        Some(1.0),
    ),
    (
        "cursor-trailing/syn_tab_crlf",
        "as cursor-trailing/syn_tab",
        Some(72.5),
    ),
    (
        "cursor-trailing/arial_tab",
        "as cursor-trailing/syn_tab",
        Some(62.3),
    ),
    (
        "cursor-trailing/syn_tab_rounding",
        "as cursor-trailing/syn_tab",
        Some(50.5),
    ),
    (
        "cursor-trailing/syn_tab_rtl",
        "as cursor-trailing/syn_tab",
        Some(65.0),
    ),
    // --- cursor_to_x ---
    (
        "cursor/syn_liga_ffi",
        "Qt divides a ligature over its characters in 26.6 fixed point, `floor(advance / n)` per \
         character (the three-letter `ffi`, 15.625 px: 5.203125 px each); ferrokit divides in f32 \
         (5.2083). 0.0104 px at most, and only for ligatures of 3+ characters",
        Some(0.011),
    ),
    (
        "cursor/syn_liga_run",
        "same ligature division as syn_liga_ffi",
        Some(0.011),
    ),
    // --- ligatures ---
    (
        "cursor-trailing/syn_liga_fi",
        "the trailing edge of a grapheme inside a ligature is the end of the whole glyph in Qt \
         (`cursorToX(.., Trailing)` moves to the next glyph cluster); ferrokit's is the end of \
         the grapheme, the ligature divided evenly",
        Some(5.5),
    ),
    (
        "cursor-trailing/syn_liga_ffi",
        "as cursor-trailing/syn_liga_fi",
        Some(10.5),
    ),
    (
        "cursor-trailing/syn_liga_fl_caret",
        "as cursor-trailing/syn_liga_fi",
        Some(6.0),
    ),
    (
        "cursor-trailing/syn_liga_run",
        "as cursor-trailing/syn_liga_fi",
        Some(10.5),
    ),
    (
        "hit/syn_liga_run",
        "`CursorOnCharacter` inside a ligature is the start of the glyph in Qt, the grapheme \
         under the point in ferrokit",
        None,
    ),
    (
        "hit/syn_combining_acute",
        "over the first letter of `a\u{301}e\u{301}b` Qt answers the end of the text (5) for \
         `CursorBetweenCharacters`; ferrokit the end of that letter's grapheme. [INFERENCE: Qt's \
         `positionInLigature` with a two character cluster]",
        None,
    ),
    (
        "cursor/arial_arabic_lam_alef",
        "inside the Arabic lam-alef glyph Qt puts the cursor at the glyph's end where it divides \
         left-to-right ligatures; ferrokit divides it evenly. [INFERENCE: Qt's right to left \
         branch of `cursorToX`]",
        Some(4.4),
    ),
    (
        "cursor-trailing/arial_arabic_lam_alef",
        "as cursor/arial_arabic_lam_alef, for the trailing edge",
        Some(17.6),
    ),
    // --- bidi ---
    (
        "align/syn_align_rtl/",
        "when a line starts with a run of the opposite direction `cursorToX` of its first \
         position takes the neighbouring item from the previous line and returns a position \
         that depends on that unrelated item (the line width, clamped, or a sum of widths); \
         ferrokit keeps the cursor on the run",
        Some(40.0),
    ),
    (
        "hit/syn_hitwrap_rtl_base/45/line0",
        "over the space that hangs at the end of a wrapped right to left line `xToCursor` \
         answers the position after the space in Qt; ferrokit the one before it, as for any \
         other right to left grapheme halfway. [INFERENCE]",
        None,
    ),
    (
        "hit/syn_hitwrap_rtl_base/45/line1",
        "as hit/syn_hitwrap_rtl_base/45/line0",
        None,
    ),
];

fn hex_decode(s: &str) -> String {
    let bytes: Vec<u8> = (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).expect("hex digit pair"))
        .collect();
    String::from_utf8(bytes).expect("oracle text is UTF-8")
}

fn wrap_mode(name: &str) -> WrapMode {
    match name {
        "nowrap" => WrapMode::NoWrap,
        "word" => WrapMode::WordWrap,
        "anywhere" => WrapMode::WrapAnywhere,
        "word-or-anywhere" => WrapMode::WrapAtWordBoundaryOrAnywhere,
        other => panic!("unknown wrap mode {other} in the oracle"),
    }
}

/// What the generator recorded for one font line.
struct FontSpec {
    /// `true` for the font file shipped with the tests, `false` for an installed family.
    embedded: bool,
    px: f32,
    family: String,
}

fn font_specs() -> BTreeMap<&'static str, FontSpec> {
    let mut out = BTreeMap::new();
    for line in ORACLE.lines().filter(|l| l.starts_with("font ")) {
        let t: Vec<&str> = line.split(' ').collect();
        let id: &'static str = match t[1] {
            "syn" => "syn",
            "arial" => "arial",
            other => panic!("unknown font id {other} in the oracle"),
        };
        out.insert(
            id,
            FontSpec {
                embedded: t[2] == "file",
                px: t[3].parse().expect("pixel size"),
                family: t[4..].join(" "),
            },
        );
    }
    out
}

/// The tab stop distance of the cases `gen_text_layout_oracle.py` lays out with one
/// (`TAB_STOPS` there); the others use the default.
const TAB_STOPS: &[(&str, f32)] = &[
    ("syn_tab_stop_20", 20.0),
    ("syn_tab_on_stop", 15.0),
    ("syn_tab_fraction", 14.9),
    ("syn_tab_small_gap", 16.0),
    ("syn_tab_rounding", 50.0),
    ("syn_tab_wrap_20", 20.0),
    ("syn_tab_wrap_20_leading", 20.0),
    ("syn_tab_wrap_20_space", 20.0),
    ("syn_tab_wrap_20_after_space", 20.0),
    ("syn_tab_wrap_20_word", 20.0),
];

fn set_tab_stop(layout: &mut TextLayout, case: &str) {
    if let Some((_, distance)) = TAB_STOPS.iter().find(|(name, _)| *name == case) {
        layout.set_tab_stop_distance(*distance);
    }
}

/// The ferrokit font for an oracle font id. Fails (never skips) when an installed font is missing.
fn font_for(id: &str) -> Font {
    let specs = font_specs();
    let spec = specs
        .get(id)
        .unwrap_or_else(|| panic!("font {id} is not in the oracle"));
    if spec.embedded {
        return Font::new(spec.family.clone(), spec.px)
            .with_font_data(SharedFontData::from_vec(SYNTHETIC_TTF.to_vec()));
    }
    #[cfg(windows)]
    {
        let found = with_global_font_database(|db| db.has_family(&spec.family));
        assert!(
            found,
            "the oracle needs the installed font {:?} (C:/Windows/Fonts); it is missing, so these \
             cases cannot run. Install it; do not skip the test",
            spec.family
        );
    }
    Font::new(spec.family.clone(), spec.px)
}

/// UTF-16 offset of the start of every grapheme of `text`, then of its end.
fn grapheme_offsets16(text: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut at = 0;
    for g in text.graphemes(true) {
        out.push(at);
        at += g.encode_utf16().count();
    }
    out.push(at);
    out
}

/// One measured quantity: how far ferrokit is from Qt, or `Err` when they cannot be compared
/// (different line breaks or cursor positions).
#[derive(Debug, Clone, PartialEq)]
enum Outcome {
    Dev(f64),
    Mismatch(String),
}

impl Outcome {
    fn agrees(&self) -> bool {
        matches!(self, Outcome::Dev(d) if *d <= TOLERANCE)
    }
}

/// Every comparison of a run: `(key, outcome)`, in file order.
type Report = Vec<(String, Outcome)>;

fn max_dev(pairs: impl Iterator<Item = (f64, f64)>) -> f64 {
    pairs.map(|(a, b)| (a - b).abs()).fold(0.0, f64::max)
}

fn check_cursor(report: &mut Report, tokens: &[&str]) {
    let (case, font_id, text, n) = (tokens[1], tokens[2], hex_decode(tokens[3]), tokens[4]);
    let n: usize = n.parse().unwrap();
    // (UTF-16 offset, x of its leading edge, x of its trailing edge)
    let qt: Vec<(usize, f64, f64)> = tokens[5..5 + n]
        .iter()
        .map(|p| {
            let f: Vec<&str> = p.split(':').collect();
            (
                f[0].parse().unwrap(),
                f[1].parse().unwrap(),
                f[2].parse().unwrap(),
            )
        })
        .collect();

    let mut layout = TextLayout::new(text.clone(), font_for(font_id));
    set_tab_stop(&mut layout, case);
    layout.do_layout();
    let key = format!("cursor/{case}");
    if layout.line_count() != 1 {
        report.push((
            key,
            Outcome::Mismatch(format!("{} lines, Qt has 1", layout.line_count())),
        ));
        return;
    }
    let line = layout.line_at(0).unwrap();
    let offs16 = grapheme_offsets16(&text);
    let ours: Vec<usize> = offs16.clone();
    let theirs: Vec<usize> = qt.iter().map(|(o, _, _)| *o).collect();
    if ours != theirs {
        report.push((
            key,
            Outcome::Mismatch(format!("cursor positions {ours:?}, Qt has {theirs:?}")),
        ));
        return;
    }
    for (edge, name, pick) in [
        (Edge::Leading, "cursor", 1usize),
        (Edge::Trailing, "cursor-trailing", 2),
    ] {
        let pairs: Vec<(f64, f64)> = qt
            .iter()
            .enumerate()
            .map(|(g, q)| {
                let want = if pick == 1 { q.1 } else { q.2 };
                (f64::from(line.cursor_to_x(g, edge)), want)
            })
            .collect();
        let dev = max_dev(pairs.iter().copied());
        let key = format!("{name}/{case}");
        if dev > TOLERANCE {
            let ours: Vec<f64> = pairs.iter().map(|p| p.0).collect();
            let theirs: Vec<f64> = pairs.iter().map(|p| p.1).collect();
            println!(
                "{key}: x at each cursor position\n  ferrokit {ours:?}\n  Qt       {theirs:?}"
            );
        }
        report.push((key, Outcome::Dev(dev)));
    }
}

/// `xToCursor` along an unwrapped line: the cursor position at each sampled x, in both modes.
fn check_hit(report: &mut Report, tokens: &[&str]) {
    let (case, font_id, text) = (tokens[1], tokens[2], hex_decode(tokens[3]));
    let n: usize = tokens[4].parse().unwrap();
    // (x, between characters, on character), positions in UTF-16 units
    let qt: Vec<(f32, usize, usize)> = tokens[5..5 + n]
        .iter()
        .map(|p| {
            let f: Vec<&str> = p.split(':').collect();
            (
                f[0].parse().unwrap(),
                f[1].parse().unwrap(),
                f[2].parse().unwrap(),
            )
        })
        .collect();
    let mut layout = TextLayout::new(text.clone(), font_for(font_id));
    layout.do_layout();
    let line = layout.line_at(0).unwrap();
    let offs16 = grapheme_offsets16(&text);
    let mut wrong = Vec::new();
    for (x, between, on) in &qt {
        let ours_between = offs16[line.x_to_cursor(*x, CursorPosition::CursorBetweenCharacters)];
        let ours_on = offs16[line.x_to_cursor(*x, CursorPosition::CursorOnCharacter)];
        if (ours_between, ours_on) != (*between, *on) {
            wrong.push(format!(
                "x {x}: ferrokit ({ours_between}, {ours_on}), Qt ({between}, {on})"
            ));
        }
    }
    let key = format!("hit/{case}");
    if wrong.is_empty() {
        report.push((key, Outcome::Dev(0.0)));
    } else {
        report.push((
            key,
            Outcome::Mismatch(format!(
                "{} of {} x differ: {}",
                wrong.len(),
                qt.len(),
                wrong[..wrong.len().min(6)].join("; ")
            )),
        ));
    }
}

/// `xToCursor` on one line of wrapped text: the cursor position at each sampled x.
fn check_hit_wrapped(report: &mut Report, tokens: &[&str]) {
    let (case, font_id, width, number, text) = (
        tokens[1],
        tokens[2],
        tokens[3],
        tokens[4],
        hex_decode(tokens[5]),
    );
    let number: usize = number.parse().unwrap();
    let n: usize = tokens[6].parse().unwrap();
    let qt: Vec<(f32, usize, usize)> = tokens[7..7 + n]
        .iter()
        .map(|p| {
            let f: Vec<&str> = p.split(':').collect();
            (
                f[0].parse().unwrap(),
                f[1].parse().unwrap(),
                f[2].parse().unwrap(),
            )
        })
        .collect();
    let mut layout = TextLayout::new(text.clone(), font_for(font_id));
    layout.set_wrap_mode(WrapMode::WrapAtWordBoundaryOrAnywhere);
    layout.set_wrap_width(Some(width.parse().unwrap()));
    layout.do_layout();
    let key = format!("hit/{case}/{width}/line{number}");
    let Some(line) = layout.line_at(number) else {
        report.push((key, Outcome::Mismatch(format!("no line {number}"))));
        return;
    };
    let offs16 = grapheme_offsets16(&text);
    let mut wrong = Vec::new();
    for (x, between, on) in &qt {
        let ours_between = offs16[line.x_to_cursor(*x, CursorPosition::CursorBetweenCharacters)];
        let ours_on = offs16[line.x_to_cursor(*x, CursorPosition::CursorOnCharacter)];
        if (ours_between, ours_on) != (*between, *on) {
            wrong.push(format!(
                "x {x}: ferrokit ({ours_between}, {ours_on}), Qt ({between}, {on})"
            ));
        }
    }
    if wrong.is_empty() {
        report.push((key, Outcome::Dev(0.0)));
    } else {
        report.push((
            key,
            Outcome::Mismatch(format!(
                "{} of {} x differ: {}",
                wrong.len(),
                qt.len(),
                wrong[..wrong.len().min(6)].join("; ")
            )),
        ));
    }
}

fn check_wrap(report: &mut Report, tokens: &[&str]) {
    let (case, font_id, width, text) = (tokens[1], tokens[2], tokens[4], hex_decode(tokens[5]));
    let n: usize = tokens[6].parse().unwrap();
    // (start, length, natural width, x at the end) of every line.
    let qt: Vec<(usize, usize, f64, f64)> = tokens[7..7 + n]
        .iter()
        .map(|p| {
            let f: Vec<&str> = p.split(':').collect();
            (
                f[0].parse().unwrap(),
                f[1].parse().unwrap(),
                f[2].parse().unwrap(),
                f[3].parse().unwrap(),
            )
        })
        .collect();

    let mut layout = TextLayout::new(text.clone(), font_for(font_id));
    set_tab_stop(&mut layout, case);
    layout.set_wrap_mode(wrap_mode(tokens[3]));
    layout.set_wrap_width(Some(width.parse().unwrap()));
    layout.do_layout();
    let offs16 = grapheme_offsets16(&text);
    let ours: Vec<(usize, usize)> = (0..layout.line_count())
        .map(|i| {
            let l = layout.line_at(i).unwrap();
            let (s, e) = (l.text_start(), l.text_start() + l.text_length());
            (offs16[s], offs16[e] - offs16[s])
        })
        .collect();
    let theirs: Vec<(usize, usize)> = qt.iter().map(|l| (l.0, l.1)).collect();
    let breaks = if ours == theirs {
        Outcome::Dev(0.0)
    } else {
        Outcome::Mismatch(format!("lines (start, length) {ours:?}, Qt has {theirs:?}"))
    };
    let same = breaks.agrees();
    report.push((format!("breaks/{case}/{width}"), breaks));
    if !same {
        // The widths of different lines say nothing about each other.
        return;
    }
    let lines: Vec<_> = (0..layout.line_count())
        .map(|i| layout.line_at(i).unwrap())
        .collect();
    let end = max_dev(lines.iter().zip(&qt).map(|(l, q)| {
        (
            f64::from(l.cursor_to_x(l.text_start() + l.text_length(), Edge::Leading)),
            q.3,
        )
    }));
    report.push((format!("endx/{case}/{width}"), Outcome::Dev(end)));
    let natural = max_dev(
        lines
            .iter()
            .zip(&qt)
            .map(|(l, q)| (f64::from(l.natural_text_width()), q.2)),
    );
    if natural > TOLERANCE {
        let ours: Vec<f32> = lines.iter().map(|l| l.natural_text_width()).collect();
        let theirs: Vec<f64> = qt.iter().map(|q| q.2).collect();
        println!("natural/{case}/{width}: per line\n  ferrokit {ours:?}\n  Qt       {theirs:?}");
    }
    report.push((format!("natural/{case}/{width}"), Outcome::Dev(natural)));
}

/// Lines wrapped at a width and aligned: where the first and the last position of each line are.
fn check_align(report: &mut Report, tokens: &[&str]) {
    let (case, font_id, align, width, text) = (
        tokens[1],
        tokens[2],
        tokens[3],
        tokens[4],
        hex_decode(tokens[5]),
    );
    let n: usize = tokens[6].parse().unwrap();
    // (start, length, x of the first position, x of the last) of every line.
    let qt: Vec<(usize, usize, f64, f64)> = tokens[7..7 + n]
        .iter()
        .map(|p| {
            let f: Vec<&str> = p.split(':').collect();
            (
                f[0].parse().unwrap(),
                f[1].parse().unwrap(),
                f[2].parse().unwrap(),
                f[3].parse().unwrap(),
            )
        })
        .collect();

    let mut layout = TextLayout::new(text.clone(), font_for(font_id));
    layout.set_alignment(match align {
        "left" => Alignment::LEFT,
        "right" => Alignment::RIGHT,
        "center" => Alignment::H_CENTER,
        other => panic!("unknown alignment {other} in the oracle"),
    });
    layout.set_wrap_mode(WrapMode::WrapAtWordBoundaryOrAnywhere);
    layout.set_wrap_width(Some(width.parse().unwrap()));
    layout.do_layout();
    let offs16 = grapheme_offsets16(&text);
    let ours: Vec<(usize, usize)> = (0..layout.line_count())
        .map(|i| {
            let l = layout.line_at(i).unwrap();
            let (s, e) = (l.text_start(), l.text_start() + l.text_length());
            (offs16[s], offs16[e] - offs16[s])
        })
        .collect();
    let theirs: Vec<(usize, usize)> = qt.iter().map(|l| (l.0, l.1)).collect();
    let key = format!("align/{case}/{align}/{width}");
    if ours != theirs {
        report.push((
            key,
            Outcome::Mismatch(format!("lines (start, length) {ours:?}, Qt has {theirs:?}")),
        ));
        return;
    }
    let dev = max_dev((0..layout.line_count()).zip(&qt).flat_map(|(i, q)| {
        let l = layout.line_at(i).unwrap();
        let (s, e) = (l.text_start(), l.text_start() + l.text_length());
        [
            (f64::from(l.cursor_to_x(s, Edge::Leading)), q.2),
            (f64::from(l.cursor_to_x(e, Edge::Leading)), q.3),
        ]
    }));
    if dev > TOLERANCE {
        let ours: Vec<(f32, f32)> = (0..layout.line_count())
            .map(|i| {
                let l = layout.line_at(i).unwrap();
                let (s, e) = (l.text_start(), l.text_start() + l.text_length());
                (
                    l.cursor_to_x(s, Edge::Leading),
                    l.cursor_to_x(e, Edge::Leading),
                )
            })
            .collect();
        let theirs: Vec<(f64, f64)> = qt.iter().map(|q| (q.2, q.3)).collect();
        println!("{key}: (x of the first, x of the last position) per line\n  ferrokit {ours:?}\n  Qt       {theirs:?}");
    }
    report.push((key, Outcome::Dev(dev)));
}

/// Number of lines of the oracle file per `(kind, font id)`. A generator run that adds, drops
/// or renames lines must update this table, so fixtures cannot disappear unnoticed, and every
/// `run` checks that it really went through all of the lines it stands for.
const EXPECTED_LINES: &[(&str, &str, usize)] = &[
    ("align", "arial", 6),
    ("align", "syn", 21),
    ("cursor", "arial", 28),
    ("cursor", "syn", 40),
    ("hit", "arial", 4),
    ("hit", "syn", 12),
    ("hitwrap", "syn", 17),
    ("wrap", "arial", 41),
    ("wrap", "syn", 367),
];

fn oracle_lines() -> impl Iterator<Item = Vec<&'static str>> {
    ORACLE
        .lines()
        .filter(|l| !l.starts_with('#') && !l.starts_with("font "))
        .map(|l| l.split(' ').collect())
}

fn pattern_matches(key: &str, pattern: &str) -> bool {
    key == pattern || (pattern.ends_with('/') && key.starts_with(pattern))
}

/// Every comparison of the oracle lines of `kinds` whose font id is one of `fonts`, and how
/// many lines that was.
fn collect(kinds: &[&str], fonts: &[&str]) -> (Report, usize) {
    set_application_device_pixel_ratio(2.0);
    let mut report = Report::new();
    let mut lines = 0;
    for t in oracle_lines() {
        if !kinds.contains(&t[0]) || !fonts.contains(&t[2]) {
            continue;
        }
        lines += 1;
        match t[0] {
            "cursor" => check_cursor(&mut report, &t),
            "hit" => check_hit(&mut report, &t),
            "hitwrap" => check_hit_wrapped(&mut report, &t),
            "wrap" => check_wrap(&mut report, &t),
            "align" => check_align(&mut report, &t),
            other => panic!("unknown oracle line kind {other}"),
        }
    }
    (report, lines)
}

/// Runs every oracle line of `kind` whose font id is one of `fonts`, prints the table of
/// deviations (`--nocapture`) and fails on every unexpected result.
fn run(kinds: &[&str], fonts: &[&str]) {
    let (report, lines) = collect(kinds, fonts);
    let expected: usize = EXPECTED_LINES
        .iter()
        .filter(|(k, f, _)| kinds.contains(k) && fonts.contains(f))
        .map(|e| e.2)
        .sum();
    assert!(expected > 0, "no oracle line is expected for {kinds:?} {fonts:?}");
    assert_eq!(
        lines, expected,
        "the oracle holds {lines} lines for {kinds:?} {fonts:?}, EXPECTED_LINES says {expected}"
    );

    let mut failures = Vec::new();
    for (key, outcome) in &report {
        let known = KNOWN_DIVERGENCES
            .iter()
            .find(|(p, _, _)| pattern_matches(key, p));
        match outcome {
            Outcome::Dev(d) => println!("{key}: max deviation {d:.6} px"),
            Outcome::Mismatch(m) => println!("{key}: {m}"),
        }
        match (known, outcome.agrees()) {
            (None, true) => {}
            (None, false) => failures.push(format!("{key}: {outcome:?}")),
            (Some((_, why, _)), true) => failures.push(format!(
                "{key}: listed in KNOWN_DIVERGENCES ({why}) but now agrees with Qt; remove the entry"
            )),
            (Some((_, _, Some(max))), false) => match outcome {
                Outcome::Dev(d) if d > max => failures.push(format!(
                    "{key}: deviates {d:.6} px, more than the {max} px its KNOWN_DIVERGENCES entry allows"
                )),
                Outcome::Mismatch(m) => failures.push(format!(
                    "{key}: its KNOWN_DIVERGENCES entry is about a difference of at most {max} px, \
                     but it is now: {m}"
                )),
                Outcome::Dev(_) => {}
            },
            (Some((_, _, None)), false) => {}
        }
    }
    assert!(
        failures.is_empty(),
        "{} divergences from Qt:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn the_oracle_file_holds_the_expected_fixtures() {
    let mut counts: BTreeMap<(&str, &str), usize> = BTreeMap::new();
    for t in oracle_lines() {
        *counts.entry((t[0], t[2])).or_default() += 1;
    }
    let expected: BTreeMap<(&str, &str), usize> =
        EXPECTED_LINES.iter().map(|&(k, f, n)| ((k, f), n)).collect();
    assert_eq!(
        counts, expected,
        "tests/data/text_layout_oracle.txt and EXPECTED_LINES disagree; regenerate the file or \
         update the table on purpose"
    );
}

#[test]
fn every_known_divergence_names_at_least_one_case() {
    let fonts: &[&str] = if cfg!(windows) {
        &["syn", "arial"]
    } else {
        &["syn"]
    };
    let (report, _) = collect(&["cursor", "hit", "hitwrap", "wrap", "align"], fonts);
    for (pattern, _, _) in KNOWN_DIVERGENCES {
        // The Arial cases only run where Arial is installed (Windows).
        if !cfg!(windows) && pattern.contains("arial_") {
            continue;
        }
        assert!(
            report.iter().any(|(key, _)| pattern_matches(key, pattern)),
            "KNOWN_DIVERGENCES entry {pattern:?} matches no oracle case"
        );
    }
}

#[test]
fn synthetic_font_cursor_x_matches_qt() {
    run(&["cursor"], &["syn"]);
}

#[test]
fn synthetic_font_line_breaks_match_qt() {
    run(&["wrap"], &["syn"]);
}

#[test]
fn synthetic_font_hit_tests_match_qt() {
    run(&["hit"], &["syn"]);
}

#[cfg(windows)]
#[test]
fn arial_hit_tests_match_qt() {
    run(&["hit"], &["arial"]);
}

#[test]
fn synthetic_font_hit_tests_on_wrapped_lines_match_qt() {
    run(&["hitwrap"], &["syn"]);
}

#[test]
fn synthetic_font_aligned_lines_match_qt() {
    run(&["align"], &["syn"]);
}

#[cfg(windows)]
#[test]
fn arial_aligned_lines_match_qt() {
    run(&["align"], &["arial"]);
}

#[cfg(windows)]
#[test]
fn arial_cursor_x_matches_qt() {
    run(&["cursor"], &["arial"]);
}

#[cfg(windows)]
#[test]
fn arial_line_breaks_match_qt() {
    run(&["wrap"], &["arial"]);
}

/// `QTextOption()` starts with `WordWrap` (what a `QTextLayout` and a `QLabel` use) and
/// `QTextDocument` with `WrapAtWordBoundaryOrAnywhere` (which `QTextEdit` and `QPlainTextEdit`
/// keep); measured in PySide6 6.11.2 with `QTextOption().wrapMode()` and
/// `QTextDocument().defaultTextOption().wrapMode()`.
#[test]
fn default_wrap_modes_are_those_of_qt() {
    use ferrokit_gui::text::document::TextDocument;
    assert_eq!(
        TextLayout::new("", Font::new("Arial", 16.0)).wrap_mode(),
        WrapMode::WordWrap
    );
    assert_eq!(
        TextDocument::new().wrap_mode(),
        WrapMode::WrapAtWordBoundaryOrAnywhere
    );
}

/// What the layout draws sits where right to left text is shown: the synthetic font's glyphs are
/// boxes of known width, so the expected picture follows from the advances (in 16 px: a space 4, `a`
/// 7.5, `b` 7.625, `c` 7.75, `d` 7.875, a digit 8, alef 8.75, bet 8.5, gimel 6.25) and the order
/// UAX #9 gives each text, worked out by hand below. Glyphs and selection rectangles are looked at
/// away from their edges, which backends antialias differently.
#[test]
fn drawn_glyphs_and_selections_follow_the_visual_order_of_bidi_text() {
    use ferrokit_gui::geometry::primitives::PointF;
    use ferrokit_gui::paint::painter::Painter;
    use ferrokit_gui::paint::pixmap::Pixmap;
    use ferrokit_gui::text::{FormatRange, TextCharFormat};
    use tiny_skia::Color;

    set_application_device_pixel_ratio(2.0);
    let advance = |c: char| -> f32 {
        match c {
            ' ' => 4.0,
            'a' => 7.5,
            'b' => 7.625,
            'c' => 7.75,
            'd' => 7.875,
            '0'..='9' => 8.0,
            '\u{5d0}' => 8.75,
            '\u{5d1}' => 8.5,
            '\u{5d2}' => 6.25,
            other => panic!("no advance for {other:?}"),
        }
    };
    // (text, the logical index of each grapheme from the left edge to the right, a selection, the
    // runs of the line with their left edges: what is drawn is each run drawn as a string there)
    type Shown = &'static [(&'static str, f32)];
    let cases: [(&str, &[usize], std::ops::Range<usize>, Shown); 5] = [
        (
            "ab \u{5d0}\u{5d2} cd",
            &[0, 1, 2, 4, 3, 5, 6, 7],
            2..5,
            &[("ab ", 0.0), ("\u{5d0}\u{5d2}", 19.125), (" cd", 34.125)],
        ),
        (
            "ab \u{5d0}\u{5d2} cd",
            &[0, 1, 2, 4, 3, 5, 6, 7],
            1..4,
            &[("ab ", 0.0), ("\u{5d0}\u{5d2}", 19.125), (" cd", 34.125)],
        ),
        (
            "\u{5d2}\u{5d0} ab \u{5d2}",
            &[6, 5, 3, 4, 2, 1, 0],
            3..7,
            &[
                ("\u{5d2}", 0.0),
                (" ", 6.25),
                ("ab", 10.25),
                (" ", 25.375),
                ("\u{5d2}\u{5d0}", 29.375),
            ],
        ),
        (
            "\u{5d2}\u{5d0} ab \u{5d2}",
            &[6, 5, 3, 4, 2, 1, 0],
            1..4,
            &[
                ("\u{5d2}", 0.0),
                (" ", 6.25),
                ("ab", 10.25),
                (" ", 25.375),
                ("\u{5d2}\u{5d0}", 29.375),
            ],
        ),
        (
            "ab \u{5d0}\u{5d2} 12 \u{5d2}",
            &[0, 1, 2, 9, 8, 6, 7, 5, 4, 3],
            2..9,
            &[
                ("ab ", 0.0),
                ("\u{5d2}", 19.125),
                (" ", 25.375),
                ("12", 29.375),
                (" ", 45.375),
                ("\u{5d0}\u{5d2}", 49.375),
            ],
        ),
    ];
    let origin = PointF::new(10.0, 2.0);
    let green = Color::from_rgba8(0, 255, 0, 255);
    for (text, visual, selection, shown) in cases {
        let chars: Vec<char> = text.chars().collect();
        assert_eq!(chars.len(), visual.len());
        // The span of every grapheme from the left edge of the text.
        let mut span = vec![(0.0f32, 0.0f32); chars.len()];
        let mut x = 0.0;
        for &logical in visual {
            span[logical] = (x, x + advance(chars[logical]));
            x += advance(chars[logical]);
        }

        let mut layout = TextLayout::new(text, font_for("syn"));
        layout.do_layout();
        let background = TextCharFormat {
            background: Some(green),
            ..TextCharFormat::default()
        };
        let selections = [FormatRange::new(
            selection.start,
            selection.len(),
            background,
        )];
        let mut pixmap = Pixmap::new(120, 24).unwrap();
        {
            let mut painter = Painter::begin(&mut pixmap);
            layout.draw(&mut painter, origin, &selections);
        }
        let alpha_at = |px: usize, y: f32| -> f32 {
            f32::from(pixmap.data()[(y as usize * 120 + px) * 4 + 3]) / 255.0
        };
        // The share of the pixel column `px` that the intervals (from the left edge of the text)
        // cover.
        let covered = |px: usize, intervals: &[(f32, f32)]| -> f32 {
            let (lo, hi) = (px as f32 - origin.x, px as f32 + 1.0 - origin.x);
            intervals
                .iter()
                .map(|&(a, b)| (b.min(hi) - a.max(lo)).max(0.0))
                .sum::<f32>()
        };
        let merge = |mut spans: Vec<(f32, f32)>| -> Vec<(f32, f32)> {
            spans.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut merged: Vec<(f32, f32)> = Vec::new();
            for (lo, hi) in spans {
                match merged.last_mut() {
                    Some(last) if lo <= last.1 + 1e-4 => last.1 = last.1.max(hi),
                    _ => merged.push((lo, hi)),
                }
            }
            merged
        };
        // Selection rectangles are snapped to whole pixels: half a pixel of error at an edge.
        let tolerance = 0.51;

        // The glyphs are where the runs of the line are shown: the picture is the one made by
        // drawing every run as a string at its place (which shapes it in its own direction).
        let mut plain = Pixmap::new(120, 24).unwrap();
        {
            let mut painter = Painter::begin(&mut plain);
            layout.draw(&mut painter, origin, &[]);
        }
        let mut reference = Pixmap::new(120, 24).unwrap();
        {
            let line = layout.line_at(0).unwrap();
            let baseline = origin.y + line.y() + line.ascent();
            let mut painter = Painter::begin(&mut reference);
            for &(run, left) in shown {
                painter.draw_text_colored(
                    PointF::new(origin.x + left, baseline),
                    run,
                    &font_for("syn"),
                    Color::BLACK,
                );
            }
        }
        assert!(
            reference.data().iter().any(|&b| b != 0),
            "the reference draws something"
        );
        assert_eq!(plain.data(), reference.data(), "{text:?}");

        // The selection fills the top of the line, above the glyphs, on the spans of the selected
        // graphemes wherever they are shown, and nowhere else.
        let selected = merge(selection.clone().map(|g| span[g]).collect());
        for px in 8..(origin.x + x + 3.0) as usize {
            let (got, want) = (alpha_at(px, origin.y + 0.5), covered(px, &selected));
            assert!(
                (got - want).abs() <= tolerance,
                "{text:?}: selection {selection:?} covers {got:.2} of pixel column {px}, it should cover {want:.2}"
            );
        }
    }
}
