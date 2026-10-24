// SPDX-License-Identifier: LGPL-3.0-only
//! Width and range rules of the text stream and of typed variant extraction.
//!
//! Qt references: `QTextStreamPrivate::putStringImpl` pads by the length of the text rather than
//! its byte size (qtextstream.cpp:874-897); `QVariant::toInt` of a `LongLong` outside the `int`
//! range fails (`ok == false`) instead of wrapping.

use ferrokit_core::serialize::{FieldAlignment, TextStream};
use ferrokit_core::variant::Variant;

#[test]
fn text_stream_pads_non_ascii_text_by_characters() {
    let mut ts = TextStream::new();
    ts.set_field_width(5);
    ts.set_pad_char('*');
    ts.set_field_alignment(FieldAlignment::Right);
    // Three characters, six bytes.
    ts.write_str("\u{e9}\u{e9}\u{e9}").unwrap();
    assert_eq!(ts.to_string(), "**\u{e9}\u{e9}\u{e9}");
}

#[test]
fn text_stream_does_not_pad_text_already_as_wide_as_the_field() {
    let mut ts = TextStream::new();
    ts.set_field_width(3);
    ts.set_pad_char('*');
    ts.write_str("\u{e9}\u{e9}\u{e9}").unwrap();
    assert_eq!(ts.to_string(), "\u{e9}\u{e9}\u{e9}");
}

#[test]
fn variant_to_i32_fails_outside_the_i32_range() {
    let too_big = i64::from(i32::MAX) + 1;
    assert_eq!(Variant::I64(too_big).to_value::<i32>(), None);
    assert_eq!(Variant::I64(i64::from(i32::MIN) - 1).to_value::<i32>(), None);
    assert_eq!(Variant::I64(i64::from(i32::MAX)).to_value::<i32>(), Some(i32::MAX));
    assert_eq!(Variant::I64(i64::from(i32::MIN)).to_value::<i32>(), Some(i32::MIN));
}

#[test]
fn variant_to_u32_fails_outside_the_u32_range() {
    let too_big = u64::from(u32::MAX) + 1;
    assert_eq!(Variant::U64(too_big).to_value::<u32>(), None);
    assert_eq!(Variant::U64(u64::from(u32::MAX)).to_value::<u32>(), Some(u32::MAX));
}
