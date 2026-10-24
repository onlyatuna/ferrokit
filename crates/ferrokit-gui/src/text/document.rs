// SPDX-License-Identifier: LGPL-3.0-only
//! Rich text document model (`QTextDocument` equivalent).
//!
//! Represents an editable, multi-block structured text document with character and paragraph formatting,
//! undo/redo revision tracking, and import/export support for Plain Text, HTML, and Markdown.

use crate::geometry::primitives::{PointF, SizeF};
use crate::paint::painter::Painter;
use crate::text::block::TextBlock;
use crate::text::document_fragment::TextDocumentFragment;
use crate::text::font::Font;
use crate::text::font_metrics::FontMetrics;
use crate::text::format::TextCharFormat;
use crate::text::fragment::TextFragment;
use crate::text::layout::TextLayout;
use crate::text::markup;
use crate::text::position::{byte_to_grapheme, grapheme_count, grapheme_to_byte};
use tiny_skia::Color;
use unicode_segmentation::UnicodeSegmentation;

/// Rich text document model (`QTextDocument`).
#[derive(Debug, Clone, PartialEq)]
pub struct TextDocument {
    /// Paragraph blocks comprising this document.
    pub(crate) blocks: Vec<TextBlock>,
    /// Default base font.
    pub(crate) default_font: Font,
    /// Page size constraint for pagination and layout (if set).
    pub(crate) page_size: Option<SizeF>,
    /// Document modification status.
    pub(crate) is_modified: bool,
    /// Document edit revision counter.
    pub(crate) revision: usize,
    /// Document title meta information (from HTML `<title>`).
    pub(crate) title: String,
}

impl Default for TextDocument {
    fn default() -> Self {
        let default_font = Font::new("Segoe UI", 12.0);
        let initial_block = TextBlock::new(0, 0);
        Self {
            blocks: vec![initial_block],
            default_font,
            page_size: None,
            is_modified: false,
            revision: 0,
            title: String::new(),
        }
    }
}

impl TextDocument {
    /// Creates a new empty text document.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a document initialized with plain text.
    pub fn from_plain_text(text: impl Into<String>) -> Self {
        let mut doc = Self::new();
        doc.set_plain_text(text);
        doc
    }

    /// Creates a document initialized from basic HTML.
    pub fn from_html(html: &str) -> Self {
        let mut doc = Self::new();
        doc.set_html(html);
        doc
    }

    /// Creates a document initialized from Markdown.
    pub fn from_markdown(md: &str) -> Self {
        let mut doc = Self::new();
        doc.set_markdown(md);
        doc
    }

    /// Returns the default font.
    #[inline]
    pub fn default_font(&self) -> &Font {
        &self.default_font
    }

    /// Sets the default font.
    pub fn set_default_font(&mut self, font: Font) {
        self.default_font = font;
    }

    /// Returns whether document has unsaved edits.
    #[inline]
    pub fn is_modified(&self) -> bool {
        self.is_modified
    }

    #[inline]
    pub fn set_modified(&mut self, m: bool) {
        self.is_modified = m;
    }

    /// Returns the revision number.
    #[inline]
    pub fn revision(&self) -> usize {
        self.revision
    }
    /// Returns the text width constraint in pixels (`QTextDocument::textWidth`).
    #[inline]
    pub fn text_width(&self) -> f32 {
        self.page_size.map_or(-1.0, |s| s.width)
    }

    /// Sets the text width constraint in pixels (`QTextDocument::setTextWidth`).
    pub fn set_text_width(&mut self, width: f32) {
        if width < 0.0 {
            self.page_size = None;
        } else {
            self.page_size = Some(SizeF::new(width, f32::MAX));
        }
    }

    /// Computes the document layout and returns all laid out lines across blocks.
    pub fn layout_lines(&self) -> Vec<crate::text::line::TextLine> {
        let mut lines = Vec::new();
        for block in &self.blocks {
            let layout = self.block_layout(block);
            for i in 0..layout.line_count() {
                if let Some(l) = layout.line_at(i) {
                    lines.push(l.clone());
                }
            }
        }
        lines
    }

    /// Returns the ideal size of the document (`QTextDocument::size`).
    pub fn size(&self) -> SizeF {
        let lines = self.layout_lines();
        let mut max_w: f32 = 0.0;
        let mut total_h: f32 = 0.0;
        for l in &lines {
            if l.natural_text_width() > max_w {
                max_w = l.natural_text_width();
            }
            total_h += l.height();
        }
        SizeF::new(max_w, total_h)
    }

    /// Sizes the text to its ideal width (`QTextDocument::adjustSize`, qtextdocument.cpp:837-857).
    ///
    /// Starts from a width of 80 `x` characters, then narrows the width towards a 5:3 aspect ratio
    /// so a long paragraph is neither one line nor a tall column. Finally the width is set to the
    /// ideal (widest unwrapped line) width.
    pub fn adjust_size(&mut self) {
        let font = self.default_font.clone();
        let x_advance = FontMetrics::from_font(&font)
            .horizontal_advance_exact("x", &font)
            .round();
        let mw = x_advance * 80.0;
        self.set_text_width(mw);
        let mut size = self.size();
        if size.width != 0.0 {
            let mut w = int_sqrt(5.0 * size.height * size.width / 3.0);
            self.set_text_width(w.min(mw));
            size = self.size();
            if w * 3.0 < 5.0 * size.height {
                w = int_sqrt(2.0 * size.height * size.width);
                self.set_text_width(w.min(mw));
            }
        }
        let ideal = self.size().width;
        self.set_text_width(ideal);
    }

    /// Lays out one block at the document's text width (`setTextWidth` wrap).
    fn block_layout(&self, block: &TextBlock) -> TextLayout {
        let mut layout = block.create_layout(&self.default_font);
        if let Some(w) = self.page_size.map(|s| s.width) {
            layout.set_wrap_width(Some(w));
            layout.do_layout();
        }
        layout
    }

    /// Returns total number of paragraph blocks.
    #[inline]
    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }

    /// Returns a block by its index.
    #[inline]
    pub fn block_at(&self, idx: usize) -> Option<&TextBlock> {
        self.blocks.get(idx)
    }

    /// Returns a mutable reference to a block.
    #[inline]
    pub fn block_at_mut(&mut self, idx: usize) -> Option<&mut TextBlock> {
        self.blocks.get_mut(idx)
    }

    /// Returns the first block.
    #[inline]
    pub fn first_block(&self) -> &TextBlock {
        &self.blocks[0]
    }

    /// Returns the last block.
    #[inline]
    pub fn last_block(&self) -> &TextBlock {
        self.blocks.last().unwrap()
    }

    /// Finds block index corresponding to a global grapheme offset.
    pub fn block_index_at_grapheme(&self, grapheme_pos: usize) -> usize {
        let mut accum = 0;
        for (i, b) in self.blocks.iter().enumerate() {
            let b_len = b.text().graphemes(true).count();
            if grapheme_pos <= accum + b_len {
                return i;
            }
            accum += b_len + 1; // +1 for block newline
        }
        self.blocks.len().saturating_sub(1)
    }

    /// Returns total number of characters (grapheme clusters) in the document.
    pub fn character_count(&self) -> usize {
        let mut count = 0;
        for (i, b) in self.blocks.iter().enumerate() {
            if i > 0 {
                count += 1; // newline
            }
            count += b.text().graphemes(true).count();
        }
        count
    }

    /// Returns plain text representation of the document.
    pub fn to_plain_text(&self) -> String {
        let mut out = String::new();
        for (i, b) in self.blocks.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            out.push_str(&b.text());
        }
        out
    }

    /// Replaces document contents with plain text.
    ///
    /// `\n`, `\r\n`, `\r` and U+2029 (paragraph separator) each start a new block.
    pub fn set_plain_text(&mut self, text: impl Into<String>) {
        let text = text.into();
        self.blocks = split_paragraphs(&text)
            .map(|line| {
                let mut block = TextBlock::new(0, 0);
                if !line.is_empty() {
                    block.fragments.push(TextFragment::plain(line));
                }
                block
            })
            .collect();
        self.title.clear();
        self.finish_edit();
    }

    /// Returns the document title (`QTextDocument::metaInformation(DocumentTitle)`),
    /// imported from an HTML `<title>` element.
    #[inline]
    pub fn document_title(&self) -> &str {
        &self.title
    }

    /// Sets the document title (`QTextDocument::setMetaInformation(DocumentTitle, …)`).
    pub fn set_document_title(&mut self, title: impl Into<String>) {
        self.title = title.into();
    }

    /// Serializes the document into HTML that `set_html` re-imports losslessly
    /// (`QTextDocument::toHtml`).
    pub fn to_html(&self) -> String {
        markup::write_html(&self.blocks, &self.title)
    }

    /// Replaces the document with parsed HTML (`QTextDocument::setHtml`).
    ///
    /// Supports inline formatting (`<b>`, `<strong>`, `<i>`, `<em>`, `<u>`, `<s>`, `<sup>`, `<sub>`,
    /// `<font color>`, `<span style>`), hyperlinks and named anchors (`<a href>`, `<a name>`, `id`),
    /// block elements (`<p>`, `<div>`, `<h1>`..`<h6>`, `<li>`, `<pre>`, `<br>`), character entities,
    /// HTML whitespace collapsing and the `<title>` element.
    pub fn set_html(&mut self, html: &str) {
        let parsed = markup::parse_html(html);
        self.blocks = parsed.blocks;
        self.title = parsed.title;
        self.finish_edit();
    }

    /// Serializes the document into Markdown (`QTextDocument::toMarkdown`).
    pub fn to_markdown(&self) -> String {
        markup::write_markdown(&self.blocks)
    }

    /// Replaces the document with parsed Markdown (`QTextDocument::setMarkdown`).
    ///
    /// Supports ATX (`#`) and setext headings, paragraphs (soft line breaks joined), hard line
    /// breaks, fenced code blocks, `**bold**`, `*italic*`, `***both***`, `~~strike~~`, code spans,
    /// `[links](url)`, `<autolinks>` and backslash escapes.
    pub fn set_markdown(&mut self, md: &str) {
        self.blocks = markup::parse_markdown(md);
        self.title.clear();
        self.finish_edit();
    }

    /// Inserts `text` with `format` at the global grapheme position `pos`, preserving the
    /// formatting of surrounding text. Line breaks split the block; new blocks inherit the
    /// paragraph format of the block they split (`QTextCursor::insertText`).
    pub fn insert_formatted_text(&mut self, pos: usize, text: &str, format: &TextCharFormat) {
        self.insert_formatted_text_end(pos, text, format);
    }

    /// Like [`insert_formatted_text`](Self::insert_formatted_text), returning the grapheme position
    /// just after the inserted text. When the text merges with the cluster that follows it, the
    /// end lies after the merged cluster. Only the affected block is re-segmented.
    pub(crate) fn insert_formatted_text_end(
        &mut self,
        pos: usize,
        text: &str,
        format: &TextCharFormat,
    ) -> usize {
        if text.is_empty() {
            return pos;
        }
        let (bi, byte) = self.locate(pos);
        let paragraphs = split_paragraphs(text).count();
        let end_byte = match split_paragraphs(text).last() {
            Some(last) if paragraphs > 1 => last.len(),
            _ => byte + text.len(),
        };
        let block_format = self.blocks[bi].block_format.clone();
        let block_char_format = self.blocks[bi].char_format.clone();
        let blocks = split_paragraphs(text)
            .map(|line| {
                let mut b = TextBlock::new(0, 0);
                b.block_format = block_format.clone();
                b.char_format = block_char_format.clone();
                if !line.is_empty() {
                    b.fragments.push(TextFragment::new(line, format.clone()));
                }
                b
            })
            .collect();
        self.insert_document_fragment(pos, &TextDocumentFragment::from_blocks(blocks));
        let end_block = &self.blocks[bi + paragraphs - 1];
        let block_text = end_block.text();
        let clusters = block_text
            .grapheme_indices(true)
            .take_while(|&(start, _)| start < end_byte)
            .count();
        end_block.position + clusters
    }

    /// Removes `count` grapheme clusters starting at `pos`, merging blocks when a block
    /// separator is removed (the merged block keeps the first block's paragraph format).
    pub fn remove_graphemes(&mut self, pos: usize, count: usize) {
        if count == 0 {
            return;
        }
        let (sb, sbyte) = self.locate(pos);
        let (eb, ebyte) = self.locate(pos.saturating_add(count));
        if sb == eb {
            if sbyte == ebyte {
                return;
            }
            self.blocks[sb].remove_byte_range(sbyte, ebyte);
        } else {
            let tail = self.blocks[eb].split_off_fragments(ebyte);
            self.blocks[sb].split_off_fragments(sbyte);
            self.blocks[sb].fragments.extend(tail);
            self.blocks[sb].normalize_fragments();
            self.blocks.drain(sb + 1..=eb);
        }
        self.finish_edit();
    }

    /// Returns a rich copy of the text between two global grapheme positions
    /// (`QTextCursor::selection`). Every returned block carries its paragraph format.
    pub fn document_fragment(&self, start: usize, end: usize) -> TextDocumentFragment {
        let (start, end) = (start.min(end), start.max(end));
        let (sb, sbyte) = self.locate(start);
        let (eb, ebyte) = self.locate(end);
        let blocks = (sb..=eb)
            .map(|bi| {
                let b = &self.blocks[bi];
                let from = if bi == sb { sbyte } else { 0 };
                let to = if bi == eb { ebyte } else { b.length() };
                let mut nb = TextBlock::new(bi - sb, 0);
                nb.block_format = b.block_format.clone();
                nb.char_format = b.char_format.clone();
                nb.fragments = b.slice_fragments(from, to);
                nb
            })
            .collect();
        TextDocumentFragment::from_blocks(blocks)
    }

    /// Inserts rich content at a global grapheme position (`QTextCursor::insertFragment`).
    ///
    /// The first fragment block is merged into the block at `pos`; each further block becomes a new
    /// paragraph with its own format, and the text after `pos` moves to the last inserted block.
    pub fn insert_document_fragment(&mut self, pos: usize, fragment: &TextDocumentFragment) {
        let mut incoming = fragment.blocks().iter();
        let Some(first) = incoming.next() else {
            return;
        };
        let (bi, byte) = self.locate(pos);
        if fragment.blocks().len() == 1 {
            if first.fragments.iter().all(|f| f.is_empty()) {
                return;
            }
            self.blocks[bi].insert_fragments_at(byte, first.fragments.iter().cloned());
        } else {
            let tail = self.blocks[bi].split_off_fragments(byte);
            self.blocks[bi]
                .fragments
                .extend(first.fragments.iter().cloned());
            self.blocks[bi].normalize_fragments();
            let mut new_blocks: Vec<TextBlock> = incoming
                .map(|b| {
                    let mut nb = TextBlock::new(0, 0);
                    nb.block_format = b.block_format.clone();
                    nb.char_format = b.char_format.clone();
                    nb.fragments = b.fragments.clone();
                    nb
                })
                .collect();
            if let Some(last) = new_blocks.last_mut() {
                last.fragments.extend(tail);
                last.normalize_fragments();
            }
            self.blocks.splice(bi + 1..bi + 1, new_blocks);
        }
        self.finish_edit();
    }

    /// Returns the character format in effect at `pos`: the format of the character before the
    /// position, or of the first character when `pos` starts a non-empty block
    /// (`QTextCursor::charFormat`).
    pub fn char_format_at(&self, pos: usize) -> TextCharFormat {
        let (bi, byte) = self.locate(pos);
        self.blocks[bi].char_format_before_byte(byte).clone()
    }

    /// Returns the format of the character that starts at `pos`, if any.
    pub fn char_format_of_character(&self, pos: usize) -> Option<&TextCharFormat> {
        let (bi, byte) = self.locate(pos);
        self.blocks[bi].char_format_at_byte(byte)
    }

    /// Returns the hyperlink target of the character starting at `pos` (`QTextEdit::anchorAt`).
    pub fn anchor_at(&self, pos: usize) -> Option<&str> {
        self.char_format_of_character(pos)
            .and_then(|f| f.anchor_href.as_deref())
    }

    /// Returns the global grapheme position of the named anchor `name`
    /// (`<a name="…">` or an element `id`), used by `scrollToAnchor`.
    pub fn find_anchor(&self, name: &str) -> Option<usize> {
        let mut block_start = 0;
        for b in &self.blocks {
            let text = b.text();
            let mut byte = 0;
            for f in &b.fragments {
                if f.char_format().anchor_name.as_deref() == Some(name) {
                    return Some(block_start + byte_to_grapheme(&text, byte));
                }
                byte += f.length();
            }
            block_start += grapheme_count(&text) + 1;
        }
        None
    }

    /// Applies `f` to the character format of every character between two global grapheme positions.
    pub fn apply_char_format(
        &mut self,
        start: usize,
        end: usize,
        mut f: impl FnMut(&mut TextCharFormat),
    ) {
        let (start, end) = (start.min(end), start.max(end));
        if start == end {
            return;
        }
        let (sb, sbyte) = self.locate(start);
        let (eb, ebyte) = self.locate(end);
        for bi in sb..=eb {
            let from = if bi == sb { sbyte } else { 0 };
            let to = if bi == eb {
                ebyte
            } else {
                self.blocks[bi].length()
            };
            self.blocks[bi].apply_char_format(from, to, &mut f);
        }
        self.finish_edit();
    }

    /// Merges `format` into the character formats of a range (`QTextCursor::mergeCharFormat`).
    pub fn merge_char_format(&mut self, start: usize, end: usize, format: &TextCharFormat) {
        self.apply_char_format(start, end, |f| f.merge(format));
    }

    /// Removes `count` whole blocks starting at block index `first`. The document always keeps
    /// at least one (empty) block.
    pub fn remove_blocks(&mut self, first: usize, count: usize) {
        let end = first.saturating_add(count).min(self.blocks.len());
        if first >= end {
            return;
        }
        self.blocks.drain(first..end);
        self.finish_edit();
    }

    /// Maps a global grapheme position to `(block index, byte offset in block text)`,
    /// clamping past-the-end positions to the end of the document.
    fn locate(&self, pos: usize) -> (usize, usize) {
        let mut accum = 0;
        for (i, b) in self.blocks.iter().enumerate() {
            let text = b.text();
            let len = grapheme_count(&text);
            if pos <= accum + len {
                return (i, grapheme_to_byte(&text, pos - accum));
            }
            accum += len + 1;
        }
        let last = self.blocks.len() - 1;
        (last, self.blocks[last].length())
    }

    /// Restores structural invariants and bumps the revision after a content change.
    fn finish_edit(&mut self) {
        if self.blocks.is_empty() {
            self.blocks.push(TextBlock::new(0, 0));
        }
        self.recompute_block_positions();
        self.revision += 1;
        self.is_modified = true;
    }

    /// Renders all blocks to a `Painter` in black where no format sets a colour.
    pub fn draw(&self, painter: &mut Painter, pos: PointF) {
        self.draw_with_color(painter, pos, Color::BLACK);
    }

    /// Renders all blocks at `pos`, wrapped to the text width, with `default_color` where no format
    /// sets a foreground colour. Blocks stack by their laid-out line heights, as `size()` measures.
    pub fn draw_with_color(&self, painter: &mut Painter, pos: PointF, default_color: Color) {
        let mut curr_y = pos.y;
        for block in &self.blocks {
            let layout = self.block_layout(block);
            layout.draw_with_color(painter, PointF::new(pos.x, curr_y), &[], default_color);
            let block_h: f32 = (0..layout.line_count())
                .filter_map(|i| layout.line_at(i))
                .map(|l| l.height())
                .sum();
            curr_y += block_h;
        }
    }

    fn recompute_block_positions(&mut self) {
        let mut pos = 0;
        for (i, b) in self.blocks.iter_mut().enumerate() {
            b.block_number = i;
            b.position = pos;
            pos += b.text().graphemes(true).count() + 1;
        }
    }
}

/// `qt_int_sqrt`: the integer square root, floored.
fn int_sqrt(v: f32) -> f32 {
    (v.max(0.0) as u32 as f32).sqrt().floor()
}

/// Canonical Qt alias.
pub type QTextDocument = TextDocument;

/// Splits text at paragraph separators (`\r\n`, `\r`, `\n`, U+2029), always yielding at least one piece.
fn split_paragraphs(text: &str) -> impl Iterator<Item = &str> {
    let mut rest = Some(text);
    std::iter::from_fn(move || {
        let s = rest?;
        match s.find(['\r', '\n', '\u{2029}']) {
            Some(i) => {
                let sep_len = if s[i..].starts_with("\r\n") {
                    2
                } else {
                    s[i..].chars().next().map_or(1, char::len_utf8)
                };
                rest = Some(&s[i + sep_len..]);
                Some(&s[..i])
            }
            None => {
                rest = None;
                Some(s)
            }
        }
    })
}
