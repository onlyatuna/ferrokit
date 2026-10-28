// SPDX-License-Identifier: LGPL-3.0-only
//! `TextLayout` / `TextLine` (`QTextLayout` / `QTextLine`) against Qt 6.11.2.
//!
//! `tests/data/text_layout_oracle.txt` holds measurements of PySide6 6.11.2's real `QTextLayout`
//! (regenerate it with `tests/data/gen_text_layout_oracle.py`, which also writes the font below):
//! `QTextLine::cursorToX` at every grapheme boundary of one unwrapped line, and the lines
//! (`textStart`, `textLength`, `naturalTextWidth`, `cursorToX` at the line end) of text wrapped at
//! several widths.
//!
//! Two fonts:
//! * `tests/data/oracle_text.ttf`, a synthetic font with kerning, a `liga` ligature with a GDEF
//!   caret, combining marks and Hebrew letters, built by the generator. Its units per em is 1024
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
//! be hit by a legitimate rounding choice and is still 8 times smaller than the smallest kerning
//! pair (the 1 px of `AV` is 64 quanta). It is applied to every case alike; no case has a looser
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
use ferrokit_gui::text::Edge;
use std::collections::BTreeMap;
use unicode_segmentation::UnicodeSegmentation;

const ORACLE: &str = include_str!("data/text_layout_oracle.txt");
const SYNTHETIC_TTF: &[u8] = include_bytes!("data/oracle_text.ttf");

/// See the module documentation.
const TOLERANCE: f64 = 1.0 / 128.0;

/// Cases where ferrokit differs from Qt: `(key, reason)`. A key is `cursor/<case>`,
/// `breaks/<case>/<width>`, `endx/<case>/<width>`, `natural/<case>/<width>` or
/// `align/<case>/<align>/<width>`; an entry ending in `/` stands for every key that
/// starts with it, and all of them must diverge. The cursor and natural-width entries are
/// differences of values; the `breaks` entries are different lines.
const KNOWN_DIVERGENCES: &[(&str, &str)] = &[
    // --- cursor_to_x ---
    (
        "cursor/syn_liga_ffi",
        "Qt divides a ligature over its characters in 26.6 fixed point, `floor(advance / n)` per \
         character (the three-letter `ffi`, 15.625 px: 5.203125 px each); ferrokit divides in f32 \
         (5.2083). 0.0104 px at most, and only for ligatures of 3+ characters",
    ),
    ("cursor/syn_liga_run", "same ligature division as syn_liga_ffi"),
    (
        "cursor/syn_rtl_hebrew",
        "`cursor_to_x` is in logical order: the x of position n is the sum of the advances of the \
         first n graphemes. Qt returns the visual x, which runs right to left inside a \
         right-to-left run (`TextLayout` has no bidi reordering)",
    ),
    ("cursor/syn_rtl_mixed", "no bidi reordering, as syn_rtl_hebrew"),
    ("cursor/syn_rtl_final", "no bidi reordering, as syn_rtl_hebrew"),
    ("cursor/arial_hebrew", "no bidi reordering, as syn_rtl_hebrew"),
    ("cursor/arial_mixed_rtl", "no bidi reordering, as syn_rtl_hebrew"),
    (
        "cursor/arial_emoji_zwj",
        "Qt shapes the ZWJ sequence 👨‍👩‍👧 as one Segoe UI Emoji glyph (20 px); ferrokit measures three \
         separate emoji (66 px). [INFERENCE: `partition_into_runs` picks a font per character by \
         its own coverage, and Arial covers U+200D, so the sequence is cut into runs]",
    ),
    (
        "cursor/arial_emoji_keycap",
        "Qt draws `1` U+FE0F U+20E3 as one Segoe UI Emoji keycap glyph (22 px); ferrokit keeps the \
         Arial `1` (8.9 px). [INFERENCE: same run splitting as arial_emoji_zwj]",
    ),
    // --- line breaking ---
    (
        "breaks/syn_newline/",
        "ferrokit ends a line at '\\n'; QTextLayout does not (it is a paragraph separator, which \
         `QTextDocument` splits into blocks; the forced break of a layout is U+2028)",
    ),
    ("breaks/syn_trailing_newline/", "as syn_newline: no break at '\\n' in Qt"),
    (
        "breaks/syn_line_separator/",
        "ferrokit does not end a line at U+2028 (QChar::LineSeparator), which is Qt's forced line \
         break",
    ),
    (
        "breaks/syn_trailing_line_separator/",
        "as syn_line_separator; Qt also gives a text ending in U+2028 an empty last line",
    ),
    // --- widths ---
    (
        "natural/arial_kerned_words/50",
        "Qt's `naturalTextWidth` is the sum of the advances plus the negative right bearing of \
         the last glyph, the ink that hangs over its advance (`QTextLine::layout_helper`'s \
         `negativeRightBearing`): the `A` that ends `AVAVA` hangs 1/64 px over, so Qt's width is \
         47.4375 against its `cursorToX` at the line end of 47.421875. [INFERENCE from the values; \
         ferrokit has no outline bounds of a glyph at hand]",
    ),
    ("natural/arial_pangram/100", "same right bearing, of the `r` that ends `jumps over`: 0.21875 px"),
    ("natural/arial_pangram/240", "same right bearing as arial_pangram/100"),
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
    let qt: Vec<(usize, f64)> = tokens[5..5 + n]
        .iter()
        .map(|p| {
            let (o, x) = p.split_once(':').unwrap();
            (o.parse().unwrap(), x.parse().unwrap())
        })
        .collect();

    let mut layout = TextLayout::new(text.clone(), font_for(font_id));
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
    let theirs: Vec<usize> = qt.iter().map(|(o, _)| *o).collect();
    if ours != theirs {
        report.push((
            key,
            Outcome::Mismatch(format!("cursor positions {ours:?}, Qt has {theirs:?}")),
        ));
        return;
    }
    let pairs: Vec<(f64, f64)> = qt
        .iter()
        .enumerate()
        .map(|(g, (_, x))| (f64::from(line.cursor_to_x(g, Edge::Leading)), *x))
        .collect();
    let dev = max_dev(pairs.iter().copied());
    if dev > TOLERANCE {
        let ours: Vec<f64> = pairs.iter().map(|p| p.0).collect();
        let theirs: Vec<f64> = pairs.iter().map(|p| p.1).collect();
        println!("{key}: x at each cursor position\n  ferrokit {ours:?}\n  Qt       {theirs:?}");
    }
    report.push((key, Outcome::Dev(dev)));
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
    report.push((key, Outcome::Dev(dev)));
}

/// Runs every oracle line of `kind` whose font id is one of `fonts`, prints the table of
/// deviations (`--nocapture`) and fails on every unexpected result.
fn run(kinds: &[&str], fonts: &[&str]) {
    set_application_device_pixel_ratio(2.0);
    let mut report = Report::new();
    for line in ORACLE
        .lines()
        .filter(|l| !l.starts_with('#') && !l.starts_with("font "))
    {
        let t: Vec<&str> = line.split(' ').collect();
        if !kinds.contains(&t[0]) || !fonts.contains(&t[2]) {
            continue;
        }
        match t[0] {
            "cursor" => check_cursor(&mut report, &t),
            "wrap" => check_wrap(&mut report, &t),
            "align" => check_align(&mut report, &t),
            other => panic!("unknown oracle line kind {other}"),
        }
    }
    assert!(
        !report.is_empty(),
        "no oracle line matched {kinds:?} {fonts:?}"
    );

    let mut failures = Vec::new();
    for (key, outcome) in &report {
        let known = KNOWN_DIVERGENCES
            .iter()
            .find(|(p, _)| key == p || (p.ends_with('/') && key.starts_with(p)));
        match outcome {
            Outcome::Dev(d) => println!("{key}: max deviation {d:.6} px"),
            Outcome::Mismatch(m) => println!("{key}: {m}"),
        }
        match (known, outcome.agrees()) {
            (None, true) => {}
            (None, false) => failures.push(format!("{key}: {outcome:?}")),
            (Some((_, why)), true) => failures.push(format!(
                "{key}: listed in KNOWN_DIVERGENCES ({why}) but now agrees with Qt; remove the entry"
            )),
            (Some(_), false) => {}
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
fn synthetic_font_cursor_x_matches_qt() {
    run(&["cursor"], &["syn"]);
}

#[test]
fn synthetic_font_line_breaks_match_qt() {
    run(&["wrap"], &["syn"]);
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
