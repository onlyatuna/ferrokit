// SPDX-License-Identifier: LGPL-3.0-only
//! QIODevice trait and OpenMode definitions (`QIODevice` equivalent).
//!
//! Provides the core abstraction for sequential and random-access I/O devices,
//! such as files, buffers, processes, and network streams.

use std::fmt;
use std::io;

/// Open mode flags for `IODevice` (`QIODevice::OpenModeFlag` equivalent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct OpenMode(pub u32);

impl OpenMode {
    pub const NOT_OPEN: Self = Self(0x0000);
    pub const READ_ONLY: Self = Self(0x0001);
    pub const WRITE_ONLY: Self = Self(0x0002);
    pub const READ_WRITE: Self = Self(0x0001 | 0x0002);
    pub const APPEND: Self = Self(0x0004);
    pub const TRUNCATE: Self = Self(0x0008);
    pub const TEXT: Self = Self(0x0010);
    pub const UNBUFFERED: Self = Self(0x0020);
    pub const NEW_ONLY: Self = Self(0x0040);
    pub const EXISTING_ONLY: Self = Self(0x0080);

    /// Returns `true` if no open mode flag is set.
    #[inline]
    pub const fn is_not_open(&self) -> bool {
        self.0 == 0
    }

    /// Returns `true` if read access is enabled.
    #[inline]
    pub const fn is_readable(&self) -> bool {
        (self.0 & Self::READ_ONLY.0) != 0
    }

    /// Returns `true` if write access is enabled.
    #[inline]
    pub const fn is_writable(&self) -> bool {
        (self.0 & Self::WRITE_ONLY.0) != 0
    }

    /// Returns `true` if append mode is enabled.
    #[inline]
    pub const fn is_append(&self) -> bool {
        (self.0 & Self::APPEND.0) != 0
    }

    /// Returns `true` if truncate mode is enabled.
    #[inline]
    pub const fn is_truncate(&self) -> bool {
        (self.0 & Self::TRUNCATE.0) != 0
    }

    /// Returns `true` if text mode is enabled.
    #[inline]
    pub const fn is_text(&self) -> bool {
        (self.0 & Self::TEXT.0) != 0
    }

    /// Returns `true` if unbuffered mode is enabled.
    #[inline]
    pub const fn is_unbuffered(&self) -> bool {
        (self.0 & Self::UNBUFFERED.0) != 0
    }

    /// Returns `true` if this mode contains the specified other mode flags.
    #[inline]
    pub const fn contains(&self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }
}

impl std::ops::BitOr for OpenMode {
    type Output = Self;
    #[inline]
    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for OpenMode {
    #[inline]
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl std::ops::BitAnd for OpenMode {
    type Output = Self;
    #[inline]
    fn bitand(self, rhs: Self) -> Self::Output {
        Self(self.0 & rhs.0)
    }
}

impl fmt::Display for OpenMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_not_open() {
            return write!(f, "NotOpen");
        }
        let mut parts = Vec::new();
        if self.is_readable() && self.is_writable() {
            parts.push("ReadWrite");
        } else if self.is_readable() {
            parts.push("ReadOnly");
        } else if self.is_writable() {
            parts.push("WriteOnly");
        }
        if self.is_append() {
            parts.push("Append");
        }
        if self.is_truncate() {
            parts.push("Truncate");
        }
        if self.is_text() {
            parts.push("Text");
        }
        if self.is_unbuffered() {
            parts.push("Unbuffered");
        }
        write!(f, "OpenMode({})", parts.join("|"))
    }
}

/// Abstract I/O Device trait (`QIODevice` equivalent).
///
/// Implemented by sequential devices (processes, streams) and random-access devices (files, buffers).
pub trait IODevice {
    /// Returns the current open mode.
    fn open_mode(&self) -> OpenMode;

    /// Returns `true` if the device is currently open.
    fn is_open(&self) -> bool {
        !self.open_mode().is_not_open()
    }

    /// Whether the open mode allows reading.
    fn is_readable(&self) -> bool {
        self.open_mode().is_readable()
    }

    /// Whether the open mode allows writing.
    fn is_writable(&self) -> bool {
        self.open_mode().is_writable()
    }

    /// Returns `true` if this device is sequential (e.g., pipes, sockets, processes)
    /// rather than random-access (e.g., files, buffers).
    fn is_sequential(&self) -> bool {
        false
    }

    /// Returns the current read/write position in bytes.
    /// For sequential devices, this returns the number of bytes read/written or 0.
    fn pos(&self) -> u64;

    /// Returns the total size of the device in bytes, or available bytes.
    fn size(&self) -> u64;

    /// Seeks to the given byte position in random-access devices.
    fn seek(&mut self, pos: u64) -> io::Result<()>;

    /// Whether the read position has reached the end, or the device is not open.
    fn at_end(&self) -> bool {
        !self.is_open() || (self.pos() >= self.size())
    }

    /// Reads up to `buf.len()` bytes into `buf`.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;

    /// Reads all remaining bytes from the device until EOF.
    fn read_all(&mut self) -> io::Result<Vec<u8>> {
        if !self.is_readable() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "device is not open for reading",
            ));
        }
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            match self.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => buffer.extend_from_slice(&chunk[..n]),
                Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(buffer)
    }

    /// Reads a single line of text from the device up to maximum `max_len` bytes (or infinite if 0).
    fn read_line(&mut self, max_len: usize) -> io::Result<Vec<u8>> {
        if !self.is_readable() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "device is not open for reading",
            ));
        }
        let mut line = Vec::new();
        let mut byte = [0u8; 1];
        while max_len == 0 || line.len() < max_len {
            match self.read(&mut byte) {
                Ok(0) => break,
                Ok(1) => {
                    line.push(byte[0]);
                    if byte[0] == b'\n' {
                        break;
                    }
                }
                Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
                _ => break,
            }
        }
        Ok(line)
    }

    /// Writes data slice to the device.
    fn write(&mut self, data: &[u8]) -> io::Result<usize>;

    /// Writes all data slice to the device until finished.
    fn write_all(&mut self, mut data: &[u8]) -> io::Result<()> {
        while !data.is_empty() {
            match self.write(data) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "failed to write whole buffer",
                    ));
                }
                Ok(n) => data = &data[n..],
                Err(ref e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Flushes any buffered data to the underlying storage.
    fn flush(&mut self) -> io::Result<()>;

    /// Closes the device.
    fn close(&mut self);

    /// Text describing the most recent error, if there was one.
    fn error_string(&self) -> Option<String> {
        None
    }
}
