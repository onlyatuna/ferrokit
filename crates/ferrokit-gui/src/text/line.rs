// SPDX-License-Identifier: LGPL-3.0-only
//! Single rendered line inside a text layout (`QTextLine` equivalent).
//!
//! Provides geometric bounds, baseline metrics, and bidirectional cursor-to-coordinate mapping.

use crate::geometry::primitives::{PointF, RectF};

/// Edge of a character or grapheme cluster for cursor positioning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Edge {
    #[default]
    Leading,
    Trailing,
}

/// Mode for mapping an X coordinate back to a cursor position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorPosition {
    #[default]
    CursorBetweenCharacters,
    CursorOnCharacter,
}

/// A laid-out line of text (`QTextLine`).
#[derive(Debug, Clone, PartialEq)]
pub struct TextLine {
    /// Line index within the parent `TextLayout`.
    pub(crate) index: usize,
    /// Starting grapheme offset in the layout text.
    pub(crate) text_start: usize,
    /// Number of grapheme clusters in this line.
    pub(crate) text_length: usize,
    /// Starting UTF-8 byte offset in layout text.
    pub(crate) byte_start: usize,
    /// Byte length of this line.
    pub(crate) byte_length: usize,
    /// Top-left position of the line.
    pub(crate) position: PointF,
    /// Allocated line width.
    pub(crate) width: f32,
    /// Line total height (`ascent + descent + leading`).
    pub(crate) height: f32,
    /// Font ascent (distance from top to baseline).
    pub(crate) ascent: f32,
    /// Font descent (distance from baseline to bottom).
    pub(crate) descent: f32,
    /// Inter-line leading gap.
    pub(crate) leading: f32,
    /// Natural advance width of the text glyphs on this line (`QTextLine::naturalTextWidth`): the
    /// spaces at the end of a wrapped line are not part of it.
    pub(crate) natural_width: f32,
    /// The x that [`cursor_to_x`](Self::cursor_to_x) never exceeds: the right end of the wrap
    /// width, past which the spaces that hang over it are not shown (Qt clamps to the line width).
    pub(crate) right_limit: f32,
    /// Cumulative horizontal offsets for each grapheme in this line.
    /// Length is `text_length + 1` (ending with `natural_width`).
    pub(crate) grapheme_x_advances: Vec<f32>,
}

impl Default for TextLine {
    fn default() -> Self {
        Self {
            index: 0,
            text_start: 0,
            text_length: 0,
            byte_start: 0,
            byte_length: 0,
            position: PointF::new(0.0, 0.0),
            width: 0.0,
            height: 0.0,
            ascent: 0.0,
            descent: 0.0,
            leading: 0.0,
            natural_width: 0.0,
            right_limit: f32::INFINITY,
            grapheme_x_advances: vec![0.0],
        }
    }
}

impl TextLine {
    /// Returns the line index.
    #[inline]
    pub fn line_number(&self) -> usize {
        self.index
    }

    /// Returns whether this line is valid and non-empty.
    #[inline]
    pub fn is_valid(&self) -> bool {
        self.height > 0.0
    }

    /// Returns the top-left position of this line.
    #[inline]
    pub fn position(&self) -> PointF {
        self.position
    }

    /// Sets the top-left position of this line.
    #[inline]
    pub fn set_position(&mut self, pos: PointF) {
        self.position = pos;
    }

    /// Returns the bounding rectangle of the line.
    #[inline]
    pub fn rect(&self) -> RectF {
        RectF::new(self.position.x, self.position.y, self.width, self.height)
    }

    /// Returns the tight rectangle enclosing only the natural text.
    #[inline]
    pub fn natural_text_rect(&self) -> RectF {
        RectF::new(
            self.position.x,
            self.position.y,
            self.natural_width,
            self.height,
        )
    }

    #[inline]
    pub fn x(&self) -> f32 {
        self.position.x
    }

    #[inline]
    pub fn y(&self) -> f32 {
        self.position.y
    }

    #[inline]
    pub fn width(&self) -> f32 {
        self.width
    }

    #[inline]
    pub fn height(&self) -> f32 {
        self.height
    }

    #[inline]
    pub fn ascent(&self) -> f32 {
        self.ascent
    }

    #[inline]
    pub fn descent(&self) -> f32 {
        self.descent
    }

    #[inline]
    pub fn leading(&self) -> f32 {
        self.leading
    }

    #[inline]
    pub fn natural_text_width(&self) -> f32 {
        self.natural_width
    }

    #[inline]
    pub fn set_line_width(&mut self, width: f32) {
        self.width = width;
    }

    /// Returns starting grapheme cluster offset in the document / layout.
    #[inline]
    pub fn text_start(&self) -> usize {
        self.text_start
    }

    /// Returns number of grapheme clusters in this line.
    #[inline]
    pub fn text_length(&self) -> usize {
        self.text_length
    }

    /// Returns starting UTF-8 byte offset in layout text.
    #[inline]
    pub fn byte_start(&self) -> usize {
        self.byte_start
    }

    /// Returns byte length in layout text.
    #[inline]
    pub fn byte_length(&self) -> usize {
        self.byte_length
    }

    /// Converts a grapheme cursor position into an absolute X coordinate (`cursorToX`).
    ///
    /// Accepts either local line offset (0..text_length) or layout-global grapheme offset.
    pub fn cursor_to_x(&self, mut cursor_pos: usize, edge: Edge) -> f32 {
        if cursor_pos >= self.text_start && cursor_pos <= self.text_start + self.text_length {
            cursor_pos -= self.text_start;
        }

        let local_pos = cursor_pos.min(self.text_length);
        let base_x = if local_pos < self.grapheme_x_advances.len() {
            self.grapheme_x_advances[local_pos]
        } else {
            self.natural_width
        };

        let offset = match edge {
            Edge::Leading => 0.0,
            Edge::Trailing => {
                if local_pos + 1 < self.grapheme_x_advances.len() {
                    self.grapheme_x_advances[local_pos + 1] - base_x
                } else {
                    0.0
                }
            }
        };

        (self.position.x + base_x + offset).min(self.right_limit)
    }

    /// Maps an X coordinate back to a grapheme cursor position (`xToCursor`).
    ///
    /// Returns the global grapheme index within the layout text.
    pub fn x_to_cursor(&self, x: f32, mode: CursorPosition) -> usize {
        let rel_x = x - self.position.x;
        if rel_x <= 0.0 || self.text_length == 0 {
            return self.text_start;
        }
        // The end of the line is where its last grapheme ends, trailing spaces included.
        let end_x = self.grapheme_x_advances.last().copied().unwrap_or(0.0);
        if rel_x >= end_x {
            return self.text_start + self.text_length;
        }

        for i in 0..self.text_length {
            let left_x = self.grapheme_x_advances.get(i).copied().unwrap_or(0.0);
            let right_x = self
                .grapheme_x_advances
                .get(i + 1)
                .copied()
                .unwrap_or(end_x);

            if rel_x >= left_x && rel_x < right_x {
                return match mode {
                    CursorPosition::CursorBetweenCharacters => {
                        let mid = (left_x + right_x) * 0.5;
                        if rel_x >= mid {
                            self.text_start + i + 1
                        } else {
                            self.text_start + i
                        }
                    }
                    CursorPosition::CursorOnCharacter => self.text_start + i,
                };
            }
        }

        self.text_start + self.text_length
    }
}

/// Canonical Qt alias.
pub type QTextLine = TextLine;
