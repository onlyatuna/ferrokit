// SPDX-License-Identifier: LGPL-3.0-only
//! QJsonObject equivalent map implementation.
//!
//! Encapsulates a JSON object with sorted key-value pairs (`BTreeMap` backed),
//! matching Qt's `QJsonObject` sorted key guarantee.

use std::collections::BTreeMap;
use std::fmt;
use std::ops::Index;

use super::value::JsonValue;
use crate::types::StringList;

static NULL_VALUE: JsonValue = JsonValue::Null;

/// A JSON object representation (`QJsonObject` equivalent).
#[derive(Clone, PartialEq, Default)]
pub struct JsonObject {
    pub(crate) map: BTreeMap<String, JsonValue>,
}

impl JsonObject {
    /// Constructs an empty JSON object.
    #[inline]
    pub fn new() -> Self {
        Self {
            map: BTreeMap::new(),
        }
    }

    /// How many key/value pairs the object holds (`QJsonObject::size` / `count`).
    #[inline]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// How many key/value pairs the object holds (`QJsonObject::count`).
    #[inline]
    pub fn count(&self) -> usize {
        self.map.len()
    }

    /// Returns `true` if the object contains no key-value pairs (`QJsonObject::isEmpty`).
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Returns `true` if the object contains the specified key (`QJsonObject::contains`).
    #[inline]
    pub fn contains(&self, key: &str) -> bool {
        self.map.contains_key(key)
    }

    /// Returns `true` if the object contains the specified key.
    #[inline]
    pub fn contains_key(&self, key: &str) -> bool {
        self.map.contains_key(key)
    }

    /// Returns a reference to the value associated with `key`, or `None` if absent.
    #[inline]
    pub fn get(&self, key: &str) -> Option<&JsonValue> {
        self.map.get(key)
    }

    /// Returns a mutable reference to the value associated with `key`, or `None` if absent.
    #[inline]
    pub fn get_mut(&mut self, key: &str) -> Option<&mut JsonValue> {
        self.map.get_mut(key)
    }

    /// Returns a reference to the value associated with `key`, or `&JsonValue::Null` (`QJsonObject::value`).
    #[inline]
    pub fn value(&self, key: &str) -> &JsonValue {
        self.map.get(key).unwrap_or(&NULL_VALUE)
    }

    /// Inserts a key-value pair into the object, returning the previous value if any (`QJsonObject::insert`).
    #[inline]
    pub fn insert(
        &mut self,
        key: impl Into<String>,
        value: impl Into<JsonValue>,
    ) -> Option<JsonValue> {
        self.map.insert(key.into(), value.into())
    }

    /// Removes `key` from the object and returns the removed value, or `None` (`QJsonObject::remove`).
    #[inline]
    pub fn remove(&mut self, key: &str) -> Option<JsonValue> {
        self.map.remove(key)
    }

    /// Removes `key` and returns the removed value, or `JsonValue::Null` (`QJsonObject::take`).
    #[inline]
    pub fn take(&mut self, key: &str) -> JsonValue {
        self.map.remove(key).unwrap_or(JsonValue::Null)
    }

    /// Removes all key-value pairs from the object.
    #[inline]
    pub fn clear(&mut self) {
        self.map.clear();
    }

    /// Returns a list of all keys in sorted order (`QJsonObject::keys` -> `QStringList`).
    pub fn keys(&self) -> StringList {
        let keys: Vec<String> = self.map.keys().cloned().collect();
        StringList::from(keys)
    }

    /// Returns an iterator over key-value pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &JsonValue)> {
        self.map.iter()
    }

    /// Returns a mutable iterator over key-value pairs.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&String, &mut JsonValue)> {
        self.map.iter_mut()
    }

    /// Returns an iterator over values.
    pub fn values(&self) -> impl Iterator<Item = &JsonValue> {
        self.map.values()
    }

    /// Returns a reference to the underlying `BTreeMap`.
    #[inline]
    pub fn as_map(&self) -> &BTreeMap<String, JsonValue> {
        &self.map
    }

    /// Returns a mutable reference to the underlying `BTreeMap`.
    #[inline]
    pub fn as_mut_map(&mut self) -> &mut BTreeMap<String, JsonValue> {
        &mut self.map
    }

    /// Constructs a `JsonObject` from a Variant map (`QJsonObject::fromVariantMap`).
    pub fn from_variant_map(
        map: &std::collections::HashMap<String, crate::variant::Variant>,
    ) -> Self {
        let mut obj = Self::new();
        for (k, v) in map.iter() {
            obj.insert(k.clone(), v.to_json_value());
        }
        obj
    }

    /// Converts this `JsonObject` into a Variant map (`QJsonObject::toVariantMap`).
    pub fn to_variant_map(&self) -> std::collections::HashMap<String, crate::variant::Variant> {
        let mut map = std::collections::HashMap::with_capacity(self.len());
        for (k, v) in self.map.iter() {
            map.insert(k.clone(), crate::variant::Variant::from_json_value(v));
        }
        map
    }
}

impl Index<&str> for JsonObject {
    type Output = JsonValue;

    #[inline]
    fn index(&self, key: &str) -> &Self::Output {
        self.value(key)
    }
}

impl fmt::Debug for JsonObject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.map.iter()).finish()
    }
}

impl FromIterator<(String, JsonValue)> for JsonObject {
    fn from_iter<T: IntoIterator<Item = (String, JsonValue)>>(iter: T) -> Self {
        let mut obj = Self::new();
        for (k, v) in iter {
            obj.insert(k, v);
        }
        obj
    }
}

impl<'a> FromIterator<(&'a str, JsonValue)> for JsonObject {
    fn from_iter<T: IntoIterator<Item = (&'a str, JsonValue)>>(iter: T) -> Self {
        let mut obj = Self::new();
        for (k, v) in iter {
            obj.insert(k, v);
        }
        obj
    }
}

impl IntoIterator for JsonObject {
    type Item = (String, JsonValue);
    type IntoIter = std::collections::btree_map::IntoIter<String, JsonValue>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.map.into_iter()
    }
}

impl<'a> IntoIterator for &'a JsonObject {
    type Item = (&'a String, &'a JsonValue);
    type IntoIter = std::collections::btree_map::Iter<'a, String, JsonValue>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.map.iter()
    }
}
