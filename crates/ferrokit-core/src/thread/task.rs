// SPDX-License-Identifier: LGPL-3.0-only
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

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
}

impl fmt::Display for TaskError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Canceled => write!(f, "task was canceled"),
            Self::Panicked(msg) => write!(f, "task panicked: {}", msg),
            Self::Timeout => write!(f, "task wait timed out"),
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
    pub(crate) cancel_token: CancellationToken,
    pub(crate) wakers: Mutex<Vec<Waker>>,
}

impl<T> TaskShared<T> {
    pub(crate) fn new(cancel_token: CancellationToken) -> Self {
        Self {
            status: Mutex::new(TaskStatus::Queued),
            cond: Condvar::new(),
            result: Mutex::new(None),
            cancel_token,
            wakers: Mutex::new(Vec::new()),
        }
    }

    pub(crate) fn mark_running(&self) {
        let mut st = self.status.lock().unwrap();
        if *st == TaskStatus::Queued {
            *st = TaskStatus::Running;
        }
    }

    pub(crate) fn finish(&self, res: Result<T, TaskError>) {
        {
            let mut st = self.status.lock().unwrap();
            *st = match &res {
                Ok(_) => TaskStatus::Completed,
                Err(TaskError::Canceled) => TaskStatus::Canceled,
                Err(TaskError::Panicked(_)) | Err(TaskError::Timeout) => TaskStatus::Failed,
            };
            let mut r = self.result.lock().unwrap();
            *r = Some(res);
        }
        self.cond.notify_all();

        let wakers = {
            let mut w = self.wakers.lock().unwrap();
            std::mem::take(&mut *w)
        };
        for waker in wakers {
            waker.wake();
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

    /// Requests cooperative cancellation of the task. Returns `true` if canceled.
    pub fn cancel(&self) -> bool {
        self.shared.cancel_token.cancel();
        let mut st = self.shared.status.lock().unwrap();
        if *st == TaskStatus::Queued {
            *st = TaskStatus::Canceled;
            let mut r = self.shared.result.lock().unwrap();
            *r = Some(Err(TaskError::Canceled));
            self.shared.cond.notify_all();
            true
        } else {
            false
        }
    }

    /// Returns the cancellation token associated with this task.
    pub fn cancellation_token(&self) -> CancellationToken {
        self.shared.cancel_token.clone()
    }

    /// Blocks until the task completes and returns a copy/clone of the result if `T: Clone`.
    pub fn wait_result(&self) -> Result<T, TaskError>
    where
        T: Clone,
    {
        let mut res = self.shared.result.lock().unwrap();
        while res.is_none() {
            res = self.shared.cond.wait(res).unwrap();
        }
        res.as_ref().unwrap().clone()
    }

    /// Blocks until the task completes within `timeout`.
    pub fn wait_timeout(&self, timeout: Duration) -> Result<Option<T>, TaskError>
    where
        T: Clone,
    {
        let deadline = Instant::now() + timeout;
        let mut res = self.shared.result.lock().unwrap();

        while res.is_none() {
            let now = Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            let remaining = deadline - now;
            let (next_res, timeout_res) = self.shared.cond.wait_timeout(res, remaining).unwrap();
            res = next_res;
            if timeout_res.timed_out() && res.is_none() {
                return Ok(None);
            }
        }

        res.as_ref().unwrap().clone().map(Some)
    }

    /// Consumes the task handle, blocking until completion and taking the owned result.
    pub fn join(self) -> Result<T, TaskError> {
        let mut res = self.shared.result.lock().unwrap();
        while res.is_none() {
            res = self.shared.cond.wait(res).unwrap();
        }
        res.take().unwrap()
    }
}

impl<T: Clone> Future for TaskHandle<T> {
    type Output = Result<T, TaskError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let res = self.shared.result.lock().unwrap();
        if let Some(r) = res.as_ref() {
            Poll::Ready(r.clone())
        } else {
            let mut wakers = self.shared.wakers.lock().unwrap();
            if !wakers.iter().any(|w| w.will_wake(cx.waker())) {
                wakers.push(cx.waker().clone());
            }
            Poll::Pending
        }
    }
}
