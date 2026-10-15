// SPDX-License-Identifier: LGPL-3.0-only
//! JSON data modeling, parsing, and serialization (`QJsonDocument`, `QJsonObject`,
//! `QJsonArray`, and `QJsonValue` equivalents).
//!
//! Provides zero-dependency RFC 8259 JSON parsing, indented and compact formatting,
//! and bidirectional conversion to/from `Variant`.

pub mod array;
pub mod document;
pub mod object;
pub mod parser;
pub mod value;
pub mod writer;

pub use array::JsonArray;
pub use document::JsonDocument;
pub use object::JsonObject;
pub use parser::{JsonParseError, ParseErrorReason};
pub use value::{JsonType, JsonValue};
pub use writer::JsonFormat;

/// Parses arbitrary JSON text into a `JsonValue`.
pub fn from_str(json: &str) -> Result<JsonValue, JsonParseError> {
    from_slice(json.as_bytes())
}

/// Parses arbitrary JSON bytes into a `JsonValue`.
pub fn from_slice(json: &[u8]) -> Result<JsonValue, JsonParseError> {
    let mut parser = parser::Parser::new(json);
    parser.parse()
}

/// Serializes a `JsonValue` to a compact JSON string.
pub fn to_string(value: &JsonValue) -> String {
    writer::to_string(value, JsonFormat::Compact)
}

/// Serializes a `JsonValue` to an indented JSON string.
pub fn to_string_pretty(value: &JsonValue) -> String {
    writer::to_string(value, JsonFormat::Indented)
}
