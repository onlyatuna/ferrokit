// SPDX-License-Identifier: LGPL-3.0-only
//! Core data structures and canonical Qt-to-Rust type mapping conventions.
//!
//! # Qt to Rust Public API Conventions
//!
//! When interacting with `ferrokit` or developing third-party crates built on `ferrokit`,
//! the following official type conventions apply:
//!
//! | Qt C++ Type | Idiomatic Rust Representation | `ferrokit` Convenience Type / Alias |
//! | :--- | :--- | :--- |
//! | `QString` | [`String`] (owned UTF-8) | [`QString`] |
//! | `QStringView` / `QUtf8StringView` | `&str` (borrowed UTF-8 slice) | [`QStringView`] |
//! | `QStringList` | [`Vec<String>`] | [`StringList`] / [`QStringList`] |
//! | `QByteArray` | [`Vec<u8>`] | [`ByteArray`] / [`QByteArray`] |
//! | `QByteArrayView` | `&[u8]` (borrowed byte slice) | [`QByteArrayView`] |
//! | `QByteArrayList` | [`Vec<ByteArray>`] | [`ByteArrayList`] / [`QByteArrayList`] |
//! | `QList<T>`, `QVector<T>` | [`Vec<T>`] | [`QList<T>`], [`QVector<T>`] |
//! | `QQueue<T>` | [`std::collections::VecDeque<T>`] | [`Queue<T>`] / [`QQueue<T>`] |
//! | `QStack<T>` | [`Vec<T>`] | [`Stack<T>`] / [`QStack<T>`] |
//! | `QHash<K, V>` | [`std::collections::HashMap<K, V>`] | [`QHash<K, V>`] |
//! | `QMap<K, V>` | [`std::collections::BTreeMap<K, V>`] | [`QMap<K, V>`] |
//! | `QSet<T>` | [`std::collections::HashSet<T>`] | [`QSet<T>`] |
//! | `QMultiMap<K, V>` | Multi-value ordered map | [`MultiMap<K, V>`] / [`QMultiMap<K, V>`] |
//! | `QMultiHash<K, V>` | Multi-value hash map | [`MultiHash<K, V>`] / [`QMultiHash<K, V>`] |
//! | `QBitArray` | Dynamic compact bit vector | [`BitArray`] / [`QBitArray`] |
//! | `QPair<T1, T2>` | Standard Rust tuple `(T1, T2)` | [`QPair<T1, T2>`] |
//! | `QSpan<T>` | Borrowed slice `&[T]` / `&mut [T]` | [`QSpan<'a, T>`], [`QSpanMut<'a, T>`] |
//! | `QVarLengthArray<T>` | [`Vec<T>`] | [`VarLengthArray<T>`], [`QVarLengthArray<T>`] |
//! | Shared immutable string | [`std::sync::Arc<str>`] | [`SharedStr`] |
//! | `QVariantList` | [`Vec<Variant>`] | [`QVariantList`] |
//! | `QVariantMap` | [`std::collections::BTreeMap<String, Variant>`] | [`QVariantMap`] |
//! | `QVariantHash` | [`std::collections::HashMap<String, Variant>`] | [`QVariantHash`] |
//! | `QVariantPair` | Tuple `(Variant, Variant)` | [`QVariantPair`] |
//!
//! # Guidelines for API Design in `ferrokit`
//!
//! 1. **Owned Strings**: Use standard Rust [`String`]. Do not reinvent a separate heap-allocated string struct.
//! 2. **String Slices**: Accept `&str` or `impl AsRef<str>`.
//! 3. **Shared Strings**: When strings are shared across threads without mutation, prefer `Arc<str>` ([`SharedStr`]).
//! 4. **Byte Slices vs. Arrays**: Use [`ByteArray`] when hex/base64/text mutation is needed; use `&[u8]` for borrowing.
//! 5. **Sequences**: Standardize on standard [`Vec<T>`]. Provide conversions to/from [`Queue`] or [`Stack`] when semantics require FIFO/LIFO queues.
//! 6. **Maps**: Use [`std::collections::HashMap`] for maximum performance with unordered keys, and [`std::collections::BTreeMap`] when deterministic key ordering or range queries are required.

pub mod bit_array;
pub mod byte_array;
pub mod geometry;
pub mod ident;
pub mod multimap;
pub mod queue_stack;
pub mod string_list;
pub mod temporal;

// Re-export primary types
pub use bit_array::BitArray;
pub use byte_array::{Base64Error, ByteArray, HexError};
pub use geometry::{Line, LineF, Margins, MarginsF, Size, SizeF};
pub use ident::{Locale, RegularExpression, Url, UrlQuery, Uuid};
pub use multimap::{MultiHash, MultiMap};
pub use queue_stack::{Queue, Stack};
pub use string_list::{StringList, StringListExt};
pub use temporal::{Date, DateTime, Time};

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use crate::variant::Variant;

// =============================================================================
// Canonical Qt-to-Rust Type Aliases
// =============================================================================

/// Canonical alias for Qt's `QString` -> Rust [`String`].
pub type QString = String;

/// Canonical alias for Qt's `QStringView` -> Rust borrowed string slice `&str`.
pub type QStringView<'a> = &'a str;

/// Canonical alias for Qt's `QUtf8StringView` -> Rust borrowed string slice `&str`.
pub type QUtf8StringView<'a> = &'a str;

/// Canonical alias for Qt's `QStringList` -> [`StringList`].
pub type QStringList = StringList;

/// Canonical alias for Qt's `QByteArray` -> [`ByteArray`].
pub type QByteArray = ByteArray;

/// Canonical alias for Qt's `QByteArrayView` -> Rust borrowed byte slice `&[u8]`.
pub type QByteArrayView<'a> = &'a [u8];

/// Canonical alias for a list of byte arrays -> [`Vec<ByteArray>`].
pub type ByteArrayList = Vec<ByteArray>;

/// Canonical alias for Qt's `QByteArrayList` -> [`ByteArrayList`].
pub type QByteArrayList = ByteArrayList;

/// Canonical alias for Qt's `QList<T>` -> Rust [`Vec<T>`].
pub type QList<T> = Vec<T>;

/// Canonical alias for Qt's `QVector<T>` -> Rust [`Vec<T>`].
pub type QVector<T> = Vec<T>;

/// Canonical alias for Qt's `QQueue<T>` -> [`Queue<T>`].
pub type QQueue<T> = Queue<T>;

/// Canonical alias for Qt's `QStack<T>` -> [`Stack<T>`].
pub type QStack<T> = Stack<T>;

/// Canonical alias for Qt's `QHash<K, V>` -> [`HashMap<K, V>`].
pub type QHash<K, V> = HashMap<K, V>;

/// Canonical alias for Qt's `QMap<K, V>` -> [`BTreeMap<K, V>`].
pub type QMap<K, V> = BTreeMap<K, V>;

/// Canonical alias for Qt's `QSet<T>` -> [`HashSet<T>`].
pub type QSet<T> = HashSet<T>;

/// Canonical alias for Qt's `QMultiMap<K, V>` -> [`MultiMap<K, V>`].
pub type QMultiMap<K, V> = MultiMap<K, V>;

/// Canonical alias for Qt's `QMultiHash<K, V>` -> [`MultiHash<K, V>`].
pub type QMultiHash<K, V> = MultiHash<K, V>;

/// Canonical alias for Qt's `QBitArray` -> [`BitArray`].
pub type QBitArray = BitArray;

/// Canonical alias for Qt's `QPair<T1, T2>` -> Rust tuple `(T1, T2)`.
pub type QPair<T1, T2> = (T1, T2);

/// Canonical alias for Qt's `QSpan<T>` -> Rust borrowed slice `&[T]`.
pub type QSpan<'a, T> = &'a [T];

/// Canonical alias for Qt's `QSpan<T>` mutable -> Rust borrowed mutable slice `&mut [T]`.
pub type QSpanMut<'a, T> = &'a mut [T];

/// Canonical alias for stack/dynamically-allocated array -> [`Vec<T>`].
pub type VarLengthArray<T> = Vec<T>;

/// Canonical alias for Qt's `QVarLengthArray<T>` -> [`VarLengthArray<T>`].
pub type QVarLengthArray<T> = VarLengthArray<T>;

/// Canonical alias for shared immutable string -> [`Arc<str>`].
pub type SharedStr = Arc<str>;

/// Canonical alias for Qt's `QVariantList` -> [`Vec<Variant>`].
pub type QVariantList = Vec<Variant>;

/// Canonical alias for Qt's `QVariantMap` -> [`BTreeMap<String, Variant>`].
pub type QVariantMap = BTreeMap<String, Variant>;

/// Canonical alias for Qt's `QVariantHash` -> [`HashMap<String, Variant>`].
pub type QVariantHash = HashMap<String, Variant>;

/// Canonical alias for Qt's `QVariantPair` -> `(Variant, Variant)`.
pub type QVariantPair = (Variant, Variant);

/// Canonical alias for Qt's `QDate` -> [`Date`].
pub type QDate = Date;

/// Canonical alias for Qt's `QTime` -> [`Time`].
pub type QTime = Time;

/// Canonical alias for Qt's `QDateTime` -> [`DateTime`].
pub type QDateTime = DateTime;

/// Canonical alias for Qt's `QUrl` -> [`Url`].
pub type QUrl = Url;

/// Canonical alias for Qt's `QUrlQuery` -> [`UrlQuery`].
pub type QUrlQuery = UrlQuery;

/// Canonical alias for Qt's `QUuid` -> [`Uuid`].
pub type QUuid = Uuid;

/// Canonical alias for Qt's `QRegularExpression` -> [`RegularExpression`].
pub type QRegularExpression = RegularExpression;

/// Canonical alias for Qt's `QLocale` -> [`Locale`].
pub type QLocale = Locale;

/// Canonical alias for Qt's `QSize` -> [`Size`].
pub type QSize = Size;

/// Canonical alias for Qt's `QSizeF` -> [`SizeF`].
pub type QSizeF = SizeF;

/// Canonical alias for Qt's `QLine` -> [`Line`].
pub type QLine = Line;

/// Canonical alias for Qt's `QLineF` -> [`LineF`].
pub type QLineF = LineF;

/// Canonical alias for Qt's `QMargins` -> [`Margins`].
pub type QMargins = Margins;

/// Canonical alias for Qt's `QMarginsF` -> [`MarginsF`].
pub type QMarginsF = MarginsF;

/// Canonical alias for Qt's `QFile` -> [`crate::io::File`].
pub type QFile = crate::io::File;

/// Canonical alias for Qt's `QSaveFile` -> [`crate::io::SaveFile`].
pub type QSaveFile = crate::io::SaveFile;

/// Canonical alias for Qt's `QBuffer` -> [`crate::io::Buffer`].
pub type QBuffer = crate::io::Buffer;

/// Canonical alias for Qt's `QDir` -> [`crate::io::Directory`].
pub type QDir = crate::io::Directory;

/// Canonical alias for Qt's `QFileInfo` -> [`crate::io::FileInfo`].
pub type QFileInfo = crate::io::FileInfo;

/// Canonical alias for Qt's `QDirIterator` -> [`crate::io::DirIterator`].
pub type QDirIterator = crate::io::DirIterator;

/// Canonical alias for Qt's `QResource` -> [`crate::io::Resource`].
pub type QResource = crate::io::Resource;

/// Canonical alias for Qt's `QStandardPaths` -> [`crate::io::StandardPaths`].
pub type QStandardPaths = crate::io::StandardPaths;

/// Canonical alias for Qt's `QProcess` -> [`crate::io::Process`].
pub type QProcess = crate::io::Process;

/// Canonical alias for Qt's `QTemporaryFile` -> [`crate::io::TemporaryFile`].
pub type QTemporaryFile = crate::io::TemporaryFile;

/// Canonical alias for Qt's `QTemporaryDir` -> [`crate::io::TemporaryDir`].
pub type QTemporaryDir = crate::io::TemporaryDir;

/// Canonical alias for Qt's `QLockFile` -> [`crate::io::LockFile`].
pub type QLockFile = crate::io::LockFile;

/// Canonical alias for Qt's `QStorageInfo` -> [`crate::io::StorageInfo`].
pub type QStorageInfo = crate::io::StorageInfo;

/// Canonical alias for Qt's `QJsonValue` -> [`crate::json::JsonValue`].
pub type QJsonValue = crate::json::JsonValue;

/// Canonical alias for Qt's `QJsonObject` -> [`crate::json::JsonObject`].
pub type QJsonObject = crate::json::JsonObject;

/// Canonical alias for Qt's `QJsonArray` -> [`crate::json::JsonArray`].
pub type QJsonArray = crate::json::JsonArray;

/// Canonical alias for Qt's `QJsonDocument` -> [`crate::json::JsonDocument`].
pub type QJsonDocument = crate::json::JsonDocument;

/// Canonical alias for Qt's `QDataStream` -> [`crate::serialize::DataStream`].
pub type QDataStream = crate::serialize::DataStream;

/// Canonical alias for Qt's `QTextStream` -> [`crate::serialize::TextStream`].
pub type QTextStream = crate::serialize::TextStream;
