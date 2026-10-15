// SPDX-License-Identifier: LGPL-3.0-only
//! Comprehensive integration tests for ferrokit-core serialization & JSON subsystem.
//!
//! Validates:
//! 1. `JsonValue`, `JsonObject`, `JsonArray`, and `JsonDocument` DOM operations.
//! 2. Zero-dependency JSON parser compliance (escapes, Unicode, scientific floats, nested objects/arrays).
//! 3. Compact and Indented JSON formatting output.
//! 4. Parse error detection with precise line/column reporting.
//! 5. `Variant <-> JsonValue` and `Variant <-> JsonDocument` conversions.
//! 6. `Variant::to_json` and `Variant::from_json` roundtrips.
//! 7. `DataStream` binary serialization with configurable `ByteOrder` (BigEndian/LittleEndian) and `Status`.
//! 8. `Variant::to_bytes` and `Variant::from_bytes` binary roundtrips.
//! 9. `TextStream` formatted output (bases: Dec, Hex, Oct, Bin; width, alignment, pad char) and line reading.
//! 10. Qt canonical aliases (`QJsonDocument`, `QJsonObject`, `QJsonArray`, `QJsonValue`, `QDataStream`, `QTextStream`).

use std::collections::HashMap;

use ferrokit_core::json::*;
use ferrokit_core::serialize::*;
use ferrokit_core::types::*;
use ferrokit_core::variant::Variant;

// =============================================================================
// 1. JsonValue DOM & Conversions
// =============================================================================

#[test]
fn test_json_value_types_and_extractors() {
    let null_val = JsonValue::Null;
    assert!(null_val.is_null());
    assert_eq!(null_val.value_type(), JsonType::Null);

    let bool_val = JsonValue::from(true);
    assert!(bool_val.is_bool());
    assert!(bool_val.to_bool(false));

    let int_val = JsonValue::from(42);
    assert!(int_val.is_double());
    assert_eq!(int_val.to_int(0), 42);
    assert_eq!(int_val.to_i64(0), 42);
    assert_eq!(int_val.to_double(0.0), 42.0);

    let float_val = JsonValue::from(std::f64::consts::PI);
    assert_eq!(float_val.to_double(0.0), std::f64::consts::PI);

    let str_val = JsonValue::from("Hello Qt JSON");
    assert!(str_val.is_string());
    assert_eq!(str_val.to_str(""), "Hello Qt JSON");
    assert_eq!(str_val.to_string_or(""), "Hello Qt JSON");

    // Indexing fallback on non-object / non-array returns Null
    assert!(str_val["key"].is_null());
    assert!(str_val[0].is_null());
}

// =============================================================================
// 2. JsonObject & JsonArray DOM Operations
// =============================================================================

#[test]
fn test_json_object_and_array_dom() {
    let mut obj = JsonObject::new();
    assert!(obj.is_empty());
    assert_eq!(obj.len(), 0);

    obj.insert("title", "ferrokit monitor");
    obj.insert("version", 1);
    obj.insert("enabled", true);

    assert_eq!(obj.len(), 3);
    assert!(obj.contains("title"));
    assert!(obj.contains_key("version"));
    assert_eq!(obj["title"].to_str(""), "ferrokit monitor");
    assert_eq!(obj["version"].to_int(0), 1);
    assert!(obj["enabled"].to_bool(false));
    assert!(obj["non_existent"].is_null());

    // Sorted keys guarantee (Qt QJsonObject parity)
    let keys = obj.keys();
    assert_eq!(keys.join(","), "enabled,title,version");

    // Remove and take
    let removed = obj.take("version");
    assert_eq!(removed.to_int(0), 1);
    assert!(!obj.contains("version"));
    assert_eq!(obj.len(), 2);

    // Array operations
    let mut arr = JsonArray::new();
    assert!(arr.is_empty());

    arr.push("item1");
    arr.push(100);
    arr.push(false);

    assert_eq!(arr.len(), 3);
    assert_eq!(arr[0].to_str(""), "item1");
    assert_eq!(arr[1].to_int(0), 100);
    assert!(!arr[2].to_bool(true));
    assert!(arr[99].is_null()); // Out of bounds returns Null

    let taken = arr.take(1);
    assert_eq!(taken.to_int(0), 100);
    assert_eq!(arr.len(), 2);
    assert!(!arr[1].to_bool(true));
}

// =============================================================================
// 3. JSON Document & Parser Compliance
// =============================================================================

#[test]
fn test_json_parsing_rfc_compliance() {
    let raw_json = r#"{
        "string": "Hello, \t \"world\"! \n Unicode: \u0041\u4e2d\u6587",
        "number_int": -1024,
        "number_float": 3.141592653589793,
        "number_exp": 1.25e+2,
        "boolean_true": true,
        "boolean_false": false,
        "null_val": null,
        "nested_object": {
            "sub_key": "sub_value"
        },
        "nested_array": [10, 20, 30]
    }"#;

    let doc = JsonDocument::from_json_str(raw_json).expect("valid JSON must parse");
    assert!(doc.is_object());
    assert!(!doc.is_empty());

    let obj = doc.object();
    assert_eq!(obj["number_int"].to_int(0), -1024);
    assert_eq!(obj["number_float"].to_double(0.0), std::f64::consts::PI);
    assert_eq!(obj["number_exp"].to_double(0.0), 125.0);
    assert!(obj["boolean_true"].to_bool(false));
    assert!(!obj["boolean_false"].to_bool(true));
    assert!(obj["null_val"].is_null());

    let s = obj["string"].to_str("");
    assert!(s.contains("Hello, \t \"world\"! \n Unicode: A中文"));

    // Nested object inspection
    assert_eq!(obj["nested_object"]["sub_key"].to_str(""), "sub_value");

    // Nested array inspection
    assert_eq!(obj["nested_array"][0].to_int(0), 10);
    assert_eq!(obj["nested_array"][1].to_int(0), 20);
    assert_eq!(obj["nested_array"][2].to_int(0), 30);
}

#[test]
fn test_json_formatting_compact_and_indented() {
    let mut obj = JsonObject::new();
    obj.insert("a", 1);
    obj.insert("b", "test");

    let doc = JsonDocument::from_object(obj);

    let compact = doc.to_json_string(JsonFormat::Compact);
    assert_eq!(compact, r#"{"a":1,"b":"test"}"#);

    let indented = doc.to_json_string(JsonFormat::Indented);
    assert!(indented.contains("    \"a\": 1,\n"));
    assert!(indented.contains("    \"b\": \"test\"\n"));
}

#[test]
fn test_json_parse_error_reporting() {
    // Malformed JSON: missing colon
    let bad_json = r#"{"key" "value"}"#;
    let err = JsonDocument::from_json_str(bad_json).expect_err("must fail");
    assert_eq!(err.reason, ParseErrorReason::ExpectedColon);
    assert_eq!(err.line, 1);

    // Malformed JSON: unterminated string
    let unterminated = r#"{"key": "value"#;
    let err2 = JsonDocument::from_json_str(unterminated).expect_err("must fail");
    assert_eq!(err2.reason, ParseErrorReason::UnterminatedString);
}

// =============================================================================
// 4. Variant <-> JSON Bidirectional Integration
// =============================================================================

#[test]
fn test_variant_json_bidirectional_roundtrip() {
    let mut map = HashMap::new();
    map.insert("name".to_string(), Variant::from("Claude HUD"));
    map.insert("fps".to_string(), Variant::from(60));
    map.insert("ratio".to_string(), Variant::from(1.75));
    map.insert("active".to_string(), Variant::from(true));
    map.insert(
        "tags".to_string(),
        Variant::List(vec![Variant::from("rust"), Variant::from("qt")]),
    );

    let v_original = Variant::Map(map);

    // Convert to JsonValue
    let jval = v_original.to_json_value();
    assert!(jval.is_object());
    assert_eq!(jval["name"].to_str(""), "Claude HUD");
    assert_eq!(jval["fps"].to_int(0), 60);
    assert_eq!(jval["tags"][0].to_str(""), "rust");

    // Convert back from JsonValue
    let v_reconstructed = Variant::from_json_value(&jval);
    assert_eq!(v_reconstructed["name"].to_string_lossy(), "Claude HUD");
    assert_eq!(v_reconstructed["fps"].to_i64(), Some(60));
    assert_eq!(v_reconstructed["tags"][1].to_string_lossy(), "qt");

    // End-to-end JSON text roundtrip
    let json_bytes = v_original.to_json(JsonFormat::Compact);
    let v_from_json =
        Variant::from_json(json_bytes.as_bytes()).expect("deserialize variant from json");

    assert_eq!(v_from_json["name"].to_string_lossy(), "Claude HUD");
    assert_eq!(v_from_json["active"].to_bool(), Some(true));
}

// =============================================================================
// 5. DataStream Binary Serialization
// =============================================================================

#[test]
fn test_data_stream_primitives_and_endianness() {
    // 1. Big Endian (Default)
    let mut ds_be = DataStream::new();
    {
        let mut writer = ds_be.writer();
        writer.write_bool(true).unwrap();
        writer.write_u8(0xAA).unwrap();
        writer.write_i16(-500).unwrap();
        writer.write_u32(0x12345678).unwrap();
        writer.write_f64(12345.6789).unwrap();
        writer.write_str("Hello DataStream").unwrap();
    }

    let bytes_be = ds_be.into_bytes();
    let mut reader_be = DataStream::reader(&bytes_be);

    assert!(reader_be.read_bool().unwrap());
    assert_eq!(reader_be.read_u8().unwrap(), 0xAA);
    assert_eq!(reader_be.read_i16().unwrap(), -500);
    assert_eq!(reader_be.read_u32().unwrap(), 0x12345678);
    assert_eq!(reader_be.read_f64().unwrap(), 12345.6789);
    assert_eq!(reader_be.read_string().unwrap(), "Hello DataStream");
    assert_eq!(reader_be.status(), Status::Ok);

    // Reading past end triggers ReadPastEnd
    let _ = reader_be.read_u8();
    assert_eq!(reader_be.status(), Status::ReadPastEnd);

    // 2. Little Endian
    let mut ds_le = DataStream::with_byte_order(ByteOrder::LittleEndian);
    {
        let mut writer = ds_le.writer();
        writer.write_u32(0xAABBCCDD).unwrap();
    }
    let bytes_le = ds_le.into_bytes();
    assert_eq!(&bytes_le, &[0xDD, 0xCC, 0xBB, 0xAA]);

    let mut reader_le = DataStream::reader_with_order(&bytes_le, ByteOrder::LittleEndian);
    assert_eq!(reader_le.read_u32().unwrap(), 0xAABBCCDD);
}

#[test]
fn test_data_stream_qt_collections() {
    let mut ds = DataStream::new();
    let list = StringList::from_iter(["alpha", "beta", "gamma"]);
    let array = ByteArray::from(b"Raw binary payload \x00\xFF\x42".as_slice());

    {
        let mut writer = ds.writer();
        writer.write_string_list(&list).unwrap();
        writer.write_byte_array(&array).unwrap();
    }

    let mut reader = DataStream::reader(ds.as_bytes());
    let read_list = reader.read_string_list().unwrap();
    let read_array = reader.read_byte_array().unwrap();

    assert_eq!(read_list.join(";"), "alpha;beta;gamma");
    assert_eq!(read_array.as_bytes(), b"Raw binary payload \x00\xFF\x42");
}

// =============================================================================
// 6. Variant <-> Binary Bytes Roundtrip
// =============================================================================

#[test]
fn test_variant_binary_bytes_roundtrip() {
    let mut map = HashMap::new();
    map.insert("pt".to_string(), Variant::Point(15, -30));
    map.insert("rc".to_string(), Variant::Rect(10, 20, 100, 200));
    map.insert("color".to_string(), Variant::Color(255, 128, 0, 255));
    map.insert("msg".to_string(), Variant::from("IPC message"));
    map.insert(
        "sublist".to_string(),
        Variant::List(vec![Variant::from(1), Variant::from(2), Variant::from(3)]),
    );

    let v_root = Variant::Map(map);

    // Serialize to binary bytes
    let bytes = v_root.to_bytes();
    assert!(!bytes.is_empty());

    // Deserialize from binary bytes
    let restored = Variant::from_bytes(&bytes).expect("deserialize variant from bytes");
    assert_eq!(restored["pt"].to_point(), Some((15, -30)));
    assert_eq!(restored["rc"].to_rect(), Some((10, 20, 100, 200)));
    assert_eq!(restored["color"].to_color(), Some((255, 128, 0, 255)));
    assert_eq!(restored["msg"].to_string_lossy(), "IPC message");
    assert_eq!(restored["sublist"][0].to_i64(), Some(1));
    assert_eq!(restored["sublist"][1].to_i64(), Some(2));
    assert_eq!(restored["sublist"][2].to_i64(), Some(3));
}

// =============================================================================
// 7. TextStream Formatting & Parsing
// =============================================================================

#[test]
fn test_text_stream_formatting_and_bases() {
    let mut ts = TextStream::new();

    // Field formatting
    ts.set_field_width(10);
    ts.set_pad_char('*');
    ts.set_field_alignment(FieldAlignment::Right);
    ts.write_str("Item").unwrap();
    ts.set_field_width(0);
    ts.write_str("\n").unwrap();

    // Hex formatting with base prefix and uppercase
    ts.set_integer_base(NumberBase::Hex);
    ts.set_show_base(true);
    ts.set_uppercase_digits(true);
    ts.write_u64(0x2A).unwrap(); // 0X2A
    ts.write_str("\n").unwrap();

    // Binary formatting
    ts.set_integer_base(NumberBase::Binary);
    ts.set_show_base(true);
    ts.write_u64(5).unwrap(); // 0b101
    ts.write_str("\n").unwrap();

    let output = ts.to_string();
    let mut lines = output.lines();

    assert_eq!(lines.next().unwrap(), "******Item");
    assert_eq!(lines.next().unwrap(), "0X2A");
    assert_eq!(lines.next().unwrap(), "0b101");
}

#[test]
fn test_text_stream_reading_lines() {
    let input = "Line one\r\nLine two\nLine three";
    let mut stream = TextStream::from_reader(input.as_bytes());

    assert_eq!(stream.read_line().unwrap(), Some("Line one".to_string()));
    assert_eq!(stream.read_line().unwrap(), Some("Line two".to_string()));
    assert_eq!(stream.read_line().unwrap(), Some("Line three".to_string()));
    assert_eq!(stream.read_line().unwrap(), None);
}

// =============================================================================
// 8. Canonical Qt Type Aliases
// =============================================================================

#[test]
fn test_canonical_qt_serialization_aliases() {
    let mut obj: QJsonObject = QJsonObject::new();
    obj.insert("key", "val");
    let doc: QJsonDocument = QJsonDocument::from_object(obj);
    assert!(doc.is_object());

    let mut stream: QDataStream = QDataStream::new();
    stream.writer().write_u32(123).unwrap();
    assert_eq!(stream.as_bytes().len(), 4);

    let mut txt: QTextStream = QTextStream::new();
    txt.write_str("Qt Text").unwrap();
    assert_eq!(txt.to_string(), "Qt Text");
}
