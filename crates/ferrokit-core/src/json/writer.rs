// SPDX-License-Identifier: LGPL-3.0-only
//! JSON serialization and formatting (Compact and Indented).
//!
//! Provides customizable JSON serialization matching `QJsonDocument::JsonFormat`.

use super::array::JsonArray;
use super::object::JsonObject;
use super::value::JsonValue;

/// Output layout format for JSON serialization (`QJsonDocument::JsonFormat` equivalent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum JsonFormat {
    /// Formatted with newlines and 4-space indentation for human readability.
    #[default]
    Indented,
    /// Minified without unnecessary whitespace.
    Compact,
}

/// Serializes a `JsonValue` to a JSON string.
pub fn to_string(value: &JsonValue, format: JsonFormat) -> String {
    let mut out = String::new();
    write_value(value, format, 0, &mut out);
    out
}

/// Serializes a `JsonObject` to a JSON string.
pub fn object_to_string(obj: &JsonObject, format: JsonFormat) -> String {
    let mut out = String::new();
    write_object(obj, format, 0, &mut out);
    out
}

/// Serializes a `JsonArray` to a JSON string.
pub fn array_to_string(arr: &JsonArray, format: JsonFormat) -> String {
    let mut out = String::new();
    write_array(arr, format, 0, &mut out);
    out
}

fn write_value(value: &JsonValue, format: JsonFormat, depth: usize, out: &mut String) {
    match value {
        JsonValue::Null => out.push_str("null"),
        JsonValue::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        JsonValue::Number(n) => {
            if n.is_nan() || n.is_infinite() {
                out.push_str("null");
            } else if n.fract() == 0.0 && *n >= (i64::MIN as f64) && *n < super::I64_EXCLUSIVE_UPPER_F64 {
                out.push_str(&format!("{}", *n as i64));
            } else {
                out.push_str(&format!("{n}"));
            }
        }
        JsonValue::String(s) => write_escaped_string(s, out),
        JsonValue::Array(a) => write_array(a, format, depth, out),
        JsonValue::Object(o) => write_object(o, format, depth, out),
        JsonValue::Undefined => out.push_str("null"),
    }
}

fn write_object(obj: &JsonObject, format: JsonFormat, depth: usize, out: &mut String) {
    if obj.is_empty() {
        out.push_str("{}");
        return;
    }

    out.push('{');
    let mut first = true;

    for (k, v) in obj.iter() {
        if !first {
            out.push(',');
        }
        first = false;

        match format {
            JsonFormat::Indented => {
                out.push('\n');
                write_indent(depth + 1, out);
                write_escaped_string(k, out);
                out.push_str(": ");
                write_value(v, format, depth + 1, out);
            }
            JsonFormat::Compact => {
                write_escaped_string(k, out);
                out.push(':');
                write_value(v, format, depth, out);
            }
        }
    }

    if format == JsonFormat::Indented {
        out.push('\n');
        write_indent(depth, out);
    }
    out.push('}');
}

fn write_array(arr: &JsonArray, format: JsonFormat, depth: usize, out: &mut String) {
    if arr.is_empty() {
        out.push_str("[]");
        return;
    }

    out.push('[');
    let mut first = true;

    for v in arr.iter() {
        if !first {
            out.push(',');
        }
        first = false;

        match format {
            JsonFormat::Indented => {
                out.push('\n');
                write_indent(depth + 1, out);
                write_value(v, format, depth + 1, out);
            }
            JsonFormat::Compact => {
                write_value(v, format, depth, out);
            }
        }
    }

    if format == JsonFormat::Indented {
        out.push('\n');
        write_indent(depth, out);
    }
    out.push(']');
}

fn write_indent(depth: usize, out: &mut String) {
    for _ in 0..depth {
        out.push_str("    ");
    }
}

fn write_escaped_string(s: &str, out: &mut String) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\x08' => out.push_str("\\b"),
            '\x0c' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}
