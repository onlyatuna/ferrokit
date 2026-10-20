// SPDX-License-Identifier: LGPL-3.0-only
//! `RecursiveMutexGuard` must not allow aliasing `&mut T` or cross-thread release. Those
//! properties are enforced at compile time by the `compile_fail` doctests on
//! `RecursiveMutexGuard`; the tests here cover the runtime behaviour that remains valid.

use std::cell::RefCell;
use std::thread;
use std::time::Duration;

use ferrokit_core::thread::RecursiveMutex;

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
