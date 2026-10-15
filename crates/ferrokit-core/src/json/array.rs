// SPDX-License-Identifier: LGPL-3.0-only
//! QJsonArray equivalent array implementation.
//!
//! Encapsulates a JSON array (`Vec<JsonValue>` backed), providing random access,
//! mutation, inspection, and iterator conversions.

use std::fmt;
use std::ops::{Index, IndexMut};

use super::value::JsonValue;

static NULL_VALUE: JsonValue = JsonValue::Null;

/// A JSON array representation (`QJsonArray` equivalent).
#[derive(Clone, PartialEq, Default)]
pub struct JsonArray {
    pub(crate) list: Vec<JsonValue>,
}

impl JsonArray {
    /// Constructs an empty JSON array.
    #[inline]
    pub fn new() -> Self {
        Self { list: Vec::new() }
    }

    /// Constructs an empty JSON array with pre-allocated capacity.
    #[inline]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            list: Vec::with_capacity(capacity),
        }
    }

    /// How many elements the array holds (`QJsonArray::size` / `count`).
    #[inline]
    pub fn len(&self) -> usize {
        self.list.len()
    }

    /// How many elements the array holds (`QJsonArray::count`).
    #[inline]
    pub fn count(&self) -> usize {
        self.list.len()
    }

    /// Returns `true` if the array contains no elements (`QJsonArray::isEmpty`).
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// Appends a value to the end of the array (`QJsonArray::append` / `push_back`).
    #[inline]
    pub fn push(&mut self, value: impl Into<JsonValue>) {
        self.list.push(value.into());
    }

    /// Appends a value to the end of the array (Qt alias for `push`).
    #[inline]
    pub fn append(&mut self, value: impl Into<JsonValue>) {
        self.push(value);
    }

    /// Inserts a value at position `index` (`QJsonArray::insert`).
    #[inline]
    pub fn insert(&mut self, index: usize, value: impl Into<JsonValue>) {
        if index <= self.list.len() {
            self.list.insert(index, value.into());
        }
    }

    /// Removes and returns the element at `index` (`QJsonArray::removeAt`).
    #[inline]
    pub fn remove(&mut self, index: usize) -> JsonValue {
        self.list.remove(index)
    }

    /// Removes and returns the element at `index`, or `JsonValue::Null` if out of bounds (`QJsonArray::takeAt`).
    #[inline]
    pub fn take(&mut self, index: usize) -> JsonValue {
        if index < self.list.len() {
            self.list.remove(index)
        } else {
            JsonValue::Null
        }
    }

    /// Borrows the element at `index`; `None` when `index` is out of range.
    #[inline]
    pub fn get(&self, index: usize) -> Option<&JsonValue> {
        self.list.get(index)
    }

    /// Returns a mutable reference to the element at `index`, or `None` if out of bounds.
    #[inline]
    pub fn get_mut(&mut self, index: usize) -> Option<&mut JsonValue> {
        self.list.get_mut(index)
    }

    /// Borrows the element at `index`, or `&JsonValue::Null` when out of range (`QJsonArray::at`).
    #[inline]
    pub fn at(&self, index: usize) -> &JsonValue {
        self.list.get(index).unwrap_or(&NULL_VALUE)
    }

    /// Returns the first element in the array, if any (`QJsonArray::first`).
    #[inline]
    pub fn first(&self) -> Option<&JsonValue> {
        self.list.first()
    }

    /// Returns the last element in the array, if any (`QJsonArray::last`).
    #[inline]
    pub fn last(&self) -> Option<&JsonValue> {
        self.list.last()
    }

    /// Clears all elements from the array.
    #[inline]
    pub fn clear(&mut self) {
        self.list.clear();
    }

    /// Returns `true` if the array contains `value` (`QJsonArray::contains`).
    #[inline]
    pub fn contains(&self, value: &JsonValue) -> bool {
        self.list.contains(value)
    }

    /// Returns a shared slice of the elements.
    #[inline]
    pub fn as_slice(&self) -> &[JsonValue] {
        &self.list
    }

    /// Returns a mutable slice of the elements.
    #[inline]
    pub fn as_mut_slice(&mut self) -> &mut [JsonValue] {
        &mut self.list
    }

    /// Returns an iterator over elements.
    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = &JsonValue> {
        self.list.iter()
    }

    /// Returns a mutable iterator over elements.
    #[inline]
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut JsonValue> {
        self.list.iter_mut()
    }

    /// Constructs a `JsonArray` from a slice of Variants (`QJsonArray::fromVariantList`).
    pub fn from_variant_list(list: &[crate::variant::Variant]) -> Self {
        let items: Vec<JsonValue> = list.iter().map(|v| v.to_json_value()).collect();
        Self { list: items }
    }

    /// Converts this `JsonArray` into a list of Variants (`QJsonArray::toVariantList`).
    pub fn to_variant_list(&self) -> Vec<crate::variant::Variant> {
        self.list
            .iter()
            .map(crate::variant::Variant::from_json_value)
            .collect()
    }
}

impl Index<usize> for JsonArray {
    type Output = JsonValue;

    #[inline]
    fn index(&self, index: usize) -> &Self::Output {
        self.at(index)
    }
}

impl IndexMut<usize> for JsonArray {
    #[inline]
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        &mut self.list[index]
    }
}

impl fmt::Debug for JsonArray {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.list.iter()).finish()
    }
}

impl FromIterator<JsonValue> for JsonArray {
    fn from_iter<T: IntoIterator<Item = JsonValue>>(iter: T) -> Self {
        Self {
            list: iter.into_iter().collect(),
        }
    }
}

impl IntoIterator for JsonArray {
    type Item = JsonValue;
    type IntoIter = std::vec::IntoIter<JsonValue>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.list.into_iter()
    }
}

impl<'a> IntoIterator for &'a JsonArray {
    type Item = &'a JsonValue;
    type IntoIter = std::slice::Iter<'a, JsonValue>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.list.iter()
    }
}
