// SPDX-License-Identifier: LGPL-3.0-only
//! Paragraph block within a rich text document (`QTextBlock` equivalent).
//!
//! A document is an ordered sequence of `TextBlock`s. Each block owns a sequence of
//! `TextFragment`s and maintains paragraph layout metrics and syntax highlighter state.

use crate::text::block_data::TextBlockUserData;
use crate::text::font::Font;
use crate::text::format::{FormatRange, TextBlockFormat, TextCharFormat};
use crate::text::fragment::TextFragment;
use crate::text::layout::TextLayout;
use crate::text::position::{byte_to_grapheme, grapheme_count};
use std::sync::Arc;
use unicode_segmentation::UnicodeSegmentation;

/// Paragraph block model (`QTextBlock`).
#[derive(Debug, Clone)]
pub struct TextBlock {
    /// 0-based block index within document.
    pub(crate) block_number: usize,
    /// Document-level character start offset.
    pub(crate) position: usize,
    /// Paragraph layout format.
    pub(crate) block_format: TextBlockFormat,
    /// Default character format for newly inserted text.
    pub(crate) char_format: TextCharFormat,
    /// Styled text fragments comprising this block.
    pub(crate) fragments: Vec<TextFragment>,
    /// Syntax highlighter multi-line parse state (defaults to -1).
    pub(crate) user_state: i32,
    /// Syntax-highlighter overlay (`QTextLayout::additionalFormats`), grapheme-indexed. Kept apart
    /// from the fragments so highlighting never alters the user's character formats.
    additional_formats: Vec<FormatRange>,
    /// Hash of the block text `additional_formats` was computed for; the overlay is ignored once
    /// the text has changed.
    additional_formats_key: u64,
    /// Custom user data pointer/payload.
    pub(crate) user_data: Option<Arc<dyn TextBlockUserData>>,
}
impl PartialEq for TextBlock {
    fn eq(&self, other: &Self) -> bool {
        self.block_number == other.block_number
            && self.position == other.position
            && self.block_format == other.block_format
            && self.char_format == other.char_format
            && self.fragments == other.fragments
            && self.user_state == other.user_state
            && self.additional_formats() == other.additional_formats()
    }
}

impl Default for TextBlock {
    fn default() -> Self {
        Self {
            block_number: 0,
            position: 0,
            block_format: TextBlockFormat::default(),
            char_format: TextCharFormat::default(),
            fragments: Vec::new(),
            user_state: -1,
            additional_formats: Vec::new(),
            additional_formats_key: 0,
            user_data: None,
        }
    }
}

impl TextBlock {
    /// Creates a new empty block with given index.
    pub fn new(block_number: usize, position: usize) -> Self {
        Self {
            block_number,
            position,
            ..Default::default()
        }
    }

    /// Creates a block initialized with a single plain text string.
    pub fn from_plain_text(block_number: usize, position: usize, text: impl Into<String>) -> Self {
        let mut b = Self::new(block_number, position);
        b.fragments.push(TextFragment::plain(text));
        b
    }

    /// Returns the 0-based block index.
    #[inline]
    pub fn block_number(&self) -> usize {
        self.block_number
    }

    /// Returns the global document start position.
    #[inline]
    pub fn position(&self) -> usize {
        self.position
    }

    /// Returns the paragraph format.
    #[inline]
    pub fn block_format(&self) -> &TextBlockFormat {
        &self.block_format
    }

    #[inline]
    pub fn set_block_format(&mut self, format: TextBlockFormat) {
        self.block_format = format;
    }

    /// Returns the default character format.
    #[inline]
    pub fn char_format(&self) -> &TextCharFormat {
        &self.char_format
    }

    #[inline]
    pub fn set_char_format(&mut self, format: TextCharFormat) {
        self.char_format = format;
    }

    /// Returns the assembled plain text string of all fragments.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for f in &self.fragments {
            out.push_str(f.text());
        }
        out
    }

    /// Returns the total UTF-8 byte length of all text in this block.
    pub fn length(&self) -> usize {
        self.fragments.iter().map(|f| f.length()).sum()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.fragments.iter().all(|f| f.is_empty())
    }

    /// Returns read-only slice of fragments.
    #[inline]
    pub fn fragments(&self) -> &[TextFragment] {
        &self.fragments
    }

    /// Returns mutable slice of fragments.
    #[inline]
    pub fn fragments_mut(&mut self) -> &mut Vec<TextFragment> {
        &mut self.fragments
    }

    /// Returns syntax highlighter multi-line state.
    #[inline]
    pub fn user_state(&self) -> i32 {
        self.user_state
    }

    #[inline]
    pub fn set_user_state(&mut self, state: i32) {
        self.user_state = state;
    }

    fn text_key(&self) -> u64 {
        use std::hash::Hasher;
        // Streamed bytes, so the key depends on the text only, not on how fragments split it.
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for f in &self.fragments {
            h.write(f.text().as_bytes());
        }
        h.finish()
    }

    /// Syntax-highlighter overlay ranges (`QTextLayout::additionalFormats`); empty once the block
    /// text has changed since they were set.
    pub fn additional_formats(&self) -> &[FormatRange] {
        if self.additional_formats_key == self.text_key() {
            &self.additional_formats
        } else {
            &[]
        }
    }

    /// Replaces the overlay formats (`QSyntaxHighlighter::setFormat` results) without touching
    /// the fragments' user formats.
    pub fn set_additional_formats(&mut self, formats: Vec<FormatRange>) {
        self.additional_formats_key = self.text_key();
        self.additional_formats = formats;
    }

    /// Returns the fragments with the overlay merged in: each fragment is split where the overlay
    /// changes and overlay formats are merged over the user format.
    pub fn effective_fragments(&self) -> Vec<TextFragment> {
        let overlay = self.additional_formats();
        if overlay.is_empty() {
            return self.fragments.clone();
        }
        let mut out: Vec<TextFragment> = Vec::new();
        let mut index = 0usize;
        for frag in &self.fragments {
            for g in frag.text().graphemes(true) {
                let mut fmt = frag.char_format().clone();
                for r in overlay {
                    if index >= r.start && index < r.start + r.length {
                        fmt.merge(&r.format);
                    }
                }
                match out.last_mut() {
                    Some(prev) if prev.format == fmt => prev.text.push_str(g),
                    _ => out.push(TextFragment::new(g, fmt)),
                }
                index += 1;
            }
        }
        out
    }
    /// Returns custom user data attached to this block (`QTextBlock::userData`).
    #[inline]
    pub fn user_data(&self) -> Option<Arc<dyn TextBlockUserData>> {
        self.user_data.clone()
    }

    /// Sets custom user data attached to this block (`QTextBlock::setUserData`).
    #[inline]
    pub fn set_user_data(&mut self, data: Option<Arc<dyn TextBlockUserData>>) {
        self.user_data = data;
    }

    /// Converts this block into a laid-out `TextLayout`.
    pub fn create_layout(&self, base_font: &Font) -> TextLayout {
        let text = self.text();
        let mut layout = TextLayout::new(text, base_font.clone());
        layout.set_alignment(self.block_format.alignment);
        let mut formats = self.format_ranges();
        formats.extend_from_slice(self.additional_formats());
        layout.set_formats(formats);
        layout.do_layout();
        layout
    }

    /// Returns the character format runs of this block as grapheme-indexed `FormatRange`s,
    /// matching the grapheme coordinates used by `TextLayout` and `TextLine`.
    pub fn format_ranges(&self) -> Vec<FormatRange> {
        let text = self.text();
        let mut ranges = Vec::with_capacity(self.fragments.len());
        let mut byte = 0;
        let mut grapheme = 0;
        for f in &self.fragments {
            if f.text().is_empty() {
                continue;
            }
            let end_byte = byte + f.text().len();
            let end_grapheme = byte_to_grapheme(&text, end_byte);
            if end_grapheme > grapheme {
                ranges.push(FormatRange::new(
                    grapheme,
                    end_grapheme - grapheme,
                    f.char_format().clone(),
                ));
            }
            byte = end_byte;
            grapheme = end_grapheme;
        }
        ranges
    }

    /// Returns the number of grapheme clusters (user-perceived characters) in this block.
    pub fn character_count(&self) -> usize {
        grapheme_count(&self.text())
    }

    /// Returns the character format of the fragment containing the byte just before `byte`
    /// (or the first fragment when `byte` is at the start), falling back to the block's char format.
    pub(crate) fn char_format_before_byte(&self, byte: usize) -> &TextCharFormat {
        let mut offset = 0;
        let mut last = None;
        for f in &self.fragments {
            if f.text().is_empty() {
                continue;
            }
            if last.is_none() && byte == 0 {
                return f.char_format();
            }
            let end = offset + f.text().len();
            if byte > offset && byte <= end {
                return f.char_format();
            }
            offset = end;
            last = Some(f.char_format());
        }
        last.unwrap_or(&self.char_format)
    }

    /// Returns the character format of the fragment containing the character starting at `byte`.
    pub(crate) fn char_format_at_byte(&self, byte: usize) -> Option<&TextCharFormat> {
        let mut offset = 0;
        for f in &self.fragments {
            let end = offset + f.text().len();
            if byte >= offset && byte < end {
                return Some(f.char_format());
            }
            offset = end;
        }
        None
    }

    /// Ensures a fragment boundary exists at `byte`, splitting a fragment if needed.
    /// Returns the index of the first fragment starting at or after `byte`.
    pub(crate) fn split_fragments_at(&mut self, byte: usize) -> usize {
        let mut offset = 0;
        for i in 0..self.fragments.len() {
            let len = self.fragments[i].text.len();
            if byte <= offset {
                return i;
            }
            if byte < offset + len {
                let tail = self.fragments[i].text.split_off(byte - offset);
                let format = self.fragments[i].format.clone();
                self.fragments
                    .insert(i + 1, TextFragment::new(tail, format));
                return i + 1;
            }
            offset += len;
        }
        self.fragments.len()
    }

    /// Inserts formatted fragments at the UTF-8 byte offset `byte` of the block text.
    pub(crate) fn insert_fragments_at(
        &mut self,
        byte: usize,
        fragments: impl IntoIterator<Item = TextFragment>,
    ) {
        let idx = self.split_fragments_at(byte);
        let tail = self.fragments.split_off(idx);
        self.fragments.extend(fragments);
        self.fragments.extend(tail);
        self.normalize_fragments();
    }

    /// Removes the text between the UTF-8 byte offsets `start..end` of the block text.
    pub(crate) fn remove_byte_range(&mut self, start: usize, end: usize) {
        if start >= end {
            return;
        }
        let first = self.split_fragments_at(start);
        let last = self.split_fragments_at(end);
        self.fragments.drain(first..last);
        self.normalize_fragments();
    }

    /// Splits the block at `byte`, returning the fragments that followed it.
    pub(crate) fn split_off_fragments(&mut self, byte: usize) -> Vec<TextFragment> {
        let idx = self.split_fragments_at(byte);
        let tail = self.fragments.split_off(idx);
        self.normalize_fragments();
        tail
    }

    /// Returns copies of the fragments covering the byte range `start..end`.
    pub(crate) fn slice_fragments(&self, start: usize, end: usize) -> Vec<TextFragment> {
        let mut out = Vec::new();
        let mut offset = 0;
        for f in &self.fragments {
            let f_end = offset + f.text.len();
            let s = start.max(offset);
            let e = end.min(f_end);
            if s < e {
                out.push(TextFragment::new(
                    &f.text[s - offset..e - offset],
                    f.format.clone(),
                ));
            }
            offset = f_end;
        }
        out
    }

    /// Applies `f` to the character formats of the text between byte offsets `start..end`.
    pub(crate) fn apply_char_format(
        &mut self,
        start: usize,
        end: usize,
        f: &mut dyn FnMut(&mut TextCharFormat),
    ) {
        if start >= end {
            return;
        }
        let first = self.split_fragments_at(start);
        let last = self.split_fragments_at(end);
        for frag in &mut self.fragments[first..last] {
            f(&mut frag.format);
        }
        self.normalize_fragments();
    }

    /// Drops empty fragments and merges adjacent fragments sharing the same format.
    pub(crate) fn normalize_fragments(&mut self) {
        let mut merged: Vec<TextFragment> = Vec::with_capacity(self.fragments.len());
        for f in self.fragments.drain(..) {
            if f.text.is_empty() {
                continue;
            }
            match merged.last_mut() {
                Some(prev) if prev.format == f.format => prev.text.push_str(&f.text),
                _ => merged.push(f),
            }
        }
        self.fragments = merged;
    }
}

/// Canonical Qt alias.
pub type QTextBlock = TextBlock;
