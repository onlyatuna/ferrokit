// SPDX-License-Identifier: LGPL-3.0-only
//! Reproduction for `RecursiveMutexGuard`: `deref_mut` hands out `&mut T` on every guard, and the
//! guard is `Send` without any owner-thread check. Each test documents the expected correct
//! behaviour; a failure or a Miri report is the defect.

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use ferrokit_core::thread::{RecursiveMutex, RecursiveMutexGuard};

fn assert_send<T: Send>() {}

/// Compile-time probe: the guard is `Send` whenever `T: Send`. `std::sync::MutexGuard` is not.
#[test]
fn recursive_mutex_guard_is_send_probe() {
    assert_send::<RecursiveMutexGuard<'static, i32>>();
}

/// Two live guards on one thread, each producing a `&mut i32`. Both `&mut` are used after the
/// other was created, so Stacked Borrows and Tree Borrows must reject the first write-through
/// once the second `&mut` exists.
#[test]
fn reentrant_guards_yield_aliasing_mut_refs() {
    let m = RecursiveMutex::new(0_i32);
    let mut g1 = m.lock();
    let mut g2 = m.lock();
    let a: &mut i32 = &mut g1;
    let b: &mut i32 = &mut g2;
    *b += 1;
    *a += 1;
    assert_eq!(*b, 2);
    drop(g2);
    drop(g1);
}

/// A guard moved to another thread and dropped there. The guard's `Drop` does not check the
/// owner, so the lock is released by a thread that never acquired it.
#[test]
fn guard_sent_to_other_thread_releases_on_drop() {
    let m = RecursiveMutex::new(0_i32);
    let g = m.lock();
    let (tx, rx) = mpsc::channel::<RecursiveMutexGuard<'_, i32>>();
    thread::scope(|s| {
        s.spawn(move || {
            let g = rx.recv_timeout(Duration::from_secs(5)).expect("guard received");
            drop(g);
        });
        tx.send(g).expect("send guard");
    });
}

/// Owner-thread violation: the main thread hands its guard to a helper, then re-locks. A correct
/// mutex still holds the lock on behalf of the helper, so a `try_lock_for` from the main thread
/// must fail. The re-entrant path accepts the main thread as owner and succeeds, giving the main
/// thread a second `&mut` while the helper still holds one.
#[test]
fn reentrant_lock_after_guard_moved_to_other_thread_succeeds() {
    let m = RecursiveMutex::new(0_i32);
    let (to_helper, helper_rx) = mpsc::channel::<RecursiveMutexGuard<'_, i32>>();
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();

    thread::scope(|s| {
        s.spawn(move || {
            let g = helper_rx.recv_timeout(Duration::from_secs(5)).expect("guard received");
            done_tx.send(()).expect("signal main");
            release_rx.recv_timeout(Duration::from_secs(5)).expect("release signal");
            drop(g);
        });

        to_helper.send(m.lock()).expect("send guard");
        done_rx.recv_timeout(Duration::from_secs(5)).expect("helper holds guard");

        // The helper's guard is still alive, so the lock must be held by another owner.
        let reentered = m.try_lock_for(Duration::from_millis(200));
        let acquired = reentered.is_some();
        drop(reentered);
        release_tx.send(()).expect("release helper");
        assert!(
            !acquired,
            "lock re-acquired on the main thread while a guard owned by the helper was alive"
        );
    });
}
