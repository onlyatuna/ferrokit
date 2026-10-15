// SPDX-License-Identifier: LGPL-3.0-only
//! Directory, FileInfo, and DirIterator (`QDir`, `QFileInfo`, `QDirIterator` equivalents).
//!
//! Provides directory traversal, metadata inspection, entry filtering,
//! sorting, and recursive directory tree iteration.

use std::cmp::Ordering;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::path::{base_name, clean_path, complete_base_name, complete_suffix, file_name, suffix};
use crate::types::StringList;

// =============================================================================
// DirFilter & SortFlag
// =============================================================================

/// Directory entry filtering flags (`QDir::Filter` equivalent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct DirFilter(pub u32);

impl DirFilter {
    pub const DIRS: Self = Self(0x0001);
    pub const FILES: Self = Self(0x0002);
    pub const DRIVES: Self = Self(0x0004);
    pub const NO_SYM_LINKS: Self = Self(0x0008);
    pub const ALL_ENTRIES: Self = Self(Self::DIRS.0 | Self::FILES.0 | Self::DRIVES.0);
    pub const READABLE: Self = Self(0x0010);
    pub const WRITABLE: Self = Self(0x0020);
    pub const EXECUTABLE: Self = Self(0x0040);
    pub const MODIFIED: Self = Self(0x0080);
    pub const HIDDEN: Self = Self(0x0100);
    pub const SYSTEM: Self = Self(0x0200);
    pub const CASE_SENSITIVE: Self = Self(0x0800);
    pub const NO_DOT_AND_DOT_DOT: Self = Self(0x4000);
    pub const NO_FILTER: Self = Self(Self::ALL_ENTRIES.0 | Self::HIDDEN.0 | Self::SYSTEM.0);

    #[inline]
    pub const fn contains(&self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }
}

impl std::ops::BitOr for DirFilter {
    type Output = Self;
    #[inline]
    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

/// Directory entry sorting flags (`QDir::SortFlag` equivalent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct SortFlag(pub u32);

impl SortFlag {
    pub const NAME: Self = Self(0x00);
    pub const TIME: Self = Self(0x01);
    pub const SIZE: Self = Self(0x02);
    pub const TYPE: Self = Self(0x80);
    pub const UNSORTED: Self = Self(0x03);
    pub const NO_SORT: Self = Self(Self::UNSORTED.0);
    pub const DIRS_FIRST: Self = Self(0x04);
    pub const REVERSED: Self = Self(0x08);
    pub const IGNORE_CASE: Self = Self(0x10);
    pub const DIRS_LAST: Self = Self(0x20);

    #[inline]
    pub const fn contains(&self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }
}

impl std::ops::BitOr for SortFlag {
    type Output = Self;
    #[inline]
    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

// =============================================================================
// FileInfo (QFileInfo equivalent)
// =============================================================================

/// File and directory metadata inspector (`QFileInfo` equivalent).
#[derive(Debug, Clone)]
pub struct FileInfo {
    path: PathBuf,
}

impl FileInfo {
    /// Creates a new FileInfo referencing the given path.
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    /// Returns the original file path.
    pub fn file_path(&self) -> &Path {
        &self.path
    }

    /// Returns the cleaned path string.
    pub fn clean_file_path(&self) -> String {
        clean_path(self.path.to_string_lossy())
    }

    /// Returns the absolute path.
    pub fn absolute_file_path(&self) -> PathBuf {
        if self.path.is_absolute() {
            self.path.clone()
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(&self.path)
        }
    }

    /// Returns the canonicalized path resolving symlinks and normalizations.
    pub fn canonical_file_path(&self) -> io::Result<PathBuf> {
        fs::canonicalize(&self.path)
    }

    /// Returns the file name (e.g. "image.png").
    pub fn file_name(&self) -> String {
        file_name(&self.path)
    }

    /// Returns the base name without the last extension.
    pub fn base_name(&self) -> String {
        base_name(&self.path)
    }

    /// Returns the complete base name without any extension.
    pub fn complete_base_name(&self) -> String {
        complete_base_name(&self.path)
    }

    /// Returns the extension without leading dot.
    pub fn suffix(&self) -> String {
        suffix(&self.path)
    }

    /// Returns the complete extension.
    pub fn complete_suffix(&self) -> String {
        complete_suffix(&self.path)
    }

    /// Returns the parent directory path.
    pub fn dir_path(&self) -> PathBuf {
        self.path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."))
    }

    /// Returns `true` if the file or directory exists.
    pub fn exists(&self) -> bool {
        self.path.exists()
    }

    /// Returns `true` if the path points to a regular file.
    pub fn is_file(&self) -> bool {
        self.path.is_file()
    }

    /// Returns `true` if the path points to a directory.
    pub fn is_dir(&self) -> bool {
        self.path.is_dir()
    }

    /// Returns `true` if the path points to a symbolic link.
    pub fn is_symlink(&self) -> bool {
        self.path.is_symlink()
    }

    /// Returns `true` if the file or directory is hidden.
    pub fn is_hidden(&self) -> bool {
        let name = self.file_name();
        if name.starts_with('.') && name != "." && name != ".." {
            return true;
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if let Ok(meta) = fs::metadata(&self.path) {
                const FILE_ATTRIBUTE_HIDDEN: u32 = 0x02;
                return (meta.file_attributes() & FILE_ATTRIBUTE_HIDDEN) != 0;
            }
        }
        false
    }

    /// Returns `true` if the file or directory is readable.
    pub fn is_readable(&self) -> bool {
        if self.is_dir() {
            fs::read_dir(&self.path).is_ok()
        } else {
            fs::File::open(&self.path).is_ok()
        }
    }

    /// Returns `true` if the file or directory is writable.
    pub fn is_writable(&self) -> bool {
        if let Ok(meta) = fs::metadata(&self.path) {
            !meta.permissions().readonly()
        } else {
            false
        }
    }

    /// Returns the file size in bytes, or 0 if it does not exist.
    pub fn size(&self) -> u64 {
        fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0)
    }

    /// Returns the file creation time if supported.
    pub fn birth_time(&self) -> io::Result<SystemTime> {
        fs::metadata(&self.path)?.created()
    }

    /// Returns the last modification time.
    pub fn last_modified(&self) -> io::Result<SystemTime> {
        fs::metadata(&self.path)?.modified()
    }

    /// Returns the last access time.
    pub fn last_read(&self) -> io::Result<SystemTime> {
        fs::metadata(&self.path)?.accessed()
    }
}

// =============================================================================
// Directory (QDir equivalent)
// =============================================================================

/// Directory manipulation, navigation, and entry query (`QDir` equivalent).
#[derive(Debug, Clone)]
pub struct Directory {
    path: PathBuf,
}

impl Directory {
    /// Creates a new Directory referencing the specified path.
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    /// Creates a Directory referencing the current working directory.
    pub fn current() -> Self {
        Self::new(std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
    }

    /// Creates a Directory referencing the system temporary directory.
    pub fn temp() -> Self {
        Self::new(std::env::temp_dir())
    }

    /// Creates a Directory referencing the user home directory.
    pub fn home() -> Self {
        let home_path = if let Ok(h) = std::env::var("USERPROFILE") {
            PathBuf::from(h)
        } else if let Ok(h) = std::env::var("HOME") {
            PathBuf::from(h)
        } else {
            PathBuf::from(".")
        };
        Self::new(home_path)
    }

    /// Creates a Directory referencing the system root directory.
    pub fn root() -> Self {
        #[cfg(windows)]
        {
            Self::new(r"C:\")
        }
        #[cfg(not(windows))]
        {
            Self::new("/")
        }
    }

    /// Returns the current directory path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Sets a new directory path.
    pub fn set_path(&mut self, path: impl AsRef<Path>) {
        self.path = path.as_ref().to_path_buf();
    }

    /// Returns the clean path representation with forward slashes.
    pub fn clean_path(&self) -> String {
        clean_path(self.path.to_string_lossy())
    }

    /// Returns the absolute path representation.
    pub fn absolute_path(&self) -> PathBuf {
        if self.path.is_absolute() {
            self.path.clone()
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(&self.path)
        }
    }

    /// Returns the canonical path with symlinks resolved.
    pub fn canonical_path(&self) -> io::Result<PathBuf> {
        fs::canonicalize(&self.path)
    }

    /// Returns the name of the directory (last path component).
    pub fn dir_name(&self) -> String {
        file_name(&self.path)
    }

    /// Returns `true` if the directory exists.
    pub fn exists(&self) -> bool {
        self.path.is_dir()
    }

    /// Returns `true` if this directory is readable.
    pub fn is_readable(&self) -> bool {
        fs::read_dir(&self.path).is_ok()
    }

    /// Changes directory to `dir_name` (relative or absolute).
    pub fn cd(&mut self, dir_name: impl AsRef<Path>) -> bool {
        let candidate = self.path.join(dir_name);
        if candidate.is_dir() {
            self.path = candidate;
            true
        } else {
            false
        }
    }

    /// Navigates one directory level up (to parent).
    pub fn cd_up(&mut self) -> bool {
        if let Some(parent) = self.path.parent() {
            self.path = parent.to_path_buf();
            true
        } else {
            false
        }
    }

    /// Creates a subdirectory inside this directory.
    pub fn mkdir(&self, name: impl AsRef<Path>) -> io::Result<()> {
        fs::create_dir(self.path.join(name))
    }

    /// Creates a full directory path recursively (`mkdir -p`).
    pub fn mkpath(&self, dir_path: impl AsRef<Path>) -> io::Result<()> {
        fs::create_dir_all(self.path.join(dir_path))
    }

    /// Removes an empty subdirectory inside this directory.
    pub fn rmdir(&self, name: impl AsRef<Path>) -> io::Result<()> {
        fs::remove_dir(self.path.join(name))
    }

    /// Removes a directory path recursively (`rm -rf`).
    pub fn rmpath(&self, dir_path: impl AsRef<Path>) -> io::Result<()> {
        fs::remove_dir_all(self.path.join(dir_path))
    }

    /// Removes a file in this directory.
    pub fn remove(&self, file_name: impl AsRef<Path>) -> io::Result<()> {
        fs::remove_file(self.path.join(file_name))
    }

    /// Renames a file or directory inside this directory.
    pub fn rename(&self, old_name: impl AsRef<Path>, new_name: impl AsRef<Path>) -> io::Result<()> {
        fs::rename(self.path.join(old_name), self.path.join(new_name))
    }

    /// Queries the list of entry names matching the given filters and sort order.
    pub fn entry_list(&self, filter: DirFilter, sort: SortFlag) -> io::Result<StringList> {
        let infos = self.entry_info_list(filter, sort)?;
        let names: Vec<String> = infos.into_iter().map(|info| info.file_name()).collect();
        Ok(StringList::from(names))
    }

    /// Queries the list of `FileInfo` metadata items matching filters and sort order.
    pub fn entry_info_list(&self, filter: DirFilter, sort: SortFlag) -> io::Result<Vec<FileInfo>> {
        let read_dir = fs::read_dir(&self.path)?;
        let mut items = Vec::new();

        for entry in read_dir {
            let entry = entry?;
            let path = entry.path();
            let info = FileInfo::new(&path);

            // Filtering
            let is_dir = info.is_dir();
            let is_file = info.is_file();

            if is_dir
                && !filter.contains(DirFilter::DIRS)
                && !filter.contains(DirFilter::ALL_ENTRIES)
            {
                continue;
            }
            if is_file
                && !filter.contains(DirFilter::FILES)
                && !filter.contains(DirFilter::ALL_ENTRIES)
            {
                continue;
            }

            if filter.contains(DirFilter::READABLE) && !info.is_readable() {
                continue;
            }
            if filter.contains(DirFilter::WRITABLE) && !info.is_writable() {
                continue;
            }
            if filter.contains(DirFilter::NO_SYM_LINKS) && info.is_symlink() {
                continue;
            }
            if !filter.contains(DirFilter::HIDDEN) && info.is_hidden() {
                continue;
            }

            items.push(info);
        }

        // Sorting
        if !sort.contains(SortFlag::UNSORTED) {
            items.sort_by(|a, b| {
                if sort.contains(SortFlag::DIRS_FIRST) && a.is_dir() != b.is_dir() {
                    return if a.is_dir() {
                        Ordering::Less
                    } else {
                        Ordering::Greater
                    };
                }
                if sort.contains(SortFlag::DIRS_LAST) && a.is_dir() != b.is_dir() {
                    return if a.is_dir() {
                        Ordering::Greater
                    } else {
                        Ordering::Less
                    };
                }

                let order = if sort.contains(SortFlag::TIME) {
                    let time_a = a.last_modified().unwrap_or(SystemTime::UNIX_EPOCH);
                    let time_b = b.last_modified().unwrap_or(SystemTime::UNIX_EPOCH);
                    time_a.cmp(&time_b)
                } else if sort.contains(SortFlag::SIZE) {
                    a.size().cmp(&b.size())
                } else if sort.contains(SortFlag::TYPE) {
                    a.suffix().cmp(&b.suffix())
                } else {
                    // Sort by name
                    if sort.contains(SortFlag::IGNORE_CASE) {
                        a.file_name()
                            .to_lowercase()
                            .cmp(&b.file_name().to_lowercase())
                    } else {
                        a.file_name().cmp(&b.file_name())
                    }
                };

                if sort.contains(SortFlag::REVERSED) {
                    order.reverse()
                } else {
                    order
                }
            });
        }

        Ok(items)
    }
}

// =============================================================================
// DirIterator (QDirIterator equivalent)
// =============================================================================

/// Directory tree iterator (`QDirIterator` equivalent).
#[derive(Debug)]
pub struct DirIterator {
    pending_entries: Vec<PathBuf>,
    dirs_to_visit: Vec<PathBuf>,
    filter: DirFilter,
    subdirectories: bool,
    current_entry: Option<FileInfo>,
}

impl DirIterator {
    /// Creates a directory iterator on `path`. If `subdirectories` is true, descends recursively.
    pub fn new(path: impl AsRef<Path>, filter: DirFilter, subdirectories: bool) -> Self {
        let dirs_to_visit = vec![path.as_ref().to_path_buf()];
        Self {
            pending_entries: Vec::new(),
            dirs_to_visit,
            filter,
            subdirectories,
            current_entry: None,
        }
    }

    /// Returns metadata for the current entry.
    pub fn file_info(&self) -> Option<&FileInfo> {
        self.current_entry.as_ref()
    }
}

impl Iterator for DirIterator {
    type Item = PathBuf;

    /// Fetches the next file or directory path.
    fn next(&mut self) -> Option<PathBuf> {
        loop {
            if let Some(path) = self.pending_entries.pop() {
                let info = FileInfo::new(&path);
                let is_dir = info.is_dir();
                let is_file = info.is_file();

                if is_dir && self.subdirectories {
                    self.dirs_to_visit.push(path.clone());
                }

                if is_dir
                    && !self.filter.contains(DirFilter::DIRS)
                    && !self.filter.contains(DirFilter::ALL_ENTRIES)
                {
                    continue;
                }
                if is_file
                    && !self.filter.contains(DirFilter::FILES)
                    && !self.filter.contains(DirFilter::ALL_ENTRIES)
                {
                    continue;
                }
                if !self.filter.contains(DirFilter::HIDDEN) && info.is_hidden() {
                    continue;
                }

                self.current_entry = Some(info);
                return Some(path);
            }

            // No pending entries, expand next directory from dirs_to_visit
            if let Some(next_dir) = self.dirs_to_visit.pop() {
                if next_dir.is_dir() {
                    if let Ok(entries) = fs::read_dir(&next_dir) {
                        for entry in entries.flatten() {
                            self.pending_entries.push(entry.path());
                        }
                    }
                }
            } else {
                break;
            }
        }
        self.current_entry = None;
        None
    }
}
