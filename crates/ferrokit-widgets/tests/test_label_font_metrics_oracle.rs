// SPDX-License-Identifier: LGPL-3.0-only
//! `QLabel::sizeHint` and `minimumSizeHint` of labels whose line height, leading and average
//! character width come from the font's tables, against Qt 6.11.2 (PySide6, Windows; the numbers
//! are hardcoded from the runs of `QLabel` with the oracle font of the gui tests registered with
//! `QFontDatabase.addApplicationFontFromData`, and with installed fonts).
//!
//! What the label asks of the font:
//! - one line is `ascent + descent` of the engine (unrounded: 12.5 + 3.5 = 16 px), rounded up with
//!   the box; `QFontMetrics::height` (13 + 4 = 17) is not what it measures;
//! - lines are stacked as `qt_format_text` does: the leading between them, the top of each line
//!   rounded up (a ratio of 1 makes the engine round and gives that font a leading of -1);
//! - a wrapping text starts from `averageCharWidth() * 80` (whole pixels: 8 * 80 at 16 px, not
//!   the 9.6 * 80 of a guessed 0.6 em), and is narrowed to half and a quarter of it while it stays
//!   shorter than 4 and 2 `lineSpacing()`s;
//! - `minimumSizeHint` of a wrapping label is the widest word by one line;
//! - a label that never had text is `averageCharWidth() x lineSpacing()`.
//!
//! The ratio is process-wide state; each test takes the lock and sets what it needs. Every
//! advance of the oracle font is a multiple of 1/64 px, so its widths do not depend on the font
//! engine; its average character width does (Windows: the advance of `x`; FreeType and CoreText:
//! `xAvgCharWidth`), and the two round to the same pixel only at 8, 12, 16 and 24 px, which are
//! the sizes checked off Windows. At a ratio of 1 Qt's GDI-style hinted advances enter the
//! widths, so those rows are Windows only.
use ferrokit_gui::text::font::{Font, SharedFontData};
use ferrokit_gui::text::font_database::set_application_device_pixel_ratio;
#[cfg(windows)]
use ferrokit_gui::text::font_metrics::FontMetrics;
use ferrokit_widgets::{Label, Widget};
use std::sync::Mutex;

static DPR: Mutex<()> = Mutex::new(());

const ORACLE_TTF: &[u8] = include_bytes!("../../ferrokit-gui/tests/data/oracle_text.ttf");

/// `(sizeHint width, sizeHint height, minimumSizeHint width, minimumSizeHint height)`.
type Hint = (i32, i32, i32, i32);

/// The five texts of every row: a line, three lines (one empty), a wrapping `"word " * 40`, a
/// wrapping text with an empty line, and no text.
fn hints(font: &Font) -> [Hint; 5] {
    let words = "word ".repeat(40);
    let texts = [
        ("Hello world", false),
        ("Hello\nworld\n\nend", false),
        (words.as_str(), true),
        ("abc def\n\nghi jkl mno pqr stu vwx", true),
        ("", false),
    ];
    texts.map(|(text, wrap)| {
        let mut label = Label::new(text);
        label.set_font(font.clone());
        label.set_word_wrap(wrap);
        let (hint, minimum) = (label.size_hint(), label.minimum_size_hint());
        (hint.width, hint.height, minimum.width, minimum.height)
    })
}

fn oracle(px: f32) -> Font {
    Font::new("FerrokitOracleText", px)
        .with_font_data(SharedFontData::from_vec(ORACLE_TTF.to_vec()))
}

/// Oracle font under Qt's DirectWrite engine (a ratio of 1.25 or any but 1).
const DIRECTWRITE: [(f32, [Hint; 5]); 7] = [
    (
        8.0,
        [
            (43, 8, 43, 8),
            (21, 32, 21, 32),
            (155, 40, 16, 8),
            (81, 24, 13, 24),
            (4, 8, 4, 8),
        ],
    ),
    (
        11.0,
        [
            (59, 11, 59, 11),
            (29, 44, 29, 44),
            (192, 55, 22, 11),
            (111, 33, 17, 33),
            (5, 11, 5, 11),
        ],
    ),
    (
        12.0,
        [
            (64, 12, 64, 12),
            (32, 48, 32, 48),
            (233, 60, 24, 12),
            (122, 36, 19, 36),
            (6, 12, 6, 12),
        ],
    ),
    (
        13.0,
        [
            (69, 13, 69, 13),
            (35, 52, 35, 52),
            (227, 65, 26, 13),
            (132, 39, 20, 39),
            (6, 13, 6, 13),
        ],
    ),
    // `"word " * 40` is 310x80 here; a guessed 0.6 em average width and 17.6 px lines made it
    // 380x71.
    (
        16.0,
        [
            (85, 16, 85, 16),
            (42, 64, 42, 64),
            (310, 80, 31, 16),
            (162, 48, 25, 48),
            (8, 17, 8, 17),
        ],
    ),
    (
        24.0,
        [
            (128, 24, 128, 24),
            (63, 96, 63, 96),
            (465, 120, 47, 24),
            (243, 72, 37, 72),
            (12, 24, 12, 24),
        ],
    ),
    (
        31.0,
        [
            (165, 31, 165, 31),
            (82, 124, 82, 124),
            (541, 155, 60, 31),
            (313, 93, 48, 93),
            (15, 31, 15, 31),
        ],
    ),
];

#[test]
fn labels_of_an_in_memory_font_are_sized_as_qt_sizes_them() {
    let _guard = DPR.lock().unwrap_or_else(|e| e.into_inner());
    set_application_device_pixel_ratio(1.25);
    for (px, expected) in DIRECTWRITE {
        if !cfg!(windows) && ![8.0, 12.0, 16.0, 24.0].contains(&px) {
            continue;
        }
        assert_eq!(hints(&oracle(px)), expected, "{px}px");
    }
    set_application_device_pixel_ratio(1.0);
}

/// `sizeForWidth` starts with `if (minimumWidth() > 0) w = qMax(w, minimumWidth())`
/// (qlabel.cpp:536-537), so a wrapping label with a minimum width has no width of its own choice:
/// its hints wrap at that width. Measured (oracle font, 16 px, `"word " * 40`, `setMinimumWidth`):
/// 0: 310x80 (minimum 31x16), 50: 50x640 (50x16), 200: 200x128 (200x16), 400: 400x64 (400x16);
/// a label that does not wrap is 1395x16 whatever the minimum.
#[test]
fn a_minimum_width_makes_a_wrapping_label_wrap_at_it() {
    let _guard = DPR.lock().unwrap_or_else(|e| e.into_inner());
    set_application_device_pixel_ratio(1.25);
    for (minimum, wrap, expected) in [
        (0, true, (310, 80, 31, 16)),
        (50, true, (50, 640, 50, 16)),
        (200, true, (200, 128, 200, 16)),
        (400, true, (400, 64, 400, 16)),
        (200, false, (1395, 16, 1395, 16)),
    ] {
        let mut label = Label::new("word ".repeat(40));
        label.set_font(oracle(16.0));
        label.set_word_wrap(wrap);
        label.set_minimum_size(ferrokit_gui::geometry::primitives::Size::new(minimum, 0));
        let (hint, least) = (label.size_hint(), label.minimum_size_hint());
        assert_eq!(
            (hint.width, hint.height, least.width, least.height),
            expected,
            "minimum {minimum}, wrap {wrap}"
        );
    }
    set_application_device_pixel_ratio(1.0);
}

/// `QLabel("x")` then `setText("")` is a text label with no text: no width and one line's height
/// (12 and 16 px here), where a label that never had text is `averageCharWidth() x lineSpacing()`
/// (6x12 and 8x17). `QLabel("")` is the second: its `setText("")` returns early.
#[test]
fn a_label_whose_text_was_cleared_is_not_the_label_that_never_had_text() {
    let _guard = DPR.lock().unwrap_or_else(|e| e.into_inner());
    set_application_device_pixel_ratio(1.25);
    for (px, never, cleared) in [(12.0, (6, 12), (0, 12)), (16.0, (8, 17), (0, 16))] {
        for word_wrap in [false, true] {
            let mut label = Label::new("x");
            label.set_font(oracle(px));
            label.set_word_wrap(word_wrap);
            label.set_text("");
            let size = |l: &Label| (l.size_hint().width, l.size_hint().height);
            assert_eq!(
                size(&label),
                cleared,
                "{px}px cleared, word_wrap={word_wrap}"
            );
            let minimum = label.minimum_size_hint();
            assert_eq!((minimum.width, minimum.height), cleared, "{px}px minimum");

            let mut fresh = Label::new("");
            fresh.set_font(oracle(px));
            fresh.set_word_wrap(word_wrap);
            fresh.set_text("");
            assert_eq!(
                size(&fresh),
                never,
                "{px}px never had text, word_wrap={word_wrap}"
            );
        }
    }
    set_application_device_pixel_ratio(1.0);
}

/// The oracle font at a ratio of 1 (Windows): the lines are 17 px with a leading of -1 at 16 px,
/// so three lines are 17 + 16 + 16 and the wrapping text 17 + 16 * 4 (81).
#[cfg(windows)]
#[test]
fn labels_of_an_in_memory_font_at_a_ratio_of_one_stack_lines_by_the_leading() {
    let _guard = DPR.lock().unwrap_or_else(|e| e.into_inner());
    set_application_device_pixel_ratio(1.0);
    let rows: [(f32, [Hint; 5]); 7] = [
        (
            8.0,
            [
                (43, 8, 43, 8),
                (21, 32, 21, 32),
                (160, 40, 16, 8),
                (82, 24, 12, 24),
                (4, 8, 4, 8),
            ],
        ),
        (
            11.0,
            [
                (59, 11, 59, 11),
                (30, 44, 30, 44),
                (184, 55, 20, 11),
                (112, 33, 18, 33),
                (5, 11, 5, 11),
            ],
        ),
        (
            12.0,
            [
                (65, 12, 65, 12),
                (32, 48, 32, 48),
                (240, 60, 24, 12),
                (123, 36, 18, 36),
                (6, 12, 6, 12),
            ],
        ),
        (
            13.0,
            [
                (70, 13, 70, 13),
                (36, 52, 36, 52),
                (240, 65, 24, 13),
                (130, 39, 21, 39),
                (6, 13, 6, 13),
            ],
        ),
        (
            16.0,
            [
                (87, 17, 87, 17),
                (43, 65, 43, 65),
                (320, 81, 32, 17),
                (164, 49, 24, 49),
                (8, 16, 8, 16),
            ],
        ),
        (
            24.0,
            [
                (127, 24, 127, 24),
                (63, 96, 63, 96),
                (462, 120, 46, 24),
                (240, 72, 36, 72),
                (12, 24, 12, 24),
            ],
        ),
        (
            31.0,
            [
                (167, 31, 167, 31),
                (83, 124, 83, 124),
                (544, 155, 60, 31),
                (317, 93, 48, 93),
                (15, 31, 15, 31),
            ],
        ),
    ];
    for (px, expected) in rows {
        assert_eq!(hints(&oracle(px)), expected, "{px}px");
    }
}

#[cfg(not(target_os = "macos"))]
struct HintGuard(ferrokit_gui::text::font_database::HintStyle);
#[cfg(not(target_os = "macos"))]
impl Drop for HintGuard {
    fn drop(&mut self) {
        ferrokit_gui::text::font_database::set_default_hint_style(self.0);
    }
}

/// Under HintLight (the Linux default, matching fontconfig hintslight) at ratio 1.0, labels
/// keep fractional line metrics: a 16px line is 16px tall (12.5 + 3.5), leading is 0.0,
/// so 5 lines of word*40 is 80px (not Windows's 81px) and 3 lines is 48px (not 49px).
/// Under HintFull (Windows full hinting), it rounds lines to 17px with leading -1, giving 81px and 49px.
#[cfg(not(target_os = "macos"))]
#[test]
fn label_size_hints_differ_between_hint_light_and_hint_full_at_ratio_one() {
    use ferrokit_gui::text::font_database::{
        HintStyle, default_hint_style, set_default_hint_style,
    };
    let _guard = DPR.lock().unwrap_or_else(|e| e.into_inner());
    let _hint_guard = HintGuard(default_hint_style());
    set_application_device_pixel_ratio(1.0);

    // HintLight (Linux desktop default: 80px and 48px)
    set_default_hint_style(HintStyle::HintLight);
    let mut lbl = Label::new("word ".repeat(40));
    lbl.set_font(oracle(16.0));
    lbl.set_word_wrap(true);
    assert_eq!(lbl.size_hint().height, 80);

    let mut lbl2 = Label::new("Hello\n\nworld");
    lbl2.set_font(oracle(16.0));
    lbl2.set_word_wrap(true);
    assert_eq!(lbl2.size_hint().height, 48);

    // HintFull (Windows default at ratio 1.0: 81px and 49px)
    set_default_hint_style(HintStyle::HintFull);
    let mut lbl_full = Label::new("word ".repeat(40));
    lbl_full.set_font(oracle(16.0));
    lbl_full.set_word_wrap(true);
    assert_eq!(lbl_full.size_hint().height, 81);

    let mut lbl2_full = Label::new("Hello\n\nworld");
    lbl2_full.set_font(oracle(16.0));
    lbl2_full.set_word_wrap(true);
    assert_eq!(lbl2_full.size_hint().height, 49);
}

/// `QFontMetrics` (ascent, descent, leading, height, lineSpacing, averageCharWidth) and
/// `QFontMetricsF::averageCharWidth` of an installed font at 12 and 16 px under DirectWrite,
/// with its label hints.
#[cfg(windows)]
type Installed = (&'static str, &'static str, f32, [i32; 6], f32, [Hint; 5]);

#[cfg(windows)]
const INSTALLED: [Installed; 10] = [
    (
        "Arial",
        "arial.ttf",
        12.0,
        [11, 3, 0, 14, 14, 6],
        6.0,
        [
            (60, 14, 60, 14),
            (29, 56, 29, 56),
            (235, 70, 26, 14),
            (122, 42, 24, 42),
            (6, 14, 6, 14),
        ],
    ),
    (
        "Arial",
        "arial.ttf",
        16.0,
        [14, 3, 0, 17, 17, 8],
        8.0,
        [
            (80, 18, 80, 18),
            (39, 72, 39, 72),
            (313, 90, 35, 18),
            (162, 54, 32, 54),
            (8, 17, 8, 17),
        ],
    ),
    (
        "Segoe UI",
        "segoeui.ttf",
        12.0,
        [13, 3, 0, 16, 16, 6],
        5.5,
        [
            (61, 16, 61, 16),
            (30, 64, 30, 64),
            (239, 80, 27, 16),
            (124, 48, 25, 48),
            (6, 16, 6, 16),
        ],
    ),
    (
        "Segoe UI",
        "segoeui.ttf",
        16.0,
        [17, 4, 0, 21, 21, 7],
        7.34375,
        [
            (82, 22, 82, 22),
            (40, 88, 40, 88),
            (278, 132, 36, 22),
            (165, 66, 33, 66),
            (7, 21, 7, 21),
        ],
    ),
    (
        "Consolas",
        "consola.ttf",
        12.0,
        [11, 3, 0, 14, 14, 7],
        6.59375,
        [
            (73, 15, 73, 15),
            (33, 60, 33, 60),
            (264, 75, 27, 15),
            (152, 45, 20, 45),
            (7, 14, 7, 14),
        ],
    ),
    (
        "Consolas",
        "consola.ttf",
        16.0,
        [15, 4, 0, 19, 19, 9],
        8.796875,
        [
            (97, 19, 97, 19),
            (44, 76, 44, 76),
            (352, 95, 36, 19),
            (203, 57, 27, 57),
            (9, 19, 9, 19),
        ],
    ),
    (
        "Times New Roman",
        "times.ttf",
        12.0,
        [11, 3, 0, 14, 14, 6],
        6.0,
        [
            (58, 14, 58, 14),
            (28, 56, 28, 56),
            (222, 70, 25, 14),
            (115, 42, 22, 42),
            (6, 14, 6, 14),
        ],
    ),
    (
        "Times New Roman",
        "times.ttf",
        16.0,
        [14, 3, 0, 17, 17, 8],
        8.0,
        [
            (77, 18, 77, 18),
            (38, 72, 38, 72),
            (295, 90, 33, 18),
            (154, 54, 29, 54),
            (8, 17, 8, 17),
        ],
    ),
    (
        "Microsoft JhengHei UI",
        "msjh.ttc",
        12.0,
        [12, 3, 0, 15, 15, 6],
        5.859375,
        [
            (65, 16, 65, 16),
            (32, 64, 32, 64),
            (219, 96, 29, 16),
            (130, 48, 26, 48),
            (6, 15, 6, 15),
        ],
    ),
    (
        "Microsoft JhengHei UI",
        "msjh.ttc",
        16.0,
        [16, 4, 0, 20, 20, 8],
        7.828125,
        [
            (87, 21, 87, 21),
            (43, 84, 43, 84),
            (293, 126, 39, 21),
            (173, 63, 35, 63),
            (8, 20, 8, 20),
        ],
    ),
];

/// Whether `C:/Windows/Fonts/<file>` is the file the numbers were measured with (same length):
/// another version of a font has other tables and other advances.
#[cfg(windows)]
fn is_the_measured_file(file: &str) -> bool {
    let length: u64 = match file {
        "arial.ttf" => 1_045_720,
        "segoeui.ttf" => 959_752,
        "consola.ttf" => 453_088,
        "times.ttf" => 1_190_736,
        "msjh.ttc" => 21_404_172,
        _ => return false,
    };
    std::fs::metadata(std::path::Path::new("C:/Windows/Fonts").join(file))
        .is_ok_and(|m| m.len() == length)
}

#[cfg(windows)]
#[test]
fn labels_of_installed_fonts_are_sized_as_qt_sizes_them() {
    let _guard = DPR.lock().unwrap_or_else(|e| e.into_inner());
    set_application_device_pixel_ratio(1.25);
    for (family, file, px, ints, average, expected) in INSTALLED {
        if !is_the_measured_file(file) {
            continue; // not installed, or not the version measured
        }
        let font = Font::new(family, px);
        let m = FontMetrics::from_font(&font);
        assert_eq!(
            [
                m.ascent as i32,
                m.descent as i32,
                m.leading() as i32,
                m.height as i32,
                m.line_spacing() as i32,
                m.rounded_average_char_width() as i32,
            ],
            ints,
            "{family} {px}px QFontMetrics"
        );
        assert_eq!(
            m.average_char_width, average,
            "{family} {px}px averageCharWidth"
        );
        assert_eq!(hints(&font), expected, "{family} {px}px");
    }
    set_application_device_pixel_ratio(1.0);
}
