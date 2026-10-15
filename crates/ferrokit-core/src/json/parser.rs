// SPDX-License-Identifier: LGPL-3.0-only
//! RFC 8259 compliant JSON parser with zero external dependencies.
//!
//! Provides tokenization, recursive descent parsing, escape processing (including `\uXXXX`
//! Unicode escapes and UTF-16 surrogate pairs), and precise line/column error reporting.

use std::fmt;

use super::array::JsonArray;
use super::object::JsonObject;
use super::value::JsonValue;

/// Specific error reason for a JSON parse failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseErrorReason {
    UnexpectedEof,
    UnexpectedChar(char),
    UnterminatedString,
    InvalidEscapeSequence,
    InvalidUnicodeEscape,
    InvalidNumber,
    ExpectedColon,
    ExpectedCommaOrCloser,
}

impl fmt::Display for ParseErrorReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnexpectedEof => write!(f, "unexpected end of file"),
            Self::UnexpectedChar(c) => write!(f, "unexpected character '{c}'"),
            Self::UnterminatedString => write!(f, "unterminated string literal"),
            Self::InvalidEscapeSequence => write!(f, "invalid escape sequence"),
            Self::InvalidUnicodeEscape => write!(f, "invalid unicode escape sequence"),
            Self::InvalidNumber => write!(f, "invalid numeric format"),
            Self::ExpectedColon => write!(f, "expected ':' after object key"),
            Self::ExpectedCommaOrCloser => write!(f, "expected ',' or closing bracket/brace"),
        }
    }
}

/// JSON parse error details (`QJsonParseError` equivalent).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonParseError {
    /// Byte offset in input where the error occurred.
    pub offset: usize,
    /// 1-based line number.
    pub line: usize,
    /// 1-based column number.
    pub column: usize,
    /// Cause of the parse error.
    pub reason: ParseErrorReason,
}

impl JsonParseError {
    /// A readable message saying where parsing failed and why.
    pub fn error_string(&self) -> String {
        format!(
            "JSON parse error at line {}, col {}: {}",
            self.line, self.column, self.reason
        )
    }
}

impl fmt::Display for JsonParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.error_string())
    }
}

impl std::error::Error for JsonParseError {}

/// Internal recursive descent parser.
pub(crate) struct Parser<'a> {
    chars: &'a [u8],
    pos: usize,
    line: usize,
    col: usize,
}

impl<'a> Parser<'a> {
    pub fn new(input: &'a [u8]) -> Self {
        Self {
            chars: input,
            pos: 0,
            line: 1,
            col: 1,
        }
    }

    pub fn parse(&mut self) -> Result<JsonValue, JsonParseError> {
        self.skip_whitespace();
        if self.is_eof() {
            return Err(self.error(ParseErrorReason::UnexpectedEof));
        }

        let val = self.parse_value()?;
        self.skip_whitespace();

        if !self.is_eof() {
            let ch = self.peek_char().unwrap_or('\0');
            return Err(self.error(ParseErrorReason::UnexpectedChar(ch)));
        }

        Ok(val)
    }

    fn parse_value(&mut self) -> Result<JsonValue, JsonParseError> {
        self.skip_whitespace();
        let ch = match self.peek_char() {
            Some(c) => c,
            None => return Err(self.error(ParseErrorReason::UnexpectedEof)),
        };

        match ch {
            'n' => self.parse_null(),
            't' | 'f' => self.parse_bool(),
            '"' => self.parse_string().map(JsonValue::String),
            '[' => self.parse_array().map(JsonValue::Array),
            '{' => self.parse_object().map(JsonValue::Object),
            '-' | '0'..='9' => self.parse_number(),
            other => Err(self.error(ParseErrorReason::UnexpectedChar(other))),
        }
    }

    fn parse_null(&mut self) -> Result<JsonValue, JsonParseError> {
        if self.consume_keyword(b"null") {
            Ok(JsonValue::Null)
        } else {
            let ch = self.peek_char().unwrap_or('\0');
            Err(self.error(ParseErrorReason::UnexpectedChar(ch)))
        }
    }

    fn parse_bool(&mut self) -> Result<JsonValue, JsonParseError> {
        if self.consume_keyword(b"true") {
            Ok(JsonValue::Bool(true))
        } else if self.consume_keyword(b"false") {
            Ok(JsonValue::Bool(false))
        } else {
            let ch = self.peek_char().unwrap_or('\0');
            Err(self.error(ParseErrorReason::UnexpectedChar(ch)))
        }
    }

    fn parse_string(&mut self) -> Result<String, JsonParseError> {
        if self.advance() != Some(b'"') {
            return Err(self.error(ParseErrorReason::UnexpectedChar('"')));
        }

        let mut out = String::new();

        while let Some(b) = self.advance() {
            match b {
                b'"' => return Ok(out),
                b'\\' => {
                    let esc = match self.advance() {
                        Some(e) => e,
                        None => return Err(self.error(ParseErrorReason::UnterminatedString)),
                    };
                    match esc {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\x08'),
                        b'f' => out.push('\x0c'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let code = self.parse_hex4()?;
                            // Check for UTF-16 surrogate pair
                            if (0xD800..=0xDBFF).contains(&code) {
                                // High surrogate; expect \uXXXX low surrogate
                                if self.consume_keyword(b"\\u") {
                                    let low = self.parse_hex4()?;
                                    if (0xDC00..=0xDFFF).contains(&low) {
                                        let codepoint = 0x10000
                                            + (((code - 0xD800) as u32) << 10)
                                            + ((low - 0xDC00) as u32);
                                        if let Some(ch) = char::from_u32(codepoint) {
                                            out.push(ch);
                                        } else {
                                            out.push('\u{FFFD}');
                                        }
                                    } else {
                                        out.push('\u{FFFD}');
                                    }
                                } else {
                                    out.push('\u{FFFD}');
                                }
                            } else if let Some(ch) = char::from_u32(code as u32) {
                                out.push(ch);
                            } else {
                                out.push('\u{FFFD}');
                            }
                        }
                        _ => return Err(self.error(ParseErrorReason::InvalidEscapeSequence)),
                    }
                }
                b if b < 0x20 => {
                    // Control characters in JSON strings must be escaped
                    return Err(self.error(ParseErrorReason::InvalidEscapeSequence));
                }
                b => {
                    // Valid UTF-8 byte
                    out.push(b as char);
                }
            }
        }

        Err(self.error(ParseErrorReason::UnterminatedString))
    }

    fn parse_hex4(&mut self) -> Result<u16, JsonParseError> {
        let mut val = 0u16;
        for _ in 0..4 {
            let b = match self.advance() {
                Some(ch) => ch,
                None => return Err(self.error(ParseErrorReason::InvalidUnicodeEscape)),
            };
            let digit = match b {
                b'0'..=b'9' => (b - b'0') as u16,
                b'a'..=b'f' => (b - b'a' + 10) as u16,
                b'A'..=b'F' => (b - b'A' + 10) as u16,
                _ => return Err(self.error(ParseErrorReason::InvalidUnicodeEscape)),
            };
            val = (val << 4) | digit;
        }
        Ok(val)
    }

    fn parse_number(&mut self) -> Result<JsonValue, JsonParseError> {
        let start = self.pos;

        // Optional negative sign
        if self.peek_byte() == Some(b'-') {
            self.advance();
        }

        let first_digit_pos = self.pos;
        while let Some(b) = self.peek_byte() {
            if b.is_ascii_digit() {
                self.advance();
            } else {
                break;
            }
        }

        if self.pos == first_digit_pos {
            return Err(self.error(ParseErrorReason::InvalidNumber));
        }

        // Fractional part
        if self.peek_byte() == Some(b'.') {
            self.advance();
            let frac_start = self.pos;
            while let Some(b) = self.peek_byte() {
                if b.is_ascii_digit() {
                    self.advance();
                } else {
                    break;
                }
            }
            if self.pos == frac_start {
                return Err(self.error(ParseErrorReason::InvalidNumber));
            }
        }

        // Exponent part
        if let Some(b'e' | b'E') = self.peek_byte() {
            self.advance();
            if let Some(b'+' | b'-') = self.peek_byte() {
                self.advance();
            }
            let exp_start = self.pos;
            while let Some(b) = self.peek_byte() {
                if b.is_ascii_digit() {
                    self.advance();
                } else {
                    break;
                }
            }
            if self.pos == exp_start {
                return Err(self.error(ParseErrorReason::InvalidNumber));
            }
        }

        let num_slice = &self.chars[start..self.pos];
        let num_str = std::str::from_utf8(num_slice)
            .map_err(|_| self.error(ParseErrorReason::InvalidNumber))?;

        let num: f64 = num_str
            .parse()
            .map_err(|_| self.error(ParseErrorReason::InvalidNumber))?;
        Ok(JsonValue::Number(num))
    }

    fn parse_array(&mut self) -> Result<JsonArray, JsonParseError> {
        self.advance(); // consume '['
        self.skip_whitespace();

        let mut array = JsonArray::new();

        if self.peek_byte() == Some(b']') {
            self.advance();
            return Ok(array);
        }

        loop {
            let elem = self.parse_value()?;
            array.push(elem);

            self.skip_whitespace();
            match self.peek_byte() {
                Some(b',') => {
                    self.advance();
                    self.skip_whitespace();
                    // Detect invalid trailing comma in array
                    if self.peek_byte() == Some(b']') {
                        return Err(self.error(ParseErrorReason::UnexpectedChar(']')));
                    }
                }
                Some(b']') => {
                    self.advance();
                    break;
                }
                _ => return Err(self.error(ParseErrorReason::ExpectedCommaOrCloser)),
            }
        }

        Ok(array)
    }

    fn parse_object(&mut self) -> Result<JsonObject, JsonParseError> {
        self.advance(); // consume '{'
        self.skip_whitespace();

        let mut obj = JsonObject::new();

        if self.peek_byte() == Some(b'}') {
            self.advance();
            return Ok(obj);
        }

        loop {
            self.skip_whitespace();
            if self.peek_byte() != Some(b'"') {
                return Err(self.error(ParseErrorReason::UnexpectedChar(
                    self.peek_char().unwrap_or('\0'),
                )));
            }

            let key = self.parse_string()?;

            self.skip_whitespace();
            if self.advance() != Some(b':') {
                return Err(self.error(ParseErrorReason::ExpectedColon));
            }

            let val = self.parse_value()?;
            obj.insert(key, val);

            self.skip_whitespace();
            match self.peek_byte() {
                Some(b',') => {
                    self.advance();
                    self.skip_whitespace();
                    // Detect invalid trailing comma in object
                    if self.peek_byte() == Some(b'}') {
                        return Err(self.error(ParseErrorReason::UnexpectedChar('}')));
                    }
                }
                Some(b'}') => {
                    self.advance();
                    break;
                }
                _ => return Err(self.error(ParseErrorReason::ExpectedCommaOrCloser)),
            }
        }

        Ok(obj)
    }

    fn skip_whitespace(&mut self) {
        while let Some(b) = self.peek_byte() {
            match b {
                b' ' | b'\t' | b'\r' => {
                    self.advance();
                }
                b'\n' => {
                    self.advance();
                }
                _ => break,
            }
        }
    }

    #[inline]
    fn is_eof(&self) -> bool {
        self.pos >= self.chars.len()
    }

    #[inline]
    fn peek_byte(&self) -> Option<u8> {
        self.chars.get(self.pos).copied()
    }

    #[inline]
    fn peek_char(&self) -> Option<char> {
        self.peek_byte().map(|b| b as char)
    }

    fn advance(&mut self) -> Option<u8> {
        if self.pos < self.chars.len() {
            let b = self.chars[self.pos];
            self.pos += 1;
            if b == b'\n' {
                self.line += 1;
                self.col = 1;
            } else {
                self.col += 1;
            }
            Some(b)
        } else {
            None
        }
    }

    fn consume_keyword(&mut self, kw: &[u8]) -> bool {
        if self.pos + kw.len() <= self.chars.len()
            && &self.chars[self.pos..self.pos + kw.len()] == kw
        {
            for _ in 0..kw.len() {
                self.advance();
            }
            true
        } else {
            false
        }
    }

    fn error(&self, reason: ParseErrorReason) -> JsonParseError {
        JsonParseError {
            offset: self.pos,
            line: self.line,
            column: self.col,
            reason,
        }
    }
}
