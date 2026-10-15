// SPDX-License-Identifier: LGPL-3.0-only
//! The thread that constructs the application is the main thread; a pool worker is not, even
//! though the worker runs after the application exists (qcoreapplication.cpp:475, 531-537).

use ferrokit_core::application::CoreApplication;
use ferrokit_core::object::ThreadContext;
use ferrokit_core::thread::ThreadPool;

#[test]
fn application_constructing_thread_is_main_and_pool_worker_is_not() {
    let _app = CoreApplication::new(Vec::new());
    assert!(ThreadContext::is_main_thread());

    let pool = ThreadPool::builder()
        .max_threads(1)
        .name_prefix("ferrokit-test-main-pool")
        .build();
    let handle = pool.spawn(ThreadContext::is_main_thread);
    assert_eq!(handle.wait_result(), Ok(false));
}
