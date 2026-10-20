// SPDX-License-Identifier: LGPL-3.0-only
//! Runtime and compile-time tests for `RecursiveMutex` and `RecursiveMutexGuard`.
//!
//! Covered here: re-entrant lock behaviour on the owner thread, and that a timed lock attempt
//! from another thread fails while the owner holds the lock. Covered here as well: positive
//! compile-time checks of the auto-trait impls (`Sync`/`Send`).
//!
//! The negative guarantees (no `&mut T` through the guard without `DerefMut`, and the guard is
//! `!Send`) are NOT tested here; they live in the `compile_fail` doctests on
//! `RecursiveMutexGuard` in `src/thread/sync.rs`.

use std::cell::{Cell, RefCell};
use std::thread;
use std::time::Duration;

use ferrokit_core::thread::{RecursiveMutex, RecursiveMutexGuard};

fn assert_sync<T: Sync>() {}
fn assert_send<T: Send>() {}
fn assert_send_sync<T: Send + Sync>() {}

/// Positive compile-time controls: these must keep compiling.
#[test]
fn auto_trait_impls_hold() {
    assert_sync::<RecursiveMutexGuard<'static, i32>>();
    assert_sync::<RecursiveMutex<Cell<i32>>>();
    assert_send_sync::<RecursiveMutex<i32>>();
    assert_send::<RecursiveMutex<i32>>();
}

/// Re-entrant guards on one thread observe the same value, and mutation goes through interior
/// mutability so no two `&mut` ever exist.
#[test]
fn reentrant_lock_reads_are_consistent() {
    let m = RecursiveMutex::new(RefCell::new(0_i32));
    let g1 = m.lock();
    let g2 = m.lock();
    let g3 = m
        .try_lock()
        .expect("re-entrant try_lock succeeds for the owner");

    *g1.borrow_mut() += 1;
    assert_eq!(*g2.borrow(), 1);
    assert_eq!(*g3.borrow(), 1);

    drop(g3);
    drop(g2);
    assert_eq!(*g1.borrow(), 1);
    drop(g1);

    assert_eq!(*m.lock().borrow(), 1);
}

/// While the owner thread holds the lock, a timed lock attempt from another thread must fail.
#[test]
fn try_lock_for_from_other_thread_fails_while_held() {
    let m = RecursiveMutex::new(0_i32);
    let g = m.lock();

    let acquired = thread::scope(|s| {
        s.spawn(|| m.try_lock_for(Duration::from_millis(200)).is_some())
            .join()
            .expect("worker thread did not panic")
    });

    assert!(
        !acquired,
        "lock acquired by another thread while the owner still holds it"
    );
    drop(g);
    assert!(m.try_lock_for(Duration::from_millis(200)).is_some());
}
