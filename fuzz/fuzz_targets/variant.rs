#![no_main]
//! `Variant` binary format: arbitrary bytes must deserialize or fail cleanly, a deserialized
//! value must serialize back to a value that deserializes equal, and every generated `Variant`
//! must survive serialize -> deserialize.

use arbitrary::Unstructured;
use ferrokit_core::variant::Variant;
use libfuzzer_sys::fuzz_target;
use std::collections::HashMap;

fuzz_target!(|data: &[u8]| {
    if let Ok(v) = Variant::from_bytes(data) {
        check_round_trip(&v);
    }
    let mut u = Unstructured::new(data);
    if let Ok(v) = gen(&mut u, 0) {
        check_round_trip(&v);
    }
});

fn check_round_trip(v: &Variant) {
    let bytes = v.to_bytes();
    let back = Variant::from_bytes(&bytes)
        .unwrap_or_else(|e| panic!("serialized Variant does not deserialize ({e}): {v:?}"));
    assert!(same(v, &back), "round trip changed {v:?} into {back:?}");
}

/// `Variant` equality with floats compared bit-for-bit (NaN != NaN under `PartialEq`).
fn same(a: &Variant, b: &Variant) -> bool {
    use Variant::*;
    let f32s = |x: &[f32], y: &[f32]| x.iter().zip(y).all(|(p, q)| p.to_bits() == q.to_bits());
    match (a, b) {
        (F64(x), F64(y)) => x.to_bits() == y.to_bits(),
        (PointF(a1, a2), PointF(b1, b2)) | (SizeF(a1, a2), SizeF(b1, b2)) => {
            f32s(&[*a1, *a2], &[*b1, *b2])
        }
        (RectF(a1, a2, a3, a4), RectF(b1, b2, b3, b4))
        | (LineF(a1, a2, a3, a4), LineF(b1, b2, b3, b4))
        | (MarginsF(a1, a2, a3, a4), MarginsF(b1, b2, b3, b4)) => {
            f32s(&[*a1, *a2, *a3, *a4], &[*b1, *b2, *b3, *b4])
        }
        (List(x), List(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q)),
        (Map(x), Map(y)) => {
            x.len() == y.len() && x.iter().all(|(k, p)| y.get(k).is_some_and(|q| same(p, q)))
        }
        _ => a == b,
    }
}

fn gen(u: &mut Unstructured<'_>, depth: u32) -> arbitrary::Result<Variant> {
    let kinds = if depth >= 8 { 25 } else { 27 };
    Ok(match u.int_in_range(0..=kinds)? {
        0 => Variant::Invalid,
        1 => Variant::Bool(u.arbitrary()?),
        2 => Variant::I64(u.arbitrary()?),
        3 => Variant::U64(u.arbitrary()?),
        4 => Variant::F64(f64::from_bits(u.arbitrary()?)),
        5 => Variant::String(u.arbitrary()?),
        6 => Variant::ByteArray(u.arbitrary()?),
        7 => Variant::Point(u.arbitrary()?, u.arbitrary()?),
        8 => Variant::PointF(f32::from_bits(u.arbitrary()?), f32::from_bits(u.arbitrary()?)),
        9 => Variant::Size(u.arbitrary()?, u.arbitrary()?),
        10 => Variant::SizeF(f32::from_bits(u.arbitrary()?), f32::from_bits(u.arbitrary()?)),
        11 => Variant::Rect(u.arbitrary()?, u.arbitrary()?, u.arbitrary()?, u.arbitrary()?),
        12 => Variant::RectF(f(u)?, f(u)?, f(u)?, f(u)?),
        13 => Variant::Line(u.arbitrary()?, u.arbitrary()?, u.arbitrary()?, u.arbitrary()?),
        14 => Variant::LineF(f(u)?, f(u)?, f(u)?, f(u)?),
        15 => Variant::Margins(u.arbitrary()?, u.arbitrary()?, u.arbitrary()?, u.arbitrary()?),
        16 => Variant::MarginsF(f(u)?, f(u)?, f(u)?, f(u)?),
        17 => Variant::Color(u.arbitrary()?, u.arbitrary()?, u.arbitrary()?, u.arbitrary()?),
        18 => Variant::Date(u.arbitrary()?, u.arbitrary()?, u.arbitrary()?),
        19 => Variant::Time(u.arbitrary()?, u.arbitrary()?, u.arbitrary()?, u.arbitrary()?),
        20 => Variant::DateTime(u.arbitrary()?),
        21 => Variant::Url(u.arbitrary()?),
        22 => Variant::Uuid(u.arbitrary()?),
        23 => Variant::RegularExpression(u.arbitrary()?, u.arbitrary()?),
        24 => Variant::Locale(u.arbitrary()?),
        25 => Variant::ObjectId(ferrokit_core::object::ObjectId(u.arbitrary()?)),
        26 => {
            let mut list = Vec::new();
            while u.arbitrary()? && list.len() < 8 {
                list.push(gen(u, depth + 1)?);
            }
            Variant::List(list)
        }
        _ => {
            let mut map = HashMap::new();
            while u.arbitrary()? && map.len() < 8 {
                map.insert(u.arbitrary::<String>()?, gen(u, depth + 1)?);
            }
            Variant::Map(map)
        }
    })
}

fn f(u: &mut Unstructured<'_>) -> arbitrary::Result<f32> {
    Ok(f32::from_bits(u.arbitrary()?))
}
