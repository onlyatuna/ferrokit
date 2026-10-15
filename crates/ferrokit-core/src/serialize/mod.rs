// SPDX-License-Identifier: LGPL-3.0-only
//! Binary and textual serialization framework (`QDataStream` and `QTextStream` equivalents).
//!
//! Provides platform-independent binary serialization with configurable endianness
//! and error reporting (`DataStream`), formatted textual streams (`TextStream`),
//! and the `DataSerializable` operator overloading trait.

pub mod data_stream;
pub mod text_stream;
pub mod traits;

pub use data_stream::{ByteOrder, DataStream, DataStreamReader, DataStreamWriter, Status};
pub use text_stream::{FieldAlignment, NumberBase, TextStream};
pub use traits::DataSerializable;
