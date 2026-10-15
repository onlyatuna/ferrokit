// SPDX-License-Identifier: LGPL-3.0-only
//! QFileSystemWatcher file and directory change monitor (`QFileSystemWatcher` equivalent).
//!
//! Monitors specified files and directories for modifications, creations, deletions,
//! and notifies listeners via `file_changed` and `directory_changed` signals.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::signal::Signal;

/// File system monitor matching Qt `QFileSystemWatcher`.
pub struct FileSystemWatcher {
    watched_files: HashMap<PathBuf, Option<SystemTime>>,
    watched_directories: HashMap<PathBuf, HashSet<PathBuf>>,

    pub file_changed: Signal<PathBuf>,
    pub directory_changed: Signal<PathBuf>,
}

impl Default for FileSystemWatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl FileSystemWatcher {
    /// Creates a new empty file system watcher.
    pub fn new() -> Self {
        Self {
            watched_files: HashMap::new(),
            watched_directories: HashMap::new(),
            file_changed: Signal::new(),
            directory_changed: Signal::new(),
        }
    }

    /// Adds a path to the watcher.
    ///
    /// If the path is a directory, it is added to watched directories.
    /// If it is a file, it is added to watched files.
    /// Returns true if the path exists and was newly added.
    pub fn add_path(&mut self, path: impl AsRef<Path>) -> bool {
        let p = path.as_ref().to_path_buf();
        if !p.exists() {
            return false;
        }

        if p.is_dir() {
            if self.watched_directories.contains_key(&p) {
                return false;
            }
            let entries = Self::snapshot_dir(&p);
            self.watched_directories.insert(p, entries);
            true
        } else {
            if self.watched_files.contains_key(&p) {
                return false;
            }
            let mtime = Self::snapshot_file_mtime(&p);
            self.watched_files.insert(p, mtime);
            true
        }
    }

    /// Adds multiple paths to the watcher. Returns paths that failed to add.
    pub fn add_paths<P: AsRef<Path>>(&mut self, paths: &[P]) -> Vec<PathBuf> {
        let mut failed = Vec::new();
        for p in paths {
            if !self.add_path(p) {
                failed.push(p.as_ref().to_path_buf());
            }
        }
        failed
    }

    /// Removes a path from the watcher. Returns true if it was removed.
    pub fn remove_path(&mut self, path: impl AsRef<Path>) -> bool {
        let p = path.as_ref();
        let removed_f = self.watched_files.remove(p).is_some();
        let removed_d = self.watched_directories.remove(p).is_some();
        removed_f || removed_d
    }

    /// Removes multiple paths from the watcher. Returns paths that were not being watched.
    pub fn remove_paths<P: AsRef<Path>>(&mut self, paths: &[P]) -> Vec<PathBuf> {
        let mut not_found = Vec::new();
        for p in paths {
            if !self.remove_path(p) {
                not_found.push(p.as_ref().to_path_buf());
            }
        }
        not_found
    }

    /// Returns a list of paths to files being watched.
    pub fn files(&self) -> Vec<PathBuf> {
        let mut f: Vec<PathBuf> = self.watched_files.keys().cloned().collect();
        f.sort();
        f
    }

    /// Returns a list of paths to directories being watched.
    pub fn directories(&self) -> Vec<PathBuf> {
        let mut d: Vec<PathBuf> = self.watched_directories.keys().cloned().collect();
        d.sort();
        d
    }

    /// Polls all watched paths and emits signals for modified files or directories.
    pub fn poll_changes(&mut self) {
        // 1. Check watched files
        let mut changed_files = Vec::new();
        for (path, last_mtime) in &mut self.watched_files {
            let current_mtime = Self::snapshot_file_mtime(path);
            if *last_mtime != current_mtime {
                *last_mtime = current_mtime;
                changed_files.push(path.clone());
            }
        }

        for path in changed_files {
            self.file_changed.emit(&path);
        }

        // 2. Check watched directories
        let mut changed_dirs = Vec::new();
        for (dir_path, last_entries) in &mut self.watched_directories {
            let current_entries = Self::snapshot_dir(dir_path);
            if *last_entries != current_entries {
                *last_entries = current_entries;
                changed_dirs.push(dir_path.clone());
            }
        }

        for dir in changed_dirs {
            self.directory_changed.emit(&dir);
        }
    }

    fn snapshot_file_mtime(path: &Path) -> Option<SystemTime> {
        fs::metadata(path).and_then(|m| m.modified()).ok()
    }

    fn snapshot_dir(dir: &Path) -> HashSet<PathBuf> {
        let mut set = HashSet::new();
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                set.insert(entry.path());
            }
        }
        set
    }
}
