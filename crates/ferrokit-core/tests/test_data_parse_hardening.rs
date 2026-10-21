// SPDX-License-Identifier: LGPL-3.0-only
//! Hostile-input hardening for the JSON parser, Variant/DataStream deserialization and types.

use ferrokit_core::*;

fn nested(open: &str, close: &str, n: usize) -> String {
    format!("{}{}", open.repeat(n), close.repeat(n))
}

#[test]
fn json_deep_nesting_is_an_error_not_a_stack_overflow() {
    let ok = nested("[", "]", 1024);
    assert!(JsonDocument::from_json_str(&ok).is_ok());
    let deep = "[".repeat(200_000);
    let err = JsonDocument::from_json_str(&deep).unwrap_err();
    assert_eq!(err.reason, ParseErrorReason::DeepNesting);
    let deep_obj = "{\"a\":".repeat(200_000);
    let err = JsonDocument::from_json_str(&deep_obj).unwrap_err();
    assert_eq!(err.reason, ParseErrorReason::DeepNesting);
}

#[test]
fn json_non_ascii_strings_round_trip() {
    let doc = JsonDocument::from_json_str("[\"é€😀\"]").unwrap();
    assert_eq!(doc.to_json_string(JsonFormat::Compact), "[\"é€😀\"]");
    let doc = JsonDocument::from_json_str("{\"ключ\":1}").unwrap();
    assert!(doc.to_json_string(JsonFormat::Compact).contains("ключ"));
}

#[test]
fn json_invalid_utf8_rejected() {
    let err = JsonDocument::from_json(b"[\"\xff\xfe\"]").unwrap_err();
    assert_eq!(err.reason, ParseErrorReason::IllegalUtf8String);
}

#[test]
fn json_leading_zeros_rejected() {
    assert!(JsonDocument::from_json_str("[007]").is_err());
    assert!(JsonDocument::from_json_str("[-01]").is_err());
    assert!(JsonDocument::from_json_str("[0]").is_ok());
    assert!(JsonDocument::from_json_str("[0.5,-0,10]").is_ok());
}

#[test]
fn json_lone_high_surrogate_keeps_following_escape() {
    let doc = JsonDocument::from_json_str("[\"\\uD800\\u0041\"]").unwrap();
    assert_eq!(doc.to_json_string(JsonFormat::Compact), "[\"\u{FFFD}A\"]");
}

#[test]
fn json_two_pow_63_is_not_written_as_i64_max() {
    let doc = JsonDocument::from_json_str("[9223372036854775808]").unwrap();
    assert_ne!(
        doc.to_json_string(JsonFormat::Compact),
        "[9223372036854775807]"
    );
    let v = Variant::from_json_value(&JsonValue::Number(9_223_372_036_854_775_808.0));
    assert!(!matches!(v, Variant::I64(i64::MAX)));
}

#[test]
fn variant_deserialize_deep_nesting_is_an_error() {
    // tag 26 (List), count 1, repeated.
    let mut bytes = Vec::new();
    for _ in 0..300_000 {
        bytes.push(26u8);
        bytes.extend_from_slice(&1u32.to_be_bytes());
    }
    assert!(Variant::from_bytes(&bytes).is_err());
}

#[test]
fn variant_deserialize_huge_counts_do_not_preallocate() {
    for tag in [26u8, 27u8] {
        let mut bytes = vec![tag];
        bytes.extend_from_slice(&u32::MAX.to_be_bytes());
        assert!(Variant::from_bytes(&bytes).is_err());
    }
}

#[test]
fn data_stream_huge_lengths_fail_cleanly() {
    let huge = u32::MAX.to_be_bytes();
    assert!(DataStream::reader(&huge).read_string_list().is_err());
    assert!(DataStream::reader(&huge).read_bytes().is_err());
    assert!(DataStream::reader(&huge).read_string().is_err());
}

#[test]
fn data_stream_large_bytes_round_trip() {
    let payload: Vec<u8> = (0..200_000u32).map(|i| i as u8).collect();
    let mut bytes = (payload.len() as u32).to_be_bytes().to_vec();
    bytes.extend_from_slice(&payload);
    assert_eq!(DataStream::reader(&bytes).read_bytes().unwrap(), payload);
}

#[test]
fn byte_array_from_hex_non_ascii_does_not_panic() {
    assert!(ByteArray::from_hex("é").is_err());
    assert!(ByteArray::from_hex("aé").is_err());
}

#[test]
fn uuid_from_string_non_ascii_does_not_panic() {
    let s = format!("{}é{}", "a".repeat(29), "b");
    assert_eq!(s.len(), 32);
    assert!(Uuid::from_string(&s).is_none());
    assert!(Uuid::from_string("+f000000-0000-0000-0000-000000000000").is_none());
}

#[test]
fn variant_u64_above_i64_max_has_no_int_value() {
    assert_eq!(Variant::U64(u64::MAX).to_int(), None);
    assert_eq!(Variant::U64(5).to_int(), Some(5));
}

#[test]
fn temporal_extremes_do_not_overflow() {
    let d = Date::new(i32::MAX, 6, 1);
    let _ = d.add_years(i32::MAX);
    let _ = d.add_days(i64::MAX);
    let _ = d.add_days(i64::MIN);
    let _ = d.to_epoch_days();
    let t = Time {
        hour: u32::MAX,
        minute: u32::MAX,
        second: u32::MAX,
        millisecond: u32::MAX,
    };
    let _ = t.msecs_since_start_of_day();
    let _ = t.add_msecs(i64::MAX);
    let _ = t.msecs_to(&Time::default());
    let dt = DateTime::from_timestamp_ms(i64::MAX);
    let _ = dt.add_msecs(i64::MAX);
    let _ = dt.msecs_to(&DateTime::from_timestamp_ms(i64::MIN));
    let _ = DateTime::new(d, t);
}
