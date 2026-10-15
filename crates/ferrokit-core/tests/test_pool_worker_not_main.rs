// SPDX-License-Identifier: LGPL-3.0-only
//! A pool worker that queries main-thread status before any application exists must not be
//! reported as the main thread. Qt has no such notion: `QThread::isMainThread` is true only on
//! the thread that created the `QCoreApplication` (qthread.cpp:52-54, 416-419).

use ferrokit_core::object::ThreadContext;
use ferrokit_core::thread::ThreadPool;

#[test]
fn pool_worker_is_not_main_thread_when_queried_first() {
    let pool = ThreadPool::builder()
        .max_threads(1)
        .name_prefix("ferrokit-test-pool")
        .build();
    let handle = pool.spawn(ThreadContext::is_main_thread);
    assert_eq!(handle.wait_result(), Ok(false));
}
