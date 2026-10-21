// SPDX-License-Identifier: LGPL-3.0-only
//! QSettings configuration persistence (`QSettings` equivalent).
//!
//! Provides cross-platform key-value configuration storage in standard INI format,
//! hierarchical group management (`begin_group` / `end_group`), typed `Variant` access,
//! and atomic writes via temporary files and OS renames.

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use crate::variant::Variant;

/// Configuration format matching Qt `QSettings::Format`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Format {
    #[default]
    IniFormat,
}

/// Configuration scope matching Qt `QSettings::Scope`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Scope {
    #[default]
    UserScope,
    SystemScope,
}

/// Cross-platform persistent configuration store matching Qt `QSettings`.
pub struct Settings {
    file_path: Option<PathBuf>,
    entries: HashMap<String, Variant>,
    group_stack: Vec<String>,
    dirty: bool,
    load_failed: bool,
}
impl Settings {
    /// Creates a new settings instance with an explicit file path.
    pub fn new(path: impl AsRef<Path>) -> Self {
        let p = path.as_ref().to_path_buf();
        let mut s = Self {
            file_path: Some(p.clone()),
            entries: HashMap::new(),
            group_stack: Vec::new(),
            dirty: false,
            load_failed: false,
        };
        if s.load_from_disk().is_err() {
            s.load_failed = true;
        }
        s
    }

    /// Creates an in-memory settings store without an associated file.
    pub fn in_memory() -> Self {
        Self {
            file_path: None,
            entries: HashMap::new(),
            group_stack: Vec::new(),
            dirty: false,
            load_failed: false,
        }
    }

    /// Resolves standard config path for organization and application name.
    pub fn from_organization(organization: &str, application: &str) -> Self {
        let config_dir = default_config_directory();
        let org_dir = config_dir.join(organization);
        let file_path = org_dir.join(format!("{}.ini", application));
        Self::new(file_path)
    }

    /// Returns the active file path if persistent.
    pub fn file_path(&self) -> Option<&Path> {
        self.file_path.as_deref()
    }

    /// Appends a prefix to the current group stack (`QSettings::beginGroup`).
    pub fn begin_group(&mut self, prefix: &str) {
        if !prefix.is_empty() {
            self.group_stack.push(prefix.to_string());
        }
    }

    /// Resets the current group to its previous state (`QSettings::endGroup`).
    pub fn end_group(&mut self) {
        self.group_stack.pop();
    }

    /// Returns the current active group string (e.g., `"App/Display"`).
    pub fn group(&self) -> String {
        self.group_stack.join("/")
    }

    /// Builds the fully qualified hierarchical key name.
    fn make_full_key(&self, key: &str) -> String {
        let g = self.group();
        if g.is_empty() {
            key.to_string()
        } else {
            format!("{}/{}", g, key)
        }
    }

    /// Sets the value of setting `key`.
    pub fn set_value(&mut self, key: &str, value: impl Into<Variant>) {
        let full_key = self.make_full_key(key);
        let val = value.into();
        self.entries.insert(full_key, val);
        self.dirty = true;
    }

    /// Returns the value of setting `key`.
    pub fn value(&self, key: &str) -> Option<Variant> {
        let full_key = self.make_full_key(key);
        self.entries.get(&full_key).cloned()
    }

    /// Returns the value of setting `key`, or `default` if not found.
    pub fn value_or(&self, key: &str, default: impl Into<Variant>) -> Variant {
        self.value(key).unwrap_or_else(|| default.into())
    }

    /// Returns true if setting `key` exists.
    pub fn contains(&self, key: &str) -> bool {
        let full_key = self.make_full_key(key);
        self.entries.contains_key(&full_key)
    }

    /// Removes the setting `key` and any sub-settings.
    pub fn remove(&mut self, key: &str) {
        let full_key = self.make_full_key(key);
        let prefix = format!("{}/", full_key);
        self.entries
            .retain(|k, _| k != &full_key && !k.starts_with(&prefix));
        self.dirty = true;
    }

    /// Returns all keys in the current group.
    pub fn child_keys(&self) -> Vec<String> {
        let current_group = self.group();
        let prefix = if current_group.is_empty() {
            String::new()
        } else {
            format!("{}/", current_group)
        };

        let mut keys = Vec::new();
        for k in self.entries.keys() {
            if let Some(sub) = k.strip_prefix(&prefix) {
                if !sub.contains('/') {
                    keys.push(sub.to_string());
                }
            }
        }
        keys.sort();
        keys
    }

    /// Returns all sub-groups in the current group.
    pub fn child_groups(&self) -> Vec<String> {
        let current_group = self.group();
        let prefix = if current_group.is_empty() {
            String::new()
        } else {
            format!("{}/", current_group)
        };

        let mut groups = std::collections::HashSet::new();
        for k in self.entries.keys() {
            if let Some(sub) = k.strip_prefix(&prefix) {
                if let Some((grp, _)) = sub.split_once('/') {
                    groups.insert(grp.to_string());
                }
            }
        }
        let mut list: Vec<String> = groups.into_iter().collect();
        list.sort();
        list
    }

    /// Returns all registered keys across all groups.
    pub fn all_keys(&self) -> Vec<String> {
        let mut list: Vec<String> = self.entries.keys().cloned().collect();
        list.sort();
        list
    }

    /// Clears all settings entries.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.dirty = true;
    }

    /// Writes unsaved changes to disk atomically.
    pub fn sync(&mut self) -> std::io::Result<()> {
        if !self.dirty {
            return Ok(());
        }
        if self.load_failed {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "settings file could not be loaded completely",
            ));
        }

        let Some(path) = &self.file_path else {
            return Ok(());
        };

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Write to temporary file first for atomic commit
        let tmp_path = path.with_extension(format!("tmp_{}", std::process::id()));
        {
            let mut file = File::create(&tmp_path)?;
            self.write_ini_to(&mut file)?;
            file.flush()?;
            file.sync_all()?;
        }

        // Atomic replace via rename
        fs::rename(&tmp_path, path)?;
        #[cfg(unix)]
        if let Some(parent) = path.parent() {
            // The rename already committed the data; directory sync failure is not an error.
            if let Ok(dir) = File::open(parent) {
                let _ = dir.sync_all();
            }
        }
        self.dirty = false;
        Ok(())
    }

    /// Serializes entries into INI format grouped by section.
    fn write_ini_to<W: Write>(&self, w: &mut W) -> std::io::Result<()> {
        // Partition entries into root section and named sections
        let mut root_entries = BTreeMap::new();
        let mut sections: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();

        for (k, v) in &self.entries {
            let str_val = serialize_variant_value(v);
            if let Some((section, sub_key)) = k.split_once('/') {
                sections
                    .entry(section.to_string())
                    .or_default()
                    .insert(sub_key.to_string(), str_val);
            } else {
                root_entries.insert(k.clone(), str_val);
            }
        }

        // Marker: only files carrying it are unescaped on load; unmarked (older) files load verbatim.
        writeln!(w, "{}", ESCAPE_MARKER)?;

        // Write root entries first
        for (k, v) in root_entries {
            writeln!(w, "{}={}", escape_text(&k), v)?;
        }

        // Write each section
        for (section, items) in sections {
            writeln!(w)?;
            writeln!(w, "[{}]", escape_text(&section))?;
            for (k, v) in items {
                writeln!(w, "{}={}", escape_text(&k), v)?;
            }
        }

        Ok(())
    }

    /// Reads INI file from disk if present.
    fn load_from_disk(&mut self) -> std::io::Result<()> {
        let Some(path) = &self.file_path else {
            return Ok(());
        };

        if !path.exists() {
            return Ok(());
        }

        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let mut current_section: Option<String> = None;

        let mut escaped = false;
        for (index, line) in reader.lines().enumerate() {
            let line = line?;
            let trimmed = line.trim();
            if index == 0 && trimmed == ESCAPE_MARKER {
                escaped = true;
                continue;
            }

            if trimmed.is_empty() || trimmed.starts_with(';') || trimmed.starts_with('#') {
                continue;
            }

            if trimmed.starts_with('[') && trimmed.ends_with(']') {
                let section_name = &trimmed[1..trimmed.len() - 1];
                let name = section_name.trim();
                current_section = Some(if escaped { unescape_text(name) } else { name.to_string() });
                continue;
            }

            let pair = if escaped {
                split_unescaped_eq(trimmed)
            } else {
                trimmed.split_once('=')
            };
            if let Some((key, val)) = pair {
                let key = if escaped { unescape_text(key.trim()) } else { key.trim().to_string() };
                let key = key.as_str();
                let val = val.trim();
                let full_key = match &current_section {
                    Some(s) if !s.is_empty() => format!("{}/{}", s, key),
                    _ => key.to_string(),
                };
                let parsed = parse_variant_value(val, escaped);
                self.entries.insert(full_key, parsed);
            }
        }

        self.dirty = false;
        Ok(())
    }
}

impl Drop for Settings {
    fn drop(&mut self) {
        if self.dirty {
            let _ = self.sync();
        }
    }
}

/// First line of files whose keys/values use backslash escapes.
const ESCAPE_MARKER: &str = "; ferrokit-settings-escaped-v1";

/// Escapes characters that would break the line-based INI structure.
fn escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '=' | ';' | '#' | '[' | ']' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

/// Reverses `escape_text`; unknown escape sequences are kept literally so older files
/// containing backslashes still load.
fn unescape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.peek().copied() {
            Some('\\') => out.push('\\'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some(e @ ('=' | ';' | '#' | '[' | ']')) => out.push(e),
            _ => {
                out.push('\\');
                continue;
            }
        }
        chars.next();
    }
    out
}

/// Splits at the first `=` that is not preceded by an escaping backslash.
fn split_unescaped_eq(line: &str) -> Option<(&str, &str)> {
    let mut escaped = false;
    for (i, c) in line.char_indices() {
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == '=' {
            return Some((&line[..i], &line[i + 1..]));
        }
    }
    None
}

fn serialize_variant_value(v: &Variant) -> String {
    // Qt INI convention: plain strings stay human-readable; @Variant only for other types
    // and for strings that would not round-trip through the plain parser.
    if let Variant::String(text) = v {
        let plain_safe = text.trim() == text
            && !text.starts_with("@Variant(")
            && matches!(parse_variant_value(&escape_text(text), true), Variant::String(ref t) if t == text);
        if plain_safe {
            return escape_text(text);
        }
    }
    let bytes = v.to_bytes();
    let mut encoded = String::with_capacity(bytes.len() * 2 + 10);
    encoded.push_str("@Variant(");
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded.push(')');
    encoded
}

fn parse_variant_value(raw: &str, escaped: bool) -> Variant {
    if let Some(encoded) = raw.strip_prefix("@Variant(").and_then(|s| s.strip_suffix(')')) {
        if encoded.len() % 2 == 0 {
            let bytes = encoded.as_bytes();
            let decoded = bytes
                .chunks_exact(2)
                .map(|pair| {
                    let high = (pair[0] as char).to_digit(16)?;
                    let low = (pair[1] as char).to_digit(16)?;
                    Some(((high << 4) | low) as u8)
                })
                .collect::<Option<Vec<_>>>();
            if let Some(decoded) = decoded {
                if let Ok(value) = Variant::from_bytes(&decoded) {
                    return value;
                }
            }
        }
    }

    // Check boolean
    if raw.eq_ignore_ascii_case("true") {
        return Variant::Bool(true);
    }
    if raw.eq_ignore_ascii_case("false") {
        return Variant::Bool(false);
    }

    // Check integer
    if let Ok(i) = raw.parse::<i64>() {
        return Variant::I64(i);
    }

    // Check float
    if let Ok(f) = raw.parse::<f64>() {
        return Variant::F64(f);
    }

    // Check Hex Color #RRGGBBAA
    if raw.starts_with('#') && (raw.len() == 9 || raw.len() == 7) {
        if let Ok(hex) = u32::from_str_radix(&raw[1..], 16) {
            if raw.len() == 9 {
                let r = ((hex >> 24) & 0xFF) as u8;
                let g = ((hex >> 16) & 0xFF) as u8;
                let b = ((hex >> 8) & 0xFF) as u8;
                let a = (hex & 0xFF) as u8;
                return Variant::Color(r, g, b, a);
            } else {
                let r = ((hex >> 16) & 0xFF) as u8;
                let g = ((hex >> 8) & 0xFF) as u8;
                let b = (hex & 0xFF) as u8;
                return Variant::Color(r, g, b, 255);
            }
        }
    }

    // Default to String
    Variant::String(if escaped { unescape_text(raw) } else { raw.to_string() })
}

/// Resolves standard platform config directory.
fn default_config_directory() -> PathBuf {
    #[cfg(windows)]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            return PathBuf::from(appdata);
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join("Library/Preferences");
        }
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    {
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
            return PathBuf::from(xdg);
        }
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join(".config");
        }
    }

    std::env::temp_dir()
}
