// SPDX-License-Identifier: LGPL-3.0-only
//! QProcess cross-platform process execution abstraction (`QProcess` equivalent).
//!
//! Provides external process launching, environment and working directory configuration,
//! asynchronous/synchronous lifecycle management, and stream I/O via stdin/stdout/stderr pipes.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use parking_lot::Mutex;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Current process lifecycle state (`QProcess::ProcessState` equivalent).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessState {
    NotRunning,
    Starting,
    Running,
}

/// Process exit classification (`QProcess::ExitStatus` equivalent).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitStatus {
    NormalExit,
    CrashExit,
}

/// Standard I/O channel forwarding mode (`QProcess::ProcessChannelMode` equivalent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProcessChannelMode {
    #[default]
    SeparateChannels,
    MergedChannels,
    ForwardedChannels,
}

/// External process launcher and controller matching Qt `QProcess`.
#[derive(Debug)]
pub struct Process {
    program: String,
    args: Vec<String>,
    working_dir: Option<PathBuf>,
    env: HashMap<String, String>,
    channel_mode: ProcessChannelMode,

    child: Option<Child>,
    stdin_handle: Option<ChildStdin>,
    stdout_reader: Option<thread::JoinHandle<()>>,
    stderr_reader: Option<thread::JoinHandle<()>>,
    stdout_buf: Arc<Mutex<Vec<u8>>>,
    stderr_buf: Arc<Mutex<Vec<u8>>>,

    state: ProcessState,
    exit_code: Option<i32>,
    exit_status: Option<ExitStatus>,
    pid: Option<u32>,
}

impl Default for Process {
    fn default() -> Self {
        Self::new()
    }
}

impl Process {
    /// Creates a new empty `Process`.
    pub fn new() -> Self {
        Self {
            program: String::new(),
            args: Vec::new(),
            working_dir: None,
            env: HashMap::new(),
            channel_mode: ProcessChannelMode::SeparateChannels,
            child: None,
            stdin_handle: None,
            stdout_reader: None,
            stderr_reader: None,
            stdout_buf: Arc::new(Mutex::new(Vec::new())),
            stderr_buf: Arc::new(Mutex::new(Vec::new())),
            state: ProcessState::NotRunning,
            exit_code: None,
            exit_status: None,
            pid: None,
        }
    }

    /// Sets the program executable name or path.
    pub fn set_program(&mut self, program: impl Into<String>) {
        self.program = program.into();
    }

    /// Sets the command-line argument list.
    pub fn set_arguments(&mut self, args: Vec<String>) {
        self.args = args;
    }

    /// Appends a single argument.
    pub fn add_argument(&mut self, arg: impl Into<String>) {
        self.args.push(arg.into());
    }

    /// Sets the working directory for the spawned process.
    pub fn set_working_directory(&mut self, dir: impl Into<PathBuf>) {
        self.working_dir = Some(dir.into());
    }

    /// Sets an environment variable for the process.
    pub fn set_environment_variable(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.env.insert(key.into(), value.into());
    }

    /// Sets the entire environment variable map.
    pub fn set_environment(&mut self, env: HashMap<String, String>) {
        self.env = env;
    }

    /// Sets the channel forwarding mode.
    pub fn set_channel_mode(&mut self, mode: ProcessChannelMode) {
        self.channel_mode = mode;
    }

    /// Returns the current process state.
    pub fn state(&self) -> ProcessState {
        self.state
    }

    /// Returns the native Process ID (PID) if currently running.
    pub fn process_id(&self) -> Option<u32> {
        self.pid
    }

    /// Returns the process exit code, if finished.
    pub fn exit_code(&self) -> Option<i32> {
        self.exit_code
    }

    /// Returns the exit status (normal or crash), if finished.
    pub fn exit_status(&self) -> Option<ExitStatus> {
        self.exit_status
    }

    /// Spawns the process.
    pub fn start(&mut self) -> io::Result<()> {
        if self.state != ProcessState::NotRunning {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "process is already running",
            ));
        }

        // Preserve NotRunning when command construction or spawn fails.
        self.state = ProcessState::Starting;
        let mut cmd = Command::new(&self.program);
        cmd.args(&self.args);

        if let Some(ref dir) = self.working_dir {
            cmd.current_dir(dir);
        }

        for (k, v) in &self.env {
            cmd.env(k, v);
        }

        match self.channel_mode {
            ProcessChannelMode::SeparateChannels => {
                cmd.stdin(Stdio::piped());
                cmd.stdout(Stdio::piped());
                cmd.stderr(Stdio::piped());
            }
            ProcessChannelMode::MergedChannels => {
                cmd.stdin(Stdio::piped());
                cmd.stdout(Stdio::piped());
                cmd.stderr(Stdio::piped());
            }
            ProcessChannelMode::ForwardedChannels => {
                cmd.stdin(Stdio::inherit());
                cmd.stdout(Stdio::inherit());
                cmd.stderr(Stdio::inherit());
            }
        }

        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }

        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(error) => {
                self.state = ProcessState::NotRunning;
                return Err(error);
            }
        };
        self.pid = Some(child.id());
        self.exit_code = None;
        self.exit_status = None;
        self.stdin_handle = child.stdin.take();
        self.stdout_buf.lock().clear();
        self.stderr_buf.lock().clear();
        self.stdout_reader = child.stdout.take().map(|mut out| {
            let output = Arc::clone(&self.stdout_buf);
            thread::spawn(move || {
                let mut chunk = [0u8; 4096];
                loop {
                    match out.read(&mut chunk) {
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                        Ok(0) | Err(_) => break,
                        Ok(count) => output.lock().extend_from_slice(&chunk[..count]),
                    }
                }
            })
        });
        self.stderr_reader = child.stderr.take().map(|mut err| {
            let output = Arc::clone(&self.stderr_buf);
            thread::spawn(move || {
                let mut chunk = [0u8; 4096];
                loop {
                    match err.read(&mut chunk) {
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                        Ok(0) | Err(_) => break,
                        Ok(count) => output.lock().extend_from_slice(&chunk[..count]),
                    }
                }
            })
        });
        self.child = Some(child);
        self.state = ProcessState::Running;
        Ok(())
    }

    /// Starts a shell command line (split into program and arguments).
    pub fn start_command(&mut self, command: &str) -> io::Result<()> {
        let parts: Vec<String> = command.split_whitespace().map(|s| s.to_string()).collect();
        if parts.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "command is empty",
            ));
        }
        self.set_program(&parts[0]);
        self.set_arguments(parts[1..].to_vec());
        self.start()
    }

    /// Waits up to `timeout` (or indefinitely if `None`) for the process to finish.
    pub fn wait_for_finished(&mut self, timeout: Option<Duration>) -> bool {
        if self.state == ProcessState::NotRunning {
            return true;
        }
        let start_time = Instant::now();

        loop {
            let result = match self.child.as_mut() {
                Some(child) => child.try_wait(),
                None => return true,
            };
            match result {
                Ok(Some(status)) => {
                    if let Some(mut child) = self.child.take() {
                        let _ = child.wait();
                    }
                    self.exit_code = status.code();
                    self.exit_status = Some(if status.code().is_some() {
                        ExitStatus::NormalExit
                    } else {
                        ExitStatus::CrashExit
                    });
                    // A grandchild may keep the pipes open; never block past the timeout.
                    for reader in [self.stdout_reader.take(), self.stderr_reader.take()]
                        .into_iter()
                        .flatten()
                    {
                        loop {
                            if reader.is_finished() {
                                let _ = reader.join();
                                break;
                            }
                            if timeout.is_some_and(|t| start_time.elapsed() >= t) {
                                break; // detach the reader
                            }
                            thread::sleep(Duration::from_millis(1));
                        }
                    }
                    self.state = ProcessState::NotRunning;
                    self.pid = None;
                    return true;
                }
                Ok(None) => {
                    if timeout.is_some_and(|t| start_time.elapsed() >= t) {
                        return false;
                    }
                    thread::sleep(Duration::from_millis(5));
                }
                Err(_) => {
                    self.state = ProcessState::NotRunning;
                    self.exit_status = Some(ExitStatus::CrashExit);
                    self.pid = None;
                    return true;
                }
            }
        }
    }

    /// Writes data to the process's standard input pipe.
    pub fn write_stdin(&mut self, data: &[u8]) -> io::Result<usize> {
        if let Some(ref mut stdin) = self.stdin_handle {
            stdin.write(data)
        } else {
            Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "process stdin is not open",
            ))
        }
    }

    /// Closes the standard input pipe.
    pub fn close_stdin(&mut self) {
        self.stdin_handle = None;
    }

    /// Reads all accumulated standard output bytes.
    pub fn read_all_stdout(&mut self) -> Vec<u8> {
        std::mem::take(&mut *self.stdout_buf.lock())
    }

    /// Reads all accumulated standard error bytes without waiting for process exit.
    pub fn read_all_stderr(&mut self) -> Vec<u8> {
        std::mem::take(&mut *self.stderr_buf.lock())
    }

    /// Forcefully kills the running process and reaps it.
    pub fn kill(&mut self) -> io::Result<()> {
        if let Some(child) = self.child.as_mut() {
            child.kill()?;
            let status = child.wait()?;
            self.exit_code = status.code();
            self.exit_status = Some(ExitStatus::CrashExit);
            self.child = None;
            self.stdin_handle = None;
            self.state = ProcessState::NotRunning;
            self.pid = None;
        }
        Ok(())
    }

    /// Requests termination of the running process.
    pub fn terminate(&mut self) -> io::Result<()> {
        self.kill()
    }

    // =========================================================================
    // Static Helpers
    // =========================================================================

    /// Executes `program` with `args`, waiting until finished, returning the exit code.
    pub fn execute(program: &str, args: &[&str]) -> io::Result<i32> {
        let mut proc = Process::new();
        proc.set_program(program);
        proc.set_arguments(args.iter().map(|s| s.to_string()).collect());
        proc.start()?;
        proc.wait_for_finished(None);
        Ok(proc.exit_code().unwrap_or(-1))
    }
 
    /// Starts a detached process without retaining a handle.
    pub fn start_detached(program: &str, args: &[&str]) -> io::Result<u32> {
        let mut cmd = Command::new(program);
        cmd.args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let child = cmd.spawn()?;
        let pid = child.id();
        thread::spawn(move || {
            let mut child = child;
            let _ = child.wait();
        });
        Ok(pid)
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        if self.state == ProcessState::Running {
            let _ = self.kill();
        }
    }
}
