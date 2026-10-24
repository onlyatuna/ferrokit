// SPDX-License-Identifier: LGPL-3.0-only
//! QTextStream formatted text input/output stream.
//!
//! Provides customizable textual formatting, field padding, alignment, numeric base
//! conversions (hex, oct, bin, dec), and line-oriented reading/writing.

use std::fmt::Write as FmtWrite;
use std::io::{self, BufRead, BufReader, Read, Write};

/// Numeric display base (`QTextStream` integer base).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NumberBase {
    #[default]
    Decimal = 10,
    Hex = 16,
    Octal = 8,
    Binary = 2,
}

/// Field alignment within formatted width (`QTextStream::FieldAlignment` equivalent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FieldAlignment {
    #[default]
    Right,
    Left,
    Center,
}

/// Formatted text stream (`QTextStream` equivalent).
pub struct TextStream<T = Vec<u8>> {
    inner: T,
    base: NumberBase,
    field_width: usize,
    pad_char: char,
    alignment: FieldAlignment,
    show_base: bool,
    force_sign: bool,
    uppercase_digits: bool,
}

impl TextStream<Vec<u8>> {
    /// Creates an empty in-memory formatted text stream (`QTextStream` over `QString`).
    pub fn new() -> Self {
        Self {
            inner: Vec::new(),
            base: NumberBase::Decimal,
            field_width: 0,
            pad_char: ' ',
            alignment: FieldAlignment::Right,
            show_base: false,
            force_sign: false,
            uppercase_digits: false,
        }
    }

    /// Consumes the stream, returning the formatted string.
    pub fn into_string(self) -> Result<String, std::string::FromUtf8Error> {
        String::from_utf8(self.inner)
    }
}

impl std::fmt::Display for TextStream<Vec<u8>> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", String::from_utf8_lossy(&self.inner))
    }
}

impl Default for TextStream<Vec<u8>> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> TextStream<T> {
    /// Wraps a custom target (reader or writer).
    pub fn wrap(inner: T) -> Self {
        Self {
            inner,
            base: NumberBase::Decimal,
            field_width: 0,
            pad_char: ' ',
            alignment: FieldAlignment::Right,
            show_base: false,
            force_sign: false,
            uppercase_digits: false,
        }
    }

    /// Returns the active number base.
    #[inline]
    pub fn integer_base(&self) -> NumberBase {
        self.base
    }

    /// Sets the number base.
    #[inline]
    pub fn set_integer_base(&mut self, base: NumberBase) {
        self.base = base;
    }

    /// Returns the target field width.
    #[inline]
    pub fn field_width(&self) -> usize {
        self.field_width
    }

    /// Sets the target field width (0 for unpadded).
    #[inline]
    pub fn set_field_width(&mut self, width: usize) {
        self.field_width = width;
    }

    /// Returns the padding fill character.
    #[inline]
    pub fn pad_char(&self) -> char {
        self.pad_char
    }

    /// Sets the padding fill character.
    #[inline]
    pub fn set_pad_char(&mut self, ch: char) {
        self.pad_char = ch;
    }

    /// Returns the field alignment.
    #[inline]
    pub fn field_alignment(&self) -> FieldAlignment {
        self.alignment
    }

    /// Sets the field alignment.
    #[inline]
    pub fn set_field_alignment(&mut self, alignment: FieldAlignment) {
        self.alignment = alignment;
    }

    /// Sets whether to include base prefixes (`0x`, `0b`, `0o`).
    #[inline]
    pub fn set_show_base(&mut self, show: bool) {
        self.show_base = show;
    }

    /// Sets whether to force a leading '+' sign on positive numbers.
    #[inline]
    pub fn set_force_sign(&mut self, force: bool) {
        self.force_sign = force;
    }

    /// Sets whether to render hexadecimal digits in uppercase.
    #[inline]
    pub fn set_uppercase_digits(&mut self, upper: bool) {
        self.uppercase_digits = upper;
    }

    /// Unwraps and returns the inner target.
    pub fn into_inner(self) -> T {
        self.inner
    }

    /// Formats an arbitrary text representation applying field width, padding, and alignment.
    fn format_field(&self, text: &str) -> String {
        // Counted in characters, not bytes: non-ASCII text is padded to the same width.
        let length = text.chars().count();
        if self.field_width == 0 || length >= self.field_width {
            return text.to_string();
        }

        let padding_needed = self.field_width - length;
        let mut out = String::with_capacity(self.field_width);

        match self.alignment {
            FieldAlignment::Right => {
                for _ in 0..padding_needed {
                    out.push(self.pad_char);
                }
                out.push_str(text);
            }
            FieldAlignment::Left => {
                out.push_str(text);
                for _ in 0..padding_needed {
                    out.push(self.pad_char);
                }
            }
            FieldAlignment::Center => {
                let left_pad = padding_needed / 2;
                let right_pad = padding_needed - left_pad;
                for _ in 0..left_pad {
                    out.push(self.pad_char);
                }
                out.push_str(text);
                for _ in 0..right_pad {
                    out.push(self.pad_char);
                }
            }
        }

        out
    }
}

// =============================================================================
// Writing methods (T: Write)
// =============================================================================

impl<W: Write> TextStream<W> {
    /// Writes text directly formatted according to width and alignment.
    pub fn write_str(&mut self, text: &str) -> io::Result<()> {
        let formatted = self.format_field(text);
        self.inner.write_all(formatted.as_bytes())
    }

    /// Writes text followed by a newline.
    pub fn write_line(&mut self, text: &str) -> io::Result<()> {
        self.write_str(text)?;
        self.inner.write_all(b"\n")
    }

    /// Writes a signed 64-bit integer formatted according to base, sign, and width.
    pub fn write_i64(&mut self, val: i64) -> io::Result<()> {
        let mut raw = String::new();

        if val < 0 {
            raw.push('-');
            let abs_val = val.unsigned_abs();
            self.format_unsigned(abs_val, &mut raw);
        } else {
            if self.force_sign {
                raw.push('+');
            }
            self.format_unsigned(val as u64, &mut raw);
        }

        self.write_str(&raw)
    }

    /// Writes an unsigned 64-bit integer formatted according to base and width.
    pub fn write_u64(&mut self, val: u64) -> io::Result<()> {
        let mut raw = String::new();
        if self.force_sign {
            raw.push('+');
        }
        self.format_unsigned(val, &mut raw);
        self.write_str(&raw)
    }

    /// Writes a 64-bit floating point number with optional decimal precision.
    pub fn write_f64(&mut self, val: f64, precision: Option<usize>) -> io::Result<()> {
        let mut raw = String::new();
        if self.force_sign && val >= 0.0 && !val.is_nan() {
            raw.push('+');
        }

        if let Some(prec) = precision {
            let _ = write!(&mut raw, "{:.prec$}", val);
        } else {
            let _ = write!(&mut raw, "{}", val);
        }

        self.write_str(&raw)
    }

    fn format_unsigned(&self, val: u64, out: &mut String) {
        match self.base {
            NumberBase::Decimal => {
                let _ = write!(out, "{val}");
            }
            NumberBase::Hex => {
                if self.show_base {
                    out.push_str(if self.uppercase_digits { "0X" } else { "0x" });
                }
                if self.uppercase_digits {
                    let _ = write!(out, "{val:X}");
                } else {
                    let _ = write!(out, "{val:x}");
                }
            }
            NumberBase::Octal => {
                if self.show_base {
                    out.push_str("0o");
                }
                let _ = write!(out, "{val:o}");
            }
            NumberBase::Binary => {
                if self.show_base {
                    out.push_str("0b");
                }
                let _ = write!(out, "{val:b}");
            }
        }
    }

    /// Flushes the underlying writer.
    pub fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

// =============================================================================
// Reading methods (R: Read)
// =============================================================================

impl<R: Read> TextStream<BufReader<R>> {
    /// Creates a reader text stream wrapping `reader` in a `BufReader`.
    pub fn from_reader(reader: R) -> Self {
        Self::wrap(BufReader::new(reader))
    }
}

impl<R: BufRead> TextStream<R> {
    /// Reads a single line of text from the stream, stripping the trailing newline.
    pub fn read_line(&mut self) -> io::Result<Option<String>> {
        let mut line = String::new();
        let bytes_read = self.inner.read_line(&mut line)?;
        if bytes_read == 0 {
            return Ok(None);
        }
        if line.ends_with('\n') {
            line.pop();
            if line.ends_with('\r') {
                line.pop();
            }
        }
        Ok(Some(line))
    }

    /// Reads all remaining text from the stream.
    pub fn read_all(&mut self) -> io::Result<String> {
        let mut out = String::new();
        self.inner.read_to_string(&mut out)?;
        Ok(out)
    }
}
