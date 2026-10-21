// SPDX-License-Identifier: LGPL-3.0-only
//! QFile and QSaveFile abstractions (`QFile` and `QSaveFile` equivalents).
//!
//! Provides file-based random-access `IODevice` backed by native file handles,
//! static file system helpers, and atomic file saving.

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static SAVE_FILE_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

use super::device::{IODevice, OpenMode};
use crate::types::ByteArray;

/// File I/O device backed by native OS file handles (`QFile` equivalent).
#[derive(Debug)]
pub struct File {
    path: PathBuf,
    handle: Option<fs::File>,
    open_mode: OpenMode,
    pos: u64,
}

impl File {
    /// Creates a new File reference with the given path. Does not open the file yet.
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            handle: None,
            open_mode: OpenMode::NOT_OPEN,
            pos: 0,
        }
    }

    /// Returns the file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Sets a new path for this file object (only permitted when file is closed).
    pub fn set_path(&mut self, path: impl AsRef<Path>) {
        if self.is_open() {
            self.close();
        }
        self.path = path.as_ref().to_path_buf();
    }

    /// Opens the file using the specified `OpenMode` flags.
    pub fn open(&mut self, mode: OpenMode) -> io::Result<()> {
        if self.is_open() {
            self.close();
        }
        if mode.is_not_open() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot open file with NotOpen mode",
            ));
        }

        let mut opts = OpenOptions::new();
        if mode.is_readable() && mode.is_writable() {
            opts.read(true).write(true);
        } else if mode.is_readable() {
            opts.read(true);
        } else if mode.is_writable() {
            opts.write(true);
        }

        if mode.is_append() {
            opts.append(true);
        }

        if mode.is_truncate() {
            opts.truncate(true);
        }

        if mode.is_writable() {
            if mode.contains(OpenMode::NEW_ONLY) {
                opts.create_new(true);
            } else if !mode.contains(OpenMode::EXISTING_ONLY) {
                opts.create(true);
            }
        }

        let mut file = opts.open(&self.path)?;
        let pos = if mode.is_append() {
            file.seek(SeekFrom::End(0))?
        } else {
            0
        };

        self.handle = Some(file);
        self.open_mode = mode;
        self.pos = pos;
        Ok(())
    }

    /// Convenient shortcut: opens file in ReadOnly mode.
    pub fn open_read(&mut self) -> io::Result<()> {
        self.open(OpenMode::READ_ONLY)
    }

    /// Convenient shortcut: opens file in WriteOnly | Truncate mode.
    pub fn open_write(&mut self) -> io::Result<()> {
        self.open(OpenMode::WRITE_ONLY | OpenMode::TRUNCATE)
    }

    /// Convenient shortcut: opens file in ReadWrite mode.
    pub fn open_read_write(&mut self) -> io::Result<()> {
        self.open(OpenMode::READ_WRITE)
    }

    /// Reads the entire file content as a `String` (UTF-8).
    pub fn read_all_text(&mut self) -> io::Result<String> {
        let bytes = self.read_all()?;
        String::from_utf8(bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    /// Reads the entire file content as a `ByteArray`.
    pub fn read_all_bytes(&mut self) -> io::Result<ByteArray> {
        let bytes = self.read_all()?;
        Ok(ByteArray::from(bytes))
    }

    /// Writes a UTF-8 text string to the file.
    pub fn write_text(&mut self, text: &str) -> io::Result<()> {
        self.write_all(text.as_bytes())
    }

    // =========================================================================
    // Static Utility Functions (matching Qt static methods)
    // =========================================================================

    /// Checks if a file exists at the given path.
    pub fn exists(path: impl AsRef<Path>) -> bool {
        path.as_ref().is_file()
    }

    /// Removes the file specified by the given path.
    pub fn remove(path: impl AsRef<Path>) -> io::Result<()> {
        fs::remove_file(path)
    }

    /// Renames/moves a file from old path to new path.
    pub fn rename(old_path: impl AsRef<Path>, new_path: impl AsRef<Path>) -> io::Result<()> {
        fs::rename(old_path, new_path)
    }

    /// Copies a file from source to destination.
    pub fn copy(src: impl AsRef<Path>, dst: impl AsRef<Path>) -> io::Result<u64> {
        fs::copy(src, dst)
    }

    /// Quick helper: reads all bytes from a file.
    pub fn read_bytes(path: impl AsRef<Path>) -> io::Result<Vec<u8>> {
        fs::read(path)
    }

    /// Quick helper: reads all text from a file.
    pub fn read_text(path: impl AsRef<Path>) -> io::Result<String> {
        fs::read_to_string(path)
    }

    /// Quick helper: writes all bytes to a file.
    pub fn write_bytes(path: impl AsRef<Path>, bytes: &[u8]) -> io::Result<()> {
        fs::write(path, bytes)
    }
    /// Quick helper: writes all text to a file.
    pub fn write_text_file(path: impl AsRef<Path>, text: &str) -> io::Result<()> {
        fs::write(path, text.as_bytes())
    }
}

impl IODevice for File {
    fn open_mode(&self) -> OpenMode {
        self.open_mode
    }

    fn pos(&self) -> u64 {
        self.pos
    }

    fn size(&self) -> u64 {
        if let Some(ref handle) = self.handle {
            handle.metadata().map(|m| m.len()).unwrap_or(0)
        } else {
            fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0)
        }
    }

    fn seek(&mut self, pos: u64) -> io::Result<()> {
        if let Some(ref mut handle) = self.handle {
            let new_pos = handle.seek(SeekFrom::Start(pos))?;
            self.pos = new_pos;
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "file is not open",
            ))
        }
    }

    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if !self.is_readable() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "file is not open for reading",
            ));
        }
        if let Some(ref mut handle) = self.handle {
            let bytes_read = handle.read(buf)?;
            self.pos += bytes_read as u64;
            Ok(bytes_read)
        } else {
            Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "file is not open",
            ))
        }
    }

    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if !self.is_writable() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "file is not open for writing",
            ));
        }
        if let Some(ref mut handle) = self.handle {
            let bytes_written = handle.write(data)?;
            self.pos += bytes_written as u64;
            Ok(bytes_written)
        } else {
            Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "file is not open",
            ))
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Some(ref mut handle) = self.handle {
            handle.flush()
        } else {
            Ok(())
        }
    }

    fn close(&mut self) {
        if let Some(mut handle) = self.handle.take() {
            let _ = handle.flush();
        }
        self.open_mode = OpenMode::NOT_OPEN;
        self.pos = 0;
    }
}

impl Drop for File {
    fn drop(&mut self) {
        self.close();
    }
}

// =============================================================================
// Atomic Safe File (QSaveFile equivalent)
// =============================================================================

/// Atomic safe file writing abstraction (`QSaveFile` equivalent).
///
/// Writes to a temporary staging file alongside the target destination and
/// atomically commits upon `commit()`. If dropped without committing or if
/// `cancel_writing()` is invoked, the temporary file is deleted without modifying
/// the target file.
#[derive(Debug)]
pub struct SaveFile {
    final_path: PathBuf,
    temp_path: Option<PathBuf>,
    temp_file: Option<File>,
    committed: bool,
}

impl SaveFile {
    /// Creates a new `SaveFile` targeting the specified path.
    pub fn new(final_path: impl AsRef<Path>) -> Self {
        Self {
            final_path: final_path.as_ref().to_path_buf(),
            temp_path: None,
            temp_file: None,
            committed: false,
        }
    }

    /// Opens the staging file for writing.
    pub fn open(&mut self, mode: OpenMode) -> io::Result<()> {
        if !mode.is_writable() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "SaveFile must be opened for writing",
            ));
        }
        if self.temp_file.is_some() || self.temp_path.is_some() {
            self.cancel_writing();
        }

        let file_name = self
            .final_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("qsavefile");
        let parent = self.final_path.parent().unwrap_or_else(|| Path::new("."));
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }

        let (temp_path, handle) = loop {
            let sequence = SAVE_FILE_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let temp_name = format!(".{}_{}_{}.tmp", file_name, std::process::id(), sequence);
            let temp_path = parent.join(temp_name);
            match opts.open(&temp_path) {
                Ok(handle) => break (temp_path, handle),
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(err) => return Err(err),
            }
        };

        #[cfg(unix)]
        if let Ok(metadata) = fs::metadata(&self.final_path) {
            use std::os::unix::fs::PermissionsExt;
            let permissions = fs::Permissions::from_mode(metadata.permissions().mode() & 0o7777);
            if let Err(err) = fs::set_permissions(&temp_path, permissions) {
                let _ = fs::remove_file(&temp_path);
                return Err(err);
            }
        }

        let mut file = File::new(&temp_path);
        file.handle = Some(handle);
        file.open_mode = mode;
        file.pos = 0;
        self.temp_path = Some(temp_path);
        self.temp_file = Some(file);
        self.committed = false;
        Ok(())
    }

    /// Atomically replaces the target file with the written content.
    pub fn commit(&mut self) -> io::Result<()> {
        if self.committed {
            return Ok(());
        }
        let temp_path = self.temp_path.as_ref().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotConnected, "SaveFile is not open")
        })?;
        if let Some(mut file) = self.temp_file.take() {
            file.flush()?;
            if let Some(handle) = file.handle.as_ref() {
                handle.sync_all()?;
            }
            file.close();
        }
        fs::rename(temp_path, &self.final_path)?;
        self.committed = true;
        self.temp_path = None;
        #[cfg(unix)]
        if let Some(parent) = self.final_path.parent() {
            let parent = if parent.as_os_str().is_empty() {
                Path::new(".")
            } else {
                parent
            };
            fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    }

    /// Cancels writing and discards the temporary file.
    pub fn cancel_writing(&mut self) {
        if let Some(mut file) = self.temp_file.take() {
            file.close();
        }
        if let Some(temp_path) = self.temp_path.take() {
            let _ = fs::remove_file(temp_path);
        }
        self.committed = true;
    }
}

impl IODevice for SaveFile {
    fn open_mode(&self) -> OpenMode {
        self.temp_file
            .as_ref()
            .map(|f| f.open_mode())
            .unwrap_or(OpenMode::NOT_OPEN)
    }

    fn pos(&self) -> u64 {
        self.temp_file.as_ref().map(|f| f.pos()).unwrap_or(0)
    }

    fn size(&self) -> u64 {
        self.temp_file.as_ref().map(|f| f.size()).unwrap_or(0)
    }

    fn seek(&mut self, pos: u64) -> io::Result<()> {
        if let Some(ref mut file) = self.temp_file {
            file.seek(pos)
        } else {
            Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "SaveFile is not open",
            ))
        }
    }

    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if let Some(ref mut file) = self.temp_file {
            file.read(buf)
        } else {
            Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "SaveFile is not open",
            ))
        }
    }

    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if let Some(ref mut file) = self.temp_file {
            file.write(data)
        } else {
            Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "SaveFile is not open",
            ))
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Some(ref mut file) = self.temp_file {
            file.flush()
        } else {
            Ok(())
        }
    }

    fn close(&mut self) {
        self.cancel_writing();
    }
}

impl Drop for SaveFile {
    fn drop(&mut self) {
        if !self.committed {
            self.cancel_writing();
        }
    }
}
#[cfg(test)]
mod save_file_tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_dir() -> PathBuf {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir()
            .join(format!("ferrokit-savefile-{}-{id}", std::process::id()));
        fs::create_dir(&path).unwrap();
        path
    }

    #[test]
    fn save_file_open_is_unique_and_reopen_removes_previous_temp() {
        let dir = test_dir();
        let target = dir.join("target");
        let mut first = SaveFile::new(&target);
        first.open(OpenMode::WRITE_ONLY).unwrap();
        let first_temp = first.temp_path.clone().unwrap();

        let mut second = SaveFile::new(&target);
        second.open(OpenMode::WRITE_ONLY).unwrap();
        assert_ne!(first.temp_path, second.temp_path);

        first.open(OpenMode::WRITE_ONLY).unwrap();
        assert!(!first_temp.exists());
        assert!(first.temp_path.as_ref().unwrap().exists());
        first.cancel_writing();
        second.cancel_writing();
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn save_file_preserves_target_permissions_and_commits() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let dir = test_dir();
        let target = dir.join("target");
        fs::write(&target, b"old").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).unwrap();
        let mut save = SaveFile::new(&target);
        save.open(OpenMode::WRITE_ONLY).unwrap();
        save.write(b"new").unwrap();
        save.commit().unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"new");
        assert_eq!(fs::metadata(&target).unwrap().mode() & 0o7777, 0o640);
        fs::remove_dir_all(dir).unwrap();
    }
}
