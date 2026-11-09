// SPDX-License-Identifier: LGPL-3.0-only
//! One line of text itemized, shaped and laid out as `QTextEngine` does for
//! `QPainter::drawText(QPointF, QString)` and `QFontMetricsF::horizontalAdvance` (qpainter.cpp:5698,
//! qfontmetrics.cpp:1388, qtextengine.cpp: `itemize`, `shapeLine`, `width`, `calculateTabWidth`,
//! `bidiReorder`).
//!
//! The text is cut into items: runs of one bidi embedding level, each tab on its own (a tab has no
//! glyph; its width is the distance to the next tab stop of the pen position reached by the items
//! before it **in logical order**), and the characters that are never shown (line feeds, carriage
//! returns and the line and paragraph separators, which have no width). A run is shaped in the
//! direction of its level, so its glyphs come out in the order they are shown. The advance of the
//! line is the sum of the widths of the items in logical order; drawing puts the items from the
//! left in the visual order of their levels, each as wide as its width.

use crate::application::LayoutDirection;
use crate::text::bidi;
use crate::text::font::Font;
use crate::text::font_database::resolve_font_engines_for_text_global;
use crate::text::font_metrics::FontMetrics;
use crate::text::glyph_layout::{FontEngine, GlyphLayout, PositionedGlyph};
use crate::text::layout::{is_unshaped, tab_width};
use unicode_segmentation::UnicodeSegmentation;

/// The part of `QTextOption` that decides how one line is itemized and measured.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextOption {
    /// `QTextOption::textDirection`: the base direction of the text. `LayoutDirectionAuto` takes
    /// it from the first strong character outside isolates (`QString::isRightToLeft`).
    pub text_direction: LayoutDirection,
    /// `QTextOption::tabStopDistance`: a tab advances to the next multiple of it, in pixels; zero
    /// or less (Qt's own default is -1) means 80.
    pub tab_stop_distance: f32,
}

impl Default for TextOption {
    fn default() -> Self {
        Self {
            text_direction: LayoutDirection::LayoutDirectionAuto,
            tab_stop_distance: -1.0,
        }
    }
}

/// What an item draws.
pub(crate) enum ItemContent {
    /// Shaped glyphs, positioned from the left edge of the item (the glyphs of a right to left item
    /// are in the order they are shown), and the font engines their `font_index` refers to.
    Glyphs {
        engines: Vec<FontEngine>,
        glyphs: Vec<PositionedGlyph>,
    },
    /// A tab, a character that is not shown, or text no font shapes: nothing to draw.
    Blank,
}

/// A `QScriptItem`: a stretch of the line at one embedding level.
pub(crate) struct Item {
    pub level: u8,
    /// `QScriptItem::width`.
    pub width: f32,
    pub content: ItemContent,
}

/// The items of one line in logical order.
pub(crate) struct LineItems {
    items: Vec<Item>,
}

/// `QFixed::fromReal`: Qt keeps a pen position in 1/64 px and cuts what is finer off.
pub(crate) fn qfixed_from_real(value: f32) -> f32 {
    (f64::from(value) * 64.0).trunc() as f32 / 64.0
}

impl LineItems {
    /// Itemizes and shapes `text` (`QTextEngine::itemize`, `shapeLine`).
    pub(crate) fn new(text: &str, font: &Font, option: &TextOption) -> Self {
        let mut items = Vec::new();
        if text.is_empty() {
            return Self { items };
        }
        let base_rtl = match option.text_direction {
            LayoutDirection::LeftToRight => false,
            LayoutDirection::RightToLeft => true,
            LayoutDirection::LayoutDirectionAuto => bidi::is_rtl(text),
        };
        // The fonts of the whole paragraph: Qt itemizes it once, so each item is shaped with the
        // script items (and their fonts) of the paragraph, not of its own text.
        let engines = resolve_font_engines_for_text_global(font, text);
        // Printable ASCII has no bidi and no tab: one item, which is most of what is drawn.
        if !base_rtl && text.bytes().all(|b| (0x20..0x7f).contains(&b)) {
            items.push(Self::shaped_item(text, 0..text.len(), font, 0, &engines));
            return Self { items };
        }

        let mut starts: Vec<usize> = text.grapheme_indices(true).map(|(i, _)| i).collect();
        starts.push(text.len());
        let levels = bidi::grapheme_levels(text, &starts, base_rtl);
        let level_of = |grapheme: usize| levels.get(grapheme).copied().unwrap_or(0);

        // The run of graphemes at one level that has not been shaped yet: (first byte, level).
        let mut run: Option<(usize, u8)> = None;
        for grapheme in 0..starts.len() - 1 {
            let piece = &text[starts[grapheme]..starts[grapheme + 1]];
            let level = level_of(grapheme);
            let own_item = piece == "\t" || is_unshaped(piece);
            if let Some((from, run_level)) = run
                && (own_item || level != run_level)
            {
                items.push(Self::shaped_item(
                    text,
                    from..starts[grapheme],
                    font,
                    run_level,
                    &engines,
                ));
                run = None;
            }
            if piece == "\t" {
                // `QTextEngine::shapeLine`: the width of a tab from the pen the items before it,
                // in logical order, have reached.
                let x: f32 = items.iter().map(|item: &Item| item.width).sum();
                items.push(Item {
                    level,
                    width: tab_width(x, option.tab_stop_distance),
                    content: ItemContent::Blank,
                });
            } else if own_item {
                items.push(Item {
                    level,
                    width: 0.0,
                    content: ItemContent::Blank,
                });
            } else if run.is_none() {
                run = Some((starts[grapheme], level));
            }
        }
        if let Some((from, run_level)) = run {
            items.push(Self::shaped_item(
                text,
                from..text.len(),
                font,
                run_level,
                &engines,
            ));
        }
        Self { items }
    }

    /// The item `&text[range]` of the paragraph `text`, shaped with the paragraph's `engines` in the
    /// direction of `level`.
    fn shaped_item(
        text: &str,
        range: std::ops::Range<usize>,
        font: &Font,
        level: u8,
        engines: &[FontEngine],
    ) -> Item {
        let run_text = &text[range.clone()];
        if engines.is_empty() {
            // No font at all: the width Qt would not have, from the character classes.
            let width = FontMetrics::from_font(font).horizontal_advance(run_text, font);
            return Item {
                level,
                width,
                content: ItemContent::Blank,
            };
        }
        let layout = GlyphLayout::shape_piece(text, range, font, engines, Some(level % 2 == 1));
        // The soft hyphen and the form feed are shaped but not drawn: Qt gives their glyph
        // `dontPrint` (`applyVisibilityRules`, qtextengine.cpp:1329-1356), and
        // `QFontEngine::getGlyphPositions` leaves such glyphs out.
        let glyphs = layout
            .glyphs
            .into_iter()
            .filter(|glyph| {
                !run_text
                    .get(glyph.cluster..)
                    .is_some_and(|rest| rest.starts_with(['\u{ad}', '\u{c}']))
            })
            .collect();
        Item {
            level,
            width: layout.width,
            content: ItemContent::Glyphs {
                engines: engines.to_vec(),
                glyphs,
            },
        }
    }

    /// The width of the line (`QTextEngine::width(0, length)`).
    pub(crate) fn width(&self) -> f32 {
        self.items.iter().map(|item| item.width).sum()
    }

    /// The items from the left edge of the line in the order they are shown
    /// (`QTextEngine::bidiReorder`), each with the distance of its left edge from the line's.
    pub(crate) fn visual(&self) -> Vec<(f32, &Item)> {
        let levels: Vec<u8> = self.items.iter().map(|item| item.level).collect();
        let mut x = 0.0f32;
        bidi::visual_order(&levels)
            .into_iter()
            .map(|index| {
                let item = &self.items[index];
                let at = x;
                x += item.width;
                (at, item)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pen_positions_keep_a_64th_of_a_pixel() {
        assert_eq!(qfixed_from_real(4.25), 4.25);
        assert_eq!(qfixed_from_real(3.3), 3.296875);
        assert_eq!(qfixed_from_real(-3.3), -3.296875);
    }
}
