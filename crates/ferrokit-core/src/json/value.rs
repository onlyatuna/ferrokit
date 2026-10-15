// SPDX-License-Identifier: LGPL-3.0-only
//! QJsonValue equivalent representation and conversions.
//!
//! Encapsulates a JSON value: Null, Bool, Number (Double/Int), String, Array, or Object.
//! Provides type inspections, fallible and default-fallback extractors, and indexing operators.

use std::fmt;
use std::ops::Index;

use super::array::JsonArray;
use super::object::JsonObject;

/// Identifies the runtime data type of a `JsonValue` (`QJsonValue::Type` equivalent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum JsonType {
    #[default]
    Null = 0x0,
    Bool = 0x1,
    Double = 0x2,
    String = 0x3,
    Array = 0x4,
    Object = 0x5,
    Undefined = 0x80,
}

/// A polymorphic JSON value (`QJsonValue` equivalent).
#[derive(Clone, PartialEq, Default)]
pub enum JsonValue {
    #[default]
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(JsonArray),
    Object(JsonObject),
    Undefined,
}

impl JsonValue {
    /// Returns the type identifier of this JSON value.
    #[inline]
    pub fn value_type(&self) -> JsonType {
        match self {
            Self::Null => JsonType::Null,
            Self::Bool(_) => JsonType::Bool,
            Self::Number(_) => JsonType::Double,
            Self::String(_) => JsonType::String,
            Self::Array(_) => JsonType::Array,
            Self::Object(_) => JsonType::Object,
            Self::Undefined => JsonType::Undefined,
        }
    }

    #[inline]
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    #[inline]
    pub fn is_bool(&self) -> bool {
        matches!(self, Self::Bool(_))
    }

    #[inline]
    pub fn is_double(&self) -> bool {
        matches!(self, Self::Number(_))
    }

    #[inline]
    pub fn is_string(&self) -> bool {
        matches!(self, Self::String(_))
    }

    #[inline]
    pub fn is_array(&self) -> bool {
        matches!(self, Self::Array(_))
    }

    #[inline]
    pub fn is_object(&self) -> bool {
        matches!(self, Self::Object(_))
    }

    #[inline]
    pub fn is_undefined(&self) -> bool {
        matches!(self, Self::Undefined)
    }

    /// Converts to boolean with optional fallback value (`QJsonValue::toBool`).
    #[inline]
    pub fn to_bool(&self, default_value: bool) -> bool {
        match self {
            Self::Bool(b) => *b,
            _ => default_value,
        }
    }

    /// Converts to integer `i32` with optional fallback value (`QJsonValue::toInt`).
    #[inline]
    pub fn to_int(&self, default_value: i32) -> i32 {
        match self {
            Self::Number(n) => *n as i32,
            _ => default_value,
        }
    }

    /// Converts to 64-bit integer `i64` with optional fallback value (`QJsonValue::toInteger`).
    #[inline]
    pub fn to_i64(&self, default_value: i64) -> i64 {
        match self {
            Self::Number(n) => *n as i64,
            _ => default_value,
        }
    }

    /// Converts to floating point `f64` with optional fallback value (`QJsonValue::toDouble`).
    #[inline]
    pub fn to_double(&self, default_value: f64) -> f64 {
        match self {
            Self::Number(n) => *n,
            _ => default_value,
        }
    }

    /// Converts to a string reference if string, or returns default reference.
    #[inline]
    pub fn to_str<'a>(&'a self, default_value: &'a str) -> &'a str {
        match self {
            Self::String(s) => s.as_str(),
            _ => default_value,
        }
    }

    /// Converts to an owned `String` (`QJsonValue::toString`).
    #[inline]
    pub fn to_string_or(&self, default_value: &str) -> String {
        match self {
            Self::String(s) => s.clone(),
            _ => default_value.to_string(),
        }
    }

    /// Returns a reference to the inner `JsonArray`, if this value is an array.
    #[inline]
    pub fn as_array(&self) -> Option<&JsonArray> {
        match self {
            Self::Array(a) => Some(a),
            _ => None,
        }
    }

    /// Returns a mutable reference to the inner `JsonArray`, if this value is an array.
    #[inline]
    pub fn as_array_mut(&mut self) -> Option<&mut JsonArray> {
        match self {
            Self::Array(a) => Some(a),
            _ => None,
        }
    }

    /// Returns a reference to the inner `JsonObject`, if this value is an object.
    #[inline]
    pub fn as_object(&self) -> Option<&JsonObject> {
        match self {
            Self::Object(o) => Some(o),
            _ => None,
        }
    }

    /// Returns a mutable reference to the inner `JsonObject`, if this value is an object.
    #[inline]
    pub fn as_object_mut(&mut self) -> Option<&mut JsonObject> {
        match self {
            Self::Object(o) => Some(o),
            _ => None,
        }
    }

    /// Converts to `JsonArray`, returning cloned array or default (`QJsonValue::toArray`).
    pub fn to_array(&self, default_value: JsonArray) -> JsonArray {
        match self {
            Self::Array(a) => a.clone(),
            _ => default_value,
        }
    }

    /// Converts to `JsonObject`, returning cloned object or default (`QJsonValue::toObject`).
    pub fn to_object(&self, default_value: JsonObject) -> JsonObject {
        match self {
            Self::Object(o) => o.clone(),
            _ => default_value,
        }
    }

    /// Constructs a `JsonValue` from a `Variant` (`QJsonValue::fromVariant`).
    #[inline]
    pub fn from_variant(variant: &crate::variant::Variant) -> Self {
        variant.to_json_value()
    }

    /// Converts this `JsonValue` into a `Variant` (`QJsonValue::toVariant`).
    #[inline]
    pub fn to_variant(&self) -> crate::variant::Variant {
        crate::variant::Variant::from_json_value(self)
    }
}

impl fmt::Debug for JsonValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Null => write!(f, "null"),
            Self::Bool(b) => write!(f, "{b}"),
            Self::Number(n) => {
                if n.fract() == 0.0
                    && !n.is_infinite()
                    && !n.is_nan()
                    && *n >= (i64::MIN as f64)
                    && *n <= (i64::MAX as f64)
                {
                    write!(f, "{}", *n as i64)
                } else {
                    write!(f, "{n}")
                }
            }
            Self::String(s) => write!(f, "{s:?}"),
            Self::Array(a) => write!(f, "{a:?}"),
            Self::Object(o) => write!(f, "{o:?}"),
            Self::Undefined => write!(f, "undefined"),
        }
    }
}

// -----------------------------------------------------------------------------
// Indexing implementations (matching QJsonValue operator[])
// -----------------------------------------------------------------------------

static NULL_VALUE: JsonValue = JsonValue::Null;

impl Index<&str> for JsonValue {
    type Output = JsonValue;

    fn index(&self, key: &str) -> &Self::Output {
        match self {
            Self::Object(o) => o.get(key).unwrap_or(&NULL_VALUE),
            _ => &NULL_VALUE,
        }
    }
}

impl Index<usize> for JsonValue {
    type Output = JsonValue;

    fn index(&self, index: usize) -> &Self::Output {
        match self {
            Self::Array(a) => a.get(index).unwrap_or(&NULL_VALUE),
            _ => &NULL_VALUE,
        }
    }
}

// -----------------------------------------------------------------------------
// From conversions
// -----------------------------------------------------------------------------

impl From<bool> for JsonValue {
    #[inline]
    fn from(b: bool) -> Self {
        Self::Bool(b)
    }
}

impl From<i32> for JsonValue {
    #[inline]
    fn from(n: i32) -> Self {
        Self::Number(n as f64)
    }
}

impl From<u32> for JsonValue {
    #[inline]
    fn from(n: u32) -> Self {
        Self::Number(n as f64)
    }
}

impl From<i64> for JsonValue {
    #[inline]
    fn from(n: i64) -> Self {
        Self::Number(n as f64)
    }
}

impl From<u64> for JsonValue {
    #[inline]
    fn from(n: u64) -> Self {
        Self::Number(n as f64)
    }
}

impl From<f32> for JsonValue {
    #[inline]
    fn from(f: f32) -> Self {
        Self::Number(f as f64)
    }
}

impl From<f64> for JsonValue {
    #[inline]
    fn from(f: f64) -> Self {
        Self::Number(f)
    }
}

impl From<&str> for JsonValue {
    #[inline]
    fn from(s: &str) -> Self {
        Self::String(s.to_string())
    }
}

impl From<String> for JsonValue {
    #[inline]
    fn from(s: String) -> Self {
        Self::String(s)
    }
}

impl From<JsonArray> for JsonValue {
    #[inline]
    fn from(a: JsonArray) -> Self {
        Self::Array(a)
    }
}

impl From<JsonObject> for JsonValue {
    #[inline]
    fn from(o: JsonObject) -> Self {
        Self::Object(o)
    }
}
