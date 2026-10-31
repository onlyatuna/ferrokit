// SPDX-License-Identifier: LGPL-3.0-only
//! Multi-line text flow, line breaking, and layout engine (`QTextLayout` equivalent).
//!
//! Coordinates text shaping, word wrapping, IME pre-edit composition display,
//! selection highlighting, and drawing to `Painter`.

use crate::alignment::Alignment;
use crate::geometry::primitives::{PointF, RectF};
use crate::paint::painter::Painter;
use crate::text::bidi;
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

/// `QChar::LineSeparator`: what ends a line of a `QTextLayout` (`QTextEngine::itemize` marks it
/// `LineOrParagraphSeparator`). A line feed does not: it is an ordinary character of no width,
/// which is why `QTextDocument` splits its paragraphs into blocks and `qt_format_text` replaces
/// the line feeds of a label's text with this before laying out.
const LINE_SEPARATOR: &str = "\u{2028}";

/// The tab stop distance of a `QTextOption` that sets none, in pixels.
pub const DEFAULT_TAB_STOP_DISTANCE: f32 = 80.0;

/// The width of a tab at `x` (`QTextEngine::calculateTabWidth`): up to the next multiple of the
/// distance, which Qt keeps in 1/64 px. `x / distance` is a 26.6 fixed point quotient that rounds
/// to nearest, so an `x` less than `distance / 128` below a stop counts as on it, and a tab on a
/// stop goes to the next one (measured against `QTextLayout`).
fn tab_width(x: f32, distance: f32) -> f32 {
    let distance = if distance.is_finite() && distance > 0.0 {
        f64::from(distance)
    } else {
        f64::from(DEFAULT_TAB_STOP_DISTANCE)
    };
    // A distance below 1/64 px is no distance at all in Qt's fixed point; keep the tab positive.
    let distance = ((distance * 64.0).trunc() / 64.0).max(1.0 / 64.0);
    let x = f64::from(x);
    let stops = ((x / distance * 64.0 + 0.5).floor() / 64.0).floor();
    ((stops + 1.0) * distance - x) as f32
}

/// Whether a grapheme is left out of shaping and drawing and has no advance: a line feed, a
/// carriage return (alone or as the `"\r\n"` cluster; Qt measures `\r` as a character of no width
/// too), the line separator or the paragraph separator U+2029 (`QTextEngine::itemize` flags both
/// separators `LineOrParagraphSeparator` and gives them no width; unlike U+2028, U+2029 does not
/// end a line of a `QTextLayout`, it is only a break opportunity).
fn is_unshaped(grapheme: &str) -> bool {
    matches!(grapheme, "\n" | "\r" | "\r\n" | "\u{2029}") || grapheme == LINE_SEPARATOR
}

/// Whether a grapheme is white space that hangs past the end of a wrapped line instead of
/// wrapping it: what `QTextEngine` flags `ScriptAnalysis::Space` (a space, a tab, the other
/// separator spaces and the zero width space; not the no-break spaces, which are ordinary
/// characters, and not the line and paragraph separators).
fn is_hanging_space(grapheme: &str) -> bool {
    // Line feeds and carriage returns hang like spaces in Qt (`a \r` at a width that fits `a ` is
    // one line).
    matches!(grapheme, "\n" | "\r" | "\r\n")
        || grapheme.chars().all(|c| {
            c == '\u{200b}'
                || (c.is_whitespace()
                    && !matches!(
                        c,
                        '\n' | '\r'
                            | '\u{b}'
                            | '\u{c}'
                            | '\u{85}'
                            | '\u{a0}'
                            | '\u{2007}'
                            | '\u{2028}'
                            | '\u{2029}'
                            | '\u{202f}'
                    ))
        })
}

/// Whether a grapheme is white space for the right bearing of a line: a hanging space, a line feed
/// or the line separator (the characters Qt's `QCharAttributes::whiteSpace` is set for that this
/// layout tells apart). The glyph before them is the one whose bearing ends the line.
fn is_blank(grapheme: &str) -> bool {
    is_hanging_space(grapheme) || is_unshaped(grapheme)
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
    /// The negative right bearing of the glyph that ends each grapheme (its absolute value, 0 when
    /// the ink stays inside the advance or the font gave none): what
    /// `QTextLine::layout_helper`'s `negativeRightBearing` adds to the width of a line ending
    /// there. A grapheme inside a ligature has the ligature's.
    bearings: Vec<f32>,
    /// `(grapheme, bearing)` for each word whose last grapheme is followed by white space that
    /// starts another run: Qt reads that bearing from the space's run, not the word's (see
    /// [`TextLayout::boundary_bearings`]). Sorted by grapheme.
    space_bearings: Vec<(usize, f32)>,
    /// The paragraphs a font shaped, in text order; the others are drawn grapheme by grapheme.
    paragraphs: Vec<ShapedParagraph>,
    /// Whether UAX #14 allows a line break in front of each grapheme, plus one entry for the end
    /// of the text (`QCharAttributes::lineBreak`).
    break_before: Vec<bool>,
    /// The bidi embedding level of every grapheme; empty when the text is left to right all
    /// through.
    levels: Vec<u8>,
    /// Whether the text as a whole reads right to left (`QTextEngine::isRightToLeft`).
    base_rtl: bool,
}

impl CompositeText {
    fn grapheme_count(&self) -> usize {
        self.starts.len() - 1
    }

    fn grapheme(&self, index: usize) -> &str {
        &self.text[self.starts[index]..self.starts[index + 1]]
    }

    /// Whether a word ends in grapheme `g`, for the line breaking of `layout_helper`: the next
    /// grapheme is white space, a place where UAX #14 allows a break, or there is none.
    fn ends_chunk(&self, g: usize) -> bool {
        g + 1 >= self.grapheme_count() || self.break_before[g + 1] || is_blank(self.grapheme(g + 1))
    }

    /// The bearing `layout_helper` ends a line with when its last word ends in grapheme `g`:
    /// that of the glyph before the white space after the word (read from the run of the space,
    /// see [`TextLayout::boundary_bearings`]), or of the word's own last glyph.
    fn end_bearing(&self, g: usize) -> f32 {
        let spaced = g + 1 < self.grapheme_count() && is_blank(self.grapheme(g + 1));
        self.space_bearings
            .binary_search_by_key(&g, |entry| entry.0)
            .ok()
            .filter(|_| spaced)
            .map_or(self.bearings[g], |at| self.space_bearings[at].1)
    }

    /// Whether the word of a line that starts at grapheme `from` (`x` the pen after what is
    /// before it) is wider than `width` once the line is as `layout_helper` ends it. The word is
    /// taken in whole, and the line then ends with the bearing of its last glyph, except for a
    /// text whose rest is white space only: nothing follows the word to test or break at, and a
    /// line that ends in white space has no bearing.
    ///
    /// The bearing is never negative, so a word that is too wide by its advances alone is too wide
    /// whatever ends it: the scan stops there instead of at the end of the word, and a line costs
    /// no more than its own width however long the word is.
    fn first_word_overflows(&self, from: usize, x: f32, width: f32) -> bool {
        let mut pen = x;
        let mut end = from;
        loop {
            #[cfg(test)]
            tests::FIRST_WORD_SCANNED.with(|c| c.set(c.get() + 1));
            pen += self.advances[end];
            if pen > width {
                return true;
            }
            if self.ends_chunk(end) {
                break;
            }
            end += 1;
        }
        let count = self.grapheme_count();
        let rest_is_space = end + 1 < count
            && (end + 1..count).all(|g| {
                let grapheme = self.grapheme(g);
                is_blank(grapheme) && grapheme != LINE_SEPARATOR
            });
        let bearing = if rest_is_space {
            0.0
        } else {
            self.end_bearing(end)
        };
        pen + bearing > width
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

/// How a line that is wider than the wrap width is broken (`QTextOption::WrapMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WrapMode {
    /// Lines are never wrapped; the wrap width does not limit them.
    NoWrap,
    /// Lines wrap at word boundaries only. A word wider than the width is not broken: it
    /// overflows. This is what `QTextOption` and so `QTextLayout` start with, and what `QLabel`
    /// sets for its word wrap.
    #[default]
    WordWrap,
    /// Lines wrap at any character, with no regard for word boundaries.
    WrapAnywhere,
    /// Lines wrap at word boundaries, and inside a word that does not fit a line of its own.
    /// This is what `QTextDocument` starts with (and so `QTextEdit` and `QPlainTextEdit`).
    WrapAtWordBoundaryOrAnywhere,
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
    /// How a line wider than `wrap_width` is broken.
    wrap_mode: WrapMode,
    /// Horizontal paragraph alignment.
    alignment: Alignment,
    /// `QTextOption::tabStopDistance`: a tab advances to the next multiple of it; zero or less
    /// means [`DEFAULT_TAB_STOP_DISTANCE`].
    tab_stop_distance: f32,
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
            wrap_mode: WrapMode::default(),
            alignment: Alignment::LEFT,
            tab_stop_distance: DEFAULT_TAB_STOP_DISTANCE,
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

    /// Returns the wrap mode.
    #[inline]
    pub fn wrap_mode(&self) -> WrapMode {
        self.wrap_mode
    }

    /// Sets the wrap mode (`QTextOption::setWrapMode`); `WordWrap` until set.
    pub fn set_wrap_mode(&mut self, mode: WrapMode) {
        self.wrap_mode = mode;
        self.clear_layout();
    }

    /// The distance between tab stops (`QTextOption::tabStopDistance`), 80 px until set.
    #[inline]
    pub fn tab_stop_distance(&self) -> f32 {
        self.tab_stop_distance
    }

    /// Sets the distance between tab stops (`QTextOption::setTabStopDistance`): a tab advances to
    /// the next multiple of it from the start of the line. Zero, a negative or a non-finite
    /// distance means the default of 80 px, as in Qt.
    pub fn set_tab_stop_distance(&mut self, distance: f32) {
        self.tab_stop_distance = distance;
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

        // A text ending in a hard break has a final empty line (the caret after the separator); an
        // empty text is a single empty line. `layout_cursor == total + 1` means that line is done.
        let ends_with_break =
            total_graphemes > 0 && composite.grapheme(total_graphemes - 1) == LINE_SEPARATOR;
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
                advance_width: 0.0,
                right_limit: self.position.x + self.wrap_width.unwrap_or(f32::INFINITY),
                runs: Vec::new(),
                base_rtl: composite.base_rtl,
                is_last: true,
                grapheme_x_advances: vec![0.0],
            });
        }

        let metrics = FontMetrics::from_font(&self.font);
        let wraps = self.wrap_mode != WrapMode::NoWrap;
        let max_width = if wraps {
            self.wrap_width.unwrap_or(f32::MAX)
        } else {
            f32::MAX
        };
        let right_limit = if wraps {
            self.position.x + self.wrap_width.unwrap_or(f32::INFINITY)
        } else {
            f32::INFINITY
        };
        let breaks_at_words = matches!(
            self.wrap_mode,
            WrapMode::WordWrap | WrapMode::WrapAtWordBoundaryOrAnywhere
        );
        let breaks_anywhere = matches!(
            self.wrap_mode,
            WrapMode::WrapAnywhere | WrapMode::WrapAtWordBoundaryOrAnywhere
        );

        // `layout_cursor < total_graphemes` here, so `starts` has an entry for it.
        let line_start_grapheme = self.layout_cursor;

        let mut curr_grapheme = line_start_grapheme;
        let mut curr_x = 0.0f32;
        let mut advances = vec![0.0f32];
        let mut last_break_grapheme = None;
        let mut last_break_advances_len = 0;
        // How many graphemes of the line (and of the line up to the last break) stay in its natural
        // width when its trailing white space is left out: up to the last grapheme that is no
        // hanging space, or a tab that ends within the width.
        let mut solid = 0;
        let mut last_break_solid = 0;
        // Set when the line ended in a tab that does not fit: nothing else is added to it.
        let mut closed = false;
        // Set when the line ends because the next word does not fit (not at the end of the text
        // or at a hard break): Qt leaves the spaces at the end of such a line out of its width.
        let mut wrapped = false;
        // Whether the line has text other than spaces yet: spaces in front of the first word
        // are part of it, not a place to break, and a line of spaces only keeps them all.
        let mut has_word = false;
        // Set when the line ends at a line separator (which belongs to the line).
        let mut ended_by_separator = false;

        // Whether the first word of the line is laid out again inside the word, see below.
        let mut first_word_relaid: Option<bool> = None;

        let byte_start = composite.starts[line_start_grapheme];

        let mut line_bytes = 0;

        while curr_grapheme < total_graphemes {
            let g = composite.grapheme(curr_grapheme);
            if closed {
                wrapped = true;
                break;
            }
            if g == LINE_SEPARATOR {
                // Hard line break
                ended_by_separator = true;
                curr_grapheme += 1;
                line_bytes += g.len();
                break;
            }

            let g_width = if g == "\t" {
                tab_width(curr_x, self.tab_stop_distance)
            } else {
                composite.advances[curr_grapheme]
            };
            let is_tab = g == "\t";
            let is_space = is_hanging_space(g);

            // `QTextLine::layout_helper` adds the negative right bearing of the glyph that would
            // end the line to the width it tests (`calculateNewWidth`): at the end of a word, and
            // at every grapheme once it breaks inside words. That is `WrapAnywhere`, and the first
            // word of a line of `WrapAtWordBoundaryOrAnywhere` that is too wide for it, which is
            // laid out again that way; whether it is too wide is decided by the width of the line
            // as it ends, with or without the bearing (`first_word_overflows`).
            let tests_bearing = !is_space
                && match self.wrap_mode {
                    WrapMode::WrapAnywhere => true,
                    WrapMode::WrapAtWordBoundaryOrAnywhere if last_break_grapheme.is_none() => {
                        *first_word_relaid.get_or_insert_with(|| {
                            composite.first_word_overflows(curr_grapheme, curr_x, max_width)
                        })
                    }
                    _ => composite.ends_chunk(curr_grapheme),
                };
            let bearing = if tests_bearing {
                composite.bearings[curr_grapheme]
            } else {
                0.0
            };

            // `QTextLine::layout_helper`: a space is added to the line without testing the width
            // (it hangs past the line end when it does not fit); a word is added only if it fits
            // after the spaces before it.
            // A tab is tested once the line has a word, and by where it starts, not where it ends
            // (measured: a tab that begins within the width stays, hanging if it ends beyond it); it
            // wraps at the last break, else in front of itself in every mode.
            if is_tab && has_word && curr_x > max_width {
                match last_break_grapheme {
                    Some(break_pos) if breaks_at_words && break_pos > line_start_grapheme => {
                        wrapped = true;
                        curr_grapheme = break_pos;
                        advances.truncate(last_break_advances_len);
                        solid = last_break_solid;
                        line_bytes = composite.starts[curr_grapheme] - byte_start;
                    }
                    _ => wrapped = true,
                }
                break;
            }
            // (A line always takes its first grapheme, whatever the width.)
            if !is_space
                && curr_grapheme > line_start_grapheme
                && (has_word || curr_x > max_width)
                && curr_x + g_width + bearing > max_width
            {
                // Wrapping needed: at the last place a word ends, if the mode breaks there and
                // there is one on this line.
                match last_break_grapheme {
                    Some(break_pos) if breaks_at_words && break_pos > line_start_grapheme => {
                        wrapped = true;
                        curr_grapheme = break_pos;
                        advances.truncate(last_break_advances_len);
                        solid = last_break_solid;
                        // Recompute byte count up to break_pos
                        line_bytes = composite.starts[curr_grapheme] - byte_start;
                        break;
                    }
                    // Otherwise before this character, if the mode breaks inside words; the others
                    // let the word overflow.
                    // A line of spaces that already hangs past the width wraps in front of its
                    // first word whatever the mode.
                    _ if breaks_anywhere || !has_word => {
                        wrapped = true;
                        break;
                    }
                    _ => {}
                }
            }

            curr_x += g_width;
            advances.push(curr_x);
            line_bytes += g.len();
            has_word |= !is_space || is_tab;
            if is_tab {
                // A tab that ends within the width (or starts the line) is part of the line's
                // width; one that does not hangs, with the spaces in front of it kept, and
                // closes the line.
                if curr_x <= max_width || curr_grapheme == line_start_grapheme {
                    solid = advances.len() - 1;
                } else {
                    solid = solid.max(advances.len() - 2);
                    closed = true;
                }
            } else if !is_space {
                solid = advances.len() - 1;
            }

            // Track word break opportunity (UAX #14)
            if breaks_at_words && has_word && composite.break_before[curr_grapheme + 1] {
                last_break_grapheme = Some(curr_grapheme + 1);
                last_break_advances_len = advances.len();
                last_break_solid = solid;
            }

            curr_grapheme += 1;
        }

        // Graphemes of the line before its separator: the ones `advances` has an end for.
        let text_len = curr_grapheme - line_start_grapheme;
        let content_len = text_len - usize::from(ended_by_separator);
        // The width of the text of a wrapped line stops before its trailing spaces
        // (`QTextLine::naturalTextWidth`), as does that of a line a separator ends; the last line
        // of a paragraph keeps them unless they hang over the width.
        let mut natural_len = content_len;
        if ended_by_separator || wrapped || (has_word && curr_x > max_width) {
            natural_len = natural_len.min(solid);
        }
        let advance_w = advances[natural_len.min(advances.len() - 1)];
        // `layout_helper` ends with the negative right bearing of the last glyph that is not a
        // space: `naturalTextWidth` is the advances plus it (`horizontalAdvance` and the alignment
        // of the line go by the advances alone). The one exception: a line that is the end of the
        // text and ends in white space has no bearing, whether its spaces fit or hang.
        let line_end = line_start_grapheme + content_len;
        let ends_in_blank =
            line_end > line_start_grapheme && is_blank(composite.grapheme(line_end - 1));
        let at_text_end = curr_grapheme >= total_graphemes && !ended_by_separator;
        let bearing = if at_text_end && ends_in_blank {
            0.0
        } else {
            (line_start_grapheme..line_end)
                .rev()
                .find(|&g| !is_blank(composite.grapheme(g)))
                .map_or(0.0, |g| {
                    if g + 1 < line_end {
                        composite.end_bearing(g)
                    } else {
                        composite.bearings[g]
                    }
                })
        };
        let natural_w = advance_w + bearing;
        let line_idx = self.lines.len();

        let y_pos = self.position.y + (line_idx as f32) * metrics.height;
        // `QTextEngine::alignLine` (qtextengine.cpp:3558-3572): Right, else HCenter, else the
        // line starts at the left (Left, Justify, no flag).
        // Justified text of a right to left paragraph is aligned right.
        let align_right = self.alignment.contains(Alignment::RIGHT)
            || (self.alignment.contains(Alignment::JUSTIFY) && composite.base_rtl);
        let x_pos = if align_right {
            let avail = self.wrap_width.unwrap_or(advance_w);
            self.position.x + (avail - advance_w).max(0.0)
        } else if self.alignment.contains(Alignment::H_CENTER) {
            let avail = self.wrap_width.unwrap_or(advance_w);
            self.position.x + (avail - advance_w).max(0.0) * 0.5
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
            advance_width: advance_w,
            right_limit,
            runs: crate::text::line::visual_runs(
                composite
                    .levels
                    .get(line_start_grapheme..curr_grapheme)
                    .unwrap_or(&[]),
                &advances,
            ),
            base_rtl: composite.base_rtl,
            is_last: curr_grapheme >= total_graphemes && !ends_with_break,
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
                    let bg_color = sel
                        .format
                        .background
                        .unwrap_or(Color::from_rgba8(0, 120, 215, 80));
                    for (x, width) in line.selection_spans(overlap_start, overlap_end) {
                        let sel_rect =
                            RectF::new(pos.x + x, pos.y + line.position.y, width, line.height);
                        painter.fill_rect(sel_rect, bg_color);
                    }
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
                        let span = g_idx..span_end;
                        if span.clone().any(|g| composite.grapheme(g) == "\t") {
                            self.draw_span_around_tabs(
                                painter,
                                pos,
                                line,
                                line_y,
                                composite,
                                paragraph,
                                span,
                                default_color,
                            );
                        } else {
                            self.draw_shaped_span(
                                painter,
                                pos,
                                line,
                                line_y,
                                composite,
                                paragraph,
                                span,
                                default_color,
                            );
                        }
                        g_idx = span_end;
                    }
                    None => {
                        // No font shaped this paragraph: draw it grapheme by grapheme.
                        let g_str = composite.grapheme(g_idx);
                        if !is_unshaped(g_str) && g_str != "\t" {
                            // The left edge of the grapheme: its trailing edge when it reads
                            // right to left.
                            let g_x = pos.x
                                + line
                                    .cursor_to_x(g_idx, Edge::Leading)
                                    .min(line.cursor_to_x(g_idx, Edge::Trailing));
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
        // With right to left text on the line the span sits where its bidi run is shown.
        let run_left = line
            .run_left_of(span.start - line.text_start)
            .map_or(0.0, |v| v.left);
        let origin = PointF::new(pos.x + line.position.x + run_left, line_y);
        for (color, glyphs) in self.span_runs(composite, paragraph, span, default_color) {
            painter.draw_glyph_run_colored(origin, &glyphs, &paragraph.engines, &self.font, color);
        }
    }

    /// Draws `span` without its tabs: a tab has no glyph, and what follows it sits where the line
    /// put it (the tab's width comes from the line, not from shaping), so every piece between tabs
    /// is a span of its own, drawn from the left edge of its graphemes on the line
    /// (`cursor_to_x` includes the line position).
    #[allow(clippy::too_many_arguments)]
    fn draw_span_around_tabs(
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
        let mut start = span.start;
        while start < span.end {
            if composite.grapheme(start) == "\t" {
                start += 1;
                continue;
            }
            let mut end = start;
            while end < span.end && composite.grapheme(end) != "\t" {
                end += 1;
            }
            let left = (start..end)
                .flat_map(|g| {
                    [
                        line.cursor_to_x(g, Edge::Leading),
                        line.cursor_to_x(g, Edge::Trailing),
                    ]
                })
                .fold(f32::INFINITY, f32::min);
            let origin = PointF::new(pos.x + left, line_y);
            for (color, glyphs) in self.span_runs(composite, paragraph, start..end, default_color) {
                painter.draw_glyph_run_colored(
                    origin,
                    &glyphs,
                    &paragraph.engines,
                    &self.font,
                    color,
                );
            }
            start = end;
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
        let base_rtl = bidi::is_rtl(&text);
        let (advances, bearings, paragraphs, levels) =
            self.shape_paragraphs(&text, &starts, base_rtl);
        let space_bearings = self.boundary_bearings(&text, &starts, &paragraphs);
        let break_before = crate::text::line_break::break_before(&text, &starts);
        let built = Arc::new(CompositeText {
            text,
            starts,
            advances,
            bearings,
            space_bearings,
            paragraphs,
            break_before,
            levels,
            base_rtl,
        });
        self.composite.0 = Some(Arc::clone(&built));
        built
    }

    /// Shapes `text` one paragraph (the text between line feeds and hard line breaks) at a time, as
    /// `QTextEngine` shapes items: kerning and ligatures are measured across grapheme
    /// boundaries and a ligature's width is divided over its graphemes. A paragraph with right to
    /// left text is cut into runs of one embedding level and each is shaped in its own direction
    /// (Qt's bidi items). Returns the advance of every grapheme (the `'\n'` and U+2028 graphemes
    /// get none), the shaped glyphs of every run a font shapes, which drawing uses so what is drawn
    /// sits where the advances put it, and the embedding level of every grapheme (empty when the
    /// whole text is left to right).
    fn shape_paragraphs(
        &self,
        text: &str,
        starts: &[usize],
        base_rtl: bool,
    ) -> (Vec<f32>, Vec<f32>, Vec<ShapedParagraph>, Vec<u8>) {
        let count = starts.len() - 1;
        let mut advances = vec![0.0f32; count];
        let mut bearings = vec![0.0f32; count];
        let mut paragraphs = Vec::new();
        let mut levels: Vec<u8> = Vec::new();
        let mut first = 0;
        while first < count {
            let mut end = first;
            while end < count && !is_unshaped(&text[starts[end]..starts[end + 1]]) {
                end += 1;
            }
            if end > first {
                let base = starts[first];
                let local: Vec<usize> = starts[first..=end].iter().map(|s| s - base).collect();
                let paragraph_levels =
                    bidi::grapheme_levels(&text[base..starts[end]], &local, base_rtl);
                if paragraph_levels.is_empty() {
                    self.shape_run(
                        text,
                        starts,
                        first..end,
                        None,
                        &mut advances,
                        &mut bearings,
                        &mut paragraphs,
                    );
                } else {
                    if levels.is_empty() {
                        levels = vec![0; count];
                    }
                    levels[first..end].copy_from_slice(&paragraph_levels);
                    for run in bidi::level_runs(&paragraph_levels) {
                        self.shape_run(
                            text,
                            starts,
                            first + run.start..first + run.end,
                            Some(run.is_rtl()),
                            &mut advances,
                            &mut bearings,
                            &mut paragraphs,
                        );
                    }
                }
            }
            first = end + 1;
        }
        // A line feed or separator between the paragraphs is at the base level.
        if base_rtl && !levels.is_empty() {
            for (index, level) in levels.iter_mut().enumerate() {
                if is_unshaped(&text[starts[index]..starts[index + 1]]) {
                    *level = 1;
                }
            }
        }
        (advances, bearings, paragraphs, levels)
    }

    /// Shapes the graphemes `range` of `text` as one run (`right_to_left` fixes its direction)
    /// and records their advances and, when a font shapes it, the shaped glyphs.
    #[allow(clippy::too_many_arguments)] // each argument is a distinct borrow of the caller's shaping state
    fn shape_run(
        &self,
        text: &str,
        starts: &[usize],
        range: std::ops::Range<usize>,
        right_to_left: Option<bool>,
        advances: &mut [f32],
        bearings: &mut [f32],
        paragraphs: &mut Vec<ShapedParagraph>,
    ) {
        let (first, end) = (range.start, range.end);
        let base = starts[first];
        let local: Vec<usize> = starts[first..=end].iter().map(|s| s - base).collect();
        let run_text = &text[base..starts[end]];
        match FontMetrics::shape_paragraph_directed(run_text, &self.font, right_to_left) {
            Some((engines, layout)) => {
                let shaped = layout.grapheme_advances(&local);
                advances[first..end].copy_from_slice(&shaped);
                self.run_bearings(&local, &engines, &layout, &mut bearings[first..end]);
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
                let heuristic = FontMetrics::from_font(&self.font)
                    .heuristic_grapheme_advances(run_text, &local, &self.font);
                advances[first..end].copy_from_slice(&heuristic);
            }
        }
    }

    /// The glyph `QTextLine::layout_helper` reads for each grapheme of a shaped run (`local` its
    /// grapheme byte offsets): `glyphs[logClusters[position - 1]]` of the last character, the
    /// first glyph of the cluster that character is in, whichever way the run reads. A grapheme
    /// that is not the first of a cluster (a ligature spans several) therefore has the glyph of
    /// the cluster it continues. The index is into `layout.glyphs`.
    fn glyph_of_graphemes(local: &[usize], layout: &GlyphLayout) -> Vec<Option<usize>> {
        let count = local.len() - 1;
        let mut first_glyph: Vec<Option<usize>> = vec![None; count];
        for (index, glyph) in layout.glyphs.iter().enumerate() {
            let grapheme = local[..count]
                .partition_point(|&start| start <= glyph.cluster)
                .saturating_sub(1);
            first_glyph[grapheme].get_or_insert(index);
        }
        let mut glyph_of_cluster = None;
        for slot in &mut first_glyph {
            glyph_of_cluster = slot.or(glyph_of_cluster);
            *slot = glyph_of_cluster;
        }
        first_glyph
    }

    /// The negative right bearing of `glyph`, as an absolute value: Qt keeps only a negative
    /// bearing (the ink hangs over the advance, `calculateRightBearing`); 0 when the face gives
    /// none.
    fn glyph_bearing(&self, engines: &[FontEngine], glyph: &PositionedGlyph) -> f32 {
        engines
            .get(usize::from(glyph.font_index))
            .and_then(|engine| {
                engine
                    .face
                    .right_side_bearing(glyph.glyph_id, self.font.size)
            })
            .map_or(0.0, |rb| (-rb).max(0.0))
    }

    /// Fills `out` (one entry per grapheme of a shaped run, `local` its grapheme byte offsets)
    /// with the negative right bearing of the glyph that ends the grapheme. The faces keep what
    /// they read of a glyph, so asking again for the same one is a lookup.
    fn run_bearings(
        &self,
        local: &[usize],
        engines: &[FontEngine],
        layout: &GlyphLayout,
        out: &mut [f32],
    ) {
        for (slot, index) in out.iter_mut().zip(Self::glyph_of_graphemes(local, layout)) {
            *slot = index.map_or(0.0, |index| {
                self.glyph_bearing(engines, &layout.glyphs[index])
            });
        }
    }

    /// The bearing of the word that ends in grapheme `g`, for every `g` that is followed by white
    /// space starting another run (a bidi item of Qt).
    ///
    /// When `layout_helper` reaches white space it remembers the glyph of the last word again
    /// (`saveCurrentGlyph`), but with the glyph array of the item the space is in and the cluster
    /// index of the word's own item. If the space is in another item that is the glyph at the same
    /// index of the space's item, and none when the index is past the end of it or the glyph is 0
    /// (`calculateRightBearingForPreviousGlyph`). [INFERENCE from reading the source; the values
    /// Qt 6.11.2 measures agree: a Hebrew word followed by a space of the left to right base has
    /// no bearing in the `syn_bearing_rtl_wrap` oracle lines, the space's own glyph being read.]
    fn boundary_bearings(
        &self,
        text: &str,
        starts: &[usize],
        paragraphs: &[ShapedParagraph],
    ) -> Vec<(usize, f32)> {
        let blank = |g: usize| is_blank(&text[starts[g]..starts[g + 1]]);
        let mut out = Vec::new();
        for pair in paragraphs.windows(2) {
            let (before, after) = (&pair[0], &pair[1]);
            if before.end != after.first || before.end == before.first {
                continue;
            }
            let last = before.end - 1;
            if blank(last) || !blank(after.first) {
                continue;
            }
            let local: Vec<usize> = starts[before.first..=before.end]
                .iter()
                .map(|s| s - before.base)
                .collect();
            let index = Self::glyph_of_graphemes(&local, &before.layout)[last - before.first];
            let bearing = index
                .and_then(|index| after.layout.glyphs.get(index))
                .filter(|glyph| glyph.glyph_id != 0)
                .map_or(0.0, |glyph| self.glyph_bearing(&after.engines, glyph));
            out.push((last, bearing));
        }
        out
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
        /// Graphemes `first_word_overflows` looked at on this thread.
        pub(super) static FIRST_WORD_SCANNED: Cell<usize> = const { Cell::new(0) };
    }

    #[test]
    fn do_layout_builds_the_composite_text_once() {
        let mut layout = TextLayout::new(
            "one\u{2028}two\u{2028}three\u{2028}four\u{2028}five",
            Font::new("Segoe UI", 12.0),
        );
        layout.set_preedit_area(3, "ime");
        COMPOSITE_BUILDS.with(|c| c.set(0));
        layout.do_layout();
        assert_eq!(layout.line_count(), 5);
        assert_eq!(
            COMPOSITE_BUILDS.with(Cell::get),
            1,
            "one composite per layout, not per line"
        );
    }

    #[test]
    fn cached_composite_text_follows_text_and_preedit_changes() {
        let mut layout = TextLayout::new("ab\u{2028}cd", Font::new("Segoe UI", 12.0));
        layout.do_layout();
        assert_eq!(layout.line_count(), 2);
        layout.set_text("ab\u{2028}cd\u{2028}ef");
        layout.do_layout();
        assert_eq!(layout.line_count(), 3);
        layout.set_preedit_area(2, "X\u{2028}Y");
        layout.do_layout();
        assert_eq!(layout.line_count(), 4);
        assert_eq!(
            layout.line_at(1).map(|l| l.text_length),
            Some(2),
            "second line is \"Y\u{2028}\""
        );
    }

    #[test]
    fn trailing_line_separator_adds_an_empty_last_line() {
        let mut layout = TextLayout::new("ab\u{2028}", Font::new("Segoe UI", 12.0));
        layout.do_layout();
        assert_eq!(layout.line_count(), 2);
        let last = layout.line_at(1).unwrap();
        assert_eq!((last.text_start(), last.text_length()), (3, 0));
        assert!(last.y() > layout.line_at(0).unwrap().y());
        // The caret after the separator sits on the empty line.
        assert_eq!(
            layout.line_for_text_position(3).map(|l| l.line_number()),
            Some(1)
        );
    }

    #[test]
    fn line_for_text_position_end_is_exclusive_except_on_the_last_line() {
        let mut layout = TextLayout::new("ab\u{2028}cd", Font::new("Segoe UI", 12.0));
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

    /// The embedded oracle font and a letter pair its kerning narrows by 2 px, so these tests run
    /// on every machine.
    fn kerned_pair() -> (Font, &'static str) {
        let font = oracle_font();
        let pair = "AV";
        let metrics = FontMetrics::from_font(&font);
        let whole = metrics.horizontal_advance_exact(pair, &font);
        let apart: f32 = pair
            .chars()
            .map(|c| metrics.horizontal_advance_exact(&c.to_string(), &font))
            .sum();
        assert!(
            (whole - apart).abs() > 0.25,
            "precondition: the oracle font kerns {pair} ({whole} vs {apart})"
        );
        (font, pair)
    }

    #[test]
    fn grapheme_advances_are_shaped_not_summed_per_grapheme() {
        let (font, pair) = kerned_pair();
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
        let (font, pair) = kerned_pair();
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
        layout.set_wrap_mode(WrapMode::WrapAnywhere);
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
        let (font, pair) = kerned_pair();
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
        let break_before = vec![false; starts.len()];
        let composite = CompositeText {
            text: text.to_owned(),
            starts,
            advances,
            bearings: Vec::new(),
            space_bearings: Vec::new(),
            paragraphs: vec![paragraph],
            break_before,
            levels: Vec::new(),
            base_rtl: false,
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

    // ---- right bearings --------------------------------------------------------------------

    /// The composite of `text` for the bearing helpers: one advance and one bearing per grapheme,
    /// no font and no shaped paragraph. `breaks` are the graphemes a break is allowed in front of.
    fn bearing_composite(
        text: &str,
        advances: &[f32],
        bearings: &[f32],
        space_bearings: Vec<(usize, f32)>,
        breaks: &[usize],
    ) -> CompositeText {
        let mut starts: Vec<usize> = text.grapheme_indices(true).map(|(i, _)| i).collect();
        starts.push(text.len());
        assert_eq!(advances.len(), starts.len() - 1);
        let mut break_before = vec![false; starts.len()];
        for &g in breaks {
            break_before[g] = true;
        }
        CompositeText {
            text: text.to_owned(),
            starts,
            advances: advances.to_vec(),
            bearings: bearings.to_vec(),
            space_bearings,
            paragraphs: Vec::new(),
            break_before,
            levels: Vec::new(),
            base_rtl: false,
        }
    }

    #[test]
    fn a_grapheme_reads_the_first_glyph_of_the_cluster_it_is_in() {
        let of = |starts: &[usize], clusters: &[usize]| {
            let glyphs = clusters.iter().map(|&c| glyph(c, 0.0, 1.0)).collect();
            TextLayout::glyph_of_graphemes(starts, &GlyphLayout { glyphs, width: 0.0 })
        };
        // "ffi!": one ligature glyph over three graphemes, the glyph of `!` after it.
        assert_eq!(
            of(&[0, 1, 2, 3, 4], &[0, 3]),
            vec![Some(0), Some(0), Some(0), Some(1)]
        );
        // Three Hebrew letters of two bytes, shaped right to left: the glyphs come last
        // character first, and the last character in reading order has the first glyph.
        assert_eq!(
            of(&[0, 2, 4, 6], &[4, 2, 0]),
            vec![Some(2), Some(1), Some(0)]
        );
        // "e" and a combining acute (two bytes) share a cluster: the base glyph, the first.
        assert_eq!(of(&[0, 3, 4], &[0, 0, 3]), vec![Some(0), Some(2)]);
    }

    #[test]
    fn a_word_ends_before_white_space_a_break_or_the_end() {
        // "ab cd-ef": a break is allowed in front of `c` (after the space) and of `e`.
        let c = bearing_composite("ab cd-ef", &[1.0; 8], &[0.0; 8], Vec::new(), &[3, 6]);
        let ends: Vec<bool> = (0..8).map(|g| c.ends_chunk(g)).collect();
        assert_eq!(
            ends,
            [false, true, true, false, false, true, false, true],
            "a, b (a space follows), the space (a break follows), c, d, - (a break follows), e, f"
        );
        // A line feed is white space too.
        let c = bearing_composite("a \nb", &[1.0; 4], &[0.0; 4], Vec::new(), &[]);
        let ends: Vec<bool> = (0..4).map(|g| c.ends_chunk(g)).collect();
        assert_eq!(ends, [true, true, false, true]);
    }

    #[test]
    fn the_bearing_ending_a_line_follows_the_run_of_the_space_after_the_word() {
        let c = bearing_composite(
            "ab cd",
            &[4.0, 4.0, 3.0, 4.0, 4.0],
            &[0.0, 0.5, 0.0, 0.0, 1.0],
            vec![(1, 0.25), (4, 9.0)],
            &[3],
        );
        // `b` is followed by a space: the bearing read for the space.
        assert_eq!(c.end_bearing(1), 0.25);
        // `d` ends the text; an entry there would belong to another word end, not read.
        assert_eq!(c.end_bearing(4), 1.0);
        // Without an entry the word's own glyph.
        assert_eq!(c.end_bearing(0), 0.0);
    }

    #[test]
    fn the_first_word_is_too_wide_by_its_advances_and_the_bearing_that_ends_its_line() {
        // "ab cd": the word `ab` is 8 px, its last glyph hangs 0.5 px over, and `cd` follows.
        let words = bearing_composite(
            "ab cd",
            &[4.0, 4.0, 3.0, 4.0, 4.0],
            &[0.0, 0.5, 0.0, 0.0, 1.0],
            Vec::new(),
            &[3],
        );
        assert!(words.first_word_overflows(0, 0.0, 8.25));
        assert!(!words.first_word_overflows(0, 0.0, 8.5));
        // The pen is where the word starts: after two spaces of 3 px.
        assert!(words.first_word_overflows(0, 6.0, 14.25));
        assert!(!words.first_word_overflows(0, 6.0, 14.5));
        // The text ends after the word and its spaces: the line ends in white space, which has no
        // bearing, so only the advances count.
        let spaces = bearing_composite(
            "ab  ",
            &[4.0, 4.0, 3.0, 3.0],
            &[0.0, 0.5, 0.0, 0.0],
            Vec::new(),
            &[],
        );
        assert!(!spaces.first_word_overflows(0, 0.0, 8.0));
        assert!(spaces.first_word_overflows(0, 0.0, 7.75));
        // A line separator after the word ends the line with the bearing of the word.
        let separated = bearing_composite(
            "ab\u{2028}",
            &[4.0, 4.0, 0.0],
            &[0.0, 0.5, 0.0],
            Vec::new(),
            &[],
        );
        assert!(separated.first_word_overflows(0, 0.0, 8.25));
        // The word ends the text: its bearing counts.
        let ending = bearing_composite("ab", &[4.0, 4.0], &[0.0, 0.5], Vec::new(), &[]);
        assert!(ending.first_word_overflows(0, 0.0, 8.25));
        assert!(!ending.first_word_overflows(0, 0.0, 8.5));
    }

    #[test]
    #[cfg_attr(miri, ignore)] // pure arithmetic, but about 35 minutes under Tree Borrows
    fn the_first_word_scan_stops_at_the_width_not_at_the_end_of_the_word() {
        // One word of 2 000 graphemes of 1 px, lines of 10 px: every line asks, and an answer
        // that went to the end of the word would read up to 2 000 graphemes each time (200 lines
        // would read 200 000 against the 2 000 allowed).
        let n = 2_000;
        let text = "a".repeat(n);
        let c = bearing_composite(&text, &vec![1.0; n], &vec![0.0; n], Vec::new(), &[]);
        FIRST_WORD_SCANNED.with(|s| s.set(0));
        for from in (0..n).step_by(10) {
            assert!(c.first_word_overflows(from, 0.0, 10.0 - 0.5));
        }
        let scanned = FIRST_WORD_SCANNED.with(Cell::get);
        assert!(scanned <= n, "scanned {scanned} graphemes for {n}");
    }

    /// The synthetic font of the Qt oracle: it has no installed-font dependency.
    fn oracle_font() -> Font {
        use crate::text::font::SharedFontData;
        Font::new("FerrokitOracleText", 16.0).with_font_data(SharedFontData::from_vec(
            include_bytes!("../../tests/data/oracle_text.ttf").to_vec(),
        ))
    }

    #[test]
    #[cfg_attr(miri, ignore)] // shapes 20 000 characters: minutes under Miri
    fn a_long_word_is_wrapped_in_work_linear_in_its_length() {
        // `WrapAtWordBoundaryOrAnywhere` lays the first word of each line again inside the word
        // when it does not fit; deciding that must not walk the rest of the word every time.
        let count = |n: usize| {
            let mut layout = TextLayout::new("abcdefghij".repeat(n / 10), oracle_font());
            layout.set_wrap_mode(WrapMode::WrapAtWordBoundaryOrAnywhere);
            layout.set_wrap_width(Some(100.0));
            FIRST_WORD_SCANNED.with(|s| s.set(0));
            layout.do_layout();
            assert!(
                layout.line_count() > n / 30,
                "the word is cut into many lines"
            );
            FIRST_WORD_SCANNED.with(Cell::get)
        };
        let (small, large) = (count(4_000), count(16_000));
        assert!(small > 0, "the scan is reached");
        assert!(
            large <= 5 * small,
            "4x the text scanned {large} graphemes against {small}: not linear"
        );
    }

    #[test]
    fn a_document_block_is_laid_out_once() {
        // Every layout pass builds (and shapes) the composite text once; laying a wrapped block
        // with custom tab stops out must not build it for the unwrapped width first.
        let mut doc = crate::text::document::TextDocument::from_plain_text("ab\tcd ef gh");
        doc.set_default_font(oracle_font());
        doc.set_tab_stop_distance(30.0);
        doc.set_text_width(40.0);
        let block = doc.block_at(0).unwrap();
        COMPOSITE_BUILDS.with(|c| c.set(0));
        let layout = doc.block_layout(block);
        assert!(layout.line_count() > 1);
        assert_eq!(COMPOSITE_BUILDS.with(Cell::get), 1);
    }
}
