// SPDX-License-Identifier: LGPL-3.0-only
//! Rich text clipboard payload (`QTextDocumentFragment` equivalent).
//!
//! Stores a sequence of formatted blocks and fragments detached from a document,
//! suitable for copy, cut, paste, and drag-and-drop operations.

use crate::text::block::TextBlock;
use crate::text::fragment::TextFragment;

/// A piece of rich text document (`QTextDocumentFragment`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TextDocumentFragment {
    pub(crate) blocks: Vec<TextBlock>,
}

impl TextDocumentFragment {
    /// Creates an empty document fragment.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a fragment from plain text.
    pub fn from_plain_text(text: impl Into<String>) -> Self {
        let text = text.into();
        let mut blocks = Vec::new();
        let mut pos = 0;

        for (idx, line) in text.lines().enumerate() {
            let mut block = TextBlock::new(idx, pos);
            block.fragments.push(TextFragment::plain(line));
            pos += line.len() + 1;
            blocks.push(block);
        }

        if blocks.is_empty() {
            blocks.push(TextBlock::new(0, 0));
        }

        Self { blocks }
    }

    /// Creates a fragment with explicit blocks.
    pub fn from_blocks(blocks: Vec<TextBlock>) -> Self {
        Self { blocks }
    }

    /// Returns whether this fragment is empty.
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty() || (self.blocks.len() == 1 && self.blocks[0].is_empty())
    }

    /// Returns plain text representation of all blocks joined with newlines.
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

    /// Exports fragment as basic HTML.
    pub fn to_html(&self) -> String {
        let mut html = String::new();
        for b in &self.blocks {
            html.push_str("<p>");
            for f in &b.fragments {
                let fmt = f.char_format();
                let mut tag_close = String::new();

                if fmt.font_weight.is_some_and(|w| w as u16 >= 700) {
                    html.push_str("<b>");
                    tag_close.insert_str(0, "</b>");
                }
                if fmt.font_italic.unwrap_or(false) {
                    html.push_str("<i>");
                    tag_close.insert_str(0, "</i>");
                }
                if fmt.font_underline {
                    html.push_str("<u>");
                    tag_close.insert_str(0, "</u>");
                }

                html.push_str(f.text());
                html.push_str(&tag_close);
            }
            html.push_str("</p>");
        }
        html
    }

    /// Returns slice of blocks.
    pub fn blocks(&self) -> &[TextBlock] {
        &self.blocks
    }

    /// Returns the number of grapheme clusters, counting one per block separator.
    pub fn character_count(&self) -> usize {
        let mut count = 0;
        for (i, b) in self.blocks.iter().enumerate() {
            if i > 0 {
                count += 1;
            }
            count += b.character_count();
        }
        count
    }

    /// Appends `other`, joining its first block onto this fragment's last block.
    pub fn append(&mut self, other: &TextDocumentFragment) {
        let mut incoming = other.blocks.iter();
        match (self.blocks.last_mut(), incoming.next()) {
            (Some(last), Some(first)) => {
                last.fragments.extend(first.fragments.iter().cloned());
                last.normalize_fragments();
            }
            (None, Some(first)) => self.blocks.push(first.clone()),
            (_, None) => return,
        }
        self.blocks.extend(incoming.cloned());
    }
}

/// Canonical Qt alias.
pub type QTextDocumentFragment = TextDocumentFragment;
