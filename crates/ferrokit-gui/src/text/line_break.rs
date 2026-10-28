// SPDX-License-Identifier: LGPL-3.0-only
//! Line break opportunities of a text, as `QTextEngine` takes them from `QUnicodeTools`
//! (UAX #14).

use unicode_linebreak::{break_property, linebreaks, BreakClass, BreakOpportunity};

/// For every grapheme of `text` (`starts` are their byte offsets followed by `text.len()`),
/// whether a line may break in front of it, then one entry for the end of the text.
///
/// This is UAX #14 as the `unicode-linebreak` crate implements it, with Qt's reading of the
/// rule for a slash in front of a digit: Qt's tables are older than the current LB25 and break
/// between `/` and a digit unless the slash is inside a number (`a/2` breaks after the slash,
/// `1/2` does not). The other numeric rules Qt predates (`PR × OP`, `CP × PR` and friends) are
/// not applied here either; they differ from Qt only in front of a bracket or sign next to
/// digits, which no rule of this module reproduces.
pub(crate) fn break_before(text: &str, starts: &[usize]) -> Vec<bool> {
    let mut breaks = vec![false; starts.len()];
    for (offset, opportunity) in linebreaks(text) {
        if opportunity == BreakOpportunity::Allowed {
            // A break inside a grapheme is not an opportunity.
            if let Ok(index) = starts.binary_search(&offset) {
                breaks[index] = true;
            }
        }
    }
    for (index, &offset) in starts.iter().enumerate().take(starts.len() - 1).skip(1) {
        if !breaks[index] && slash_in_front_of_digit_outside_number(text, offset) {
            breaks[index] = true;
        }
    }
    breaks
}

fn class_of(c: char) -> BreakClass {
    break_property(c as u32)
}

/// Whether the break at byte `offset` of `text` is between a symbol (`SY`, the slash) and a digit
/// with no number before the symbol.
fn slash_in_front_of_digit_outside_number(text: &str, offset: usize) -> bool {
    let mut before = text[..offset].chars().rev();
    let (Some(last), Some(next)) = (before.next(), text[offset..].chars().next()) else {
        return false;
    };
    if class_of(last) != BreakClass::Symbol || class_of(next) != BreakClass::Numeric {
        return false;
    }
    // Walk back over what can be part of a number: digits, slashes, and separators.
    for c in before {
        match class_of(c) {
            BreakClass::Numeric => return false,
            BreakClass::Symbol | BreakClass::InfixSeparator => {}
            _ => return true,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opportunities(text: &str) -> Vec<usize> {
        let mut starts: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
        starts.push(text.len());
        let breaks = break_before(text, &starts);
        (1..starts.len() - 1)
            .filter(|&i| breaks[i])
            .map(|i| starts[i])
            .collect()
    }

    #[test]
    fn a_slash_in_front_of_a_digit_breaks_unless_it_is_inside_a_number() {
        assert_eq!(opportunities("a/2"), vec![2]);
        assert_eq!(opportunities("1/2"), Vec::<usize>::new());
        assert_eq!(opportunities("1/2/3"), Vec::<usize>::new());
        assert_eq!(opportunities("aa/22bb"), vec![3]);
        assert_eq!(opportunities(")/2"), vec![2]);
    }

    #[test]
    fn commas_do_not_break_in_front_of_letters() {
        assert_eq!(opportunities("a,b"), Vec::<usize>::new());
        assert_eq!(opportunities("a-b"), vec![2]);
        assert_eq!(opportunities("a b"), vec![2]);
        assert_eq!(opportunities("a/b"), vec![2]);
    }
}
