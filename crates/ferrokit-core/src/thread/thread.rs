// SPDX-License-Identifier: LGPL-3.0-only
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{Builder as StdThreadBuilder, JoinHandle};
use std::time::Duration;

use parking_lot::{Condvar, Mutex};

use crate::event_loop::{EventLoop, EventLoopHandle};
use crate::object::{EventSender, ThreadContext, ThreadId};
use crate::signal::Signal;

/// Thread priority levels, modeled after Qt's `QThread::Priority`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThreadPriority {
    /// Gets CPU time only while no other thread is ready to run.
    Idle,
    /// Lowest scheduling priority.
    Lowest,
    /// Lower scheduling priority than normal.
    Low,
    /// Default operating system priority.
    #[default]
    Normal,
    /// Higher scheduling priority than normal.
    High,
    /// Highest priority before real-time/time-critical.
    Highest,
    /// Real-time or time-critical priority.
    TimeCritical,
}

thread_local! {
    static CURRENT_INTERRUPTION_FLAG: RefCell<Option<Arc<AtomicBool>>> = const { RefCell::new(None) };
}

/// Applies a requested priority on the new thread, matching `QThread::start`.
///
/// Windows maps every [`ThreadPriority`] to `SetThreadPriority` levels (qthread_win.cpp:404-452).
/// Linux applies only `Idle`, as `SCHED_IDLE` (qthread_unix.cpp:729-767). Other Unix targets
/// ignore the request: Qt scales by `sched_get_priority_min/max`, which is not mirrored here.
mod os_priority {
    use super::ThreadPriority;

    #[cfg(windows)]
    mod imp {
        use super::ThreadPriority;
        use windows_sys::Win32::System::Threading::{
            GetCurrentThread, GetThreadPriority, SetThreadPriority, THREAD_PRIORITY_ABOVE_NORMAL,
            THREAD_PRIORITY_BELOW_NORMAL, THREAD_PRIORITY_HIGHEST, THREAD_PRIORITY_IDLE,
            THREAD_PRIORITY_LOWEST, THREAD_PRIORITY_NORMAL, THREAD_PRIORITY_TIME_CRITICAL,
        };

        /// Returned by `GetThreadPriority` on failure (`THREAD_PRIORITY_ERROR_RETURN`).
        const ERROR_RETURN: i32 = i32::MAX;

        /// Win32 level resolved on the creating thread; `None` leaves the new thread unchanged.
        pub(in super::super) type Pending = Option<i32>;

        pub(in super::super) fn resolve(requested: Option<ThreadPriority>) -> Pending {
            match requested {
                Some(priority) => Some(to_win32(priority)),
                // `Inherit`: copy the creator's current level (qthread_win.cpp:439-442).
                None => {
                    let current = unsafe { GetThreadPriority(GetCurrentThread()) };
                    (current != ERROR_RETURN).then_some(current)
                }
            }
        }

        pub(in super::super) fn apply(pending: Pending) {
            if let Some(level) = pending {
                if unsafe { SetThreadPriority(GetCurrentThread(), level) } == 0 {
                    eprintln!(
                        "ferrokit: SetThreadPriority failed: {}",
                        std::io::Error::last_os_error()
                    );
                }
            }
        }

        fn to_win32(priority: ThreadPriority) -> i32 {
            match priority {
                ThreadPriority::Idle => THREAD_PRIORITY_IDLE,
                ThreadPriority::Lowest => THREAD_PRIORITY_LOWEST,
                ThreadPriority::Low => THREAD_PRIORITY_BELOW_NORMAL,
                ThreadPriority::Normal => THREAD_PRIORITY_NORMAL,
                ThreadPriority::High => THREAD_PRIORITY_ABOVE_NORMAL,
                ThreadPriority::Highest => THREAD_PRIORITY_HIGHEST,
                ThreadPriority::TimeCritical => THREAD_PRIORITY_TIME_CRITICAL,
            }
        }
    }

    #[cfg(all(unix, target_os = "linux"))]
    mod imp {
        use super::ThreadPriority;

        /// `true` when the new thread must run as `SCHED_IDLE`.
        pub(in super::super) type Pending = bool;

        // Qt's non-idle priorities scale within SCHED_OTHER's range, which is 0 on Linux, so
        // they resolve to the default policy and are a no-op here (qthread_unix.cpp:729-767).
        pub(in super::super) fn resolve(requested: Option<ThreadPriority>) -> Pending {
            matches!(requested, Some(ThreadPriority::Idle))
        }

        pub(in super::super) fn apply(idle: Pending) {
            if !idle {
                return;
            }
            let param: libc::sched_param = unsafe { std::mem::zeroed() };
            let rc = unsafe {
                libc::pthread_setschedparam(libc::pthread_self(), libc::SCHED_IDLE, &param)
            };
            if rc != 0 {
                eprintln!(
                    "ferrokit: pthread_setschedparam(SCHED_IDLE) failed: {}",
                    std::io::Error::from_raw_os_error(rc)
                );
            }
        }
    }

    #[cfg(not(any(windows, all(unix, target_os = "linux"))))]
    mod imp {
        use super::ThreadPriority;

        pub(in super::super) type Pending = ();

        pub(in super::super) fn resolve(_requested: Option<ThreadPriority>) -> Pending {}

        pub(in super::super) fn apply(_pending: Pending) {}
    }

    pub(super) use imp::{apply, resolve};
}

/// Completion state shared between a thread and its handle.
///
/// `finishing` mirrors `QThread::isFinished()` (`threadState >= Finishing`, qthread.cpp:495-499):
/// it becomes true once the body has returned, before `finished` is emitted. `done` is set after
/// `finished` has been emitted and is what [`Completion::wait`] blocks on.
struct Completion {
    finishing: AtomicBool,
    done: Mutex<bool>,
    done_cv: Condvar,
}

impl Completion {
    fn new() -> Self {
        Self {
            finishing: AtomicBool::new(false),
            done: Mutex::new(false),
            done_cv: Condvar::new(),
        }
    }

    fn wait(&self, timeout: Duration) -> bool {
        let mut done = self.done.lock();
        self.done_cv.wait_while_for(&mut done, |d| !*d, timeout);
        *done
    }
}

/// Marks completion and emits `finished` on the worker thread, including when the body panics.
struct FinishGuard {
    completion: Arc<Completion>,
    finished: Signal<()>,
}

impl Drop for FinishGuard {
    fn drop(&mut self) {
        self.completion.finishing.store(true, Ordering::SeqCst);
        self.finished.emit(&());
        *self.completion.done.lock() = true;
        self.completion.done_cv.notify_all();
    }
}

/// Thread management utilities, modeled after Qt's `QThread`.
pub struct Thread;

impl Thread {
    /// Spawns a new OS thread initialized with `ThreadContext`.
    pub fn spawn<F, T>(f: F) -> ThreadHandle<T>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        ThreadBuilder::new()
            .spawn(f)
            .expect("failed to spawn thread")
    }

    /// Spawns a dedicated thread with an integrated Qt-style event loop.
    pub fn spawn_with_event_loop<F>(setup: F) -> EventLoopThreadHandle
    where
        F: FnOnce(&EventSender) + Send + 'static,
    {
        let (tx, rx) = std::sync::mpsc::channel();
        let interruption_flag = Arc::new(AtomicBool::new(false));
        let flag_clone = Arc::clone(&interruption_flag);
        let completion = Arc::new(Completion::new());
        let thread_completion = Arc::clone(&completion);
        let pending_exit = Arc::new(Mutex::new(None));
        let thread_pending_exit = Arc::clone(&pending_exit);
        let started = Signal::new();
        let finished = Signal::new();
        let thread_started = started.clone();
        let thread_finished = finished.clone();
        // `QThread::start()` defaults to `Inherit`.
        let priority = os_priority::resolve(None);

        let handle = StdThreadBuilder::new()
            .name("ferrokit-event-thread".to_string())
            .spawn(move || {
                os_priority::apply(priority);
                // Declared before `FinishGuard`, so `finished` is emitted while the context is
                // installed; the context and sender registration are cleared on every exit path,
                // including a panic in `setup` or in the loop.
                let _clear_context = ClearContextOnDrop;
                let guard = FinishGuard {
                    completion: thread_completion,
                    finished: thread_finished,
                };

                // A spawned thread is never the main thread; its own context answers
                // `is_main_thread()` before anything can consult the application's main-thread id.
                ThreadContext::init_current(false, None);
                let event_loop = EventLoop::new();
                let sender = event_loop.sender();
                let thread_id = ThreadId::current();

                ThreadContext::init_current(false, Some(sender.clone()));
                CURRENT_INTERRUPTION_FLAG.with(|flag| {
                    *flag.borrow_mut() = Some(flag_clone);
                });

                setup(&sender);
                let _ = tx.send((thread_id, sender.clone(), event_loop.handle()));

                thread_started.emit(&());
                // `QThread::exec` (qthread.cpp:685-698): an `exit` that came before the loop ran
                // is returned at once, and the request is spent once the loop has ended.
                let code = event_loop.exec_after(|| *thread_pending_exit.lock());
                *thread_pending_exit.lock() = None;
                // `finished` is emitted while this thread's context is still installed.
                drop(guard);
                code
            })
            .expect("failed to spawn event loop thread");

        let (thread_id, sender, loop_handle) =
            rx.recv().expect("event loop thread failed to initialize");

        EventLoopThreadHandle {
            thread_id,
            sender,
            loop_handle,
            pending_exit,
            interruption_flag,
            join_handle: Some(handle),
            completion,
            started,
            finished,
        }
    }

    /// Creates a builder to configure and spawn a thread.
    pub fn builder() -> ThreadBuilder {
        ThreadBuilder::new()
    }

    /// Returns the current thread's unique [`ThreadId`].
    pub fn current_id() -> ThreadId {
        ThreadContext::current_id()
    }

    /// Returns whether the current thread is the main UI thread.
    pub fn is_main_thread() -> bool {
        ThreadContext::is_main_thread()
    }

    /// Returns whether interruption has been requested on the current thread.
    pub fn is_interruption_requested() -> bool {
        CURRENT_INTERRUPTION_FLAG.with(|flag| {
            flag.borrow()
                .as_ref()
                .map(|f| f.load(Ordering::SeqCst))
                .unwrap_or(false)
        })
    }

    /// Suspends execution for the specified milliseconds, matching `QThread::msleep`.
    pub fn msleep(ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }

    /// Suspends execution for the specified duration, matching `QThread::sleep`.
    pub fn sleep(duration: Duration) {
        std::thread::sleep(duration);
    }

    /// Yields CPU execution to another ready thread, matching `QThread::yieldCurrentThread`.
    pub fn yield_now() {
        std::thread::yield_now();
    }

    /// Returns the ideal number of threads for the system (CPU cores), matching `QThread::idealThreadCount`.
    pub fn ideal_thread_count() -> usize {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
    }
}

/// A builder for configuring and spawning OS threads.
pub struct ThreadBuilder {
    name: Option<String>,
    stack_size: Option<usize>,
    /// `None` is Qt's `Inherit`: the new thread takes the creator's priority.
    priority: Option<ThreadPriority>,
}

impl Default for ThreadBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Clears the thread's `ThreadContext` on every exit path, including unwinding out of the body.
/// Declared before `FinishGuard`, so `finished` is still emitted while the context is installed.
struct ClearContextOnDrop;

impl Drop for ClearContextOnDrop {
    fn drop(&mut self) {
        ThreadContext::clear_current();
    }
}

impl ThreadBuilder {
    /// Creates a default thread builder.
    pub fn new() -> Self {
        Self {
            name: None,
            stack_size: None,
            priority: None,
        }
    }

    /// Sets the thread's name.
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Sets the thread stack size in bytes.
    pub fn stack_size(mut self, size: usize) -> Self {
        self.stack_size = Some(size);
        self
    }

    /// Sets the thread scheduling priority, applied when the thread starts.
    pub fn priority(mut self, priority: ThreadPriority) -> Self {
        self.priority = Some(priority);
        self
    }

    /// Spawns the thread executing `f`.
    pub fn spawn<F, T>(self, f: F) -> std::io::Result<ThreadHandle<T>>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let mut builder = StdThreadBuilder::new();
        if let Some(name) = self.name {
            builder = builder.name(name);
        }
        if let Some(stack) = self.stack_size {
            builder = builder.stack_size(stack);
        }

        // Resolved on the creating thread so `Inherit` copies the creator's current level.
        let priority = os_priority::resolve(self.priority);
        let interruption_flag = Arc::new(AtomicBool::new(false));
        let flag_clone = Arc::clone(&interruption_flag);
        let completion = Arc::new(Completion::new());
        let thread_completion = Arc::clone(&completion);
        let started = Signal::new();
        let finished = Signal::new();
        let thread_started = started.clone();
        let thread_finished = finished.clone();

        let (id_tx, id_rx) = std::sync::mpsc::channel();

        let handle = builder.spawn(move || {
            os_priority::apply(priority);
            let current_id = ThreadId::current();
            let _ = id_tx.send(current_id);

            ThreadContext::init_current(false, None);
            CURRENT_INTERRUPTION_FLAG.with(|flag| {
                *flag.borrow_mut() = Some(flag_clone);
            });
            let _clear_context = ClearContextOnDrop;

            thread_started.emit(&());
            let guard = FinishGuard {
                completion: thread_completion,
                finished: thread_finished,
            };
            let res = f();
            // `finished` is emitted while this thread's context is still installed.
            drop(guard);
            res
        })?;

        let thread_id = id_rx.recv().unwrap_or_else(|_| ThreadId::current());

        Ok(ThreadHandle {
            thread_id,
            interruption_flag,
            join_handle: Some(handle),
            completion,
            started,
            finished,
        })
    }
}

/// A handle to a spawned OS thread.
pub struct ThreadHandle<T> {
    thread_id: ThreadId,
    interruption_flag: Arc<AtomicBool>,
    join_handle: Option<JoinHandle<T>>,
    completion: Arc<Completion>,
    started: Signal<()>,
    finished: Signal<()>,
}

impl<T> ThreadHandle<T> {
    /// Returns the thread's unique identifier.
    pub fn id(&self) -> ThreadId {
        self.thread_id
    }

    /// Requests cooperative interruption of the thread, matching `QThread::requestInterruption`.
    pub fn request_interruption(&self) {
        self.interruption_flag.store(true, Ordering::SeqCst);
    }

    /// Returns whether interruption has been requested.
    pub fn is_interruption_requested(&self) -> bool {
        self.interruption_flag.load(Ordering::SeqCst)
    }

    /// Whether the thread has finished executing, matching `QThread::isFinished`.
    ///
    /// Becomes true once the body returns, without requiring [`ThreadHandle::join`].
    pub fn is_finished(&self) -> bool {
        self.completion.finishing.load(Ordering::SeqCst)
    }

    /// Whether the thread is still executing, matching `QThread::isRunning`.
    pub fn is_running(&self) -> bool {
        !self.is_finished()
    }

    /// Blocks until the thread finishes or `timeout` elapses, matching `QThread::wait`.
    ///
    /// Returns `true` once the thread has finished. Returns before the OS thread has fully
    /// exited on some platforms (qthread.cpp:955-959); [`ThreadHandle::join`] waits for that.
    pub fn wait(&self, timeout: Duration) -> bool {
        self.completion.wait(timeout)
    }

    /// Signal emitted on the new thread just before its body runs, matching `QThread::started`.
    pub fn started(&self) -> &Signal<()> {
        &self.started
    }

    /// Signal emitted on the thread as it finishes, matching `QThread::finished`.
    pub fn finished(&self) -> &Signal<()> {
        &self.finished
    }

    /// Blocks until the thread finishes and returns its result, matching `QThread::wait`.
    pub fn join(mut self) -> std::thread::Result<T> {
        if let Some(h) = self.join_handle.take() {
            h.join()
        } else {
            panic!("thread already joined");
        }
    }
}

/// A handle to a running thread with an active [`EventLoop`].
pub struct EventLoopThreadHandle {
    thread_id: ThreadId,
    sender: EventSender,
    loop_handle: EventLoopHandle,
    /// The code of an [`EventLoopThreadHandle::exit`] the thread's loop may not have seen yet
    /// (`QThread::exited` and `returnCode`, qthread.cpp:779-780).
    pending_exit: Arc<Mutex<Option<i32>>>,
    interruption_flag: Arc<AtomicBool>,
    join_handle: Option<JoinHandle<i32>>,
    completion: Arc<Completion>,
    started: Signal<()>,
    finished: Signal<()>,
}

impl EventLoopThreadHandle {
    /// Returns the thread's unique identifier.
    pub fn id(&self) -> ThreadId {
        self.thread_id
    }

    /// Returns an [`EventSender`] for dispatching events to this thread.
    pub fn sender(&self) -> &EventSender {
        &self.sender
    }

    /// Ends every event loop running on the thread (including nested ones) with `code`, matching
    /// `QThread::exit`. The code is returned by [`EventLoopThreadHandle::join`].
    ///
    /// The loops are told directly, not through an event posted to the thread, so an event
    /// filter cannot swallow the request. The loop that is delivering events at that moment
    /// finishes its current pass first. An `exit` that comes before the thread's loop runs is
    /// remembered: the loop then returns `code` at once, without delivering events.
    pub fn exit(&self, code: i32) {
        // Recorded before the loops are told: a loop that starts in between sees the code.
        let mut pending = self.pending_exit.lock();
        *pending = Some(code);
        self.loop_handle.exit(code);
    }

    /// Tells the thread's event loop to quit with code 0, matching `QThread::quit`.
    ///
    /// Does not request interruption (qthread.cpp:777-778).
    pub fn quit(&self) {
        self.exit(0);
    }

    /// Requests interruption of the thread.
    pub fn request_interruption(&self) {
        self.interruption_flag.store(true, Ordering::SeqCst);
    }

    /// Returns whether interruption has been requested.
    pub fn is_interruption_requested(&self) -> bool {
        self.interruption_flag.load(Ordering::SeqCst)
    }

    /// Whether the thread has finished executing, matching `QThread::isFinished`.
    pub fn is_finished(&self) -> bool {
        self.completion.finishing.load(Ordering::SeqCst)
    }

    /// Whether the thread is still executing, matching `QThread::isRunning`.
    pub fn is_running(&self) -> bool {
        !self.is_finished()
    }

    /// Blocks until the thread finishes or `timeout` elapses, matching `QThread::wait`.
    pub fn wait(&self, timeout: Duration) -> bool {
        self.completion.wait(timeout)
    }

    /// Signal emitted on the thread just before its event loop starts, matching `QThread::started`.
    pub fn started(&self) -> &Signal<()> {
        &self.started
    }

    /// Signal emitted on the thread as its event loop finishes, matching `QThread::finished`.
    pub fn finished(&self) -> &Signal<()> {
        &self.finished
    }

    /// Blocks until the thread's event loop exits and thread terminates.
    pub fn join(mut self) -> std::thread::Result<i32> {
        if let Some(h) = self.join_handle.take() {
            h.join()
        } else {
            panic!("event loop thread already joined");
        }
    }
}

impl Drop for EventLoopThreadHandle {
    fn drop(&mut self) {
        // Qt QThread::~QThread (qthread.cpp:465-476): if still running, wait for it or join.
        // If the thread was not manually joined, ask it to quit and wait/join so resources do not leak.
        if let Some(h) = self.join_handle.take() {
            if !self.is_finished() {
                self.exit(0);
            }
            let _ = h.join();
        }
    }
}
