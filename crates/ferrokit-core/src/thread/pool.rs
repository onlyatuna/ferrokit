// SPDX-License-Identifier: LGPL-3.0-only
use std::collections::VecDeque;
use std::panic::catch_unwind;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread::{Builder as StdThreadBuilder, JoinHandle};
use std::time::{Duration, Instant};

use crate::thread::task::{CancellationToken, TaskError, TaskHandle, TaskShared};

/// A queued unit of work. The closure is told whether it is being run (`true`) or discarded
/// without running (`false`), so a job dropped from the queue still completes its task handle.
struct Job {
    work: Option<Box<dyn FnOnce(bool) + Send + 'static>>,
}

impl Job {
    fn new(work: impl FnOnce(bool) + Send + 'static) -> Self {
        Self {
            work: Some(Box::new(work)),
        }
    }

    fn run(mut self) {
        if let Some(work) = self.work.take() {
            work(true);
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        if let Some(work) = self.work.take() {
            work(false);
        }
    }
}

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

impl PoolShared {
    /// Reserves as many new workers as the queue needs and the thread limit allows, and returns
    /// the id of the first one and how many. Must be called with `state` locked.
    ///
    /// A worker that exists but has not picked up a job yet counts as available, so a burst of
    /// submissions grows the pool by one worker per queued job instead of waiting for the first
    /// worker to become busy.
    fn reserve_workers(&self, state: &PoolState) -> (usize, usize) {
        let total = self.total_threads.load(Ordering::SeqCst);
        let available = total.saturating_sub(state.busy_workers);
        let wanted = state.queue.len().saturating_sub(available);
        let room = state.max_threads.saturating_sub(total);
        let count = wanted.min(room);
        self.total_threads.fetch_add(count, Ordering::SeqCst);
        (total + 1, count)
    }
}

/// A thread pool for executing tasks concurrently, modeled after Qt's `QThreadPool`.
pub struct ThreadPool {
    shared: Arc<PoolShared>,
    name_prefix: String,
    workers: Mutex<Vec<JoinHandle<()>>>,
}

impl Drop for ThreadPool {
    fn drop(&mut self) {
        let discarded = {
            let mut state = self.shared.state.lock().unwrap();
            state.shutdown = true;
            std::mem::take(&mut state.queue)
        };
        self.shared.work_cond.notify_all();
        // Dropping the jobs completes their handles as canceled. Done outside the pool lock
        // because that may wake arbitrary user code (async wakers).
        drop(discarded);

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
        let job = Job::new(move |run| {
            if !run {
                // Dropped from the queue (clear / pool teardown) without ever running.
                job_shared.cancel_unstarted();
                return;
            }
            // The Queued -> Running transition is atomic with respect to `TaskHandle::cancel`:
            // a task reported as canceled never runs, and a running one is never "canceled".
            if !job_shared.try_start() {
                return;
            }
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
        let (first_id, count) = {
            let mut state = self.shared.state.lock().unwrap();
            state.queue.push_back(job);
            self.shared.reserve_workers(&state)
        };

        for id in first_id..first_id + count {
            self.spawn_worker_thread(id);
        }

        self.shared.work_cond.notify_one();
    }

    fn spawn_worker_thread(&self, id: usize) {
        let pool = Arc::clone(&self.shared);
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
                    job.run();
                }

                {
                    let mut state = pool.state.lock().unwrap();
                    state.busy_workers -= 1;
                    if state.queue.is_empty() && state.busy_workers == 0 {
                        pool.done_cond.notify_all();
                    }
                }
            });

        match handle_res {
            Ok(handle) => self.workers.lock().unwrap().push(handle),
            // The slot reserved for this worker is free again; the next submission retries.
            Err(_) => {
                self.shared.total_threads.fetch_sub(1, Ordering::SeqCst);
            }
        }
    }

    /// Current maximum number of worker threads.
    pub fn max_thread_count(&self) -> usize {
        self.shared.state.lock().unwrap().max_threads
    }

    /// Sets the maximum number of worker threads.
    ///
    /// Raising the limit immediately starts workers for tasks that are already queued.
    pub fn set_max_thread_count(&self, count: usize) {
        let (first_id, spawn_count) = {
            let mut state = self.shared.state.lock().unwrap();
            state.max_threads = count.max(1);
            self.shared.reserve_workers(&state)
        };
        for id in first_id..first_id + spawn_count {
            self.spawn_worker_thread(id);
        }
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
    ///
    /// The handles of the discarded tasks complete as [`TaskError::Canceled`].
    pub fn clear_queue(&self) {
        let (discarded, idle) = {
            let mut state = self.shared.state.lock().unwrap();
            (std::mem::take(&mut state.queue), state.busy_workers == 0)
        };
        // Dropping the jobs completes their handles; done outside the lock because that may
        // wake arbitrary user code (async wakers).
        drop(discarded);
        if idle {
            // Nothing is running, so no worker will report completion: wake `wait_for_done`.
            self.shared.done_cond.notify_all();
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `clear_queue` with queued jobs and no busy worker must wake `wait_for_done` (the idle
    /// branch): no worker will report completion. The job is queued without starting a worker
    /// so that state is reachable deterministically.
    #[test]
    fn clear_queue_wakes_wait_for_done_when_idle() {
        let pool = Arc::new(ThreadPool::new(1));
        let task_shared = Arc::new(TaskShared::<()>::new(CancellationToken::new()));
        {
            let canceled = Arc::clone(&task_shared);
            let mut state = pool.shared.state.lock().unwrap();
            state.queue.push_back(Job::new(move |run| {
                assert!(!run);
                canceled.cancel_unstarted();
            }));
        }
        let waiter = {
            let pool = Arc::clone(&pool);
            std::thread::spawn(move || pool.wait_for_done_timeout(Duration::from_secs(10)))
        };
        // Give the waiter the chance to park before clearing.
        for _ in 0..1000 {
            std::thread::yield_now();
        }
        pool.clear_queue();
        assert!(
            waiter.join().unwrap(),
            "wait_for_done was not woken by clear_queue"
        );
        assert_eq!(
            *task_shared.status.lock().unwrap(),
            crate::thread::TaskStatus::Canceled
        );
    }
}
