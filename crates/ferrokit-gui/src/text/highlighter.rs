// SPDX-License-Identifier: LGPL-3.0-only
//! Syntax highlighting framework (`QSyntaxHighlighter` equivalent).
//!
//! Provides rule-based syntax coloring, token matching, multi-line comment / string
//! block state tracking (`current_block_state` / `previous_block_state`), and fragment re-formatting.

use crate::text::block::TextBlock;
use crate::text::document::TextDocument;
use crate::text::format::{FormatRange, TextCharFormat};
use tiny_skia::Color;
use unicode_segmentation::UnicodeSegmentation;

/// Context passed to highlighter callbacks to record formatting and state changes.
pub struct HighlightContext<'a> {
    pub(crate) text: &'a str,
    pub(crate) ranges: Vec<FormatRange>,
    pub(crate) current_state: i32,
    pub(crate) previous_state: i32,
}

impl<'a> HighlightContext<'a> {
    pub fn new(text: &'a str, prev_state: i32) -> Self {
        Self {
            text,
            ranges: Vec::new(),
            current_state: -1,
            previous_state: prev_state,
        }
    }

    /// Sets character format for a range of grapheme clusters.
    /// Returns the text being highlighted.
    #[inline]
    pub fn text(&self) -> &str {
        self.text
    }

    pub fn set_format(&mut self, start: usize, length: usize, format: TextCharFormat) {
        if length > 0 {
            self.ranges.push(FormatRange::new(start, length, format));
        }
    }

    /// Sets foreground color format for a range of graphemes.
    pub fn set_format_color(&mut self, start: usize, length: usize, color: Color) {
        let fmt = TextCharFormat::default().with_foreground(color);
        self.set_format(start, length, fmt);
    }

    /// Returns previous block syntax state (useful for multi-line comments).
    #[inline]
    pub fn previous_block_state(&self) -> i32 {
        self.previous_state
    }

    /// Returns current block syntax state.
    #[inline]
    pub fn current_block_state(&self) -> i32 {
        self.current_state
    }

    /// Sets the syntax state of the current block.
    #[inline]
    pub fn set_current_block_state(&mut self, new_state: i32) {
        self.current_state = new_state;
    }
}

/// A pattern-based syntax highlighting rule.
#[derive(Debug, Clone, PartialEq)]
pub enum HighlightRule {
    /// Exact keyword match (delimited by word boundaries).
    Keyword {
        word: String,
        format: TextCharFormat,
    },
    /// Single-line delimiter to end of line (e.g. `//` comments).
    LineComment {
        prefix: String,
        format: TextCharFormat,
    },
    /// Quoted literal string with given quote character (e.g. `"` or `'`).
    QuotedString { quote: char, format: TextCharFormat },
}

/// Syntax highlighting engine (`QSyntaxHighlighter`).
#[derive(Debug, Clone, Default)]
pub struct SyntaxHighlighter {
    rules: Vec<HighlightRule>,
}

impl SyntaxHighlighter {
    /// Creates an empty syntax highlighter.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a keyword rule.
    pub fn add_keyword(&mut self, keyword: impl Into<String>, format: TextCharFormat) {
        self.rules.push(HighlightRule::Keyword {
            word: keyword.into(),
            format,
        });
    }

    /// Adds multiple keyword rules sharing the same format.
    pub fn add_keywords(&mut self, keywords: &[&str], format: TextCharFormat) {
        for &k in keywords {
            self.rules.push(HighlightRule::Keyword {
                word: k.to_string(),
                format: format.clone(),
            });
        }
    }

    /// Adds a line comment rule (e.g. `//`).
    pub fn add_line_comment(&mut self, prefix: impl Into<String>, format: TextCharFormat) {
        self.rules.push(HighlightRule::LineComment {
            prefix: prefix.into(),
            format,
        });
    }

    /// Adds a quoted string rule (e.g. `"`).
    pub fn add_quoted_string(&mut self, quote: char, format: TextCharFormat) {
        self.rules
            .push(HighlightRule::QuotedString { quote, format });
    }

    /// Rehighlights all blocks in the given document.
    pub fn rehighlight(&self, doc: &mut TextDocument) {
        let mut prev_state = -1;
        for i in 0..doc.block_count() {
            if let Some(block) = doc.block_at_mut(i) {
                let next_state = self.highlight_block_internal(block, prev_state);
                prev_state = next_state;
            }
        }
    }

    /// Highlights a single block given previous block's state.
    pub fn rehighlight_block(&self, block: &mut TextBlock, prev_state: i32) -> i32 {
        self.highlight_block_internal(block, prev_state)
    }

    fn highlight_block_internal(&self, block: &mut TextBlock, prev_state: i32) -> i32 {
        let text = block.text();
        let mut ctx = HighlightContext::new(&text, prev_state);

        // Apply rules
        for rule in &self.rules {
            match rule {
                HighlightRule::Keyword { word, format } => {
                    for (offset, _) in text.split_word_bound_indices() {
                        if text[offset..].starts_with(word.as_str()) {
                            let end = offset + word.len();
                            // Verify word boundary at end
                            let is_end_boundary = end == text.len()
                                || text[end..]
                                    .chars()
                                    .next()
                                    .is_none_or(|c| !c.is_alphanumeric() && c != '_');
                            if is_end_boundary {
                                let grapheme_start = text[..offset].graphemes(true).count();
                                let grapheme_len = word.graphemes(true).count();
                                ctx.set_format(grapheme_start, grapheme_len, format.clone());
                            }
                        }
                    }
                }
                HighlightRule::LineComment { prefix, format } => {
                    if let Some(idx) = text.find(prefix) {
                        let grapheme_start = text[..idx].graphemes(true).count();
                        let grapheme_len = text[idx..].graphemes(true).count();
                        ctx.set_format(grapheme_start, grapheme_len, format.clone());
                    }
                }
                HighlightRule::QuotedString { quote, format } => {
                    let mut in_str = false;
                    let mut start_byte = 0;
                    for (byte_idx, ch) in text.char_indices() {
                        if ch == *quote {
                            if in_str {
                                in_str = false;
                                let len_bytes = byte_idx + ch.len_utf8() - start_byte;
                                let g_start = text[..start_byte].graphemes(true).count();
                                let g_len = text[start_byte..start_byte + len_bytes]
                                    .graphemes(true)
                                    .count();
                                ctx.set_format(g_start, g_len, format.clone());
                            } else {
                                in_str = true;
                                start_byte = byte_idx;
                            }
                        }
                    }
                }
            }
        }

        // Like `QSyntaxHighlighter::setFormat` -> `QTextLayout::setAdditionalFormats`: the result
        // is a per-block overlay; the fragments' user formats are left untouched.
        block.set_additional_formats(ctx.ranges);
        block.set_user_state(ctx.current_state);
        ctx.current_state
    }
}

/// Canonical Qt alias.
pub type QSyntaxHighlighter = SyntaxHighlighter;
