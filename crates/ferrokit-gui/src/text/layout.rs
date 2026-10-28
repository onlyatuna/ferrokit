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
use crate::text::glyph_layout::{FontEngine, GlyphLayout, PositionedGlyph};
use crate::text::line::{Edge, TextLine};
use crate::text::position::TextPosition;
use std::sync::Arc;
use tiny_skia::Color;
use unicode_segmentation::UnicodeSegmentation;

/// One paragraph (the text between hard line breaks) as a font shaped it, kept so drawing uses the
/// glyphs and pen positions the advances were folded from.
struct ShapedParagraph {
    /// Grapheme index of the first grapheme of the paragraph.
    first: usize,
    /// Grapheme index one past the last grapheme of the paragraph.
    end: usize,
    /// Byte offset of the paragraph in the composite text; glyph clusters are relative to it.
    base: usize,
    /// The font engines the glyphs' `font_index` refers to.
    engines: Vec<FontEngine>,
    layout: GlyphLayout,
    /// Pen x before every grapheme of the paragraph, then at its end.
    pen: Vec<f32>,
    /// Whether the glyphs come in logical order (left-to-right shaping); right-to-left runs are
    /// returned visually, last character first.
    logical_order: bool,
}

impl ShapedParagraph {
    /// The glyphs whose cluster starts in the byte range `lo..hi` of the paragraph. A glyph for a
    /// ligature the range ends inside belongs to the range it starts in, so a line break inside a
    /// ligature draws it once, on the first line, and the next line leaves it out.
    fn glyphs_in(&self, lo: usize, hi: usize) -> Vec<&PositionedGlyph> {
        if self.logical_order {
            // Clusters are sorted, so the range is one slice (no scan of the whole paragraph for
            // every line).
            let glyphs = &self.layout.glyphs;
            let start = glyphs.partition_point(|g| g.cluster < lo);
            let end = glyphs.partition_point(|g| g.cluster < hi);
            glyphs[start..end.max(start)].iter().collect()
        } else {
            self.glyphs_in_by_scan(lo, hi)
        }
    }

    /// [`glyphs_in`](Self::glyphs_in) by testing every glyph, for glyphs in visual order.
    fn glyphs_in_by_scan(&self, lo: usize, hi: usize) -> Vec<&PositionedGlyph> {
        self.layout
            .glyphs
            .iter()
            .filter(|g| g.cluster >= lo && g.cluster < hi)
            .collect()
    }
}

impl std::fmt::Debug for ShapedParagraph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShapedParagraph")
            .field("graphemes", &(self.first..self.end))
            .field("glyphs", &self.layout.glyphs.len())
            .finish()
    }
}

/// The text plus pre-edit as one string with the byte offset of every grapheme, built once per
/// layout run so `create_line` does not rebuild and re-segment it for every line.
#[derive(Debug)]
struct CompositeText {
    text: String,
    /// Byte offset where each grapheme starts, followed by `text.len()`.
    starts: Vec<usize>,
    /// Shaped advance of each grapheme, shaped a paragraph at a time.
    advances: Vec<f32>,
    /// The paragraphs a font shaped, in text order; the others are drawn grapheme by grapheme.
    paragraphs: Vec<ShapedParagraph>,
}

impl CompositeText {
    fn grapheme_count(&self) -> usize {
        self.starts.len() - 1
    }

    fn grapheme(&self, index: usize) -> &str {
        &self.text[self.starts[index]..self.starts[index + 1]]
    }

    /// The shaped paragraph holding grapheme `index`, if a font shaped it.
    fn paragraph_of(&self, index: usize) -> Option<&ShapedParagraph> {
        let at = self.paragraphs.partition_point(|p| p.end <= index);
        self.paragraphs.get(at).filter(|p| p.first <= index)
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

            let g_width = composite.advances[curr_grapheme];

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
        // Lines come from `create_line`, which builds the composite; without one there is nothing
        // laid out to draw.
        let Some(composite) = self.composite.0.as_deref() else {
            return;
        };

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
            let line_end = line_end.min(composite.grapheme_count());
            let mut g_idx = line_start;
            while g_idx < line_end {
                match composite.paragraph_of(g_idx) {
                    Some(paragraph) => {
                        // A line break inside a ligature draws the ligature once, on the line
                        // holding its first grapheme (see `ShapedParagraph::glyphs_in`).
                        let span_end = line_end.min(paragraph.end);
                        self.draw_shaped_span(
                            painter,
                            pos,
                            line,
                            line_y,
                            composite,
                            paragraph,
                            g_idx..span_end,
                            default_color,
                        );
                        g_idx = span_end;
                    }
                    None => {
                        // No font shaped this paragraph: draw it grapheme by grapheme.
                        let g_str = composite.grapheme(g_idx);
                        if g_str != "\n" {
                            let g_x = pos.x + line.cursor_to_x(g_idx, Edge::Leading);
                            painter.draw_text_colored(
                                PointF::new(g_x, line_y),
                                g_str,
                                &self.font,
                                self.foreground_at(g_idx, default_color),
                            );
                        }
                        g_idx += 1;
                    }
                }
            }
        }
    }

    /// The foreground colour of grapheme `g_idx`: the last format covering it that sets one.
    fn foreground_at(&self, g_idx: usize, default_color: Color) -> Color {
        let mut color = default_color;
        for fmt in &self.formats {
            if g_idx >= fmt.start && g_idx < fmt.start + fmt.length {
                if let Some(fg) = fmt.format.foreground {
                    color = fg;
                }
            }
        }
        color
    }

    /// Draws the graphemes `span` (all inside `paragraph`, on `line`) from the paragraph's shaped
    /// glyphs, one glyph run per foreground colour (see [`span_runs`](Self::span_runs)).
    #[allow(clippy::too_many_arguments)]
    fn draw_shaped_span(
        &self,
        painter: &mut Painter,
        pos: PointF,
        line: &TextLine,
        line_y: f32,
        composite: &CompositeText,
        paragraph: &ShapedParagraph,
        span: std::ops::Range<usize>,
        default_color: Color,
    ) {
        let origin = PointF::new(pos.x + line.position.x, line_y);
        for (color, glyphs) in self.span_runs(composite, paragraph, span, default_color) {
            painter.draw_glyph_run_colored(origin, &glyphs, &paragraph.engines, &self.font, color);
        }
    }

    /// The shaped glyphs of the graphemes `span` (inside `paragraph`), split into runs at
    /// foreground colour changes only. Each glyph sits where shaping put it relative to the
    /// span's first grapheme, which is where `cursor_to_x` puts that grapheme, so a ligature is
    /// one glyph at the start of its graphemes and joined scripts keep their joined forms. A glyph
    /// takes the colour of the grapheme its cluster starts in; a ligature the span ends inside is
    /// drawn with the span it starts in.
    fn span_runs(
        &self,
        composite: &CompositeText,
        paragraph: &ShapedParagraph,
        span: std::ops::Range<usize>,
        default_color: Color,
    ) -> Vec<(Color, Vec<PositionedGlyph>)> {
        let byte_lo = composite.starts[span.start] - paragraph.base;
        let byte_hi = composite.starts[span.end] - paragraph.base;
        let glyphs = paragraph.glyphs_in(byte_lo, byte_hi);
        if glyphs.is_empty() {
            return Vec::new();
        }
        // Left-to-right text: the pen position of the span's first grapheme. Right-to-left glyphs
        // come in visual order and `cursor_to_x` is logical, so there the span starts at its
        // leftmost glyph.
        let origin_x = if paragraph.logical_order {
            paragraph.pen[span.start - paragraph.first]
        } else {
            glyphs.iter().map(|g| g.x).fold(f32::INFINITY, f32::min)
        };

        let color_of = |glyph: &PositionedGlyph| {
            let g_idx = composite.starts[..composite.grapheme_count()]
                .partition_point(|&s| s <= paragraph.base + glyph.cluster)
                .saturating_sub(1);
            self.foreground_at(g_idx, default_color)
        };
        let mut runs: Vec<(Color, Vec<PositionedGlyph>)> = Vec::new();
        for glyph in glyphs {
            let color = color_of(glyph);
            if runs.last().is_none_or(|(run_color, _)| *run_color != color) {
                runs.push((color, Vec::new()));
            }
            if let Some((_, run)) = runs.last_mut() {
                run.push(PositionedGlyph {
                    x: glyph.x - origin_x,
                    ..*glyph
                });
            }
        }
        runs
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
        let (advances, paragraphs) = self.shape_paragraphs(&text, &starts);
        let built = Arc::new(CompositeText {
            text,
            starts,
            advances,
            paragraphs,
        });
        self.composite.0 = Some(Arc::clone(&built));
        built
    }

    /// Shapes `text` one paragraph (the text between hard line breaks) at a time, as
    /// `QTextEngine` shapes items: kerning and ligatures are measured across grapheme
    /// boundaries and a ligature's width is divided over its graphemes. Returns the advance of
    /// every grapheme (the `'\n'` graphemes get none; line breaking never measures them) and the
    /// shaped glyphs of every paragraph a font shapes, which drawing uses so what is drawn sits
    /// where the advances put it.
    fn shape_paragraphs(&self, text: &str, starts: &[usize]) -> (Vec<f32>, Vec<ShapedParagraph>) {
        let metrics = FontMetrics::from_font(&self.font);
        let count = starts.len() - 1;
        let mut advances = vec![0.0f32; count];
        let mut paragraphs = Vec::new();
        let mut first = 0;
        while first < count {
            let mut end = first;
            while end < count && &text[starts[end]..starts[end + 1]] != "\n" {
                end += 1;
            }
            if end > first {
                let base = starts[first];
                let local: Vec<usize> = starts[first..=end].iter().map(|s| s - base).collect();
                let paragraph = &text[base..starts[end]];
                match FontMetrics::shape_paragraph(paragraph, &self.font) {
                    Some((engines, layout)) => {
                        let shaped = layout.grapheme_advances(&local);
                        advances[first..end].copy_from_slice(&shaped);
                        let mut pen = Vec::with_capacity(shaped.len() + 1);
                        let mut x = 0.0f32;
                        pen.push(x);
                        for advance in &shaped {
                            x += advance;
                            pen.push(x);
                        }
                        let logical_order = layout
                            .glyphs
                            .windows(2)
                            .all(|w| w[0].cluster <= w[1].cluster);
                        paragraphs.push(ShapedParagraph {
                            first,
                            end,
                            base,
                            engines,
                            layout,
                            pen,
                            logical_order,
                        });
                    }
                    None => {
                        let heuristic =
                            metrics.heuristic_grapheme_advances(paragraph, &local, &self.font);
                        advances[first..end].copy_from_slice(&heuristic);
                    }
                }
            }
            first = end + 1;
        }
        (advances, paragraphs)
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
    use crate::text::line::CursorPosition;
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
        // The pair is shaped as one string, so whatever the font kerns between `W` and `i` is in.
        let expected = metrics.horizontal_advance_exact("Wi", &font);
        assert!((line.natural_text_width() - expected).abs() < 1e-3);
    }

    /// A font and a letter pair its shaping kerns: the pair is measurably narrower shaped than
    /// as two separate letters. `None` on a machine with none of the usual fonts.
    fn kerned_pair() -> Option<(Font, &'static str)> {
        let families = [
            "Segoe UI",
            "Arial",
            "Calibri",
            "Microsoft JhengHei UI",
            "DejaVu Sans",
        ];
        for family in families {
            let font = Font::new(family, 16.0);
            let metrics = FontMetrics::from_font(&font);
            for pair in ["AV", "To", "Wa", "AT", "Ty", "LT"] {
                let whole = metrics.horizontal_advance_exact(pair, &font);
                let apart: f32 = pair
                    .chars()
                    .map(|c| metrics.horizontal_advance_exact(&c.to_string(), &font))
                    .sum();
                if (whole - apart).abs() > 0.25 {
                    return Some((font, pair));
                }
            }
        }
        None
    }

    #[test]
    fn grapheme_advances_are_shaped_not_summed_per_grapheme() {
        let Some((font, pair)) = kerned_pair() else {
            eprintln!("no kerning font installed; skipping");
            return;
        };
        let metrics = FontMetrics::from_font(&font);
        let advances = metrics.grapheme_advances(pair, &font);
        let whole = metrics.horizontal_advance_exact(pair, &font);
        assert_eq!(advances.len(), 2);
        assert!((advances.iter().sum::<f32>() - whole).abs() < 1e-3);
        let first_alone = metrics.horizontal_advance_exact(&pair[..1], &font);
        assert!(
            (advances[0] - first_alone).abs() > 0.25,
            "the kern lives on the first letter: {advances:?} vs alone {first_alone}"
        );
    }

    #[test]
    fn wrapping_measures_kerned_runs_as_shaped() {
        let Some((font, pair)) = kerned_pair() else {
            eprintln!("no kerning font installed; skipping");
            return;
        };
        let metrics = FontMetrics::from_font(&font);
        let text = pair.repeat(8); // no break opportunity: wrapping falls back to a grapheme cut
        let advances = metrics.grapheme_advances(&text, &font);
        let shaped: f32 = advances[..8].iter().sum();
        let apart: f32 = text
            .chars()
            .take(8)
            .map(|c| metrics.horizontal_advance_exact(&c.to_string(), &font))
            .sum();
        assert!(
            apart > shaped + 0.5,
            "precondition: kerning narrows the run ({apart} vs {shaped})"
        );

        let mut layout = TextLayout::new(text.clone(), font);
        layout.set_wrap_width(Some(shaped + 0.001));
        layout.do_layout();
        let first = layout.line_at(0).unwrap();
        assert_eq!(
            first.text_length(),
            8,
            "eight kerned graphemes fit; summing letters cuts earlier"
        );

        // Lines tile the text on grapheme boundaries.
        let mut next = 0;
        for i in 0..layout.line_count() {
            let line = layout.line_at(i).unwrap();
            assert_eq!(line.text_start(), next);
            assert_eq!(
                line.byte_length(),
                line.text_length(),
                "ASCII: one byte per grapheme"
            );
            next += line.text_length();
        }
        assert_eq!(next, text.len());
    }

    #[test]
    fn cursor_positions_follow_shaped_grapheme_boundaries() {
        let Some((font, pair)) = kerned_pair() else {
            eprintln!("no kerning font installed; skipping");
            return;
        };
        let metrics = FontMetrics::from_font(&font);
        let text = pair.repeat(3);
        let advances = metrics.grapheme_advances(&text, &font);
        let mut layout = TextLayout::new(text.clone(), font.clone());
        layout.do_layout();
        assert_eq!(layout.line_count(), 1);
        let line = layout.line_at(0).unwrap();

        let mut x = 0.0f32;
        for (i, advance) in advances.iter().enumerate() {
            assert!(
                (line.cursor_to_x(i, Edge::Leading) - x).abs() < 1e-3,
                "boundary {i}"
            );
            assert_eq!(
                line.x_to_cursor(x, CursorPosition::CursorBetweenCharacters),
                i,
                "boundary {i} maps back to its grapheme"
            );
            x += advance;
        }
        assert!((line.cursor_to_x(advances.len(), Edge::Leading) - x).abs() < 1e-3);
        let first_alone = metrics.horizontal_advance_exact(&pair[..1], &font);
        assert!((line.cursor_to_x(1, Edge::Leading) - first_alone).abs() > 0.25);
    }

    #[test]
    fn cursor_positions_of_a_combining_sequence_stay_on_grapheme_boundaries() {
        // "e" + U+0301 is one grapheme: three graphemes for the four chars of "e\u{301}xy".
        let text = "e\u{301}xy";
        let mut layout = TextLayout::new(text, Font::new("Segoe UI", 12.0));
        layout.do_layout();
        let line = layout.line_at(0).unwrap();
        assert_eq!(line.text_length(), 3);
        assert_eq!(line.byte_length(), text.len());
        let xs: Vec<f32> = (0..=3)
            .map(|i| line.cursor_to_x(i, Edge::Leading))
            .collect();
        assert_eq!(xs[0], 0.0);
        assert!(xs.windows(2).all(|w| w[1] > w[0]), "{xs:?}");
        assert!((xs[3] - line.natural_text_width()).abs() < 1e-3);
    }

    // ---- drawn glyph runs ------------------------------------------------------------------

    /// A glyph of `cluster` (a byte offset) at pen `x` advancing `advance`.
    fn glyph(cluster: usize, x: f32, advance: f32) -> PositionedGlyph {
        PositionedGlyph {
            glyph_id: 1,
            font_index: 0,
            x,
            y: 0.0,
            cluster,
            advance,
        }
    }

    /// `text` as one shaped paragraph holding `glyphs`, with the per-grapheme advances folded from
    /// them as layout does; no font is involved.
    fn synthetic(text: &str, glyphs: Vec<PositionedGlyph>) -> (TextLayout, CompositeText) {
        let layout = TextLayout::new(text, Font::new("Arial", 16.0));
        let mut starts: Vec<usize> = text.grapheme_indices(true).map(|(i, _)| i).collect();
        starts.push(text.len());
        let width = glyphs.iter().map(|g| g.advance).sum();
        let glyph_layout = GlyphLayout { glyphs, width };
        let advances = glyph_layout.grapheme_advances(&starts);
        let mut pen = vec![0.0f32];
        for a in &advances {
            pen.push(pen.last().unwrap() + a);
        }
        let logical_order = glyph_layout
            .glyphs
            .windows(2)
            .all(|w| w[0].cluster <= w[1].cluster);
        let paragraph = ShapedParagraph {
            first: 0,
            end: advances.len(),
            base: 0,
            engines: Vec::new(),
            layout: glyph_layout,
            pen,
            logical_order,
        };
        let composite = CompositeText {
            text: text.to_owned(),
            starts,
            advances,
            paragraphs: vec![paragraph],
        };
        (layout, composite)
    }

    fn xs(run: &(Color, Vec<PositionedGlyph>)) -> Vec<f32> {
        run.1.iter().map(|g| g.x).collect()
    }

    fn red() -> Color {
        Color::from_rgba8(255, 0, 0, 255)
    }

    fn red_from(start: usize, length: usize) -> FormatRange {
        use crate::text::format::TextCharFormat;
        FormatRange::new(
            start,
            length,
            TextCharFormat::default().with_foreground(red()),
        )
    }

    #[test]
    fn a_ligature_is_drawn_as_one_glyph_at_its_first_grapheme() {
        // "ffi!": a single `ffi` glyph covers three graphemes.
        let (layout, composite) = synthetic("ffi!", vec![glyph(0, 0.0, 9.0), glyph(3, 9.0, 2.0)]);
        let paragraph = &composite.paragraphs[0];
        let runs = layout.span_runs(&composite, paragraph, 0..4, Color::BLACK);
        assert_eq!(runs.len(), 1);
        assert_eq!(
            xs(&runs[0]),
            vec![0.0, 9.0],
            "two glyphs for four graphemes"
        );
        assert_eq!(composite.advances, vec![3.0, 3.0, 3.0, 2.0]);

        // A line starting on the last grapheme measures from there.
        let tail = layout.span_runs(&composite, paragraph, 3..4, Color::BLACK);
        assert_eq!(xs(&tail[0]), vec![0.0]);
        // A line that ends inside the ligature draws it with the graphemes it starts in; the
        // next line, starting inside it, does not draw it again.
        let head = layout.span_runs(&composite, paragraph, 0..2, Color::BLACK);
        assert_eq!(xs(&head[0]), vec![0.0]);
        let inside = layout.span_runs(&composite, paragraph, 2..4, Color::BLACK);
        assert_eq!(inside.len(), 1);
        assert!(inside[0].1.iter().all(|g| g.cluster == 3));
    }

    #[test]
    fn drawn_positions_follow_the_shaped_pen_not_the_grapheme_widths() {
        // "AVA": the pair kern shortens `A` to 7 where `A` alone is 8.
        let (layout, composite) = synthetic(
            "AVA",
            vec![glyph(0, 0.0, 7.0), glyph(1, 7.0, 8.0), glyph(2, 15.0, 8.0)],
        );
        let paragraph = &composite.paragraphs[0];
        let runs = layout.span_runs(&composite, paragraph, 0..3, Color::BLACK);
        assert_eq!(xs(&runs[0]), vec![0.0, 7.0, 15.0]);
        assert_eq!(xs(&runs[0]), paragraph.pen[..3].to_vec());
    }

    #[test]
    fn runs_split_at_format_boundaries_and_keep_their_positions() {
        let (mut layout, composite) = synthetic(
            "AVAV",
            vec![
                glyph(0, 0.0, 7.0),
                glyph(1, 7.0, 8.0),
                glyph(2, 15.0, 7.0),
                glyph(3, 22.0, 8.0),
            ],
        );
        layout.set_formats(vec![red_from(1, 2)]);
        let runs = layout.span_runs(&composite, &composite.paragraphs[0], 0..4, Color::BLACK);
        assert_eq!(runs.len(), 3);
        assert_eq!(runs[0].0, Color::BLACK);
        assert_eq!(runs[1].0, red());
        assert_eq!(runs[2].0, Color::BLACK);
        let all: Vec<f32> = runs.iter().flat_map(xs).collect();
        assert_eq!(
            all,
            vec![0.0, 7.0, 15.0, 22.0],
            "splitting does not move a glyph"
        );
    }

    #[test]
    fn a_ligature_takes_the_colour_of_the_grapheme_it_starts_in() {
        let (mut layout, composite) =
            synthetic("ffi!", vec![glyph(0, 0.0, 9.0), glyph(3, 9.0, 2.0)]);
        layout.set_formats(vec![red_from(1, 3)]);
        let runs = layout.span_runs(&composite, &composite.paragraphs[0], 0..4, Color::BLACK);
        assert_eq!(runs.len(), 2);
        assert_eq!(
            runs[0].0,
            Color::BLACK,
            "the ligature starts in an unformatted grapheme"
        );
        assert_eq!(xs(&runs[0]), vec![0.0]);
        assert_eq!(runs[1].0, red());
        assert_eq!(xs(&runs[1]), vec![9.0]);
    }

    #[test]
    fn a_mark_is_drawn_with_its_base_grapheme() {
        // "e" + U+0301 + "x": the mark shares the base's cluster, offset a little to the left.
        let (layout, composite) = synthetic(
            "e\u{301}x",
            vec![glyph(0, 0.0, 5.0), glyph(0, -1.0, 0.0), glyph(3, 5.0, 4.0)],
        );
        let runs = layout.span_runs(&composite, &composite.paragraphs[0], 0..2, Color::BLACK);
        assert_eq!(runs.len(), 1);
        assert_eq!(xs(&runs[0]), vec![0.0, -1.0, 5.0]);
    }

    #[test]
    fn right_to_left_glyphs_are_positioned_from_the_leftmost_glyph_of_the_line() {
        // Glyphs come visually: the last character first.
        let (layout, composite) = synthetic(
            "\u{5d0}\u{5d1}\u{5d2}",
            vec![glyph(4, 0.0, 3.0), glyph(2, 3.0, 2.0), glyph(0, 5.0, 1.0)],
        );
        let paragraph = &composite.paragraphs[0];
        assert!(!paragraph.logical_order);
        let runs = layout.span_runs(&composite, paragraph, 0..3, Color::BLACK);
        assert_eq!(xs(&runs[0]), vec![0.0, 3.0, 5.0]);
        // The first two letters sit at the right of the paragraph; their line starts at the
        // leftmost of them.
        let first_two = layout.span_runs(&composite, paragraph, 0..2, Color::BLACK);
        assert_eq!(xs(&first_two[0]), vec![0.0, 2.0]);
    }

    #[test]
    fn glyph_slices_match_scanning_every_glyph_on_a_many_line_paragraph() {
        // 300 graphemes, every third pair shaped as one two-grapheme glyph, so lines of any width
        // start and end on and inside glyphs.
        let mut text = String::new();
        let mut glyphs = Vec::new();
        let mut pen = 0.0f32;
        let mut byte = 0;
        for i in 0..150 {
            text.push_str("ab");
            if i % 3 == 0 {
                glyphs.push(glyph(byte, pen, 7.0));
                pen += 7.0;
            } else {
                glyphs.push(glyph(byte, pen, 4.0));
                glyphs.push(glyph(byte + 1, pen + 4.0, 5.0));
                pen += 9.0;
            }
            byte += 2;
        }
        let (layout, composite) = synthetic(&text, glyphs);
        let paragraph = &composite.paragraphs[0];
        assert!(paragraph.logical_order);
        let count = composite.grapheme_count();
        let mut lines = 0;
        for width in [1, 2, 7, 13, 64] {
            let mut start = 0;
            while start < count {
                let end = (start + width).min(count);
                let (lo, hi) = (composite.starts[start], composite.starts[end]);
                assert_eq!(
                    paragraph.glyphs_in(lo, hi),
                    paragraph.glyphs_in_by_scan(lo, hi),
                    "graphemes {start}..{end}"
                );
                let runs = layout.span_runs(&composite, paragraph, start..end, Color::BLACK);
                let drawn: usize = runs.iter().map(|r| r.1.len()).sum();
                assert_eq!(drawn, paragraph.glyphs_in_by_scan(lo, hi).len());
                start = end;
                lines += 1;
            }
        }
        assert!(lines > 500);
    }

    /// Arial from the font database with its data attached, so shaping does not depend on how
    /// the family name resolves.
    fn arial(size: f32) -> Font {
        let raw = crate::text::font_database::with_global_font_database(|db| {
            db.get_raw_font_data("Arial")
        })
        .expect("Arial is installed");
        Font::new("Arial", size).with_font_data(raw)
    }

    #[test]
    fn shaped_glyphs_of_a_kerned_pair_sit_at_the_cursor_positions() {
        let font = arial(16.0);
        let mut layout = TextLayout::new("AVAVAV", font.clone());
        layout.do_layout();
        let line = layout.line_at(0).unwrap();
        let composite = layout.composite.0.as_deref().unwrap();
        let paragraph = composite
            .paragraph_of(0)
            .expect("Arial shapes the paragraph");
        assert_eq!(paragraph.layout.glyphs.len(), 6, "one glyph per grapheme");

        let runs = layout.span_runs(composite, paragraph, 0..6, Color::BLACK);
        assert_eq!(runs.len(), 1);
        for (i, glyph) in runs[0].1.iter().enumerate() {
            let cursor = line.cursor_to_x(i, Edge::Leading) - line.position.x;
            assert!(
                (glyph.x - cursor).abs() < 1e-3,
                "glyph {i}: {} vs {cursor}",
                glyph.x
            );
        }
        // The pair kerns: drawing at per-grapheme widths would put the second glyph elsewhere.
        // Only Windows' Arial kerns this pair; the metric-compatible fonts other platforms install
        // for the name "Arial" may not.
        #[cfg(windows)]
        {
            let metrics = FontMetrics::from_font(&font);
            let a_alone = metrics.horizontal_advance_exact("A", &font);
            assert!(
                (runs[0].1[1].x - a_alone).abs() > 0.25,
                "AV is kerned: {}",
                runs[0].1[1].x
            );
        }
    }

    #[test]
    fn drawing_a_layout_matches_drawing_its_text_as_one_shaped_string() {
        use crate::paint::pixmap::Pixmap;
        let font = arial(16.0);
        let text = "AVAV To Wa fi";
        let mut layout = TextLayout::new(text, font.clone());
        layout.do_layout();
        let line = layout.line_at(0).unwrap();
        let baseline = PointF::new(4.0, 2.0 + line.position.y + line.ascent);

        let mut by_layout = Pixmap::new(200, 40).unwrap();
        {
            let mut painter = Painter::begin(&mut by_layout);
            layout.draw(&mut painter, PointF::new(4.0, 2.0), &[]);
        }
        let mut by_text = Pixmap::new(200, 40).unwrap();
        {
            let mut painter = Painter::begin(&mut by_text);
            painter.draw_text_colored(baseline, text, &font, Color::BLACK);
        }
        assert!(
            by_text.data().iter().any(|&b| b != 0),
            "the reference draws something"
        );
        assert_eq!(by_layout.data(), by_text.data());
    }

    #[test]
    fn a_format_boundary_does_not_move_drawn_glyphs() {
        use crate::paint::pixmap::Pixmap;
        let font = arial(16.0);
        let text = "AVAVAV";
        let mut plain = TextLayout::new(text, font.clone());
        plain.do_layout();
        let mut formatted = plain.clone();
        formatted.set_formats(vec![red_from(2, 2)]);

        let render = |layout: &TextLayout| {
            let mut pm = Pixmap::new(120, 30).unwrap();
            {
                let mut painter = Painter::begin(&mut pm);
                layout.draw(&mut painter, PointF::new(2.0, 2.0), &[]);
            }
            pm
        };
        let (a, b) = (render(&plain), render(&formatted));
        // Same glyph coverage everywhere: only the colour of the middle changed.
        let covered =
            |pm: &Pixmap| -> Vec<bool> { pm.data().chunks(4).map(|p| p[3] != 0).collect() };
        assert_eq!(covered(&a), covered(&b));
        assert_ne!(a.data(), b.data(), "the formatted run is red");
    }
}
