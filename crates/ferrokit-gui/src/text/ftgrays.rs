// SPDX-License-Identifier: LGPL-3.0-only AND FTL
//! FreeType's anti-aliasing rasterizer (`ftgrays`) in safe Rust, with the parts of FreeType's
//! glyph slot code that turn a glyph outline into the 8-bit coverage bitmap `FT_Render_Glyph`
//! returns for `FT_RENDER_MODE_NORMAL` and `FT_RENDER_MODE_LIGHT`.
//!
//! Qt's FreeType engine draws every grey-scale glyph with `FT_Render_Glyph`
//! (`QFontEngineFT::loadGlyph`, qfontengine_ft.cpp), so a glyph bitmap that is to equal Qt's has to
//! come out of this algorithm bit for bit. This is a port of FreeType 2.14.2, the release Qt 6.12
//! runs on in the Linux reference setup (Ubuntu 26.04, `libfreetype6` 2.14.2):
//!
//! * `src/smooth/ftgrays.c`: `gray_raster_render`, `gray_convert_glyph` with its bands,
//!   `gray_set_cell`, the 64-bit `gray_render_line`, the DDA `gray_render_conic`, the bisecting
//!   `gray_render_cubic`, `gray_sweep` and `gray_sweep_direct`;
//! * `src/smooth/ftsmooth.c`: `ft_smooth_render` for the two grey modes and
//!   `ft_smooth_raster_overlap` (4x4 oversampling of outlines flagged `FT_OUTLINE_OVERLAP`);
//! * `src/base/ftobjs.c`: `ft_glyphslot_preset_bitmap` (the bitmap box from the control box);
//! * `src/base/ftoutln.c`: `FT_Outline_Decompose` and `FT_Outline_Get_CBox`.
//!
//! Types have the widths of an LP64 build, where Qt's FreeType engine is measured: `FT_Pos` and
//! `TPos` are 64-bit, `TCoord` and `TArea` 32-bit, and a cell takes 24 bytes, so the 16384-byte
//! render pool holds 682 cells (the pool size decides only when a band is bisected, never the
//! result). Arithmetic that FreeType does modulo 2^n (`ADD_INT`, `ADD_LONG`, integer casts) wraps
//! here as well. The one deliberate departure: where FreeType would run past the end of its fixed
//! cubic bisection stack (undefined behaviour, unreachable for outlines inside the bitmap limits
//! of `ft_glyphslot_preset_bitmap`), the arc is drawn as a line instead.
//!
//! Not ported: the LCD modes (Qt asks FreeType for them only for `Format_A32` glyphs, which its
//! raster engine never requests for `QImage` targets), monochrome rendering (`ftraster.c`), and
//! bitmap and SVG glyphs.
//!
//! The FreeType sources this file is derived from carry this notice (`ftgrays.c` and `ftsmooth.c`
//! with the years 2000-2025, `ftobjs.c` and `ftoutln.c` with 1996-2025):
//!
//! ```text
//! Copyright (C) 1996-2025 by
//! David Turner, Robert Wilhelm, and Werner Lemberg.
//!
//! This file is part of the FreeType project, and may only be used,
//! modified, and distributed under the terms of the FreeType project
//! license, LICENSE.TXT.  By continuing to use, modify, or distribute
//! this file you indicate that you have read the license and
//! understand and accept it fully.
//! ```
//!
//! The FreeType License is in `LICENSES/FTL.txt`; `NOTICE` has the credit it asks for.

/// `FT_CURVE_TAG_ON`: an on-curve point.
pub const TAG_ON: u8 = 0x01;
/// `FT_CURVE_TAG_CONIC`: the control point of a quadratic arc.
pub const TAG_CONIC: u8 = 0x00;
/// `FT_CURVE_TAG_CUBIC`: a control point of a cubic arc.
pub const TAG_CUBIC: u8 = 0x02;
/// `FT_OUTLINE_EVEN_ODD_FILL`: fill with the even-odd rule instead of the non-zero winding rule.
pub const OUTLINE_EVEN_ODD_FILL: u32 = 0x2;
/// `FT_OUTLINE_OVERLAP`: the outline has overlapping contours; rendered with 4x4 oversampling.
pub const OUTLINE_OVERLAP: u32 = 0x40;

/// `FT_Vector`: a point in 26.6 fixed point (1/64 pixel), y growing upwards.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Vector {
    pub x: i64,
    pub y: i64,
}

impl Vector {
    pub const fn new(x: i64, y: i64) -> Self {
        Self { x, y }
    }
}

/// `FT_Outline`: points with their curve tags, the index of the last point of every contour, and
/// the outline flags (`OUTLINE_*`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outline {
    pub points: Vec<Vector>,
    pub tags: Vec<u8>,
    pub contours: Vec<u16>,
    pub flags: u32,
}

/// `FT_BBox`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BBox {
    pub x_min: i64,
    pub y_min: i64,
    pub x_max: i64,
    pub y_max: i64,
}

/// The FreeType errors the grey rendering path can report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// `FT_Err_Invalid_Outline`.
    InvalidOutline,
    /// `FT_Err_Raster_Overflow`: a bitmap beyond the size limit, or a band that cannot be bisected.
    RasterOverflow,
}

/// The grey bitmap `FT_Render_Glyph` leaves in the glyph slot: `bitmap_left`, `bitmap_top` and an
/// 8-bit coverage buffer of `width` x `rows` bytes, top row first (`pitch == width`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Bitmap {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub rows: u32,
    pub buffer: Vec<u8>,
}

/// `FT_MulFix`: `a * b / 0x10000`, rounded half away from zero (`src/base/ftcalc.c`).
pub fn mul_fix(a: i64, b: i64) -> i64 {
    let ab = a.wrapping_mul(b);
    ab.wrapping_add(0x8000).wrapping_add(ab >> 63) >> 16
}

/// `FT_DivFix`: `a * 0x10000 / b`, rounded half away from zero; `0x7FFFFFFF` for `b == 0`.
pub fn div_fix(a: i64, b: i64) -> i64 {
    let negative = (a < 0) != (b < 0);
    let (a, b) = (a.unsigned_abs(), b.unsigned_abs());
    let q = a
        .wrapping_shl(16)
        .wrapping_add(b >> 1)
        .checked_div(b)
        .map_or(0x7FFF_FFFF, |q| q as i64);
    if negative { q.wrapping_neg() } else { q }
}

/// `FT_MulDiv`: `a * b / c`, rounded half away from zero; `0x7FFFFFFF` for `c == 0`.
pub fn mul_div(a: i64, b: i64, c: i64) -> i64 {
    let negative = ((a < 0) != (b < 0)) != (c < 0);
    let (a, b, c) = (a.unsigned_abs(), b.unsigned_abs(), c.unsigned_abs());
    let d = a
        .wrapping_mul(b)
        .wrapping_add(c >> 1)
        .checked_div(c)
        .map_or(0x7FFF_FFFF, |q| q as i64);
    if negative { d.wrapping_neg() } else { d }
}

/// `FT_Outline_Transform` with the matrix `(xx, xy, yx, yy)` in 16.16.
pub fn transform(points: &mut [Vector], xx: i64, xy: i64, yx: i64, yy: i64) {
    for v in points {
        let x = mul_fix(v.x, xx).wrapping_add(mul_fix(v.y, xy));
        let y = mul_fix(v.x, yx).wrapping_add(mul_fix(v.y, yy));
        *v = Vector::new(x, y);
    }
}

/// `FT_Outline_Get_CBox`: the box of every point, control points included.
pub fn control_box(points: &[Vector]) -> BBox {
    let Some(first) = points.first() else {
        return BBox::default();
    };
    let mut b = BBox {
        x_min: first.x,
        y_min: first.y,
        x_max: first.x,
        y_max: first.y,
    };
    for v in &points[1..] {
        b.x_min = b.x_min.min(v.x);
        b.x_max = b.x_max.max(v.x);
        b.y_min = b.y_min.min(v.y);
        b.y_max = b.y_max.max(v.y);
    }
    b
}

/// Placement and size of the bitmap `ft_glyphslot_preset_bitmap` sets up for a grey render mode
/// (no origin shift): `(bitmap_left, bitmap_top, width, rows)`, or `None` where FreeType refuses the
/// box (beyond 10 ppem or 0x8000 pixels from the origin; `FT_Render_Glyph` then fails with
/// `Raster_Overflow`). `ppem` is the face's `x_ppem` and `y_ppem`.
pub fn preset_bitmap(points: &[Vector], ppem: (u16, u16)) -> Option<(i32, i32, u32, u32)> {
    let cbox = control_box(points);
    // Rough estimate of the pixel box, then the tiny remainder box grid-fitted (ftobjs.c).
    let mut x_min = cbox.x_min >> 6;
    let mut y_min = cbox.y_min >> 6;
    let mut x_max = cbox.x_max >> 6;
    let mut y_max = cbox.y_max >> 6;
    x_min += (cbox.x_min & 63) >> 6;
    y_min += (cbox.y_min & 63) >> 6;
    x_max += ((cbox.x_max & 63) + 63) >> 6;
    y_max += ((cbox.y_max & 63) + 63) >> 6;

    let xlim = 0x8000i64.min(10 * i64::from(ppem.0));
    let ylim = 0x8000i64.min(10 * i64::from(ppem.1));
    if x_min < -xlim || x_max >= xlim || y_min < -ylim || y_max >= ylim {
        return None;
    }
    Some((
        x_min as i32,
        y_max as i32,
        (x_max - x_min) as u32,
        (y_max - y_min) as u32,
    ))
}

/// `FT_Render_Glyph` of an outline glyph slot in `FT_RENDER_MODE_NORMAL` or
/// `FT_RENDER_MODE_LIGHT` (they render alike; the modes differ only in how the glyph was hinted).
/// `ppem` is the face's `x_ppem` and `y_ppem` (see [`preset_bitmap`]).
pub fn render(outline: &Outline, ppem: (u16, u16)) -> Result<Bitmap, Error> {
    let (left, top, width, rows) =
        preset_bitmap(&outline.points, ppem).ok_or(Error::RasterOverflow)?;
    let mut bitmap = Bitmap {
        left,
        top,
        width,
        rows,
        buffer: Vec::new(),
    };
    // `pitch` is `width` for grey bitmaps.
    if rows == 0 || width == 0 {
        return Ok(bitmap);
    }
    bitmap.buffer = vec![0; rows as usize * width as usize];

    // Translate the outline to render it into the bitmap; FreeType shifts the slot's outline in
    // place and back, this works on a copy.
    let x_shift = i64::from(64i32.wrapping_mul(left.wrapping_neg()));
    let y_shift = i64::from(
        64i32
            .wrapping_mul(top.wrapping_neg())
            .wrapping_add(64i32.wrapping_mul(rows as i32)),
    );
    let mut moved = outline.clone();
    for v in &mut moved.points {
        v.x = v.x.wrapping_add(x_shift);
        v.y = v.y.wrapping_add(y_shift);
    }

    if outline.flags & OUTLINE_OVERLAP != 0 {
        raster_overlap(&mut moved, &mut bitmap)?;
    } else {
        let mut target = Target::Bitmap {
            buffer: &mut bitmap.buffer,
            pitch: width as usize,
            rows: rows as usize,
        };
        raster_render(&moved, &mut target, BBox::default())?;
    }
    Ok(bitmap)
}

/// `ft_smooth_raster_overlap`: renders the outline at four times the size in both directions
/// through direct spans and averages every 4x4 block into one pixel.
fn raster_overlap(outline: &mut Outline, bitmap: &mut Bitmap) -> Result<(), Error> {
    const SCALE: i64 = 4;
    // Reject outlines that are too wide for 16-bit `FT_Span`.
    if u64::from(bitmap.width) * SCALE as u64 > 0x7FFF {
        return Err(Error::RasterOverflow);
    }
    let clip = BBox {
        x_min: 0,
        y_min: 0,
        x_max: i64::from(bitmap.width) * SCALE,
        y_max: i64::from(bitmap.rows) * SCALE,
    };
    for v in &mut outline.points {
        v.x = v.x.wrapping_mul(SCALE);
        v.y = v.y.wrapping_mul(SCALE);
    }
    let mut target = Target::Overlap {
        buffer: &mut bitmap.buffer,
        pitch: bitmap.width as usize,
        rows: bitmap.rows as usize,
    };
    raster_render(outline, &mut target, clip)
}

/// Where the sweep writes: a bitmap (`gray_sweep`) or the spans of `ft_smooth_overlap_spans`
/// (`gray_sweep_direct`). Both buffers are top row first; FreeType addresses them from the
/// bottom-left origin with a negative step, which is the same memory.
enum Target<'a> {
    Bitmap {
        buffer: &'a mut [u8],
        pitch: usize,
        rows: usize,
    },
    Overlap {
        buffer: &'a mut [u8],
        pitch: usize,
        rows: usize,
    },
}

impl Target<'_> {
    /// `ft_smooth_overlap_spans` for one span of the oversampled row `y`.
    fn overlap_span(&mut self, y: i32, x: i32, len: i32, coverage: u8) {
        let Target::Overlap {
            buffer,
            pitch,
            rows,
        } = self
        else {
            return;
        };
        const SCALE: i32 = 4;
        let row = y / SCALE;
        if row < 0 || row as usize >= *rows {
            return;
        }
        let line = (*rows - 1 - row as usize) * *pitch;
        let cover = (u32::from(coverage) + (SCALE * SCALE / 2) as u32) / (SCALE * SCALE) as u32;
        for i in 0..len {
            let column = (x + i) / SCALE;
            if column < 0 || column as usize >= *pitch {
                continue;
            }
            let dst = &mut buffer[line + column as usize];
            let sum = u32::from(*dst) + cover;
            *dst = (sum - (sum >> 8)) as u8;
        }
    }
}

/// `PIXEL_BITS`: the rasterizer works in 24.8 sub-pixels.
const PIXEL_BITS: i32 = 8;
const ONE_PIXEL: i64 = 1 << PIXEL_BITS;
/// `FT_RENDER_POOL_SIZE / sizeof (TCell)` with 24-byte cells.
const MAX_GRAY_POOL: usize = 16384 / 24;
/// The dumpster cell at the end of the pool (`ras.cell_null`).
const NULL_CELL: usize = MAX_GRAY_POOL - 1;
/// `sizeof (PCell)` and `sizeof (TCell)` on LP64: the row lists take pool space.
const PCELL_BYTES: usize = 8;
const TCELL_BYTES: usize = 24;

#[inline]
fn trunc(x: i64) -> i32 {
    (x >> PIXEL_BITS) as i32
}

#[inline]
fn fract(x: i64) -> i32 {
    (x & (ONE_PIXEL - 1)) as i32
}

#[inline]
fn upscale(x: i64) -> i64 {
    x.wrapping_mul(ONE_PIXEL >> 6)
}

/// `FT_UDIV`: `a / b` through the reciprocal `FT_UDIVPREP` computed.
#[inline]
fn udiv(a: i64, reciprocal: i64) -> i32 {
    ((a as u64).wrapping_mul(reciprocal as u64) >> 32) as i32
}

/// `LEFT_SHIFT`.
#[inline]
fn left_shift(a: i64, b: i32) -> i64 {
    (a as u64).wrapping_shl(b as u32) as i64
}

/// `FT_FILL_RULE`: scales an area to a coverage and applies the fill rule. The caller keeps the
/// low byte.
#[inline]
fn fill_rule(area: i32, fill: i32) -> i32 {
    let mut coverage = area >> (PIXEL_BITS * 2 + 1 - 8);
    if coverage & fill != 0 {
        coverage = !coverage;
    }
    if coverage > 255 && fill & i32::MIN != 0 {
        coverage = 255;
    }
    coverage
}

#[derive(Clone, Copy)]
struct Cell {
    x: i32,
    cover: i32,
    area: i32,
    next: usize,
}

const EMPTY_CELL: Cell = Cell {
    x: i32::MAX,
    cover: 0,
    area: 0,
    next: NULL_CELL,
};

/// Where `gray_set_cell` links a cell in: the head of a row list or a cell's `next`.
#[derive(Clone, Copy)]
enum Link {
    Row(usize),
    Next(usize),
}

/// `gray_TWorker`.
struct Worker {
    min_ex: i32,
    max_ex: i32,
    min_ey: i32,
    max_ey: i32,
    count_ey: i32,
    error: Option<Error>,
    cell: usize,
    cell_free: usize,
    cells: [Cell; MAX_GRAY_POOL],
    /// The row lists; FreeType keeps them at the start of the cell pool (see `PCELL_BYTES`).
    ycells: Vec<usize>,
    x: i64,
    y: i64,
    even_odd: bool,
}

impl Worker {
    fn link(&self, link: Link) -> usize {
        match link {
            Link::Row(row) => self.ycells[row],
            Link::Next(cell) => self.cells[cell].next,
        }
    }

    fn set_link(&mut self, link: Link, to: usize) {
        match link {
            Link::Row(row) => self.ycells[row] = to,
            Link::Next(cell) => self.cells[cell].next = to,
        }
    }

    /// `gray_set_cell`: makes the cell at `(ex, ey)` current, inserting it into its row list; the
    /// null cell takes everything outside the band, cells left of it pile up at `min_ex - 1`.
    fn set_cell(&mut self, ex: i32, ey: i32) {
        let ey_index = ey.wrapping_sub(self.min_ey);
        if ey_index < 0 || ey_index >= self.count_ey || ex >= self.max_ex {
            self.cell = NULL_CELL;
            return;
        }
        let ex = ex.max(self.min_ex.wrapping_sub(1));
        let mut link = Link::Row(ey_index as usize);
        loop {
            let cell = self.link(link);
            let x = self.cells[cell].x;
            if x > ex {
                break;
            }
            if x == ex {
                self.cell = cell;
                return;
            }
            link = Link::Next(cell);
        }
        let cell = self.cell_free;
        if cell == NULL_CELL {
            self.error = Some(Error::RasterOverflow);
            self.cell = NULL_CELL;
            return;
        }
        self.cell_free = cell + 1;
        self.cells[cell] = Cell {
            x: ex,
            cover: 0,
            area: 0,
            next: self.link(link),
        };
        self.set_link(link, cell);
        self.cell = cell;
    }

    /// `FT_INTEGRATE`.
    #[inline]
    fn integrate(&mut self, a: i32, b: i32) {
        let cell = &mut self.cells[self.cell];
        cell.cover = cell.cover.wrapping_add(a);
        cell.area = cell.area.wrapping_add(a.wrapping_mul(b));
    }

    /// `gray_render_line`, the version FreeType builds when `FT_INT64` exists.
    fn render_line(&mut self, to_x: i64, to_y: i64) {
        let mut ey1 = trunc(self.y);
        let ey2 = trunc(to_y);

        // Vertical clipping.
        if (ey1 >= self.max_ey && ey2 >= self.max_ey) || (ey1 < self.min_ey && ey2 < self.min_ey) {
            self.x = to_x;
            self.y = to_y;
            return;
        }

        let mut ex1 = trunc(self.x);
        let ex2 = trunc(to_x);
        let mut fx1 = fract(self.x);
        let mut fy1 = fract(self.y);
        let dx = to_x.wrapping_sub(self.x);
        let dy = to_y.wrapping_sub(self.y);
        let one = ONE_PIXEL as i32;

        if ex1 == ex2 && ey1 == ey2 {
            // Inside one cell.
        } else if dy == 0 {
            // Any horizontal line.
            self.set_cell(ex2, ey2);
            self.x = to_x;
            self.y = to_y;
            return;
        } else if dx == 0 {
            if dy > 0 {
                // Vertical line up.
                loop {
                    self.integrate(one - fy1, fx1.wrapping_mul(2));
                    fy1 = 0;
                    ey1 = ey1.wrapping_add(1);
                    self.set_cell(ex1, ey1);
                    if ey1 == ey2 {
                        break;
                    }
                }
            } else {
                // Vertical line down.
                loop {
                    self.integrate(-fy1, fx1.wrapping_mul(2));
                    fy1 = one;
                    ey1 = ey1.wrapping_sub(1);
                    self.set_cell(ex1, ey1);
                    if ey1 == ey2 {
                        break;
                    }
                }
            }
        } else {
            // Any other line. `prod` says on which side, and where exactly, the line leaves the
            // current cell; it is updated as the line moves from cell to cell.
            let mut prod = dx
                .wrapping_mul(i64::from(fy1))
                .wrapping_sub(dy.wrapping_mul(i64::from(fx1)));
            let dx_r = if ex1 != ex2 { 0xFFFF_FFFFi64 / dx } else { 0 };
            let dy_r = if ey1 != ey2 { 0xFFFF_FFFFi64 / dy } else { 0 };
            let dx_one = dx.wrapping_mul(ONE_PIXEL);
            let dy_one = dy.wrapping_mul(ONE_PIXEL);
            loop {
                let fx2;
                let fy2;
                if prod.wrapping_sub(dx_one) > 0 && prod <= 0 {
                    // Left.
                    fx2 = 0;
                    fy2 = udiv(prod.wrapping_neg(), dx_r.wrapping_neg());
                    prod = prod.wrapping_sub(dy_one);
                    self.integrate(fy2.wrapping_sub(fy1), fx1.wrapping_add(fx2));
                    fx1 = one;
                    fy1 = fy2;
                    ex1 = ex1.wrapping_sub(1);
                } else if prod.wrapping_sub(dx_one).wrapping_add(dy_one) > 0
                    && prod.wrapping_sub(dx_one) <= 0
                {
                    // Up.
                    prod = prod.wrapping_sub(dx_one);
                    fx2 = udiv(prod.wrapping_neg(), dy_r);
                    fy2 = one;
                    self.integrate(fy2.wrapping_sub(fy1), fx1.wrapping_add(fx2));
                    fx1 = fx2;
                    fy1 = 0;
                    ey1 = ey1.wrapping_add(1);
                } else if prod.wrapping_add(dy_one) >= 0
                    && prod.wrapping_sub(dx_one).wrapping_add(dy_one) <= 0
                {
                    // Right.
                    prod = prod.wrapping_add(dy_one);
                    fx2 = one;
                    fy2 = udiv(prod, dx_r);
                    self.integrate(fy2.wrapping_sub(fy1), fx1.wrapping_add(fx2));
                    fx1 = 0;
                    fy1 = fy2;
                    ex1 = ex1.wrapping_add(1);
                } else {
                    // Down.
                    fx2 = udiv(prod, dy_r.wrapping_neg());
                    fy2 = 0;
                    prod = prod.wrapping_add(dx_one);
                    self.integrate(fy2.wrapping_sub(fy1), fx1.wrapping_add(fx2));
                    fx1 = fx2;
                    fy1 = one;
                    ey1 = ey1.wrapping_sub(1);
                }
                self.set_cell(ex1, ey1);
                if ex1 == ex2 && ey1 == ey2 {
                    break;
                }
            }
        }

        let fx2 = fract(to_x);
        let fy2 = fract(to_y);
        self.integrate(fy2.wrapping_sub(fy1), fx1.wrapping_add(fx2));
        self.x = to_x;
        self.y = to_y;
    }

    /// Whether every one of `ys` lies above or every one below the current band (the short cut
    /// of the curve renderers).
    fn outside_band(&self, ys: &[i64]) -> bool {
        ys.iter().all(|&y| trunc(y) >= self.max_ey) || ys.iter().all(|&y| trunc(y) < self.min_ey)
    }

    /// `gray_render_conic`, the DDA version FreeType builds when `FT_INT64` exists.
    fn render_conic(&mut self, control: Vector, to: Vector) {
        let (p0x, p0y) = (self.x, self.y);
        let (p1x, p1y) = (upscale(control.x), upscale(control.y));
        let (p2x, p2y) = (upscale(to.x), upscale(to.y));

        if self.outside_band(&[p0y, p1y, p2y]) {
            self.x = p2x;
            self.y = p2y;
            return;
        }

        let bx = p1x.wrapping_sub(p0x);
        let by = p1y.wrapping_sub(p0y);
        let ax = p2x.wrapping_sub(p1x).wrapping_sub(bx);
        let ay = p2y.wrapping_sub(p1y).wrapping_sub(by);

        let mut dx = ax.wrapping_abs();
        let dy = ay.wrapping_abs();
        if dx < dy {
            dx = dy;
        }
        if dx <= ONE_PIXEL / 4 {
            self.render_line(p2x, p2y);
            return;
        }

        // Each bisection reduces the deviation exactly 4-fold, so the number of segments is known.
        let mut shift = 16i32;
        loop {
            dx >>= 2;
            shift -= 1;
            if dx <= ONE_PIXEL / 4 {
                break;
            }
        }
        // A negative shift needs a deviation of 2^38 sub-pixels, far outside any bitmap FreeType
        // agrees to render.
        let shift = shift.max(0);
        let mut count = 0x10000u32 >> shift;

        let mut rx = left_shift(ax, shift + shift);
        let mut ry = left_shift(ay, shift + shift);
        let mut qx = left_shift(bx, shift + 17).wrapping_add(rx);
        let mut qy = left_shift(by, shift + 17).wrapping_add(ry);
        rx = rx.wrapping_mul(2);
        ry = ry.wrapping_mul(2);
        let mut px = left_shift(p0x, 32);
        let mut py = left_shift(p0y, 32);
        loop {
            px = px.wrapping_add(qx);
            py = py.wrapping_add(qy);
            qx = qx.wrapping_add(rx);
            qy = qy.wrapping_add(ry);
            self.render_line(px >> 32, py >> 32);
            count -= 1;
            if count == 0 {
                break;
            }
        }
    }

    /// `gray_render_cubic`.
    fn render_cubic(&mut self, control1: Vector, control2: Vector, to: Vector) {
        const STACK: usize = 16 * 3 + 1;
        let mut arc = [(0i64, 0i64); STACK];
        arc[0] = (upscale(to.x), upscale(to.y));
        arc[1] = (upscale(control2.x), upscale(control2.y));
        arc[2] = (upscale(control1.x), upscale(control1.y));
        arc[3] = (self.x, self.y);

        if self.outside_band(&[arc[0].1, arc[1].1, arc[2].1, arc[3].1]) {
            self.x = arc[0].0;
            self.y = arc[0].1;
            return;
        }

        let half = ONE_PIXEL / 2;
        let mut top = 0usize;
        loop {
            let a = &arc[top..top + 4];
            // With each split the control points converge towards the chord trisection points;
            // these distances vanish when the arc is flat enough to draw.
            let flat = deviation(a[0].0, a[1].0, a[3].0) <= half
                && deviation(a[0].1, a[1].1, a[3].1) <= half
                && deviation(a[3].0, a[2].0, a[0].0) <= half
                && deviation(a[3].1, a[2].1, a[0].1) <= half;
            if !flat && top + 7 <= STACK {
                split_cubic(&mut arc[top..top + 7]);
                top += 3;
                continue;
            }
            self.render_line(arc[top].0, arc[top].1);
            if top == 0 {
                return;
            }
            top -= 3;
        }
    }

    fn move_to(&mut self, to: Vector) -> Result<(), Error> {
        let x = upscale(to.x);
        let y = upscale(to.y);
        self.set_cell(trunc(x), trunc(y));
        self.x = x;
        self.y = y;
        self.error.map_or(Ok(()), Err)
    }

    fn line_to(&mut self, to: Vector) -> Result<(), Error> {
        self.render_line(upscale(to.x), upscale(to.y));
        self.error.map_or(Ok(()), Err)
    }

    fn conic_to(&mut self, control: Vector, to: Vector) -> Result<(), Error> {
        self.render_conic(control, to);
        self.error.map_or(Ok(()), Err)
    }

    fn cubic_to(&mut self, control1: Vector, control2: Vector, to: Vector) -> Result<(), Error> {
        self.render_cubic(control1, control2, to);
        self.error.map_or(Ok(()), Err)
    }

    /// `FT_Outline_Decompose` with the rasterizer's callbacks (shift 0, delta 0).
    fn decompose(&mut self, outline: &Outline) -> Result<(), Error> {
        let points = &outline.points;
        let tags = &outline.tags;
        let tag_of = |i: usize| tags[i] & 3;
        let mut last: isize = -1;
        for &end in &outline.contours {
            let first = (last + 1) as usize;
            last = end as isize;
            if last < first as isize {
                return Err(Error::InvalidOutline);
            }
            let last_ix = last as usize;
            let mut limit = last_ix as isize;

            let mut v_start = points[first];
            let v_last = points[last_ix];
            let mut v_control;
            let mut point = first as isize;

            let tag = tag_of(first);
            // A contour cannot start with a cubic control point.
            if tag == TAG_CUBIC {
                return Err(Error::InvalidOutline);
            }
            if tag == TAG_CONIC {
                if tag_of(last_ix) == TAG_ON {
                    // Start at the last point if it is on the curve.
                    v_start = v_last;
                    limit -= 1;
                } else {
                    // Both first and last points are conic: start at their middle.
                    v_start.x = v_start.x.wrapping_add(v_last.x) / 2;
                    v_start.y = v_start.y.wrapping_add(v_last.y) / 2;
                }
                point -= 1;
            }

            self.move_to(v_start)?;

            let mut closed = false;
            'contour: while point < limit {
                point += 1;
                let p = point as usize;
                match tag_of(p) {
                    TAG_ON => {
                        self.line_to(points[p])?;
                    }
                    TAG_CONIC => {
                        v_control = points[p];
                        loop {
                            if point < limit {
                                point += 1;
                                let p = point as usize;
                                let vec = points[p];
                                let tag = tag_of(p);
                                if tag == TAG_ON {
                                    self.conic_to(v_control, vec)?;
                                    continue 'contour;
                                }
                                if tag != TAG_CONIC {
                                    return Err(Error::InvalidOutline);
                                }
                                let v_middle = Vector::new(
                                    v_control.x.wrapping_add(vec.x) / 2,
                                    v_control.y.wrapping_add(vec.y) / 2,
                                );
                                self.conic_to(v_control, v_middle)?;
                                v_control = vec;
                                continue;
                            }
                            self.conic_to(v_control, v_start)?;
                            closed = true;
                            break 'contour;
                        }
                    }
                    _ => {
                        // FT_CURVE_TAG_CUBIC (and the unused tag 3).
                        if point + 1 > limit || tag_of(p + 1) != TAG_CUBIC {
                            return Err(Error::InvalidOutline);
                        }
                        point += 2;
                        let p = point as usize;
                        let vec1 = points[p - 2];
                        let vec2 = points[p - 1];
                        if point <= limit {
                            self.cubic_to(vec1, vec2, points[p])?;
                            continue;
                        }
                        self.cubic_to(vec1, vec2, v_start)?;
                        closed = true;
                        break 'contour;
                    }
                }
            }
            if !closed {
                // Close the contour with a line segment.
                self.line_to(v_start)?;
            }
        }
        Ok(())
    }

    /// `gray_sweep` into a bitmap.
    fn sweep(&self, buffer: &mut [u8], pitch: usize, rows: usize) {
        let fill = if self.even_odd { 0x100 } else { i32::MIN };
        for y in self.min_ey..self.max_ey {
            let mut cell = self.ycells[(y - self.min_ey) as usize];
            let mut x = self.min_ex;
            let mut cover: i32 = 0;
            let line = (rows - 1 - y as usize) * pitch;
            while cell != NULL_CELL {
                let c = self.cells[cell];
                if cover != 0 && c.x > x {
                    let coverage = fill_rule(cover, fill) as u8;
                    buffer[line + x as usize..line + c.x as usize].fill(coverage);
                }
                cover = cover.wrapping_add(c.cover.wrapping_mul(ONE_PIXEL as i32 * 2));
                let area = cover.wrapping_sub(c.area);
                if area != 0 && c.x >= self.min_ex {
                    buffer[line + c.x as usize] = fill_rule(area, fill) as u8;
                }
                x = c.x + 1;
                cell = c.next;
            }
            // Only if cropped.
            if cover != 0 && x < self.max_ex {
                let coverage = fill_rule(cover, fill) as u8;
                buffer[line + x as usize..line + self.max_ex as usize].fill(coverage);
            }
        }
    }

    /// `gray_sweep_direct`: the same coverages as spans for the target.
    fn sweep_direct(&self, target: &mut Target) {
        let fill = if self.even_odd { 0x100 } else { i32::MIN };
        for y in self.min_ey..self.max_ey {
            let mut cell = self.ycells[(y - self.min_ey) as usize];
            let mut x = self.min_ex;
            let mut cover: i32 = 0;
            while cell != NULL_CELL {
                let c = self.cells[cell];
                if cover != 0 && c.x > x {
                    target.overlap_span(y, x, c.x - x, fill_rule(cover, fill) as u8);
                }
                cover = cover.wrapping_add(c.cover.wrapping_mul(ONE_PIXEL as i32 * 2));
                let area = cover.wrapping_sub(c.area);
                if area != 0 && c.x >= self.min_ex {
                    target.overlap_span(y, c.x, 1, fill_rule(area, fill) as u8);
                }
                x = c.x + 1;
                cell = c.next;
            }
            if cover != 0 {
                // `FT_Span.len` is 16 bits.
                let len = (self.max_ex - x) as u16;
                target.overlap_span(y, x, i32::from(len), fill_rule(cover, fill) as u8);
            }
        }
    }
}

/// The deviation `|2 a - 3 b + c|` `gray_render_cubic` tests against half a pixel.
#[inline]
fn deviation(a: i64, b: i64, c: i64) -> i64 {
    a.wrapping_mul(2)
        .wrapping_sub(b.wrapping_mul(3))
        .wrapping_add(c)
        .wrapping_abs()
}

/// `gray_split_cubic`: splits the arc `base[0..4]` (end point first) into `base[0..7]`.
fn split_cubic(base: &mut [(i64, i64)]) {
    fn split(b: [i64; 4]) -> [i64; 7] {
        let mut out = [0; 7];
        out[0] = b[0];
        out[6] = b[3];
        let mut a = b[0].wrapping_add(b[1]);
        let bb = b[1].wrapping_add(b[2]);
        let mut c = b[2].wrapping_add(b[3]);
        out[5] = c >> 1;
        c = c.wrapping_add(bb);
        out[4] = c >> 2;
        out[1] = a >> 1;
        a = a.wrapping_add(bb);
        out[2] = a >> 2;
        out[3] = a.wrapping_add(c) >> 3;
        out
    }
    let xs = split([base[0].0, base[1].0, base[2].0, base[3].0]);
    let ys = split([base[0].1, base[1].1, base[2].1, base[3].1]);
    for i in 0..7 {
        base[i] = (xs[i], ys[i]);
    }
}

/// `gray_raster_render` and `gray_convert_glyph`: `clip` is the direct-mode clip box (ignored for
/// a bitmap target, whose box is the bitmap).
fn raster_render(outline: &Outline, target: &mut Target, clip: BBox) -> Result<(), Error> {
    let n_points = outline.points.len();
    if n_points == 0 || outline.contours.is_empty() {
        return Ok(());
    }
    if outline.tags.len() != n_points
        || n_points != usize::from(outline.contours[outline.contours.len() - 1]) + 1
    {
        return Err(Error::InvalidOutline);
    }

    let cbox = match target {
        Target::Bitmap { pitch, rows, .. } => BBox {
            x_min: 0,
            y_min: 0,
            x_max: *pitch as i64,
            y_max: *rows as i64,
        },
        Target::Overlap { .. } => clip,
    };
    if cbox.x_min >= cbox.x_max || cbox.y_min >= cbox.y_max {
        return Ok(());
    }
    let (x_min, x_max) = (cbox.x_min as i32, cbox.x_max as i32);
    let (y_min, y_max) = (cbox.y_min as i32, cbox.y_max as i32);

    let mut worker = Worker {
        min_ex: 0,
        max_ex: 0,
        min_ey: 0,
        max_ey: 0,
        count_ey: 0,
        error: None,
        cell: NULL_CELL,
        cell_free: 0,
        cells: [EMPTY_CELL; MAX_GRAY_POOL],
        ycells: Vec::new(),
        x: 0,
        y: 0,
        even_odd: outline.flags & OUTLINE_EVEN_ODD_FILL != 0,
    };

    // Vertical bands of at most `n` rows.
    let mut height = (cbox.y_max - cbox.y_min) as usize;
    let mut n = MAX_GRAY_POOL / 8;
    if height > n {
        // Two divisions rounded up.
        n = height.div_ceil(n);
        height = height.div_ceil(n);
    }

    let mut y = y_min;
    while y < y_max {
        worker.min_ey = y;
        y = y.wrapping_add(height as i32);
        worker.max_ey = y.min(y_max);
        worker.count_ey = worker.max_ey - worker.min_ey;

        // The horizontal band stack of `gray_convert_glyph`: `bands[top]` is `band[0]` (the right
        // edge), `bands[top + 1]` is `band[1]` (the left edge).
        let mut bands = [0i32; 33];
        let mut top: isize = 0;
        bands[1] = x_min;
        bands[0] = x_max;
        loop {
            let t = top as usize;
            worker.min_ex = bands[t + 1];
            worker.max_ex = bands[t];

            // Memory management: reset and skip the row lists.
            worker.ycells.clear();
            worker.ycells.resize(worker.count_ey as usize, NULL_CELL);
            worker.cell_free = (worker.count_ey as usize * PCELL_BYTES).div_ceil(TCELL_BYTES);
            worker.cell = NULL_CELL;
            worker.cells[NULL_CELL] = EMPTY_CELL;
            worker.error = None;

            match worker.decompose(outline) {
                Ok(()) => {
                    match target {
                        Target::Bitmap {
                            buffer,
                            pitch,
                            rows,
                        } => worker.sweep(buffer, *pitch, *rows),
                        Target::Overlap { .. } => worker.sweep_direct(target),
                    }
                    top -= 1;
                    if top < 0 {
                        break;
                    }
                }
                Err(Error::RasterOverflow) => {
                    // Render pool overflow: halve the band.
                    let i = (bands[t] - bands[t + 1]) >> 1;
                    if i == 0 || t + 2 >= bands.len() {
                        return Err(Error::RasterOverflow);
                    }
                    top += 1;
                    let t = top as usize;
                    bands[t + 1] = bands[t];
                    bands[t] += i;
                }
                Err(e) => return Err(e),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An outline from 26.6 points, tags and contour ends.
    fn outline(points: &[(i64, i64)], tags: &[u8], contours: &[u16], flags: u32) -> Outline {
        Outline {
            points: points.iter().map(|&(x, y)| Vector { x, y }).collect(),
            tags: tags.to_vec(),
            contours: contours.to_vec(),
            flags,
        }
    }

    fn hex_rows(b: &Bitmap) -> Vec<String> {
        b.buffer
            .chunks(b.width as usize)
            .map(|row| row.iter().map(|v| format!("{v:02x}")).collect())
            .collect()
    }

    /// Bitmaps of `FT_Outline_Get_Bitmap` (FreeType 2.14.2) for the same outlines, written by
    /// `tests/data/gen_ft_golden.py outline`. `render` also places the box as `FT_Render_Glyph`
    /// does, which for these outlines is the floor and ceiling of the control box.
    fn check(o: &Outline, left: i32, top: i32, rows: &[&str]) {
        let b = render(o, (16, 16)).unwrap();
        assert_eq!(
            (b.left, b.top, b.width, b.rows),
            (left, top, rows[0].len() as u32 / 2, rows.len() as u32)
        );
        assert_eq!(hex_rows(&b), rows);
    }

    const ON: u8 = TAG_ON;

    #[test]
    fn pixel_aligned_square_is_fully_covered() {
        let o = outline(&[(0, 0), (0, 192), (192, 192), (192, 0)], &[ON; 4], &[3], 0);
        check(&o, 0, 3, &["ffffff", "ffffff", "ffffff"]);
    }

    #[test]
    fn square_on_half_pixels_has_half_covered_edges() {
        let o = outline(
            &[(32, 32), (32, 224), (224, 224), (224, 32)],
            &[ON; 4],
            &[3],
            0,
        );
        check(&o, 0, 4, &["40808040", "80ffff80", "80ffff80", "40808040"]);
    }

    #[test]
    fn triangle_matches_freetype() {
        let o = outline(&[(0, 0), (160, 320), (320, 0)], &[ON; 3], &[2], 0);
        check(
            &o,
            0,
            5,
            &[
                "0000800000",
                "0040ff3f00",
                "00c0ffbf00",
                "40ffffff3f",
                "c0ffffffbf",
            ],
        );
    }

    #[test]
    fn conic_arc_matches_freetype() {
        let o = outline(
            &[(0, 0), (128, 384), (256, 0)],
            &[ON, TAG_CONIC, ON],
            &[2],
            0,
        );
        check(
            &o,
            0,
            6,
            &[
                "00000000", "00000000", "00000000", "04b7b704", "64ffff64", "cfffffcf",
            ],
        );
    }

    /// A contour of conic points only: its on-curve points are the midpoints of neighbouring
    /// controls, here at odd coordinate sums.
    #[test]
    fn contour_of_conic_points_matches_freetype() {
        let o = outline(
            &[(0, 65), (130, 321), (257, 65), (131, -191)],
            &[TAG_CONIC; 4],
            &[3],
            0,
        );
        check(
            &o,
            0,
            6,
            &[
                "0000000000",
                "0000000000",
                "009fa80000",
                "2effff3500",
                "6effff7400",
                "6dffff7400",
                "2bffff3500",
                "0096a40000",
                "0000000000",
            ],
        );
    }

    #[test]
    fn cubic_arc_matches_freetype() {
        let o = outline(
            &[(0, 0), (0, 400), (300, 400), (300, 0)],
            &[ON, TAG_CUBIC, TAG_CUBIC, ON],
            &[3],
            0,
        );
        check(
            &o,
            0,
            7,
            &[
                "0000000000",
                "0000000000",
                "0051972700",
                "3afcffdf06",
                "a3ffffff51",
                "dbffffff89",
                "f4ffffffa3",
            ],
        );
    }

    #[test]
    fn even_odd_fill_leaves_the_inner_contour_empty() {
        let pts = [
            (0, 0),
            (0, 384),
            (384, 384),
            (384, 0),
            (96, 96),
            (96, 288),
            (288, 288),
            (288, 96),
        ];
        let o = outline(&pts, &[ON; 8], &[3, 7], OUTLINE_EVEN_ODD_FILL);
        check(
            &o,
            0,
            6,
            &[
                "ffffffffffff",
                "ffbf7f7fbfff",
                "ff7f00007fff",
                "ff7f00007fff",
                "ffbf7f7fbfff",
                "ffffffffffff",
            ],
        );
        // Non-zero winding fills the same two clockwise contours completely.
        let o = outline(&pts, &[ON; 8], &[3, 7], 0);
        assert!(
            render(&o, (16, 16))
                .unwrap()
                .buffer
                .iter()
                .all(|&v| v == 0xff)
        );
    }

    #[test]
    fn fixed_point_helpers_round_half_away_from_zero() {
        assert_eq!(mul_fix(3, 0x8000), 2);
        assert_eq!(mul_fix(-3, 0x8000), -2);
        assert_eq!(mul_fix(0x10000, 0x10000), 0x10000);
        assert_eq!(div_fix(1, 2), 0x8000);
        assert_eq!(div_fix(-1, 2), -0x8000);
        assert_eq!(div_fix(5, 0), 0x7FFF_FFFF);
        assert_eq!(mul_div(7, 3, 2), 11);
        assert_eq!(mul_div(-7, 3, 2), -11);
        assert_eq!(mul_div(1, 1, 0), 0x7FFF_FFFF);
    }

    #[test]
    fn control_box_includes_off_curve_points() {
        let o = outline(
            &[(0, 0), (128, 384), (256, -5)],
            &[ON, TAG_CONIC, ON],
            &[2],
            0,
        );
        let b = control_box(&o.points);
        assert_eq!((b.x_min, b.y_min, b.x_max, b.y_max), (0, -5, 256, 384));
        assert_eq!(control_box(&[]), BBox::default());
    }

    #[test]
    fn an_empty_outline_has_no_bitmap_area() {
        let b = render(&Outline::default(), (16, 16)).unwrap();
        assert!(b.width == 0 || b.rows == 0);
    }

    #[test]
    fn a_glyph_beyond_the_bitmap_limit_is_refused() {
        let o = outline(&[(0, 0), (0, 64 << 15), (64 << 15, 0)], &[ON; 3], &[2], 0);
        assert!(render(&o, (16, 16)).is_err());
    }
}
