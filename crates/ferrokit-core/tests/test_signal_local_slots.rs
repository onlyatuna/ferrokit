// SPDX-License-Identifier: LGPL-3.0-only
//! `Signal::connect_local*`: slots that capture state of one thread (`Rc`, `RefCell`).
//!
//! A local slot runs only when the signal is emitted from the thread that connected it; any other
//! thread skips it and never touches the captures. Removing the connection from another thread
//! drops the closure on the connecting thread, at its next loop turn.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use ferrokit_core::event_loop::EventLoop;
use ferrokit_core::object::ThreadId;
use ferrokit_core::signal::{ConnectionId, Signal};

fn assert_send_sync<X: Send + Sync>() {}

#[test]
fn a_signal_with_a_non_send_payload_stays_send_and_sync() {
    assert_send_sync::<Signal<Rc<u8>>>();
}

#[test]
fn a_local_slot_captures_an_rc_and_runs_on_the_connecting_thread() {
    let signal: Signal<i32> = Signal::new();
    let total = Rc::new(Cell::new(0));
    let sink = Rc::clone(&total);
    signal.connect_local(move |value| sink.set(sink.get() + *value));

    signal.emit(&5);
    signal.emit(&7);
    assert_eq!(total.get(), 12);
}

#[test]
fn local_slots_run_in_connection_order_with_ordinary_slots() {
    let signal: Signal<i32> = Signal::new();
    let log = Rc::new(RefCell::new(Vec::new()));
    let shared = Arc::new(Mutex::new(Vec::new()));

    let l = Rc::clone(&log);
    signal.connect_local(move |v| l.borrow_mut().push(("local-a", *v)));
    let s = Arc::clone(&shared);
    signal.connect(move |v| s.lock().unwrap().push(*v));
    let l = Rc::clone(&log);
    signal.connect_local(move |v| l.borrow_mut().push(("local-b", *v)));

    signal.emit(&1);
    assert_eq!(*log.borrow(), vec![("local-a", 1), ("local-b", 1)]);
    assert_eq!(*shared.lock().unwrap(), vec![1]);
}

#[test]
fn the_payload_may_be_neither_send_nor_clone() {
    struct Model {
        names: Rc<RefCell<Vec<String>>>,
    }
    let signal: Signal<Model> = Signal::new();
    let seen = Rc::new(Cell::new(0));
    let sink = Rc::clone(&seen);
    signal.connect_local(move |model: &Model| sink.set(model.names.borrow().len()));

    signal.emit(&Model {
        names: Rc::new(RefCell::new(vec!["a".into(), "b".into()])),
    });
    assert_eq!(seen.get(), 2);
}

#[test]
fn a_signal_clone_reaches_the_local_slot() {
    let signal: Signal<i32> = Signal::new();
    let total = Rc::new(Cell::new(0));
    let sink = Rc::clone(&total);
    signal.connect_local(move |value| sink.set(sink.get() + *value));

    let clone = signal.clone();
    clone.emit(&3);
    assert_eq!(total.get(), 3);
}

#[test]
fn an_emission_from_another_thread_skips_the_slot_and_leaves_its_captures_alone() {
    let signal: Signal<i32> = Signal::new();
    let total = Rc::new(Cell::new(0));
    let sink = Rc::clone(&total);
    signal.connect_local(move |value| sink.set(sink.get() + *value));

    let remote = signal.clone();
    std::thread::spawn(move || {
        remote.emit(&100);
        remote.emit(&100); // the diagnostic is printed once; both are skipped
    })
    .join()
    .unwrap();

    assert_eq!(total.get(), 0, "the slot must not run off its thread");
    assert_eq!(
        Rc::strong_count(&total),
        2,
        "no capture was touched or dropped"
    );

    signal.emit(&1);
    assert_eq!(total.get(), 1, "the slot still works on its own thread");
}

#[test]
fn a_foreign_emission_consumes_a_single_shot_local_slot_without_running_it() {
    let signal: Signal<i32> = Signal::new();
    let runs = Rc::new(Cell::new(0));
    let sink = Rc::clone(&runs);
    signal.connect_local_single_shot(move |_| sink.set(sink.get() + 1));

    let remote = signal.clone();
    std::thread::spawn(move || remote.emit(&1)).join().unwrap();
    signal.emit(&1);
    assert_eq!(runs.get(), 0);
}

#[test]
fn disconnecting_from_another_thread_drops_the_closure_on_the_next_loop_turn_here() {
    let event_loop = EventLoop::new(); // gives this thread a loop for the retire post
    let signal: Signal<i32> = Signal::new();
    let total = Rc::new(Cell::new(0));
    let sink = Rc::clone(&total);
    let id = signal.connect_local(move |value| sink.set(sink.get() + *value));

    let remote = signal.clone();
    let removed = std::thread::spawn(move || remote.disconnect(id))
        .join()
        .unwrap();
    assert!(removed);
    assert_eq!(
        Rc::strong_count(&total),
        2,
        "another thread must not drop the closure"
    );

    signal.emit(&9);
    assert_eq!(total.get(), 0, "a disconnected slot never runs again");

    assert!(event_loop.process_events(false));
    assert_eq!(
        Rc::strong_count(&total),
        1,
        "dropped here, at the loop turn"
    );
}

#[test]
fn the_last_signal_clone_dropped_on_another_thread_drops_the_closure_here() {
    let event_loop = EventLoop::new();
    let signal: Signal<i32> = Signal::new();
    let total = Rc::new(Cell::new(0));
    let sink = Rc::clone(&total);
    signal.connect_local(move |value| sink.set(sink.get() + *value));

    std::thread::spawn(move || drop(signal)).join().unwrap();
    assert_eq!(Rc::strong_count(&total), 2);

    assert!(event_loop.process_events(false));
    assert_eq!(Rc::strong_count(&total), 1);
}

#[test]
fn disconnecting_on_the_connecting_thread_drops_the_closure_at_once() {
    let signal: Signal<i32> = Signal::new();
    let total = Rc::new(Cell::new(0));
    let sink = Rc::clone(&total);
    let id = signal.connect_local(move |value| sink.set(sink.get() + *value));
    assert_eq!(Rc::strong_count(&total), 2);

    assert!(signal.disconnect(id));
    assert_eq!(Rc::strong_count(&total), 1);
}

#[test]
fn the_exit_of_the_connecting_thread_drops_the_closure_on_that_thread() {
    struct OnThread {
        drops: Arc<AtomicUsize>,
        thread: ThreadId,
    }
    impl Drop for OnThread {
        fn drop(&mut self) {
            assert_eq!(
                ThreadId::current(),
                self.thread,
                "dropped on another thread"
            );
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }

    let drops = Arc::new(AtomicUsize::new(0));
    let kept: Arc<Mutex<Option<Signal<i32>>>> = Arc::new(Mutex::new(None));
    let (drops_in, kept_in) = (Arc::clone(&drops), Arc::clone(&kept));
    std::thread::spawn(move || {
        let signal: Signal<i32> = Signal::new();
        let guard = OnThread {
            drops: drops_in,
            thread: ThreadId::current(),
        };
        signal.connect_local(move |_| {
            let _ = &guard;
        });
        // The signal outlives the thread, so only the thread's own table can drop the closure.
        *kept_in.lock().unwrap() = Some(signal);
    })
    .join()
    .unwrap();
    assert_eq!(drops.load(Ordering::SeqCst), 1);

    // The last clone goes here; the owner thread has ended, so the retire post is dropped.
    let signal = kept.lock().unwrap().take().unwrap();
    signal.emit(&1); // the slot is gone with its thread: nothing runs, nothing panics
    drop(signal);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn a_scoped_local_connection_disconnects_when_dropped() {
    let signal: Signal<i32> = Signal::new();
    let total = Rc::new(Cell::new(0));
    {
        let sink = Rc::clone(&total);
        let _guard = signal.connect_local_scoped(move |value| sink.set(sink.get() + *value));
        signal.emit(&2);
        assert_eq!(signal.subscriber_count(), 1);
    }
    assert_eq!(signal.subscriber_count(), 0);
    signal.emit(&2);
    assert_eq!(total.get(), 2);
    assert_eq!(Rc::strong_count(&total), 1);
}

#[test]
fn a_scoped_local_connection_can_be_disconnected_explicitly_or_released() {
    let signal: Signal<i32> = Signal::new();
    let runs = Rc::new(Cell::new(0));

    let sink = Rc::clone(&runs);
    let guard = signal.connect_local_scoped(move |_| sink.set(sink.get() + 1));
    guard.disconnect();
    signal.emit(&1);
    assert_eq!(runs.get(), 0);

    let sink = Rc::clone(&runs);
    let released = signal.connect_local_scoped(move |_| sink.set(sink.get() + 1));
    let _id: ConnectionId = released.release();
    signal.emit(&1);
    assert_eq!(runs.get(), 1);
}

#[test]
fn a_scoped_local_guard_dropped_on_another_thread_removes_the_connection_at_once() {
    let event_loop = EventLoop::new();
    let signal: Signal<i32> = Signal::new();
    let runs = Rc::new(Cell::new(0));
    let sink = Rc::clone(&runs);
    let guard = signal.connect_local_scoped(move |_| sink.set(sink.get() + 1));

    std::thread::spawn(move || drop(guard)).join().unwrap();
    signal.emit(&1);
    assert_eq!(runs.get(), 0);
    assert_eq!(
        Rc::strong_count(&runs),
        2,
        "the closure waits for the loop turn"
    );
    assert!(event_loop.process_events(false));
    assert_eq!(Rc::strong_count(&runs), 1);
}

#[test]
fn a_single_shot_local_slot_runs_once_even_when_emitted_again_inside_the_slot() {
    let signal: Signal<i32> = Signal::new();
    let runs = Rc::new(Cell::new(0));
    let (sink, again) = (Rc::clone(&runs), signal.clone());
    signal.connect_local_single_shot(move |value| {
        sink.set(sink.get() + 1);
        if *value == 1 {
            again.emit(&2);
        }
    });

    signal.emit(&1);
    signal.emit(&1);
    assert_eq!(runs.get(), 1);
    assert_eq!(signal.subscriber_count(), 0);
    assert_eq!(Rc::strong_count(&runs), 1);
}

#[test]
fn a_slot_may_disconnect_itself_and_its_captures_outlive_the_call() {
    let signal: Signal<i32> = Signal::new();
    let state = Rc::new(Cell::new(0));
    let id: Rc<Cell<Option<ConnectionId>>> = Rc::new(Cell::new(None));
    let after: Rc<Cell<usize>> = Rc::new(Cell::new(0));

    let (s, i, a, sig) = (
        Rc::clone(&state),
        Rc::clone(&id),
        Rc::clone(&after),
        signal.clone(),
    );
    id.set(Some(signal.connect_local(move |_| {
        s.set(s.get() + 1);
        assert!(sig.disconnect(i.get().unwrap()));
        // The call is still running: the closure and everything it captured are alive.
        a.set(Rc::strong_count(&s));
        s.set(s.get() + 1);
    })));

    signal.emit(&1);
    assert_eq!(
        state.get(),
        2,
        "the slot ran to its end after disconnecting itself"
    );
    assert_eq!(
        after.get(),
        2,
        "the capture was still held while the slot ran"
    );
    signal.emit(&1);
    assert_eq!(state.get(), 2);
    drop((id, after, signal));
    assert_eq!(
        Rc::strong_count(&state),
        1,
        "closure dropped once the emission ended"
    );
}

#[test]
fn a_slot_may_connect_another_local_slot_which_does_not_run_in_that_emission() {
    let signal: Signal<i32> = Signal::new();
    let log = Rc::new(RefCell::new(Vec::new()));

    let (outer_log, inner_log, sig) = (Rc::clone(&log), Rc::clone(&log), signal.clone());
    let connected = Rc::new(Cell::new(false));
    signal.connect_local(move |_| {
        outer_log.borrow_mut().push("outer");
        if !connected.replace(true) {
            let inner_log = Rc::clone(&inner_log);
            sig.connect_local(move |_| inner_log.borrow_mut().push("inner"));
        }
    });

    signal.emit(&1);
    assert_eq!(*log.borrow(), vec!["outer"]);
    signal.emit(&1);
    assert_eq!(*log.borrow(), vec!["outer", "outer", "inner"]);
    signal.disconnect_all();
}

#[test]
fn a_slot_may_emit_its_own_signal_again() {
    let signal: Signal<i32> = Signal::new();
    let depth_seen = Rc::new(RefCell::new(Vec::new()));
    let (sink, again) = (Rc::clone(&depth_seen), signal.clone());
    signal.connect_local(move |depth| {
        sink.borrow_mut().push(*depth);
        if *depth < 3 {
            again.emit(&(*depth + 1));
        }
    });

    signal.emit(&1);
    assert_eq!(*depth_seen.borrow(), vec![1, 2, 3]);
    signal.disconnect_all();
}

#[test]
fn a_panicking_local_slot_is_contained_like_any_other_and_the_slot_stays_usable() {
    let signal: Signal<i32> = Signal::new();
    let log = Rc::new(RefCell::new(Vec::new()));

    let l = Rc::clone(&log);
    signal.connect_local(move |value| {
        l.borrow_mut().push(*value);
        if *value == 1 {
            panic!("a slot that fails");
        }
    });
    let l = Rc::clone(&log);
    signal.connect_local(move |value| l.borrow_mut().push(*value * 10));

    signal.emit(&1); // the panic is contained by `emit`, as for `connect`
    signal.emit(&2);
    assert_eq!(*log.borrow(), vec![1, 10, 2, 20]);
}
