// SPDX-License-Identifier: LGPL-3.0-only
//! `FontMetrics` of an in-memory font against Qt 6.11.2 (PySide6, Windows; the numbers are
//! hardcoded from `QFontMetrics`, `QFontMetricsF`, `QLabel` runs of `tests/data/oracle_text.ttf`
//! registered with `QFontDatabase.addApplicationFontFromData`).
//!
//! Qt 6 derives the line metrics of every engine from the font's tables
//! (`QFontEngine::initializeHeightMetrics`, qfontengine.cpp:419-448): the `hhea` ascender,
//! descender and line gap, replaced by the OS/2 typographic values (with their line gap) when
//! `fsSelection` has `USE_TYPO_METRICS`, and by the OS/2 Windows ascent and descent (leading 0)
//! otherwise. `QFontMetricsF` reports those 26.6 values; `QFontMetrics` rounds each separately
//! (`qRound(ascent) + qRound(descent)` is `height()`, 13 + 4 = 17 for a 12.5 + 3.5 line). Where the
//! engine has no sub-pixel positions (DirectWrite at a device pixel ratio of 1) the engine itself
//! rounds ascent and descent and the leading takes up the rounding of the line (13 + 4 - 16 = -1).
//! The average character width of Qt's Windows engines is the design advance of `x`
//! (`QFontEngine::averageCharWidth`, qfontengine.cpp:246-251): the oracle font's `xAvgCharWidth`
//! (523) is not what Qt reports there.
//!
//! The ratio is process-wide state, so every test takes the lock and sets what it needs.
use ferrokit_gui::text::font::{Font, SharedFontData};
use ferrokit_gui::text::font_database::set_application_device_pixel_ratio;
use ferrokit_gui::text::font_metrics::FontMetrics;
use std::sync::Mutex;

const ORACLE_TTF: &[u8] = include_bytes!("data/oracle_text.ttf");

static DPR: Mutex<()> = Mutex::new(());

fn font(data: Vec<u8>, px: f32) -> Font {
    Font::new("FerrokitOracleText", px).with_font_data(SharedFontData::from_vec(data))
}

/// `QFontMetrics` (ascent, descent, leading, height, lineSpacing) of `font`.
fn ints(font: &Font) -> [i32; 5] {
    let m = FontMetrics::from_font(font);
    [
        m.ascent as i32,
        m.descent as i32,
        m.leading() as i32,
        m.height as i32,
        m.line_spacing() as i32,
    ]
}

/// `QFontMetricsF` (ascent, descent, leading, height) of `font`.
fn floats(font: &Font) -> [f32; 4] {
    let line = FontMetrics::engine_metrics(font).expect("the face has line metrics");
    [
        line.ascent,
        line.descent,
        line.leading,
        FontMetrics::layout_height(font),
    ]
}

/// `(px, QFontMetrics (ascent, descent, leading, height, lineSpacing), QFontMetricsF (ascent,
/// descent, leading, height))` of the oracle font under Qt's DirectWrite engine (a device pixel
/// ratio of 1.25; the same at 2.5).
const DIRECTWRITE: [(f32, [i32; 5], [f32; 4]); 7] = [
    (8.0, [6, 2, 0, 8, 8], [6.25, 1.75, 0.0, 8.0]),
    (11.0, [9, 2, 0, 11, 11], [8.59375, 2.40625, 0.0, 11.0]),
    (12.0, [9, 3, 0, 12, 12], [9.375, 2.625, 0.0, 12.0]),
    (13.0, [10, 3, 0, 13, 13], [10.15625, 2.84375, 0.0, 13.0]),
    (16.0, [13, 4, 0, 17, 17], [12.5, 3.5, 0.0, 16.0]),
    (24.0, [19, 5, 0, 24, 24], [18.75, 5.25, 0.0, 24.0]),
    (31.0, [24, 7, 0, 31, 31], [24.21875, 6.78125, 0.0, 31.0]),
];

/// The same at a device pixel ratio of 1 (`QT_SCALE_FACTOR=0.8` on a 125 % display): the engine
/// rounds, `QFontMetricsF` reports whole pixels and a leading of -1 at 16 px.
#[cfg(not(target_os = "macos"))]
const ROUNDED: [(f32, [i32; 5], [f32; 4]); 7] = [
    (8.0, [6, 2, 0, 8, 8], [6.0, 2.0, 0.0, 8.0]),
    (11.0, [9, 2, 0, 11, 11], [9.0, 2.0, 0.0, 11.0]),
    (12.0, [9, 3, 0, 12, 12], [9.0, 3.0, 0.0, 12.0]),
    (13.0, [10, 3, 0, 13, 13], [10.0, 3.0, 0.0, 13.0]),
    (16.0, [13, 4, -1, 17, 16], [13.0, 4.0, -1.0, 17.0]),
    (24.0, [19, 5, 0, 24, 24], [19.0, 5.0, 0.0, 24.0]),
    (31.0, [24, 7, 0, 31, 31], [24.0, 7.0, 0.0, 31.0]),
];

#[test]
fn an_in_memory_font_takes_its_line_metrics_from_its_tables() {
    let _guard = DPR.lock().unwrap_or_else(|e| e.into_inner());
    set_application_device_pixel_ratio(1.25);
    for (px, expected_ints, expected_floats) in DIRECTWRITE {
        let f = font(ORACLE_TTF.to_vec(), px);
        assert_eq!(ints(&f), expected_ints, "{px}px QFontMetrics");
        assert_eq!(floats(&f), expected_floats, "{px}px QFontMetricsF");
        // The text layout measures a line by the unrounded sum: 16 px, not `height()`'s 17.
        assert_eq!(FontMetrics::layout_height(&f), expected_floats[3], "{px}px");
    }
    set_application_device_pixel_ratio(1.0);
}

#[cfg(windows)]
#[test]
fn an_engine_without_sub_pixel_positions_rounds_and_moves_the_difference_into_the_leading() {
    let _guard = DPR.lock().unwrap_or_else(|e| e.into_inner());
    set_application_device_pixel_ratio(1.0);
    for (px, expected_ints, expected_floats) in ROUNDED {
        let f = font(ORACLE_TTF.to_vec(), px);
        assert_eq!(ints(&f), expected_ints, "{px}px QFontMetrics");
        assert_eq!(floats(&f), expected_floats, "{px}px QFontMetricsF");
    }
}

/// With full hinting (`PreferFullHinting`), an engine disables sub-pixel positions and rounds
/// line metrics, moving the difference into leading.
#[cfg(not(target_os = "macos"))]
#[test]
fn prefer_full_hinting_disables_sub_pixel_positions_and_rounds_line_metrics() {
    use ferrokit_gui::text::HintingPreference;
    let _guard = DPR.lock().unwrap_or_else(|e| e.into_inner());
    set_application_device_pixel_ratio(1.0);
    for (px, expected_ints, expected_floats) in ROUNDED {
        let f = font(ORACLE_TTF.to_vec(), px)
            .with_hinting_preference(HintingPreference::PreferFullHinting);
        assert_eq!(
            ints(&f),
            expected_ints,
            "{px}px QFontMetrics (PreferFullHinting)"
        );
        assert_eq!(
            floats(&f),
            expected_floats,
            "{px}px QFontMetricsF (PreferFullHinting)"
        );
    }
}

/// On Linux (FreeType with default `HintLight` / fontconfig `hintslight`) and macOS (CoreText),
/// engines keep fractional line metrics at ratio 1.0 under default hinting preference.
#[cfg(not(windows))]
#[test]
fn linux_and_macos_engines_keep_fractional_metrics_at_a_ratio_of_one() {
    let _guard = DPR.lock().unwrap_or_else(|e| e.into_inner());
    set_application_device_pixel_ratio(1.0);
    for (px, expected_ints, expected_floats) in DIRECTWRITE {
        let f = font(ORACLE_TTF.to_vec(), px);
        assert_eq!(ints(&f), expected_ints, "{px}px QFontMetrics");
        assert_eq!(floats(&f), expected_floats, "{px}px QFontMetricsF");
        assert_eq!(FontMetrics::layout_height(&f), expected_floats[3], "{px}px");
    }
}

/// Switching the default hint style between HintLight and HintFull toggles sub-pixel rounding
/// at a ratio of 1.0 on FreeType/DirectWrite platforms (macOS CoreText never rounds).
#[cfg(not(target_os = "macos"))]
#[test]
fn default_hint_style_switch_controls_sub_pixel_rounding() {
    use ferrokit_gui::text::font_database::{
        default_hint_style, set_default_hint_style, HintStyle,
    };
    struct HintGuard(HintStyle);
    impl Drop for HintGuard {
        fn drop(&mut self) {
            set_default_hint_style(self.0);
        }
    }
    let _guard = DPR.lock().unwrap_or_else(|e| e.into_inner());
    let _hint_guard = HintGuard(default_hint_style());
    set_application_device_pixel_ratio(1.0);

    // HintFull rounds
    set_default_hint_style(HintStyle::HintFull);
    let f16 = font(ORACLE_TTF.to_vec(), 16.0);
    assert_eq!(floats(&f16), [13.0, 4.0, -1.0, 17.0]);

    // HintLight keeps fractional
    set_default_hint_style(HintStyle::HintLight);
    let f16 = font(ORACLE_TTF.to_vec(), 16.0);
    assert_eq!(floats(&f16), [12.5, 3.5, 0.0, 16.0]);

    // HintNone keeps fractional
    set_default_hint_style(HintStyle::HintNone);
    let f16 = font(ORACLE_TTF.to_vec(), 16.0);
    assert_eq!(floats(&f16), [12.5, 3.5, 0.0, 16.0]);
}

/// Table offset of `tag` in an sfnt.
fn table(data: &[u8], tag: &[u8; 4]) -> usize {
    let count = usize::from(u16::from_be_bytes([data[4], data[5]]));
    (0..count)
        .map(|i| 12 + 16 * i)
        .find(|&rec| &data[rec..rec + 4] == tag)
        .map(|rec| u32::from_be_bytes(data[rec + 8..rec + 12].try_into().unwrap()) as usize)
        .expect("table")
}

/// The oracle font with its `hhea` (ascender, descender, line gap) and OS/2 (`fsSelection`, typo
/// ascender, descender, line gap, win ascent, win descent) values replaced.
fn patched(hhea: [i16; 3], os2: (u16, [i16; 3], [u16; 2])) -> Vec<u8> {
    let mut data = ORACLE_TTF.to_vec();
    let at = table(&data, b"hhea") + 4;
    for (i, v) in hhea.iter().enumerate() {
        data[at + 2 * i..at + 2 * i + 2].copy_from_slice(&v.to_be_bytes());
    }
    let at = table(&data, b"OS/2");
    data[at + 62..at + 64].copy_from_slice(&os2.0.to_be_bytes());
    for (i, v) in os2.1.iter().enumerate() {
        data[at + 68 + 2 * i..at + 70 + 2 * i].copy_from_slice(&v.to_be_bytes());
    }
    for (i, v) in os2.2.iter().enumerate() {
        data[at + 74 + 2 * i..at + 76 + 2 * i].copy_from_slice(&v.to_be_bytes());
    }
    data
}

/// Variants of the oracle font's tables measured with Qt 6.11.2 under DirectWrite: per size,
/// `QFontMetrics` (ascent, descent, leading, height, lineSpacing), `QFontMetricsF` (ascent,
/// descent, leading, height) and the height of a `QLabel("Hello\nworld")` size hint.
type Variant = (
    &'static str,
    [i16; 3],
    (u16, [i16; 3], [u16; 2]),
    [(f32, [i32; 5], [f32; 4], i32); 3],
);

const VARIANTS: [Variant; 4] = [
    // No USE_TYPO_METRICS: the OS/2 Windows ascent and descent win over `hhea`, and the line gap
    // of `hhea` (100) is dropped.
    (
        "win metrics",
        [800, -224, 100],
        (0, [700, -300, 128], [900, 300]),
        [
            (
                12.0,
                [11, 4, 0, 15, 15],
                [10.546875, 3.515625, 0.0, 14.0625],
                30,
            ),
            (16.0, [14, 5, 0, 19, 19], [14.0625, 4.6875, 0.0, 18.75], 38),
            (
                24.0,
                [21, 7, 0, 28, 28],
                [21.09375, 7.03125, 0.0, 28.125],
                58,
            ),
        ],
    ),
    // USE_TYPO_METRICS: the typographic ascender, descender and line gap.
    (
        "typo metrics",
        [800, -224, 100],
        (0x80, [700, -300, 128], [900, 300]),
        [
            (
                12.0,
                [8, 4, 2, 12, 14],
                [8.203125, 3.515625, 1.5, 11.71875],
                26,
            ),
            (16.0, [11, 5, 2, 16, 18], [10.9375, 4.6875, 2.0, 15.625], 34),
            (
                24.0,
                [16, 7, 3, 23, 26],
                [16.40625, 7.03125, 3.0, 23.4375],
                51,
            ),
        ],
    ),
    // USE_TYPO_METRICS with no typographic ascender or descender: invalid, `hhea` (with its
    // gap) stays.
    (
        "typo flag, empty typo metrics",
        [800, -224, 96],
        (0x80, [0, 0, 50], [900, 300]),
        [
            (12.0, [9, 3, 1, 12, 13], [9.375, 2.625, 1.125, 12.0], 26),
            (16.0, [13, 4, 2, 17, 19], [12.5, 3.5, 1.5, 16.0], 34),
            (24.0, [19, 5, 2, 24, 26], [18.75, 5.25, 2.25, 24.0], 51),
        ],
    ),
    // No Windows ascent or descent either: `hhea` again.
    (
        "empty win metrics",
        [800, -224, 96],
        (0, [700, -300, 128], [0, 0]),
        [
            (12.0, [9, 3, 1, 12, 13], [9.375, 2.625, 1.125, 12.0], 26),
            (16.0, [13, 4, 2, 17, 19], [12.5, 3.5, 1.5, 16.0], 34),
            (24.0, [19, 5, 2, 24, 26], [18.75, 5.25, 2.25, 24.0], 51),
        ],
    ),
];

#[cfg(not(target_os = "macos"))]
#[test]
fn hhea_typographic_and_windows_metrics_are_chosen_as_qt_chooses_them() {
    let _guard = DPR.lock().unwrap_or_else(|e| e.into_inner());
    set_application_device_pixel_ratio(1.25);
    for (name, hhea, os2, rows) in VARIANTS {
        let data = patched(hhea, os2);
        for (px, expected_ints, expected_floats, two_lines) in rows {
            let f = font(data.clone(), px);
            assert_eq!(ints(&f), expected_ints, "{name} {px}px QFontMetrics");
            assert_eq!(floats(&f), expected_floats, "{name} {px}px QFontMetricsF");
            // `QLabel("Hello\nworld")`: the second line starts a pitch (height plus leading,
            // rounded up) below the first, the last line adds its own height.
            let two = FontMetrics::line_pitch(&f) + FontMetrics::line_height(&f);
            assert_eq!(two as i32, two_lines, "{name} {px}px two lines");
        }
    }
    set_application_device_pixel_ratio(1.0);
}

/// The outline backend reads the same tables itself, with no font database in the way (so this
/// one also runs under Miri): the engine's 26.6 values, whole pixels after `rounded()` (13 + 4 with
/// a leading of -1 for 12.5 + 3.5), and `xAvgCharWidth` scaled as FreeType and CoreText scale it.
/// CoreText takes the `hhea` values and ignores OS/2 ones, so the variants are not checked there.
#[test]
fn the_outline_backend_reads_line_metrics_and_average_width_from_the_tables() {
    use ferrokit_gui::text::glyph_face::{GlyphFace, LineMetrics};
    use ferrokit_gui::text::outline_face::OutlineFace;
    let face = |data: Vec<u8>| OutlineFace::new(SharedFontData::from_vec(data), 0).expect("face");

    let oracle = face(ORACLE_TTF.to_vec());
    let line = GlyphFace::line_metrics(&oracle, 16.0).expect("line metrics");
    assert_eq!(
        line,
        LineMetrics {
            ascent: 12.5,
            descent: 3.5,
            leading: 0.0
        }
    );
    let rounded = line.rounded();
    assert_eq!(
        (rounded.ascent, rounded.descent, rounded.leading),
        (13.0, 4.0, -1.0)
    );
    // 523 * 16 / 1024 and 523 * 32 / 1024 are exact in 26.6, so FreeType and CoreText agree.
    for (px, average) in [(16.0, 8.171875), (32.0, 16.34375)] {
        assert_eq!(GlyphFace::average_char_width(&oracle, px), Some(average));
    }

    if cfg!(target_os = "macos") {
        return;
    }
    for (name, hhea, os2, rows) in VARIANTS {
        let variant = face(patched(hhea, os2));
        for (px, _, expected, _) in rows {
            let line = GlyphFace::line_metrics(&variant, px).expect("line metrics");
            assert_eq!(
                [line.ascent, line.descent, line.leading],
                [expected[0], expected[1], expected[2]],
                "{name} {px}px"
            );
        }
    }
}

/// `QFontMetricsF::averageCharWidth` of the oracle font, `QFontMetrics::averageCharWidth` after it:
/// the design advance of `x` truncated to 26.6 on Windows (7.75 at 16 px, 15.015625 at 31 px),
/// not the 8.17 or 15.83 of `xAvgCharWidth`.
#[cfg(windows)]
#[test]
fn the_average_char_width_of_the_windows_engines_is_the_advance_of_x() {
    let _guard = DPR.lock().unwrap_or_else(|e| e.into_inner());
    for dpr in [1.0, 1.25] {
        set_application_device_pixel_ratio(dpr);
        for (px, avg_f, avg) in [
            (8.0, 3.875, 4),
            (11.0, 5.328125, 5),
            (12.0, 5.8125, 6),
            (13.0, 6.296875, 6),
            (16.0, 7.75, 8),
            (24.0, 11.625, 12),
            (31.0, 15.015625, 15),
        ] {
            let m = FontMetrics::from_font(&font(ORACLE_TTF.to_vec(), px));
            assert_eq!(m.average_char_width, avg_f, "{px}px QFontMetricsF");
            assert_eq!(
                m.rounded_average_char_width() as i32,
                avg,
                "{px}px QFontMetrics"
            );
        }
    }
    set_application_device_pixel_ratio(1.0);
}

/// Qt's FreeType and CoreText engines scale the OS/2 `xAvgCharWidth` instead
/// (qfontengine_ft.cpp:2167-2178, qfontengine_coretext.mm:240-251): 523 * 16 / 1024 = 8.171875.
/// [inferred] from the Qt source; checked against Qt's FreeType engine only on Windows
/// (`QT_QPA_PLATFORM=windows:fontengine=freetype`, system fonts).
#[cfg(not(windows))]
#[test]
fn the_average_char_width_of_the_other_engines_is_the_os2_average() {
    for (px, avg_f, avg) in [(16.0, 8.171875, 8), (32.0, 16.34375, 16)] {
        let m = FontMetrics::from_font(&font(ORACLE_TTF.to_vec(), px));
        assert_eq!(m.average_char_width, avg_f, "{px}px QFontMetricsF");
        assert_eq!(
            m.rounded_average_char_width() as i32,
            avg,
            "{px}px QFontMetrics"
        );
    }
}

/// The bytes of the installed font `file` if it is the very file measured (same length): the
/// numbers below belong to one version of it.
#[cfg(windows)]
fn measured_font(file: &str) -> Option<Vec<u8>> {
    let length = match file {
        "arial.ttf" => 1_045_720,
        "segoeui.ttf" => 959_752,
        _ => return None,
    };
    let bytes = std::fs::read(format!("C:/Windows/Fonts/{file}")).ok()?;
    (bytes.len() == length).then_some(bytes)
}
/// The generic outline backend plays Qt's FreeType engine, which rounds ascent and descent (full
/// hinting) and scales `xAvgCharWidth` as `QFixed(xAvgCharWidth * x_ppem) / emSquareSize()`.
/// Measured with PySide6 6.11.2 (`QT_QPA_PLATFORM=windows:fontengine=freetype`, installed fonts):
/// per font file and size, `QFontMetricsF` ascent, descent, leading and averageCharWidth.
#[cfg(windows)]
#[test]
fn the_outline_backend_matches_qts_freetype_engine() {
    use ferrokit_gui::text::glyph_face::GlyphFace;
    use ferrokit_gui::text::outline_face::OutlineFace;
    for (file, px, ascent, descent, leading, average) in [
        ("arial.ttf", 31.0, 28.0, 7.0, 0.0, 13.6875),
        ("arial.ttf", 16.0, 14.0, 3.0, 1.0, 7.0625),
        ("segoeui.ttf", 12.0, 13.0, 3.0, 0.0, 6.46875),
    ] {
        let Some(bytes) = measured_font(file) else {
            continue; // not installed, or not the version measured
        };
        let face = OutlineFace::new(SharedFontData::from_vec(bytes), 0).expect("outline face");
        let line = GlyphFace::line_metrics(&face, px)
            .expect("line metrics")
            .rounded();
        assert_eq!(
            (line.ascent, line.descent, line.leading),
            (ascent, descent, leading),
            "{file} {px}px"
        );
        assert_eq!(
            GlyphFace::average_char_width(&face, px),
            Some(average),
            "{file} {px}px"
        );
    }
}

/// A pixel size no font has must not overflow the 26.6 arithmetic of the average width or the
/// line metrics (a debug build panics on integer overflow); NaN and infinity neither.
#[test]
fn absurd_pixel_sizes_do_not_overflow() {
    use ferrokit_gui::text::glyph_face::GlyphFace;
    use ferrokit_gui::text::outline_face::OutlineFace;
    let face = OutlineFace::new(SharedFontData::from_vec(ORACLE_TTF.to_vec()), 0).expect("face");
    for px in [1.0e9, f32::MAX, f32::INFINITY, f32::NAN, -5.0, 0.0] {
        let _ = GlyphFace::average_char_width(&face, px);
        let _ = GlyphFace::line_metrics(&face, px);
    }
    // Past FreeType's 16-bit ppem the width is that of the largest strike, not a wrapped one
    // (CoreText scales by the size itself).
    if !cfg!(target_os = "macos") {
        assert_eq!(
            GlyphFace::average_char_width(&face, 1.0e9),
            GlyphFace::average_char_width(&face, 65535.0)
        );
    }
}
