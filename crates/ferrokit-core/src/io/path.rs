// SPDX-License-Identifier: LGPL-3.0-only
//! Cross-platform Path manipulation utilities (`QDir` path methods equivalent).
//!
//! Provides normalization, native separator conversion, relative path resolution,
//! and component extraction matching Qt path conventions.

use std::path::{Component, Path};

/// Normalizes a path by removing redundant separators, resolving `.` and `..`,
/// and converting all separators to forward slashes `/` (`QDir::cleanPath` equivalent).
pub fn clean_path(path: impl AsRef<str>) -> String {
    let raw = path.as_ref();
    if raw.is_empty() {
        return String::new();
    }

    #[cfg(windows)]
    let is_network_unc = raw.starts_with(r"\\") || raw.starts_with("//");
    #[cfg(not(windows))]
    let is_network_unc = false;
    let mut drive_prefix = String::new();
    if !is_network_unc {
        let bytes = raw.as_bytes();
        if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
            drive_prefix = raw[..2].to_string();
        }
    }

    let remainder = if is_network_unc || !drive_prefix.is_empty() {
        &raw[2..]
    } else {
        raw
    };

    let is_absolute = remainder.starts_with('/') || remainder.starts_with('\\');

    let mut parts: Vec<&str> = Vec::new();
    for seg in remainder.split(['/', '\\']) {
        if seg.is_empty() || seg == "." {
            continue;
        }
        if seg == ".." {
            if let Some(last) = parts.last() {
                if *last != ".." {
                    parts.pop();
                    continue;
                }
            }
            if !is_absolute {
                parts.push("..");
            }
        } else {
            parts.push(seg);
        }
    }

    let joined = parts.join("/");
    if is_network_unc {
        format!("//{}", joined)
    } else if !drive_prefix.is_empty() {
        if is_absolute {
            format!("{}/{}", drive_prefix, joined)
        } else if joined.is_empty() {
            drive_prefix
        } else {
            format!("{}:{}", &drive_prefix[..1], joined)
        }
    } else if is_absolute {
        format!("/{}", joined)
    } else if joined.is_empty() {
        ".".to_string()
    } else {
        joined
    }
}

/// Converts forward slashes to the native OS directory separator (`QDir::toNativeSeparators` equivalent).
pub fn to_native_separators(path: impl AsRef<str>) -> String {
    #[cfg(windows)]
    {
        path.as_ref().replace('/', "\\")
    }
    #[cfg(not(windows))]
    {
        path.as_ref().replace('\\', "/")
    }
}

/// Converts OS-native directory separators to forward slashes `/` (`QDir::fromNativeSeparators` equivalent).
pub fn from_native_separators(path: impl AsRef<str>) -> String {
    path.as_ref().replace('\\', "/")
}

/// Returns `true` if the path is relative (`QDir::isRelativePath` equivalent).
pub fn is_relative(path: impl AsRef<Path>) -> bool {
    let p = path.as_ref();
    let s = p.to_string_lossy();
    if s.starts_with('/') || s.starts_with('\\') {
        return false;
    }
    // Check drive letters: C:/ or C:\
    let bytes = s.as_bytes();
    if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'/' || bytes[2] == b'\\')
    {
        return false;
    }
    p.is_relative()
}

/// Returns `true` if the path is absolute (`QDir::isAbsolutePath` equivalent).
pub fn is_absolute(path: impl AsRef<Path>) -> bool {
    !is_relative(path)
}

/// Computes the relative path from `base` directory to `target` (`QDir::relativeFilePath` equivalent).
pub fn relative_path(base: impl AsRef<Path>, target: impl AsRef<Path>) -> String {
    let base_clean = clean_path(base.as_ref().to_string_lossy());
    let target_clean = clean_path(target.as_ref().to_string_lossy());

    let base_path = Path::new(&base_clean);
    let target_path = Path::new(&target_clean);

    let mut base_comps = base_path.components().peekable();
    let mut target_comps = target_path.components().peekable();

    // Skip common prefix
    while let (Some(b), Some(t)) = (base_comps.peek(), target_comps.peek()) {
        if b == t {
            base_comps.next();
            target_comps.next();
        } else {
            break;
        }
    }

    let mut result_parts: Vec<&str> = Vec::new();
    for comp in base_comps {
        if let Component::Normal(_) = comp {
            result_parts.push("..");
        }
    }
    for comp in target_comps {
        if let Component::Normal(name) = comp {
            if let Some(s) = name.to_str() {
                result_parts.push(s);
            }
        }
    }

    if result_parts.is_empty() {
        ".".to_string()
    } else {
        result_parts.join("/")
    }
}

/// Returns the file name component of a path (e.g. "image.png" from "/path/to/image.png").
pub fn file_name(path: impl AsRef<Path>) -> String {
    path.as_ref()
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string()
}

/// Returns the base name without the last extension (e.g. "archive.tar" from "archive.tar.gz").
pub fn base_name(path: impl AsRef<Path>) -> String {
    let name = file_name(path);
    name.find('.')
        .map_or(name.as_str(), |idx| &name[..idx])
        .to_string()
}

/// Returns the complete base name without the last extension.
pub fn complete_base_name(path: impl AsRef<Path>) -> String {
    path.as_ref()
        .file_stem()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string()
}

/// Returns the suffix / file extension without the dot (e.g. "gz" from "archive.tar.gz").
pub fn suffix(path: impl AsRef<Path>) -> String {
    let name = file_name(&path);
    if name.starts_with('.') && !name[1..].contains('.') {
        return name[1..].to_string();
    }
    path.as_ref()
        .extension()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string()
}

/// Returns the complete suffix from the first dot (e.g. "tar.gz" from "archive.tar.gz").
pub fn complete_suffix(path: impl AsRef<Path>) -> String {
    let name = file_name(path);
    if let Some(idx) = name.find('.') {
        name[idx + 1..].to_string()
    } else {
        String::new()
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_names_and_suffixes_follow_qt_dot_boundaries() {
        assert_eq!(base_name("archive.tar.gz"), "archive");
        assert_eq!(complete_base_name("archive.tar.gz"), "archive.tar");
        assert_eq!(suffix("archive.tar.gz"), "gz");
        assert_eq!(complete_suffix("archive.tar.gz"), "tar.gz");
    }

    #[cfg(unix)]
    #[test]
    fn clean_path_collapses_repeated_leading_slashes_on_unix() {
        assert_eq!(clean_path("//server///share/../file"), "/server/file");
    }
}
