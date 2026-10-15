// SPDX-License-Identifier: LGPL-3.0-only
//! String list container matching Qt's `QStringList`.

use std::collections::HashSet;
use std::fmt;
use std::ops::{Deref, DerefMut, Index, IndexMut};

/// A list of strings with rich search, manipulation, and filtering utilities,
/// modeled after Qt's `QStringList`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct StringList(pub Vec<String>);

impl StringList {
    /// Creates an empty string list.
    pub fn new() -> Self {
        Self(Vec::new())
    }

    /// Creates an empty string list with the specified initial capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        Self(Vec::with_capacity(capacity))
    }

    /// Creates a string list from an existing `Vec<String>`.
    pub fn from_vec(vec: Vec<String>) -> Self {
        Self(vec)
    }

    /// Consumes the string list and returns the underlying `Vec<String>`.
    pub fn into_inner(self) -> Vec<String> {
        self.0
    }

    /// Returns a shared reference to the inner `Vec<String>`.
    /// Returns a slice of strings.
    #[inline]
    pub fn as_slice(&self) -> &[String] {
        &self.0
    }

    /// Returns a shared reference to the inner `Vec<String>`.
    pub fn as_vec(&self) -> &Vec<String> {
        &self.0
    }

    /// Returns a mutable reference to the inner `Vec<String>`.
    pub fn as_mut_vec(&mut self) -> &mut Vec<String> {
        &mut self.0
    }

    /// Joins all strings in the list using the specified separator, matching `QStringList::join`.
    pub fn join(&self, sep: &str) -> String {
        self.0.join(sep)
    }

    /// Splits a string by a delimiter into a `StringList`, matching `QString::split`.
    pub fn split(s: &str, sep: &str) -> Self {
        if sep.is_empty() {
            return Self(s.chars().map(|c| c.to_string()).collect());
        }
        Self(s.split(sep).map(|part| part.to_string()).collect())
    }

    /// Splits a string by whitespace into a `StringList`.
    pub fn split_whitespace(s: &str) -> Self {
        Self(s.split_whitespace().map(|part| part.to_string()).collect())
    }

    /// Returns a new `StringList` containing only elements that contain `pattern`,
    /// matching `QStringList::filter`.
    pub fn filter(&self, pattern: &str) -> Self {
        Self(
            self.0
                .iter()
                .filter(|s| s.contains(pattern))
                .cloned()
                .collect(),
        )
    }

    /// Case-insensitive version of `filter`.
    pub fn filter_case_insensitive(&self, pattern: &str) -> Self {
        let pat_lower = pattern.to_lowercase();
        Self(
            self.0
                .iter()
                .filter(|s| s.to_lowercase().contains(&pat_lower))
                .cloned()
                .collect(),
        )
    }

    /// Filters elements using an arbitrary predicate.
    pub fn filter_fn<F: Fn(&str) -> bool>(&self, f: F) -> Self {
        Self(self.0.iter().filter(|s| f(s.as_str())).cloned().collect())
    }

    /// Returns `true` if the list contains the given exact string, matching `QStringList::contains`.
    pub fn contains(&self, s: &str) -> bool {
        self.0.iter().any(|item| item == s)
    }

    /// Case-insensitive search for an element.
    pub fn contains_case_insensitive(&self, s: &str) -> bool {
        let target_lower = s.to_lowercase();
        self.0
            .iter()
            .any(|item| item.to_lowercase() == target_lower)
    }

    /// Position of the first item equal to `s`, like `QStringList::indexOf`.
    pub fn index_of(&self, s: &str) -> Option<usize> {
        self.0.iter().position(|item| item == s)
    }

    /// Position of the last item equal to `s`, like `QStringList::lastIndexOf`.
    pub fn last_index_of(&self, s: &str) -> Option<usize> {
        self.0.iter().rposition(|item| item == s)
    }

    /// Removes duplicate strings from the list while preserving original ordering of first occurrences,
    /// matching `QStringList::removeDuplicates`.
    pub fn remove_duplicates(&mut self) {
        let mut seen = HashSet::new();
        self.0.retain(|item| seen.insert(item.clone()));
    }

    /// Sorts the list in ascending lexicographical order, matching `QStringList::sort`.
    pub fn sort(&mut self) {
        self.0.sort();
    }

    /// Sorts the list in ascending case-insensitive lexicographical order.
    pub fn sort_case_insensitive(&mut self) {
        self.0.sort_by_cached_key(|s| s.to_lowercase());
    }

    /// Replaces every occurrence of `before` with `after` in each string in the list,
    /// matching `QStringList::replaceInStrings`.
    pub fn replace_in_strings(&mut self, before: &str, after: &str) {
        for s in &mut self.0 {
            if s.contains(before) {
                *s = s.replace(before, after);
            }
        }
    }
}

impl Deref for StringList {
    type Target = [String];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for StringList {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Index<usize> for StringList {
    type Output = String;

    fn index(&self, index: usize) -> &Self::Output {
        &self.0[index]
    }
}

impl IndexMut<usize> for StringList {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        &mut self.0[index]
    }
}

impl From<Vec<String>> for StringList {
    fn from(vec: Vec<String>) -> Self {
        Self(vec)
    }
}

impl From<Vec<&str>> for StringList {
    fn from(vec: Vec<&str>) -> Self {
        Self(vec.into_iter().map(|s| s.to_string()).collect())
    }
}

impl From<&[&str]> for StringList {
    fn from(slice: &[&str]) -> Self {
        Self(slice.iter().map(|s| s.to_string()).collect())
    }
}

impl From<StringList> for Vec<String> {
    fn from(list: StringList) -> Self {
        list.0
    }
}

impl FromIterator<String> for StringList {
    fn from_iter<I: IntoIterator<Item = String>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

impl<'a> FromIterator<&'a str> for StringList {
    fn from_iter<I: IntoIterator<Item = &'a str>>(iter: I) -> Self {
        Self(iter.into_iter().map(|s| s.to_string()).collect())
    }
}

impl IntoIterator for StringList {
    type Item = String;
    type IntoIter = std::vec::IntoIter<String>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a> IntoIterator for &'a StringList {
    type Item = &'a String;
    type IntoIter = std::slice::Iter<'a, String>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl<'a> IntoIterator for &'a mut StringList {
    type Item = &'a mut String;
    type IntoIter = std::slice::IterMut<'a, String>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter_mut()
    }
}

impl Extend<String> for StringList {
    fn extend<T: IntoIterator<Item = String>>(&mut self, iter: T) {
        self.0.extend(iter);
    }
}

impl<'a> Extend<&'a str> for StringList {
    fn extend<T: IntoIterator<Item = &'a str>>(&mut self, iter: T) {
        self.0.extend(iter.into_iter().map(|s| s.to_string()));
    }
}

impl fmt::Display for StringList {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "StringList[{}]", self.join(", "))
    }
}

/// Extension trait on standard Rust `Vec<String>` and slices to provide `StringList` conveniences.
pub trait StringListExt {
    /// Converts a slice or vector to a [`StringList`].
    fn to_string_list(&self) -> StringList;

    /// Joins elements using Qt separator semantics.
    fn join_qt(&self, sep: &str) -> String;

    /// Filters elements containing `pattern`.
    fn filter_qt(&self, pattern: &str) -> Vec<String>;
}

impl StringListExt for [String] {
    fn to_string_list(&self) -> StringList {
        StringList(self.to_vec())
    }

    fn join_qt(&self, sep: &str) -> String {
        self.join(sep)
    }

    fn filter_qt(&self, pattern: &str) -> Vec<String> {
        self.iter()
            .filter(|s| s.contains(pattern))
            .cloned()
            .collect()
    }
}

impl StringListExt for Vec<String> {
    fn to_string_list(&self) -> StringList {
        StringList(self.clone())
    }

    fn join_qt(&self, sep: &str) -> String {
        self.join(sep)
    }

    fn filter_qt(&self, pattern: &str) -> Vec<String> {
        self.as_slice().filter_qt(pattern)
    }
}
