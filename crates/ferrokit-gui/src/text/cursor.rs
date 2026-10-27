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
    /// Target column of repeated `Up`/`Down` (`QTextCursorPrivate::x`).
    pub(crate) sticky: StickyColumn,
}

/// Target column kept across consecutive vertical moves: the x coordinate on a wrapping document,
/// the grapheme column otherwise. It is only honoured while the cursor still sits where the last
/// vertical move left it, and every other cursor operation clears it. Compares equal always so
/// cursor equality stays position/anchor/format, as `QTextCursor::operator==`.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct StickyColumn(Option<(usize, Target)>);

#[derive(Debug, Clone, Copy)]
enum Target {
    X(f32),
    Column(usize),
}

impl PartialEq for StickyColumn {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl StickyColumn {
    fn x_at(self, pos: usize) -> Option<f32> {
        match self.0 {
            Some((p, Target::X(x))) if p == pos => Some(x),
            _ => None,
        }
    }

    fn column_at(self, pos: usize) -> Option<usize> {
        match self.0 {
            Some((p, Target::Column(c))) if p == pos => Some(c),
            _ => None,
        }
    }
}

/// Whether the document wraps blocks into visual lines (`QTextDocument::setTextWidth`).
fn wraps(doc: &TextDocument) -> bool {
    doc.text_width() >= 0.0
}

/// `StartOfLine` / `EndOfLine` (`QTextCursor::movePosition`): the edge of the visual line holding
/// `pos`. Without a text width every block is a single line, so this is the block edge.
fn visual_line_edge(doc: &TextDocument, pos: usize, end: bool) -> usize {
    let block_idx = doc.block_index_at_grapheme(pos);
    let Some(b) = doc.block_at(block_idx) else {
        return if end { doc.character_count() } else { 0 };
    };
    let block_len = b.text().graphemes(true).count();
    if !wraps(doc) {
        return b.position + if end { block_len } else { 0 };
    }
    let layout = doc.block_layout(b);
    let Some(line) = layout.line_for_text_position(pos.saturating_sub(b.position)) else {
        return b.position + if end { block_len } else { 0 };
    };
    if !end {
        return b.position + line.text_start();
    }
    let mut local = line.text_start() + line.text_length();
    // A wrapped (non-last) line ends before the space that caused the break.
    if local > 0 && line.line_number() + 1 < layout.line_count() {
        if let Some(g) = b.text().graphemes(true).nth(local - 1) {
            if g.chars().all(char::is_whitespace) {
                local -= 1;
            }
        }
    }
    b.position + local.min(block_len)
}

/// `Up` / `Down` over visual lines, keeping the x coordinate; crosses into the adjacent block's
/// last/first visual line at a block edge. `None` when the document has no wrap width; at the
/// document's first/last visual line it stays put and keeps the x.
fn visual_line_step(
    doc: &TextDocument,
    pos: usize,
    up: bool,
    sticky_x: Option<f32>,
) -> Option<(usize, f32)> {
    use crate::text::line::{CursorPosition, Edge};
    if !wraps(doc) {
        return None;
    }
    let block_idx = doc.block_index_at_grapheme(pos);
    let b = doc.block_at(block_idx)?;
    let layout = doc.block_layout(b);
    let local = pos.checked_sub(b.position)?;
    let line = layout.line_for_text_position(local)?;
    let x = sticky_x.unwrap_or_else(|| line.cursor_to_x(local, Edge::Leading));
    let n = line.line_number();
    // At the first/last visual line of the document nothing moves but the x column is kept.
    let stay = Some((pos, x));
    let (target_block, target_layout, target_n) = if up {
        if n > 0 {
            (b, layout, n - 1)
        } else {
            let Some(pb) = block_idx.checked_sub(1).and_then(|i| doc.block_at(i)) else {
                return stay;
            };
            let pl = doc.block_layout(pb);
            let Some(last) = pl.line_count().checked_sub(1) else {
                return stay;
            };
            (pb, pl, last)
        }
    } else if n + 1 < layout.line_count() {
        (b, layout, n + 1)
    } else {
        let Some(nb) = doc.block_at(block_idx + 1) else {
            return stay;
        };
        (nb, doc.block_layout(nb), 0)
    };
    let Some(target_line) = target_layout.line_at(target_n) else {
        return stay;
    };
    let mut col = target_line.x_to_cursor(x, CursorPosition::CursorBetweenCharacters);
    let end = target_line.text_start() + target_line.text_length();
    // On a wrapped line the end position is the next line's start: stay on this line.
    if col >= end && target_n + 1 < target_layout.line_count() && target_line.text_length() > 0 {
        col = end - 1;
    }
    Some((target_block.position + col, x))
}

/// `Up` / `Down` without a layout: every block is one line, so keep the column and move to the
/// adjacent block, clamped to its length; at the first/last block nothing moves. Returns the new
/// position and the column to keep.
fn block_line_step(
    doc: &TextDocument,
    pos: usize,
    up: bool,
    sticky_col: Option<usize>,
) -> (usize, usize) {
    let block_idx = doc.block_index_at_grapheme(pos);
    let column =
        sticky_col.unwrap_or_else(|| doc.block_at(block_idx).map_or(0, |b| pos - b.position));
    let target = if up {
        block_idx.checked_sub(1)
    } else {
        (block_idx + 1 < doc.block_count()).then_some(block_idx + 1)
    };
    match target.and_then(|i| doc.block_at(i)) {
        Some(b) => (b.position + column.min(b.text().graphemes(true).count()), column),
        None => (pos, column),
    }
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
            sticky: StickyColumn::default(),
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
        self.sticky = StickyColumn::default();
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
        let mut sticky = self.sticky;
        let mut vertical = false;

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
                MoveOperation::StartOfLine => visual_line_edge(doc, new_pos, false),
                MoveOperation::StartOfWord => {
                    let full = doc.to_plain_text();
                    let pos = TextPosition::from_grapheme(&full, new_pos);
                    pos.prev_word(&full).grapheme.0
                }
                MoveOperation::EndOfLine => visual_line_edge(doc, new_pos, true),
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
                    let up = op == MoveOperation::Up;
                    vertical = true;
                    let x = sticky.x_at(new_pos);
                    let (next, target) = match visual_line_step(doc, new_pos, up, x) {
                        Some((p, x)) => (p, Target::X(x)),
                        None => {
                            let col = sticky.column_at(new_pos);
                            let (p, c) = block_line_step(doc, new_pos, up, col);
                            (p, Target::Column(c))
                        }
                    };
                    sticky = StickyColumn(Some((next, target)));
                    next
                }
            };
        }

        self.set_position(new_pos, mode);
        if vertical {
            self.sticky = sticky;
        }
        self.position != old_pos
    }

    /// Selects text under cursor according to `SelectionType`.
    pub fn select(&mut self, doc: &TextDocument, selection: SelectionType) {
        self.sticky = StickyColumn::default();
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
        self.sticky = StickyColumn::default();
        if self.has_selection() {
            self.remove_selected_text(doc);
        } else {
            let pos = self.position;
            doc.remove_graphemes(pos, 1);
        }
    }

    /// Deletes character backward (Backspace key).
    pub fn delete_previous_char(&mut self, doc: &mut TextDocument) {
        self.sticky = StickyColumn::default();
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
        self.sticky = StickyColumn::default();
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
        self.sticky = StickyColumn::default();
        if self.has_selection() {
            self.remove_selected_text(doc);
        }
        // The document reports where the inserted text ends: a combining mark merges into the
        // previous cluster, and a following mark merges into the inserted one.
        self.position = doc.insert_formatted_text_end(self.position, text, format);
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
