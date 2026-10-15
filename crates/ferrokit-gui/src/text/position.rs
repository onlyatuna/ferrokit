// SPDX-License-Identifier: LGPL-3.0-only
//! Formal text coordinate and positioning model.
//!
//! Replaces ambiguous `usize` indices with strongly-typed:
//! - `GraphemeIndex`: User-perceived grapheme clusters (Extended Grapheme Clusters, UAX #29).
//!   Prevents splitting multi-codepoint emoji (e.g. 👨‍👩‍👧‍👦), flags (e.g. 🇨🇦), and combining characters (e.g. é).
//! - `ByteOffset`: UTF-8 byte position, guaranteed to be on a valid char boundary.
//! - `Utf16Offset`: UTF-16 code unit offset (Qt `QString` / Windows IME / macOS Cocoa parity).
//! - `TextPosition`: Unified multidimensional text coordinate.
//! - `TextRange`: Directed selection range (`[anchor..position]`).

use std::fmt;
use std::ops::{Deref, Range};
use unicode_segmentation::UnicodeSegmentation;

/// A 0-based index representing an Extended Grapheme Cluster (user-perceived character).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct GraphemeIndex(pub usize);

impl Deref for GraphemeIndex {
    type Target = usize;
    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<usize> for GraphemeIndex {
    #[inline]
    fn from(val: usize) -> Self {
        Self(val)
    }
}

impl fmt::Display for GraphemeIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Grapheme({})", self.0)
    }
}
impl PartialEq<usize> for GraphemeIndex {
    #[inline]
    fn eq(&self, other: &usize) -> bool {
        self.0 == *other
    }
}

impl PartialEq<GraphemeIndex> for usize {
    #[inline]
    fn eq(&self, other: &GraphemeIndex) -> bool {
        *self == other.0
    }
}

/// A 0-based UTF-8 byte offset within a string, on a Unicode scalar boundary.
///
/// `ByteOffset` values produced by `TextPosition` are additionally grapheme boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ByteOffset(pub usize);

impl Deref for ByteOffset {
    type Target = usize;
    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<usize> for ByteOffset {
    #[inline]
    fn from(val: usize) -> Self {
        Self(val)
    }
}

impl fmt::Display for ByteOffset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Byte({})", self.0)
    }
}
impl PartialEq<usize> for ByteOffset {
    #[inline]
    fn eq(&self, other: &usize) -> bool {
        self.0 == *other
    }
}

impl PartialEq<ByteOffset> for usize {
    #[inline]
    fn eq(&self, other: &ByteOffset) -> bool {
        *self == other.0
    }
}

/// A 0-based UTF-16 code unit offset (Qt `QString` / IME index).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Utf16Offset(pub usize);

impl Deref for Utf16Offset {
    type Target = usize;
    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<usize> for Utf16Offset {
    #[inline]
    fn from(val: usize) -> Self {
        Self(val)
    }
}

impl fmt::Display for Utf16Offset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Utf16({})", self.0)
    }
}
impl PartialEq<usize> for Utf16Offset {
    #[inline]
    fn eq(&self, other: &usize) -> bool {
        self.0 == *other
    }
}

impl PartialEq<Utf16Offset> for usize {
    #[inline]
    fn eq(&self, other: &Utf16Offset) -> bool {
        *self == other.0
    }
}

/// A unified, exact multidimensional text coordinate within a given string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TextPosition {
    /// Extended grapheme-cluster boundary index (user-perceived character).
    pub grapheme: GraphemeIndex,
    /// UTF-8 byte offset at the corresponding grapheme boundary.
    pub byte: ByteOffset,
    /// UTF-16 code-unit offset at the corresponding grapheme boundary.
    pub utf16: Utf16Offset,
}

impl TextPosition {
    /// Creates a text position at index 0.
    #[inline]
    pub const fn zero() -> Self {
        Self {
            grapheme: GraphemeIndex(0),
            byte: ByteOffset(0),
            utf16: Utf16Offset(0),
        }
    }

    /// Resolves a grapheme-cluster boundary index. Indices beyond the text clamp to its end.
    pub fn from_grapheme(text: &str, grapheme_target: usize) -> Self {
        let mut curr_grapheme = 0;
        let mut curr_byte = 0;
        let mut curr_u16 = 0;

        for (byte_offset, grapheme_str) in text.grapheme_indices(true) {
            if curr_grapheme >= grapheme_target {
                return Self {
                    grapheme: GraphemeIndex(curr_grapheme),
                    byte: ByteOffset(byte_offset),
                    utf16: Utf16Offset(curr_u16),
                };
            }
            curr_grapheme += 1;
            curr_byte = byte_offset + grapheme_str.len();
            curr_u16 += grapheme_str.encode_utf16().count();
        }

        Self {
            grapheme: GraphemeIndex(curr_grapheme),
            byte: ByteOffset(curr_byte),
            utf16: Utf16Offset(curr_u16),
        }
    }

    /// Resolves a UTF-8 byte offset to its containing grapheme boundary.
    ///
    /// Offsets beyond the text clamp to the end. Offsets inside a scalar or inside a
    /// multi-scalar grapheme cluster snap backward to that cluster's start.
    pub fn from_byte(text: &str, byte_target: usize) -> Self {
        let byte_target = byte_target.min(text.len());
        let mut curr_grapheme = 0;
        let mut curr_u16 = 0;

        for (byte_offset, grapheme_str) in text.grapheme_indices(true) {
            let next_byte = byte_offset + grapheme_str.len();
            if byte_target <= byte_offset {
                return Self {
                    grapheme: GraphemeIndex(curr_grapheme),
                    byte: ByteOffset(byte_offset),
                    utf16: Utf16Offset(curr_u16),
                };
            }
            if byte_target < next_byte {
                // Snap to beginning of this grapheme
                return Self {
                    grapheme: GraphemeIndex(curr_grapheme),
                    byte: ByteOffset(byte_offset),
                    utf16: Utf16Offset(curr_u16),
                };
            }
            curr_grapheme += 1;
            curr_u16 += grapheme_str.encode_utf16().count();
        }

        Self {
            grapheme: GraphemeIndex(curr_grapheme),
            byte: ByteOffset(text.len()),
            utf16: Utf16Offset(curr_u16),
        }
    }

    /// Resolves a UTF-16 code-unit offset to its containing grapheme boundary.
    ///
    /// Offsets beyond the text clamp to the end. Offsets inside a surrogate pair or
    /// inside a multi-scalar grapheme cluster snap backward to that cluster's start.
    pub fn from_utf16(text: &str, utf16_target: usize) -> Self {
        let mut curr_grapheme = 0;
        let mut curr_byte = 0;
        let mut curr_u16 = 0;

        for (byte_offset, grapheme_str) in text.grapheme_indices(true) {
            let g_u16 = grapheme_str.encode_utf16().count();
            if curr_u16 + g_u16 > utf16_target {
                return Self {
                    grapheme: GraphemeIndex(curr_grapheme),
                    byte: ByteOffset(byte_offset),
                    utf16: Utf16Offset(curr_u16),
                };
            }
            curr_grapheme += 1;
            curr_byte = byte_offset + grapheme_str.len();
            curr_u16 += g_u16;
        }

        Self {
            grapheme: GraphemeIndex(curr_grapheme),
            byte: ByteOffset(curr_byte),
            utf16: Utf16Offset(curr_u16),
        }
    }

    /// Returns a text position positioned at the end of the text.
    pub fn end_of(text: &str) -> Self {
        Self::from_grapheme(text, usize::MAX)
    }

    /// Checks if this position is at the very beginning of the string.
    #[inline]
    pub fn is_at_start(&self) -> bool {
        self.grapheme.0 == 0
    }

    /// Checks if this position is at or beyond the end of the string.
    #[inline]
    pub fn is_at_end(&self, text: &str) -> bool {
        self.byte.0 >= text.len()
    }

    /// Moves one grapheme cluster backwards.
    pub fn prev_grapheme(&self, text: &str) -> Self {
        if self.grapheme.0 == 0 {
            *self
        } else {
            Self::from_grapheme(text, self.grapheme.0 - 1)
        }
    }

    /// Moves one grapheme cluster forwards.
    pub fn next_grapheme(&self, text: &str) -> Self {
        if self.is_at_end(text) {
            *self
        } else {
            Self::from_grapheme(text, self.grapheme.0 + 1)
        }
    }

    /// Moves backwards by a word boundary.
    pub fn prev_word(&self, text: &str) -> Self {
        let position = Self::from_byte(text, self.byte.0);
        if position.is_at_start() {
            return position;
        }
        let byte_pos = position.byte.0;
        let prefix = &text[..byte_pos];
        let mut last_boundary = 0;

        for (offset, _) in prefix.split_word_bound_indices() {
            if offset < byte_pos {
                last_boundary = offset;
            }
        }
        Self::from_byte(text, last_boundary)
    }

    /// Moves forwards by a word boundary.
    pub fn next_word(&self, text: &str) -> Self {
        let position = Self::from_byte(text, self.byte.0);
        if position.is_at_end(text) {
            return position;
        }
        let byte_pos = position.byte.0;
        for (offset, _) in text.split_word_bound_indices() {
            if offset > byte_pos {
                return Self::from_byte(text, offset);
            }
        }
        Self::end_of(text)
    }
}

/// A directed range of text between an `anchor` and the active cursor `position`.
///
/// Preserves the selection direction (e.g. forward selection vs reverse selection).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TextRange {
    /// The fixed pivot where selection started.
    pub anchor: TextPosition,
    /// The moving head of the selection.
    pub position: TextPosition,
}

impl TextRange {
    /// Creates a new directed text range.
    #[inline]
    pub const fn new(anchor: TextPosition, position: TextPosition) -> Self {
        Self { anchor, position }
    }

    /// Creates a collapsed range (cursor without active selection).
    #[inline]
    pub const fn collapsed(pos: TextPosition) -> Self {
        Self {
            anchor: pos,
            position: pos,
        }
    }

    /// Creates a text range from grapheme start and end indices.
    pub fn from_graphemes(text: &str, start_grapheme: usize, end_grapheme: usize) -> Self {
        Self {
            anchor: TextPosition::from_grapheme(text, start_grapheme),
            position: TextPosition::from_grapheme(text, end_grapheme),
        }
    }

    /// Returns whether the range is collapsed (anchor == position).
    #[inline]
    pub fn is_collapsed(&self) -> bool {
        self.anchor.byte.0 == self.position.byte.0
    }

    /// Returns whether the selection direction is backwards (position < anchor).
    #[inline]
    pub fn is_reversed(&self) -> bool {
        self.position < self.anchor
    }

    /// Returns the normalized beginning of the range (`min(anchor, position)`).
    #[inline]
    pub fn start(&self) -> TextPosition {
        if self.position < self.anchor {
            self.position
        } else {
            self.anchor
        }
    }

    /// Returns the normalized end of the range (`max(anchor, position)`).
    #[inline]
    pub fn end(&self) -> TextPosition {
        if self.position > self.anchor {
            self.position
        } else {
            self.anchor
        }
    }

    /// Returns the byte range suitable for `&str[start..end]`.
    #[inline]
    pub fn byte_range(&self) -> Range<usize> {
        let s = self.start().byte.0;
        let e = self.end().byte.0;
        s..e
    }

    /// Slices `text` using this range's byte positions.
    ///
    /// Positions are normally constructed for the same text. If a position is out of
    /// bounds or is not a scalar boundary for `text`, each endpoint is clamped backward
    /// to a valid scalar boundary; reversed-after-clamping ranges return an empty slice.
    #[inline]
    pub fn slice_str<'a>(&self, text: &'a str) -> &'a str {
        let range = self.byte_range();
        let mut start = range.start.min(text.len());
        let mut end = range.end.min(text.len());
        while !text.is_char_boundary(start) {
            start -= 1;
        }
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        if start > end {
            start = end;
        }
        &text[start..end]
    }

    /// Returns the number of grapheme clusters spanned by this range.
    #[inline]
    pub fn grapheme_len(&self) -> usize {
        self.end()
            .grapheme
            .0
            .saturating_sub(self.start().grapheme.0)
    }

    /// Returns the number of UTF-8 bytes spanned by this range.
    #[inline]
    pub fn byte_len(&self) -> usize {
        self.end().byte.0.saturating_sub(self.start().byte.0)
    }

    /// Returns the number of UTF-16 code units spanned by this range.
    #[inline]
    pub fn utf16_len(&self) -> usize {
        self.end().utf16.0.saturating_sub(self.start().utf16.0)
    }
}

// ---------------------------------------------------------------------------
// Convenience free functions
// ---------------------------------------------------------------------------

/// Computes the total number of extended grapheme clusters in `s`.
#[inline]
pub fn grapheme_count(s: &str) -> usize {
    s.graphemes(true).count()
}

/// Converts a grapheme-cluster boundary index to a UTF-8 byte offset.
/// Indices beyond the text clamp to its end.
#[inline]
pub fn grapheme_to_byte(s: &str, grapheme_idx: usize) -> usize {
    TextPosition::from_grapheme(s, grapheme_idx).byte.0
}

/// Converts a UTF-8 byte offset to its containing grapheme-cluster index.
/// Out-of-range offsets clamp to the end; offsets inside a cluster snap backward.
#[inline]
pub fn byte_to_grapheme(s: &str, byte_offset: usize) -> usize {
    TextPosition::from_byte(s, byte_offset).grapheme.0
}

/// Converts a UTF-16 code-unit offset to the UTF-8 byte offset of its containing
/// grapheme-cluster boundary. Out-of-range offsets clamp to the end; offsets inside
/// a surrogate pair or grapheme cluster snap backward.
#[inline]
pub fn utf16_to_byte(s: &str, u16_offset: usize) -> usize {
    TextPosition::from_utf16(s, u16_offset).byte.0
}

/// Converts a UTF-8 byte offset to the UTF-16 code-unit offset at its containing
/// grapheme-cluster boundary. Out-of-range offsets clamp to the end; offsets inside
/// a scalar or grapheme cluster snap backward.
#[inline]
pub fn byte_to_utf16(s: &str, byte_offset: usize) -> usize {
    TextPosition::from_byte(s, byte_offset).utf16.0
}
