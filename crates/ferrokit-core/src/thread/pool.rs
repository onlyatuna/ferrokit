// SPDX-License-Identifier: LGPL-3.0-only
use std::cell::RefCell;
use std::collections::VecDeque;
use std::panic::catch_unwind;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::thread::{Builder as StdThreadBuilder, JoinHandle};
use std::time::{Duration, Instant};

use crate::thread::task::{CancellationToken, TaskError, TaskHandle, TaskShared};

/// A queued unit of work. The closure is told whether it is being run (`true`) or discarded
/// without running (`false`), so a job dropped from the queue still completes its task handle.
struct Job {
    /// Identifies the job in its pool's queue (see [`JobRef`]).
    id: u64,
    work: Option<Box<dyn FnOnce(bool) + Send + 'static>>,
}

/// Returns a job id never handed out before.
fn next_job_id() -> u64 {
    static NEXT_JOB_ID: AtomicU64 = AtomicU64::new(0);
    NEXT_JOB_ID.fetch_add(1, Ordering::Relaxed)
}

impl Job {
    fn new(id: u64, work: impl FnOnce(bool) + Send + 'static) -> Self {
        Self {
            id,
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
    /// Busy workers currently parked in `block_on`. Up to `max_threads` of them do not count
    /// against `max_threads` (see [`PoolState::capacity`]).
    blocked_workers: usize,
    shutdown: bool,
    queue: VecDeque<Job>,
}

impl PoolState {
    /// How many workers may exist right now: `max_threads`, plus one replacement per worker
    /// parked in `block_on`, but never more than `max_threads` replacements. The hard cap keeps
    /// jobs that all block (on I/O, say) from growing the pool to one OS thread per job.
    fn capacity(&self) -> usize {
        self.max_threads + self.blocked_workers.min(self.max_threads)
    }
}

pub(crate) struct PoolShared {
    pub(crate) state: Mutex<PoolState>,
    pub(crate) work_cond: Condvar,
    pub(crate) done_cond: Condvar,
    pub(crate) total_threads: AtomicUsize,
    /// The id the next worker thread is named with.
    next_worker_id: AtomicUsize,
    name_prefix: String,
    workers: Mutex<Vec<JoinHandle<()>>>,
}

thread_local! {
    /// The pool whose worker the current thread is, if any.
    static WORKER_POOL: RefCell<Option<Weak<PoolShared>>> = const { RefCell::new(None) };
    /// While `block_on` polls its future: the queued pool jobs that future is awaiting.
    static AWAITED_JOBS: RefCell<Option<Vec<JobRef>>> = const { RefCell::new(None) };
}

/// Marks the current pool worker as parked in `block_on` for as long as it lives.
pub(crate) struct BlockedWorker(Arc<PoolShared>);

/// If the current thread is a pool worker, releases its slot for the duration of the returned
/// guard and starts replacement workers for queued jobs, within [`PoolState::capacity`].
pub(crate) fn enter_blocking() -> Option<BlockedWorker> {
    let shared = WORKER_POOL.with(|pool| pool.borrow().as_ref().and_then(Weak::upgrade))?;
    let (first_id, count) = {
        let mut state = shared.state.lock().unwrap();
        state.blocked_workers += 1;
        shared.reserve_workers(&state)
    };
    for id in first_id..first_id + count {
        shared.spawn_worker_thread(id);
    }
    Some(BlockedWorker(shared))
}

impl Drop for BlockedWorker {
    fn drop(&mut self) {
        let surplus = {
            let mut state = self.0.state.lock().unwrap();
            state.blocked_workers -= 1;
            self.0.has_surplus_workers(&state)
        };
        if surplus {
            // Idle workers beyond the limit notice this and retire.
            self.0.work_cond.notify_all();
        }
    }
}

/// The pool job behind a [`TaskHandle`] returned by [`ThreadPool::spawn`].
#[derive(Clone)]
pub(crate) struct JobRef {
    pool: Weak<PoolShared>,
    id: u64,
}

/// Records that the future `block_on` is polling awaits `job`, which has not started yet.
/// Outside a `block_on` poll this does nothing.
pub(crate) fn note_awaited_job(job: &JobRef) {
    AWAITED_JOBS.with(|jobs| {
        if let Some(jobs) = jobs.borrow_mut().as_mut() {
            jobs.push(job.clone());
        }
    });
}

/// Runs `poll` and returns its result with the jobs it awaited (see [`note_awaited_job`]).
pub(crate) fn poll_noting_awaited_jobs<R>(poll: impl FnOnce() -> R) -> (R, Vec<JobRef>) {
    /// Restores the enclosing `block_on`'s list (`None` outside one), also if `poll` panics.
    struct Restore(Option<Vec<JobRef>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let outer = self.0.take();
            AWAITED_JOBS.with(|jobs| *jobs.borrow_mut() = outer);
        }
    }

    let _restore = Restore(AWAITED_JOBS.with(|jobs| jobs.replace(Some(Vec::new()))));
    let result = poll();
    let awaited = AWAITED_JOBS
        .with(|jobs| jobs.borrow_mut().take())
        .unwrap_or_default();
    (result, awaited)
}

/// Removes `job` from its pool's queue so the current thread can run it inline, like
/// `QThreadPool::tryTake`. Only done on a worker of that same pool, and only while the job is
/// still queued.
pub(crate) fn steal_awaited_job(job: &JobRef) -> Option<InlineJob> {
    let on_own_worker = WORKER_POOL.with(|pool| {
        pool.borrow()
            .as_ref()
            .is_some_and(|pool| Weak::ptr_eq(pool, &job.pool))
    });
    if !on_own_worker {
        return None;
    }
    let shared = job.pool.upgrade()?;
    let mut state = shared.state.lock().unwrap();
    let pos = state.queue.iter().position(|queued| queued.id == job.id)?;
    state.queue.remove(pos).map(InlineJob)
}

/// A queued job taken by the worker awaiting it, to be run on that worker's stack.
pub(crate) struct InlineJob(Job);

impl InlineJob {
    /// Runs the job on the current worker, which stays busy (it is not parked) meanwhile.
    pub(crate) fn run(self) {
        // As in the worker loop: a panicking waker/completion must not unwind into the
        // `block_on` that is running this job.
        let _ = catch_unwind(std::panic::AssertUnwindSafe(|| self.0.run()));
    }
}

impl PoolShared {
    /// Whether more workers exist than the current capacity allows.
    fn has_surplus_workers(&self, state: &PoolState) -> bool {
        self.total_threads.load(Ordering::SeqCst) > state.capacity()
    }

    /// Reserves as many new workers as the queue needs and the thread limit allows, and returns
    /// the id of the first one and how many. Must be called with `state` locked.
    ///
    /// A worker that exists but has not picked up a job yet counts as available, so a burst of
    /// submissions grows the pool by one worker per queued job instead of waiting for the first
    /// worker to become busy. Workers parked in `block_on` raise the limit by one each, up to
    /// `max_threads` extra workers in total.
    fn reserve_workers(&self, state: &PoolState) -> (usize, usize) {
        let total = self.total_threads.load(Ordering::SeqCst);
        let available = total.saturating_sub(state.busy_workers);
        let wanted = state.queue.len().saturating_sub(available);
        let room = state.capacity().saturating_sub(total);
        let count = wanted.min(room);
        self.total_threads.fetch_add(count, Ordering::SeqCst);
        // Ids are never handed out twice: a worker that retired or failed to start leaves its
        // id behind, so no two workers (live or not) share a thread name.
        let first_id = self.next_worker_id.fetch_add(count, Ordering::SeqCst);
        (first_id, count)
    }

    fn spawn_worker_thread(self: &Arc<Self>, id: usize) {
        let pool = Arc::clone(self);
        let thread_name = format!("{}-{}", self.name_prefix, id);

        let handle_res = StdThreadBuilder::new().name(thread_name).spawn(move || {
            WORKER_POOL.with(|slot| *slot.borrow_mut() = Some(Arc::downgrade(&pool)));
            loop {
                let job = {
                    let mut state = pool.state.lock().unwrap();
                    let mut retire = false;
                    while state.queue.is_empty() && !state.shutdown {
                        if pool.has_surplus_workers(&state) {
                            retire = true;
                            break;
                        }
                        state = pool.work_cond.wait(state).unwrap();
                    }

                    if retire {
                        pool.total_threads.fetch_sub(1, Ordering::SeqCst);
                        break;
                    }
                    if state.shutdown {
                        break;
                    }

                    state.busy_workers += 1;
                    state.queue.pop_front()
                };

                if let Some(job) = job {
                    // A panicking waker/completion must not skip the accounting below,
                    // or `wait_for_done` would hang; the worker stays alive.
                    let _ = catch_unwind(std::panic::AssertUnwindSafe(|| job.run()));
                }

                {
                    let mut state = pool.state.lock().unwrap();
                    state.busy_workers -= 1;
                    if state.queue.is_empty() && state.busy_workers == 0 {
                        pool.done_cond.notify_all();
                    }
                }
            }
        });

        match handle_res {
            Ok(handle) => {
                let mut workers = self.workers.lock().unwrap();
                // Checked under the `workers` lock: `Drop` sets `shutdown` before it takes the
                // list, so a handle pushed while `shutdown` is unset is joined by `Drop`.
                if self.state.lock().unwrap().shutdown {
                    drop(workers);
                    // `Drop` has taken (or is taking) the list without this handle. The pool
                    // has shut down, so the new worker exits at once: join it here instead.
                    let _ = handle.join();
                    return;
                }
                // Retired workers leave finished handles behind.
                workers.retain(|worker| !worker.is_finished());
                workers.push(handle);
            }
            // The slot reserved for this worker is free again; the next submission retries.
            Err(_) => {
                self.total_threads.fetch_sub(1, Ordering::SeqCst);
            }
        }
    }
}

/// A thread pool for executing tasks concurrently, modeled after Qt's `QThreadPool`.
///
/// A worker that parks in [`block_on`](crate::thread::block_on) (for example an async task
/// awaiting another task of the same pool) does not occupy one of the `max_thread_count`
/// slots while it waits: the pool temporarily starts an extra worker in its place, so work
/// queued behind blocked workers can still run. At most `max_thread_count` such extra workers
/// exist at a time, so the pool never runs more than twice `max_thread_count` threads.
///
/// A task on a worker that awaits a task of the same pool which has not started yet takes that
/// task out of the queue and runs it itself, inline, then re-polls its future: like Qt's
/// `QFuture::waitForFinished`, which steals the awaited runnable with `QThreadPool::tryTake`.
/// [`TaskHandle::wait_result`], [`TaskHandle::wait_timeout`] and [`TaskHandle::join`] called
/// from a job of the same pool do the same. So tasks awaiting tasks of their own pool complete
/// however many there are, and the nesting depth follows the program's own chain of awaits.
/// Only the awaited task runs inline, never unrelated queued jobs; it runs on the awaiting
/// worker's thread (and sees its thread-locals).
///
/// Waiting for anything else is not helped this way: when a worker waits for I/O, for a channel,
/// or for a task that is already running, the pool cannot know which queued job would unblock
/// it. A worker parked in `block_on` or in one of those task waits frees its slot meanwhile
/// (within the cap above), so a queued job that feeds a channel it waits on can still run, but
/// once the cap is reached queued jobs wait for a worker to become free. The extra workers
/// retire once they are idle and the blocked ones have resumed.
pub struct ThreadPool {
    shared: Arc<PoolShared>,
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

        // The last owner can be one of the workers (a task holding the final `Arc`): it must
        // not join itself, it exits on its own once the task returns.
        let current = std::thread::current().id();
        // Taken out so that a worker still starting replacements (`block_on`) cannot deadlock
        // against the join below.
        let workers = std::mem::take(&mut *self.shared.workers.lock().unwrap());
        for worker in workers {
            if worker.thread().id() != current {
                let _ = worker.join();
            }
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
        let job_id = next_job_id();
        let shared = Arc::new(TaskShared::new(
            cancel_token.clone(),
            Some(JobRef {
                pool: Arc::downgrade(&self.shared),
                id: job_id,
            }),
        ));
        let handle = TaskHandle::new(Arc::clone(&shared));

        let job_shared = Arc::clone(&shared);
        let job = Job::new(job_id, move |run| {
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
            self.shared.spawn_worker_thread(id);
        }

        self.shared.work_cond.notify_one();
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
            self.shared.spawn_worker_thread(id);
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
                blocked_workers: 0,
                shutdown: false,
                queue: VecDeque::new(),
            }),
            work_cond: Condvar::new(),
            done_cond: Condvar::new(),
            total_threads: AtomicUsize::new(0),
            next_worker_id: AtomicUsize::new(1),
            name_prefix: self.name_prefix,
            workers: Mutex::new(Vec::new()),
        });

        ThreadPool { shared }
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
        let task_shared = Arc::new(TaskShared::<()>::new(CancellationToken::new(), None));
        {
            let canceled = Arc::clone(&task_shared);
            let mut state = pool.shared.state.lock().unwrap();
            state.queue.push_back(Job::new(next_job_id(), move |run| {
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

    /// A worker that retired or failed to start gives its slot back, not its id: the next
    /// workers get names that no earlier worker had.
    #[test]
    fn reserved_worker_ids_are_never_reused() {
        let pool = ThreadPool::new(2);
        let mut state = pool.shared.state.lock().unwrap();
        for _ in 0..2 {
            state.queue.push_back(Job::new(next_job_id(), |_| {}));
        }

        let (first, count) = pool.shared.reserve_workers(&state);
        assert_eq!((first, count), (1, 2));
        // Both of them retire (or never started).
        pool.shared.total_threads.fetch_sub(2, Ordering::SeqCst);

        let (again, count) = pool.shared.reserve_workers(&state);
        assert_eq!(count, 2);
        assert!(
            again >= first + 2,
            "worker ids {first}.. were handed out again from {again}"
        );
    }
}
