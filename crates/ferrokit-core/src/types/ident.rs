// SPDX-License-Identifier: LGPL-3.0-only
//! Identifiers and web types (`QUrl`, `QUuid`, `QRegularExpression`, `QLocale` equivalents).

use std::fmt;
use std::path::{Path, PathBuf};

/// Query string manipulation helper (`QUrlQuery` equivalent).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UrlQuery {
    items: Vec<(String, String)>,
}

impl UrlQuery {
    /// Creates an empty UrlQuery.
    pub fn new() -> Self {
        Self { items: Vec::new() }
    }

    /// Parses a URL query string (without the leading '?').
    pub fn from_query_string(qs: &str) -> Self {
        let clean = qs.strip_prefix('?').unwrap_or(qs);
        let mut items = Vec::new();
        if clean.is_empty() {
            return Self { items };
        }

        for pair in clean.split('&') {
            if pair.is_empty() {
                continue;
            }
            if let Some((k, v)) = pair.split_once('=') {
                items.push((Self::percent_decode(k), Self::percent_decode(v)));
            } else {
                items.push((Self::percent_decode(pair), String::new()));
            }
        }
        Self { items }
    }

    /// Adds a key-value query item.
    pub fn add_query_item(&mut self, key: &str, value: &str) {
        self.items.push((key.to_string(), value.to_string()));
    }

    /// Returns `true` if a query item with the specified key exists.
    pub fn has_query_item(&self, key: &str) -> bool {
        self.items.iter().any(|(k, _)| k == key)
    }

    /// Returns the value of the first matching query item.
    pub fn query_item_value(&self, key: &str) -> Option<&str> {
        self.items
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// Returns all values associated with the specified key.
    pub fn all_query_item_values(&self, key: &str) -> Vec<&str> {
        self.items
            .iter()
            .filter(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
            .collect()
    }

    /// Removes all query items with the specified key.
    pub fn remove_query_item(&mut self, key: &str) {
        self.items.retain(|(k, _)| k != key);
    }

    /// Returns the list of all (key, value) pairs.
    pub fn query_items(&self) -> &[(String, String)] {
        &self.items
    }

    /// Clears all query items.
    pub fn clear(&mut self) {
        self.items.clear();
    }

    /// Returns `true` if there are no query items.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Serializes the query items into standard `key=val&key2=val2` query string.
    pub fn to_query_string(&self) -> String {
        if self.items.is_empty() {
            return String::new();
        }
        let mut out = String::new();
        for (i, (k, v)) in self.items.iter().enumerate() {
            if i > 0 {
                out.push('&');
            }
            out.push_str(&Self::percent_encode(k));
            if !v.is_empty() {
                out.push('=');
                out.push_str(&Self::percent_encode(v));
            }
        }
        out
    }

    fn percent_encode(s: &str) -> String {
        let mut res = String::with_capacity(s.len());
        for b in s.bytes() {
            if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.' || b == b'~' {
                res.push(b as char);
            } else {
                res.push_str(&format!("%{:02X}", b));
            }
        }
        res
    }

    fn percent_decode(s: &str) -> String {
        let mut bytes = Vec::with_capacity(s.len());
        let s_bytes = s.as_bytes();
        let mut i = 0;
        while i < s_bytes.len() {
            if s_bytes[i] == b'%' && i + 2 < s_bytes.len() {
                if let Ok(val) = u8::from_str_radix(
                    std::str::from_utf8(&s_bytes[i + 1..i + 3]).unwrap_or(""),
                    16,
                ) {
                    bytes.push(val);
                    i += 3;
                    continue;
                }
            } else if s_bytes[i] == b'+' {
                bytes.push(b' ');
                i += 1;
                continue;
            }
            bytes.push(s_bytes[i]);
            i += 1;
        }
        String::from_utf8_lossy(&bytes).to_string()
    }
}

impl fmt::Display for UrlQuery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_query_string())
    }
}

/// Uniform Resource Locator (`QUrl` equivalent).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Url {
    raw: String,
}

impl Url {
    /// Creates a new Url from a string slice.
    pub fn new(url: impl Into<String>) -> Self {
        Self { raw: url.into() }
    }

    /// Returns the raw URL string.
    pub fn as_str(&self) -> &str {
        &self.raw
    }

    /// Returns `true` if the URL is empty or invalid.
    pub fn is_empty(&self) -> bool {
        self.raw.is_empty()
    }

    /// Extracts the URL scheme if present (e.g. "https", "file").
    pub fn scheme(&self) -> Option<&str> {
        let (scheme, _) = self.raw.split_once("://")?;
        Some(scheme)
    }

    /// Extracts the host component if present.
    pub fn host(&self) -> Option<&str> {
        let (_, rest) = self.raw.split_once("://")?;
        let host_port = rest.split(['/', '?', '#']).next()?;
        let (host, _) = host_port.split_once(':').unwrap_or((host_port, ""));
        if host.is_empty() {
            None
        } else {
            Some(host)
        }
    }

    /// Extracts the port number if present.
    pub fn port(&self) -> Option<u16> {
        let (_, rest) = self.raw.split_once("://")?;
        let host_port = rest.split(['/', '?', '#']).next()?;
        let (_, port_str) = host_port.split_once(':')?;
        port_str.parse::<u16>().ok()
    }

    /// Extracts the path component.
    pub fn path(&self) -> &str {
        if let Some((_, rest)) = self.raw.split_once("://") {
            if let Some(idx) = rest.find('/') {
                let path_and_beyond = &rest[idx..];
                path_and_beyond.split(['?', '#']).next().unwrap_or("/")
            } else {
                "/"
            }
        } else {
            &self.raw
        }
    }

    /// Extracts the query string if present (without '?').
    pub fn query(&self) -> Option<&str> {
        let (_, query_and_frag) = self.raw.split_once('?')?;
        let (query, _) = query_and_frag
            .split_once('#')
            .unwrap_or((query_and_frag, ""));
        Some(query)
    }

    /// Returns parsed `UrlQuery` from the URL's query component.
    pub fn query_items(&self) -> UrlQuery {
        self.query()
            .map(UrlQuery::from_query_string)
            .unwrap_or_default()
    }

    /// Sets the query string component of the URL.
    pub fn set_query(&mut self, query: &str) {
        let base = if let Some((before_q, _)) = self.raw.split_once('?') {
            before_q
        } else if let Some((before_f, _)) = self.raw.split_once('#') {
            before_f
        } else {
            &self.raw
        };

        let fragment = self.raw.split_once('#').map(|(_, f)| f);

        let clean_q = query.strip_prefix('?').unwrap_or(query);
        let mut new_raw = base.to_string();
        if !clean_q.is_empty() {
            new_raw.push('?');
            new_raw.push_str(clean_q);
        }
        if let Some(frag) = fragment {
            new_raw.push('#');
            new_raw.push_str(frag);
        }
        self.raw = new_raw;
    }

    /// Sets the query from a `UrlQuery` object.
    pub fn set_query_items(&mut self, query: &UrlQuery) {
        self.set_query(&query.to_query_string());
    }

    /// Creates a file URL from a local file system path (`fromLocalFile` equivalent).
    pub fn from_local_file(path: impl AsRef<Path>) -> Self {
        let path_str = path.as_ref().to_string_lossy();
        let forward_slashed = path_str.replace('\\', "/");
        let norm_path = if forward_slashed.starts_with('/') {
            forward_slashed
        } else {
            format!("/{}", forward_slashed)
        };
        Self::new(format!("file://{}", norm_path))
    }

    /// Extracts a local file system path if the URL scheme is `file` (`toLocalFile` equivalent).
    pub fn to_local_file(&self) -> Option<PathBuf> {
        if self.scheme()? != "file" {
            return None;
        }
        let raw_path = self.path();
        // Handle Windows paths: file:///C:/path -> C:/path
        let clean = if raw_path.len() >= 3
            && raw_path.starts_with('/')
            && raw_path.as_bytes()[1].is_ascii_alphabetic()
            && raw_path.as_bytes()[2] == b':'
        {
            &raw_path[1..]
        } else {
            raw_path
        };
        Some(PathBuf::from(clean))
    }
}

impl fmt::Display for Url {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.raw)
    }
}

impl From<&str> for Url {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

impl From<String> for Url {
    fn from(s: String) -> Self {
        Self::new(s)
    }
}

/// Universally unique identifier (128-bit) (`QUuid` equivalent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Uuid {
    pub bytes: [u8; 16],
}

impl Uuid {
    /// Creates a nil (all zero) UUID.
    pub const fn nil() -> Self {
        Self { bytes: [0; 16] }
    }

    /// Creates a Uuid from raw 16 bytes.
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self { bytes }
    }

    /// Returns the raw 16 bytes.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.bytes
    }

    /// Returns `true` if the UUID is nil (all zeroes).
    pub fn is_null(&self) -> bool {
        self.bytes == [0; 16]
    }

    /// Parses a standard UUID string `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx` or with `{...}`.
    pub fn from_string(s: &str) -> Option<Self> {
        let clean = s.trim().trim_start_matches('{').trim_end_matches('}');
        let hex_only: String = clean.chars().filter(|c| *c != '-').collect();
        if hex_only.len() != 32 {
            return None;
        }

        let mut bytes = [0u8; 16];
        for (i, byte) in bytes.iter_mut().enumerate() {
            let chunk = &hex_only[i * 2..i * 2 + 2];
            *byte = u8::from_str_radix(chunk, 16).ok()?;
        }
        Some(Self { bytes })
    }

    /// Formats as standard canonical string `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`.
    pub fn to_string_canonical(&self) -> String {
        format!(
            "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            self.bytes[0], self.bytes[1], self.bytes[2], self.bytes[3],
            self.bytes[4], self.bytes[5],
            self.bytes[6], self.bytes[7],
            self.bytes[8], self.bytes[9],
            self.bytes[10], self.bytes[11], self.bytes[12], self.bytes[13], self.bytes[14], self.bytes[15]
        )
    }
}

impl fmt::Display for Uuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_string_canonical())
    }
}

/// Regular expression pattern container (`QRegularExpression` equivalent).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct RegularExpression {
    pattern: String,
    case_insensitive: bool,
}

impl RegularExpression {
    /// Creates a new regular expression pattern.
    pub fn new(pattern: impl Into<String>) -> Self {
        Self {
            pattern: pattern.into(),
            case_insensitive: false,
        }
    }

    /// Sets case-insensitive matching mode.
    pub fn with_case_insensitive(mut self, insensitive: bool) -> Self {
        self.case_insensitive = insensitive;
        self
    }

    /// Returns the pattern string.
    pub fn pattern(&self) -> &str {
        &self.pattern
    }

    /// Returns `true` if case-insensitive.
    pub fn is_case_insensitive(&self) -> bool {
        self.case_insensitive
    }
}

impl fmt::Display for RegularExpression {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.pattern)
    }
}

impl From<&str> for RegularExpression {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

impl From<String> for RegularExpression {
    fn from(s: String) -> Self {
        Self::new(s)
    }
}

/// Locale identifier (`QLocale` equivalent).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Locale {
    name: String,
}

impl Locale {
    /// Creates a system default locale ("C").
    pub fn c() -> Self {
        Self {
            name: "C".to_string(),
        }
    }

    /// Creates a locale from a BCP47 / POSIX name (e.g. "en_US", "zh_TW").
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }

    /// Returns the locale name.
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl fmt::Display for Locale {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name)
    }
}

impl From<&str> for Locale {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

impl From<String> for Locale {
    fn from(s: String) -> Self {
        Self::new(s)
    }
}
