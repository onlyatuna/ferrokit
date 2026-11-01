// SPDX-License-Identifier: LGPL-3.0-only
//! QLockFile advisory cross-process file lock abstraction (`QLockFile` equivalent).
//!
//! Provides advisory file locking across processes with process ID registration,
//! stale lock detection, and timeout-based acquisition.

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

/// Error states for lock file operations (`QLockFile::LockError` equivalent).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockError {
    NoError,
    LockFailedError,
    PermissionError,
    UnknownError,
}

/// Advisory cross-process file lock (`QLockFile` equivalent).
#[derive(Debug)]
pub struct LockFile {
    file_path: PathBuf,
    is_locked: bool,
    stale_lock_time: Duration,
}

impl LockFile {
    /// Creates a new `LockFile` targeting the given path (e.g. `app.lock`).
    pub fn new(file_path: impl AsRef<Path>) -> Self {
        Self {
            file_path: file_path.as_ref().to_path_buf(),
            is_locked: false,
            stale_lock_time: Duration::from_secs(30),
        }
    }

    /// Returns the file path of the lock file.
    pub fn file_path(&self) -> &Path {
        &self.file_path
    }

    /// Returns `true` if this instance currently holds the lock.
    pub fn is_locked(&self) -> bool {
        self.is_locked
    }

    /// Sets how old a lock file may get before it counts as stale (default: 30s).
    pub fn set_stale_lock_time(&mut self, time: Duration) {
        self.stale_lock_time = time;
    }

    /// Returns the current stale lock timeout.
    pub fn stale_lock_time(&self) -> Duration {
        self.stale_lock_time
    }

    /// Attempts to acquire the lock immediately without waiting.
    pub fn try_lock_now(&mut self) -> Result<bool, LockError> {
        self.try_lock(Duration::from_millis(0))
    }

    /// Attempts to acquire the lock, waiting up to `timeout`.
    pub fn try_lock(&mut self, timeout: Duration) -> Result<bool, LockError> {
        if self.is_locked {
            return Ok(true);
        }

        let start = Instant::now();
        let pid = std::process::id();
        let app_name = std::env::current_exe()
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .unwrap_or_else(|| "ferrokit_app".to_string());

        let lock_content = format!("PID={}\nAPP={}\nTIME={}\n", pid, app_name, now_epoch_secs());

        loop {
            // Creating the lock file and reclaiming a stale one both happen under the guard
            // file, so a reclaimer that has judged a lock stale cannot remove the fresh lock of
            // a waiter that created it in between.
            match self.take_guard() {
                Ok(Some(guard)) => {
                    let attempt = self.create_lock_file(&lock_content);
                    drop(guard);
                    if attempt? {
                        self.is_locked = true;
                        return Ok(true);
                    }
                }
                // Another process holds the guard, for a moment.
                Ok(None) => {}
                Err(error) => return Err(error),
            }

            if start.elapsed() >= timeout {
                return Err(LockError::LockFailedError);
            }
            thread::sleep(Duration::from_millis(25));
        }
    }

    /// Creates the lock file, first removing one that is stale. Returns `false` while a live
    /// lock is in the way. Must be called with the guard held.
    fn create_lock_file(&self, content: &str) -> Result<bool, LockError> {
        loop {
            // Attempt atomic creation with create_new
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&self.file_path)
            {
                Ok(mut file) => {
                    let _ = file.write_all(content.as_bytes());
                    let _ = file.flush();
                    return Ok(true);
                }
                Err(ref e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    if self.is_stale() && fs::remove_file(&self.file_path).is_ok() {
                        continue;
                    }
                    return Ok(false);
                }
                Err(ref e) if e.kind() == io::ErrorKind::PermissionDenied => {
                    return Err(LockError::PermissionError);
                }
                Err(_) => return Err(LockError::UnknownError),
            }
        }
    }

    /// Acquires the lock, blocking indefinitely.
    pub fn lock(&mut self) -> Result<(), LockError> {
        self.try_lock(Duration::from_secs(u64::MAX))?;
        Ok(())
    }

    /// Releases the lock and removes the lock file.
    pub fn unlock(&mut self) {
        if self.is_locked {
            let _ = fs::remove_file(&self.file_path);
            self.is_locked = false;
        }
    }

    /// Reads the PID and application name stored in the existing lock file, if any.
    pub fn get_lock_info(&self) -> Option<(u32, String)> {
        let mut content = String::new();
        let mut file = fs::File::open(&self.file_path).ok()?;
        file.read_to_string(&mut content).ok()?;

        let mut pid = None;
        let mut app = None;

        for line in content.lines() {
            if let Some(rest) = line.strip_prefix("PID=") {
                pid = rest.parse::<u32>().ok();
            } else if let Some(rest) = line.strip_prefix("APP=") {
                app = Some(rest.to_string());
            }
        }

        match (pid, app) {
            (Some(p), Some(a)) => Some((p, a)),
            (Some(p), None) => Some((p, String::new())),
            _ => None,
        }
    }

    /// Checks whether the existing lock file is stale (process dead or exceeded timeout).
    pub fn is_stale(&self) -> bool {
        if let Ok(meta) = fs::metadata(&self.file_path)
            && let Ok(modified) = meta.modified()
            && let Ok(elapsed) = SystemTime::now().duration_since(modified)
            && elapsed > self.stale_lock_time
        {
            return true;
        }

        // Check if owner process is dead
        if let Some((pid, _)) = self.get_lock_info()
            && !is_process_alive(pid)
        {
            return true;
        }

        false
    }

    /// Takes the guard file `<lock>.reclaim`, which serializes every change of the lock file.
    /// Returns `None` while another process holds it. A guard left behind by a crashed holder is
    /// removed once its owner is gone, and taken on a later attempt.
    fn take_guard(&self) -> Result<Option<GuardFile>, LockError> {
        let mut guard_name = self.file_path.as_os_str().to_owned();
        guard_name.push(".reclaim");
        let guard = PathBuf::from(guard_name);
        match OpenOptions::new().write(true).create_new(true).open(&guard) {
            Ok(mut guard_file) => {
                let _ = write!(guard_file, "PID={}\nAPP=reclaim\n", std::process::id());
                drop(guard_file);
                Ok(Some(GuardFile(guard)))
            }
            Err(ref e) if e.kind() == io::ErrorKind::AlreadyExists => {
                // A crashed holder may have left the guard behind; drop it once its
                // owner process is gone (same PID rule as the lock itself).
                let owner_dead = match LockFile::new(&guard).get_lock_info() {
                    Some((pid, _)) => !is_process_alive(pid),
                    // Crashed before writing its PID: stale once older than stale_lock_time.
                    None => fs::metadata(&guard)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.elapsed().ok())
                        .is_some_and(|age| age >= self.stale_lock_time),
                };
                if owner_dead {
                    let _ = fs::remove_file(&guard);
                }
                Ok(None)
            }
            Err(ref e) if e.kind() == io::ErrorKind::PermissionDenied => {
                Err(LockError::PermissionError)
            }
            Err(_) => Err(LockError::UnknownError),
        }
    }

    /// Removes a stale lock file from disk.
    pub fn remove_stale_lock_file(&self) -> bool {
        fs::remove_file(&self.file_path).is_ok()
    }
}

/// The guard file of a lock file; removed when dropped.
struct GuardFile(PathBuf);

impl Drop for GuardFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

impl Drop for LockFile {
    fn drop(&mut self) {
        self.unlock();
    }
}

fn now_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn is_process_alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        const STILL_ACTIVE: u32 = 259;

        // SAFETY: `OpenProcess` takes integers only. Its returned handle is checked for null
        // (failure) before use, so `GetExitCodeProcess` receives an open process handle with
        // the query right requested here and a pointer to the live local `exit_code`, and
        // `CloseHandle` closes that handle exactly once on the only path that reaches it.
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return false;
            }
            let mut exit_code: u32 = 0;
            let ok = GetExitCodeProcess(handle, &mut exit_code);
            CloseHandle(handle);
            ok != 0 && exit_code == STILL_ACTIVE
        }
    }
    #[cfg(not(windows))]
    {
        // Qt: QLockFilePrivate::isProcessRunning treats the PID as dead only when
        // kill(pid, 0) fails with ESRCH (qlockfile_unix.cpp:296-299); EPERM means it exists.
        let Ok(pid) = libc::pid_t::try_from(pid) else {
            return false;
        };
        // SAFETY: `kill` takes a plain pid and a signal number and touches no memory of ours;
        // signal 0 sends nothing and only performs the existence and permission check
        // (per the POSIX `kill` documentation), and `pid` was range-checked into `pid_t` above.
        if unsafe { libc::kill(pid, 0) } == 0 {
            return true;
        }
        std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    }
}
