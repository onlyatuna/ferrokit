// SPDX-License-Identifier: LGPL-3.0-only
//! QProcess cross-platform process execution abstraction (`QProcess` equivalent).
//!
//! Provides external process launching, environment and working directory configuration,
//! asynchronous/synchronous lifecycle management, and stream I/O via stdin/stdout/stderr pipes.

use parking_lot::Mutex;
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// How long, after the child has exited, the pipe readers get to reach the end of their pipes
/// when the wait has a time limit: long enough to collect what the child wrote, short enough
/// not to hang on a grandchild that holds the pipe open.
const READER_DRAIN: Duration = Duration::from_secs(1);

/// A read end of a child pipe.
enum PipeSource {
    Stdout(ChildStdout),
    Stderr(ChildStderr),
    /// Both of the child's streams, for [`ProcessChannelMode::MergedChannels`].
    Merged(io::PipeReader),
}

impl PipeSource {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Stdout(pipe) => pipe.read(buf),
            Self::Stderr(pipe) => pipe.read(buf),
            Self::Merged(pipe) => pipe.read(buf),
        }
    }

    #[cfg(unix)]
    fn as_raw_fd(&self) -> std::os::fd::RawFd {
        use std::os::fd::AsRawFd;
        match self {
            Self::Stdout(pipe) => pipe.as_raw_fd(),
            Self::Stderr(pipe) => pipe.as_raw_fd(),
            Self::Merged(pipe) => pipe.as_raw_fd(),
        }
    }

    #[cfg(windows)]
    fn as_raw_handle(&self) -> std::os::windows::io::RawHandle {
        use std::os::windows::io::AsRawHandle;
        match self {
            Self::Stdout(pipe) => pipe.as_raw_handle(),
            Self::Stderr(pipe) => pipe.as_raw_handle(),
            Self::Merged(pipe) => pipe.as_raw_handle(),
        }
    }
}

/// The half of a reader's wake-up channel that stays with the reader thread: what lets it block
/// until data arrives and still be stopped at once while a grandchild holds the pipe open.
///
/// On Unix this is the read end of a pipe that [`Stopper`] writes to, and the reader `poll`s it
/// together with the child's pipe. On Windows a blocked `ReadFile` is interrupted instead, with
/// `CancelIoEx` (see [`ReaderThread::stop_and_join`]), so there is nothing to wait on.
struct Waiter {
    #[cfg(unix)]
    bell: io::PipeReader,
    /// Cleared once the other end is gone without a stop request, so it is not polled again.
    #[cfg(unix)]
    bell_open: bool,
}

/// The half of a reader's wake-up channel that stays with the owner of the reader.
#[derive(Debug)]
struct Stopper {
    /// What wakes the reader: a byte is written to it, and it is closed, when the reader is to
    /// stop. The byte matters when a copy of this descriptor is held elsewhere (a child forked
    /// before the descriptor was marked close-on-exec): closing alone would then not be seen.
    #[cfg(unix)]
    bell: io::PipeWriter,
}

/// Creates the wake-up channel of one reader.
fn wake_channel() -> io::Result<(Waiter, Stopper)> {
    #[cfg(unix)]
    {
        let (rx, tx) = io::pipe()?;
        Ok((
            Waiter {
                bell: rx,
                bell_open: true,
            },
            Stopper { bell: tx },
        ))
    }
    #[cfg(windows)]
    {
        Ok((Waiter {}, Stopper {}))
    }
}

impl Waiter {
    /// Waits until `source` can be read without blocking (data, end of the pipe or an error), or
    /// until `stop` is set. Returns `false` on stop. A reader that blocked in `read` could not be
    /// stopped while a grandchild still holds the write end open.
    ///
    /// `stop` is set before the stopper is closed, so a reader that saw the stopper closed and
    /// finds `stop` clear knows its `Process` was dropped without a stop request: the pipe is
    /// then read to its end, as it always was.
    #[cfg(unix)]
    fn wait(&mut self, source: &PipeSource, stop: &AtomicBool) -> bool {
        use std::os::fd::AsRawFd;
        loop {
            if stop.load(Ordering::Acquire) {
                return false;
            }
            let mut fds = [
                libc::pollfd {
                    fd: source.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    // A negative descriptor is ignored by `poll`.
                    fd: if self.bell_open {
                        self.bell.as_raw_fd()
                    } else {
                        -1
                    },
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            // SAFETY: `fds` is two valid `pollfd`s, and `source` and `self.bell` keep their
            // descriptors open for the whole call.
            let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, -1) };
            if ready < 0 {
                if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                // Let `read` report what is wrong.
                return true;
            }
            if fds[1].revents != 0 {
                if stop.load(Ordering::Acquire) {
                    return false;
                }
                self.bell_open = false;
            }
            // Readable, hung up or failed: `read` reports which without blocking.
            if fds[0].revents != 0 {
                return true;
            }
        }
    }

    /// See the Unix version. A Windows reader blocks in `read` itself and is interrupted from
    /// outside, so it only has to look at the flag between reads.
    #[cfg(windows)]
    fn wait(&mut self, _source: &PipeSource, stop: &AtomicBool) -> bool {
        !stop.load(Ordering::Acquire)
    }
}

/// The handle of the pipe a Windows reader thread reads, for as long as the thread may still be
/// blocked in a read of it. The thread clears it before the pipe is closed, and a stopper holds
/// the lock while it uses the value, so the value is never used after the handle was closed (and
/// perhaps reused for something else).
#[cfg(windows)]
type OpenHandle = Arc<Mutex<Option<usize>>>;

/// A thread that appends everything read from a pipe to a buffer.
#[derive(Debug)]
struct ReaderThread {
    thread: thread::JoinHandle<()>,
    #[cfg(unix)]
    stopper: Stopper,
    #[cfg(windows)]
    pipe: OpenHandle,
}

impl ReaderThread {
    fn is_finished(&self) -> bool {
        self.thread.is_finished()
    }

    /// Waits for the thread to reach the end of its pipe.
    fn join(self) {
        let _ = self.thread.join();
    }

    /// Makes the thread stop waiting for data, however long it would wait, and joins it. The
    /// caller has set the stop flag. The pipe is closed when this returns.
    #[cfg(unix)]
    fn stop_and_join(self) {
        let Self { thread, stopper } = self;
        // The pipe is empty, so one byte does not block. A failed write means the reader is
        // already gone, and the close below is then all that is left to do.
        let _ = (&stopper.bell).write_all(&[1]);
        drop(stopper);
        let _ = thread.join();
    }

    /// See the Unix version. `CancelIoEx` aborts the read the thread is blocked in. A thread that
    /// is between two reads has none to abort and starts the next one, so the call is repeated
    /// until the thread has seen the stop flag and ended.
    #[cfg(windows)]
    fn stop_and_join(self) {
        use windows_sys::Win32::System::IO::CancelIoEx;
        while !self.thread.is_finished() {
            if let Some(handle) = *self.pipe.lock() {
                // SAFETY: the reader clears `pipe` before it closes the handle and the lock is
                // held, so `handle` is still open. A null `OVERLAPPED` cancels every pending
                // operation on it; failing when there is none is expected.
                unsafe { CancelIoEx(handle as _, std::ptr::null()) };
            }
            thread::sleep(Duration::from_millis(1));
        }
        let _ = self.thread.join();
    }
}

/// Starts a thread that appends everything read from `source` to `output` until the end of the
/// pipe, or until `stop` is set and the reader is woken. The thread closes the pipe when it ends.
fn spawn_reader(
    source: PipeSource,
    (mut waiter, stopper): (Waiter, Stopper),
    output: &Arc<Mutex<Vec<u8>>>,
    stop: &Arc<AtomicBool>,
) -> ReaderThread {
    let output = Arc::clone(output);
    let stop = Arc::clone(stop);
    // A Windows reader is interrupted through its pipe, not woken through a channel.
    #[cfg(windows)]
    let _ = stopper;
    #[cfg(windows)]
    let pipe: OpenHandle = Arc::new(Mutex::new(Some(source.as_raw_handle() as usize)));
    #[cfg(windows)]
    let thread_pipe = Arc::clone(&pipe);
    let thread = thread::spawn(move || {
        let mut source = source;
        // Declared after `source`, so it runs first when the thread ends: `source` is closed
        // only after the handle stoppers may use has been withdrawn.
        #[cfg(windows)]
        let _withdraw = Withdraw(thread_pipe);
        let mut chunk = [0u8; 4096];
        while waiter.wait(&source, &stop) {
            match source.read(&mut chunk) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Ok(0) | Err(_) => break,
                Ok(count) => output.lock().extend_from_slice(&chunk[..count]),
            }
        }
    });
    ReaderThread {
        thread,
        #[cfg(unix)]
        stopper,
        #[cfg(windows)]
        pipe,
    }
}

/// Withdraws an [`OpenHandle`] when dropped.
#[cfg(windows)]
struct Withdraw(OpenHandle);

#[cfg(windows)]
impl Drop for Withdraw {
    fn drop(&mut self) {
        *self.0.lock() = None;
    }
}

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
    stdout_reader: Option<ReaderThread>,
    stderr_reader: Option<ReaderThread>,
    stdout_buf: Arc<Mutex<Vec<u8>>>,
    stderr_buf: Arc<Mutex<Vec<u8>>>,
    /// Set to make the reader threads of the current run stop and close their pipes.
    reader_stop: Arc<AtomicBool>,

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
            reader_stop: Arc::new(AtomicBool::new(false)),
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

        // The wake-up channels of the readers, made before the child exists so that failing to
        // make one leaves nothing to undo.
        let reader_count = match self.channel_mode {
            ProcessChannelMode::SeparateChannels => 2,
            ProcessChannelMode::MergedChannels => 1,
            ProcessChannelMode::ForwardedChannels => 0,
        };
        let mut wake_channels = Vec::with_capacity(reader_count);
        for _ in 0..reader_count {
            match wake_channel() {
                Ok(channel) => wake_channels.push(channel),
                Err(error) => {
                    self.state = ProcessState::NotRunning;
                    return Err(error);
                }
            }
        }
        let mut wake_channels = wake_channels.into_iter();
        let (stdout_wake, stderr_wake) = (wake_channels.next(), wake_channels.next());

        let mut merged_reader = None;
        match self.channel_mode {
            ProcessChannelMode::SeparateChannels => {
                cmd.stdin(Stdio::piped());
                cmd.stdout(Stdio::piped());
                cmd.stderr(Stdio::piped());
            }
            ProcessChannelMode::MergedChannels => {
                // Qt semantics: the child's stderr is the same pipe as its stdout, read by a
                // single reader, so interleaving is the child's write order and stderr reads empty.
                let (reader, writer) = match io::pipe() {
                    Ok(pair) => pair,
                    Err(error) => {
                        self.state = ProcessState::NotRunning;
                        return Err(error);
                    }
                };
                let writer_err = match writer.try_clone() {
                    Ok(clone) => clone,
                    Err(error) => {
                        self.state = ProcessState::NotRunning;
                        return Err(error);
                    }
                };
                cmd.stdin(Stdio::piped());
                cmd.stdout(writer);
                cmd.stderr(writer_err);
                merged_reader = Some(reader);
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

        let spawned = cmd.spawn();
        // The command owns the pipe's write ends; drop them so the reader sees EOF at exit.
        drop(cmd);
        let mut child = match spawned {
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
        // Fresh buffers and stop flag per run, so nothing of a previous run mixes into this one.
        self.stdout_buf = Arc::new(Mutex::new(Vec::new()));
        self.stderr_buf = Arc::new(Mutex::new(Vec::new()));
        self.reader_stop = Arc::new(AtomicBool::new(false));
        let stdout_source = match merged_reader {
            Some(reader) => Some(PipeSource::Merged(reader)),
            None => child.stdout.take().map(PipeSource::Stdout),
        };
        self.stdout_reader = stdout_source
            .zip(stdout_wake)
            .map(|(source, wake)| spawn_reader(source, wake, &self.stdout_buf, &self.reader_stop));
        self.stderr_reader = child
            .stderr
            .take()
            .map(PipeSource::Stderr)
            .zip(stderr_wake)
            .map(|(source, wake)| spawn_reader(source, wake, &self.stderr_buf, &self.reader_stop));
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
                    // The wait's own deadline does not apply to what the child had already
                    // written: its tail is collected for `READER_DRAIN` from this exit.
                    self.finish_readers(timeout.map(|_| READER_DRAIN));
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
                    if let Some(mut child) = self.child.take() {
                        let _ = child.kill();
                        let _ = child.wait();
                    }
                    // Bounded: a grandchild may still hold the pipes open.
                    self.finish_readers(Some(READER_DRAIN));
                    self.state = ProcessState::NotRunning;
                    self.exit_status = Some(ExitStatus::CrashExit);
                    self.pid = None;
                    return true;
                }
            }
        }
    }

    /// Joins the reader threads. With `Some(grace)`, readers that have not reached the end of
    /// their pipe `grace` after this call are told to stop instead of blocking on a pipe that a
    /// grandchild still holds open: they are woken out of their wait, close their end of the
    /// pipe and are joined, so no thread or descriptor outlives the call. With `None` they are
    /// joined when they reach the end of their pipe.
    fn finish_readers(&mut self, grace: Option<Duration>) {
        let start = Instant::now();
        let readers = [self.stdout_reader.take(), self.stderr_reader.take()];
        for reader in readers.iter().flatten() {
            while !reader.is_finished() {
                if grace.is_some_and(|g| start.elapsed() >= g) {
                    self.reader_stop.store(true, Ordering::Release);
                    break;
                }
                thread::sleep(Duration::from_millis(1));
            }
        }
        let stopping = self.reader_stop.load(Ordering::Acquire);
        for reader in readers.into_iter().flatten() {
            if stopping {
                reader.stop_and_join();
            } else {
                reader.join();
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
            self.finish_readers(Some(Duration::from_millis(100)));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn reader_of(reader: io::PipeReader) -> (ReaderThread, Arc<Mutex<Vec<u8>>>, Arc<AtomicBool>) {
        let output = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let channel = wake_channel().unwrap();
        let handle = spawn_reader(PipeSource::Merged(reader), channel, &output, &stop);
        (handle, output, stop)
    }

    /// Stops `handle` on a helper thread, so that a reader that cannot be stopped fails the test
    /// instead of hanging it.
    fn stop_within_the_deadline(handle: ReaderThread) {
        let (done, finished) = std::sync::mpsc::channel();
        thread::spawn(move || {
            handle.stop_and_join();
            let _ = done.send(());
        });
        finished
            .recv_timeout(Duration::from_secs(10))
            .expect("the reader to stop");
    }

    /// A reader of a pipe that still has a writer (a grandchild holding it open) is not stuck in
    /// `read`: it stops on request and closes its end, so the thread and the descriptor are
    /// released.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_stopped_reader_closes_a_pipe_that_still_has_a_writer() {
        let (reader, mut writer) = io::pipe().unwrap();
        let (handle, output, stop) = reader_of(reader);

        writer.write_all(b"hello").unwrap();
        wait_until("the written bytes", || output.lock().len() == 5);
        assert!(
            !handle.is_finished(),
            "the writer is open: the reader keeps waiting"
        );

        stop.store(true, Ordering::Release);
        stop_within_the_deadline(handle);
        assert_eq!(*output.lock(), b"hello");
        // The read end is closed, so the pipe has no reader left.
        assert!(writer.write_all(b"x").is_err());
    }

    /// A copy of the wake-up pipe's write end that lives on elsewhere (a child that inherited it)
    /// keeps the pipe from ever reporting a hang-up: the stop must still reach the reader.
    #[cfg(unix)]
    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_stop_wakes_the_reader_although_a_copy_of_the_wake_up_pipe_is_open() {
        let (reader, mut writer) = io::pipe().unwrap();
        let output = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (waiter, stopper) = wake_channel().unwrap();
        let leaked = stopper.bell.try_clone().unwrap();
        let handle = spawn_reader(
            PipeSource::Merged(reader),
            (waiter, stopper),
            &output,
            &stop,
        );

        writer.write_all(b"hi").unwrap();
        wait_until("the written bytes", || output.lock().len() == 2);
        stop.store(true, Ordering::Release);
        stop_within_the_deadline(handle);
        assert_eq!(*output.lock(), b"hi");
        assert!(writer.write_all(b"x").is_err(), "the read end is closed");
        drop(leaked);
    }

    /// A stop that arrives while the reader is starting up, between two reads or before its first
    /// one is not lost: whatever the reader was doing, it ends.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_stop_is_never_lost_whatever_the_reader_is_doing() {
        for round in 0..200u32 {
            let (reader, mut writer) = io::pipe().unwrap();
            let (handle, output, stop) = reader_of(reader);
            if round % 2 == 1 {
                writer.write_all(b"x").unwrap();
                if round % 4 == 3 {
                    wait_until("the written byte", || output.lock().len() == 1);
                }
            }
            stop.store(true, Ordering::Release);
            stop_within_the_deadline(handle);
            assert!(
                writer.write_all(b"y").is_err(),
                "round {round}: the read end is closed"
            );
        }
    }

    /// An owner that goes away without asking the reader to stop (a dropped `Process` whose child
    /// could not be killed) leaves it reading to the end of the pipe.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_reader_whose_owner_is_gone_keeps_reading_until_the_end_of_its_pipe() {
        let (reader, mut writer) = io::pipe().unwrap();
        let (handle, output, _stop) = reader_of(reader);
        drop(handle);

        writer.write_all(b"a").unwrap();
        wait_until("the first byte", || output.lock().len() == 1);
        thread::sleep(Duration::from_millis(50));
        writer.write_all(b"b").unwrap();
        wait_until("the second byte", || output.lock().len() == 2);
        drop(writer);
        assert_eq!(*output.lock(), b"ab");
    }

    /// Without a stop request a reader runs to the end of the pipe, past the last byte.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_reader_collects_everything_up_to_the_end_of_its_pipe() {
        let (reader, mut writer) = io::pipe().unwrap();
        let (handle, output, _stop) = reader_of(reader);

        let data = vec![7u8; 100_000];
        writer.write_all(&data).unwrap();
        drop(writer);
        handle.join();
        assert_eq!(*output.lock(), data);
    }
}
