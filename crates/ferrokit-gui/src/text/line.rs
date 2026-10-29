// SPDX-License-Identifier: LGPL-3.0-only
//! Single rendered line inside a text layout (`QTextLine` equivalent).
//!
//! Provides geometric bounds, baseline metrics, and bidirectional cursor-to-coordinate mapping.

use crate::geometry::primitives::{PointF, RectF};
use crate::text::bidi::{level_runs, visual_order, LevelRun};

/// Edge of a character or grapheme cluster for cursor positioning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Edge {
    #[default]
    Leading,
    Trailing,
}

/// Mode for mapping an X coordinate back to a cursor position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorPosition {
    #[default]
    CursorBetweenCharacters,
    CursorOnCharacter,
}

/// A laid-out line of text (`QTextLine`).
#[derive(Debug, Clone, PartialEq)]
pub struct TextLine {
    /// Line index within the parent `TextLayout`.
    pub(crate) index: usize,
    /// Starting grapheme offset in the layout text.
    pub(crate) text_start: usize,
    /// Number of grapheme clusters in this line.
    pub(crate) text_length: usize,
    /// Starting UTF-8 byte offset in layout text.
    pub(crate) byte_start: usize,
    /// Byte length of this line.
    pub(crate) byte_length: usize,
    /// Top-left position of the line.
    pub(crate) position: PointF,
    /// Allocated line width.
    pub(crate) width: f32,
    /// Line total height (`ascent + descent + leading`).
    pub(crate) height: f32,
    /// Font ascent (distance from top to baseline).
    pub(crate) ascent: f32,
    /// Font descent (distance from baseline to bottom).
    pub(crate) descent: f32,
    /// Inter-line leading gap.
    pub(crate) leading: f32,
    /// Width of the text on this line with the ink that hangs over the end of its last glyph
    /// (`QTextLine::naturalTextWidth`): [`advance_width`](Self::advance_width) plus the negative
    /// right bearing of the last glyph. The spaces at the end of a wrapped line are not part of it.
    pub(crate) natural_width: f32,
    /// The advances of the text of the line (`QTextLine::horizontalAdvance`): where the pen is
    /// after its last glyph, what [`cursor_to_x`](Self::cursor_to_x) at the line end gives and
    /// what aligning the line goes by.
    pub(crate) advance_width: f32,
    /// The x that [`cursor_to_x`](Self::cursor_to_x) never exceeds: the right end of the wrap
    /// width, past which the spaces that hang over it are not shown (Qt clamps to the line width).
    pub(crate) right_limit: f32,
    /// The bidi embedding level of every grapheme of the line, in logical order; empty when the
    /// line is left to right all through.
    pub(crate) levels: Vec<u8>,
    /// Whether the text of the layout reads right to left as a whole.
    pub(crate) base_rtl: bool,
    /// Whether this is the last line of its layout.
    pub(crate) is_last: bool,
    /// Cumulative horizontal offsets for each grapheme in this line.
    /// Length is `text_length + 1` (ending with `advance_width`).
    pub(crate) grapheme_x_advances: Vec<f32>,
}

impl Default for TextLine {
    fn default() -> Self {
        Self {
            index: 0,
            text_start: 0,
            text_length: 0,
            byte_start: 0,
            byte_length: 0,
            position: PointF::new(0.0, 0.0),
            width: 0.0,
            height: 0.0,
            ascent: 0.0,
            descent: 0.0,
            leading: 0.0,
            natural_width: 0.0,
            advance_width: 0.0,
            right_limit: f32::INFINITY,
            levels: Vec::new(),
            base_rtl: false,
            is_last: true,
            grapheme_x_advances: vec![0.0],
        }
    }
}

impl TextLine {
    /// Returns the line index.
    #[inline]
    pub fn line_number(&self) -> usize {
        self.index
    }

    /// Returns whether this line is valid and non-empty.
    #[inline]
    pub fn is_valid(&self) -> bool {
        self.height > 0.0
    }

    /// Returns the top-left position of this line.
    #[inline]
    pub fn position(&self) -> PointF {
        self.position
    }

    /// Sets the top-left position of this line.
    #[inline]
    pub fn set_position(&mut self, pos: PointF) {
        self.position = pos;
    }

    /// Returns the bounding rectangle of the line.
    #[inline]
    pub fn rect(&self) -> RectF {
        RectF::new(self.position.x, self.position.y, self.width, self.height)
    }

    /// Returns the tight rectangle enclosing only the natural text.
    #[inline]
    pub fn natural_text_rect(&self) -> RectF {
        RectF::new(
            self.position.x,
            self.position.y,
            self.natural_width,
            self.height,
        )
    }

    #[inline]
    pub fn x(&self) -> f32 {
        self.position.x
    }

    #[inline]
    pub fn y(&self) -> f32 {
        self.position.y
    }

    #[inline]
    pub fn width(&self) -> f32 {
        self.width
    }

    #[inline]
    pub fn height(&self) -> f32 {
        self.height
    }

    #[inline]
    pub fn ascent(&self) -> f32 {
        self.ascent
    }

    #[inline]
    pub fn descent(&self) -> f32 {
        self.descent
    }

    #[inline]
    pub fn leading(&self) -> f32 {
        self.leading
    }

    /// Width of the text including the ink that overhangs the advance of its last glyph
    /// (`QTextLine::naturalTextWidth`).
    #[inline]
    pub fn natural_text_width(&self) -> f32 {
        self.natural_width
    }

    /// Sum of the advances of the text of the line (`QTextLine::horizontalAdvance`).
    #[inline]
    pub fn horizontal_advance(&self) -> f32 {
        self.advance_width
    }

    #[inline]
    pub fn set_line_width(&mut self, width: f32) {
        self.width = width;
    }

    /// Returns starting grapheme cluster offset in the document / layout.
    #[inline]
    pub fn text_start(&self) -> usize {
        self.text_start
    }

    /// Returns number of grapheme clusters in this line.
    #[inline]
    pub fn text_length(&self) -> usize {
        self.text_length
    }

    /// Returns starting UTF-8 byte offset in layout text.
    #[inline]
    pub fn byte_start(&self) -> usize {
        self.byte_start
    }

    /// Returns byte length in layout text.
    #[inline]
    pub fn byte_length(&self) -> usize {
        self.byte_length
    }

    /// Converts a grapheme cursor position into an absolute X coordinate (`cursorToX`).
    ///
    /// Accepts either local line offset (0..text_length) or layout-global grapheme offset.
    pub fn cursor_to_x(&self, mut cursor_pos: usize, edge: Edge) -> f32 {
        if cursor_pos >= self.text_start && cursor_pos <= self.text_start + self.text_length {
            cursor_pos -= self.text_start;
        }

        let local_pos = cursor_pos.min(self.text_length);
        if !self.levels.is_empty() {
            return self.bidi_cursor_to_x(local_pos, edge).min(self.right_limit);
        }
        // A line a separator ends has no entry for the end (the separator has no width): its end
        // is where the pen stopped.
        let base_x = if local_pos < self.grapheme_x_advances.len() {
            self.grapheme_x_advances[local_pos]
        } else {
            self.grapheme_x_advances
                .last()
                .copied()
                .unwrap_or(self.advance_width)
        };

        let offset = match edge {
            Edge::Leading => 0.0,
            Edge::Trailing => {
                if local_pos + 1 < self.grapheme_x_advances.len() {
                    self.grapheme_x_advances[local_pos + 1] - base_x
                } else {
                    0.0
                }
            }
        };

        (self.position.x + base_x + offset).min(self.right_limit)
    }

    /// Maps an X coordinate back to a grapheme cursor position (`xToCursor`, qtextlayout.cpp:3079).
    ///
    /// Returns the global grapheme index within the layout text. Left of the line is its start
    /// (its end in a right to left text); right of the text it is the end (the start in a right
    /// to left text), except on every line but the last, where it is one position short of that
    /// so the cursor stays on the line instead of jumping to the next one.
    pub fn x_to_cursor(&self, x: f32, mode: CursorPosition) -> usize {
        let rel_x = x - self.position.x;
        if self.text_length == 0 {
            return self.text_start;
        }
        if rel_x <= 0.0 {
            return self.text_start + if self.base_rtl { self.text_length } else { 0 };
        }
        // Qt looks for the run under x only up to `textWidth`; the ink that hangs over the last
        // advance is within it but in no run, so it answers as the space right of the text.
        if rel_x < self.natural_width {
            if let Some(pos) = self.x_to_cursor_in_text(rel_x, mode) {
                return self.text_start + pos;
            }
        }
        let end = self.text_start + if self.base_rtl { 0 } else { self.text_length };
        if self.is_last {
            end
        } else {
            end.saturating_sub(1).min(end)
        }
    }

    /// The position under `rel_x` (from the line's left edge) when it is over the text: the run
    /// whose right edge is at or past it (so a boundary between runs belongs to the left one),
    /// then the grapheme in it, counted from the run's leading edge. `None` when no run reaches
    /// as far as `rel_x`.
    fn x_to_cursor_in_text(&self, rel_x: f32, mode: CursorPosition) -> Option<usize> {
        let visual = if self.levels.is_empty() {
            vec![VisualRun {
                run: LevelRun {
                    start: 0,
                    end: self.text_length,
                    level: 0,
                },
                left: 0.0,
                width: self.advance_between(0, self.text_length),
            }]
        } else {
            self.visual_runs()
        };
        if visual.is_empty() {
            return Some(0);
        }
        let run = visual.iter().find(|v| rel_x <= v.left + v.width).copied()?;
        let local = rel_x - run.left;
        // Distance from the run's leading edge, where its first grapheme starts: the right edge of
        // a right to left run.
        let from_leading = if run.run.is_rtl() {
            run.width - local
        } else {
            local
        };
        let boundary = |i: usize| self.advance_between(run.run.start, i);
        match mode {
            // The grapheme whose leading edge is the last one not past the point.
            CursorPosition::CursorOnCharacter => Some(
                (run.run.start..run.run.end)
                    .rev()
                    .find(|&i| boundary(i) <= from_leading)
                    .unwrap_or(run.run.start),
            ),
            // The nearest boundary; halfway the first one reached from the leading edge wins.
            CursorPosition::CursorBetweenCharacters => {
                let mut best = run.run.start;
                let mut best_distance = f32::INFINITY;
                for i in run.run.start..=run.run.end {
                    let distance = (from_leading - boundary(i)).abs();
                    if distance < best_distance {
                        best = i;
                        best_distance = distance;
                    }
                }
                Some(best)
            }
        }
    }
}

/// One run of a line in the order it is shown, with where it sits.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct VisualRun {
    /// The graphemes of the run, as indices into the line (not the layout).
    pub run: LevelRun,
    /// The left edge of the run, from the line's own left edge.
    pub left: f32,
    /// The width of the run.
    pub width: f32,
}

impl TextLine {
    /// The advance of the graphemes `start..end` of the line.
    fn advance_between(&self, start: usize, end: usize) -> f32 {
        let at = |i: usize| self.grapheme_x_advances.get(i).copied().unwrap_or(0.0);
        at(end) - at(start)
    }

    /// The runs of the line from left to right (rule L2), each with its left edge. Only for lines
    /// with right to left text; a left to right line is one run starting at 0.
    pub(crate) fn visual_runs(&self) -> Vec<VisualRun> {
        let runs = level_runs(&self.levels);
        let levels: Vec<u8> = runs.iter().map(|r| r.level).collect();
        let mut left = 0.0;
        visual_order(&levels)
            .into_iter()
            .map(|i| {
                let run = runs[i];
                let width = self.advance_between(run.start, run.end);
                let visual = VisualRun { run, left, width };
                left += width;
                visual
            })
            .collect()
    }

    /// The left edge, from the line's own left edge, of the visual run holding the line's
    /// grapheme `at`, and that run's graphemes.
    pub(crate) fn run_left_of(&self, at: usize) -> Option<VisualRun> {
        self.visual_runs()
            .into_iter()
            .find(|v| v.run.start <= at && at < v.run.end)
    }

    /// The `(x, width)` pieces of the line covered by the layout graphemes `start..end` (clipped
    /// to the line): one piece for left to right text, one per run of the selection in a line
    /// with right to left text, where a selection can be in several places.
    pub(crate) fn selection_spans(&self, start: usize, end: usize) -> Vec<(f32, f32)> {
        let line_start = self.text_start;
        let start = start.saturating_sub(line_start).min(self.text_length);
        let end = end.saturating_sub(line_start).min(self.text_length);
        if start >= end {
            return Vec::new();
        }
        if self.levels.is_empty() {
            let x = self.position.x + self.advance_between(0, start);
            return vec![(x, self.advance_between(start, end))];
        }
        self.visual_runs()
            .into_iter()
            .filter_map(|v| {
                let (a, b) = (start.max(v.run.start), end.min(v.run.end));
                if a >= b {
                    return None;
                }
                let before = if v.run.is_rtl() {
                    self.advance_between(b, v.run.end)
                } else {
                    self.advance_between(v.run.start, a)
                };
                Some((
                    self.position.x + v.left + before,
                    self.advance_between(a, b),
                ))
            })
            .collect()
    }

    /// `QTextLine::cursorToX` for a line with right to left text: the cursor between two runs of
    /// different direction goes to the run that reads in the paragraph's direction (left to
    /// right), and inside a right to left run the leading edge of a grapheme is its right edge.
    fn bidi_cursor_to_x(&self, pos: usize, edge: Edge) -> f32 {
        let runs = level_runs(&self.levels);
        let Some(last) = runs.len().checked_sub(1) else {
            return self.position.x;
        };
        let containing = |at: usize| runs.iter().position(|r| r.start <= at && at < r.end);
        let mut item = if pos >= self.text_length {
            containing(self.text_length.saturating_sub(1)).unwrap_or(last)
        } else {
            containing(pos).unwrap_or(last)
        };
        // Between two runs of different direction the cursor goes to the one that reads in the
        // direction of the text.
        if runs[item].is_rtl() != self.base_rtl {
            let mut neighbor = item;
            if item > 0 && runs[item].start == pos {
                neighbor -= 1;
            } else if item < last && runs[item].end == pos {
                neighbor += 1;
            }
            if neighbor != item && runs[neighbor].level != runs[item].level {
                item = neighbor;
            }
        }
        let run = runs[item];
        let mut inside = pos.clamp(run.start, run.end);
        if edge == Edge::Trailing && inside < run.end {
            inside += 1;
        }
        let left = self
            .visual_runs()
            .into_iter()
            .find(|v| v.run == run)
            .map_or(0.0, |v| v.left);
        let within = if run.is_rtl() {
            self.advance_between(inside, run.end)
        } else {
            self.advance_between(run.start, inside)
        };
        self.position.x + left + within
    }
}

/// Canonical Qt alias.
pub type QTextLine = TextLine;
