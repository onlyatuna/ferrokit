// SPDX-License-Identifier: LGPL-3.0-only
//! Document editing cursor and selection model (`QTextCursor` equivalent).
//!
//! Provides comprehensive text traversal, range selections, deletion, and formatted text insertion.

use crate::text::document::TextDocument;
use crate::text::format::TextCharFormat;
use crate::text::position::{TextPosition, TextRange};
use unicode_segmentation::UnicodeSegmentation;

/// Anchor mode for cursor movement (`QTextCursor::MoveMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MoveMode {
    /// Moves both anchor and position, clearing any active selection.
    #[default]
    MoveAnchor,
    /// Moves only position, extending or shrinking the selection range.
    KeepAnchor,
}

/// Navigation operation for cursor stepping (`QTextCursor::MoveOperation`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveOperation {
    NoMove,
    Start,
    End,
    StartOfLine,
    EndOfLine,
    StartOfBlock,
    EndOfBlock,
    StartOfWord,
    EndOfWord,
    PreviousBlock,
    NextBlock,
    PreviousCharacter,
    NextCharacter,
    PreviousWord,
    NextWord,
    Left,
    Right,
    Up,
    Down,
}

/// Predefined selection modes (`QTextCursor::SelectionType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionType {
    WordUnderCursor,
    LineUnderCursor,
    BlockUnderCursor,
    Document,
}

/// Document editing cursor (`QTextCursor`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TextCursor {
    /// Active cursor position (global grapheme index).
    pub(crate) position: usize,
    /// Selection anchor (global grapheme index).
    pub(crate) anchor: usize,
    /// Default character format applied when typing.
    pub(crate) char_format: TextCharFormat,
}

impl TextCursor {
    /// Creates a cursor at the start of a document.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a cursor at a given grapheme position.
    pub fn at_position(pos: usize) -> Self {
        Self {
            position: pos,
            anchor: pos,
            char_format: TextCharFormat::default(),
        }
    }

    /// Returns the active cursor position in graphemes.
    #[inline]
    pub fn position(&self) -> usize {
        self.position
    }

    /// Returns the selection anchor position in graphemes.
    #[inline]
    pub fn anchor(&self) -> usize {
        self.anchor
    }

    /// Sets the cursor position, optionally keeping the anchor to form a selection.
    pub fn set_position(&mut self, pos: usize, mode: MoveMode) {
        self.position = pos;
        if mode == MoveMode::MoveAnchor {
            self.anchor = pos;
        }
    }

    /// Returns whether there is an active text selection.
    #[inline]
    pub fn has_selection(&self) -> bool {
        self.position != self.anchor
    }

    /// Clears any active selection by snapping anchor to position.
    #[inline]
    pub fn clear_selection(&mut self) {
        self.anchor = self.position;
    }

    /// Returns start of selection in graphemes (`min(anchor, position)`).
    #[inline]
    pub fn selection_start(&self) -> usize {
        self.anchor.min(self.position)
    }

    /// Returns end of selection in graphemes (`max(anchor, position)`).
    #[inline]
    pub fn selection_end(&self) -> usize {
        self.anchor.max(self.position)
    }

    /// Returns the character format for text inserted by this cursor.
    #[inline]
    pub fn char_format(&self) -> &TextCharFormat {
        &self.char_format
    }

    #[inline]
    pub fn set_char_format(&mut self, format: TextCharFormat) {
        self.char_format = format;
    }

    /// Navigates the cursor according to `MoveOperation`.
    pub fn move_position(
        &mut self,
        doc: &TextDocument,
        op: MoveOperation,
        mode: MoveMode,
        n: usize,
    ) -> bool {
        let total_graphemes = doc.character_count();
        let old_pos = self.position;
        let mut new_pos = old_pos;

        for _ in 0..n {
            new_pos = match op {
                MoveOperation::NoMove => new_pos,
                MoveOperation::Start => 0,
                MoveOperation::End => total_graphemes,
                MoveOperation::StartOfBlock => {
                    let block_idx = doc.block_index_at_grapheme(new_pos);
                    doc.block_at(block_idx).map_or(0, |b| b.position)
                }
                MoveOperation::EndOfBlock => {
                    let block_idx = doc.block_index_at_grapheme(new_pos);
                    doc.block_at(block_idx).map_or(total_graphemes, |b| {
                        b.position + b.text().graphemes(true).count()
                    })
                }
                MoveOperation::StartOfLine => {
                    let block_idx = doc.block_index_at_grapheme(new_pos);
                    doc.block_at(block_idx).map_or(0, |b| b.position)
                }
                MoveOperation::StartOfWord => {
                    let full = doc.to_plain_text();
                    let pos = TextPosition::from_grapheme(&full, new_pos);
                    pos.prev_word(&full).grapheme.0
                }
                MoveOperation::EndOfLine => {
                    let block_idx = doc.block_index_at_grapheme(new_pos);
                    doc.block_at(block_idx).map_or(total_graphemes, |b| {
                        b.position + b.text().graphemes(true).count()
                    })
                }
                MoveOperation::EndOfWord => {
                    let full = doc.to_plain_text();
                    let pos = TextPosition::from_grapheme(&full, new_pos);
                    pos.next_word(&full).grapheme.0
                }
                MoveOperation::PreviousBlock => {
                    let block_idx = doc.block_index_at_grapheme(new_pos);
                    if block_idx > 0 {
                        doc.block_at(block_idx - 1).map_or(0, |b| b.position)
                    } else {
                        0
                    }
                }
                MoveOperation::NextBlock => {
                    let block_idx = doc.block_index_at_grapheme(new_pos);
                    if block_idx + 1 < doc.block_count() {
                        doc.block_at(block_idx + 1)
                            .map_or(total_graphemes, |b| b.position)
                    } else {
                        total_graphemes
                    }
                }
                MoveOperation::PreviousCharacter | MoveOperation::Left => new_pos.saturating_sub(1),
                MoveOperation::NextCharacter | MoveOperation::Right => {
                    (new_pos + 1).min(total_graphemes)
                }
                MoveOperation::PreviousWord => {
                    let full = doc.to_plain_text();
                    let pos = TextPosition::from_grapheme(&full, new_pos);
                    pos.prev_word(&full).grapheme.0
                }
                MoveOperation::NextWord => {
                    let full = doc.to_plain_text();
                    let pos = TextPosition::from_grapheme(&full, new_pos);
                    pos.next_word(&full).grapheme.0
                }
                MoveOperation::Up | MoveOperation::Down => {
                    // Without a layout every block is one line: keep the column and move to the
                    // adjacent block, clamped to its length; at the first/last block nothing moves.
                    let block_idx = doc.block_index_at_grapheme(new_pos);
                    let column = doc.block_at(block_idx).map_or(0, |b| new_pos - b.position);
                    let target = if op == MoveOperation::Up {
                        block_idx.checked_sub(1)
                    } else {
                        (block_idx + 1 < doc.block_count()).then_some(block_idx + 1)
                    };
                    match target.and_then(|i| doc.block_at(i)) {
                        Some(b) => b.position + column.min(b.text().graphemes(true).count()),
                        None => new_pos,
                    }
                }
            };
        }

        self.set_position(new_pos, mode);
        self.position != old_pos
    }

    /// Selects text under cursor according to `SelectionType`.
    pub fn select(&mut self, doc: &TextDocument, selection: SelectionType) {
        match selection {
            SelectionType::Document => {
                self.anchor = 0;
                self.position = doc.character_count();
            }
            SelectionType::BlockUnderCursor => {
                let block_idx = doc.block_index_at_grapheme(self.position);
                if let Some(b) = doc.block_at(block_idx) {
                    self.anchor = b.position;
                    self.position = b.position + b.text().graphemes(true).count();
                }
            }
            SelectionType::WordUnderCursor => {
                let full = doc.to_plain_text();
                let pos = TextPosition::from_grapheme(&full, self.position);
                self.anchor = pos.prev_word(&full).grapheme.0;
                self.position = pos.next_word(&full).grapheme.0;
            }
            SelectionType::LineUnderCursor => {
                self.select(doc, SelectionType::BlockUnderCursor);
            }
        }
    }

    /// Returns plain text of the current selection.
    pub fn selected_text(&self, doc: &TextDocument) -> String {
        if !self.has_selection() {
            return String::new();
        }
        let full = doc.to_plain_text();
        let s = self.selection_start();
        let e = self.selection_end();
        let range = TextRange::from_graphemes(&full, s, e);
        range.slice_str(&full).to_string()
    }

    /// Deletes character forward (Delete key).
    pub fn delete_char(&mut self, doc: &mut TextDocument) {
        if self.has_selection() {
            self.remove_selected_text(doc);
        } else {
            let pos = self.position;
            doc.remove_graphemes(pos, 1);
        }
    }

    /// Deletes character backward (Backspace key).
    pub fn delete_previous_char(&mut self, doc: &mut TextDocument) {
        if self.has_selection() {
            self.remove_selected_text(doc);
        } else if self.position > 0 {
            self.position -= 1;
            self.anchor = self.position;
            doc.remove_graphemes(self.position, 1);
        }
    }

    /// Removes currently selected text.
    pub fn remove_selected_text(&mut self, doc: &mut TextDocument) {
        if !self.has_selection() {
            return;
        }
        let s = self.selection_start();
        let count = self.selection_end() - s;
        doc.remove_graphemes(s, count);
        self.position = s;
        self.anchor = s;
    }

    /// Inserts plain text at cursor position.
    pub fn insert_text(&mut self, doc: &mut TextDocument, text: &str) {
        let fmt = self.char_format.clone();
        self.insert_text_with_format(doc, text, &fmt);
    }

    /// Inserts text with formatting at cursor position.
    pub fn insert_text_with_format(
        &mut self,
        doc: &mut TextDocument,
        text: &str,
        format: &TextCharFormat,
    ) {
        if self.has_selection() {
            self.remove_selected_text(doc);
        }
        let inserted_graphemes = text.graphemes(true).count();
        doc.insert_formatted_text(self.position, text, format);
        self.position += inserted_graphemes;
        self.anchor = self.position;
    }

    /// Inserts a new paragraph block at cursor position.
    pub fn insert_block(&mut self, doc: &mut TextDocument) {
        self.insert_text(doc, "\n");
    }
}

#[cfg(test)]
mod line_move_tests {
    use super::*;

    fn doc(text: &str) -> TextDocument {
        let mut d = TextDocument::new();
        d.set_plain_text(text);
        d
    }

    fn mv(d: &TextDocument, from: usize, op: MoveOperation) -> usize {
        let mut c = TextCursor::at_position(from);
        c.move_position(d, op, MoveMode::MoveAnchor, 1);
        c.position()
    }

    #[test]
    fn home_and_end_stay_on_the_line_not_the_word() {
        let d = doc("foo bar baz");
        assert_eq!(mv(&d, 11, MoveOperation::StartOfLine), 0);
        assert_eq!(mv(&d, 5, MoveOperation::StartOfLine), 0);
        assert_eq!(mv(&d, 0, MoveOperation::EndOfLine), 11);
        assert_eq!(mv(&d, 5, MoveOperation::EndOfLine), 11);
    }

    #[test]
    fn home_and_end_respect_block_boundaries() {
        let d = doc("ab\ncdef");
        // Positions: a0 b1 \n2 c3 d4 e5 f6 end7.
        assert_eq!(mv(&d, 5, MoveOperation::StartOfLine), 3);
        assert_eq!(mv(&d, 3, MoveOperation::EndOfLine), 7);
        assert_eq!(mv(&d, 1, MoveOperation::EndOfLine), 2);
    }

    #[test]
    fn up_down_keep_the_column_and_clamp() {
        let d = doc("abcdef\nxy\nlonger text");
        // Blocks start at 0, 7, 10.
        assert_eq!(mv(&d, 4, MoveOperation::Down), 9); // clamped to end of "xy"
        assert_eq!(mv(&d, 8, MoveOperation::Down), 10 + 1);
        assert_eq!(mv(&d, 12, MoveOperation::Up), 9); // column 2 of "xy"
        assert_eq!(mv(&d, 8, MoveOperation::Up), 1);
    }

    #[test]
    fn up_on_first_and_down_on_last_block_do_not_move() {
        let d = doc("abc\ndef");
        assert_eq!(mv(&d, 2, MoveOperation::Up), 2);
        assert_eq!(mv(&d, 6, MoveOperation::Down), 6);
    }
}

/// Canonical Qt alias.
pub type QTextCursor = TextCursor;
