//! CFF stem darkening.
//!
//! A port of the stem darkening of FreeType 2.14.2's Adobe CFF engine, which
//! FreeType applies when the `no-stem-darkening` property of the `cff`
//! driver is false (Qt sets it so, `qfontengine_ft.cpp`):
//!
//! * [`Darkening::new`] is `cf2_computeDarkening` and the part of
//!   `cf2_font_setup` that derives `darkenX` and `darkenY`
//!   (`src/psaux/psfont.c`),
//! * [`GlyphPath`] is `CF2_GlyphPath` (`cf2_glyphpath_*` of
//!   `src/psaux/pshints.c`): it moves every outline edge outward by the
//!   darkening amounts and joins the moved edges again,
//! * the winding order measurement that decides the direction of the move
//!   is `windingMomentum` here, taken by [`GlyphPath::compute_offset`], and
//!   `cf2_getGlyphOutline` (`psfont.c`) runs the charstring a second time
//!   with a reversed offset when it is negative (see `Outlines::draw`).
//!
//! All arithmetic is FreeType's integer 16.16 arithmetic, with the same
//! rounding.

use super::{HintMap, HintState};
use read_fonts::{ps::charstring::CommandSink, types::Fixed};

/// FreeType's default `darkening-parameters`, `x1, y1, x2, y2, x3, y3, x4, y4`
/// (`CFF_CONFIG_OPTION_DARKENING_PARAMETER_*`, `ftoption.h`).
const DARKENING_PARAMETERS: [i32; 8] = [500, 400, 1000, 275, 1667, 275, 2333, 0];

/// `cf2_doubleToFixed( 0.1 )`
const SNAP_THRESHOLD: i32 = 6554;
/// `cf2_doubleToFixed( 0.7 )`
const SEVEN_TENTHS: i32 = 45875;
/// `cf2_doubleToFixed( -0.7 )`
const NEG_SEVEN_TENTHS: i32 = -45874;
/// `cf2_doubleToFixed( 1.0 - 0.7 )`
const THREE_TENTHS: i32 = 19661;
/// `cf2_doubleToFixed( 1.0 + 0.7 )`
const SEVENTEEN_TENTHS: i32 = 111411;

fn int_to_fixed(i: i32) -> i32 {
    i.wrapping_shl(16)
}

/// `FT_MulFix`
fn mul_fix(a: i32, b: i32) -> i32 {
    (Fixed::from_bits(a) * Fixed::from_bits(b)).to_bits()
}

/// `FT_DivFix`
fn div_fix(a: i32, b: i32) -> i32 {
    (Fixed::from_bits(a) / Fixed::from_bits(b)).to_bits()
}

/// `FT_MulDiv`
fn mul_div(a: i32, b: i32, c: i32) -> i32 {
    Fixed::from_bits(a)
        .mul_div(Fixed::from_bits(b), Fixed::from_bits(c))
        .to_bits()
}

/// `FT_MSB`
fn msb(z: u32) -> i32 {
    if z == 0 {
        0
    } else {
        31 - z.leading_zeros() as i32
    }
}

/// Amounts by which `cf2_font_setup` moves the edges of an outline outward
/// (`darkenX` and `darkenY`), in character space.
#[derive(Copy, Clone, PartialEq, Eq, Default, Debug)]
pub(crate) struct Darkening {
    pub x: Fixed,
    pub y: Fixed,
}

impl Darkening {
    /// `cf2_font_setup`: the amounts for a font of `units_per_em` at the
    /// whole pixel size `ppem` (`FT_Size_Metrics::y_ppem`) whose Private
    /// DICT has the integers `std_vw` (`StdVW`) and `std_hw` (`StdHW`).
    pub fn new(units_per_em: i32, ppem: i32, std_vw: i32, std_hw: i32) -> Self {
        let units_per_em = if units_per_em == 0 { 1000 } else { units_per_em };
        // use minimum ppem of 4
        let ppem = int_to_fixed(ppem).max(int_to_fixed(4));
        let em_ratio = int_to_fixed(1000) / units_per_em;
        let mut std_vw = int_to_fixed(std_vw);
        if std_vw <= 0 {
            std_vw = div_fix(int_to_fixed(75), em_ratio);
        }
        let x = compute_darkening(em_ratio, ppem, std_vw);
        let std_hw_in = int_to_fixed(std_hw);
        let std_hw = if std_hw_in > 0 && std_vw > std_hw_in.wrapping_mul(2) {
            div_fix(int_to_fixed(75), em_ratio)
        } else {
            // low contrast font gets less hstem darkening
            div_fix(int_to_fixed(110), em_ratio)
        };
        let y = compute_darkening(em_ratio, ppem, std_hw);
        Self {
            x: Fixed::from_bits(x),
            y: Fixed::from_bits(y),
        }
    }
}

/// `cf2_computeDarkening` with stem darkening on, the default parameters and
/// no synthetic emboldening, which FreeType's CFF driver never sets.
fn compute_darkening(em_ratio: i32, ppem: i32, stem_width: i32) -> i32 {
    // protect against range problems and divide by zero
    if em_ratio < 655 {
        return 0;
    }
    let [x1, y1, x2, y2, x3, y3, x4, y4] = DARKENING_PARAMETERS;
    let stem_width_per_1000 = mul_fix(stem_width, em_ratio);
    // `scaled_stem` can easily overflow, so we must clamp its maximum value
    let log_base2 = msb(stem_width_per_1000 as u32) + msb(ppem as u32);
    let scaled_stem = if log_base2 >= 46 {
        // possible overflow
        int_to_fixed(x4)
    } else {
        mul_fix(stem_width_per_1000, ppem)
    };
    let mut darken_amount = if scaled_stem < int_to_fixed(x1) {
        div_fix(int_to_fixed(y1), ppem)
    } else if scaled_stem < int_to_fixed(x2) {
        let x = stem_width_per_1000.wrapping_sub(div_fix(int_to_fixed(x1), ppem));
        mul_div(x, y2 - y1, x2 - x1).wrapping_add(div_fix(int_to_fixed(y1), ppem))
    } else if scaled_stem < int_to_fixed(x3) {
        let x = stem_width_per_1000.wrapping_sub(div_fix(int_to_fixed(x2), ppem));
        mul_div(x, y3 - y2, x3 - x2).wrapping_add(div_fix(int_to_fixed(y2), ppem))
    } else if scaled_stem < int_to_fixed(x4) {
        let x = stem_width_per_1000.wrapping_sub(div_fix(int_to_fixed(x3), ppem));
        mul_div(x, y4 - y3, x4 - x3).wrapping_add(div_fix(int_to_fixed(y3), ppem))
    } else {
        div_fix(int_to_fixed(y4), ppem)
    };
    // use half the amount on each side and convert back to true character
    // space
    darken_amount = div_fix(darken_amount, em_ratio.wrapping_mul(2));
    darken_amount
}

/// `cf2_getWindingMomentum`
fn winding_momentum(x1: i32, y1: i32, x2: i32, y2: i32) -> i32 {
    (x1 >> 16)
        .wrapping_mul(y2.wrapping_sub(y1) >> 16)
        .wrapping_sub((y1 >> 16).wrapping_mul(x2.wrapping_sub(x1) >> 16))
}

/// FreeType converts from 16.16 to 26.6 by truncation. The values stay
/// 16.16 here with the low 10 bits cleared.
fn to_output(value: i32) -> Fixed {
    Fixed::from_bits(value & !0x3FF)
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum PrevOp {
    Line,
    Curve,
}

/// `CF2_GlyphPath`: path elements in character space go in, darkened and
/// hinted path elements in device space come out.
///
/// Every element waits in a queue of length one until the next element
/// shows how to join it. The hint maps are the caller's: the one that is
/// current and the one that was current at the last move.
pub(super) struct GlyphPath {
    scale: i32,
    x_offset: i32,
    y_offset: i32,
    miter_limit: i32,
    pub(super) reverse_winding: bool,
    pub(super) momentum: i32,
    /// A move happened. FreeType makes up the first one if the charstring
    /// starts with a line.
    pub(super) started: bool,
    current_cs: [i32; 2],
    pub(super) start: [i32; 2],
    move_pending: bool,
    open: bool,
    closing: bool,
    queued: bool,
    prev_op: PrevOp,
    prev: [[i32; 2]; 4],
    offset_start0: [i32; 2],
    offset_start1: [i32; 2],
    current_ds: [i32; 2],
    /// The contour of the output that has a start point (`path_begun`).
    contour_open: bool,
}

impl GlyphPath {
    pub(super) fn new(state: &HintState) -> Self {
        let x_offset = state.darken_x.to_bits();
        let y_offset = state.darken_y.to_bits();
        Self {
            scale: state.scale.to_bits(),
            x_offset,
            y_offset,
            miter_limit: 2 * abs(x_offset).max(abs(y_offset)),
            reverse_winding: false,
            momentum: 0,
            started: false,
            current_cs: [0; 2],
            start: [0; 2],
            move_pending: true,
            open: false,
            closing: false,
            queued: false,
            prev_op: PrevOp::Line,
            prev: [[0; 2]; 4],
            offset_start0: [0; 2],
            offset_start1: [0; 2],
            current_ds: [0; 2],
            contour_open: false,
        }
    }

    /// Starts the next charstring (`seac` component): the path of the last
    /// one is finished, and the winding state belongs to the whole glyph.
    pub(super) fn reset(&mut self) {
        let Self {
            scale,
            x_offset,
            y_offset,
            miter_limit,
            reverse_winding,
            momentum,
            contour_open,
            ..
        } = *self;
        *self = Self {
            scale,
            x_offset,
            y_offset,
            miter_limit,
            reverse_winding,
            momentum,
            started: false,
            current_cs: [0; 2],
            start: [0; 2],
            move_pending: true,
            open: false,
            closing: false,
            queued: false,
            prev_op: PrevOp::Line,
            prev: [[0; 2]; 4],
            offset_start0: [0; 2],
            offset_start1: [0; 2],
            current_ds: [0; 2],
            contour_open,
        };
    }

    /// `cf2_glyphpath_hintPoint` with the outer transform of FreeType's CFF
    /// driver, which is the identity.
    fn hint_point(&self, map: &HintMap, x: i32, y: i32) -> [i32; 2] {
        [
            mul_fix(self.scale, x),
            map.transform(Fixed::from_bits(y)).to_bits(),
        ]
    }

    fn close_contour<S: CommandSink>(&mut self, sink: &mut S) {
        if self.contour_open {
            sink.close();
            self.contour_open = false;
        }
    }

    /// `cf2_builder_lineTo`
    fn out_line<S: CommandSink>(&mut self, sink: &mut S, p0: [i32; 2], p1: [i32; 2]) {
        self.begin_contour(sink, p0);
        sink.line_to(to_output(p1[0]), to_output(p1[1]));
    }

    /// `cf2_builder_cubeTo`
    fn out_curve<S: CommandSink>(
        &mut self,
        sink: &mut S,
        p0: [i32; 2],
        p1: [i32; 2],
        p2: [i32; 2],
        p3: [i32; 2],
    ) {
        self.begin_contour(sink, p0);
        sink.curve_to(
            to_output(p1[0]),
            to_output(p1[1]),
            to_output(p2[0]),
            to_output(p2[1]),
            to_output(p3[0]),
            to_output(p3[1]),
        );
    }

    fn begin_contour<S: CommandSink>(&mut self, sink: &mut S, p0: [i32; 2]) {
        if !self.contour_open {
            sink.move_to(to_output(p0[0]), to_output(p0[1]));
            self.contour_open = true;
        }
    }

    /// Ends the output at the end of the glyph.
    pub(super) fn finish<S: CommandSink>(
        &mut self,
        map: &HintMap,
        first_map: &HintMap,
        sink: &mut S,
    ) {
        self.close_open_path(map, first_map, sink);
        self.close_contour(sink);
    }

    /// `cf2_glyphpath_computeOffset`: the offset of the path element from
    /// `(x1, y1)` to `(x2, y2)`; it also adds the element to the winding
    /// momentum.
    fn compute_offset(&mut self, x1: i32, y1: i32, x2: i32, y2: i32) -> [i32; 2] {
        let mut dx = x2.wrapping_sub(x1);
        let mut dy = y2.wrapping_sub(y1);
        // note: negative offsets don't work here; negate deltas to change
        // quadrants, below
        if self.reverse_winding {
            dx = dx.wrapping_neg();
            dy = dy.wrapping_neg();
        }
        // add momentum for this path element
        self.momentum = self
            .momentum
            .wrapping_add(winding_momentum(x1, y1, x2, y2));
        let xo = self.x_offset;
        let yo = self.y_offset;
        let twice = |v: i32| v.wrapping_mul(2);
        if dx >= 0 {
            if dy >= 0 {
                // first quadrant, +x +y
                if dx > twice(dy) {
                    // +x
                    [0, 0]
                } else if dy > twice(dx) {
                    // +y
                    [xo, yo]
                } else {
                    // +x +y
                    [mul_fix(SEVEN_TENTHS, xo), mul_fix(THREE_TENTHS, yo)]
                }
            } else {
                // fourth quadrant, +x -y
                if dx > twice(dy).wrapping_neg() {
                    // +x
                    [0, 0]
                } else if dy.wrapping_neg() > twice(dx) {
                    // -y
                    [xo.wrapping_neg(), yo]
                } else {
                    // +x -y
                    [mul_fix(NEG_SEVEN_TENTHS, xo), mul_fix(THREE_TENTHS, yo)]
                }
            }
        } else if dy >= 0 {
            // second quadrant, -x +y
            if dx.wrapping_neg() > twice(dy) {
                // -x
                [0, twice(yo)]
            } else if dy > twice(dx).wrapping_neg() {
                // +y
                [xo, yo]
            } else {
                // -x +y
                [mul_fix(SEVEN_TENTHS, xo), mul_fix(SEVENTEEN_TENTHS, yo)]
            }
        } else {
            // third quadrant, -x -y
            if dx.wrapping_neg() > twice(dy).wrapping_neg() {
                // -x
                [0, twice(yo)]
            } else if dy.wrapping_neg() > twice(dx).wrapping_neg() {
                // -y
                [xo.wrapping_neg(), yo]
            } else {
                // -x -y
                [mul_fix(NEG_SEVEN_TENTHS, xo), mul_fix(SEVENTEEN_TENTHS, yo)]
            }
        }
    }

    /// `cf2_glyphpath_computeIntersection`: a point of intersection of the
    /// lines of the segments `(u1, u2)` and `(v1, v2)`, unless there is none
    /// or it is too far from the ends `u2` and `v1`.
    fn compute_intersection(
        &self,
        u1: [i32; 2],
        u2: [i32; 2],
        v1: [i32; 2],
        v2: [i32; 2],
    ) -> Option<[i32; 2]> {
        // round and divide by 32
        fn cs_scale(x: i32) -> i32 {
            ((x as i64 + 0x10) >> 5) as i32
        }
        fn perp(a: [i32; 2], b: [i32; 2]) -> i32 {
            mul_fix(a[0], b[1]).wrapping_sub(mul_fix(a[1], b[0]))
        }
        let sub = |a: [i32; 2], b: [i32; 2]| {
            [
                cs_scale(a[0].wrapping_sub(b[0])),
                cs_scale(a[1].wrapping_sub(b[1])),
            ]
        };
        let u = sub(u2, u1);
        let v = sub(v2, v1);
        let w = sub(v1, u1);
        let denominator = perp(u, v);
        if denominator == 0 {
            // parallel or coincident lines
            return None;
        }
        let s = div_fix(perp(w, v), denominator);
        let mut intersection = [
            u1[0].wrapping_add(mul_fix(s, u2[0].wrapping_sub(u1[0]))),
            u1[1].wrapping_add(mul_fix(s, u2[1].wrapping_sub(u1[1]))),
        ];
        // Special case snapping for horizontal and vertical lines. This
        // cleans up intersections and reduces problems with winding order
        // detection.
        if u1[0] == u2[0] && abs(intersection[0].wrapping_sub(u1[0])) < SNAP_THRESHOLD {
            intersection[0] = u1[0];
        }
        if u1[1] == u2[1] && abs(intersection[1].wrapping_sub(u1[1])) < SNAP_THRESHOLD {
            intersection[1] = u1[1];
        }
        if v1[0] == v2[0] && abs(intersection[0].wrapping_sub(v1[0])) < SNAP_THRESHOLD {
            intersection[0] = v1[0];
        }
        if v1[1] == v2[1] && abs(intersection[1].wrapping_sub(v1[1])) < SNAP_THRESHOLD {
            intersection[1] = v1[1];
        }
        // limit the intersection distance from midpoint of u2 and v1
        if abs(intersection[0].wrapping_sub(u2[0].wrapping_add(v1[0]) / 2)) > self.miter_limit
            || abs(intersection[1].wrapping_sub(u2[1].wrapping_add(v1[1]) / 2)) > self.miter_limit
        {
            return None;
        }
        Some(intersection)
    }

    /// `cf2_glyphpath_pushPrevElem`: outputs the queued element, converted
    /// to device space. Its end point may become the intersection with the
    /// next element, whose start point `next_p0` then becomes the
    /// intersection, or a connecting line is made.
    fn push_prev_elem<S: CommandSink>(
        &mut self,
        map: &HintMap,
        first_map: &HintMap,
        sink: &mut S,
        next_p0: &mut [i32; 2],
        next_p1: [i32; 2],
        close: bool,
    ) {
        let (ix0, ix1) = match self.prev_op {
            PrevOp::Line => (0, 1),
            PrevOp::Curve => (2, 3),
        };
        let mut intersection = [0, 0];
        let mut use_intersection = false;
        // if previous and next elements are offset by the same amount, then
        // there will be no gap, and no need to compute an intersection
        if self.prev[ix1] != *next_p0 {
            // previous element does not join next element: adjust end point
            // of previous element to the intersection
            if let Some(found) =
                self.compute_intersection(self.prev[ix0], self.prev[ix1], *next_p0, next_p1)
            {
                use_intersection = true;
                intersection = found;
                // modify the last point of the cached element (either line
                // or curve)
                self.prev[ix1] = found;
            }
        }
        let pt0 = self.current_ds;
        match self.prev_op {
            PrevOp::Line => {
                // use first hint map if closing
                let pt1 = if close {
                    self.hint_point(first_map, self.prev[1][0], self.prev[1][1])
                } else {
                    self.hint_point(map, self.prev[1][0], self.prev[1][1])
                };
                // output only non-zero length lines
                if pt0 != pt1 {
                    self.out_line(sink, pt0, pt1);
                    self.current_ds = pt1;
                }
            }
            PrevOp::Curve => {
                let pt1 = self.hint_point(map, self.prev[1][0], self.prev[1][1]);
                let pt2 = self.hint_point(map, self.prev[2][0], self.prev[2][1]);
                let pt3 = self.hint_point(map, self.prev[3][0], self.prev[3][1]);
                self.out_curve(sink, pt0, pt1, pt2, pt3);
                self.current_ds = pt3;
            }
        }
        if !use_intersection || close {
            // insert connecting line between end of previous element and
            // start of current one
            let pt1 = if close {
                // if we are closing the subpath, then next_p0 is in the
                // first hint zone
                self.hint_point(first_map, next_p0[0], next_p0[1])
            } else {
                self.hint_point(map, next_p0[0], next_p0[1])
            };
            if pt1 != self.current_ds {
                // length is nonzero
                let pt0 = self.current_ds;
                self.out_line(sink, pt0, pt1);
                self.current_ds = pt1;
            }
        }
        if use_intersection {
            // return intersection point to caller
            *next_p0 = intersection;
        }
    }

    /// `cf2_glyphpath_pushMove`
    fn push_move<S: CommandSink>(&mut self, map: &HintMap, sink: &mut S, start: [i32; 2]) {
        let pt1 = self.hint_point(map, start[0], start[1]);
        // `cf2_builder_moveTo` only closes the contour: the point of the
        // next line or curve starts the next one
        self.close_contour(sink);
        self.current_ds = pt1;
        self.offset_start0 = start;
    }

    /// `cf2_glyphpath_moveTo` without the hint map, which the caller builds
    /// and saves.
    pub(super) fn move_to<S: CommandSink>(
        &mut self,
        map: &HintMap,
        first_map: &HintMap,
        sink: &mut S,
        x: i32,
        y: i32,
    ) {
        self.close_open_path(map, first_map, sink);
        // save the parameters of the move for later, when we'll know how to
        // offset it; also save last move point
        self.current_cs = [x, y];
        self.start = [x, y];
        self.move_pending = true;
        self.started = true;
    }

    /// `cf2_glyphpath_lineTo`. Returns true if the caller has to build a new
    /// hint map now (`is_new_mask` and the path is not being closed).
    pub(super) fn line_to<S: CommandSink>(
        &mut self,
        map: &HintMap,
        first_map: &HintMap,
        sink: &mut S,
        x: i32,
        y: i32,
        is_new_mask: bool,
    ) -> bool {
        // New hints will be applied after `push_prev_elem` has run. In case
        // this is a synthesized closing line, any new hints should be
        // delayed until this path is closed.
        let new_hint_map = is_new_mask && !self.closing;
        // Ignore zero-length lines in CS where the hint map is the same
        // because the line in DS will also be zero length. Ignore
        // zero-length lines when we synthesize a closing line because the
        // close will be handled in `push_prev_elem`.
        if self.current_cs == [x, y] && !new_hint_map {
            return false;
        }
        let [x_offset, y_offset] = self.compute_offset(self.current_cs[0], self.current_cs[1], x, y);
        // construct offset points
        let mut p0 = [
            self.current_cs[0].wrapping_add(x_offset),
            self.current_cs[1].wrapping_add(y_offset),
        ];
        let p1 = [x.wrapping_add(x_offset), y.wrapping_add(y_offset)];
        if self.move_pending {
            // emit offset 1st point as MoveTo
            self.push_move(map, sink, p0);
            self.move_pending = false;
            self.open = true;
            // record second point
            self.offset_start1 = p1;
        }
        if self.queued {
            self.push_prev_elem(map, first_map, sink, &mut p0, p1, false);
        }
        // queue the current element with offset points
        self.queued = true;
        self.prev_op = PrevOp::Line;
        self.prev[0] = p0;
        self.prev[1] = p1;
        // pre-offset current point
        self.current_cs = [x, y];
        new_hint_map
    }

    /// `cf2_glyphpath_curveTo`. Returns true if the caller has to build a
    /// new hint map now (`is_new_mask`).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn curve_to<S: CommandSink>(
        &mut self,
        map: &HintMap,
        first_map: &HintMap,
        sink: &mut S,
        [x1, y1, x2, y2, x3, y3]: [i32; 6],
        is_new_mask: bool,
    ) -> bool {
        let [x_offset1, y_offset1] =
            self.compute_offset(self.current_cs[0], self.current_cs[1], x1, y1);
        let [x_offset3, y_offset3] = self.compute_offset(x2, y2, x3, y3);
        // add momentum from the middle segment
        self.momentum = self
            .momentum
            .wrapping_add(winding_momentum(x1, y1, x2, y2));
        // construct offset points
        let mut p0 = [
            self.current_cs[0].wrapping_add(x_offset1),
            self.current_cs[1].wrapping_add(y_offset1),
        ];
        let p1 = [x1.wrapping_add(x_offset1), y1.wrapping_add(y_offset1)];
        // note: preserve angle of final segment by using offset3 at both ends
        let p2 = [x2.wrapping_add(x_offset3), y2.wrapping_add(y_offset3)];
        let p3 = [x3.wrapping_add(x_offset3), y3.wrapping_add(y_offset3)];
        if self.move_pending {
            // emit offset 1st point as MoveTo
            self.push_move(map, sink, p0);
            self.move_pending = false;
            self.open = true;
            // record second point
            self.offset_start1 = p1;
        }
        if self.queued {
            self.push_prev_elem(map, first_map, sink, &mut p0, p1, false);
        }
        // queue the current element with offset points
        self.queued = true;
        self.prev_op = PrevOp::Curve;
        self.prev = [p0, p1, p2, p3];
        // pre-offset current point
        self.current_cs = [x3, y3];
        is_new_mask
    }

    /// `cf2_glyphpath_closeOpenPath`
    pub(super) fn close_open_path<S: CommandSink>(
        &mut self,
        map: &HintMap,
        first_map: &HintMap,
        sink: &mut S,
    ) {
        if self.open {
            // A closing line in Character Space line is always generated
            // below with `line_to`. It may be ignored later if it turns out
            // to be zero length in Device Space.
            self.closing = true;
            let [x, y] = self.start;
            self.line_to(map, first_map, sink, x, y, false);
            // empty the final element from the queue and close the path
            if self.queued {
                let mut start0 = self.offset_start0;
                let start1 = self.offset_start1;
                self.push_prev_elem(map, first_map, sink, &mut start0, start1, true);
            }
            // reset state machine
            self.move_pending = true;
            self.open = false;
            self.closing = false;
            self.queued = false;
        }
    }
}

/// `cf2_fixedAbs`
fn abs(value: i32) -> i32 {
    value.wrapping_abs()
}
