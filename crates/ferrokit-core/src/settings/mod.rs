// SPDX-License-Identifier: LGPL-3.0-only
//! QSettings configuration persistence (`QSettings` equivalent).
//!
//! Provides cross-platform key-value configuration storage in standard INI format,
//! hierarchical group management (`begin_group` / `end_group`), typed `Variant` access,
//! and atomic writes via temporary files and OS renames.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::io::{LockError, LockFile};
use crate::variant::Variant;

/// Distinguishes the temporary files of concurrent `sync` calls within this process.
static SYNC_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Held across the re-read, merge and replace steps of `sync`.
static SYNC_LOCK: Mutex<()> = Mutex::new(());

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
    /// Keys set since the last read of the file; they win over the file's values on `sync`.
    modified: HashSet<String>,
    /// Keys removed since the last read of the file; `sync` removes them from the file.
    removed: HashSet<String>,
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
            modified: HashSet::new(),
            removed: HashSet::new(),
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
            modified: HashSet::new(),
            removed: HashSet::new(),
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
        self.removed.remove(&full_key);
        self.modified.insert(full_key.clone());
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
        let doomed: Vec<String> = self
            .entries
            .keys()
            .filter(|k| **k == full_key || k.starts_with(&prefix))
            .cloned()
            .collect();
        for k in doomed {
            self.entries.remove(&k);
            self.modified.remove(&k);
            self.removed.insert(k);
        }
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
            if let Some(sub) = k.strip_prefix(&prefix)
                && !sub.contains('/')
            {
                keys.push(sub.to_string());
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
            if let Some(sub) = k.strip_prefix(&prefix)
                && let Some((grp, _)) = sub.split_once('/')
            {
                groups.insert(grp.to_string());
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
        self.removed.extend(self.entries.drain().map(|(k, _)| k));
        self.modified.clear();
        self.dirty = true;
    }

    /// Writes unsaved changes to disk atomically.
    ///
    /// Like `QSettings::sync`, the file is re-read first and this instance's pending changes
    /// are merged into it: keys set here win, keys removed here are removed, and keys that
    /// other instances or processes wrote since this instance last read the file are kept.
    /// This instance then adopts the merged contents.
    ///
    /// A file that could not be read completely, such as one with a line that is not valid
    /// UTF-8, is never overwritten: `sync` returns an [`InvalidData`](std::io::ErrorKind::InvalidData)
    /// error and leaves the file alone. This is deliberate, the data-safe choice: writing back
    /// only the lines that were parsed would silently drop the others. The test
    /// `test_qsettings_refuses_to_overwrite_after_load_error` pins it.
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

        // Serializes the read-merge-replace cycle among instances of this process so that two
        // of them cannot each merge into the same stale snapshot.
        let _guard = SYNC_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Serializes it with other processes through `<file>.lock`, as `QSettings` does
        // (qsettings.cpp:1349-1368). Released when `sync` returns.
        let _file_lock = lock_settings_file(path)?;

        let mut merged = HashMap::new();
        read_ini_file(path, &mut merged)?;
        for key in &self.removed {
            merged.remove(key);
        }
        for key in &self.modified {
            if let Some(value) = self.entries.get(key) {
                merged.insert(key.clone(), value.clone());
            }
        }

        // Write to a uniquely named temporary file first for atomic commit.
        let (tmp_path, mut file) = loop {
            let sequence = SYNC_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let tmp_path = path.with_extension(format!("tmp_{}_{}", std::process::id(), sequence));
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp_path)
            {
                Ok(file) => break (tmp_path, file),
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(err) => return Err(err),
            }
        };
        let written = Self::write_ini_entries(&merged, &mut file)
            .and_then(|()| file.flush())
            .and_then(|()| file.sync_all());
        drop(file);
        let committed = written.and_then(|()| fs::rename(&tmp_path, path));
        if let Err(err) = committed {
            let _ = fs::remove_file(&tmp_path);
            return Err(err);
        }
        #[cfg(unix)]
        if let Some(parent) = path.parent() {
            // The rename already committed the data; directory sync failure is not an error.
            if let Ok(dir) = File::open(parent) {
                let _ = dir.sync_all();
            }
        }
        self.entries = merged;
        self.modified.clear();
        self.removed.clear();
        self.dirty = false;
        Ok(())
    }

    /// Serializes entries into INI format grouped by section.
    fn write_ini_entries<W: Write>(
        entries: &HashMap<String, Variant>,
        w: &mut W,
    ) -> std::io::Result<()> {
        // Partition entries into root section and named sections
        let mut root_entries = BTreeMap::new();
        let mut sections: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();

        for (k, v) in entries {
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
            writeln!(w, "{}={}", escape_name(&k), v)?;
        }

        // Write each section
        for (section, items) in sections {
            writeln!(w)?;
            writeln!(w, "[{}]", escape_name(&section))?;
            for (k, v) in items {
                writeln!(w, "{}={}", escape_name(&k), v)?;
            }
        }

        Ok(())
    }

    /// Reads INI file from disk if present.
    fn load_from_disk(&mut self) -> std::io::Result<()> {
        let Some(path) = &self.file_path else {
            return Ok(());
        };
        read_ini_file(path, &mut self.entries)?;
        self.dirty = false;
        Ok(())
    }
}

/// Takes the lock file `<path>.lock` that keeps other processes from rewriting the settings
/// file at the same time.
fn lock_settings_file(path: &Path) -> std::io::Result<LockFile> {
    let mut lock_path = path.as_os_str().to_owned();
    lock_path.push(".lock");
    let mut lock = LockFile::new(lock_path);
    match lock.lock() {
        Ok(()) => Ok(lock),
        Err(LockError::PermissionError) => Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "settings lock file could not be created",
        )),
        Err(_) => Err(std::io::Error::other(
            "settings lock file could not be acquired",
        )),
    }
}

/// Parses the INI file at `path` into `entries`; a missing file is not an error. Entries parsed
/// before an I/O error stay in `entries`.
fn read_ini_file(path: &Path, entries: &mut HashMap<String, Variant>) -> std::io::Result<()> {
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
            current_section = Some(if escaped {
                unescape_text(name)
            } else {
                name.to_string()
            });
            continue;
        }

        let pair = if escaped {
            split_unescaped_eq(trimmed)
        } else {
            trimmed.split_once('=')
        };
        if let Some((key, val)) = pair {
            let key = if escaped {
                unescape_text(key.trim())
            } else {
                key.trim().to_string()
            };
            let key = key.as_str();
            let val = val.trim();
            let full_key = match &current_section {
                Some(s) if !s.is_empty() => format!("{}/{}", s, key),
                _ => key.to_string(),
            };
            let parsed = parse_variant_value(val, escaped);
            entries.insert(full_key, parsed);
        }
    }

    Ok(())
}

/// Writes the diagnostic for a `sync` that failed while its [`Settings`] was dropped. `Drop`
/// cannot return the error, so the unsaved changes would otherwise be lost silently.
fn report_failed_sync_on_drop(out: &mut impl Write, path: Option<&Path>, error: &std::io::Error) {
    let path = path.map_or_else(String::new, |p| format!(" `{}`", p.display()));
    // A closed stderr must not turn a failed save into a panic inside `drop`.
    let _ = writeln!(
        out,
        "ferrokit: Settings{path} was dropped with unsaved changes and could not write them: \
         {error}; call `sync` before dropping to handle this error"
    );
}

impl Drop for Settings {
    /// Writes unsaved changes like [`Settings::sync`]. A failure cannot be returned from here,
    /// so it is reported once on stderr and the changes are lost; call `sync` explicitly to
    /// handle the error.
    fn drop(&mut self) {
        if self.dirty
            && let Err(error) = self.sync()
        {
            report_failed_sync_on_drop(&mut std::io::stderr(), self.file_path.as_deref(), &error);
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

/// Like `escape_text`, but also escapes leading/trailing whitespace (which the loader
/// trims) so keys and section names keep it across a save/load round trip.
fn escape_name(s: &str) -> String {
    let start = s.len() - s.trim_start().len();
    let end = s.trim_end().len();
    let mut out = String::with_capacity(s.len());
    for (i, c) in s.char_indices() {
        if c.is_whitespace() && (i < start || i >= end) && c != '\n' && c != '\r' {
            match c {
                ' ' => out.push_str("\\s"),
                '\t' => out.push_str("\\t"),
                other => out.push_str(&format!("\\u{{{:x}}}", other as u32)),
            }
        } else {
            out.push_str(&escape_text(c.encode_utf8(&mut [0; 4])));
        }
    }
    out
}

/// Reverses `escape_name`/`escape_text`; unknown escape sequences are kept literally so older
/// files containing backslashes still load.
fn unescape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        let Some(c) = s[i..].chars().next() else {
            break;
        };
        if c != '\\' {
            out.push(c);
            i += c.len_utf8();
            continue;
        }
        // Every escape is a backslash plus ASCII, so the byte after it is a char boundary
        // whenever it is one of the recognised escape letters.
        let rest = &s[i + 1..];
        match rest.as_bytes().first().copied() {
            Some(b'\\') => out.push('\\'),
            Some(b'n') => out.push('\n'),
            Some(b'r') => out.push('\r'),
            Some(b's') => out.push(' '),
            Some(b't') => out.push('\t'),
            Some(b'u') => match parse_unicode_escape(&rest[1..]) {
                Some((ch, consumed)) => {
                    out.push(ch);
                    // Backslash, `u` and the `{hex}` part.
                    i += 2 + consumed;
                    continue;
                }
                None => {
                    out.push('\\');
                    i += 1;
                    continue;
                }
            },
            Some(e @ (b'=' | b';' | b'#' | b'[' | b']')) => out.push(e as char),
            _ => {
                out.push('\\');
                i += 1;
                continue;
            }
        }
        i += 2;
    }
    out
}

/// Parses the `{hex}` part of a `\u{hex}` escape (`s` starts right after the `u`), returning
/// the character and the number of bytes consumed. Looks at no more than 10 bytes, so a long
/// run of malformed escapes is still linear overall.
fn parse_unicode_escape(s: &str) -> Option<(char, usize)> {
    // `char::MAX` needs six hex digits; allow a little leading-zero padding.
    const MAX_DIGITS: usize = 8;
    let bytes = s.as_bytes();
    if bytes.first() != Some(&b'{') {
        return None;
    }
    let digits = bytes[1..]
        .iter()
        .take(MAX_DIGITS + 1)
        .take_while(|b| b.is_ascii_hexdigit())
        .count();
    if digits == 0 || digits > MAX_DIGITS || bytes.get(1 + digits) != Some(&b'}') {
        return None;
    }
    let value = u32::from_str_radix(&s[1..1 + digits], 16).ok()?;
    Some((char::from_u32(value)?, digits + 2))
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
    // A value nested beyond the serialization depth cap cannot be stored; write it as invalid.
    let bytes = v
        .to_bytes()
        .unwrap_or_else(|_| Variant::Invalid.to_bytes().unwrap_or_default());
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
    if let Some(encoded) = raw
        .strip_prefix("@Variant(")
        .and_then(|s| s.strip_suffix(')'))
        && encoded.len() % 2 == 0
    {
        let bytes = encoded.as_bytes();
        let decoded = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| {
                let high = (pair[0] as char).to_digit(16)?;
                let low = (pair[1] as char).to_digit(16)?;
                Some(((high << 4) | low) as u8)
            })
            .collect::<Option<Vec<_>>>();
        if let Some(decoded) = decoded
            && let Ok(value) = Variant::from_bytes(&decoded)
        {
            return value;
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
    if raw.starts_with('#')
        && (raw.len() == 9 || raw.len() == 7)
        && let Ok(hex) = u32::from_str_radix(&raw[1..], 16)
    {
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

    // Default to String
    Variant::String(if escaped {
        unescape_text(raw)
    } else {
        raw.to_string()
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The diagnostic names the file and the error, and says how to handle it.
    #[test]
    fn test_failed_sync_on_drop_is_reported_with_the_path_and_the_error() {
        let error = std::io::Error::new(std::io::ErrorKind::InvalidData, "could not be loaded");
        let mut out = Vec::new();

        report_failed_sync_on_drop(&mut out, Some(Path::new("a/b.ini")), &error);

        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("ferrokit: Settings `"), "{text}");
        assert!(text.contains("b.ini"), "{text}");
        assert!(text.contains("could not be loaded"), "{text}");
        assert!(text.contains("call `sync`"), "{text}");
        assert!(text.ends_with('\n'), "one line: {text}");
        assert_eq!(text.lines().count(), 1, "{text}");
    }
}
