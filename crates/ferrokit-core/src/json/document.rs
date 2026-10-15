// SPDX-License-Identifier: LGPL-3.0-only
//! QJsonDocument equivalent representation.
//!
//! Encapsulates a complete JSON document containing either a root `JsonObject`,
//! a root `JsonArray`, or empty/null.

use std::fmt;
use std::ops::Index;

use super::array::JsonArray;
use super::object::JsonObject;
use super::parser::{JsonParseError, ParseErrorReason, Parser};
use super::value::JsonValue;
use super::writer::{self, JsonFormat};
use crate::types::ByteArray;

static EMPTY_OBJECT: JsonObject = JsonObject {
    map: std::collections::BTreeMap::new(),
};
static EMPTY_ARRAY: JsonArray = JsonArray { list: Vec::new() };
static NULL_VALUE: JsonValue = JsonValue::Null;

#[derive(Clone, PartialEq, Default)]
enum DocumentContent {
    #[default]
    Empty,
    Object(JsonObject),
    Array(JsonArray),
}

/// A top-level JSON document (`QJsonDocument` equivalent).
#[derive(Clone, PartialEq, Default)]
pub struct JsonDocument {
    content: DocumentContent,
}

impl JsonDocument {
    /// Constructs an empty JSON document (`QJsonDocument::QJsonDocument()`).
    #[inline]
    pub fn new() -> Self {
        Self {
            content: DocumentContent::Empty,
        }
    }

    /// Constructs a JSON document from a root `JsonObject` (`QJsonDocument(const QJsonObject &)`).
    #[inline]
    pub fn from_object(object: JsonObject) -> Self {
        Self {
            content: DocumentContent::Object(object),
        }
    }

    /// Constructs a JSON document from a root `JsonArray` (`QJsonDocument(const QJsonArray &)`).
    #[inline]
    pub fn from_array(array: JsonArray) -> Self {
        Self {
            content: DocumentContent::Array(array),
        }
    }

    /// Parses a JSON document from a byte slice (`QJsonDocument::fromJson`).
    pub fn from_json(json: &[u8]) -> Result<Self, JsonParseError> {
        let mut parser = Parser::new(json);
        let value = parser.parse()?;

        match value {
            JsonValue::Object(o) => Ok(Self::from_object(o)),
            JsonValue::Array(a) => Ok(Self::from_array(a)),
            _ => Err(JsonParseError {
                offset: 0,
                line: 1,
                column: 1,
                reason: ParseErrorReason::UnexpectedChar(' '),
            }),
        }
    }

    /// Parses a JSON document from a string slice.
    #[inline]
    pub fn from_json_str(json: &str) -> Result<Self, JsonParseError> {
        Self::from_json(json.as_bytes())
    }

    /// Constructs a JSON document from a `Variant` (`QJsonDocument::fromVariant`).
    pub fn from_variant(variant: &crate::variant::Variant) -> Self {
        let val = variant.to_json_value();
        match val {
            JsonValue::Object(o) => Self::from_object(o),
            JsonValue::Array(a) => Self::from_array(a),
            _ => Self::new(),
        }
    }

    /// Converts the JSON document into a `Variant` (`QJsonDocument::toVariant`).
    pub fn to_variant(&self) -> crate::variant::Variant {
        match &self.content {
            DocumentContent::Empty => crate::variant::Variant::Invalid,
            DocumentContent::Object(o) => {
                crate::variant::Variant::from_json_value(&JsonValue::Object(o.clone()))
            }
            DocumentContent::Array(a) => {
                crate::variant::Variant::from_json_value(&JsonValue::Array(a.clone()))
            }
        }
    }

    /// Returns `true` if the document contains a root object (`QJsonDocument::isObject`).
    #[inline]
    pub fn is_object(&self) -> bool {
        matches!(self.content, DocumentContent::Object(_))
    }

    /// Returns `true` if the document contains a root array (`QJsonDocument::isArray`).
    #[inline]
    pub fn is_array(&self) -> bool {
        matches!(self.content, DocumentContent::Array(_))
    }

    /// Returns `true` if the document is empty (`QJsonDocument::isEmpty`).
    #[inline]
    pub fn is_empty(&self) -> bool {
        match &self.content {
            DocumentContent::Empty => true,
            DocumentContent::Object(o) => o.is_empty(),
            DocumentContent::Array(a) => a.is_empty(),
        }
    }

    /// Returns `true` if the document is uninitialized / null (`QJsonDocument::isNull`).
    #[inline]
    pub fn is_null(&self) -> bool {
        matches!(self.content, DocumentContent::Empty)
    }

    /// Returns a reference to the root `JsonObject` (`QJsonDocument::object`).
    pub fn object(&self) -> &JsonObject {
        match &self.content {
            DocumentContent::Object(o) => o,
            _ => &EMPTY_OBJECT,
        }
    }

    /// Returns a mutable reference to the root `JsonObject`, converting the document to an object if needed.
    pub fn object_mut(&mut self) -> &mut JsonObject {
        if !self.is_object() {
            self.content = DocumentContent::Object(JsonObject::new());
        }
        match &mut self.content {
            DocumentContent::Object(o) => o,
            _ => unreachable!(),
        }
    }

    /// Returns a reference to the root `JsonArray` (`QJsonDocument::array`).
    pub fn array(&self) -> &JsonArray {
        match &self.content {
            DocumentContent::Array(a) => a,
            _ => &EMPTY_ARRAY,
        }
    }

    /// Returns a mutable reference to the root `JsonArray`, converting the document to an array if needed.
    pub fn array_mut(&mut self) -> &mut JsonArray {
        if !self.is_array() {
            self.content = DocumentContent::Array(JsonArray::new());
        }
        match &mut self.content {
            DocumentContent::Array(a) => a,
            _ => unreachable!(),
        }
    }

    /// Replaces the document content with `object` (`QJsonDocument::setObject`).
    #[inline]
    pub fn set_object(&mut self, object: JsonObject) {
        self.content = DocumentContent::Object(object);
    }

    /// Replaces the document content with `array` (`QJsonDocument::setArray`).
    #[inline]
    pub fn set_array(&mut self, array: JsonArray) {
        self.content = DocumentContent::Array(array);
    }

    /// Serializes the document to a UTF-8 string according to `format`.
    pub fn to_json_string(&self, format: JsonFormat) -> String {
        match &self.content {
            DocumentContent::Empty => String::new(),
            DocumentContent::Object(o) => writer::object_to_string(o, format),
            DocumentContent::Array(a) => writer::array_to_string(a, format),
        }
    }

    /// Serializes the document to a `ByteArray` according to `format` (`QJsonDocument::toJson`).
    pub fn to_json(&self, format: JsonFormat) -> ByteArray {
        ByteArray::from(self.to_json_string(format).into_bytes())
    }
}

impl Index<&str> for JsonDocument {
    type Output = JsonValue;

    #[inline]
    fn index(&self, key: &str) -> &Self::Output {
        match &self.content {
            DocumentContent::Object(o) => o.get(key).unwrap_or(&NULL_VALUE),
            _ => &NULL_VALUE,
        }
    }
}

impl Index<usize> for JsonDocument {
    type Output = JsonValue;

    #[inline]
    fn index(&self, index: usize) -> &Self::Output {
        match &self.content {
            DocumentContent::Array(a) => a.get(index).unwrap_or(&NULL_VALUE),
            _ => &NULL_VALUE,
        }
    }
}

impl fmt::Debug for JsonDocument {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.content {
            DocumentContent::Empty => write!(f, "JsonDocument(Empty)"),
            DocumentContent::Object(o) => write!(f, "JsonDocument({o:?})"),
            DocumentContent::Array(a) => write!(f, "JsonDocument({a:?})"),
        }
    }
}
