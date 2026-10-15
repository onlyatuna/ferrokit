// SPDX-License-Identifier: LGPL-3.0-only
//! Data serialization trait matching Qt's `QDataStream` operator overloading.
//!
//! Types implementing `DataSerializable` can be streamed to and from binary formats
//! using `DataStreamWriter` and `DataStreamReader`.

use std::io;

use super::data_stream::{DataStreamReader, DataStreamWriter};

/// Trait for types that can be binary-serialized to and deserialized from a `DataStream`.
pub trait DataSerializable: Sized {
    /// Writes this object to the binary data stream.
    fn serialize<W: io::Write>(&self, stream: &mut DataStreamWriter<W>) -> io::Result<()>;

    /// Reads this object from the binary data stream.
    fn deserialize<R: io::Read>(stream: &mut DataStreamReader<R>) -> io::Result<Self>;
}
