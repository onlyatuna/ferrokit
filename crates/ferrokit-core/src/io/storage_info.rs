// SPDX-License-Identifier: LGPL-3.0-only
//! QStorageInfo volume and disk space query abstraction (`QStorageInfo` equivalent).
//!
//! Provides disk partition, volume name, file system type, total space, and free space inspection.

use std::path::{Path, PathBuf};

/// Storage volume information (`QStorageInfo` equivalent).
#[derive(Debug, Clone)]
pub struct StorageInfo {
    root_path: PathBuf,
    name: String,
    fs_type: String,
    bytes_total: u64,
    bytes_free: u64,
    bytes_available: u64,
    is_read_only: bool,
    is_ready: bool,
    is_valid: bool,
}

impl Default for StorageInfo {
    fn default() -> Self {
        Self::root()
    }
}

impl StorageInfo {
    /// Creates a new `StorageInfo` referencing the mount point containing `path`.
    pub fn new(path: impl AsRef<Path>) -> Self {
        let mut info = Self {
            root_path: path.as_ref().to_path_buf(),
            name: String::new(),
            fs_type: String::new(),
            bytes_total: 0,
            bytes_free: 0,
            bytes_available: 0,
            is_read_only: false,
            is_ready: false,
            is_valid: false,
        };
        info.refresh();
        info
    }

    /// Queries the root system storage drive.
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

    /// Returns the root mount point path (e.g. `C:\` or `/`).
    pub fn root_path(&self) -> &Path {
        &self.root_path
    }

    /// Returns the human-readable volume name (e.g. "System", "Data").
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the file system type name (e.g. "NTFS", "FAT32", "apfs", "ext4").
    pub fn file_system_type(&self) -> &str {
        &self.fs_type
    }

    /// Returns total storage capacity in bytes.
    pub fn bytes_total(&self) -> u64 {
        self.bytes_total
    }

    /// Returns total free storage space in bytes.
    pub fn bytes_free(&self) -> u64 {
        self.bytes_free
    }

    /// Returns available storage space to the current non-privileged user in bytes.
    pub fn bytes_available(&self) -> u64 {
        self.bytes_available
    }

    /// Returns `true` if the volume is mounted read-only.
    pub fn is_read_only(&self) -> bool {
        self.is_read_only
    }

    /// Returns `true` if the volume is ready for I/O operations.
    pub fn is_ready(&self) -> bool {
        self.is_ready
    }

    /// Returns `true` if the volume query succeeded and metadata is valid.
    pub fn is_valid(&self) -> bool {
        self.is_valid
    }

    /// Refreshes the storage information from the operating system.
    pub fn refresh(&mut self) {
        #[cfg(windows)]
        {
            self.refresh_windows();
        }
        #[cfg(not(windows))]
        {
            self.refresh_unix();
        }
    }

    /// Returns a list of all currently mounted storage volumes.
    pub fn mounted_volumes() -> Vec<StorageInfo> {
        #[cfg(windows)]
        {
            Self::mounted_volumes_windows()
        }
        #[cfg(not(windows))]
        {
            vec![Self::root()]
        }
    }

    #[cfg(windows)]
    fn refresh_windows(&mut self) {
        use windows_sys::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetVolumeInformationW};

        let path_str = self.root_path.to_string_lossy();
        let mut wide: Vec<u16> = path_str.encode_utf16().collect();
        if !wide.ends_with(&[0]) {
            wide.push(0);
        }

        // SAFETY: `wide` is NUL-terminated (a terminator is pushed above if missing) and
        // outlives every call. The out-pointers (`free_avail`, `total`, `free_total`, `flags`)
        // are live locals; `volume_name` and `fs_name` are local `[u16; 260]` buffers and the
        // sizes passed are their exact lengths. The two optional out-parameters (serial number,
        // max component length) are null, which `GetVolumeInformationW` documents as allowed.
        unsafe {
            let mut free_avail: u64 = 0;
            let mut total: u64 = 0;
            let mut free_total: u64 = 0;

            let ok_space =
                GetDiskFreeSpaceExW(wide.as_ptr(), &mut free_avail, &mut total, &mut free_total);

            if ok_space != 0 {
                self.bytes_available = free_avail;
                self.bytes_total = total;
                self.bytes_free = free_total;
                self.is_ready = true;
                self.is_valid = true;
            } else {
                self.is_ready = false;
                self.is_valid = false;
                return;
            }

            let mut volume_name = [0u16; 260];
            let mut fs_name = [0u16; 260];
            let mut flags: u32 = 0;

            let ok_vol = GetVolumeInformationW(
                wide.as_ptr(),
                volume_name.as_mut_ptr(),
                volume_name.len() as u32,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut flags,
                fs_name.as_mut_ptr(),
                fs_name.len() as u32,
            );

            if ok_vol != 0 {
                const FILE_READ_ONLY_VOLUME: u32 = 0x00080000;
                self.is_read_only = (flags & FILE_READ_ONLY_VOLUME) != 0;

                let vol_len = volume_name.iter().position(|&c| c == 0).unwrap_or(0);
                self.name = String::from_utf16_lossy(&volume_name[..vol_len]);

                let fs_len = fs_name.iter().position(|&c| c == 0).unwrap_or(0);
                self.fs_type = String::from_utf16_lossy(&fs_name[..fs_len]);
            }
        }
    }

    #[cfg(windows)]
    fn mounted_volumes_windows() -> Vec<StorageInfo> {
        use windows_sys::Win32::Storage::FileSystem::GetLogicalDriveStringsW;

        let mut buffer = [0u16; 512];
        // SAFETY: `buffer` is a live local array and `buffer.len() as u32` is exactly its
        // length in `u16` units, which is the capacity `GetLogicalDriveStringsW` is told it
        // may write; the call returns the needed size instead of overrunning when too small
        // (handled by the `len` check below).
        let len = unsafe { GetLogicalDriveStringsW(buffer.len() as u32, buffer.as_mut_ptr()) };
        if len == 0 || len > buffer.len() as u32 {
            return vec![Self::root()];
        }

        let mut drives = Vec::new();
        let mut start = 0;
        for i in 0..len as usize {
            if buffer[i] == 0 {
                if start < i {
                    let drive_str = String::from_utf16_lossy(&buffer[start..i]);
                    let storage = StorageInfo::new(drive_str);
                    if storage.is_valid() {
                        drives.push(storage);
                    }
                }
                start = i + 1;
            }
        }

        if drives.is_empty() {
            vec![Self::root()]
        } else {
            drives
        }
    }

    /// `QStorageInfoPrivate::retrieveVolumeInfo` (qstorageinfo_unix.cpp:403-418): one
    /// statvfs; valid and ready only when it succeeds; sizes are block counts times the
    /// fragment size.
    #[cfg(not(windows))]
    // statvfs field widths differ: fsblkcnt_t is u32 on macOS, u64 on Linux.
    #[allow(clippy::useless_conversion)]
    fn refresh_unix(&mut self) {
        use std::os::unix::ffi::OsStrExt;
        let Ok(path) = std::ffi::CString::new(self.root_path.as_os_str().as_bytes()) else {
            self.is_ready = false;
            self.is_valid = false;
            return;
        };
        // SAFETY: `libc::statvfs` is a plain C struct of integer fields (no references or
        // niches), for which all-zero bytes are a valid value.
        let mut buf: libc::statvfs = unsafe { std::mem::zeroed() };
        // SAFETY: `path` is a `CString`, so NUL-terminated and alive until after the call,
        // and `&mut buf` points to the initialised local `statvfs` the call fills in.
        let ok = unsafe { libc::statvfs(path.as_ptr(), &mut buf) } == 0;
        self.is_ready = ok;
        self.is_valid = ok;
        if ok {
            let fragment = u64::from(buf.f_frsize);
            self.bytes_total = u64::from(buf.f_blocks) * fragment;
            self.bytes_free = u64::from(buf.f_bfree) * fragment;
            self.bytes_available = u64::from(buf.f_bavail) * fragment;
        }
        self.fs_type = "posix".to_string();
    }
}
