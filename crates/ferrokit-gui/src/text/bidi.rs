// SPDX-License-Identifier: LGPL-3.0-only
//! Bidirectional text (UAX #9) as `QTextEngine` applies it: the base direction of a text (a
//! `QTextOption` starts with `LayoutDirectionAuto`, which takes it from the first strong
//! character), embedding levels per grapheme, and the visual order of the runs of one line.

use unicode_bidi::{BidiInfo, Level};

/// Whether `text` reads right to left as a whole: its first strong character is a right to left
/// one (rules P2 and P3).
pub(crate) fn is_rtl(text: &str) -> bool {
    if text.is_ascii() {
        return false;
    }
    BidiInfo::new(text, None)
        .paragraphs
        .first()
        .is_some_and(|p| p.level.is_rtl())
}

/// The embedding level of every grapheme of the paragraph `text`, whose graphemes start at the
/// byte offsets `starts` (followed by `text.len()`), in a text whose base direction is
/// `base_rtl`. Empty when every grapheme is at level 0 (the paragraph is left to right all
/// through), which is the case that needs no bidi handling at all.
pub(crate) fn grapheme_levels(text: &str, starts: &[usize], base_rtl: bool) -> Vec<u8> {
    if (text.is_ascii() && !base_rtl) || starts.len() < 2 {
        return Vec::new();
    }
    let info = BidiInfo::new(
        text,
        Some(if base_rtl { Level::rtl() } else { Level::ltr() }),
    );
    // A tab is a segment separator (rule L1): it is at the paragraph level, whatever surrounds it,
    // so the runs on either side of it do not join (`QTextEngine` gives it the base level).
    let paragraph_level = if base_rtl { 1 } else { 0 };
    let levels: Vec<u8> = starts[..starts.len() - 1]
        .iter()
        .map(|&byte| {
            if text.as_bytes()[byte] == b'\t' {
                paragraph_level
            } else {
                info.levels[byte].number()
            }
        })
        .collect();
    if levels.iter().all(|&level| level == 0) {
        Vec::new()
    } else {
        levels
    }
}

/// A maximal sequence of graphemes at one embedding level, in logical order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LevelRun {
    /// First grapheme (an index into the levels the run was made from).
    pub start: usize,
    /// One past the last grapheme.
    pub end: usize,
    pub level: u8,
}

impl LevelRun {
    /// Whether the run reads right to left.
    pub fn is_rtl(&self) -> bool {
        self.level % 2 == 1
    }
}

/// The runs of `levels`, in logical order.
pub(crate) fn level_runs(levels: &[u8]) -> Vec<LevelRun> {
    let mut runs: Vec<LevelRun> = Vec::new();
    for (index, &level) in levels.iter().enumerate() {
        match runs.last_mut() {
            Some(run) if run.level == level => run.end = index + 1,
            _ => runs.push(LevelRun {
                start: index,
                end: index + 1,
                level,
            }),
        }
    }
    runs
}

/// The visual order of runs with the given levels (rule L2, `QTextEngine::bidiReorder`):
/// `order[i]` is the logical index of the run shown `i`-th from the left.
pub(crate) fn visual_order(levels: &[u8]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..levels.len()).collect();
    let highest = levels.iter().copied().max().unwrap_or(0);
    let lowest_odd = levels.iter().copied().filter(|l| l % 2 == 1).min();
    let Some(lowest_odd) = lowest_odd else {
        return order;
    };
    // From the highest level down to the lowest odd one, reverse every sequence of runs at that
    // level or above.
    for level in (lowest_odd..=highest).rev() {
        let mut i = 0;
        while i < order.len() {
            if levels[order[i]] >= level {
                let start = i;
                while i < order.len() && levels[order[i]] >= level {
                    i += 1;
                }
                order[start..i].reverse();
            } else {
                i += 1;
            }
        }
    }
    order
}

#[cfg(test)]
mod tests {
    use super::*;

    fn starts(text: &str) -> Vec<usize> {
        let mut starts: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
        starts.push(text.len());
        starts
    }

    #[test]
    fn left_to_right_text_needs_no_levels() {
        assert!(grapheme_levels("abc def", &starts("abc def"), false).is_empty());
        assert!(grapheme_levels("a \u{e9}b", &starts("a \u{e9}b"), false).is_empty());
    }

    #[test]
    fn hebrew_letters_are_one_right_to_left_run() {
        let text = "abc \u{5d0}\u{5d1}\u{5d2} def";
        let levels = grapheme_levels(text, &starts(text), false);
        assert_eq!(levels, vec![0, 0, 0, 0, 1, 1, 1, 0, 0, 0, 0]);
        let runs = level_runs(&levels);
        assert_eq!(runs.len(), 3);
        assert_eq!((runs[1].start, runs[1].end, runs[1].is_rtl()), (4, 7, true));
    }

    #[test]
    fn the_first_strong_character_sets_the_base_direction() {
        assert!(!is_rtl("abc \u{5d0}"));
        assert!(is_rtl("\u{5d0}\u{5d1} abc"));
        assert!(is_rtl("12 \u{5d0}"));
        assert!(!is_rtl("12 345"));
        // an RTL paragraph: the Latin word is one level up, the space between stays at its level
        let text = "\u{5d0}\u{5d1} abc";
        assert_eq!(
            grapheme_levels(text, &starts(text), true),
            vec![1, 1, 1, 2, 2, 2]
        );
    }

    #[test]
    fn digits_inside_right_to_left_text_go_up_one_level() {
        let text = "\u{5d0}12\u{5d1}";
        assert_eq!(grapheme_levels(text, &starts(text), true), vec![1, 2, 2, 1]);
    }

    #[test]
    fn visual_order_reverses_odd_runs_and_nested_runs_twice() {
        assert_eq!(visual_order(&[0, 1, 0]), vec![0, 1, 2]);
        assert_eq!(
            visual_order(&[1, 1]),
            vec![0, 1].into_iter().rev().collect::<Vec<_>>()
        );
        // runs: RTL word, number inside it, RTL word: the number keeps its direction
        assert_eq!(visual_order(&[1, 2, 1]), vec![2, 1, 0]);
        assert_eq!(visual_order(&[0, 1, 2, 1, 0]), vec![0, 3, 2, 1, 4]);
        assert_eq!(visual_order(&[0, 0]), vec![0, 1]);
    }
}
