// SPDX-License-Identifier: LGPL-3.0-only
//! Multi-line text flow, line breaking, and layout engine (`QTextLayout` equivalent).
//!
//! Coordinates text shaping, word wrapping, IME pre-edit composition display,
//! selection highlighting, and drawing to `Painter`.

use crate::alignment::Alignment;
use crate::geometry::primitives::{PointF, RectF};
use crate::paint::painter::Painter;
use crate::text::font::Font;
use crate::text::font_metrics::FontMetrics;
use crate::text::format::FormatRange;
use crate::text::line::{Edge, TextLine};
use crate::text::position::TextPosition;
use std::sync::Arc;
use tiny_skia::Color;
use unicode_segmentation::UnicodeSegmentation;

/// The text plus pre-edit as one string with the byte offset of every grapheme, built once per
/// layout run so `create_line` does not rebuild and re-segment it for every line.
#[derive(Debug)]
struct CompositeText {
    text: String,
    /// Byte offset where each grapheme starts, followed by `text.len()`.
    starts: Vec<usize>,
}

impl CompositeText {
    fn grapheme_count(&self) -> usize {
        self.starts.len() - 1
    }

    fn grapheme(&self, index: usize) -> &str {
        &self.text[self.starts[index]..self.starts[index + 1]]
    }
}

/// Lazily built [`CompositeText`]; a cache, so it never takes part in layout equality.
#[derive(Debug, Clone, Default)]
struct CompositeCache(Option<Arc<CompositeText>>);

impl PartialEq for CompositeCache {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

/// Multi-line text layout engine (`QTextLayout`).
#[derive(Debug, Clone, PartialEq)]
pub struct TextLayout {
    /// Full plain text to lay out.
    text: String,
    /// Base font.
    font: Font,
    /// IME composition pre-edit text inserted at `preedit_pos`.
    preedit_pos: usize,
    preedit_text: String,
    /// Overriding format ranges (syntax highlighting, links, user formats).
    formats: Vec<FormatRange>,
    /// Word wrapping width constraint (if None, single unbounded line per paragraph).
    wrap_width: Option<f32>,
    /// Horizontal paragraph alignment.
    alignment: Alignment,
    /// Laid-out lines.
    lines: Vec<TextLine>,
    /// Top-left origin position.
    position: PointF,
    /// Internal layout iterator state for `create_line()`.
    layout_cursor: usize,
    /// Composite text of the layout in progress; reset by every change that restarts layout.
    composite: CompositeCache,
}

impl TextLayout {
    /// Creates a new text layout with the given text and font.
    pub fn new(text: impl Into<String>, font: Font) -> Self {
        Self {
            text: text.into(),
            font,
            preedit_pos: 0,
            preedit_text: String::new(),
            formats: Vec::new(),
            wrap_width: None,
            alignment: Alignment::LEFT,
            lines: Vec::new(),
            position: PointF::new(0.0, 0.0),
            layout_cursor: 0,
            composite: CompositeCache::default(),
        }
    }

    /// Returns the text currently being laid out.
    #[inline]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Sets the text and resets layout.
    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.clear_layout();
    }

    /// Returns the base font.
    #[inline]
    pub fn font(&self) -> &Font {
        &self.font
    }

    /// Sets the base font.
    pub fn set_font(&mut self, font: Font) {
        self.font = font;
        self.clear_layout();
    }

    /// Returns the composition text pre-edit area.
    #[inline]
    pub fn preedit_area_text(&self) -> &str {
        &self.preedit_text
    }

    #[inline]
    pub fn preedit_area_position(&self) -> usize {
        self.preedit_pos
    }

    /// Sets IME pre-edit composition text at the given grapheme index.
    pub fn set_preedit_area(&mut self, position: usize, text: impl Into<String>) {
        self.preedit_pos = position;
        self.preedit_text = text.into();
        self.clear_layout();
    }

    /// Sets format overrides (e.g. for syntax highlighting or styling).
    pub fn set_formats(&mut self, formats: Vec<FormatRange>) {
        self.formats = formats;
    }

    #[inline]
    pub fn formats(&self) -> &[FormatRange] {
        &self.formats
    }

    pub fn clear_formats(&mut self) {
        self.formats.clear();
    }

    /// Sets the line wrap width limit.
    pub fn set_wrap_width(&mut self, width: Option<f32>) {
        self.wrap_width = width;
        self.clear_layout();
    }

    /// Sets text alignment (`QTextOption::setAlignment`); only the horizontal flags apply.
    pub fn set_alignment(&mut self, alignment: Alignment) {
        self.alignment = alignment;
    }

    #[inline]
    pub fn position(&self) -> PointF {
        self.position
    }

    #[inline]
    pub fn set_position(&mut self, pos: PointF) {
        self.position = pos;
    }

    /// Clears existing line layout.
    pub fn clear_layout(&mut self) {
        self.lines.clear();
        self.layout_cursor = 0;
        self.composite = CompositeCache::default();
    }

    /// Begins line-by-line layout.
    pub fn begin_layout(&mut self) {
        self.clear_layout();
    }

    /// Produces the next `TextLine` from the layout, wrapping words if necessary.
    pub fn create_line(&mut self) -> Option<TextLine> {
        let composite = self.composite();
        let total_graphemes = composite.grapheme_count();

        // A text ending in a hard break has a final empty line (the caret after `'\n'`); an
        // empty text is a single empty line. `layout_cursor == total + 1` means that line is done.
        let ends_with_break =
            total_graphemes > 0 && composite.grapheme(total_graphemes - 1) == "\n";
        if self.layout_cursor > total_graphemes
            || (self.layout_cursor == total_graphemes && total_graphemes > 0 && !ends_with_break)
        {
            return None;
        }

        if self.layout_cursor == total_graphemes {
            self.layout_cursor = total_graphemes + 1;
            let metrics = FontMetrics::from_font(&self.font);
            let line_idx = self.lines.len();
            let x_pos = if self.alignment.contains(Alignment::RIGHT) {
                self.position.x + self.wrap_width.unwrap_or(0.0)
            } else if self.alignment.contains(Alignment::H_CENTER) {
                self.position.x + self.wrap_width.unwrap_or(0.0) * 0.5
            } else {
                self.position.x
            };
            return Some(TextLine {
                index: line_idx,
                text_start: total_graphemes,
                text_length: 0,
                byte_start: composite.text.len(),
                byte_length: 0,
                position: PointF::new(x_pos, self.position.y + line_idx as f32 * metrics.height),
                width: self.wrap_width.unwrap_or(0.0),
                height: metrics.height,
                ascent: metrics.ascent,
                descent: metrics.descent,
                leading: metrics.leading(),
                natural_width: 0.0,
                grapheme_x_advances: vec![0.0],
            });
        }

        let metrics = FontMetrics::from_font(&self.font);
        let max_width = self.wrap_width.unwrap_or(f32::MAX);

        // `layout_cursor < total_graphemes` here, so `starts` has an entry for it.
        let line_start_grapheme = self.layout_cursor;

        let mut curr_grapheme = line_start_grapheme;
        let mut curr_x = 0.0f32;
        let mut advances = vec![0.0f32];
        let mut last_break_grapheme = None;
        let mut last_break_advances_len = 0;

        let byte_start = composite.starts[line_start_grapheme];

        let mut line_bytes = 0;

        while curr_grapheme < total_graphemes {
            let g = composite.grapheme(curr_grapheme);
            if g == "\n" {
                // Hard line break
                curr_grapheme += 1;
                line_bytes += g.len();
                break;
            }

            let g_width = metrics.horizontal_advance_exact(g, &self.font);

            if curr_x + g_width > max_width && curr_grapheme > line_start_grapheme {
                // Wrapping needed
                if let Some(break_pos) = last_break_grapheme {
                    if break_pos > line_start_grapheme {
                        curr_grapheme = break_pos;
                        advances.truncate(last_break_advances_len);
                        // Recompute byte count up to break_pos
                        line_bytes = composite.starts[curr_grapheme] - byte_start;
                        break;
                    }
                }
                // If no break point was found, break at current character (hard break)
                break;
            }

            curr_x += g_width;
            advances.push(curr_x);
            line_bytes += g.len();

            // Track word break opportunity (whitespace or punctuation)
            if g.chars()
                .all(|c| c.is_whitespace() || c == '-' || c == '/' || c == ',')
            {
                last_break_grapheme = Some(curr_grapheme + 1);
                last_break_advances_len = advances.len();
            }

            curr_grapheme += 1;
        }

        let text_len = curr_grapheme - line_start_grapheme;
        let natural_w = advances.last().copied().unwrap_or(0.0);
        let line_idx = self.lines.len();

        let y_pos = self.position.y + (line_idx as f32) * metrics.height;
        // `QTextEngine::alignLine` (qtextengine.cpp:3558-3572): Right, else HCenter, else the
        // line starts at the left (Left, Justify, no flag).
        let x_pos = if self.alignment.contains(Alignment::RIGHT) {
            let avail = self.wrap_width.unwrap_or(natural_w);
            self.position.x + (avail - natural_w).max(0.0)
        } else if self.alignment.contains(Alignment::H_CENTER) {
            let avail = self.wrap_width.unwrap_or(natural_w);
            self.position.x + (avail - natural_w).max(0.0) * 0.5
        } else {
            self.position.x
        };

        self.layout_cursor = curr_grapheme;

        Some(TextLine {
            index: line_idx,
            text_start: line_start_grapheme,
            text_length: text_len,
            byte_start,
            byte_length: line_bytes,
            position: PointF::new(x_pos, y_pos),
            width: self.wrap_width.unwrap_or(natural_w),
            height: metrics.height,
            ascent: metrics.ascent,
            descent: metrics.descent,
            leading: metrics.leading(),
            natural_width: natural_w,
            grapheme_x_advances: advances,
        })
    }

    /// Finishes layout and stores the created lines.
    pub fn end_layout(&mut self) {
        while let Some(line) = self.create_line() {
            self.lines.push(line);
        }
    }

    /// Performs complete layout execution automatically.
    pub fn do_layout(&mut self) {
        self.begin_layout();
        self.end_layout();
    }

    #[inline]
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    #[inline]
    pub fn line_at(&self, idx: usize) -> Option<&TextLine> {
        self.lines.get(idx)
    }

    #[inline]
    pub fn line_at_mut(&mut self, idx: usize) -> Option<&mut TextLine> {
        self.lines.get_mut(idx)
    }

    /// Finds the line containing the given global grapheme cluster position.
    pub fn line_for_text_position(&self, pos: usize) -> Option<&TextLine> {
        // `QTextLayout::lineForTextPosition`: a line owns `[start, start + length)`; the end of
        // text (and anything past it) belongs to the last line.
        self.lines
            .iter()
            .find(|line| pos >= line.text_start && pos < line.text_start + line.text_length)
            .or_else(|| self.lines.last())
    }

    /// Returns the complete bounding rectangle covering all laid out lines.
    pub fn bounding_rect(&self) -> RectF {
        if self.lines.is_empty() {
            return RectF::new(self.position.x, self.position.y, 0.0, 0.0);
        }

        let mut max_w: f32 = 0.0;
        let mut total_h: f32 = 0.0;

        for line in &self.lines {
            if line.natural_width > max_w {
                max_w = line.natural_width;
            }
            total_h += line.height;
        }

        RectF::new(self.position.x, self.position.y, max_w, total_h)
    }

    /// Draws the laid-out text to the given `Painter`, in black where no format sets a colour.
    pub fn draw(&self, painter: &mut Painter, pos: PointF, selections: &[FormatRange]) {
        self.draw_with_color(painter, pos, selections, Color::BLACK);
    }

    /// Draws the laid-out text; `default_color` is used where no format sets a foreground colour
    /// (`QAbstractTextDocumentLayout` takes the palette text colour as its default).
    pub fn draw_with_color(
        &self,
        painter: &mut Painter,
        pos: PointF,
        selections: &[FormatRange],
        default_color: Color,
    ) {
        let full_text = self.composite_text();
        let graphemes: Vec<&str> = full_text.graphemes(true).collect();

        for line in &self.lines {
            let line_y = pos.y + line.position.y + line.ascent;
            let line_start = line.text_start;
            let line_end = line_start + line.text_length;

            // Draw selection background if applicable
            for sel in selections {
                let sel_end = sel.start + sel.length;
                let overlap_start = sel.start.max(line_start).min(line_end);
                let overlap_end = sel_end.max(line_start).min(line_end);

                if overlap_start < overlap_end {
                    let x1 = line.cursor_to_x(overlap_start, Edge::Leading);
                    let x2 = line.cursor_to_x(overlap_end, Edge::Leading);
                    let bg_color = sel
                        .format
                        .background
                        .unwrap_or(Color::from_rgba8(0, 120, 215, 80));

                    let sel_rect =
                        RectF::new(pos.x + x1, pos.y + line.position.y, x2 - x1, line.height);
                    painter.fill_rect(sel_rect, bg_color);
                }
            }

            // Draw line text segments
            for i in 0..line.text_length {
                let g_idx = line_start + i;
                if g_idx >= graphemes.len() {
                    break;
                }
                let g_str = graphemes[g_idx];
                let g_x = pos.x + line.cursor_to_x(g_idx, Edge::Leading);

                // Format override
                let mut char_color = default_color;
                for fmt in &self.formats {
                    if g_idx >= fmt.start && g_idx < fmt.start + fmt.length {
                        if let Some(fg) = fmt.format.foreground {
                            char_color = fg;
                        }
                    }
                }

                painter.draw_text_colored(PointF::new(g_x, line_y), g_str, &self.font, char_color);
            }
        }
    }

    /// Draws a text cursor at the specified grapheme position.
    pub fn draw_cursor(
        &self,
        painter: &mut Painter,
        pos: PointF,
        cursor_pos: usize,
        cursor_width: f32,
    ) {
        if let Some(line) = self.line_for_text_position(cursor_pos) {
            let cursor_x = pos.x + line.cursor_to_x(cursor_pos, Edge::Leading);
            let cursor_y = pos.y + line.position.y;
            let cursor_rect = RectF::new(cursor_x, cursor_y, cursor_width.max(1.0), line.height);
            painter.fill_rect(cursor_rect, Color::BLACK);
        }
    }

    /// The composite text for the layout in progress, built on first use after the layout was
    /// (re)started.
    fn composite(&mut self) -> Arc<CompositeText> {
        if let Some(cached) = &self.composite.0 {
            return Arc::clone(cached);
        }
        let text = self.composite_text();
        let mut starts: Vec<usize> = text.grapheme_indices(true).map(|(i, _)| i).collect();
        starts.push(text.len());
        let built = Arc::new(CompositeText { text, starts });
        self.composite.0 = Some(Arc::clone(&built));
        built
    }

    /// Produces combined text including preedit IME text.
    fn composite_text(&self) -> String {
        #[cfg(test)]
        tests::COMPOSITE_BUILDS.with(|c| c.set(c.get() + 1));
        if self.preedit_text.is_empty() {
            return self.text.clone();
        }
        let pos = TextPosition::from_grapheme(&self.text, self.preedit_pos);
        let byte_split = pos.byte.0.min(self.text.len());
        format!(
            "{}{}{}",
            &self.text[..byte_split],
            self.preedit_text,
            &self.text[byte_split..]
        )
    }
}

/// Canonical Qt alias.
pub type QTextLayout = TextLayout;

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    thread_local! {
        /// `composite_text` calls on this thread.
        pub(super) static COMPOSITE_BUILDS: Cell<usize> = const { Cell::new(0) };
    }

    #[test]
    fn do_layout_builds_the_composite_text_once() {
        let mut layout = TextLayout::new("one\ntwo\nthree\nfour\nfive", Font::new("Segoe UI", 12.0));
        layout.set_preedit_area(3, "ime");
        COMPOSITE_BUILDS.with(|c| c.set(0));
        layout.do_layout();
        assert_eq!(layout.line_count(), 5);
        assert_eq!(COMPOSITE_BUILDS.with(Cell::get), 1, "one composite per layout, not per line");
    }

    #[test]
    fn cached_composite_text_follows_text_and_preedit_changes() {
        let mut layout = TextLayout::new("ab\ncd", Font::new("Segoe UI", 12.0));
        layout.do_layout();
        assert_eq!(layout.line_count(), 2);
        layout.set_text("ab\ncd\nef");
        layout.do_layout();
        assert_eq!(layout.line_count(), 3);
        layout.set_preedit_area(2, "X\nY");
        layout.do_layout();
        assert_eq!(layout.line_count(), 4);
        assert_eq!(layout.line_at(1).map(|l| l.text_length), Some(2), "second line is \"Y\\n\"");
    }

    #[test]
    fn trailing_newline_adds_an_empty_last_line() {
        let mut layout = TextLayout::new("ab\n", Font::new("Segoe UI", 12.0));
        layout.do_layout();
        assert_eq!(layout.line_count(), 2);
        let last = layout.line_at(1).unwrap();
        assert_eq!((last.text_start(), last.text_length()), (3, 0));
        assert!(last.y() > layout.line_at(0).unwrap().y());
        // The caret after the newline sits on the empty line.
        assert_eq!(layout.line_for_text_position(3).map(|l| l.line_number()), Some(1));
    }

    #[test]
    fn line_for_text_position_end_is_exclusive_except_on_the_last_line() {
        let mut layout = TextLayout::new("ab\ncd", Font::new("Segoe UI", 12.0));
        layout.do_layout();
        assert_eq!(layout.line_count(), 2);
        let line_of = |p| layout.line_for_text_position(p).map(|l| l.line_number());
        assert_eq!(line_of(0), Some(0));
        assert_eq!(line_of(2), Some(0));
        assert_eq!(line_of(3), Some(1), "boundary belongs to the next line");
        assert_eq!(line_of(5), Some(1), "end of text belongs to the last line");
        assert_eq!(line_of(99), Some(1));
    }

    #[test]
    fn line_advances_follow_shaped_widths() {
        let font = Font::new("Segoe UI", 12.0);
        let metrics = FontMetrics::from_font(&font);
        let mut layout = TextLayout::new("Wi", font.clone());
        layout.do_layout();
        let line = layout.line_at(0).unwrap();
        let expected = metrics.horizontal_advance_exact("W", &font)
            + metrics.horizontal_advance_exact("i", &font);
        assert!((line.natural_text_width() - expected).abs() < 1e-3);
    }
}
