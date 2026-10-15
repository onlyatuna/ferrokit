// SPDX-License-Identifier: LGPL-3.0-only
#[cfg(not(windows))]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Commands sent from secondary instances to the primary instance.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SingleInstanceCommand {
    WakeUp = 1,
    Toggle = 2,
    Show = 3,
    Quit = 4,
}

impl SingleInstanceCommand {
    pub fn from_u8(val: u8) -> Option<Self> {
        match val {
            1 => Some(Self::WakeUp),
            2 => Some(Self::Toggle),
            3 => Some(Self::Show),
            4 => Some(Self::Quit),
            _ => None,
        }
    }
}

/// Result of acquiring single instance protection.
pub enum SingleInstanceResult {
    /// First instance acquired the lock. Holds the guard until exit.
    Primary(SingleInstanceGuard),
    /// Another instance is already running. The command was sent to wake it up.
    Secondary { command_sent: bool },
}

type CommandCallback = Box<dyn Fn(SingleInstanceCommand) + Send + Sync>;

/// Guard holding the platform single instance lock and IPC listener.
pub struct SingleInstanceGuard {
    #[cfg(windows)]
    mutex_handle: isize,
    #[cfg(windows)]
    wake_msg_id: u32,
    #[cfg(not(windows))]
    #[allow(dead_code)]
    lock_file: Option<std::fs::File>,
    #[cfg(not(windows))]
    socket_path: Option<std::path::PathBuf>,
    #[cfg(not(windows))]
    lock_path: Option<std::path::PathBuf>,
    #[cfg(not(windows))]
    listener_stop: Arc<AtomicBool>,
    command_callbacks: Arc<Mutex<Vec<CommandCallback>>>,
}

impl SingleInstanceGuard {
    /// Registers a callback to be invoked when a secondary instance sends a command.
    pub fn on_command<F>(&self, callback: F)
    where
        F: Fn(SingleInstanceCommand) + Send + Sync + 'static,
    {
        let mut list = self.command_callbacks.lock().unwrap();
        list.push(Box::new(callback));
    }

    /// Triggers registered command callbacks manually (e.g. from native window message filter).
    pub fn dispatch_command(&self, cmd: SingleInstanceCommand) {
        let callbacks = self.command_callbacks.lock().unwrap();
        for cb in callbacks.iter() {
            cb(cmd);
        }
    }

    #[cfg(windows)]
    pub fn wake_message_id(&self) -> u32 {
        self.wake_msg_id
    }
}

impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            if self.mutex_handle != 0 {
                #[link(name = "kernel32")]
                extern "system" {
                    fn CloseHandle(hObject: isize) -> i32;
                }
                unsafe {
                    CloseHandle(self.mutex_handle);
                }
            }
        }
        #[cfg(not(windows))]
        {
            self.listener_stop.store(true, Ordering::SeqCst);
            if let Some(sock) = &self.socket_path {
                let _ = std::fs::remove_file(sock);
            }
            if let Some(lock) = &self.lock_path {
                let _ = std::fs::remove_file(lock);
            }
        }
    }
}

pub struct SingleInstance;

impl SingleInstance {
    /// Attempts to acquire single instance lock for the given application identifier.
    ///
    /// If an existing instance is active, sends `command` and returns `SingleInstanceResult::Secondary`.
    /// Otherwise, returns `SingleInstanceResult::Primary(guard)`.
    pub fn acquire(app_id: &str, command: SingleInstanceCommand) -> SingleInstanceResult {
        #[cfg(windows)]
        {
            Self::acquire_windows(app_id, command)
        }
        #[cfg(not(windows))]
        {
            Self::acquire_unix(app_id, command)
        }
    }

    #[cfg(windows)]
    fn acquire_windows(app_id: &str, command: SingleInstanceCommand) -> SingleInstanceResult {
        use std::ffi::OsStr;
        use std::os::windows::ffi::OsStrExt;

        #[link(name = "kernel32")]
        extern "system" {
            fn CreateMutexW(
                lpMutexAttributes: *const std::ffi::c_void,
                bInitialOwner: i32,
                lpName: *const u16,
            ) -> isize;
            fn GetLastError() -> u32;
            fn CloseHandle(hObject: isize) -> i32;
        }
        #[link(name = "user32")]
        extern "system" {
            fn RegisterWindowMessageW(lpString: *const u16) -> u32;
            fn PostMessageW(hWnd: isize, Msg: u32, wParam: usize, lParam: isize) -> i32;
        }

        let mutex_name: Vec<u16> = OsStr::new(&format!("Local\\{}Mutex\0", app_id))
            .encode_wide()
            .collect();
        let wake_name: Vec<u16> = OsStr::new(&format!("{}_WakeUp\0", app_id))
            .encode_wide()
            .collect();

        unsafe {
            let handle = CreateMutexW(std::ptr::null(), 0, mutex_name.as_ptr());
            let msg_id = RegisterWindowMessageW(wake_name.as_ptr());

            if GetLastError() == 183 {
                // ERROR_ALREADY_EXISTS (183): another instance is already running
                if handle != 0 {
                    CloseHandle(handle);
                }
                let mut sent = false;
                if msg_id != 0 {
                    const HWND_BROADCAST: isize = 0xFFFF;
                    let ret = PostMessageW(HWND_BROADCAST, msg_id, command as usize, 0);
                    sent = ret != 0;
                }
                return SingleInstanceResult::Secondary { command_sent: sent };
            }

            SingleInstanceResult::Primary(SingleInstanceGuard {
                mutex_handle: handle,
                wake_msg_id: msg_id,
                command_callbacks: Arc::new(Mutex::new(Vec::new())),
            })
        }
    }

    #[cfg(not(windows))]
    fn acquire_unix(app_id: &str, command: SingleInstanceCommand) -> SingleInstanceResult {
        use std::io::{Read, Write};
        use std::os::unix::fs::OpenOptionsExt;
        use std::os::unix::io::AsRawFd;
        use std::os::unix::net::{UnixListener, UnixStream};

        extern "C" {
            fn flock(fd: i32, operation: i32) -> i32;
        }
        const LOCK_EX: i32 = 2;
        const LOCK_NB: i32 = 4;

        let base_dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);

        let sanitized_id = app_id.replace(['/', '\\', ' '], "_");
        let lock_path = base_dir.join(format!("{}.lock", sanitized_id));
        let socket_path = base_dir.join(format!("{}.sock", sanitized_id));

        // 1. Try opening lock file
        let lock_file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .open(&lock_path);

        let file = match lock_file {
            Ok(f) => f,
            Err(_) => {
                // Failed to open lock file; fallback to secondary attempt
                return SingleInstanceResult::Secondary {
                    command_sent: false,
                };
            }
        };

        // 2. Try acquiring non-blocking exclusive flock
        let lock_acquired = unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) } == 0;

        if !lock_acquired {
            // Another instance holds the lock. Send command via UnixStream.
            // Retry up to 5 times with a brief backoff in case the primary's listener
            // socket is still completing bind/listen under heavy test runner load.
            let mut sent = false;
            for _ in 0..5 {
                if let Ok(mut stream) = UnixStream::connect(&socket_path) {
                    let buf = [command as u8];
                    if stream.write_all(&buf).is_ok() {
                        sent = true;
                        break;
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            return SingleInstanceResult::Secondary { command_sent: sent };
        }

        // Lock acquired: we are Primary. Write our PID.
        {
            use std::io::Seek;
            let mut f = &file;
            let _ = f.set_len(0);
            let _ = f.rewind();
            let _ = writeln!(f, "{}", std::process::id());
        }

        // Clean stale socket if it exists
        let _ = std::fs::remove_file(&socket_path);

        // Bind UnixListener
        let listener_stop = Arc::new(AtomicBool::new(false));
        let command_callbacks: Arc<Mutex<Vec<CommandCallback>>> = Arc::new(Mutex::new(Vec::new()));

        if let Ok(listener) = UnixListener::bind(&socket_path) {
            let _ = listener.set_nonblocking(true);
            let stop_flag = Arc::clone(&listener_stop);
            let callbacks = Arc::clone(&command_callbacks);

            // Background polling thread for IPC listener
            std::thread::spawn(move || {
                while !stop_flag.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let mut buf = [0u8; 1];
                            if stream.read_exact(&mut buf).is_ok() {
                                if let Some(cmd) = SingleInstanceCommand::from_u8(buf[0]) {
                                    let cbs = callbacks.lock().unwrap();
                                    for cb in cbs.iter() {
                                        cb(cmd);
                                    }
                                }
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(std::time::Duration::from_millis(100));
                        }
                        Err(_) => break,
                    }
                }
            });
        }

        SingleInstanceResult::Primary(SingleInstanceGuard {
            lock_file: Some(file),
            socket_path: Some(socket_path),
            lock_path: Some(lock_path),
            listener_stop,
            command_callbacks,
        })
    }
}
