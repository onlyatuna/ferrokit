// SPDX-License-Identifier: LGPL-3.0-only
use std::collections::VecDeque;
use std::panic::catch_unwind;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread::{Builder as StdThreadBuilder, JoinHandle};
use std::time::{Duration, Instant};

use crate::thread::task::{CancellationToken, TaskError, TaskHandle, TaskShared};

type Job = Box<dyn FnOnce() + Send + 'static>;

pub(crate) struct PoolState {
    max_threads: usize,
    busy_workers: usize,
    shutdown: bool,
    queue: VecDeque<Job>,
}

pub(crate) struct PoolShared {
    pub(crate) state: Mutex<PoolState>,
    pub(crate) work_cond: Condvar,
    pub(crate) done_cond: Condvar,
    pub(crate) total_threads: AtomicUsize,
}

/// A thread pool for executing tasks concurrently, modeled after Qt's `QThreadPool`.
pub struct ThreadPool {
    shared: Arc<PoolShared>,
    name_prefix: String,
    workers: Mutex<Vec<JoinHandle<()>>>,
}

impl Drop for ThreadPool {
    fn drop(&mut self) {
        {
            let mut state = self.shared.state.lock().unwrap();
            state.shutdown = true;
            state.queue.clear();
        }
        self.shared.work_cond.notify_all();

        let mut workers = self.workers.lock().unwrap();
        for worker in workers.drain(..) {
            let _ = worker.join();
        }
    }
}

static GLOBAL_POOL: OnceLock<ThreadPool> = OnceLock::new();

impl ThreadPool {
    /// Returns the global shared thread pool singleton, matching `QThreadPool::globalInstance()`.
    pub fn global() -> &'static ThreadPool {
        GLOBAL_POOL.get_or_init(|| {
            let cores = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4);
            ThreadPool::builder()
                .name_prefix("ferrokit-global-worker")
                .max_threads(cores.max(2))
                .build()
        })
    }

    /// Creates a new thread pool with the specified maximum worker threads.
    pub fn new(max_threads: usize) -> Self {
        Self::builder().max_threads(max_threads).build()
    }

    /// Creates a builder to configure and construct a thread pool.
    pub fn builder() -> ThreadPoolBuilder {
        ThreadPoolBuilder::new()
    }

    /// Submits a closure for execution in the thread pool, returning a [`TaskHandle`].
    pub fn spawn<F, T>(&self, task: F) -> TaskHandle<T>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        self.spawn_cancellable(|_| task())
    }

    /// Submits a cancellable closure for execution in the thread pool.
    pub fn spawn_cancellable<F, T>(&self, task: F) -> TaskHandle<T>
    where
        F: FnOnce(CancellationToken) -> T + Send + 'static,
        T: Send + 'static,
    {
        let cancel_token = CancellationToken::new();
        let shared = Arc::new(TaskShared::new(cancel_token.clone()));
        let handle = TaskHandle::new(Arc::clone(&shared));

        let job_shared = Arc::clone(&shared);
        let job = Box::new(move || {
            if cancel_token.is_canceled() {
                job_shared.finish(Err(TaskError::Canceled));
                return;
            }

            job_shared.mark_running();
            let result = catch_unwind(std::panic::AssertUnwindSafe(|| task(cancel_token)));
            match result {
                Ok(val) => job_shared.finish(Ok(val)),
                Err(err) => {
                    let msg = if let Some(s) = err.downcast_ref::<&str>() {
                        s.to_string()
                    } else if let Some(s) = err.downcast_ref::<String>() {
                        s.clone()
                    } else {
                        "unknown panic payload".to_string()
                    };
                    job_shared.finish(Err(TaskError::Panicked(msg)));
                }
            }
        });

        self.submit_job(job);
        handle
    }

    fn submit_job(&self, job: Job) {
        let mut spawn_worker = false;
        {
            let mut state = self.shared.state.lock().unwrap();
            state.queue.push_back(job);

            let total = self.shared.total_threads.load(Ordering::SeqCst);
            if state.busy_workers >= total && total < state.max_threads {
                spawn_worker = true;
                self.shared.total_threads.fetch_add(1, Ordering::SeqCst);
            }
        }

        if spawn_worker {
            self.spawn_worker_thread();
        }

        self.shared.work_cond.notify_one();
    }

    fn spawn_worker_thread(&self) {
        let pool = Arc::clone(&self.shared);
        let id = self.shared.total_threads.load(Ordering::SeqCst);
        let thread_name = format!("{}-{}", self.name_prefix, id);

        let handle_res = StdThreadBuilder::new()
            .name(thread_name)
            .spawn(move || loop {
                let job = {
                    let mut state = pool.state.lock().unwrap();
                    while state.queue.is_empty() && !state.shutdown {
                        state = pool.work_cond.wait(state).unwrap();
                    }

                    if state.shutdown {
                        break;
                    }

                    state.busy_workers += 1;
                    state.queue.pop_front()
                };

                if let Some(job) = job {
                    job();
                }

                {
                    let mut state = pool.state.lock().unwrap();
                    state.busy_workers -= 1;
                    if state.queue.is_empty() && state.busy_workers == 0 {
                        pool.done_cond.notify_all();
                    }
                }
            });

        if let Ok(handle) = handle_res {
            self.workers.lock().unwrap().push(handle);
        }
    }

    /// Current maximum number of worker threads.
    pub fn max_thread_count(&self) -> usize {
        self.shared.state.lock().unwrap().max_threads
    }

    /// Sets the maximum number of worker threads.
    pub fn set_max_thread_count(&self, count: usize) {
        let mut state = self.shared.state.lock().unwrap();
        state.max_threads = count.max(1);
    }

    /// Returns the number of currently active (busy) worker threads.
    pub fn active_thread_count(&self) -> usize {
        self.shared.state.lock().unwrap().busy_workers
    }

    /// Returns the number of queued tasks waiting for workers.
    pub fn queued_task_count(&self) -> usize {
        self.shared.state.lock().unwrap().queue.len()
    }

    /// Clears all unstarted tasks from the queue, matching `QThreadPool::clear()`.
    pub fn clear_queue(&self) {
        let mut state = self.shared.state.lock().unwrap();
        state.queue.clear();
    }

    /// Blocks until all queued and executing tasks have completed.
    pub fn wait_for_done(&self) -> bool {
        let mut state = self.shared.state.lock().unwrap();
        while !state.queue.is_empty() || state.busy_workers > 0 {
            state = self.shared.done_cond.wait(state).unwrap();
        }
        true
    }

    /// Blocks until all tasks complete or `timeout` expires.
    pub fn wait_for_done_timeout(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut state = self.shared.state.lock().unwrap();

        while !state.queue.is_empty() || state.busy_workers > 0 {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            let remaining = deadline - now;
            let (next_state, timeout_res) = self
                .shared
                .done_cond
                .wait_timeout(state, remaining)
                .unwrap();
            state = next_state;
            if timeout_res.timed_out() && (!state.queue.is_empty() || state.busy_workers > 0) {
                return false;
            }
        }
        true
    }
}

/// A builder for constructing configured [`ThreadPool`] instances.
pub struct ThreadPoolBuilder {
    max_threads: usize,
    name_prefix: String,
}

impl Default for ThreadPoolBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ThreadPoolBuilder {
    /// Creates a default builder.
    pub fn new() -> Self {
        let default_cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        Self {
            max_threads: default_cores.max(2),
            name_prefix: "ferrokit-worker".to_string(),
        }
    }

    /// Sets the maximum number of worker threads.
    pub fn max_threads(mut self, max: usize) -> Self {
        self.max_threads = max.max(1);
        self
    }

    /// Sets worker thread name prefix.
    pub fn name_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.name_prefix = prefix.into();
        self
    }

    /// Builds the configured [`ThreadPool`].
    pub fn build(self) -> ThreadPool {
        let shared = Arc::new(PoolShared {
            state: Mutex::new(PoolState {
                max_threads: self.max_threads,
                busy_workers: 0,
                shutdown: false,
                queue: VecDeque::new(),
            }),
            work_cond: Condvar::new(),
            done_cond: Condvar::new(),
            total_threads: AtomicUsize::new(0),
        });

        ThreadPool {
            shared,
            name_prefix: self.name_prefix,
            workers: Mutex::new(Vec::new()),
        }
    }
}
