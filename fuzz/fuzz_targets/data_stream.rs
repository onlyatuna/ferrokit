#![no_main]
//! `DataStream`: every typed read on arbitrary bytes must fail cleanly (no panic, no huge
//! allocation), and a sequence of typed writes must read back unchanged in either byte order.

use arbitrary::{Arbitrary, Unstructured};
use ferrokit_core::serialize::{ByteOrder, DataStream};
use ferrokit_core::types::StringList;
use libfuzzer_sys::fuzz_target;

#[derive(Debug, Arbitrary)]
enum Op {
    Bool(bool),
    U8(u8),
    I8(i8),
    U16(u16),
    I16(i16),
    U32(u32),
    I32(i32),
    U64(u64),
    I64(i64),
    F32(u32),
    F64(u64),
    Bytes(Vec<u8>),
    Str(String),
    List(Vec<String>),
}

fuzz_target!(|data: &[u8]| {
    let Some((&selector, rest)) = data.split_first() else {
        return;
    };
    let order = if selector & 1 == 0 {
        ByteOrder::BigEndian
    } else {
        ByteOrder::LittleEndian
    };

    // Arbitrary bytes through arbitrary typed reads.
    let mut reader = DataStream::reader_with_order(rest, order);
    for &op in rest.iter().take(64) {
        let _ = match op % 14 {
            0 => reader.read_bool().map(drop),
            1 => reader.read_u8().map(drop),
            2 => reader.read_i8().map(drop),
            3 => reader.read_u16().map(drop),
            4 => reader.read_i16().map(drop),
            5 => reader.read_u32().map(drop),
            6 => reader.read_i32().map(drop),
            7 => reader.read_u64().map(drop),
            8 => reader.read_i64().map(drop),
            9 => reader.read_f32().map(drop),
            10 => reader.read_f64().map(drop),
            11 => reader.read_bytes().map(drop),
            12 => reader.read_string().map(drop),
            _ => reader.read_string_list().map(drop),
        };
    }

    // Typed writes round-trip.
    let mut u = Unstructured::new(rest);
    let Ok(ops) = Vec::<Op>::arbitrary(&mut u) else {
        return;
    };
    let mut stream = DataStream::with_byte_order(order);
    let mut writer = stream.writer();
    for op in &ops {
        match op {
            Op::Bool(v) => writer.write_bool(*v),
            Op::U8(v) => writer.write_u8(*v),
            Op::I8(v) => writer.write_i8(*v),
            Op::U16(v) => writer.write_u16(*v),
            Op::I16(v) => writer.write_i16(*v),
            Op::U32(v) => writer.write_u32(*v),
            Op::I32(v) => writer.write_i32(*v),
            Op::U64(v) => writer.write_u64(*v),
            Op::I64(v) => writer.write_i64(*v),
            Op::F32(v) => writer.write_f32(f32::from_bits(*v)),
            Op::F64(v) => writer.write_f64(f64::from_bits(*v)),
            Op::Bytes(v) => writer.write_bytes(v),
            Op::Str(v) => writer.write_str(v),
            Op::List(v) => writer.write_string_list(&StringList::from(v.clone())),
        }
        .unwrap();
    }
    let mut reader = DataStream::reader_with_order(stream.as_bytes(), order);
    for op in &ops {
        match op {
            Op::Bool(v) => assert_eq!(reader.read_bool().unwrap(), *v),
            Op::U8(v) => assert_eq!(reader.read_u8().unwrap(), *v),
            Op::I8(v) => assert_eq!(reader.read_i8().unwrap(), *v),
            Op::U16(v) => assert_eq!(reader.read_u16().unwrap(), *v),
            Op::I16(v) => assert_eq!(reader.read_i16().unwrap(), *v),
            Op::U32(v) => assert_eq!(reader.read_u32().unwrap(), *v),
            Op::I32(v) => assert_eq!(reader.read_i32().unwrap(), *v),
            Op::U64(v) => assert_eq!(reader.read_u64().unwrap(), *v),
            Op::I64(v) => assert_eq!(reader.read_i64().unwrap(), *v),
            Op::F32(v) => assert_eq!(reader.read_f32().unwrap().to_bits(), *v),
            Op::F64(v) => assert_eq!(reader.read_f64().unwrap().to_bits(), *v),
            Op::Bytes(v) => assert_eq!(&reader.read_bytes().unwrap(), v),
            Op::Str(v) => assert_eq!(&reader.read_string().unwrap(), v),
            Op::List(v) => assert_eq!(reader.read_string_list().unwrap().0, *v),
        }
    }
});
