// SPDX-License-Identifier: LGPL-3.0-only
//! `ActionTarget` registrations are cleaned up whichever thread drops the target.
//!
//! Runs against the mock Objective-C runtime (`ActionTarget::perform` is what the native
//! `-ferrokitPerformAction:` does). On macOS the objects are real and AppKit owns the delivery,
//! so the file is not built there.
#![cfg(not(target_os = "macos"))]

use ferrokit_platform::{ActionTarget, Id};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Lets a test move a target to another thread to drop it there, as a holder with an
/// `unsafe impl Send` (the theme's observer) may.
#[allow(dead_code)] // held for its `Drop`
struct Moved(ActionTarget);
// SAFETY: `ActionTarget` holds an `Id` (raw pointer), which is why it is not `Send`. This wrapper
// is only used by tests to move a target to another thread and drop it there; the mock Objective-C
// runtime behind it is guarded by a mutex, so a drop on another thread touches no unsynchronised
// state (the same contract the theme observer's `unsafe impl Send` relies on). [inferred: the
// runtime's locking is not re-checked here]
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
        // The closure stays in this thread's table until this thread next touches a target.
        let held_before = Rc::strong_count(&hits);
        // The entry this thread's table still holds must not run, and releases the closure.
        let still = ActionTarget::perform(Id(id.0));
        (
            hits.load(Ordering::SeqCst),
            still,
            held_before,
            Rc::strong_count(&hits),
        )
    });
    let (moved, address) = rx.recv().unwrap();
    drop(moved);
    done_tx.send(()).unwrap();
    let (hits, still_performed, held_before, held_after) = creator.join().unwrap();
    assert_ne!(address, 0);
    assert!(!still_performed, "a dropped target has no live callback");
    assert_eq!(hits, 0);
    assert_eq!(
        held_before, 2,
        "released only when the owner next touches a target"
    );
    assert_eq!(held_after, 1, "the owner's next call released the closure");
}

#[test]
fn a_foreign_drop_is_released_by_the_owners_next_new_or_drop() {
    for next in ["new", "drop"] {
        let (tx, rx) = std::sync::mpsc::channel();
        let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
        let creator = std::thread::spawn(move || {
            let witness = Rc::new(());
            let held = Rc::clone(&witness);
            let target = ActionTarget::new(move || {
                let _ = &held;
            })
            .unwrap();
            let other = ActionTarget::new(|| {}).unwrap();
            tx.send(Moved(target)).unwrap();
            go_rx.recv().unwrap();
            let before = Rc::strong_count(&witness);
            if next == "new" {
                let _another = ActionTarget::new(|| {}).unwrap();
                assert_eq!(Rc::strong_count(&witness), 1);
            } else {
                drop(other);
            }
            let after = Rc::strong_count(&witness);
            (before, after)
        });
        drop(rx.recv().unwrap());
        go_tx.send(()).unwrap();
        assert_eq!(creator.join().unwrap(), (2, 1), "owner's next {next}");
    }
}
