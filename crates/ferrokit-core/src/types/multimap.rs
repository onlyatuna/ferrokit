// SPDX-License-Identifier: LGPL-3.0-only
//! Multi-value map and hash containers matching Qt's `QMultiMap` and `QMultiHash`.

use std::collections::{BTreeMap, HashMap};
use std::hash::Hash;
use std::ops::{Deref, DerefMut};

/// An ordered associative container that maps keys to multiple values,
/// modeled after Qt's `QMultiMap`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct MultiMap<K: Ord, V>(pub BTreeMap<K, Vec<V>>);

impl<K: Ord, V> MultiMap<K, V> {
    /// Creates an empty multimap.
    pub fn new() -> Self {
        Self(BTreeMap::new())
    }

    /// Inserts a key-value pair into the multimap, matching `QMultiMap::insert`.
    pub fn insert(&mut self, key: K, value: V) {
        self.0.entry(key).or_default().push(value);
    }

    /// Returns the most recently inserted value for `key`, matching Qt's `QMultiMap::value`.
    pub fn get(&self, key: &K) -> Option<&V> {
        self.0.get(key).and_then(|v| v.last())
    }

    /// Returns a slice of all values associated with `key`, matching `QMultiMap::values(key)`.
    pub fn get_all(&self, key: &K) -> &[V] {
        self.0.get(key).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Returns a mutable slice of all values associated with `key`.
    pub fn get_all_mut(&mut self, key: &K) -> &mut [V] {
        if let Some(v) = self.0.get_mut(key) {
            v.as_mut_slice()
        } else {
            &mut []
        }
    }

    /// Returns `true` if the multimap contains the specified `key`, matching `QMultiMap::contains(key)`.
    pub fn contains_key(&self, key: &K) -> bool {
        self.0.contains_key(key)
    }

    /// Returns `true` if the multimap contains the given `(key, value)` pair,
    /// matching `QMultiMap::contains(key, value)`.
    pub fn contains(&self, key: &K, value: &V) -> bool
    where
        V: PartialEq,
    {
        self.0
            .get(key)
            .map(|vec| vec.iter().any(|v| v == value))
            .unwrap_or(false)
    }

    /// Removes all values associated with `key` and returns them, matching `QMultiMap::remove(key)`.
    pub fn remove(&mut self, key: &K) -> Option<Vec<V>> {
        self.0.remove(key)
    }

    /// Removes the first occurrence of `value` associated with `key`.
    /// Returns `true` if an item was removed, matching `QMultiMap::remove(key, value)`.
    pub fn remove_value(&mut self, key: &K, value: &V) -> bool
    where
        V: PartialEq,
    {
        if let Some(values) = self.0.get_mut(key)
            && let Some(pos) = values.iter().position(|v| v == value)
        {
            values.remove(pos);
            if values.is_empty() {
                self.0.remove(key);
            }
            return true;
        }
        false
    }

    /// Returns a vector of all unique keys in ascending order, matching `QMultiMap::uniqueKeys`.
    pub fn unique_keys(&self) -> Vec<&K> {
        self.0.keys().collect()
    }

    /// Returns a vector of references to all values across all keys in the multimap.
    pub fn all_values(&self) -> Vec<&V> {
        self.0.values().flat_map(|v| v.iter()).collect()
    }

    /// Returns the total number of values across all keys in the multimap.
    pub fn len(&self) -> usize {
        self.0.values().map(|v| v.len()).sum()
    }

    /// Returns the number of distinct keys in the multimap.
    pub fn key_count(&self) -> usize {
        self.0.len()
    }

    /// Returns `true` if the multimap contains no values.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Removes all keys and values from the multimap.
    pub fn clear(&mut self) {
        self.0.clear();
    }

    /// Consumes the multimap and returns the underlying [`BTreeMap`].
    pub fn into_inner(self) -> BTreeMap<K, Vec<V>> {
        self.0
    }
}

impl<K: Ord, V> Deref for MultiMap<K, V> {
    type Target = BTreeMap<K, Vec<V>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<K: Ord, V> DerefMut for MultiMap<K, V> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<K: Ord, V> FromIterator<(K, V)> for MultiMap<K, V> {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        let mut map = Self::new();
        for (k, v) in iter {
            map.insert(k, v);
        }
        map
    }
}

impl<K: Ord, V> Extend<(K, V)> for MultiMap<K, V> {
    fn extend<I: IntoIterator<Item = (K, V)>>(&mut self, iter: I) {
        for (k, v) in iter {
            self.insert(k, v);
        }
    }
}

/// An unordered associative container that maps keys to multiple values using hashing,
/// modeled after Qt's `QMultiHash`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MultiHash<K: Eq + Hash, V>(pub HashMap<K, Vec<V>>);

impl<K: Eq + Hash, V> MultiHash<K, V> {
    /// Creates an empty multihash.
    pub fn new() -> Self {
        Self(HashMap::new())
    }

    /// Creates an empty multihash with the specified capacity of unique keys.
    pub fn with_capacity(capacity: usize) -> Self {
        Self(HashMap::with_capacity(capacity))
    }

    /// Inserts a key-value pair into the multihash, matching `QMultiHash::insert`.
    pub fn insert(&mut self, key: K, value: V) {
        self.0.entry(key).or_default().push(value);
    }

    /// Returns the most recently inserted value for `key`, matching Qt's `QMultiHash::value`.
    pub fn get(&self, key: &K) -> Option<&V> {
        self.0.get(key).and_then(|v| v.last())
    }

    /// Returns a slice of all values associated with `key`, matching `QMultiHash::values(key)`.
    pub fn get_all(&self, key: &K) -> &[V] {
        self.0.get(key).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Returns `true` if the multihash contains the specified `key`, matching `QMultiHash::contains(key)`.
    pub fn contains_key(&self, key: &K) -> bool {
        self.0.contains_key(key)
    }

    /// Returns `true` if the multihash contains the given `(key, value)` pair,
    /// matching `QMultiHash::contains(key, value)`.
    pub fn contains(&self, key: &K, value: &V) -> bool
    where
        V: PartialEq,
    {
        self.0
            .get(key)
            .map(|vec| vec.iter().any(|v| v == value))
            .unwrap_or(false)
    }

    /// Removes all values associated with `key` and returns them, matching `QMultiHash::remove(key)`.
    pub fn remove(&mut self, key: &K) -> Option<Vec<V>> {
        self.0.remove(key)
    }

    /// Removes the first occurrence of `value` associated with `key`.
    /// Returns `true` if an item was removed, matching `QMultiHash::remove(key, value)`.
    pub fn remove_value(&mut self, key: &K, value: &V) -> bool
    where
        V: PartialEq,
    {
        if let Some(values) = self.0.get_mut(key)
            && let Some(pos) = values.iter().position(|v| v == value)
        {
            values.remove(pos);
            if values.is_empty() {
                self.0.remove(key);
            }
            return true;
        }
        false
    }

    /// Returns a vector of references to all unique keys in the multihash.
    pub fn unique_keys(&self) -> Vec<&K> {
        self.0.keys().collect()
    }

    /// Returns a vector of references to all values across all keys in the multihash.
    pub fn all_values(&self) -> Vec<&V> {
        self.0.values().flat_map(|v| v.iter()).collect()
    }

    /// Returns the total number of values across all keys in the multihash.
    pub fn len(&self) -> usize {
        self.0.values().map(|v| v.len()).sum()
    }

    /// Returns the number of distinct keys in the multihash.
    pub fn key_count(&self) -> usize {
        self.0.len()
    }

    /// Returns `true` if the multihash contains no values.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Removes all keys and values from the multihash.
    pub fn clear(&mut self) {
        self.0.clear();
    }

    /// Consumes the multihash and returns the underlying [`HashMap`].
    pub fn into_inner(self) -> HashMap<K, Vec<V>> {
        self.0
    }
}

impl<K: Eq + Hash, V> Deref for MultiHash<K, V> {
    type Target = HashMap<K, Vec<V>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<K: Eq + Hash, V> DerefMut for MultiHash<K, V> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<K: Eq + Hash, V> FromIterator<(K, V)> for MultiHash<K, V> {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        let mut map = Self::new();
        for (k, v) in iter {
            map.insert(k, v);
        }
        map
    }
}

impl<K: Eq + Hash, V> Extend<(K, V)> for MultiHash<K, V> {
    fn extend<I: IntoIterator<Item = (K, V)>>(&mut self, iter: I) {
        for (k, v) in iter {
            self.insert(k, v);
        }
    }
}
