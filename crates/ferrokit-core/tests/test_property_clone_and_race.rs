// SPDX-License-Identifier: LGPL-3.0-only
//! `Property` clones share one dependency node: dropping one clone must not detach the others, and
//! a recompute must not overwrite an invalidation that arrived while the binding ran.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ferrokit_core::property::Property;

#[test]
fn dropping_a_clone_keeps_dependents_invalidated() {
    let a = Property::new(1);
    let a2 = a.clone();
    let b = Property::with_binding(move || a2.get() + 1);
    let b2 = b.clone();
    let c = Property::with_binding(move || b2.get() * 10);
    assert_eq!(c.get(), 20);

    drop(b.clone());
    a.set(5);
    assert_eq!(b.get(), 6);
    assert_eq!(c.get(), 60);
}

#[test]
fn invalidation_during_binding_is_not_lost() {
    let a = Property::new(1);
    let (entered_tx, entered_rx) = mpsc::channel::<()>();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let go_rx = Mutex::new(go_rx);
    let entered_tx = Mutex::new(entered_tx);
    let pause = Arc::new(AtomicBool::new(false));

    let a_dep = a.clone();
    let pause_c = Arc::clone(&pause);
    let b = Property::with_binding(move || {
        let v = a_dep.get();
        if pause_c.swap(false, Ordering::SeqCst) {
            let _ = entered_tx.lock().unwrap().send(());
            let _ = go_rx.lock().unwrap().recv_timeout(Duration::from_secs(30));
        }
        v
    });
    assert_eq!(b.get(), 1);

    // Dirty b, then read it on another thread and hold the binding after it read a == 2.
    pause.store(true, Ordering::SeqCst);
    a.set(2);
    let b_reader = b.clone();
    let t = std::thread::spawn(move || b_reader.get());
    entered_rx.recv_timeout(Duration::from_secs(30)).unwrap();
    // Invalidates b while its binding is running.
    a.set(5);
    go_tx.send(()).unwrap();
    let _ = t.join().unwrap();

    assert_eq!(
        b.get(),
        5,
        "invalidation that arrived during compute was lost"
    );
}
