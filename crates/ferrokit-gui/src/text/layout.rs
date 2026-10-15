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
use tiny_skia::Color;
use unicode_segmentation::UnicodeSegmentation;

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
    }

    /// Begins line-by-line layout.
    pub fn begin_layout(&mut self) {
        self.lines.clear();
        self.layout_cursor = 0;
    }

    /// Produces the next `TextLine` from the layout, wrapping words if necessary.
    pub fn create_line(&mut self) -> Option<TextLine> {
        let full_text = self.composite_text();
        let total_graphemes = full_text.graphemes(true).count();

        if self.layout_cursor >= total_graphemes && !full_text.is_empty() {
            return None;
        }

        if full_text.is_empty() {
            if self.layout_cursor > 0 {
                return None;
            }
            self.layout_cursor = 1;
            let metrics = FontMetrics::from_font(&self.font);
            return Some(TextLine {
                index: self.lines.len(),
                text_start: 0,
                text_length: 0,
                byte_start: 0,
                byte_length: 0,
                position: PointF::new(self.position.x, self.position.y),
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

        let grapheme_list: Vec<&str> = full_text.graphemes(true).collect();
        let line_start_grapheme = self.layout_cursor;

        let mut curr_grapheme = line_start_grapheme;
        let mut curr_x = 0.0f32;
        let mut advances = vec![0.0f32];
        let mut last_break_grapheme = None;
        let mut last_break_advances_len = 0;

        let mut byte_start = 0;
        for g in &grapheme_list[..line_start_grapheme] {
            byte_start += g.len();
        }

        let mut line_bytes = 0;

        while curr_grapheme < total_graphemes {
            let g = grapheme_list[curr_grapheme];
            if g == "\n" {
                // Hard line break
                curr_grapheme += 1;
                line_bytes += g.len();
                break;
            }

            let g_width = metrics.horizontal_advance(g, &self.font);

            if curr_x + g_width > max_width && curr_grapheme > line_start_grapheme {
                // Wrapping needed
                if let Some(break_pos) = last_break_grapheme {
                    if break_pos > line_start_grapheme {
                        curr_grapheme = break_pos;
                        advances.truncate(last_break_advances_len);
                        // Recompute byte count up to break_pos
                        line_bytes = grapheme_list[line_start_grapheme..curr_grapheme]
                            .iter()
                            .map(|s| s.len())
                            .sum();
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
        for line in &self.lines {
            if pos >= line.text_start && pos <= line.text_start + line.text_length {
                return Some(line);
            }
        }
        self.lines.last()
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

    /// Produces combined text including preedit IME text.
    fn composite_text(&self) -> String {
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
