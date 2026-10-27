// SPDX-License-Identifier: LGPL-3.0-only
//! Hostile-input hardening for the JSON parser, Variant/DataStream deserialization and types.

use ferrokit_core::*;

fn nested(open: &str, close: &str, n: usize) -> String {
    format!("{}{}", open.repeat(n), close.repeat(n))
}

fn on_big_stack<F: FnOnce() + Send + 'static>(f: F) {
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn json_nesting_at_cap_fits_a_2mb_stack() {
    // Parse, write and drop at the cap on a 2 MiB stack (debug builds included).
    std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(|| {
            let ok = nested("[", "]", MAX_NESTING_DEPTH);
            let doc = JsonDocument::from_json_str(&ok).unwrap();
            assert_eq!(doc.to_json_string(JsonFormat::Compact), ok);
            drop(doc);
            let ok_obj = format!(
                "{}1{}",
                "{\"a\":".repeat(MAX_NESTING_DEPTH),
                "}".repeat(MAX_NESTING_DEPTH)
            );
            let doc = JsonDocument::from_json_str(&ok_obj).unwrap();
            assert_eq!(doc.to_json_string(JsonFormat::Compact), ok_obj);
            let deep = nested("[", "]", MAX_NESTING_DEPTH + 1);
            let err = JsonDocument::from_json_str(&deep).unwrap_err();
            assert_eq!(err.reason, ParseErrorReason::DeepNesting);
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn json_deep_nesting_is_an_error_not_a_stack_overflow() {
    on_big_stack(|| {
        let ok = nested("[", "]", MAX_NESTING_DEPTH);
        assert!(JsonDocument::from_json_str(&ok).is_ok());
        let deep = "[".repeat(200_000);
        let err = JsonDocument::from_json_str(&deep).unwrap_err();
        assert_eq!(err.reason, ParseErrorReason::DeepNesting);
        let deep_obj = "{\"a\":".repeat(200_000);
        let err = JsonDocument::from_json_str(&deep_obj).unwrap_err();
        assert_eq!(err.reason, ParseErrorReason::DeepNesting);
    });
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
    on_big_stack(|| {
        // tag 26 (List), count 1, repeated.
        let mut bytes = Vec::new();
        for _ in 0..300_000 {
            bytes.push(26u8);
            bytes.extend_from_slice(&1u32.to_be_bytes());
        }
        assert!(Variant::from_bytes(&bytes).is_err());
    });
}

#[test]
fn variant_serialize_depth_cap_matches_deserialize() {
    // Deserialization accepts nesting depth 0..=128 (129 levels); serialization must agree.
    fn nest(levels: usize) -> Variant {
        let mut v = Variant::I64(1);
        for _ in 0..levels {
            v = Variant::List(vec![v]);
        }
        v
    }
    on_big_stack(|| {
        let at_cap = nest(128);
        let bytes = at_cap.to_bytes().expect("serialize at cap");
        assert_eq!(Variant::from_bytes(&bytes).unwrap(), at_cap);
        let over = nest(129);
        assert!(over.to_bytes().is_err());
    });
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
fn variant_f64_to_integer_rounds_half_away_from_zero() {
    for (input, expected) in [
        (1.5, 2),
        (-1.5, -2),
        (1.7, 2),
        (-1.7, -2),
        (1.4, 1),
        (0.5, 1),
        (-0.5, -1),
        (2.5, 3),
    ] {
        assert_eq!(Variant::F64(input).to_int(), Some(expected), "to_int({input})");
    }
    assert_eq!(Variant::F64(1.5).to_uint(), Some(2));
    assert_eq!(Variant::F64(1.7).to_uint(), Some(2));
    assert_eq!(Variant::F64(2.5).to_uint(), Some(3));
    assert_eq!(Variant::F64(-0.4).to_uint(), Some(0));
    assert_eq!(Variant::F64(-0.5).to_uint(), None);
    assert_eq!(Variant::F64(-1.5).to_uint(), None);
    assert_eq!(i32::from_variant(&Variant::F64(-1.5)), Some(-2));
    assert_eq!(u32::from_variant(&Variant::F64(1.5)), Some(2));
}

#[test]
fn variant_f64_non_finite_and_out_of_range_have_no_integer_value() {
    for f in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 1e19, -1e19] {
        assert_eq!(Variant::F64(f).to_int(), None, "to_int({f})");
    }
    for f in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 2e19, -1.0] {
        assert_eq!(Variant::F64(f).to_uint(), None, "to_uint({f})");
    }
}

#[test]
fn temporal_extremes_saturate() {
    let d = Date::new(i32::MAX, 6, 1);
    assert_eq!(d.add_years(i32::MAX).year, i32::MAX);
    assert_eq!(d.add_days(i64::MAX).year, i32::MAX);
    assert_eq!(d.add_days(i64::MIN).year, i32::MIN);
    assert_eq!(Date::new(i32::MIN, 6, 1).add_years(i32::MIN).year, i32::MIN);
    let t = Time {
        hour: u32::MAX,
        minute: u32::MAX,
        second: u32::MAX,
        millisecond: u32::MAX,
    };
    assert_eq!(t.msecs_since_start_of_day(), u32::MAX);
    assert!(t.add_msecs(i64::MAX).is_valid());
    assert!(t.add_msecs(i64::MIN).is_valid());
    assert_eq!(t.msecs_to(&Time::default()), -(u32::MAX as i64));
    // True wrap-around: one hour before midnight plus two hours is 01:00.
    let late = Time { hour: 23, minute: 0, second: 0, millisecond: 0 };
    assert_eq!(late.add_msecs(2 * 3_600_000).hour, 1);
    let max = DateTime::from_timestamp_ms(i64::MAX);
    let min = DateTime::from_timestamp_ms(i64::MIN);
    assert_eq!(max.add_msecs(i64::MAX).to_timestamp_ms(), i64::MAX);
    assert_eq!(min.add_msecs(i64::MIN).to_timestamp_ms(), i64::MIN);
    assert_eq!(min.msecs_to(&max), i64::MAX);
    assert_eq!(max.msecs_to(&min), i64::MIN);
    assert_eq!(DateTime::new(d, t).to_timestamp_ms(), i64::MAX);
}
