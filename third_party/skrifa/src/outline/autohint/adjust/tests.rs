// SPDX-License-Identifier: (MIT OR Apache-2.0) AND FTL
//
// Tests of the vertical separation adjustment. The expected values were worked out by hand
// from the C code of FreeType 2.14.2 (the file and line of each function are in `mod.rs`).

use super::super::{metrics::ScaledWidth, QuirksMode};
use super::*;
use raw::{model::glyph::outline::UnscaledOutlineSink, tables::glyf::PointFlags};

/// An outline whose points are in 26.6 units already (`x`, `y` equal the font units, so a
/// coordinate of 64 is one pixel).
fn outline_of(contours: &[&[(i32, i32, bool)]]) -> Outline {
    let mut outline = Outline::default();
    outline
        .fill_with(GlyphId::new(1), 1000, QuirksMode::Jit, |sink| {
            for contour in contours {
                sink.start_contour();
                for &(x, y, on_curve) in *contour {
                    let flags = if on_curve {
                        PointFlags::on_curve()
                    } else {
                        PointFlags::off_curve_quad()
                    };
                    sink.point(x, y, flags);
                }
                sink.end_contour();
            }
            Ok(0)
        })
        .unwrap();
    for point in &mut outline.points {
        point.x = point.fx;
        point.y = point.fy;
        point.ox = point.fx;
        point.oy = point.fy;
    }
    outline
}

const fn rect(x0: i32, y0: i32, x1: i32, y1: i32) -> [(i32, i32, bool); 4] {
    [
        (x0, y0, true),
        (x0, y1, true),
        (x1, y1, true),
        (x1, y0, true),
    ]
}

/// The y coordinates of a contour.
fn ys(outline: &Outline, contour: usize) -> Vec<i32> {
    outline.points[outline.contours[contour].range()]
        .iter()
        .map(|p| p.y)
        .collect()
}

/// A tilde: ends at y -20 and 60 and two round wiggles inside, one at 30 (a peak) and one at 10
/// (a valley), each an on-curve point between two off-curve points of the same y.
const TILDE: [(i32, i32, bool); 10] = [
    (0, -20, true),
    (20, 30, false),
    (40, 30, true),
    (60, 30, false),
    (80, 15, true),
    (100, 10, false),
    (120, 10, true),
    (140, 10, false),
    (160, 60, true),
    (80, 20, true),
];

#[test]
fn reverse_map_keeps_the_last_database_entry() {
    // U+00C7 comes before U+00C8 in the table: when both map to glyph 5, U+00C8 (UP) wins,
    // although U+00C7 (IGNORE_CAPITAL_BOTTOM) is also in the table.
    let map = |cp: u32| match cp {
        0xC7 | 0xC8 => Some(GlyphId::new(5)),
        0xE9 => Some(GlyphId::new(6)),
        // U+0021 and U+0051 map to glyph 7; U+0051 is later in the table, so its flags win,
        // and FreeType's binary search does not find U+0051 (the table is not sorted).
        0x21 | 0x51 => Some(GlyphId::new(7)),
        // .notdef is never put into the map.
        0x3F => Some(GlyphId::NOTDEF),
        _ => None,
    };
    let reverse = ReverseCharMap::from_cmap(map);
    assert_eq!(reverse.flags(GlyphId::new(5)), UP);
    assert_eq!(reverse.flags(GlyphId::new(6)), UP);
    assert_eq!(reverse.flags(GlyphId::new(7)), 0);
    assert_eq!(reverse.flags(GlyphId::NOTDEF), 0);
    assert_eq!(reverse.flags(GlyphId::new(8)), 0);
}

#[test]
fn reverse_map_of_a_font_without_database_characters_is_empty() {
    let reverse = ReverseCharMap::from_cmap(|_| None);
    assert_eq!(reverse.flags(GlyphId::new(1)), 0);
}

#[test]
fn contour_extrema_and_finders() {
    // base, accent, the counter of the accent, a two-point contour (dimensionless)
    let outline = outline_of(&[
        &rect(0, 0, 200, 400),
        &rect(50, 440, 150, 520),
        &rect(60, 450, 140, 510),
        &[(300, 900, true), (310, 900, true)],
    ]);
    let extrema = compute_extrema(&outline);
    let range = |e: &Extrema| (e.min, e.max);
    assert_eq!(range(&extrema[0]), (0, 400));
    assert_eq!(range(&extrema[1]), (440, 520));
    assert_eq!(range(&extrema[2]), (450, 510));
    assert_eq!(range(&extrema[3]), (i64::MAX, i64::MIN));
    assert_eq!(find_highest(&extrema), 1);
    assert_eq!(find_lowest(&extrema), 0);
    // The second highest contour starts below the highest one's minimum (440).
    assert_eq!(find_second_highest(&extrema), 0);
    // The second lowest contour ends above the lowest one's maximum (400).
    assert_eq!(find_second_lowest(&extrema), 1);
    // Fewer than three contours: FreeType answers 0.
    assert_eq!(find_second_highest(&extrema[..2]), 0);
    assert_eq!(find_second_lowest(&extrema[..2]), 0);
}

#[test]
fn highest_contour_ties_go_to_the_smaller_contour() {
    let outline = outline_of(&[&rect(0, 0, 100, 500), &rect(0, 300, 100, 500)]);
    let extrema = compute_extrema(&outline);
    assert_eq!(find_highest(&extrema), 1);
    let outline = outline_of(&[&rect(0, 0, 100, 500), &rect(0, 0, 100, 300)]);
    let extrema = compute_extrema(&outline);
    assert_eq!(find_lowest(&extrema), 1);
}

#[test]
fn accent_is_raised_to_one_pixel_above_its_base() {
    // The base ends at 400 and the accent starts at 440: 40 units apart, 24 short of a pixel.
    let mut outline = outline_of(&[&rect(0, 0, 200, 400), &rect(50, 440, 150, 520)]);
    apply_vertical_separation(&mut outline, &ScaledBlues::new(), UP, 1000, 0x10000);
    assert_eq!(ys(&outline, 0), [0, 400, 400, 0]);
    assert_eq!(ys(&outline, 1), [464, 544, 544, 464]);
}

#[test]
fn accent_that_is_a_pixel_away_stays() {
    for gap in [64, 100] {
        let mut outline =
            outline_of(&[&rect(0, 0, 200, 400), &rect(50, 400 + gap, 150, 480 + gap)]);
        apply_vertical_separation(&mut outline, &ScaledBlues::new(), UP, 1000, 0x10000);
        assert_eq!(ys(&outline, 1)[0], 400 + gap);
    }
    // Touching contours move by a whole pixel.
    let mut outline = outline_of(&[&rect(0, 0, 200, 400), &rect(50, 400, 150, 480)]);
    apply_vertical_separation(&mut outline, &ScaledBlues::new(), UP, 1000, 0x10000);
    assert_eq!(ys(&outline, 1)[0], 464);
}

#[test]
fn no_height_check_keeps_freetypes_zero_limit() {
    // aflatin.c:4855 starts `accent_height_limit` at 0 and sets it only when the height check
    // is on, so a flagged glyph without a height check (`!`, `?`) is never adjusted.
    let mut outline = outline_of(&[&rect(0, 0, 200, 400), &rect(50, 440, 150, 520)]);
    apply_vertical_separation(
        &mut outline,
        &ScaledBlues::new(),
        UP | NO_HEIGHT_CHECK,
        1000,
        0x10000,
    );
    assert_eq!(ys(&outline, 1), [440, 520, 520, 440]);
}

#[test]
fn accent_taller_than_the_limit_is_not_adjusted() {
    // Without blue zones the limit is 2/5 em: 400 here.
    let mut outline = outline_of(&[&rect(0, 0, 200, 400), &rect(50, 440, 150, 900)]);
    apply_vertical_separation(&mut outline, &ScaledBlues::new(), UP, 1000, 0x10000);
    assert_eq!(ys(&outline, 1)[0], 440);
}

#[test]
fn accent_beside_the_base_is_not_adjusted() {
    let mut outline = outline_of(&[&rect(0, 0, 200, 400), &rect(500, 440, 600, 520)]);
    apply_vertical_separation(&mut outline, &ScaledBlues::new(), UP, 1000, 0x10000);
    assert_eq!(ys(&outline, 1)[0], 440);
}

#[test]
fn cedilla_is_lowered_to_one_pixel_below_its_base() {
    let mut outline = outline_of(&[&rect(0, 0, 200, 400), &rect(60, -90, 140, -10)]);
    apply_vertical_separation(&mut outline, &ScaledBlues::new(), DOWN, 1000, 0x10000);
    // 10 units below the base: 54 short of a pixel.
    assert_eq!(ys(&outline, 1), [-144, -64, -64, -144]);
    assert_eq!(ys(&outline, 0), [0, 400, 400, 0]);
}

#[test]
fn a_skipped_top_adjustment_skips_the_bottom_one_too() {
    // FreeType returns from the whole function when the highest contour does not overlap
    // the rest (aflatin.c:3677), so the cedilla stays.
    let mut outline = outline_of(&[
        &rect(0, 0, 200, 400),
        &rect(500, 440, 600, 520),
        &rect(60, -90, 140, -10),
    ]);
    apply_vertical_separation(&mut outline, &ScaledBlues::new(), UP | DOWN, 1000, 0x10000);
    assert_eq!(ys(&outline, 1)[0], 440);
    assert_eq!(ys(&outline, 2)[0], -90);
}

#[test]
fn second_accent_follows_the_highest_one() {
    // Double acute: two accents of the same height; both are about as high as the highest one
    // and move together (`min_y_limit`, aflatin.c:3773).
    let mut outline = outline_of(&[
        &rect(0, 0, 400, 400),
        &rect(50, 440, 150, 520),
        &rect(250, 440, 350, 520),
    ]);
    apply_vertical_separation(&mut outline, &ScaledBlues::new(), UP, 1000, 0x10000);
    assert_eq!(ys(&outline, 1)[0], 464);
    assert_eq!(ys(&outline, 2)[0], 464);
}

#[test]
fn dimensionless_contours_do_not_overflow() {
    // The extrema of a contour of two points are `i64::MAX` and `i64::MIN`; the subtractions
    // of FreeType wrap (SUB_LONG), they must not panic.
    let mut outline = outline_of(&[
        &rect(0, 0, 200, 400),
        &rect(50, 440, 150, 520),
        &[(60, 450, true), (90, 460, true)],
    ]);
    apply_vertical_separation(&mut outline, &ScaledBlues::new(), UP | DOWN, 1000, 0x10000);
    let mut outline = outline_of(&[
        &[(0, 0, true), (9, 9, true)],
        &[(0, 90, true), (9, 99, true)],
    ]);
    apply_vertical_separation(&mut outline, &ScaledBlues::new(), UP | DOWN, 1000, 0x10000);
    apply_tilde_pre_pass(&mut outline, TILDE_TOP | TILDE_BOTTOM);
}

#[test]
fn tilde_measurement_finds_the_nearest_wiggle() {
    let outline = outline_of(&[&TILDE]);
    // The peak (y 30) is 30 below the top (60); the valley (y 10) is 30 above the bottom
    // (-20); the end points are not candidates.
    assert_eq!(tilde_min_measurement(&outline, 0, -20, 60), Some(30));
    // A wiggle nearer than an eighth of the height to an extremum is ignored: with the extrema
    // 9 and 31 the threshold is 2, and the peak is 1 below the top, the valley 1 above the bottom.
    assert_eq!(tilde_min_measurement(&outline, 0, 9, 31), None);
}

#[test]
fn tilde_is_stretched_to_the_distance_of_its_wiggles_plus_a_pixel() {
    let mut outline = outline_of(&[&TILDE]);
    let extrema = compute_extrema(&outline);
    // height 80, shortest wiggle distance 30: the target height is 94.
    let change = stretch_tilde(&mut outline, &extrema, 0, true);
    assert_eq!(change, 14);
    let tilde = &outline.points[outline.contours[0].range()];
    // (y + 20) * 94 / 80 - 20, C division truncating: (30 + 20) * 94 / 80 = 58.75 -> 58
    assert_eq!(tilde[0].y, -20);
    assert_eq!(tilde[2].y, 38);
    assert_eq!(tilde[8].y, 74);
    assert!(tilde
        .iter()
        .all(|p| p.flags.has_marker(PointMarker::IGNORE)));
    assert!(tilde
        .iter()
        .all(|p| p.flags.has_marker(PointMarker::TOUCHED_Y) == p.is_on_curve()));
}

#[test]
fn tilde_that_is_tall_enough_is_not_stretched_but_still_touched() {
    let mut outline = outline_of(&[&TILDE]);
    // The valley 12 above the bottom: the target height is 12 + 64 = 76, under the height 80.
    for ix in [5, 6, 7] {
        outline.points[ix].y = -8;
    }
    let extrema = compute_extrema(&outline);
    assert_eq!(stretch_tilde(&mut outline, &extrema, 0, true), 0);
    assert_eq!(outline.points[8].y, 60);
    // FreeType touches the contours before it looks at the height (aflatin.c:3300).
    assert!(outline.points[0].flags.has_marker(PointMarker::IGNORE));
}

#[test]
fn bottom_tilde_is_stretched_about_its_maximum() {
    let mut outline = outline_of(&[&TILDE]);
    let extrema = compute_extrema(&outline);
    assert_eq!(stretch_tilde(&mut outline, &extrema, 0, false), 14);
    let tilde = &outline.points[outline.contours[0].range()];
    // (y - 60) * 94 / 80 + 60: the bottom point goes to 60 - 80 * 94 / 80 = -34
    assert_eq!(tilde[0].y, -34);
    assert_eq!(tilde[8].y, 60);
    // (30 - 60) * 94 / 80 = -35.25 -> -35 (truncated toward zero)
    assert_eq!(tilde[2].y, 25);
}

#[test]
fn tilde_alignment_truncates_the_centering_like_c() {
    // bottom -20, top 75: 95 tall, under three pixels, so the centre is aligned.
    let mut outline = outline_of(&[&[
        (0, -20, true),
        (0, 75, true),
        (50, 75, true),
        (50, -20, true),
    ]]);
    // the bottom goes to 0 (+20), the height rounds up to 64 * 2 = 128... pix_round(95) is
    // 64, which is 31 short; half of it is -15 in C (-16 if rounded down)
    assert_eq!(align_tilde(&mut outline, 0, true), 5);
    assert_eq!(ys(&outline, 0), [-15, 80, 80, -15]);
    let mut outline = outline_of(&[&[
        (0, -20, true),
        (0, 75, true),
        (50, 75, true),
        (50, -20, true),
    ]]);
    // aligned about the top (75 -> 64: -11), centre correction +15
    assert_eq!(align_tilde(&mut outline, 0, false), -11 + 15);
}

#[test]
fn tilde_pre_pass_moves_only_the_tilde() {
    let mut outline = outline_of(&[&rect(0, -300, 200, -100), &TILDE]);
    apply_tilde_pre_pass(&mut outline, TILDE_TOP);
    // stretched to 94 tall, then moved up by 5 (see above)
    let tilde = ys(&outline, 1);
    assert_eq!((tilde[0], tilde[8]), (-15, 79));
    assert_eq!(ys(&outline, 0), [-300, -100, -100, -300]);
    assert!(!outline.points[0].flags.has_marker(PointMarker::IGNORE));
}

#[test]
fn no_tilde_flag_leaves_the_outline_alone() {
    let mut outline = outline_of(&[&rect(0, -300, 200, -100), &TILDE]);
    let before = outline.points.clone();
    apply_tilde_pre_pass(&mut outline, UP | DOWN | IGNORE_SMALL_TOP);
    assert_eq!(outline.points, before);
}

fn blue(zones: BlueZones, position: i32, overshoot: i32, is_active: bool) -> ScaledBlue {
    ScaledBlue {
        position: ScaledWidth {
            scaled: position,
            fitted: position,
        },
        overshoot: ScaledWidth {
            scaled: overshoot,
            fitted: overshoot,
        },
        zones,
        is_active,
    }
}

/// The zones of FreeType's Latin style at some size: capital top 400, capital bottom -6,
/// small top (x height) 300, small bottom -6, and an inactive top zone that comes first.
fn latin_blues() -> ScaledBlues {
    let mut blues = ScaledBlues::new();
    blues.push(blue(BlueZones::TOP, 1000, 1000, false));
    blues.push(blue(BlueZones::TOP, 390, 400, true));
    blues.push(blue(BlueZones::CAPITAL_BOTTOM, 0, -6, true));
    blues.push(blue(BlueZones::TOP | BlueZones::ADJUSTMENT, 290, 300, true));
    blues.push(blue(BlueZones::SMALL_BOTTOM, 0, -6, true));
    blues
}

#[test]
fn base_blues_are_the_first_active_zones() {
    let blues = latin_blues();
    let (top, bottom) = base_blues(&blues, true);
    assert_eq!(
        (overshoot(top.unwrap()), overshoot(bottom.unwrap())),
        (400, -6)
    );
    let (top, bottom) = base_blues(&blues, false);
    assert_eq!(
        (overshoot(top.unwrap()), overshoot(bottom.unwrap())),
        (300, -6)
    );
    // Without bottom properties there are no base blues (FreeType before 2.14).
    let mut blues = ScaledBlues::new();
    blues.push(blue(BlueZones::TOP, 390, 400, true));
    blues.push(blue(BlueZones::NONE, 0, -6, true));
    assert!(base_blues(&blues, true).1.is_none());
}

#[test]
fn accent_height_limit_uses_the_small_zones_first() {
    // 2 * (300 - -6) / 3
    assert_eq!(accent_height_limit(&latin_blues(), 1000, 0x10000), 204);
    // only capitals: (400 - -6) / 2
    let mut capitals = ScaledBlues::new();
    capitals.push(blue(BlueZones::TOP, 390, 400, true));
    capitals.push(blue(BlueZones::CAPITAL_BOTTOM, 0, -6, true));
    assert_eq!(accent_height_limit(&capitals, 1000, 0x10000), 203);
    // neither: 4/10 of the em, scaled; 0x8000 is a scale of one half
    assert_eq!(accent_height_limit(&ScaledBlues::new(), 1000, 0x8000), 200);
}

fn edge_at(pos: i32) -> Edge {
    Edge {
        pos,
        ..Default::default()
    }
}

#[test]
fn ignored_zones_are_the_ones_beyond_a_seventh_of_the_height() {
    let blues = latin_blues();
    let mut edges = [-100, -80, -79, -6, 0, 300, 474, 475, 480].map(edge_at);
    apply_blue_ignores(&mut edges, &blues, IGNORE_CAPITAL_BOTTOM);
    // limit = -6 - 406 / 7 - 16 = -80: edges below -80 lose their zone (aflatin.c:3139)
    let flagged = |edges: &[Edge]| {
        edges
            .iter()
            .map(|e| e.flags.contains(TopoFlags::NO_BLUE))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        flagged(&edges),
        [true, false, false, false, false, false, false, false, false]
    );
    let mut edges = [-100, 300, 474, 475, 480].map(edge_at);
    apply_blue_ignores(&mut edges, &blues, IGNORE_CAPITAL_TOP);
    // limit = 400 + 406 / 7 + 16 = 474: edges above it lose their zone (aflatin.c:3123)
    assert_eq!(flagged(&edges), [false, false, false, true, true]);
    let mut edges = [-100, -50, -40, 300, 320, 340].map(edge_at);
    apply_blue_ignores(&mut edges, &blues, IGNORE_SMALL_TOP | IGNORE_SMALL_BOTTOM);
    // small zones: height 306, limits -6 - 43 - 16 = -65 and 300 + 43 + 16 = 359
    assert_eq!(flagged(&edges), [true, false, false, false, false, false]);
    // Flags of other kinds ignore nothing.
    let mut edges = [-100, 480].map(edge_at);
    apply_blue_ignores(&mut edges, &blues, UP | DOWN);
    assert_eq!(flagged(&edges), [false, false]);
}
