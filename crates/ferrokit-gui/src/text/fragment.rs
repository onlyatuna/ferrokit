// SPDX-License-Identifier: LGPL-3.0-only
//! Continuous run of text sharing uniform character formatting (`QTextFragment` equivalent).

use crate::text::format::TextCharFormat;

/// Continuous run of text in a document paragraph (`QTextFragment`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TextFragment {
    pub(crate) text: String,
    pub(crate) format: TextCharFormat,
}

impl TextFragment {
    /// Creates a new text fragment with formatting.
    pub fn new(text: impl Into<String>, format: TextCharFormat) -> Self {
        Self {
            text: text.into(),
            format,
        }
    }

    /// Creates an unstyled plain text fragment.
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            format: TextCharFormat::default(),
        }
    }

    /// Returns the text content of the fragment.
    #[inline]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns the character format of this fragment.
    #[inline]
    pub fn char_format(&self) -> &TextCharFormat {
        &self.format
    }

    /// Sets the character format of this fragment.
    #[inline]
    pub fn set_char_format(&mut self, format: TextCharFormat) {
        self.format = format;
    }

    /// Returns the UTF-8 byte length of the fragment.
    #[inline]
    pub fn length(&self) -> usize {
        self.text.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }
}

/// Canonical Qt alias.
pub type QTextFragment = TextFragment;
