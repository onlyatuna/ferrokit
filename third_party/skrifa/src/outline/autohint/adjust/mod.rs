// SPDX-License-Identifier: (MIT OR Apache-2.0) AND FTL
//
// Vertical separation adjustments for accents, ported from FreeType 2.14.2
// (`src/autofit/afadjust.c`, and the adjustment parts of `src/autofit/aflatin.c`).
//
// The reference is FreeType built without HarfBuzz: the reverse character map
// is derived from the cmap alone, as in Ubuntu's 2.14.2 package.

mod database;

use read_fonts::{tables::glyf::PointMarker, types::GlyphId};

use super::{
    metrics::{
        blues::{BlueZones, ScaledBlue, ScaledBlues},
        fixed_mul,
    },
    outline::{Outline, Point},
    topo::{Edge, TopoFlags},
};
use database::{
    ADJUSTMENT_DATABASE, DOWN, DOWN2, IGNORE_CAPITAL_BOTTOM, IGNORE_CAPITAL_TOP,
    IGNORE_SMALL_BOTTOM, IGNORE_SMALL_TOP, NO_HEIGHT_CHECK, TILDE_BOTTOM, TILDE_BOTTOM2,
    TILDE_TOP, TILDE_TOP2, UP, UP2,
};

/// Vertical extrema of one contour, in 26.6 units.
///
/// Dimensionless contours (fewer than three points) carry the sentinels
/// `min = i64::MAX` and `max = i64::MIN`, as in FreeType.
#[derive(Copy, Clone, Debug)]
struct Extrema {
    min: i64,
    max: i64,
}

const EMPTY_EXTREMA: Extrema = Extrema {
    min: i64::MAX,
    max: i64::MIN,
};

/// Iterates over the point indices of the contour that starts at `first`.
fn ring(outline: &Outline, first: usize) -> impl Iterator<Item = usize> + '_ {
    let mut next = Some(first);
    std::iter::from_fn(move || {
        let ix = next?;
        let following = outline.points[ix].next();
        next = (following != first).then_some(following);
        Some(ix)
    })
}

/// Applies `update` to each point of the contour that starts at `first`.
fn update_contour(outline: &mut Outline, first: usize, mut update: impl FnMut(&mut Point)) {
    let mut ix = first;
    loop {
        update(&mut outline.points[ix]);
        ix = outline.points[ix].next();
        if ix == first {
            break;
        }
    }
}

fn first_point(outline: &Outline, contour: usize) -> usize {
    outline.contours[contour].first()
}

fn is_dimensionless(outline: &Outline, first: usize) -> bool {
    let second = outline.points[first].next();
    outline.points[second].next() == first
}

fn overshoot(blue: &ScaledBlue) -> i64 {
    i64::from(blue.overshoot.scaled)
}

/// Rounds a 26.6 value to the nearest whole pixel, as `FT_PIX_ROUND`.
fn pix_round(value: i64) -> i64 {
    (value + 32) & !63
}

/// Computes the vertical extrema of every contour.
fn compute_extrema(outline: &Outline) -> Vec<Extrema> {
    (0..outline.contours.len())
        .map(|contour| {
            let first = first_point(outline, contour);
            if is_dimensionless(outline, first) {
                return EMPTY_EXTREMA;
            }
            ring(outline, first).fold(EMPTY_EXTREMA, |e, ix| {
                let y = i64::from(outline.points[ix].y);
                Extrema {
                    min: e.min.min(y),
                    max: e.max.max(y),
                }
            })
        })
        .collect()
}

/// Returns the contour with the highest maximum; ties go to the higher minimum.
fn find_highest(extrema: &[Extrema]) -> usize {
    let mut best = 0;
    let mut highest = EMPTY_EXTREMA;
    for (contour, e) in extrema.iter().enumerate() {
        if e.max > highest.max || (e.max == highest.max && e.min > highest.min) {
            highest = *e;
            best = contour;
        }
    }
    best
}

/// Returns the contour with the second highest maximum that lies entirely
/// below the highest contour's minimum, or 0 with fewer than three contours.
fn find_second_highest(extrema: &[Extrema]) -> usize {
    if extrema.len() < 3 {
        return 0;
    }
    let highest = find_highest(extrema);
    let highest_min = extrema[highest].min;
    let mut second = 0;
    let mut second_max = i64::MIN;
    for (contour, e) in extrema.iter().enumerate() {
        if contour == highest {
            continue;
        }
        if e.max > second_max && e.min < highest_min {
            second_max = e.max;
            second = contour;
        }
    }
    second
}

/// Returns the contour with the lowest minimum; ties go to the lower maximum.
fn find_lowest(extrema: &[Extrema]) -> usize {
    let mut best = 0;
    let mut lowest = EMPTY_EXTREMA;
    for (contour, e) in extrema.iter().enumerate() {
        if e.min < lowest.min || (e.min == lowest.min && e.max < lowest.max) {
            lowest = *e;
            best = contour;
        }
    }
    best
}

/// Returns the contour with the second lowest minimum that lies entirely
/// above the lowest contour's maximum, or 0 with fewer than three contours.
fn find_second_lowest(extrema: &[Extrema]) -> usize {
    if extrema.len() < 3 {
        return 0;
    }
    let lowest = find_lowest(extrema);
    let lowest_max = extrema[lowest].max;
    let mut second = 0;
    let mut second_min = i64::MAX;
    for (contour, e) in extrema.iter().enumerate() {
        if contour == lowest {
            continue;
        }
        if e.min < second_min && e.max > lowest_max {
            second_min = e.min;
            second = contour;
        }
    }
    second
}

/// Moves every contour whose minimum lies above `limit` up by `delta`.
fn move_contours_up(outline: &mut Outline, extrema: &[Extrema], limit: i64, delta: i64) {
    for (contour, e) in extrema.iter().enumerate() {
        if e.min < e.max && e.min > limit {
            move_contour(outline, first_point(outline, contour), delta);
        }
    }
}

/// Moves every contour whose maximum lies below `limit` down by `delta`.
fn move_contours_down(outline: &mut Outline, extrema: &[Extrema], limit: i64, delta: i64) {
    for (contour, e) in extrema.iter().enumerate() {
        if e.min < e.max && e.max < limit {
            move_contour(outline, first_point(outline, contour), -delta);
        }
    }
}

fn move_contour(outline: &mut Outline, first: usize, delta: i64) {
    update_contour(outline, first, |p| p.y += delta as i32);
}

/// Marks every point of a contour as ignored by segment detection, and
/// touched in y for on-curve points.
fn touch_contour(outline: &mut Outline, first: usize) {
    update_contour(outline, first, |p| {
        p.flags.set_marker(PointMarker::IGNORE);
        if p.is_on_curve() {
            p.flags.set_marker(PointMarker::TOUCHED_Y);
        }
    });
}

/// Touches the contours whose minimum is at or above the minimum of
/// `limit_contour`, provided they are not dimensionless.
fn touch_top_contours(outline: &mut Outline, extrema: &[Extrema], limit_contour: usize) {
    let limit = extrema[limit_contour].min;
    for (contour, e) in extrema.iter().enumerate() {
        if e.min < e.max && e.min >= limit {
            touch_contour(outline, first_point(outline, contour));
        }
    }
}

/// Touches the contours whose maximum is at or below the minimum of
/// `limit_contour`, provided they are not dimensionless.
fn touch_bottom_contours(outline: &mut Outline, extrema: &[Extrema], limit_contour: usize) {
    let limit = extrema[limit_contour].min;
    for (contour, e) in extrema.iter().enumerate() {
        if e.min < e.max && e.max <= limit {
            touch_contour(outline, first_point(outline, contour));
        }
    }
}

/// Finds the smallest distance from the top (or bottom) of a tilde to a
/// horizontal stroke within it. Returns `None` if there is no such stroke.
fn tilde_min_measurement(outline: &Outline, first: usize, min_y: i64, max_y: i64) -> Option<i64> {
    let threshold = (max_y - min_y) / 8;
    let mut min_measurement: Option<i64> = None;
    for ix in ring(outline, first) {
        let p = &outline.points[ix];
        let prev = &outline.points[p.prev()];
        let next = &outline.points[p.next()];
        if p.is_on_curve() || prev.is_on_curve() || next.is_on_curve() {
            continue;
        }
        let y = i64::from(p.y);
        if i64::from(prev.y) != y || i64::from(next.y) != y || y == min_y || y == max_y {
            continue;
        }

        let mut prev_on = p.prev();
        while !outline.points[prev_on].is_on_curve() {
            prev_on = outline.points[prev_on].prev();
        }
        let mut next_on = p.next();
        while !outline.points[next_on].is_on_curve() {
            next_on = outline.points[next_on].next();
        }
        let prev_y = i64::from(outline.points[prev_on].y);
        let next_y = i64::from(outline.points[next_on].y);

        let measurement = if next_y > y && prev_y > y {
            y - min_y
        } else if next_y < y && prev_y < y {
            max_y - y
        } else {
            continue;
        };
        if measurement < threshold {
            continue;
        }
        min_measurement = Some(min_measurement.map_or(measurement, |m| m.min(measurement)));
    }
    min_measurement
}

/// Stretches a tilde contour (top or bottom) so that its height is a whole
/// number of pixels plus the stroke clearance. Returns the height change.
fn stretch_tilde(outline: &mut Outline, extrema: &[Extrema], tilde: usize, top: bool) -> i64 {
    let first = first_point(outline, tilde);
    let Extrema {
        min: min_y,
        max: max_y,
    } = extrema[tilde];
    if min_y == max_y {
        return 0;
    }
    let height = max_y - min_y;
    let min_measurement = tilde_min_measurement(outline, first, min_y, max_y).unwrap_or(0);
    if top {
        touch_top_contours(outline, extrema, tilde);
    } else {
        touch_bottom_contours(outline, extrema, tilde);
    }

    let target = min_measurement + 64;
    if height >= target {
        return 0;
    }
    let base = if top { min_y } else { max_y };
    update_contour(outline, first, |p| {
        let scaled = (i64::from(p.y) - base) * target / height + base;
        p.y = scaled as i32;
    });
    target - height
}

/// Aligns a stretched tilde contour to the pixel grid. Returns the movement.
fn align_tilde(outline: &mut Outline, tilde: usize, top: bool) -> i64 {
    let first = first_point(outline, tilde);
    let (min_y, max_y) = ring(outline, first).fold((i64::MAX, i64::MIN), |(lo, hi), ix| {
        let y = i64::from(outline.points[ix].y);
        (lo.min(y), hi.max(y))
    });
    let height = max_y - min_y;
    let mut delta = if top {
        pix_round(min_y) - min_y
    } else {
        pix_round(max_y) - max_y
    };
    if height < 192 {
        let centering = (pix_round(height) - height) / 2;
        delta += if top { centering } else { -centering };
    }
    move_contour(outline, first, delta);
    delta
}

/// Returns true if `index` horizontally overlaps with the other contours.
fn check_horizontal_overlap(outline: &Outline, index: usize) -> bool {
    let (mut c_min, mut c_max) = (i64::MAX, i64::MIN);
    let (mut o_min, mut o_max) = (i64::MAX, i64::MIN);
    for contour in 0..outline.contours.len() {
        let first = first_point(outline, contour);
        if is_dimensionless(outline, first) {
            continue;
        }
        for ix in ring(outline, first) {
            let x = i64::from(outline.points[ix].x);
            if contour == index {
                c_min = c_min.min(x);
                c_max = c_max.max(x);
            } else {
                o_min = o_min.min(x);
                o_max = o_max.max(x);
            }
        }
    }
    (o_min <= c_max && c_max <= o_max)
        || (o_min <= c_min && c_min <= o_max)
        || (c_max >= o_max && c_min <= o_min)
}

/// Returns the first active blue zone for the top and bottom of capital
/// (`is_capital`) or small letters.
fn base_blues(blues: &ScaledBlues, is_capital: bool) -> (Option<&ScaledBlue>, Option<&ScaledBlue>) {
    let (top_zone, bottom_zone) = if is_capital {
        (BlueZones::TOP, BlueZones::CAPITAL_BOTTOM)
    } else {
        (BlueZones::ADJUSTMENT, BlueZones::SMALL_BOTTOM)
    };
    let top = blues
        .iter()
        .find(|b| b.is_active && b.zones.contains(top_zone));
    let bottom = blues
        .iter()
        .find(|b| b.is_active && b.zones.contains(bottom_zone));
    (top, bottom)
}

/// Returns the adjustment flags of the glyph, or 0 if it has none.
///
/// FreeType builds a reverse map from the database: for each entry in table
/// order, the cmap glyph is mapped to the codepoint with overwrite, so the
/// last matching entry wins. `map` is the cmap lookup.
pub(crate) fn glyph_adjust_flags(glyph: GlyphId, map: impl Fn(u32) -> Option<GlyphId>) -> u16 {
    if glyph == GlyphId::NOTDEF {
        return 0;
    }
    let mut codepoint = None;
    for &(cp, _) in ADJUSTMENT_DATABASE.iter() {
        if map(cp) == Some(glyph) {
            codepoint = Some(cp);
        }
    }
    codepoint.map_or(0, database::lookup)
}

/// Applies the tilde pre-pass, which runs before segment detection.
///
/// Stretches and aligns tilde accents, and shifts the contours above or below
/// them. All operations share one set of extrema, computed on entry, as in
/// FreeType.
pub(crate) fn apply_tilde_pre_pass(outline: &mut Outline, flags: u16) {
    let is_top = flags & TILDE_TOP != 0;
    let is_bottom = flags & TILDE_BOTTOM != 0;
    let is_below_top = flags & TILDE_TOP2 != 0;
    let is_above_bottom = flags & TILDE_BOTTOM2 != 0;
    if !(is_top || is_bottom || is_below_top || is_above_bottom) || outline.contours.is_empty() {
        return;
    }
    let extrema = compute_extrema(outline);

    if is_below_top {
        let contour = find_second_highest(&extrema);
        let mut offset = stretch_tilde(outline, &extrema, contour, true);
        offset += align_tilde(outline, contour, true);
        move_contours_up(outline, &extrema, extrema[contour].min, offset);
    }
    if is_above_bottom {
        let contour = find_second_lowest(&extrema);
        let mut offset = stretch_tilde(outline, &extrema, contour, false);
        offset -= align_tilde(outline, contour, false);
        move_contours_down(outline, &extrema, extrema[contour].max, offset);
    }
    if is_top {
        let contour = find_highest(&extrema);
        stretch_tilde(outline, &extrema, contour, true);
        align_tilde(outline, contour, true);
    }
    if is_bottom {
        let contour = find_lowest(&extrema);
        stretch_tilde(outline, &extrema, contour, false);
        align_tilde(outline, contour, false);
    }
}

/// Keeps the blue zone flags from ignoring the top/bottom zones of accents.
///
/// Edges beyond the ignore limit lose their blue zone (`NO_BLUE`).
pub(crate) fn apply_blue_ignores(edges: &mut [Edge], blues: &ScaledBlues, flags: u16) {
    let (capital_top, capital_bottom) = base_blues(blues, true);
    let (small_top, small_bottom) = base_blues(blues, false);
    if let (Some(top), Some(bottom)) = (capital_top, capital_bottom) {
        if flags & IGNORE_CAPITAL_TOP != 0 {
            ignore_top(edges, top, bottom);
        }
        if flags & IGNORE_CAPITAL_BOTTOM != 0 {
            ignore_bottom(edges, top, bottom);
        }
    }
    if let (Some(top), Some(bottom)) = (small_top, small_bottom) {
        if flags & IGNORE_SMALL_TOP != 0 {
            ignore_top(edges, top, bottom);
        }
        if flags & IGNORE_SMALL_BOTTOM != 0 {
            ignore_bottom(edges, top, bottom);
        }
    }
}

fn ignore_top(edges: &mut [Edge], top: &ScaledBlue, bottom: &ScaledBlue) {
    let top_shoot = overshoot(top);
    let base_height = top_shoot - overshoot(bottom);
    let limit = top_shoot + base_height / 7 + 16;
    for edge in edges.iter_mut() {
        if i64::from(edge.pos) > limit {
            edge.flags |= TopoFlags::NO_BLUE;
        }
    }
}

fn ignore_bottom(edges: &mut [Edge], top: &ScaledBlue, bottom: &ScaledBlue) {
    let bottom_shoot = overshoot(bottom);
    let base_height = overshoot(top) - bottom_shoot;
    let limit = bottom_shoot - base_height / 7 - 16;
    for edge in edges.iter_mut() {
        if i64::from(edge.pos) < limit {
            edge.flags |= TopoFlags::NO_BLUE;
        }
    }
}

/// Returns the accent height limit used by the separation adjustment.
fn accent_height_limit(blues: &ScaledBlues, units_per_em: i32, y_scale: i32) -> i64 {
    let (capital_top, capital_bottom) = base_blues(blues, true);
    let (small_top, small_bottom) = base_blues(blues, false);
    if let (Some(top), Some(bottom)) = (small_top, small_bottom) {
        2 * (overshoot(top) - overshoot(bottom)) / 3
    } else if let (Some(top), Some(bottom)) = (capital_top, capital_bottom) {
        (overshoot(top) - overshoot(bottom)) / 2
    } else {
        i64::from(fixed_mul(units_per_em * 4 / 10, y_scale))
    }
}

/// Pushes the top contour up when it nearly touches another contour.
///
/// Returns false if FreeType would skip the bottom adjustment as well.
fn separate_top(
    outline: &mut Outline,
    extrema: &[Extrema],
    flags: u16,
    accent_height_limit: i64,
) -> bool {
    let adjust_below_top = flags & UP2 != 0;
    let is_top_tilde = flags & TILDE_TOP != 0;
    let is_below_top_tilde = flags & TILDE_TOP2 != 0;

    let high = if adjust_below_top {
        find_second_highest(extrema)
    } else {
        find_highest(extrema)
    };
    if !check_horizontal_overlap(outline, high) {
        return false;
    }
    let high_min = extrema[high].min;
    let high_height = extrema[high].max - high_min;
    if high_height > accent_height_limit {
        return false;
    }

    let mut min_distance = 64;
    for (contour, e) in extrema.iter().enumerate() {
        if contour == high {
            continue;
        }
        let distance = high_min - e.max;
        if distance < 64 && distance < min_distance && e.min < high_min {
            min_distance = distance;
        }
    }
    let adjustment_amount = 64 - min_distance;

    let mut centering_adjustment = 0;
    if is_top_tilde || is_below_top_tilde {
        // The tilde contour is always the high contour on this path.
        let tilde_height = high_height;
        let mut pos = high_min + adjustment_amount;
        if adjust_below_top && is_top_tilde {
            pos += high_height;
        }
        if pos % 64 == 0 && tilde_height < 3 * 64 {
            centering_adjustment = (pix_round(tilde_height) - tilde_height) / 2;
        }
    }

    let calculated_amount = if (flags & UP != 0 && is_top_tilde)
        || (adjust_below_top && is_below_top_tilde)
    {
        adjustment_amount + centering_adjustment
    } else {
        adjustment_amount
    };

    if calculated_amount != 0
        && calculated_amount >= -2
        && (calculated_amount <= 66 || adjustment_amount <= 66)
    {
        let min_y_limit = high_min - high_height / 8;
        move_contours_up(outline, extrema, min_y_limit, adjustment_amount);
        if adjust_below_top && is_top_tilde {
            move_contours_up(
                outline,
                extrema,
                min_y_limit + high_height,
                centering_adjustment,
            );
        }
    }
    true
}

/// Pushes the bottom contour down when it nearly touches another contour.
fn separate_bottom(
    outline: &mut Outline,
    extrema: &[Extrema],
    flags: u16,
    accent_height_limit: i64,
) {
    let adjust_bottom = flags & DOWN != 0;
    let adjust_above_bottom = flags & DOWN2 != 0;
    let is_bottom_tilde = flags & TILDE_BOTTOM != 0;
    let is_above_bottom_tilde = flags & TILDE_BOTTOM2 != 0;

    let low = if adjust_above_bottom {
        find_second_lowest(extrema)
    } else {
        find_lowest(extrema)
    };
    if !check_horizontal_overlap(outline, low) {
        return;
    }
    let low_max = extrema[low].max;
    let low_height = low_max - extrema[low].min;
    if low_height > accent_height_limit {
        return;
    }

    let mut min_distance = 64;
    for (contour, e) in extrema.iter().enumerate() {
        if contour == low {
            continue;
        }
        let distance = e.min - low_max;
        if distance < 64 && distance < min_distance && e.max > low_max {
            min_distance = distance;
        }
    }
    let adjustment_amount = 64 - min_distance;

    let mut centering_adjustment = 0;
    if is_bottom_tilde || is_above_bottom_tilde {
        // The tilde contour is always the low contour on this path.
        let tilde_height = low_height;
        let mut pos = low_max - adjustment_amount;
        if adjust_above_bottom && is_bottom_tilde {
            pos -= low_height;
        }
        if pos % 64 == 0 && tilde_height < 3 * 64 {
            centering_adjustment = (pix_round(tilde_height) - tilde_height) / 2;
        }
    }

    let calculated_amount = if (adjust_bottom && is_bottom_tilde)
        || (adjust_above_bottom && is_above_bottom_tilde)
    {
        adjustment_amount + centering_adjustment
    } else {
        adjustment_amount
    };

    if calculated_amount != 0
        && calculated_amount >= -2
        && (calculated_amount <= 66 || adjustment_amount <= 66)
    {
        let max_y_limit = low_max + low_height / 8;
        move_contours_down(outline, extrema, max_y_limit, adjustment_amount);
        if adjust_above_bottom && is_bottom_tilde {
            move_contours_down(
                outline,
                extrema,
                max_y_limit - low_height,
                centering_adjustment,
            );
        }
    }
}

/// Applies the vertical separation adjustment for accents.
///
/// Runs after the vertical edges have been aligned. `flags` are the glyph's
/// adjustment flags, and `units_per_em` and `y_scale` define the accent height
/// fallback limit. A glyph with `NO_HEIGHT_CHECK` uses a limit of zero, which
/// mirrors FreeType's `accent_height_limit`, left at zero in that case.
pub(crate) fn apply_vertical_separation(
    outline: &mut Outline,
    blues: &ScaledBlues,
    flags: u16,
    units_per_em: i32,
    y_scale: i32,
) {
    let adjust_top = flags & UP != 0;
    let adjust_below_top = flags & UP2 != 0;
    let adjust_bottom = flags & DOWN != 0;
    let adjust_above_bottom = flags & DOWN2 != 0;
    if !(adjust_top || adjust_below_top || adjust_bottom || adjust_above_bottom) {
        return;
    }
    let limit = if flags & NO_HEIGHT_CHECK != 0 {
        0
    } else {
        accent_height_limit(blues, units_per_em, y_scale)
    };

    let extrema = compute_extrema(outline);
    let count = extrema.len();
    if (adjust_top && count >= 2) || (adjust_below_top && count >= 3) {
        if !separate_top(outline, &extrema, flags, limit) {
            return;
        }
    }
    if (adjust_bottom && count >= 2) || (adjust_above_bottom && count >= 3) {
        separate_bottom(outline, &extrema, flags, limit);
    }
}
