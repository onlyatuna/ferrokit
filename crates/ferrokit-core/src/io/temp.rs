// SPDX-License-Identifier: LGPL-3.0-only
//! QTemporaryFile and QTemporaryDir abstractions (`QTemporaryFile` and `QTemporaryDir` equivalents).
//!
//! Provides automatic cleanup of uniquely named temporary files and directories upon drop.

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use super::device::{IODevice, OpenMode};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(1);

fn generate_temp_name(prefix: &str, suffix: &str) -> String {
    let pid = std::process::id();
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let time = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}_{}_{}_{}{}", prefix, pid, time, counter, suffix)
}

/// Automatically cleaned temporary file (`QTemporaryFile` equivalent).
#[derive(Debug)]
pub struct TemporaryFile {
    path: PathBuf,
    file: Option<fs::File>,
    pos: u64,
    auto_remove: bool,
}

impl TemporaryFile {
    /// Creates a new temporary file in the system temp directory and opens it in ReadWrite mode.
    pub fn new() -> io::Result<Self> {
        Self::with_prefix_and_suffix("ferrokit_temp", ".tmp")
    }

    /// Creates a new temporary file with custom prefix and suffix.
    pub fn with_prefix_and_suffix(prefix: &str, suffix: &str) -> io::Result<Self> {
        let temp_dir = std::env::temp_dir();
        loop {
            let path = temp_dir.join(generate_temp_name(prefix, suffix));
            let mut options = OpenOptions::new();
            options.read(true).write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(&path) {
                Ok(file) => {
                    return Ok(Self {
                        path,
                        file: Some(file),
                        pos: 0,
                        auto_remove: true,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
    }

    /// Returns the temporary file path.
    pub fn file_path(&self) -> &Path {
        &self.path
    }

    /// Returns `true` if this file will be automatically removed on drop.
    pub fn auto_remove(&self) -> bool {
        self.auto_remove
    }

    /// Sets whether the temporary file should be automatically removed on drop.
    pub fn set_auto_remove(&mut self, remove: bool) {
        self.auto_remove = remove;
    }

    /// Retains the temporary file on disk after drop (`keep` equivalent).
    pub fn keep(&mut self) {
        self.auto_remove = false;
    }
}

impl IODevice for TemporaryFile {
    fn open_mode(&self) -> OpenMode {
        if self.file.is_some() {
            OpenMode::READ_WRITE
        } else {
            OpenMode::NOT_OPEN
        }
    }

    fn pos(&self) -> u64 {
        self.pos
    }

    fn size(&self) -> u64 {
        self.file
            .as_ref()
            .and_then(|file| file.metadata().ok())
            .map(|metadata| metadata.len())
            .unwrap_or(0)
    }

    fn seek(&mut self, pos: u64) -> io::Result<()> {
        let file = self.file.as_mut().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotConnected, "temporary file is closed")
        })?;
        self.pos = file.seek(SeekFrom::Start(pos))?;
        Ok(())
    }

    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let count = self.file.as_mut().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotConnected, "temporary file is closed")
        })?.read(buf)?;
        self.pos += count as u64;
        Ok(count)
    }

    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let count = self.file.as_mut().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotConnected, "temporary file is closed")
        })?.write(data)?;
        self.pos += count as u64;
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.as_mut().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotConnected, "temporary file is closed")
        })?.flush()
    }

    fn close(&mut self) {
        self.file = None;
    }
}

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        self.close();
        if self.auto_remove {
            let _ = fs::remove_file(&self.path);
        }
    }
}

// =============================================================================
// TemporaryDir (QTemporaryDir equivalent)
// =============================================================================

/// Automatically cleaned temporary directory (`QTemporaryDir` equivalent).
#[derive(Debug)]
pub struct TemporaryDir {
    path: PathBuf,
    auto_remove: bool,
}

impl Default for TemporaryDir {
    fn default() -> Self {
        Self::new().expect("failed to create temporary dir")
    }
}

impl TemporaryDir {
    /// Creates a new temporary directory in the system temp location.
    pub fn new() -> io::Result<Self> {
        Self::with_prefix("ferrokit_dir")
    }

    /// Creates a new temporary directory with custom prefix.
    pub fn with_prefix(prefix: &str) -> io::Result<Self> {
        let temp_dir = std::env::temp_dir();
        loop {
            let path = temp_dir.join(generate_temp_name(prefix, ""));
            let result = {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::DirBuilderExt;
                    let mut builder = fs::DirBuilder::new();
                    builder.mode(0o700);
                    builder.create(&path)
                }
                #[cfg(not(unix))]
                {
                    fs::create_dir(&path)
                }
            };
            match result {
                Ok(()) => {
                    return Ok(Self {
                        path,
                        auto_remove: true,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
    }
    /// Returns the directory path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns `true` if the directory currently exists.
    pub fn is_valid(&self) -> bool {
        self.path.is_dir()
    }

    /// Returns `true` if the directory will be automatically removed on drop.
    pub fn auto_remove(&self) -> bool {
        self.auto_remove
    }

    /// Sets whether the directory should be automatically removed on drop.
    pub fn set_auto_remove(&mut self, remove: bool) {
        self.auto_remove = remove;
    }

    /// Retains the directory on disk after drop.
    pub fn keep(&mut self) {
        self.auto_remove = false;
    }

    /// Manually removes the temporary directory and all its contents immediately.
    pub fn remove(&mut self) -> io::Result<()> {
        if self.path.exists() {
            fs::remove_dir_all(&self.path)?;
        }
        Ok(())
    }
}

impl Drop for TemporaryDir {
    fn drop(&mut self) {
        if self.auto_remove && self.path.exists() {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}
