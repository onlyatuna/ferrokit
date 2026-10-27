// SPDX-License-Identifier: LGPL-3.0-only
//! `ActionTarget` registrations are cleaned up whichever thread drops the target.
//!
//! Runs against the mock Objective-C runtime (`ActionTarget::perform` is what the native
//! `-ferrokitPerformAction:` does). On macOS the objects are real and AppKit owns the delivery,
//! so the file is not built there.
#![cfg(not(target_os = "macos"))]

use ferrokit_platform::{ActionTarget, Id};
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Lets a test move a target to another thread to drop it there, as a holder with an
/// `unsafe impl Send` (the theme's observer) may.
#[allow(dead_code)] // held for its `Drop`
struct Moved(ActionTarget);
// SAFETY: test-only; the target is only dropped on the receiving thread, and the mock runtime
// that backs it is behind a mutex.
unsafe impl Send for Moved {}

/// `ActionTarget::perform(id)` from a fresh thread (an `Id` is a raw pointer, so not `Send`).
fn perform_elsewhere(id: Id) -> bool {
    let address = id.0 as usize;
    std::thread::spawn(move || ActionTarget::perform(Id(address as *mut std::ffi::c_void)))
        .join()
        .unwrap()
}

#[test]
fn a_shared_target_dropped_on_another_thread_loses_its_callback() {
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&hits);
    let target = ActionTarget::new_shared(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    })
    .unwrap();
    let id = target.id();
    assert!(ActionTarget::perform(id));
    let from_other = perform_elsewhere(id);
    assert!(from_other, "a shared callback runs on any thread");
    assert_eq!(hits.load(Ordering::SeqCst), 2);

    let moved = Moved(target);
    std::thread::spawn(move || drop(moved)).join().unwrap();
    assert!(!ActionTarget::perform(id));
    assert!(!perform_elsewhere(id));
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    assert_eq!(Arc::strong_count(&hits), 1, "the callback was released");
}

#[test]
fn a_local_target_runs_only_on_its_creating_thread() {
    let hits = Rc::new(AtomicUsize::new(0));
    let counter = Rc::clone(&hits);
    let target = ActionTarget::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    })
    .unwrap();
    let id = target.id();
    assert!(ActionTarget::perform(id));
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    assert!(
        !perform_elsewhere(id),
        "its closure is not Send, so another thread cannot run it"
    );
    drop(target);
    assert!(!ActionTarget::perform(id));
    assert_eq!(
        Rc::strong_count(&hits),
        1,
        "dropping on the creator releases it"
    );
}

#[test]
fn a_local_target_dropped_on_another_thread_is_no_longer_performed() {
    let (tx, rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let creator = std::thread::spawn(move || {
        let hits = Rc::new(AtomicUsize::new(0));
        let counter = Rc::clone(&hits);
        let target = ActionTarget::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        })
        .unwrap();
        let id = target.id();
        tx.send((Moved(target), id.0 as usize)).unwrap();
        // Keep this thread (and its thread-local table) alive until the target is gone.
        done_rx.recv().unwrap();
        // The entry this thread's table still holds must not run.
        let still = ActionTarget::perform(Id(id.0));
        (hits.load(Ordering::SeqCst), still)
    });
    let (moved, address) = rx.recv().unwrap();
    drop(moved);
    done_tx.send(()).unwrap();
    let (hits, still_performed) = creator.join().unwrap();
    assert_ne!(address, 0);
    assert!(!still_performed, "a dropped target has no live callback");
    assert_eq!(hits, 0);
}
