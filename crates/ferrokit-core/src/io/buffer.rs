// SPDX-License-Identifier: LGPL-3.0-only
//! QBuffer in-memory I/O device (`QBuffer` equivalent).
//!
//! Provides an `IODevice` interface for operating directly on an in-memory byte buffer.

use std::cmp;
use std::io::{self, Read, Seek, SeekFrom, Write};

use super::device::{IODevice, OpenMode};
use crate::types::ByteArray;

/// In-memory I/O device operating on a byte buffer (`QBuffer` equivalent).
#[derive(Debug, Clone, Default)]
pub struct Buffer {
    data: Vec<u8>,
    pos: u64,
    open_mode: OpenMode,
}

impl Buffer {
    /// Creates a new empty, closed buffer.
    pub fn new() -> Self {
        Self {
            data: Vec::new(),
            pos: 0,
            open_mode: OpenMode::NOT_OPEN,
        }
    }

    /// Creates a new buffer initialized with the specified byte vector.
    pub fn with_data(data: Vec<u8>) -> Self {
        Self {
            data,
            pos: 0,
            open_mode: OpenMode::NOT_OPEN,
        }
    }

    /// Creates a new buffer initialized with a `ByteArray`.
    pub fn with_byte_array(array: &ByteArray) -> Self {
        Self::with_data(array.as_bytes().to_vec())
    }

    /// Opens the buffer with the given mode flags.
    pub fn open(&mut self, mode: OpenMode) -> io::Result<()> {
        if self.is_open() {
            self.close();
        }
        if mode.is_not_open() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot open with NotOpen mode",
            ));
        }

        self.open_mode = mode;
        if mode.is_truncate() {
            self.data.clear();
            self.pos = 0;
        } else if mode.is_append() {
            self.pos = self.data.len() as u64;
        } else {
            self.pos = 0;
        }
        Ok(())
    }

    /// Returns an immutable reference to the underlying byte slice.
    #[inline]
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Returns a mutable reference to the underlying byte vector.
    #[inline]
    pub fn data_mut(&mut self) -> &mut Vec<u8> {
        &mut self.data
    }

    /// Replaces the underlying buffer with new data and resets pos to 0.
    pub fn set_data(&mut self, data: &[u8]) {
        self.data = data.to_vec();
        self.pos = 0;
    }

    /// Converts the buffer contents into an owned `ByteArray`.
    pub fn to_byte_array(&self) -> ByteArray {
        ByteArray::from(self.data.clone())
    }
}

impl IODevice for Buffer {
    fn open_mode(&self) -> OpenMode {
        self.open_mode
    }

    fn pos(&self) -> u64 {
        self.pos
    }

    fn size(&self) -> u64 {
        self.data.len() as u64
    }

    fn seek(&mut self, pos: u64) -> io::Result<()> {
        if !self.is_open() {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "buffer is not open",
            ));
        }
        self.pos = pos;
        Ok(())
    }

    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if !self.is_readable() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "buffer not open for reading",
            ));
        }
        let current_pos = self.pos as usize;
        if current_pos >= self.data.len() {
            return Ok(0);
        }
        let available = self.data.len() - current_pos;
        let to_read = cmp::min(buf.len(), available);
        buf[..to_read].copy_from_slice(&self.data[current_pos..current_pos + to_read]);
        self.pos += to_read as u64;
        Ok(to_read)
    }

    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if !self.is_writable() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "buffer not open for writing",
            ));
        }
        if self.open_mode.is_append() {
            self.pos = self.data.len() as u64;
        }

        let current_pos = self.pos as usize;
        let write_len = data.len();

        if current_pos + write_len > self.data.len() {
            self.data.resize(current_pos + write_len, 0);
        }

        self.data[current_pos..current_pos + write_len].copy_from_slice(data);
        self.pos += write_len as u64;
        Ok(write_len)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn close(&mut self) {
        self.open_mode = OpenMode::NOT_OPEN;
        self.pos = 0;
    }
}

impl Read for Buffer {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        IODevice::read(self, buf)
    }
}

impl Write for Buffer {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        IODevice::write(self, buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        IODevice::flush(self)
    }
}

impl Seek for Buffer {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let new_pos = match pos {
            SeekFrom::Start(offset) => offset as i64,
            SeekFrom::End(offset) => self.data.len() as i64 + offset,
            SeekFrom::Current(offset) => self.pos as i64 + offset,
        };

        if new_pos < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot seek before buffer start",
            ));
        }

        self.pos = new_pos as u64;
        Ok(self.pos)
    }
}
