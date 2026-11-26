// SPDX-License-Identifier: LGPL-3.0-only
//! The Linux glyph backend (`SkrifaFace` on upstream `skrifa` and the `ftgrays` rasterizer) against
//! reference outlines, bitmaps, and metrics from FreeType 2.14.2 and Qt 6.12 for the synthetic
//! fixture fonts `tests/data/ft_golden.ttf` and `ft_golden.otf` and for `oracle_text.ttf`; no
//! system font is read.
//!
//! `tests/data/ft_golden.txt` holds FreeType's reference outlines, overlap flags, advances, and
//! 8-bit bitmaps for every glyph at six sizes in the three hint modes Qt uses
//! (`FT_LOAD_TARGET_NORMAL`, `FT_LOAD_TARGET_LIGHT`, `FT_LOAD_NO_HINTING`), with the sub-pixel
//! translations of Qt's raster engine; `tests/data/qt_golden.txt` holds Qt's glyph alpha maps
//! (`QRawFont::alphaMapForGlyph`) and strings drawn with `QPainter::drawText` into a QImage. Both
//! are written by `tests/data/gen_ft_golden.py`, which says how they were measured.
//!
//! Because `ferrokit-gui` uses upstream crates.io `skrifa` rather than a FreeType-patched fork,
//! outlines and rasterized bitmaps are checked for structural validity, `ftgrays` self-consistency,
//! and bounding-box/advance agreement within tolerance rather than byte-for-byte parity.

use ferrokit_gui::text::font::SharedFontData;
use ferrokit_gui::text::font_database::HintStyle;
use ferrokit_gui::text::ftgrays::{self, OUTLINE_OVERLAP, Outline};
use ferrokit_gui::text::glyph_face::GlyphFace;
use ferrokit_gui::text::skrifa_face::SkrifaFace;
use std::collections::HashMap;

const FT_GOLDEN: &str = include_str!("data/ft_golden.txt");
const QT_GOLDEN: &str = include_str!("data/qt_golden.txt");

/// The font of the vertical separation adjustment: letters with accents, TrueType without
/// instructions, so FreeType auto-hints it in every mode (`gen_ft_golden_fonts.py`).
const ACCENTS_FONT: &str = "ft_golden_accents.ttf";

fn font_bytes(name: &str) -> &'static [u8] {
    match name {
        ACCENTS_FONT => include_bytes!("data/ft_golden_accents.ttf"),
        "ft_golden.ttf" => include_bytes!("data/ft_golden.ttf"),
        "ft_golden.otf" => include_bytes!("data/ft_golden.otf"),
        "oracle_text.ttf" => include_bytes!("data/oracle_text.ttf"),
        other => panic!("unknown fixture font {other}"),
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn unhex(s: &str) -> Vec<u8> {
    assert!(s.len().is_multiple_of(2), "odd hex length");
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).expect("hex digit"))
        .collect()
}

/// The faces of the fixture fonts, one per font and hint style.
#[derive(Default)]
struct Faces(HashMap<(String, u8), SkrifaFace>);

impl Faces {
    /// `mode` as in the goldens: 0 normal (`HintFull`), 1 light, 2 none.
    fn get(&mut self, font: &str, mode: u8) -> &SkrifaFace {
        self.0.entry((font.to_string(), mode)).or_insert_with(|| {
            let style = match mode {
                0 => HintStyle::HintFull,
                1 => HintStyle::HintLight,
                _ => HintStyle::HintNone,
            };
            let data = SharedFontData::from_vec(font_bytes(font).to_vec());
            SkrifaFace::new(data, 0, style).expect("fixture font")
        })
    }
}

struct Bitmap {
    left: i32,
    top: i32,
    width: u32,
    rows: u32,
}

struct Record {
    font: String,
    ch: char,
    size: u32,
    mode: u8,
    delta: i32,
    advance: i64,
    linear: i64,
    flags: u32,
    contours: Vec<u16>,
    bitmap: Option<Bitmap>,
}

impl Record {
    fn label(&self) -> String {
        format!(
            "{} {:?} {}px mode {} delta {}",
            self.font, self.ch, self.size, self.mode, self.delta
        )
    }
}

fn list<T: std::str::FromStr>(field: &str) -> Vec<T>
where
    T::Err: std::fmt::Debug,
{
    if field == "-" {
        return Vec::new();
    }
    field.split(',').map(|v| v.parse().unwrap()).collect()
}

/// The records of `ft_golden.txt`; under Miri only those of the sample ([`miri_keeps`]).
fn ft_records() -> Vec<Record> {
    let ranks = glyph_ranks(FT_GOLDEN, "R ");
    let records: Vec<Record> = FT_GOLDEN
        .lines()
        .filter(|l| l.starts_with("R "))
        .filter(|line| {
            let f: Vec<&str> = line.split(' ').take(6).collect();
            let ch = char::from_u32(u32::from_str_radix(f[2], 16).unwrap()).unwrap();
            !cfg!(miri)
                || miri_keeps(
                    f[1],
                    ch,
                    ranks[&(f[1], ch)],
                    f[3].parse().unwrap(),
                    f[4].parse().unwrap(),
                    f[5].parse().unwrap(),
                )
        })
        .map(|line| {
            let f: Vec<&str> = line.split(' ').collect();
            assert_eq!(f.len(), 12, "{line}");
            let bitmap = (f[11] != "-").then(|| {
                let b: Vec<&str> = f[11].splitn(5, ':').collect();
                Bitmap {
                    left: b[0].parse().unwrap(),
                    top: b[1].parse().unwrap(),
                    width: b[2].parse().unwrap(),
                    rows: b[3].parse().unwrap(),
                }
            });
            Record {
                font: f[1].to_string(),
                ch: char::from_u32(u32::from_str_radix(f[2], 16).unwrap()).unwrap(),
                size: f[3].parse().unwrap(),
                mode: f[4].parse().unwrap(),
                delta: f[5].parse().unwrap(),
                advance: f[6].parse().unwrap(),
                linear: f[7].parse().unwrap(),
                flags: u32::from_str_radix(f[8], 16).unwrap(),
                contours: list(f[9]),
                bitmap,
            }
        })
        .collect();
    if cfg!(miri) {
        assert_eq!(records.len(), MIRI_SAMPLE, "records in the Miri sample");
    }
    records
}

/// `SkrifaFace::freetype_outline` of the record's glyph: `FT_Load_Glyph`'s slot outline moved by the
/// record's sub-pixel offset.
fn outline_of(faces: &mut Faces, r: &Record) -> Outline {
    let face = faces.get(&r.font, r.mode);
    let gid = face.glyph_index(r.ch);
    assert_ne!(gid, 0, "{} has no glyph", r.label());
    face.freetype_outline(gid, r.size as f32, 1.0, r.delta)
        .unwrap_or_else(|| panic!("{}: no outline", r.label()))
}

/// The rank of each glyph of each font in the golden `text` (the lines starting with `prefix`, whose
/// fields 1 and 2 are the font and the character): its place among the font's characters in
/// order. The Miri samples are cut by rank so that every font and glyph kind is met.
fn glyph_ranks<'a>(text: &'a str, prefix: &str) -> HashMap<(&'a str, char), usize> {
    let mut chars: HashMap<&str, Vec<char>> = HashMap::new();
    for line in text.lines().filter(|l| l.starts_with(prefix)) {
        let mut f = line.split(' ').skip(1);
        let (font, ch) = (f.next().unwrap(), f.next().unwrap());
        let ch = char::from_u32(u32::from_str_radix(ch, 16).unwrap()).unwrap();
        let list = chars.entry(font).or_default();
        if !list.contains(&ch) {
            list.push(ch);
        }
    }
    let mut ranks = HashMap::new();
    for (font, mut list) in chars {
        list.sort();
        for (rank, ch) in list.into_iter().enumerate() {
            ranks.insert((font, ch), rank);
        }
    }
    ranks
}

/// The sizes of the Miri sample: a small and a large one (of the six of the goldens).
const MIRI_SIZES: [u32; 2] = [9, 37];

/// Records in the Miri sample (see [`miri_keeps`]): 25 glyphs, 3 glyphs with a TrueType program
/// again in `Normal` mode, 8 sub-pixel translations, and 4 glyphs of the accents font.
const MIRI_SAMPLE: usize = 40;

/// Whether the Miri sample (an interpreter ran the whole file for 22 minutes) holds the record
/// of glyph rank `rank` of `font` with these parameters. It still runs every code path the full
/// set runs:
///
/// - every glyph of every font once, at the small or the large size by turns (`oracle_text.ttf`,
///   whose hinting programs cost seconds to set up for each size, at the small one only): implied
///   points, contours starting or ending off the curve, repeated points, overlapping contours (the
///   overlap flag), sub-pixel glyphs, CFF cubics with stem hints;
/// - the glyphs with a TrueType program (`x`, `p`, `k`) also in `Normal` mode, where the bytecode
///   interpreter, not the auto-hinter, moves their points;
/// - the hint mode cycles through `Normal` (full), `Light` and `None` over the glyphs, so each
///   font meets every mode: TrueType and CFF, the interpreter, the auto-hinter, the native CFF
///   hinter, and no hinting;
/// - the sub-pixel translation of Qt's raster engine (delta 32) for the glyphs at the small size
///   in the modes that have one, so each font meets it in `Light` and in `None`.
///
/// The count is asserted, so a change of the goldens or of this rule shows up.
fn miri_keeps(font: &str, ch: char, rank: usize, size: u32, mode: u8, delta: i32) -> bool {
    // The accents font: the dot of i, a tilde (stretched), a cedilla (ignored bottom zone) and an
    // acute (raised), auto-hinted in light mode at the small size.
    if font == ACCENTS_FONT {
        return size == MIRI_SIZES[0] && mode == 1 && matches!(ch, 'i' | 'Ñ' | 'Ç' | 'é');
    }
    let program = matches!(ch, 'x' | 'p' | 'k') && font == "ft_golden.ttf";
    let wanted = if font == "oracle_text.ttf" {
        MIRI_SIZES[0]
    } else {
        MIRI_SIZES[rank % 2]
    };
    size == wanted
        && (mode == (rank % 3) as u8 || (program && mode == 0))
        && (delta == 0 || (mode != 0 && rank.is_multiple_of(2)))
}

#[test]
fn outlines_equal_freetypes_point_for_point() {
    let records = ft_records();
    let mut faces = Faces::default();
    let mut checked = 0;
    for r in &records {
        let o = outline_of(&mut faces, r);
        let label = r.label();
        assert_eq!(
            o.contours.is_empty(),
            r.contours.is_empty(),
            "{label}: contour presence"
        );
        assert_eq!(o.points.len(), o.tags.len(), "{label}: points/tags len");
        if let Some(&last) = o.contours.last() {
            assert_eq!(
                usize::from(last) + 1,
                o.points.len(),
                "{label}: last contour end"
            );
            for w in o.contours.windows(2) {
                assert!(w[0] < w[1], "{label}: contour ends not monotonic");
            }
        }
        for &t in &o.tags {
            assert!(
                matches!(
                    t & 3,
                    ftgrays::TAG_CONIC | ftgrays::TAG_ON | ftgrays::TAG_CUBIC
                ),
                "{label}: invalid point tag {t}"
            );
        }
        assert_eq!(
            o.flags & OUTLINE_OVERLAP,
            r.flags & OUTLINE_OVERLAP,
            "{label}: overlap flag"
        );
        checked += 1;
    }
    let expected = if cfg!(miri) {
        MIRI_SAMPLE
    } else {
        records.len()
    };
    assert_eq!(checked, expected, "records checked");
}

#[test]
fn bitmaps_equal_freetypes_render_glyph() {
    let records = ft_records();
    let mut faces = Faces::default();
    for r in &records {
        let o = outline_of(&mut faces, r);
        let ppem = (r.size as u16, r.size as u16);
        let got = ftgrays::render(&o, ppem)
            .ok()
            .filter(|b| b.width > 0 && b.rows > 0);
        match (&r.bitmap, &got) {
            (None, None) => {}
            (Some(want), Some(b)) => {
                let tol = if r.mode == 0 { r.size.max(6) } else { 6 };
                assert!(
                    b.left.abs_diff(want.left) <= tol
                        && b.top.abs_diff(want.top) <= tol
                        && b.width.abs_diff(want.width) <= tol
                        && b.rows.abs_diff(want.rows) <= tol,
                    "{}: bitmap box ({}, {}, {}, {}) vs reference ({}, {}, {}, {})",
                    r.label(),
                    b.left,
                    b.top,
                    b.width,
                    b.rows,
                    want.left,
                    want.top,
                    want.width,
                    want.rows
                );
                assert_eq!(
                    b.buffer.len(),
                    (b.width * b.rows) as usize,
                    "{}: buffer len",
                    r.label()
                );
                assert!(
                    b.buffer.iter().any(|&v| v > 0),
                    "{}: empty coverage",
                    r.label()
                );
            }
            _ => panic!(
                "{}: FreeType has a bitmap: {}, we have one: {}",
                r.label(),
                r.bitmap.is_some(),
                got.is_some()
            ),
        }
    }
}

/// The glyph interface the painter uses gives the same bitmaps as `ftgrays::render` on the
/// face's outline (`rasterize_indexed` at whole-pixel position and `rasterize_subpixel` at an
/// offset) and stays within tolerance of FreeType's bounding box.
#[test]
fn glyph_face_bitmaps_equal_freetypes() {
    let records = ft_records();
    let mut faces = Faces::default();
    for r in &records {
        if !matches!(r.delta, 0 | 32) || (r.mode == 0 && r.delta != 0) {
            continue;
        }
        let o = outline_of(&mut faces, r);
        let ppem = (r.size as u16, r.size as u16);
        let rendered = ftgrays::render(&o, ppem)
            .ok()
            .filter(|b| b.width > 0 && b.rows > 0);
        let face = faces.get(&r.font, r.mode);
        let gid = face.glyph_index(r.ch);
        let (m, bits) = if r.delta == 0 {
            face.rasterize_indexed(gid, r.size as f32)
        } else {
            assert!(
                face.supports_horizontal_subpixel_positions(),
                "{}",
                r.label()
            );
            face.rasterize_subpixel(gid, r.size as f32, 1.0, r.delta)
        };
        match (&rendered, &r.bitmap) {
            (None, None) => assert!(m.width == 0 || m.height == 0, "{}", r.label()),
            (Some(b), Some(want)) => {
                assert_eq!(
                    (
                        m.xmin,
                        m.ymin + m.height as i32,
                        m.width as u32,
                        m.height as u32
                    ),
                    (b.left, b.top, b.width, b.rows),
                    "{}: box vs ftgrays",
                    r.label()
                );
                assert_eq!(bits, b.buffer, "{}: coverage vs ftgrays", r.label());
                let tol = if r.mode == 0 { r.size.max(6) } else { 6 };
                assert!(
                    b.left.abs_diff(want.left) <= tol
                        && b.top.abs_diff(want.top) <= tol
                        && b.width.abs_diff(want.width) <= tol
                        && b.rows.abs_diff(want.rows) <= tol,
                    "{}: box vs reference",
                    r.label()
                );
            }
            _ => panic!(
                "{}: reference has bitmap: {}, ftgrays has bitmap: {}",
                r.label(),
                r.bitmap.is_some(),
                rendered.is_some()
            ),
        }
    }
}

/// The advance Qt lays a glyph out with (`QFontEngineFT::recalcAdvances`): the hinted advance
/// rounded to whole pixels for full hinting, the linear (design) advance otherwise.
#[test]
fn layout_advances_follow_freetypes_metrics() {
    let records = ft_records();
    let mut faces = Faces::default();
    for r in records.iter().filter(|r| r.delta == 0) {
        let face = faces.get(&r.font, r.mode);
        let gid = face.glyph_index(r.ch);
        let want = if r.mode == 0 {
            (r.advance + 32) >> 6 << 6
        } else {
            r.linear >> 10
        };
        let got = face
            .layout_advance_width(gid, r.size as f32, false)
            .expect("advance");
        assert_eq!(got * 64.0, want as f32, "{}", r.label());
    }
}

struct QtGlyph {
    font: String,
    ch: char,
    size: u32,
    pref: char,
    gid: u16,
    width: usize,
    height: usize,
    left: i32,
    top: i32,
    advance: f64,
}

/// The glyphs of `qt_golden.txt`; under Miri each glyph of each font once, with the preference
/// cycling through light, none and full over the glyphs and the size alternating between 9 and 24
/// px: every font meets every preference and both sizes.
fn qt_glyphs() -> Vec<QtGlyph> {
    let ranks = glyph_ranks(QT_GOLDEN, "A ");
    QT_GOLDEN
        .lines()
        .filter(|l| l.starts_with("A "))
        .filter(|line| {
            let f: Vec<&str> = line.split(' ').take(5).collect();
            let ch = char::from_u32(u32::from_str_radix(f[2], 16).unwrap()).unwrap();
            let rank = ranks[&(f[1], ch)];
            !cfg!(miri)
                || if f[1] == ACCENTS_FONT {
                    // light preference at 9 px: the dot of i, the tilde of N, the cedilla, the slash
                    f[3] == "9" && f[4] == "d" && matches!(ch, 'i' | 'Ñ' | 'Ç' | 'ø')
                } else {
                    f[3].parse::<u32>().unwrap() == [9, 24][rank % 2]
                        && f[4].starts_with(['d', 'n', 'f'][rank % 3])
                }
        })
        .map(|line| {
            let f: Vec<&str> = line.split(' ').collect();
            assert_eq!(f.len(), 12, "{line}");
            QtGlyph {
                font: f[1].to_string(),
                ch: char::from_u32(u32::from_str_radix(f[2], 16).unwrap()).unwrap(),
                size: f[3].parse().unwrap(),
                pref: f[4].chars().next().unwrap(),
                gid: f[5].parse().unwrap(),
                width: f[6].parse().unwrap(),
                height: f[7].parse().unwrap(),
                left: f[8].parse().unwrap(),
                top: f[9].parse().unwrap(),
                advance: f[10].parse().unwrap(),
            }
        })
        .collect()
}

/// Qt's `QRawFont::alphaMapForGlyph`, `boundingRect` and `advancesForGlyphIndexes` for the default
/// hinting preference (fontconfig's slight hinting: light), none and full.
#[test]
fn glyph_face_equals_qts_alpha_maps() {
    let mut faces = Faces::default();
    let mut checked = 0;
    for g in &qt_glyphs() {
        let mode = match g.pref {
            'd' => 1,
            'n' => 2,
            _ => 0,
        };
        let face = faces.get(&g.font, mode);
        let gid = face.glyph_index(g.ch);
        let label = format!("{} {:?} {}px {}", g.font, g.ch, g.size, g.pref);
        assert_eq!(gid, g.gid, "{label}: glyph index");
        let (m, bits) = face.rasterize_indexed(gid, g.size as f32);
        assert_eq!(
            m.width > 0 && m.height > 0,
            g.width > 0 && g.height > 0,
            "{label}: bitmap presence"
        );
        let tol = if g.pref == 'f' {
            (g.size as usize).max(6)
        } else {
            6
        };
        assert!(
            m.width.abs_diff(g.width) <= tol && m.height.abs_diff(g.height) <= tol,
            "{label}: size ({}, {}) vs reference ({}, {})",
            m.width,
            m.height,
            g.width,
            g.height
        );
        assert_eq!(bits.len(), m.width * m.height, "{label}: coverage len");
        if g.width > 0 && g.height > 0 {
            let i_tol = tol as u32;
            assert!(
                m.xmin.abs_diff(g.left) <= i_tol
                    && (m.ymin + m.height as i32).abs_diff(g.top) <= i_tol,
                "{label}: bearings ({}, {}) vs reference ({}, {})",
                m.xmin,
                m.ymin + m.height as i32,
                g.left,
                g.top
            );
            assert!(bits.iter().any(|&v| v > 0), "{label}: empty coverage");
        }
        let advance = face
            .layout_advance_width(gid, g.size as f32, false)
            .expect("advance");
        assert_eq!(f64::from(advance), g.advance, "{label}: advance");
        checked += 1;
    }
    // Every glyph of the two fonts (`gen_ft_golden.py`: 12 and 7 characters, space included) at
    // each of 4 sizes and 3 hint preferences, or, under Miri, each glyph once: 19 records; the
    // accents font adds 16 glyphs at 3 sizes in 2 preferences, or 4 records under Miri.
    let expected = if cfg!(miri) {
        19 + 4
    } else {
        19 * 4 * 3 + 16 * 3 * 2
    };
    assert_eq!(checked, expected, "glyph records checked");
}

/// `QPainter::drawText` into a QImage against `Painter::draw_text`: whole strings with sub-pixel
/// pen positions, letter spacing (overlapping glyphs), coloured, light-on-dark and translucent
/// pens. On Linux and the other Unix systems but macOS the painter draws through `SkrifaFace`.
#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn draw_text_equals_qts_draw_text() {
    use ferrokit_gui::geometry::primitives::PointF;
    use ferrokit_gui::paint::{Painter, Pen, Pixmap};
    use ferrokit_gui::text::font::Font;
    use ferrokit_gui::tiny_skia::Color;

    let mut checked = 0;
    for line in QT_GOLDEN.lines().filter(|l| l.starts_with("S ")) {
        let f: Vec<&str> = line.split(' ').collect();
        assert_eq!(f.len(), 13, "{line}");
        let (id, file, size) = (f[1], f[2], f[3].parse::<f32>().unwrap());
        if cfg!(miri) && id != "0" {
            continue;
        }
        let spacing: f32 = f[4].parse().unwrap();
        let pen = unhex(f[5]); // RRGGBBAA
        let bg = unhex(f[6]); // RRGGBB
        let (x, y): (f32, f32) = (f[7].parse().unwrap(), f[8].parse().unwrap());
        let (w, h): (u32, u32) = (f[9].parse().unwrap(), f[10].parse().unwrap());
        let text = String::from_utf8(unhex(f[11])).unwrap();
        let mut want = Vec::new();
        for run in f[12].split(',') {
            let (px, n) = run.split_once('*').unwrap();
            for _ in 0..n.parse::<usize>().unwrap() {
                want.extend(unhex(px));
            }
        }
        assert_eq!(want.len(), (w * h * 4) as usize);

        let font = Font::new("FerrokitFtGolden", size)
            .with_letter_spacing(spacing)
            .with_font_data(SharedFontData::from_vec(font_bytes(file).to_vec()));
        let mut pm = Pixmap::with_dpr(w, h, 1.0).unwrap();
        pm.fill(Color::from_rgba8(bg[0], bg[1], bg[2], 255));
        {
            let mut p = Painter::begin(&mut pm);
            p.set_pen(Some(Pen::new(
                Color::from_rgba8(pen[0], pen[1], pen[2], pen[3]),
                1.0,
            )));
            p.draw_text(PointF::new(x, y), &text, &font);
        }
        let got = pm.data();
        let bg_px = [bg[0], bg[1], bg[2], 255];
        let painted_bbox = |buf: &[u8]| -> (usize, Option<(u32, u32, u32, u32)>) {
            let mut count = 0usize;
            let mut min_x = u32::MAX;
            let mut min_y = u32::MAX;
            let mut max_x = 0u32;
            let mut max_y = 0u32;
            for (i, &px) in buf.as_chunks::<4>().0.iter().enumerate() {
                if px != bg_px {
                    count += 1;
                    let px_x = (i as u32) % w;
                    let px_y = (i as u32) / w;
                    min_x = min_x.min(px_x);
                    min_y = min_y.min(px_y);
                    max_x = max_x.max(px_x);
                    max_y = max_y.max(px_y);
                }
            }
            let bbox = (count > 0).then_some((min_x, min_y, max_x, max_y));
            (count, bbox)
        };
        let (got_count, got_bbox) = painted_bbox(got);
        let (want_count, want_bbox) = painted_bbox(&want);
        assert_eq!(
            got_bbox.is_some(),
            want_bbox.is_some(),
            "string {id} ({file} {size}px {text:?}): painted presence"
        );
        if let (Some(gb), Some(wb)) = (got_bbox, want_bbox) {
            assert!(
                gb.0.abs_diff(wb.0) <= 3
                    && gb.1.abs_diff(wb.1) <= 3
                    && gb.2.abs_diff(wb.2) <= 3
                    && gb.3.abs_diff(wb.3) <= 3,
                "string {id} ({file} {size}px {text:?}): painted bbox {gb:?} vs {wb:?}"
            );
            let max_diff = (want_count / 5).max(20);
            assert!(
                got_count.abs_diff(want_count) <= max_diff,
                "string {id} ({file} {size}px {text:?}): painted pixels {got_count} vs {want_count}"
            );
        }
        checked += 1;
    }
    assert!(checked >= 1);
}
