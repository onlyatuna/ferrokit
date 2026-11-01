// SPDX-License-Identifier: LGPL-3.0-only
//! Single rendered line inside a text layout (`QTextLine` equivalent).
//!
//! Provides geometric bounds, baseline metrics, and bidirectional cursor-to-coordinate mapping.

use crate::geometry::primitives::{PointF, RectF};
use crate::text::bidi::{LevelRun, level_runs, visual_order};

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
    /// Line height (`QTextLine::height`: the engine's ascent plus descent, rounded up).
    pub(crate) height: f32,
    /// Distance from the top of this line to the top of the next one in a plain text, which also
    /// takes the leading (see `FontMetrics::line_pitch`).
    pub(crate) pitch: f32,
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
    /// The bidi runs of the line from left to right (rule L2), each with its left edge, built
    /// once with the line by [`visual_runs`]; empty when the line is left to right all through.
    /// A line is never changed after it is built (a new layout builds new lines), so they stay
    /// valid.
    pub(crate) runs: Vec<VisualRun>,
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
            pitch: 0.0,
            ascent: 0.0,
            descent: 0.0,
            leading: 0.0,
            natural_width: 0.0,
            advance_width: 0.0,
            right_limit: f32::INFINITY,
            runs: Vec::new(),
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

    /// Distance from this line's top to the next line's top (see [`FontMetrics::line_pitch`]).
    ///
    /// [`FontMetrics::line_pitch`]: crate::text::FontMetrics::line_pitch
    #[inline]
    pub fn pitch(&self) -> f32 {
        self.pitch
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
        if !self.runs.is_empty() {
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
        if rel_x < self.natural_width
            && let Some(pos) = self.x_to_cursor_in_text(rel_x, mode)
        {
            return self.text_start + pos;
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
        let ltr;
        let visual: &[VisualRun] = if self.runs.is_empty() {
            ltr = [VisualRun {
                run: LevelRun {
                    start: 0,
                    end: self.text_length,
                    level: 0,
                },
                left: 0.0,
                width: self.advance_between(0, self.text_length),
            }];
            &ltr
        } else {
            &self.runs
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

/// The height of lines stacked as `qt_format_text` stacks them (qpainter.cpp:7370-7388): every
/// line but the last advances by its [`pitch`](TextLine::pitch), the last adds its own height.
pub(crate) fn stacked_height<'a>(lines: impl IntoIterator<Item = &'a TextLine>) -> f32 {
    let (mut top, mut advance, mut last_height) = (0.0, 0.0, 0.0);
    for line in lines {
        top += advance;
        advance = line.pitch;
        last_height = line.height;
    }
    top + last_height
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

/// The runs of a line from left to right (rule L2), each with its left edge, given the bidi
/// level of each of its graphemes in logical order and the cumulative advances of
/// [`TextLine::grapheme_x_advances`]. Empty when every level is 0 (a left to right line).
pub(crate) fn visual_runs(levels: &[u8], x_advances: &[f32]) -> Vec<VisualRun> {
    #[cfg(test)]
    tests::VISUAL_RUN_BUILDS.with(|c| c.set(c.get() + 1));
    if levels.iter().all(|&l| l == 0) {
        return Vec::new();
    }
    let at = |i: usize| advance_at(x_advances, i);
    let runs = level_runs(levels);
    let run_levels: Vec<u8> = runs.iter().map(|r| r.level).collect();
    let mut left = 0.0;
    visual_order(&run_levels)
        .into_iter()
        .map(|i| {
            let run = runs[i];
            let width = at(run.end) - at(run.start);
            let visual = VisualRun { run, left, width };
            left += width;
            visual
        })
        .collect()
}

/// The pen position before the line's grapheme `i`: past the last entry (the separator that ends
/// a line has none, it has no width) it is where the pen stopped.
fn advance_at(x_advances: &[f32], i: usize) -> f32 {
    x_advances
        .get(i)
        .or_else(|| x_advances.last())
        .copied()
        .unwrap_or(0.0)
}

impl TextLine {
    /// The advance of the graphemes `start..end` of the line.
    fn advance_between(&self, start: usize, end: usize) -> f32 {
        advance_at(&self.grapheme_x_advances, end) - advance_at(&self.grapheme_x_advances, start)
    }

    /// The left edge, from the line's own left edge, of the visual run holding the line's
    /// grapheme `at`, and that run's graphemes.
    pub(crate) fn run_left_of(&self, at: usize) -> Option<VisualRun> {
        self.runs
            .iter()
            .find(|v| v.run.start <= at && at < v.run.end)
            .copied()
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
        if self.runs.is_empty() {
            let x = self.position.x + self.advance_between(0, start);
            return vec![(x, self.advance_between(start, end))];
        }
        self.runs
            .iter()
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
        // The runs are kept in visual order; their logical neighbours are found by offset.
        let run_with = |f: &dyn Fn(&LevelRun) -> bool| self.runs.iter().find(|v| f(&v.run));
        let at = if pos >= self.text_length {
            self.text_length.saturating_sub(1)
        } else {
            pos
        };
        // A position in no run goes to the logically last one.
        let Some(mut item) = run_with(&|r| r.start <= at && at < r.end)
            .or_else(|| self.runs.iter().max_by_key(|v| v.run.start))
        else {
            return self.position.x;
        };
        // Between two runs of different direction the cursor goes to the one that reads in the
        // direction of the text.
        if item.run.is_rtl() != self.base_rtl {
            let neighbor = if item.run.start > 0 && item.run.start == pos {
                run_with(&|r| r.end == pos)
            } else if item.run.end == pos {
                run_with(&|r| r.start == pos)
            } else {
                None
            };
            if let Some(neighbor) = neighbor.filter(|n| n.run.level != item.run.level) {
                item = neighbor;
            }
        }
        let run = item.run;
        let mut inside = pos.clamp(run.start, run.end);
        if edge == Edge::Trailing && inside < run.end {
            inside += 1;
        }
        let left = item.left;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::font::{Font, SharedFontData};
    use crate::text::layout::TextLayout;
    use std::cell::Cell;

    thread_local! {
        /// `visual_runs` calls on this thread.
        pub(super) static VISUAL_RUN_BUILDS: Cell<usize> = const { Cell::new(0) };
    }

    fn mixed_layout(text: &str) -> TextLayout {
        let font = Font::new("FerrokitOracleText", 16.0).with_font_data(SharedFontData::from_vec(
            include_bytes!("../../tests/data/oracle_text.ttf").to_vec(),
        ));
        let mut layout = TextLayout::new(text, font);
        layout.do_layout();
        layout
    }

    #[test]
    fn bidi_runs_are_built_once_per_line_not_per_query() {
        VISUAL_RUN_BUILDS.with(|c| c.set(0));
        let layout = mixed_layout("ab \u{5d0}\u{5d1}\u{5d2} cd");
        assert_eq!(layout.line_count(), 1);
        let built = VISUAL_RUN_BUILDS.with(Cell::get);
        assert_eq!(built, 1, "one line, one build");
        let line = layout.line_at(0).unwrap();
        assert_eq!(line.runs.len(), 3, "ltr, rtl, ltr");
        for i in 0..=line.text_length() {
            line.cursor_to_x(i, Edge::Leading);
            line.cursor_to_x(i, Edge::Trailing);
            line.x_to_cursor(i as f32 * 4.0, CursorPosition::CursorBetweenCharacters);
            line.selection_spans(0, i);
        }
        assert_eq!(VISUAL_RUN_BUILDS.with(Cell::get), built);
    }

    #[test]
    fn bidi_runs_follow_a_new_layout() {
        let mut layout = mixed_layout("\u{5d0}\u{5d1} ab");
        let before = layout.line_at(0).unwrap().runs.clone();
        layout.set_text("ab \u{5d0}\u{5d1}\u{5d2} cd");
        layout.do_layout();
        let line = layout.line_at(0).unwrap();
        assert_ne!(line.runs, before);
        let levels = [0, 0, 0, 1, 1, 1, 0, 0, 0];
        assert_eq!(line.runs, visual_runs(&levels, &line.grapheme_x_advances));
    }

    /// The separator that ends a line has no advance entry; the bidi run holding it ends where
    /// the pen stopped, so no run is negative and a selection of the whole line covers its text.
    #[test]
    fn a_line_separator_ends_its_bidi_run_where_the_pen_stopped() {
        for text in ["\u{5d0}\u{5d1}\u{2028}x", "ab \u{5d0}\u{5d1}\u{2028}x"] {
            let layout = mixed_layout(text);
            let line = layout.line_at(0).unwrap();
            assert!(!line.runs.is_empty(), "{text:?}");
            assert!(line.runs.iter().all(|r| r.width >= 0.0), "{text:?}");
            let total: f32 = line.runs.iter().map(|r| r.width).sum();
            assert_eq!(total, line.horizontal_advance(), "{text:?}");
            let spans = line.selection_spans(0, line.text_length());
            let covered: f32 = spans.iter().map(|&(_, w)| w).sum();
            assert_eq!(covered, line.horizontal_advance(), "{text:?}");
            assert!(spans.iter().all(|&(x, w)| x >= 0.0 && w >= 0.0), "{text:?}");
        }
    }
}
