// SPDX-License-Identifier: LGPL-3.0-only
//! Comprehensive modern Rust threading and concurrency subsystem for `ferrokit-core`.
//!
//! Replaces legacy C++ Qt threading primitives (`QThread`, `QThreadPool`, `QRunnable`,
//! `QMutex`, `QRecursiveMutex`, `QReadWriteLock`, `QSemaphore`, `QWaitCondition`,
//! `QFuture`, `QPromise`, `QFutureWatcher`, `QFutureSynchronizer`) with idiomatic,
//! memory-safe Rust abstractions:
//!
//! - [`Thread`]: OS thread management, interruption, event-loop integrated worker threads.
//! - [`ThreadPool`]: High-performance task-stealing worker pool (`ThreadPool::global()`).
//! - [`TaskHandle`]: Task lifecycle, cancellation, status tracking, and future polling.
//! - [`Future`] / [`Promise`]: Asynchronous computation producer/consumer primitives.
//! - [`FutureWatcher`]: Signal-driven reactive watcher for `Future<T>`.
//! - [`FutureSynchronizer`]: Bulk future synchronization.
//! - [`Mutex`], [`RecursiveMutex`], [`RwLock`], [`Semaphore`], [`WaitCondition`]: Qt-parity sync primitives.
//! - [`channel`], [`bounded`]: Event-loop and Signal-connected channels.
//! - [`block_on`], [`spawn_async`], [`Executor`]: Async boundary bridging standard Rust futures.

pub mod channel;
pub mod executor;
pub mod future;
pub mod pool;
pub mod sync;
pub mod task;
#[allow(clippy::module_inception)]
pub mod thread;

pub use channel::*;
pub use executor::*;
pub use future::*;
pub use pool::*;
pub use sync::*;
pub use task::*;
pub use thread::*;

// Re-export thread affinity and context primitives from object::thread for unified access
#[allow(deprecated)]
pub use crate::object::move_to_thread;
pub use crate::object::{
    EventSender, ThreadContext, ThreadId, move_to_thread_by_id, query_object_thread,
    query_thread_sender, register_thread_sender, unregister_thread_sender,
};
