// SPDX-License-Identifier: LGPL-3.0-only
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use crate::thread::pool::{
    BlockedWorker, JobRef, enter_blocking, note_awaited_job, steal_awaited_job,
};

/// Status of a task in its execution lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskStatus {
    /// Queued and waiting for an available worker thread.
    Queued,
    /// Currently executing on a worker thread.
    Running,
    /// Completed successfully with a result.
    Completed,
    /// Canceled before or during execution.
    Canceled,
    /// Terminated abnormally due to a panic.
    Failed,
}

/// Errors that may occur when joining or waiting for a task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskError {
    /// The task was canceled before or during execution.
    Canceled,
    /// The task panicked during execution.
    Panicked(String),
    /// Operation timed out.
    Timeout,
    /// [`TaskHandle::join`] on another handle to the same task already moved the result out.
    ResultTaken,
}

impl fmt::Display for TaskError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Canceled => write!(f, "task was canceled"),
            Self::Panicked(msg) => write!(f, "task panicked: {}", msg),
            Self::Timeout => write!(f, "task wait timed out"),
            Self::ResultTaken => write!(f, "task result was already taken by join"),
        }
    }
}

impl std::error::Error for TaskError {}

/// A cooperative cancellation token passed into cancellable tasks.
#[derive(Clone, Default)]
pub struct CancellationToken {
    canceled: Arc<AtomicBool>,
}

impl CancellationToken {
    /// Creates a new cancellation token in the active state.
    pub fn new() -> Self {
        Self {
            canceled: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Requests cancellation of the task.
    pub fn cancel(&self) {
        self.canceled.store(true, Ordering::SeqCst);
    }

    /// Returns `true` if cancellation has been requested.
    pub fn is_canceled(&self) -> bool {
        self.canceled.load(Ordering::SeqCst)
    }
}

pub(crate) struct TaskShared<T> {
    pub(crate) status: Mutex<TaskStatus>,
    pub(crate) cond: Condvar,
    pub(crate) result: Mutex<Option<Result<T, TaskError>>>,
    /// Set (under `result`) once `join` moved the result out.
    pub(crate) taken: AtomicBool,
    pub(crate) cancel_token: CancellationToken,
    pub(crate) wakers: Mutex<Vec<Waker>>,
    /// The pool job that runs this task, if it was spawned on a [`ThreadPool`]. Lets a worker
    /// of that pool awaiting the task run it inline while it is still queued.
    ///
    /// [`ThreadPool`]: crate::thread::ThreadPool
    pub(crate) queued_job: Option<JobRef>,
}

impl<T> TaskShared<T> {
    pub(crate) fn new(cancel_token: CancellationToken, queued_job: Option<JobRef>) -> Self {
        Self {
            status: Mutex::new(TaskStatus::Queued),
            cond: Condvar::new(),
            result: Mutex::new(None),
            taken: AtomicBool::new(false),
            cancel_token,
            wakers: Mutex::new(Vec::new()),
            queued_job,
        }
    }

    /// Moves the task from `Queued` to `Running`. Returns `false` if the task must not run:
    /// it was already canceled while queued, or its token was canceled before it started (in
    /// which case it is completed as canceled here).
    pub(crate) fn try_start(&self) -> bool {
        let mut st = self.status.lock().unwrap();
        if *st != TaskStatus::Queued {
            return false;
        }
        if self.cancel_token.is_canceled() {
            self.settle(st, Err(TaskError::Canceled));
            return false;
        }
        *st = TaskStatus::Running;
        true
    }

    /// Completes a task that was dropped from a queue without running as canceled, so nobody
    /// waiting on it is left blocked.
    pub(crate) fn cancel_unstarted(&self) {
        let st = self.status.lock().unwrap();
        if *st == TaskStatus::Queued {
            self.settle(st, Err(TaskError::Canceled));
        }
    }

    pub(crate) fn finish(&self, res: Result<T, TaskError>) {
        let st = self.status.lock().unwrap();
        self.settle(st, res);
    }

    /// Publishes the final status and result, then wakes blocked waiters and polling tasks.
    fn settle(&self, mut st: MutexGuard<'_, TaskStatus>, res: Result<T, TaskError>) {
        *st = match &res {
            Ok(_) => TaskStatus::Completed,
            Err(TaskError::Canceled) => TaskStatus::Canceled,
            Err(TaskError::Panicked(_) | TaskError::Timeout | TaskError::ResultTaken) => {
                TaskStatus::Failed
            }
        };
        *self.result.lock().unwrap() = Some(res);
        drop(st);
        self.cond.notify_all();

        let wakers = {
            let mut w = self.wakers.lock().unwrap();
            std::mem::take(&mut *w)
        };
        for waker in wakers {
            // One panicking waker must not leave the remaining ones unwoken.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| waker.wake()));
        }
    }
}

/// A handle to a scheduled asynchronous task or thread computation.
///
/// Implements both synchronous blocking queries (`wait`, `join`) and
/// asynchronous polling (`std::future::Future`).
pub struct TaskHandle<T> {
    pub(crate) shared: Arc<TaskShared<T>>,
}

impl<T> Clone for TaskHandle<T> {
    fn clone(&self) -> Self {
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl<T> TaskHandle<T> {
    pub(crate) fn new(shared: Arc<TaskShared<T>>) -> Self {
        Self { shared }
    }

    /// Current execution status of the task.
    pub fn status(&self) -> TaskStatus {
        *self.shared.status.lock().unwrap()
    }

    /// Whether the task has finished executing (completed, canceled, or failed).
    pub fn is_finished(&self) -> bool {
        matches!(
            self.status(),
            TaskStatus::Completed | TaskStatus::Canceled | TaskStatus::Failed
        )
    }

    /// Whether the task was canceled.
    pub fn is_canceled(&self) -> bool {
        self.shared.cancel_token.is_canceled() || self.status() == TaskStatus::Canceled
    }

    /// Requests cooperative cancellation of the task. Returns `true` if the task was still queued
    /// and is now canceled without ever running; `false` if it had already started (it can only
    /// stop cooperatively through its [`CancellationToken`]) or had finished.
    pub fn cancel(&self) -> bool {
        // The token is set under the status lock so that a worker deciding whether to start the
        // task (see `TaskShared::try_start`) sees "canceled" exactly when `cancel` reports it.
        let st = self.shared.status.lock().unwrap();
        self.shared.cancel_token.cancel();
        if *st == TaskStatus::Queued {
            self.shared.settle(st, Err(TaskError::Canceled));
            true
        } else {
            false
        }
    }

    /// Returns the cancellation token associated with this task.
    pub fn cancellation_token(&self) -> CancellationToken {
        self.shared.cancel_token.clone()
    }

    /// Before a synchronous wait: on a worker of the task's own pool, takes the task out of the
    /// queue if it has not started and runs it here, as `QFuture::waitForFinished` does through
    /// `QThreadPool::tryTake`; otherwise, if the task is still unfinished, frees the worker's
    /// slot until the returned guard is dropped (as `block_on` does), so jobs queued behind the
    /// waiting one can run. The guard must outlive the wait.
    fn prepare_to_wait(&self) -> Option<BlockedWorker> {
        if let Some(job) = self.shared.queued_job.as_ref().and_then(steal_awaited_job) {
            job.run();
        }
        if self.is_settled() {
            return None;
        }
        enter_blocking()
    }

    /// Whether the result is there or has been moved out by `join`.
    fn is_settled(&self) -> bool {
        self.shared.result.lock().unwrap().is_some() || self.shared.taken.load(Ordering::SeqCst)
    }

    /// Blocks until the task completes and returns a copy/clone of the result if `T: Clone`.
    ///
    /// Inside a job of the task's own [`ThreadPool`](crate::thread::ThreadPool) a task that is
    /// still queued is run by the waiting job itself, instead of waiting for a free worker.
    pub fn wait_result(&self) -> Result<T, TaskError>
    where
        T: Clone,
    {
        let _blocked = self.prepare_to_wait();
        let mut res = self.shared.result.lock().unwrap();
        while res.is_none() && !self.shared.taken.load(Ordering::SeqCst) {
            res = self.shared.cond.wait(res).unwrap();
        }
        match res.as_ref() {
            Some(r) => r.clone(),
            None => Err(TaskError::ResultTaken),
        }
    }

    /// Blocks until the task completes within `timeout`.
    ///
    /// Inside a job of the task's own [`ThreadPool`](crate::thread::ThreadPool) a task that is
    /// still queued is run by the waiting job itself first. That run is not cut short by
    /// `timeout`: it can last past it, and the call then returns the task's result.
    pub fn wait_timeout(&self, timeout: Duration) -> Result<Option<T>, TaskError>
    where
        T: Clone,
    {
        let deadline = Instant::now() + timeout;
        let _blocked = self.prepare_to_wait();
        let mut res = self.shared.result.lock().unwrap();

        while res.is_none() && !self.shared.taken.load(Ordering::SeqCst) {
            let now = Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            let remaining = deadline - now;
            let (next_res, timeout_res) = self.shared.cond.wait_timeout(res, remaining).unwrap();
            res = next_res;
            if timeout_res.timed_out() && res.is_none() && !self.shared.taken.load(Ordering::SeqCst)
            {
                return Ok(None);
            }
        }

        match res.as_ref() {
            Some(r) => r.clone().map(Some),
            None => Err(TaskError::ResultTaken),
        }
    }

    /// Consumes the task handle, blocking until completion and taking the owned result.
    ///
    /// The result is moved out, so other clones of the handle observe
    /// [`TaskError::ResultTaken`] instead of the value.
    ///
    /// Inside a job of the task's own [`ThreadPool`](crate::thread::ThreadPool) a task that is
    /// still queued is run by the joining job itself, instead of waiting for a free worker.
    pub fn join(self) -> Result<T, TaskError> {
        let _blocked = self.prepare_to_wait();
        let mut res = self.shared.result.lock().unwrap();
        while res.is_none() && !self.shared.taken.load(Ordering::SeqCst) {
            res = self.shared.cond.wait(res).unwrap();
        }
        match res.take() {
            Some(r) => {
                self.shared.taken.store(true, Ordering::SeqCst);
                r
            }
            None => Err(TaskError::ResultTaken),
        }
    }
}

impl<T: Clone> Future for TaskHandle<T> {
    type Output = Result<T, TaskError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        {
            let res = self.shared.result.lock().unwrap();
            if let Some(r) = res.as_ref() {
                return Poll::Ready(r.clone());
            }
            if self.shared.taken.load(Ordering::SeqCst) {
                return Poll::Ready(Err(TaskError::ResultTaken));
            }
            let mut wakers = self.shared.wakers.lock().unwrap();
            if !wakers.iter().any(|w| w.will_wake(cx.waker())) {
                wakers.push(cx.waker().clone());
            }
        }
        // Checked with `result` released: `settle` takes `status` before `result`.
        if let Some(job) = &self.shared.queued_job
            && *self.shared.status.lock().unwrap() == TaskStatus::Queued
        {
            // Lets `block_on` on a worker of the same pool run the task inline.
            note_awaited_job(job);
        }
        Poll::Pending
    }
}
