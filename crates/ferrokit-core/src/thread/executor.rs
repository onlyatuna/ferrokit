// SPDX-License-Identifier: LGPL-3.0-only
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};
use std::thread::{self, Thread as StdThread};

use crate::thread::pool::{enter_blocking, ThreadPool};
use crate::thread::task::TaskHandle;

struct ThreadWaker {
    thread: StdThread,
    notified: AtomicBool,
}

impl ThreadWaker {
    fn new() -> Self {
        Self {
            thread: thread::current(),
            notified: AtomicBool::new(false),
        }
    }

    fn wake(&self) {
        if !self.notified.swap(true, Ordering::SeqCst) {
            self.thread.unpark();
        }
    }
}

fn create_thread_waker(waker: Arc<ThreadWaker>) -> Waker {
    unsafe fn clone_waker(data: *const ()) -> RawWaker {
        let arc = Arc::from_raw(data as *const ThreadWaker);
        std::mem::forget(Arc::clone(&arc));
        std::mem::forget(arc);
        RawWaker::new(data, &VTABLE)
    }

    unsafe fn wake(data: *const ()) {
        let arc = Arc::from_raw(data as *const ThreadWaker);
        arc.wake();
    }

    unsafe fn wake_by_ref(data: *const ()) {
        let arc = &*(data as *const ThreadWaker);
        arc.wake();
    }

    unsafe fn drop_waker(data: *const ()) {
        drop(Arc::from_raw(data as *const ThreadWaker));
    }

    static VTABLE: RawWakerVTable = RawWakerVTable::new(clone_waker, wake, wake_by_ref, drop_waker);

    let ptr = Arc::into_raw(waker) as *const ();
    unsafe { Waker::from_raw(RawWaker::new(ptr, &VTABLE)) }
}

/// Runs a Rust asynchronous `Future` to completion on the current thread, blocking until done.
pub fn block_on<F: Future>(mut future: F) -> F::Output {
    let mut pinned = unsafe { Pin::new_unchecked(&mut future) };
    let thread_waker = Arc::new(ThreadWaker::new());
    let waker = create_thread_waker(Arc::clone(&thread_waker));
    let mut cx = Context::from_waker(&waker);

    loop {
        match pinned.as_mut().poll(&mut cx) {
            Poll::Ready(val) => return val,
            Poll::Pending => {
                if !thread_waker.notified.swap(false, Ordering::SeqCst) {
                    // About to park: if this is a pool worker, free its slot so jobs queued
                    // behind it (possibly the very ones this future awaits) can run.
                    let _blocked = enter_blocking();
                    while !thread_waker.notified.swap(false, Ordering::SeqCst) {
                        thread::park();
                    }
                }
            }
        }
    }
}

/// Spawns a Rust `Future` onto the global thread pool, returning a [`TaskHandle`].
pub fn spawn_async<F>(future: F) -> TaskHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    ThreadPool::global().spawn(|| block_on(future))
}

/// An asynchronous executor managing future execution across thread pools or local thread contexts.
pub struct Executor {
    pool: Option<Arc<ThreadPool>>,
}

impl Default for Executor {
    fn default() -> Self {
        Self::new()
    }
}

impl Executor {
    /// Creates an executor using the global thread pool.
    pub fn new() -> Self {
        Self { pool: None }
    }

    /// Creates an executor with a custom thread pool.
    pub fn with_pool(pool: Arc<ThreadPool>) -> Self {
        Self { pool: Some(pool) }
    }

    /// Spawns a future on this executor's thread pool.
    pub fn spawn<F>(&self, future: F) -> TaskHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        if let Some(pool) = &self.pool {
            pool.spawn(|| block_on(future))
        } else {
            spawn_async(future)
        }
    }

    /// Runs a future synchronously to completion on the current thread.
    pub fn run_local<F>(&self, future: F) -> F::Output
    where
        F: Future,
    {
        block_on(future)
    }
}
