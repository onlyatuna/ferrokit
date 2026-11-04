// SPDX-License-Identifier: (MIT OR Apache-2.0) AND FTL
//
// Vertical separation adjustments for accents, ported from FreeType 2.14.2
// (`src/autofit/afadjust.c` and the adjustment parts of `src/autofit/aflatin.c`).
// Part of the skrifa crate of the fontations project (MIT OR Apache-2.0),
// changed by ferrokit; the FreeType-derived code is under the FreeType licence.
//
// The reference is FreeType built without HarfBuzz: the reverse character map
// is derived from the cmap alone, as in Ubuntu's 2.14.2 package, which Qt links.
//
// Hand-edited generated entries: `generated/generated_autohint_styles.rs` comes
// from fontations' `skrifa/scripts/gen_autohint_styles.py`, which is pinned to a
// FreeType snapshot that predates the bottom-zone properties of `afblue.dat`
// (`AF_BLUE_PROPERTY_LATIN_CAPITAL_BOTTOM`, `AF_BLUE_PROPERTY_LATIN_SMALL_BOTTOM`).
// That script is not vendored, so the 23 bottom-zone tuples below were changed
// by hand from `BlueZones::NONE` to `CAPITAL_BOTTOM` or `SMALL_BOTTOM`. Every
// blue string and property of the generated table was checked against the
// `AF_BLUE_STRINGSET_*` records of FreeType 2.14.2's `afblue.dat` (lines shown):
//   Adlam: 703 capital (𞤂 𞤖), 706 small (𞤤 𞤨 𞤩 𞤭 𞤴 𞤸 𞤺 𞥀)
//   Armenian: 717 capital (Ւ Ո Դ Ճ Շ Ս Տ Օ), 721 small (հ ո ճ ա ե ծ ս օ)
//   Canadian Syllabics: 763 small (ᕃ ᓂ ᓀ ᕂ ᓗ ᓚ ᕆ ᘣ)
//   Coptic: 785 capital (Ⳑ Ⳙ Ⳟ Ⲏ Ⲟ Ⲑ Ⳝ Ⲱ), 788 small (ⳑ ⳙ ⳟ ⲏ ⲟ ⲑ ⳝ Ⳓ)
//   Cyrillic: 800 capital (Б В Е Ш З О С Э)
//   Deseret: 819 capital (𐐀 𐐂 𐐄 𐐗 𐐛), 822 small (𐐨 𐐪 𐐬 𐐿 𐑃)
//   Glagolitic: 852 capital (Ⰵ Ⰴ Ⰲ Ⱚ Ⱎ Ⱑ Ⰺ Ⱄ), 855 small (ⰵ ⰴ ⰲ ⱚ ⱎ ⱑ ⰺ ⱄ)
//   Greek: 865 capital (Β Δ Ζ Ξ Θ Ο)
//   Latin: 939 capital (H E Z L O C U S), 943 small (n r x z o e s c)
//   Latin subscripts: 949 capital (₀ ₁ ₂ ₃ ₈)
//   Latin superscripts: 959 capital (⁰ ¹ ² ³ ᴱ ᴸ ᴼ ᵁ)
//   Medefaidrin: 979 capital (𖹀 𖹁 𖹂 𖹃 𖹏 𖹚 𖹒 𖹓), 983 small (𖹠 𖹡 𖹢 𖹳 𖹭 𖹽)
//   N'Ko: 1006 small (ߎ ߏ ߛ ߋ)
//   Osage: 1024 capital (𐒰 𐓍 𐓂 𐒿 𐓎 𐒹), 1028 small (𐓘 𐓚 𐓣 𐓵 𐓡 𐓧 𐓪 𐓶)
//   Shavian: 1055 small (𐑴 𐑻 𐑹)
// These bits drive `base_blues`, and so the ignore limits and the accent height
// limit; without them the bottom zone of these glyphs is not recognised and the
// result diverges from FreeType 2.14.2.

mod database;
mod gsub;
#[cfg(test)]
mod tests;

use alloc::{collections::BTreeMap, vec::Vec};

use read_fonts::{
    tables::glyf::PointMarker,
    types::{GlyphId, Tag},
};

use super::{
    context::AutohintContext,
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

// FreeType's `FT_Pos` is a `long`, and its `ADD_LONG`, `SUB_LONG` and `MUL_LONG`
// (ftcalc.h) wrap around instead of overflowing. The extrema of a dimensionless
// contour are the sentinels `FT_LONG_MAX` and `FT_LONG_MIN`, so these wrap for
// real on outlines with one- or two-point contours.
fn add(a: i64, b: i64) -> i64 {
    a.wrapping_add(b)
}

fn sub(a: i64, b: i64) -> i64 {
    a.wrapping_sub(b)
}

/// FreeType's reverse character map (afadjust.c:1277): the code point of the adjustment
/// database that each glyph gets.
///
/// FreeType builds the map once per Latin style. First, for each database entry in table order,
/// the cmap glyph of the entry's code point is mapped to that code point with
/// `ft_hash_num_insert`, which overwrites: when two database code points map to one glyph, the
/// one later in the table wins. Then (the reference FreeType has HarfBuzz) the glyphs that
/// single and alternate substitutions of the style's script make from those glyphs are added
/// without overwriting (see `gsub.rs`).
#[derive(Clone, Debug, Default)]
pub(crate) struct ReverseCharMap {
    /// Glyph index to code point.
    codepoints: BTreeMap<u32, u32>,
}

impl ReverseCharMap {
    /// Builds the map of a font for the style of `style_script` (the tag of its script class).
    pub(crate) fn new(context: &dyn AutohintContext, style_script: Tag) -> Self {
        let mut map = Self::from_cmap(|cp| context.map(cp));
        gsub::extend_reverse_map(&mut map, context, style_script);
        map
    }

    /// The part of the map that comes from the cmap lookup `map` (afadjust.c:1314-1354).
    pub(crate) fn from_cmap(map: impl Fn(u32) -> Option<GlyphId>) -> Self {
        let mut codepoints = BTreeMap::new();
        for &(codepoint, _) in ADJUSTMENT_DATABASE.iter() {
            let Some(glyph) = map(codepoint) else {
                continue;
            };
            // `cmap_glyph == 0` is skipped (afadjust.c:1348).
            if glyph != GlyphId::NOTDEF {
                codepoints.insert(glyph.to_u32(), codepoint);
            }
        }
        Self { codepoints }
    }

    /// `ft_hash_num_insert_no_overwrite`: true if the glyph was not in the map.
    fn insert_no_overwrite(&mut self, glyph: u32, codepoint: u32) -> bool {
        match self.codepoints.entry(glyph) {
            alloc::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(codepoint);
                true
            }
            alloc::collections::btree_map::Entry::Occupied(_) => false,
        }
    }

    fn codepoint(&self, glyph: u32) -> Option<u32> {
        self.codepoints.get(&glyph).copied()
    }

    /// Returns the adjustment flags of `glyph`, or 0 if it has none: FreeType looks the code
    /// point up in the database when it hints the glyph (aflatin.c:4910-4936 and
    /// aflatin.c:3606-3622); the binary search over the unsorted table can miss (U+0051).
    pub(crate) fn flags(&self, glyph: GlyphId) -> u16 {
        self.codepoint(glyph.to_u32()).map_or(0, database::lookup)
    }
}

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
    core::iter::from_fn(move || {
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

/// A contour of one or two points, which FreeType's loops skip
/// (`first_point->next->next == first_point`).
fn is_dimensionless(outline: &Outline, first: usize) -> bool {
    let second = outline.points[first].next();
    outline.points[second].next() == first
}

/// The scaled overshoot of a blue zone (`blue->shoot.cur`).
fn overshoot(blue: &ScaledBlue) -> i64 {
    i64::from(blue.overshoot.scaled)
}

/// Rounds a 26.6 value to the nearest whole pixel (`FT_PIX_ROUND`).
fn pix_round(value: i64) -> i64 {
    add(value, 32) & !63
}

// aflatin.c:2842 af_compute_vertical_extrema
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

// aflatin.c:2878 af_find_highest_contour
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

// aflatin.c:2916 af_find_second_highest_contour
/// Returns the contour with the second highest maximum that starts below the
/// highest contour's minimum, or 0 with fewer than three contours.
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

// aflatin.c:2961 af_find_lowest_contour
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

// aflatin.c:2991 af_find_second_lowest_contour
/// Returns the contour with the second lowest minimum that ends above the
/// lowest contour's maximum, or 0 with fewer than three contours.
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

// aflatin.c:2796 af_move_contours_up
/// Moves every contour whose minimum lies above `limit` up by `delta`.
fn move_contours_up(outline: &mut Outline, extrema: &[Extrema], limit: i64, delta: i64) {
    for (contour, e) in extrema.iter().enumerate() {
        if e.min < e.max && e.min > limit {
            move_contour(outline, first_point(outline, contour), delta);
        }
    }
}

// aflatin.c:2818 af_move_contours_down
/// Moves every contour whose maximum lies below `limit` down by `delta`.
fn move_contours_down(outline: &mut Outline, extrema: &[Extrema], limit: i64, delta: i64) {
    for (contour, e) in extrema.iter().enumerate() {
        if e.min < e.max && e.max < limit {
            move_contour(outline, first_point(outline, contour), delta.wrapping_neg());
        }
    }
}

// aflatin.c:2775 af_move_contour_vertically
/// Moves a contour vertically. `FT_Int movement` truncates `delta` to 32 bits.
fn move_contour(outline: &mut Outline, first: usize, delta: i64) {
    let movement = delta as i32;
    update_contour(outline, first, |p| p.y = p.y.wrapping_add(movement));
}

// aflatin.c:3146 af_touch_contour (the IGNORE flag is set at aflatin.c:3157)
/// Marks every point of a contour as ignored by segment detection, and
/// on-curve points as touched in y.
fn touch_contour(outline: &mut Outline, first: usize) {
    update_contour(outline, first, |p| {
        p.flags.set_marker(PointMarker::IGNORE);
        if p.is_on_curve() {
            p.flags.set_marker(PointMarker::TOUCHED_Y);
        }
    });
}

// aflatin.c:3166 af_touch_top_contours
/// Touches the contours that are not dimensionless and whose minimum is at or
/// above the minimum of `limit_contour`.
fn touch_top_contours(outline: &mut Outline, extrema: &[Extrema], limit_contour: usize) {
    let limit = extrema[limit_contour].min;
    for (contour, e) in extrema.iter().enumerate() {
        if e.min < e.max && e.min >= limit {
            touch_contour(outline, first_point(outline, contour));
        }
    }
}

// aflatin.c:3188 af_touch_bottom_contours (it compares with the minimum of
// `limit_contour`, as `af_touch_top_contours` does)
/// Touches the contours that are not dimensionless and whose maximum is at or
/// below the minimum of `limit_contour`.
fn touch_bottom_contours(outline: &mut Outline, extrema: &[Extrema], limit_contour: usize) {
    let limit = extrema[limit_contour].min;
    for (contour, e) in extrema.iter().enumerate() {
        if e.min < e.max && e.max <= limit {
            touch_contour(outline, first_point(outline, contour));
        }
    }
}

// aflatin.c:3247-3292 (af_latin_stretch_top_tilde) and aflatin.c:3354-3395
// (af_latin_stretch_bottom_tilde), which have the same measurement loop
/// Finds the smallest distance from the top or bottom of a tilde to one of its
/// other round extrema. Returns `None` if there is no such extremum.
fn tilde_min_measurement(outline: &Outline, first: usize, min_y: i64, max_y: i64) -> Option<i64> {
    let threshold = sub(max_y, min_y) / 8;
    let mut min_measurement: Option<i64> = None;
    for ix in ring(outline, first) {
        let p = &outline.points[ix];
        let prev = &outline.points[p.prev()];
        let next = &outline.points[p.next()];
        // A candidate is an on-curve point between two control points that
        // all have the same y.
        if !p.is_on_curve() || prev.is_on_curve() || next.is_on_curve() {
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
            sub(y, min_y)
        } else if next_y < y && prev_y < y {
            sub(max_y, y)
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

// aflatin.c:3212 af_latin_stretch_top_tilde; aflatin.c:3328 af_latin_stretch_bottom_tilde
/// Stretches a tilde contour so that it is one pixel taller than the distance
/// of its other wiggle, and touches the contours above (or below) it. Returns
/// the height change.
fn stretch_tilde(outline: &mut Outline, extrema: &[Extrema], tilde: usize, top: bool) -> i64 {
    let first = first_point(outline, tilde);
    let Extrema {
        min: min_y,
        max: max_y,
    } = extrema[tilde];
    if min_y == max_y {
        return 0;
    }
    let height = sub(max_y, min_y);
    let min_measurement = tilde_min_measurement(outline, first, min_y, max_y).unwrap_or(0);
    // aflatin.c:3300 and aflatin.c:3400: this happens before the height check.
    if top {
        touch_top_contours(outline, extrema, tilde);
    } else {
        touch_bottom_contours(outline, extrema, tilde);
    }

    let target = add(min_measurement, 64);
    if height >= target {
        return 0;
    }
    // aflatin.c:3316-3319 and aflatin.c:3410-3413: C division truncates.
    let base = if top { min_y } else { max_y };
    update_contour(outline, first, |p| {
        let scaled = sub(i64::from(p.y), base).wrapping_mul(target) / height;
        p.y = add(scaled, base) as i32;
    });
    sub(target, height)
}

// aflatin.c:3445 af_latin_align_top_tilde; aflatin.c:3487 af_latin_align_bottom_tilde
/// Aligns a stretched tilde contour to the pixel grid: its bottom (top) edge,
/// or its centre if it is less than three pixels tall. Returns the movement.
fn align_tilde(outline: &mut Outline, tilde: usize, top: bool) -> i64 {
    let first = first_point(outline, tilde);
    let (min_y, max_y) = ring(outline, first).fold((i64::MAX, i64::MIN), |(lo, hi), ix| {
        let y = i64::from(outline.points[ix].y);
        (lo.min(y), hi.max(y))
    });
    let height = sub(max_y, min_y);
    let mut delta = if top {
        sub(pix_round(min_y), min_y)
    } else {
        sub(pix_round(max_y), max_y)
    };
    if height < 64 * 3 {
        // C division truncates toward zero, which matters for a negative difference.
        let centering = sub(pix_round(height), height) / 2;
        delta = if top {
            add(delta, centering)
        } else {
            sub(delta, centering)
        };
    }
    move_contour(outline, first, delta);
    delta
}

// aflatin.c:3528 af_check_contour_horizontal_overlap
/// Returns true if `index` horizontally overlaps with the bounding box of all
/// other contours.
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

// aflatin.c:3069 af_latin_get_base_glyph_blues
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

// aflatin.c:4938-4981 (in af_latin_hints_apply, before the vertical features are detected)
/// Applies the tilde pre-pass: stretches and aligns tilde accents, and shifts
/// the contours above or below them.
///
/// It runs before the segments of the vertical dimension are built, because it
/// marks the points of the accent as ignored. All steps share one set of
/// extrema, computed on entry (aflatin.c:4938-4940), as in FreeType; they go
/// stale as contours move, and FreeType leaves them so.
pub(crate) fn apply_tilde_pre_pass(outline: &mut Outline, flags: u16) {
    let is_top = flags & TILDE_TOP != 0;
    let is_bottom = flags & TILDE_BOTTOM != 0;
    let is_below_top = flags & TILDE_TOP2 != 0;
    let is_above_bottom = flags & TILDE_BOTTOM2 != 0;
    if !(is_top || is_bottom || is_below_top || is_above_bottom) || outline.contours.is_empty() {
        return;
    }
    let extrema = compute_extrema(outline);

    // Inner tilde glyphs first (aflatin.c:4942-4966).
    if is_below_top {
        let contour = find_second_highest(&extrema);
        let mut offset = stretch_tilde(outline, &extrema, contour, true);
        offset = add(offset, align_tilde(outline, contour, true));
        move_contours_up(outline, &extrema, extrema[contour].min, offset);
    }
    if is_above_bottom {
        let contour = find_second_lowest(&extrema);
        let mut offset = stretch_tilde(outline, &extrema, contour, false);
        offset = sub(offset, align_tilde(outline, contour, false));
        move_contours_down(outline, &extrema, extrema[contour].max, offset);
    }

    // aflatin.c:4968-4981
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

// aflatin.c:5023-5040 (in af_latin_hints_apply, after the vertical features are detected)
/// Makes the blue zone alignment ignore the edges that the glyph's flags
/// mark as belonging to an accent (`NO_BLUE`), which `compute_blue_edges` skips.
pub(crate) fn apply_blue_ignores(edges: &mut [Edge], blues: &ScaledBlues, flags: u16) {
    const IGNORE: u16 =
        IGNORE_CAPITAL_TOP | IGNORE_CAPITAL_BOTTOM | IGNORE_SMALL_TOP | IGNORE_SMALL_BOTTOM;
    if flags & IGNORE == 0 {
        return;
    }
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

// aflatin.c:3110 af_latin_ignore_top, with af_prevent_top_blue_alignment (aflatin.c:3035)
fn ignore_top(edges: &mut [Edge], top: &ScaledBlue, bottom: &ScaledBlue) {
    let top_shoot = overshoot(top);
    let base_height = top_shoot - overshoot(bottom);
    // aflatin.c:3123: one seventh of the height and a quarter of a pixel.
    let limit = top_shoot + base_height / 7 + 16;
    for edge in edges.iter_mut() {
        if i64::from(edge.pos) > limit {
            edge.flags |= TopoFlags::NO_BLUE;
        }
    }
}

// aflatin.c:3130 af_latin_ignore_bottom, with af_prevent_bottom_blue_alignment (aflatin.c:3052)
fn ignore_bottom(edges: &mut [Edge], top: &ScaledBlue, bottom: &ScaledBlue) {
    let bottom_shoot = overshoot(bottom);
    let base_height = overshoot(top) - bottom_shoot;
    // aflatin.c:3139
    let limit = bottom_shoot - base_height / 7 - 16;
    for edge in edges.iter_mut() {
        if i64::from(edge.pos) < limit {
            edge.flags |= TopoFlags::NO_BLUE;
        }
    }
}

// aflatin.c:4999-5020
/// Returns the accent height limit that `apply_vertical_separation` uses.
fn accent_height_limit(blues: &ScaledBlues, units_per_em: i32, y_scale: i32) -> i64 {
    let (capital_top, capital_bottom) = base_blues(blues, true);
    let (small_top, small_bottom) = base_blues(blues, false);
    if let (Some(top), Some(bottom)) = (small_top, small_bottom) {
        // 2/3 of the lowercase blue zone height
        2 * (overshoot(top) - overshoot(bottom)) / 3
    } else if let (Some(top), Some(bottom)) = (capital_top, capital_bottom) {
        // 1/2 of the uppercase blue zone height
        (overshoot(top) - overshoot(bottom)) / 2
    } else {
        // 2/5 of the em, scaled
        i64::from(fixed_mul(units_per_em * 4 / 10, y_scale))
    }
}

/// The result of the top (or bottom) block of the separation adjustment.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Separation {
    /// The block ran or had nothing to do; the other block may run.
    Done,
    /// FreeType returned from the whole function (aflatin.c:3677, 3691, 3842, 3856).
    Return,
}

// aflatin.c:3634-3797 (top block of af_glyph_hints_apply_vertical_separation_adjustments)
/// Pushes the highest (or second highest) contour up when it nearly touches
/// another contour.
fn separate_top(
    outline: &mut Outline,
    extrema: &[Extrema],
    flags: u16,
    accent_height_limit: i64,
) -> Separation {
    let adjust_top = flags & UP != 0;
    let adjust_below_top = flags & UP2 != 0;
    let is_top_tilde = flags & TILDE_TOP != 0;
    let is_below_top_tilde = flags & TILDE_TOP2 != 0;

    // aflatin.c:3664
    let high = if adjust_below_top {
        find_second_highest(extrema)
    } else {
        find_highest(extrema)
    };
    // aflatin.c:3670-3678
    if !check_horizontal_overlap(outline, high) {
        return Separation::Return;
    }
    let high_min = extrema[high].min;
    let high_height = sub(extrema[high].max, high_min);
    // aflatin.c:3684-3692
    if high_height > accent_height_limit {
        return Separation::Return;
    }

    // aflatin.c:3698-3719
    let mut min_distance = 64;
    for (contour, e) in extrema.iter().enumerate() {
        if contour == high {
            continue;
        }
        let distance = sub(high_min, e.max);
        if distance < 64 && distance < min_distance && e.min < high_min {
            min_distance = distance;
        }
    }
    let adjustment_amount = 64 - min_distance;

    // aflatin.c:3723-3752
    let mut centering_adjustment = 0;
    if is_top_tilde || is_below_top_tilde {
        let tilde = if adjust_top || is_below_top_tilde {
            high
        } else {
            find_highest(extrema)
        };
        let tilde_height = sub(extrema[tilde].max, extrema[tilde].min);
        // The separation can undo the grid alignment of a tilde; if it would
        // align a tilde under three pixels tall, centre it again.
        let mut pos = add(high_min, adjustment_amount);
        if adjust_below_top && is_top_tilde {
            pos = add(pos, high_height);
        }
        if pos % 64 == 0 && tilde_height < 3 * 64 {
            centering_adjustment = sub(pix_round(tilde_height), tilde_height) / 2;
        }
    }

    // aflatin.c:3754-3758
    let calculated_amount = if (adjust_top && is_top_tilde) || (adjust_below_top && is_below_top_tilde)
    {
        add(adjustment_amount, centering_adjustment)
    } else {
        adjustment_amount
    };

    // aflatin.c:3767-3796: a delta of 2/64 px is tolerated for rounding.
    if calculated_amount != 0
        && calculated_amount >= -2
        && (calculated_amount <= 66 || adjustment_amount <= 66)
    {
        let min_y_limit = sub(high_min, high_height / 8);
        // The amount is applied to all contours that are about as high or higher
        // (the ring of a Czech accent, the second acute of a double acute).
        move_contours_up(outline, extrema, min_y_limit, adjustment_amount);
        if adjust_below_top && is_top_tilde {
            move_contours_up(
                outline,
                extrema,
                add(min_y_limit, high_height),
                centering_adjustment,
            );
        }
    }
    Separation::Done
}

// aflatin.c:3799-3942 (bottom block of af_glyph_hints_apply_vertical_separation_adjustments)
/// Pushes the lowest (or second lowest) contour down when it nearly touches
/// another contour.
fn separate_bottom(
    outline: &mut Outline,
    extrema: &[Extrema],
    flags: u16,
    accent_height_limit: i64,
) -> Separation {
    let adjust_bottom = flags & DOWN != 0;
    let adjust_above_bottom = flags & DOWN2 != 0;
    let is_bottom_tilde = flags & TILDE_BOTTOM != 0;
    let is_above_bottom_tilde = flags & TILDE_BOTTOM2 != 0;

    // aflatin.c:3831
    let low = if adjust_above_bottom {
        find_second_lowest(extrema)
    } else {
        find_lowest(extrema)
    };
    // aflatin.c:3835-3843
    if !check_horizontal_overlap(outline, low) {
        return Separation::Return;
    }
    let low_max = extrema[low].max;
    let low_height = sub(low_max, extrema[low].min);
    // aflatin.c:3849-3857
    if low_height > accent_height_limit {
        return Separation::Return;
    }

    // aflatin.c:3859-3877
    let mut min_distance = 64;
    for (contour, e) in extrema.iter().enumerate() {
        if contour == low {
            continue;
        }
        let distance = sub(e.min, low_max);
        if distance < 64 && distance < min_distance && e.max > low_max {
            min_distance = distance;
        }
    }
    let adjustment_amount = 64 - min_distance;

    // aflatin.c:3881-3905
    let mut centering_adjustment = 0;
    if is_bottom_tilde || is_above_bottom_tilde {
        let tilde = if adjust_bottom || is_above_bottom_tilde {
            low
        } else {
            find_lowest(extrema)
        };
        let tilde_height = sub(extrema[tilde].max, extrema[tilde].min);
        let mut pos = sub(low_max, adjustment_amount);
        if adjust_above_bottom && is_bottom_tilde {
            pos = sub(pos, low_height);
        }
        if pos % 64 == 0 && tilde_height < 3 * 64 {
            centering_adjustment = sub(pix_round(tilde_height), tilde_height) / 2;
        }
    }

    // aflatin.c:3907-3911
    let calculated_amount = if (adjust_bottom && is_bottom_tilde)
        || (adjust_above_bottom && is_above_bottom_tilde)
    {
        add(adjustment_amount, centering_adjustment)
    } else {
        adjustment_amount
    };

    // aflatin.c:3919-3941
    if calculated_amount != 0
        && calculated_amount >= -2
        && (calculated_amount <= 66 || adjustment_amount <= 66)
    {
        let max_y_limit = add(low_max, low_height / 8);
        move_contours_down(outline, extrema, max_y_limit, adjustment_amount);
        if adjust_above_bottom && is_bottom_tilde {
            move_contours_down(
                outline,
                extrema,
                sub(max_y_limit, low_height),
                centering_adjustment,
            );
        }
    }
    Separation::Done
}

// aflatin.c:3583 af_glyph_hints_apply_vertical_separation_adjustments
// (body 3590-3955); called from aflatin.c:5060 after the vertical edges have
// been fitted
/// Applies the vertical separation adjustment: it keeps an accent at least one
/// pixel away from its base glyph.
///
/// `flags` are the glyph's adjustment flags, `units_per_em` and `y_scale` give the
/// fallback accent height limit. A glyph with `NO_HEIGHT_CHECK` keeps FreeType's
/// limit of zero (aflatin.c:4855, 4904), and so is only adjusted if the accent
/// has no height at all.
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

    // aflatin.c:3624-3632: the extrema of the already hinted outline.
    let extrema = compute_extrema(outline);
    let count = extrema.len();
    if ((adjust_top && count >= 2) || (adjust_below_top && count >= 3))
        && separate_top(outline, &extrema, flags, limit) == Separation::Return
    {
        return;
    }
    if (adjust_bottom && count >= 2) || (adjust_above_bottom && count >= 3) {
        separate_bottom(outline, &extrema, flags, limit);
    }
}
