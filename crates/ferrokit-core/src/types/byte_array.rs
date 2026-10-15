// SPDX-License-Identifier: LGPL-3.0-only
//! Byte array container matching Qt's `QByteArray`.

use std::error::Error;
use std::fmt;
use std::ops::{Deref, DerefMut, Index, IndexMut};
use std::str::Utf8Error;

/// Error type for invalid hexadecimal string decoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HexError {
    OddLength,
    InvalidByte(char),
}

impl fmt::Display for HexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HexError::OddLength => write!(f, "odd length hex string"),
            HexError::InvalidByte(c) => write!(f, "invalid hex character '{}'", c),
        }
    }
}

impl Error for HexError {}

/// Error type for invalid Base64 string decoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Base64Error {
    InvalidLength,
    InvalidByte(char),
}

impl fmt::Display for Base64Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Base64Error::InvalidLength => write!(f, "invalid base64 string length"),
            Base64Error::InvalidByte(c) => write!(f, "invalid base64 character '{}'", c),
        }
    }
}

impl Error for Base64Error {}

/// A byte array providing rich binary and text-processing utilities,
/// modeled after Qt's `QByteArray`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct ByteArray(pub Vec<u8>);

impl ByteArray {
    /// Creates an empty byte array.
    pub fn new() -> Self {
        Self(Vec::new())
    }

    /// Creates an empty byte array with the specified capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        Self(Vec::with_capacity(capacity))
    }

    /// Creates a `ByteArray` from a `Vec<u8>`.
    pub fn from_vec(vec: Vec<u8>) -> Self {
        Self(vec)
    }

    /// Consumes the byte array and returns the underlying `Vec<u8>`.
    pub fn into_inner(self) -> Vec<u8> {
        self.0
    }

    /// Returns a shared byte slice.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Returns a mutable byte slice.
    pub fn as_mut_bytes(&mut self) -> &mut [u8] {
        &mut self.0
    }

    /// Attempts to view the byte array as a UTF-8 string slice, matching `QByteArray::toStdString`.
    pub fn as_str(&self) -> Result<&str, Utf8Error> {
        std::str::from_utf8(&self.0)
    }

    /// Converts the byte array to a string, replacing invalid UTF-8 sequences with the replacement character.
    pub fn to_string_lossy(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.0)
    }

    /// Encodes the byte array into a lowercase hexadecimal string, matching `QByteArray::toHex`.
    pub fn to_hex(&self) -> String {
        const HEX_CHARS: &[u8; 16] = b"0123456789abcdef";
        let mut hex = String::with_capacity(self.0.len() * 2);
        for &byte in &self.0 {
            hex.push(HEX_CHARS[(byte >> 4) as usize] as char);
            hex.push(HEX_CHARS[(byte & 0x0F) as usize] as char);
        }
        hex
    }

    /// Decodes a hexadecimal string into a `ByteArray`, matching `QByteArray::fromHex`.
    pub fn from_hex(hex: &str) -> Result<Self, HexError> {
        let clean = hex.trim();
        if !clean.len().is_multiple_of(2) {
            return Err(HexError::OddLength);
        }
        let mut bytes = Vec::with_capacity(clean.len() / 2);
        let chars: Vec<char> = clean.chars().collect();
        for chunk in chars.chunks(2) {
            let h = chunk[0]
                .to_digit(16)
                .ok_or(HexError::InvalidByte(chunk[0]))?;
            let l = chunk[1]
                .to_digit(16)
                .ok_or(HexError::InvalidByte(chunk[1]))?;
            bytes.push(((h << 4) | l) as u8);
        }
        Ok(Self(bytes))
    }

    /// Encodes the byte array into standard Base64 string, matching `QByteArray::toBase64`.
    pub fn to_base64(&self) -> String {
        const B64_CHARS: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::with_capacity(self.0.len().div_ceil(3) * 4);
        let (chunks, remainder) = self.0.as_chunks::<3>();

        for chunk in chunks {
            let b0 = chunk[0] as usize;
            let b1 = chunk[1] as usize;
            let b2 = chunk[2] as usize;
            out.push(B64_CHARS[b0 >> 2] as char);
            out.push(B64_CHARS[((b0 & 0x03) << 4) | (b1 >> 4)] as char);
            out.push(B64_CHARS[((b1 & 0x0F) << 2) | (b2 >> 6)] as char);
            out.push(B64_CHARS[b2 & 0x3F] as char);
        }

        match remainder.len() {
            1 => {
                let b0 = remainder[0] as usize;
                out.push(B64_CHARS[b0 >> 2] as char);
                out.push(B64_CHARS[(b0 & 0x03) << 4] as char);
                out.push('=');
                out.push('=');
            }
            2 => {
                let b0 = remainder[0] as usize;
                let b1 = remainder[1] as usize;
                out.push(B64_CHARS[b0 >> 2] as char);
                out.push(B64_CHARS[((b0 & 0x03) << 4) | (b1 >> 4)] as char);
                out.push(B64_CHARS[(b1 & 0x0F) << 2] as char);
                out.push('=');
            }
            _ => {}
        }

        out
    }

    /// Decodes a Base64 string into a `ByteArray`, matching `QByteArray::fromBase64`.
    pub fn from_base64(b64: &str) -> Result<Self, Base64Error> {
        let clean: Vec<u8> = b64.bytes().filter(|&b| !b.is_ascii_whitespace()).collect();
        if clean.is_empty() {
            return Ok(Self::new());
        }
        if !clean.len().is_multiple_of(4) {
            return Err(Base64Error::InvalidLength);
        }

        fn decode_char(c: u8) -> Result<u8, Base64Error> {
            match c {
                b'A'..=b'Z' => Ok(c - b'A'),
                b'a'..=b'z' => Ok(c - b'a' + 26),
                b'0'..=b'9' => Ok(c - b'0' + 52),
                b'+' => Ok(62),
                b'/' => Ok(63),
                _ => Err(Base64Error::InvalidByte(c as char)),
            }
        }

        let mut out = Vec::with_capacity(clean.len() / 4 * 3);
        for chunk in clean.chunks(4) {
            let c0 = decode_char(chunk[0])?;
            let c1 = decode_char(chunk[1])?;

            if chunk[2] == b'=' {
                if chunk[3] != b'=' {
                    return Err(Base64Error::InvalidByte(chunk[3] as char));
                }
                out.push((c0 << 2) | (c1 >> 4));
            } else if chunk[3] == b'=' {
                let c2 = decode_char(chunk[2])?;
                out.push((c0 << 2) | (c1 >> 4));
                out.push(((c1 & 0x0F) << 4) | (c2 >> 2));
            } else {
                let c2 = decode_char(chunk[2])?;
                let c3 = decode_char(chunk[3])?;
                out.push((c0 << 2) | (c1 >> 4));
                out.push(((c1 & 0x0F) << 4) | (c2 >> 2));
                out.push(((c2 & 0x03) << 6) | c3);
            }
        }

        Ok(Self(out))
    }

    /// Returns a new `ByteArray` with leading and trailing ASCII whitespace removed,
    /// matching `QByteArray::trimmed`.
    pub fn trimmed(&self) -> Self {
        let is_whitespace = |b: &u8| b.is_ascii_whitespace();
        let start = self.0.iter().position(|b| !is_whitespace(b)).unwrap_or(0);
        let end = self
            .0
            .iter()
            .rposition(|b| !is_whitespace(b))
            .map(|i| i + 1)
            .unwrap_or(0);
        if start >= end {
            Self::new()
        } else {
            Self(self.0[start..end].to_vec())
        }
    }

    /// Returns a new `ByteArray` with leading/trailing whitespace removed and
    /// internal whitespace sequences replaced with a single ASCII space `0x20`,
    /// matching `QByteArray::simplified`.
    pub fn simplified(&self) -> Self {
        let mut out = Vec::with_capacity(self.0.len());
        let mut in_whitespace = false;

        let trimmed = self.trimmed();
        for &byte in &trimmed.0 {
            if byte.is_ascii_whitespace() {
                if !in_whitespace {
                    out.push(b' ');
                    in_whitespace = true;
                }
            } else {
                out.push(byte);
                in_whitespace = false;
            }
        }

        Self(out)
    }

    /// Splits the byte array by a single separator byte, matching `QByteArray::split`.
    pub fn split(&self, sep: u8) -> Vec<ByteArray> {
        self.0
            .split(|&b| b == sep)
            .map(|chunk| ByteArray(chunk.to_vec()))
            .collect()
    }

    /// Returns `true` if the byte array starts with `prefix`, matching `QByteArray::startsWith`.
    pub fn starts_with(&self, prefix: &[u8]) -> bool {
        self.0.starts_with(prefix)
    }

    /// Returns `true` if the byte array ends with `suffix`, matching `QByteArray::endsWith`.
    pub fn ends_with(&self, suffix: &[u8]) -> bool {
        self.0.ends_with(suffix)
    }

    /// Finds the index of the first occurrence of `needle`, matching `QByteArray::indexOf`.
    pub fn index_of(&self, needle: &[u8]) -> Option<usize> {
        if needle.is_empty() {
            return Some(0);
        }
        self.0
            .windows(needle.len())
            .position(|window| window == needle)
    }

    /// Returns `true` if the byte array contains `needle`, matching `QByteArray::contains`.
    pub fn contains(&self, needle: &[u8]) -> bool {
        self.index_of(needle).is_some()
    }

    /// Replaces occurrences of `from` with `to` in-place, matching `QByteArray::replace`.
    pub fn replace(&mut self, from: &[u8], to: &[u8]) {
        if from.is_empty() || self.0.is_empty() {
            return;
        }

        let mut result = Vec::with_capacity(self.0.len());
        let mut i = 0;
        while i < self.0.len() {
            if self.0[i..].starts_with(from) {
                result.extend_from_slice(to);
                i += from.len();
            } else {
                result.push(self.0[i]);
                i += 1;
            }
        }
        self.0 = result;
    }

    /// Appends bytes to the end of this array.
    pub fn append(&mut self, bytes: &[u8]) {
        self.0.extend_from_slice(bytes);
    }
}

impl Deref for ByteArray {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for ByteArray {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Index<usize> for ByteArray {
    type Output = u8;

    fn index(&self, index: usize) -> &Self::Output {
        &self.0[index]
    }
}

impl IndexMut<usize> for ByteArray {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        &mut self.0[index]
    }
}

impl From<Vec<u8>> for ByteArray {
    fn from(vec: Vec<u8>) -> Self {
        Self(vec)
    }
}

impl From<&[u8]> for ByteArray {
    fn from(slice: &[u8]) -> Self {
        Self(slice.to_vec())
    }
}

impl From<&str> for ByteArray {
    fn from(s: &str) -> Self {
        Self(s.as_bytes().to_vec())
    }
}

impl From<String> for ByteArray {
    fn from(s: String) -> Self {
        Self(s.into_bytes())
    }
}

impl From<ByteArray> for Vec<u8> {
    fn from(ba: ByteArray) -> Self {
        ba.0
    }
}

impl FromIterator<u8> for ByteArray {
    fn from_iter<I: IntoIterator<Item = u8>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

impl<'a> FromIterator<&'a u8> for ByteArray {
    fn from_iter<I: IntoIterator<Item = &'a u8>>(iter: I) -> Self {
        Self(iter.into_iter().copied().collect())
    }
}

impl IntoIterator for ByteArray {
    type Item = u8;
    type IntoIter = std::vec::IntoIter<u8>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a> IntoIterator for &'a ByteArray {
    type Item = &'a u8;
    type IntoIter = std::slice::Iter<'a, u8>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl<'a> IntoIterator for &'a mut ByteArray {
    type Item = &'a mut u8;
    type IntoIter = std::slice::IterMut<'a, u8>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter_mut()
    }
}

impl Extend<u8> for ByteArray {
    fn extend<T: IntoIterator<Item = u8>>(&mut self, iter: T) {
        self.0.extend(iter);
    }
}

impl<'a> Extend<&'a u8> for ByteArray {
    fn extend<T: IntoIterator<Item = &'a u8>>(&mut self, iter: T) {
        self.0.extend(iter.into_iter().copied());
    }
}

impl fmt::Display for ByteArray {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Ok(s) = self.as_str() {
            write!(f, "ByteArray(\"{}\")", s)
        } else {
            write!(f, "ByteArray(hex:{})", self.to_hex())
        }
    }
}
