// SPDX-License-Identifier: LGPL-3.0-only
//! QDataStream binary serialization engine.
//!
//! Provides platform-independent serialization of primitive types, strings, byte arrays,
//! and composite data structures with configurable byte endianness and status reporting.

use std::io::{self, ErrorKind, Read, Write};

use super::traits::DataSerializable;
use crate::types::{ByteArray, StringList};

/// Byte endianness for binary serialization (`QDataStream::ByteOrder` equivalent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ByteOrder {
    /// Big-endian network byte order (Qt default).
    #[default]
    BigEndian,
    /// Little-endian byte order.
    LittleEndian,
}

/// Status flag for binary stream operations (`QDataStream::Status` equivalent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Status {
    #[default]
    Ok,
    ReadPastEnd,
    ReadCorruptData,
    WriteFailed,
}

// =============================================================================
// DataStreamWriter
// =============================================================================

/// Binary serializer writing to an underlying `io::Write` sink (`QDataStream` output).
pub struct DataStreamWriter<W: Write> {
    writer: W,
    byte_order: ByteOrder,
    status: Status,
}

impl<W: Write> DataStreamWriter<W> {
    /// Constructs a writer wrapping `writer` with default `BigEndian` order.
    pub fn new(writer: W) -> Self {
        Self {
            writer,
            byte_order: ByteOrder::BigEndian,
            status: Status::Ok,
        }
    }

    /// Constructs a writer wrapping `writer` with the specified `byte_order`.
    pub fn with_byte_order(writer: W, byte_order: ByteOrder) -> Self {
        Self {
            writer,
            byte_order,
            status: Status::Ok,
        }
    }

    /// Returns the current byte order.
    #[inline]
    pub fn byte_order(&self) -> ByteOrder {
        self.byte_order
    }

    /// Sets the byte order.
    #[inline]
    pub fn set_byte_order(&mut self, order: ByteOrder) {
        self.byte_order = order;
    }

    /// Returns the current stream status.
    #[inline]
    pub fn status(&self) -> Status {
        self.status
    }

    /// Writes a boolean (1 byte: 0 or 1).
    pub fn write_bool(&mut self, val: bool) -> io::Result<()> {
        self.write_u8(if val { 1 } else { 0 })
    }

    /// Writes an unsigned 8-bit integer.
    pub fn write_u8(&mut self, val: u8) -> io::Result<()> {
        self.write_raw(&[val])
    }

    /// Writes a signed 8-bit integer.
    pub fn write_i8(&mut self, val: i8) -> io::Result<()> {
        self.write_raw(&[val as u8])
    }

    /// Writes an unsigned 16-bit integer respecting byte order.
    pub fn write_u16(&mut self, val: u16) -> io::Result<()> {
        let bytes = match self.byte_order {
            ByteOrder::BigEndian => val.to_be_bytes(),
            ByteOrder::LittleEndian => val.to_le_bytes(),
        };
        self.write_raw(&bytes)
    }

    /// Writes a signed 16-bit integer respecting byte order.
    pub fn write_i16(&mut self, val: i16) -> io::Result<()> {
        let bytes = match self.byte_order {
            ByteOrder::BigEndian => val.to_be_bytes(),
            ByteOrder::LittleEndian => val.to_le_bytes(),
        };
        self.write_raw(&bytes)
    }

    /// Writes an unsigned 32-bit integer respecting byte order.
    pub fn write_u32(&mut self, val: u32) -> io::Result<()> {
        let bytes = match self.byte_order {
            ByteOrder::BigEndian => val.to_be_bytes(),
            ByteOrder::LittleEndian => val.to_le_bytes(),
        };
        self.write_raw(&bytes)
    }

    /// Writes a signed 32-bit integer respecting byte order.
    pub fn write_i32(&mut self, val: i32) -> io::Result<()> {
        let bytes = match self.byte_order {
            ByteOrder::BigEndian => val.to_be_bytes(),
            ByteOrder::LittleEndian => val.to_le_bytes(),
        };
        self.write_raw(&bytes)
    }

    /// Writes an unsigned 64-bit integer respecting byte order.
    pub fn write_u64(&mut self, val: u64) -> io::Result<()> {
        let bytes = match self.byte_order {
            ByteOrder::BigEndian => val.to_be_bytes(),
            ByteOrder::LittleEndian => val.to_le_bytes(),
        };
        self.write_raw(&bytes)
    }

    /// Writes a signed 64-bit integer respecting byte order.
    pub fn write_i64(&mut self, val: i64) -> io::Result<()> {
        let bytes = match self.byte_order {
            ByteOrder::BigEndian => val.to_be_bytes(),
            ByteOrder::LittleEndian => val.to_le_bytes(),
        };
        self.write_raw(&bytes)
    }

    /// Writes a 32-bit floating point number respecting byte order.
    pub fn write_f32(&mut self, val: f32) -> io::Result<()> {
        self.write_u32(val.to_bits())
    }

    /// Writes a 64-bit floating point number respecting byte order.
    pub fn write_f64(&mut self, val: f64) -> io::Result<()> {
        self.write_u64(val.to_bits())
    }

    /// Writes raw bytes without any length prefix.
    pub fn write_raw(&mut self, bytes: &[u8]) -> io::Result<()> {
        if let Err(e) = self.writer.write_all(bytes) {
            self.status = Status::WriteFailed;
            Err(e)
        } else {
            Ok(())
        }
    }

    /// Writes a byte slice with a 32-bit length prefix (Qt `QByteArray` serialization format).
    pub fn write_bytes(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.write_u32(bytes.len() as u32)?;
        self.write_raw(bytes)
    }

    /// Writes a UTF-8 string with a 32-bit byte-length prefix (Qt `QString` UTF-8 serialization format).
    pub fn write_str(&mut self, s: &str) -> io::Result<()> {
        self.write_bytes(s.as_bytes())
    }

    /// Writes a `ByteArray`.
    pub fn write_byte_array(&mut self, array: &ByteArray) -> io::Result<()> {
        self.write_bytes(array.as_bytes())
    }

    /// Writes a `StringList` (length prefix followed by strings).
    pub fn write_string_list(&mut self, list: &StringList) -> io::Result<()> {
        self.write_u32(list.len() as u32)?;
        for s in list.iter() {
            self.write_str(s)?;
        }
        Ok(())
    }

    /// Serializes a type implementing `DataSerializable`.
    pub fn write_serializable<T: DataSerializable>(&mut self, value: &T) -> io::Result<()> {
        value.serialize(self)
    }

    /// Flushes the underlying writer.
    pub fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }

    /// Unwraps the underlying writer.
    pub fn into_inner(self) -> W {
        self.writer
    }
}

// =============================================================================
// DataStreamReader
// =============================================================================

/// Binary deserializer reading from an underlying `io::Read` source (`QDataStream` input).
pub struct DataStreamReader<R: Read> {
    reader: R,
    byte_order: ByteOrder,
    status: Status,
}

impl<R: Read> DataStreamReader<R> {
    /// Constructs a reader wrapping `reader` with default `BigEndian` order.
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            byte_order: ByteOrder::BigEndian,
            status: Status::Ok,
        }
    }

    /// Constructs a reader wrapping `reader` with the specified `byte_order`.
    pub fn with_byte_order(reader: R, byte_order: ByteOrder) -> Self {
        Self {
            reader,
            byte_order,
            status: Status::Ok,
        }
    }

    /// Returns the current byte order.
    #[inline]
    pub fn byte_order(&self) -> ByteOrder {
        self.byte_order
    }

    /// Sets the byte order.
    #[inline]
    pub fn set_byte_order(&mut self, order: ByteOrder) {
        self.byte_order = order;
    }

    /// Returns the current stream status.
    #[inline]
    pub fn status(&self) -> Status {
        self.status
    }

    /// Reads exact bytes into `buf`.
    pub fn read_raw(&mut self, buf: &mut [u8]) -> io::Result<()> {
        if let Err(e) = self.reader.read_exact(buf) {
            if e.kind() == ErrorKind::UnexpectedEof {
                self.status = Status::ReadPastEnd;
            } else {
                self.status = Status::ReadCorruptData;
            }
            Err(e)
        } else {
            Ok(())
        }
    }

    /// Reads a boolean.
    pub fn read_bool(&mut self) -> io::Result<bool> {
        let b = self.read_u8()?;
        Ok(b != 0)
    }

    /// Reads an unsigned 8-bit integer.
    pub fn read_u8(&mut self) -> io::Result<u8> {
        let mut buf = [0u8; 1];
        self.read_raw(&mut buf)?;
        Ok(buf[0])
    }

    /// Reads a signed 8-bit integer.
    pub fn read_i8(&mut self) -> io::Result<i8> {
        let mut buf = [0u8; 1];
        self.read_raw(&mut buf)?;
        Ok(buf[0] as i8)
    }

    /// Reads an unsigned 16-bit integer respecting byte order.
    pub fn read_u16(&mut self) -> io::Result<u16> {
        let mut buf = [0u8; 2];
        self.read_raw(&mut buf)?;
        Ok(match self.byte_order {
            ByteOrder::BigEndian => u16::from_be_bytes(buf),
            ByteOrder::LittleEndian => u16::from_le_bytes(buf),
        })
    }

    /// Reads a signed 16-bit integer respecting byte order.
    pub fn read_i16(&mut self) -> io::Result<i16> {
        let mut buf = [0u8; 2];
        self.read_raw(&mut buf)?;
        Ok(match self.byte_order {
            ByteOrder::BigEndian => i16::from_be_bytes(buf),
            ByteOrder::LittleEndian => i16::from_le_bytes(buf),
        })
    }

    /// Reads an unsigned 32-bit integer respecting byte order.
    pub fn read_u32(&mut self) -> io::Result<u32> {
        let mut buf = [0u8; 4];
        self.read_raw(&mut buf)?;
        Ok(match self.byte_order {
            ByteOrder::BigEndian => u32::from_be_bytes(buf),
            ByteOrder::LittleEndian => u32::from_le_bytes(buf),
        })
    }

    /// Reads a signed 32-bit integer respecting byte order.
    pub fn read_i32(&mut self) -> io::Result<i32> {
        let mut buf = [0u8; 4];
        self.read_raw(&mut buf)?;
        Ok(match self.byte_order {
            ByteOrder::BigEndian => i32::from_be_bytes(buf),
            ByteOrder::LittleEndian => i32::from_le_bytes(buf),
        })
    }

    /// Reads an unsigned 64-bit integer respecting byte order.
    pub fn read_u64(&mut self) -> io::Result<u64> {
        let mut buf = [0u8; 8];
        self.read_raw(&mut buf)?;
        Ok(match self.byte_order {
            ByteOrder::BigEndian => u64::from_be_bytes(buf),
            ByteOrder::LittleEndian => u64::from_le_bytes(buf),
        })
    }

    /// Reads a signed 64-bit integer respecting byte order.
    pub fn read_i64(&mut self) -> io::Result<i64> {
        let mut buf = [0u8; 8];
        self.read_raw(&mut buf)?;
        Ok(match self.byte_order {
            ByteOrder::BigEndian => i64::from_be_bytes(buf),
            ByteOrder::LittleEndian => i64::from_le_bytes(buf),
        })
    }

    /// Reads a 32-bit floating point number respecting byte order.
    pub fn read_f32(&mut self) -> io::Result<f32> {
        let bits = self.read_u32()?;
        Ok(f32::from_bits(bits))
    }

    /// Reads a 64-bit floating point number respecting byte order.
    pub fn read_f64(&mut self) -> io::Result<f64> {
        let bits = self.read_u64()?;
        Ok(f64::from_bits(bits))
    }

    /// Reads a 32-bit length prefixed byte array.
    pub fn read_bytes(&mut self) -> io::Result<Vec<u8>> {
        let len = self.read_u32()? as usize;
        let mut buf = vec![0u8; len];
        self.read_raw(&mut buf)?;
        Ok(buf)
    }

    /// Reads a 32-bit length prefixed UTF-8 string.
    pub fn read_string(&mut self) -> io::Result<String> {
        let bytes = self.read_bytes()?;
        String::from_utf8(bytes).map_err(|e| {
            self.status = Status::ReadCorruptData;
            io::Error::new(ErrorKind::InvalidData, e)
        })
    }

    /// Reads a `ByteArray`.
    pub fn read_byte_array(&mut self) -> io::Result<ByteArray> {
        let bytes = self.read_bytes()?;
        Ok(ByteArray::from(bytes))
    }

    /// Reads a `StringList`.
    pub fn read_string_list(&mut self) -> io::Result<StringList> {
        let count = self.read_u32()? as usize;
        let mut list = Vec::with_capacity(count);
        for _ in 0..count {
            list.push(self.read_string()?);
        }
        Ok(StringList::from(list))
    }

    /// Deserializes a type implementing `DataSerializable`.
    pub fn read_serializable<T: DataSerializable>(&mut self) -> io::Result<T> {
        T::deserialize(self)
    }

    /// Unwraps the underlying reader.
    pub fn into_inner(self) -> R {
        self.reader
    }
}

// =============================================================================
// In-Memory DataStream Helper
// =============================================================================

/// Convenient in-memory binary serializer and deserializer (`QDataStream` over `QByteArray`).
pub struct DataStream {
    buffer: Vec<u8>,
    byte_order: ByteOrder,
}

impl DataStream {
    /// Creates an empty in-memory binary stream with default `BigEndian` order.
    pub fn new() -> Self {
        Self {
            buffer: Vec::new(),
            byte_order: ByteOrder::BigEndian,
        }
    }

    /// Creates an empty in-memory binary stream with the specified byte order.
    pub fn with_byte_order(byte_order: ByteOrder) -> Self {
        Self {
            buffer: Vec::new(),
            byte_order,
        }
    }

    /// Creates a reader over an existing byte slice.
    pub fn reader(bytes: &[u8]) -> DataStreamReader<&[u8]> {
        DataStreamReader::new(bytes)
    }

    /// Creates a reader with specific byte order over an existing byte slice.
    pub fn reader_with_order(bytes: &[u8], byte_order: ByteOrder) -> DataStreamReader<&[u8]> {
        DataStreamReader::with_byte_order(bytes, byte_order)
    }

    /// Returns a writer handle writing into this buffer.
    pub fn writer(&mut self) -> DataStreamWriter<&mut Vec<u8>> {
        DataStreamWriter::with_byte_order(&mut self.buffer, self.byte_order)
    }

    /// Returns the accumulated serialized bytes as a slice.
    pub fn as_bytes(&self) -> &[u8] {
        &self.buffer
    }

    /// Consumes the stream, returning the accumulated bytes.
    pub fn into_bytes(self) -> Vec<u8> {
        self.buffer
    }

    /// Consumes the stream, returning a `ByteArray`.
    pub fn into_byte_array(self) -> ByteArray {
        ByteArray::from(self.buffer)
    }
}

impl Default for DataStream {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// DataSerializable standard implementations
// =============================================================================

impl DataSerializable for bool {
    fn serialize<W: Write>(&self, stream: &mut DataStreamWriter<W>) -> io::Result<()> {
        stream.write_bool(*self)
    }
    fn deserialize<R: Read>(stream: &mut DataStreamReader<R>) -> io::Result<Self> {
        stream.read_bool()
    }
}

impl DataSerializable for u8 {
    fn serialize<W: Write>(&self, stream: &mut DataStreamWriter<W>) -> io::Result<()> {
        stream.write_u8(*self)
    }
    fn deserialize<R: Read>(stream: &mut DataStreamReader<R>) -> io::Result<Self> {
        stream.read_u8()
    }
}

impl DataSerializable for i8 {
    fn serialize<W: Write>(&self, stream: &mut DataStreamWriter<W>) -> io::Result<()> {
        stream.write_i8(*self)
    }
    fn deserialize<R: Read>(stream: &mut DataStreamReader<R>) -> io::Result<Self> {
        stream.read_i8()
    }
}

impl DataSerializable for u16 {
    fn serialize<W: Write>(&self, stream: &mut DataStreamWriter<W>) -> io::Result<()> {
        stream.write_u16(*self)
    }
    fn deserialize<R: Read>(stream: &mut DataStreamReader<R>) -> io::Result<Self> {
        stream.read_u16()
    }
}

impl DataSerializable for i16 {
    fn serialize<W: Write>(&self, stream: &mut DataStreamWriter<W>) -> io::Result<()> {
        stream.write_i16(*self)
    }
    fn deserialize<R: Read>(stream: &mut DataStreamReader<R>) -> io::Result<Self> {
        stream.read_i16()
    }
}

impl DataSerializable for u32 {
    fn serialize<W: Write>(&self, stream: &mut DataStreamWriter<W>) -> io::Result<()> {
        stream.write_u32(*self)
    }
    fn deserialize<R: Read>(stream: &mut DataStreamReader<R>) -> io::Result<Self> {
        stream.read_u32()
    }
}

impl DataSerializable for i32 {
    fn serialize<W: Write>(&self, stream: &mut DataStreamWriter<W>) -> io::Result<()> {
        stream.write_i32(*self)
    }
    fn deserialize<R: Read>(stream: &mut DataStreamReader<R>) -> io::Result<Self> {
        stream.read_i32()
    }
}

impl DataSerializable for u64 {
    fn serialize<W: Write>(&self, stream: &mut DataStreamWriter<W>) -> io::Result<()> {
        stream.write_u64(*self)
    }
    fn deserialize<R: Read>(stream: &mut DataStreamReader<R>) -> io::Result<Self> {
        stream.read_u64()
    }
}

impl DataSerializable for i64 {
    fn serialize<W: Write>(&self, stream: &mut DataStreamWriter<W>) -> io::Result<()> {
        stream.write_i64(*self)
    }
    fn deserialize<R: Read>(stream: &mut DataStreamReader<R>) -> io::Result<Self> {
        stream.read_i64()
    }
}

impl DataSerializable for f32 {
    fn serialize<W: Write>(&self, stream: &mut DataStreamWriter<W>) -> io::Result<()> {
        stream.write_f32(*self)
    }
    fn deserialize<R: Read>(stream: &mut DataStreamReader<R>) -> io::Result<Self> {
        stream.read_f32()
    }
}

impl DataSerializable for f64 {
    fn serialize<W: Write>(&self, stream: &mut DataStreamWriter<W>) -> io::Result<()> {
        stream.write_f64(*self)
    }
    fn deserialize<R: Read>(stream: &mut DataStreamReader<R>) -> io::Result<Self> {
        stream.read_f64()
    }
}

impl DataSerializable for String {
    fn serialize<W: Write>(&self, stream: &mut DataStreamWriter<W>) -> io::Result<()> {
        stream.write_str(self)
    }
    fn deserialize<R: Read>(stream: &mut DataStreamReader<R>) -> io::Result<Self> {
        stream.read_string()
    }
}

impl DataSerializable for Vec<u8> {
    fn serialize<W: Write>(&self, stream: &mut DataStreamWriter<W>) -> io::Result<()> {
        stream.write_bytes(self)
    }
    fn deserialize<R: Read>(stream: &mut DataStreamReader<R>) -> io::Result<Self> {
        stream.read_bytes()
    }
}

impl DataSerializable for ByteArray {
    fn serialize<W: Write>(&self, stream: &mut DataStreamWriter<W>) -> io::Result<()> {
        stream.write_byte_array(self)
    }
    fn deserialize<R: Read>(stream: &mut DataStreamReader<R>) -> io::Result<Self> {
        stream.read_byte_array()
    }
}

impl DataSerializable for StringList {
    fn serialize<W: Write>(&self, stream: &mut DataStreamWriter<W>) -> io::Result<()> {
        stream.write_string_list(self)
    }
    fn deserialize<R: Read>(stream: &mut DataStreamReader<R>) -> io::Result<Self> {
        stream.read_string_list()
    }
}
