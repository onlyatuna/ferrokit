// SPDX-License-Identifier: LGPL-3.0-only
//! Bidirectional text (UAX #9) as `QTextEngine` applies it: the base direction of a text (a
//! `QTextOption` starts with `LayoutDirectionAuto`, which takes it from the first strong
//! character), embedding levels per grapheme, and the visual order of the runs of one line.

use unicode_bidi::{BidiClass, BidiInfo, Level, bidi_class};

/// Whether `text` reads right to left as a whole: its first strong character outside isolates is
/// a right to left one (rules P2 and P3, `QtPrivate::isRightToLeft`, qstring.cpp:9606: the
/// characters between an isolate initiator and its terminator do not count, however strong). The
/// whole text counts, line feeds included: a text that starts with a line feed before its first
/// strong character is not taken as left to right for that.
pub(crate) fn is_rtl(text: &str) -> bool {
    if text.is_ascii() {
        return false;
    }
    let mut isolates = 0u32;
    for c in text.chars() {
        match bidi_class(c) {
            BidiClass::RLI | BidiClass::LRI | BidiClass::FSI => isolates += 1,
            BidiClass::PDI => isolates = isolates.saturating_sub(1),
            BidiClass::L if isolates == 0 => return false,
            BidiClass::R | BidiClass::AL if isolates == 0 => return true,
            _ => {}
        }
    }
    false
}

/// Whether a character belongs to the classes that make `QBidiAlgorithm::checkForBidi`
/// (qtextengine.cpp:1026) run the algorithm for a left to right text: the right to left and
/// Arabic classes, Arabic numbers, and the explicit embedding, override and isolate controls.
/// Everything else left to right has level 0 whatever surrounds it.
fn is_bidi_trigger(c: char) -> bool {
    c as u32 >= 0x590
        && matches!(
            bidi_class(c),
            BidiClass::R
                | BidiClass::AN
                | BidiClass::LRE
                | BidiClass::LRO
                | BidiClass::AL
                | BidiClass::RLE
                | BidiClass::RLO
                | BidiClass::PDF
                | BidiClass::LRI
                | BidiClass::RLI
                | BidiClass::FSI
                | BidiClass::PDI
        )
}

/// The embedding level of every grapheme of `text`, whose graphemes start at the byte offsets
/// `starts` (followed by `text.len()`), in a text whose base direction is `base_rtl`: what
/// `QBidiAlgorithm::process` (qtextengine.cpp:1046) leaves in `QScriptAnalysis::bidiLevel` for the
/// first character of the grapheme. Empty when every grapheme is at level 0, which is the case
/// that needs no bidi handling at all.
///
/// After the levels of rules X and W/N/I, Qt applies rule L1 to the whole text (not per line):
/// segment and paragraph separators, and the white space and isolate and explicit formatting
/// characters before them or at the end of the text, go back to the base level; then every
/// explicit formatting character and boundary neutral takes the lower level of its neighbours
/// (its own level means nothing, it is not in the rendered text). A tab is a segment separator, so it is
/// at the base level whatever surrounds it and the runs on either side of it do not join.
pub(crate) fn grapheme_levels(text: &str, starts: &[usize], base_rtl: bool) -> Vec<u8> {
    if starts.len() < 2 || (!base_rtl && !text.chars().any(is_bidi_trigger)) {
        return Vec::new();
    }
    let info = BidiInfo::new(
        text,
        Some(if base_rtl { Level::rtl() } else { Level::ltr() }),
    );
    let base = u8::from(base_rtl);
    let offsets: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
    let classes: Vec<BidiClass> = offsets.iter().map(|&i| info.original_classes[i]).collect();
    let mut levels: Vec<u8> = offsets.iter().map(|&i| info.levels[i].number()).collect();

    // Rule L1 (qtextengine.cpp:1077-1091), from the end of the text.
    let mut reset = true;
    for (class, level) in classes.iter().zip(levels.iter_mut()).rev() {
        match class {
            BidiClass::B | BidiClass::S => {
                *level = base;
                reset = true;
            }
            BidiClass::WS
            | BidiClass::LRI
            | BidiClass::RLI
            | BidiClass::FSI
            | BidiClass::PDI
            | BidiClass::LRE
            | BidiClass::RLE
            | BidiClass::LRO
            | BidiClass::RLO
            | BidiClass::PDF
            | BidiClass::BN
                if reset =>
            {
                *level = base;
            }
            _ => reset = false,
        }
    }

    // Boundary neutrals and explicit formatting characters (qtextengine.cpp:1093-1122).
    let is_bn = |class: BidiClass| {
        matches!(
            class,
            BidiClass::LRE
                | BidiClass::RLE
                | BidiClass::LRO
                | BidiClass::RLO
                | BidiClass::PDF
                | BidiClass::BN
        )
    };
    let (mut last_level, mut first_bn) = (base, None);
    for i in 0..levels.len() {
        if is_bn(classes[i]) {
            first_bn.get_or_insert(i);
            levels[i] = last_level;
        } else {
            let level = levels[i];
            if let Some(from) = first_bn.take()
                && level < last_level
            {
                levels[from..i].fill(level);
            }
            last_level = level;
        }
    }
    if let Some(from) = first_bn
        && base < last_level
    {
        levels[from..].fill(base);
    }

    let grapheme_levels: Vec<u8> = starts[..starts.len() - 1]
        .iter()
        .map(|byte| {
            let char_index = offsets
                .binary_search(byte)
                .unwrap_or_else(|at| at.saturating_sub(1));
            levels[char_index]
        })
        .collect();
    if grapheme_levels.iter().all(|&level| level == 0) {
        Vec::new()
    } else {
        grapheme_levels
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

/// The visual order of runs with the given levels (rule L2, `QTextEngine::bidiReorder`,
/// qtextengine.cpp:1142): `order[i]` is the logical index of the run shown `i`-th from the left.
/// Qt reverses down to the lowest level of the runs, or the one above it when that is even (not
/// to the lowest odd level of the rule).
pub(crate) fn visual_order(levels: &[u8]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..levels.len()).collect();
    let highest = levels.iter().copied().max().unwrap_or(0);
    let lowest = levels.iter().copied().min().unwrap_or(0);
    let lowest = lowest + u8::from(lowest % 2 == 0);
    // From the highest level down, reverse every sequence of runs at that level or above.
    for level in (lowest..=highest).rev() {
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

    #[test]
    fn white_space_before_a_tab_or_the_end_goes_back_to_the_base_level() {
        // Rule L1 over the whole text, as `QBidiAlgorithm::process` does: the space before the
        // tab and the one at the end are at the base level, the one between two Latin words of
        // a right to left text is not.
        let text = "ab \tcd e ";
        assert_eq!(
            grapheme_levels(text, &starts(text), true),
            vec![2, 2, 1, 1, 2, 2, 2, 2, 1]
        );
    }

    #[test]
    fn boundary_neutrals_take_the_lower_level_of_their_neighbours() {
        // The joiner between a Hebrew and a Latin letter is at the lower level of the two, not
        // the one of the character before it (qtextengine.cpp:1093).
        let text = "\u{5d0}\u{200d}ab";
        assert_eq!(
            grapheme_levels(text, &starts(text), false),
            vec![1, 0, 0, 0]
        );
        let text = "ab\u{200d}\u{5d0}";
        assert_eq!(
            grapheme_levels(text, &starts(text), false),
            vec![0, 0, 0, 1]
        );
    }

    #[test]
    fn isolates_hide_their_text_from_the_base_direction() {
        // `QtPrivate::isRightToLeft` skips what is between an isolate initiator and its end.
        assert!(!is_rtl("\u{2067}\u{5d0}\u{2069}abc"));
        assert!(is_rtl("\u{2066}abc\u{2069}\u{5d0}"));
        assert!(is_rtl("\u{2067}\u{2066}abc\u{2069}\u{5d0}\u{2069} \u{5d1}"));
    }
}
