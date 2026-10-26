// SPDX-License-Identifier: LGPL-3.0-only
//! `Signal::connect_local_to` and its variants: slots that are not `Send`, connected against a
//! context object (Qt `connect(sender, signal, context, functor, type)`).
//!
//! The context decides the thread of an `Auto` connection, cuts the connection when it is
//! destroyed, and must be a registered, non-movable object on the connecting thread. The slot
//! itself always runs on the connecting thread.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use ferrokit_core::event_loop::EventLoop;
use ferrokit_core::object::{
    register_owned_qobject, ObjectData, ObjectDataMut, ObjectId, OwnedObject, QObject, ThreadId,
};
use ferrokit_core::signal::{sender, ConnectionType, LocalConnectError, Signal, SlotId, SlotKey};

/// Only a failing test waits this long: every wait below ends as soon as its event happens.
const WATCHDOG: Duration = Duration::from_secs(10);

/// An object that only receives.
struct Receiver {
    data: ObjectData,
}

impl Receiver {
    /// A registered receiver on this thread, held by its owner.
    fn registered() -> OwnedObject<Self> {
        // SAFETY: the owner stays on this thread and nothing touches the object while a callback
        // of it runs.
        unsafe {
            register_owned_qobject(Self {
                data: ObjectData::with_auto_id(),
            })
        }
    }
}

impl QObject for Receiver {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ObjectDataMut<'_> {
        ObjectDataMut::new(&mut self.data)
    }
}

/// Another thread's id (the thread has ended; the id stays unique).
fn other_thread_id() -> ThreadId {
    std::thread::spawn(ThreadId::current).join().unwrap()
}

#[test]
fn auto_on_the_context_thread_runs_inline() {
    let context = Receiver::registered();
    let signal: Signal<i32> = Signal::new();
    let total = Rc::new(Cell::new(0));
    let sink = Rc::clone(&total);
    context
        .with(|c| {
            signal.connect_local_to(c, ConnectionType::Auto, move |v| sink.set(sink.get() + *v))
        })
        .unwrap()
        .unwrap();

    signal.emit(&4);
    assert_eq!(total.get(), 4);
}

#[test]
fn auto_from_another_thread_is_queued_to_the_context_thread() {
    let event_loop = EventLoop::new();
    let context = Receiver::registered();
    let owner_thread = ThreadId::current();
    let source = ObjectData::with_auto_id();
    let source_id = source.id();
    let signal: Signal<i32> = Signal::with_owner(&source);

    let seen = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&seen);
    context
        .with(|c| {
            signal.connect_local_to(c, ConnectionType::Auto, move |v| {
                sink.borrow_mut().push((*v, ThreadId::current(), sender()));
            })
        })
        .unwrap()
        .unwrap();

    let remote = signal.clone();
    std::thread::spawn(move || remote.emit(&10)).join().unwrap();
    assert!(
        seen.borrow().is_empty(),
        "queued, not run in the emitting thread"
    );

    assert!(event_loop.process_events(false));
    assert_eq!(*seen.borrow(), vec![(10, owner_thread, Some(source_id))]);
}

#[test]
fn sender_is_visible_inside_a_direct_local_slot() {
    let signal_owner = ObjectData::with_auto_id();
    let id = signal_owner.id();
    let signal: Signal<i32> = Signal::with_owner(&signal_owner);
    let context = Receiver::registered();
    let seen = Rc::new(Cell::new(None));
    let sink = Rc::clone(&seen);
    context
        .with(|c| signal.connect_local_to(c, ConnectionType::Direct, move |_| sink.set(sender())))
        .unwrap()
        .unwrap();

    signal.emit(&1);
    assert_eq!(seen.get(), Some(id));
}

#[test]
fn queued_on_the_context_thread_is_posted_not_run_inline() {
    let event_loop = EventLoop::new();
    let context = Receiver::registered();
    let signal: Signal<i32> = Signal::new();
    let total = Rc::new(Cell::new(0));
    let sink = Rc::clone(&total);
    context
        .with(|c| {
            signal.connect_local_to(c, ConnectionType::Queued, move |v| {
                sink.set(sink.get() + *v)
            })
        })
        .unwrap()
        .unwrap();

    signal.emit(&3);
    assert_eq!(
        total.get(),
        0,
        "a queued call waits for the loop, even on its own thread"
    );
    assert!(event_loop.process_events(false));
    assert_eq!(total.get(), 3);
}

#[test]
fn direct_from_another_thread_skips_the_slot_and_keeps_the_connection() {
    let context = Receiver::registered();
    let signal: Signal<i32> = Signal::new();
    let total = Rc::new(Cell::new(0));
    let sink = Rc::clone(&total);
    context
        .with(|c| {
            signal.connect_local_to(c, ConnectionType::Direct, move |v| {
                sink.set(sink.get() + *v)
            })
        })
        .unwrap()
        .unwrap();

    let remote = signal.clone();
    std::thread::spawn(move || remote.emit(&100))
        .join()
        .unwrap();
    assert_eq!(total.get(), 0);
    assert_eq!(Rc::strong_count(&total), 2);
    signal.emit(&1);
    assert_eq!(total.get(), 1);
}

#[test]
fn blocking_queued_from_a_worker_returns_only_after_the_owner_has_run_the_slot() {
    let event_loop = EventLoop::new();
    let context = Receiver::registered();
    let owner_thread = ThreadId::current();
    let signal: Signal<i32> = Signal::new();

    let ran = Arc::new(AtomicUsize::new(0));
    let ran_in_slot = Arc::clone(&ran);
    let slot_thread = Rc::new(Cell::new(None));
    let sink = Rc::clone(&slot_thread);
    context
        .with(|c| {
            signal.connect_local_blocking_queued(c, move |_| {
                sink.set(Some(ThreadId::current()));
                ran_in_slot.fetch_add(1, Ordering::SeqCst);
            })
        })
        .unwrap()
        .unwrap();

    let remote = signal.clone();
    let ran_in_worker = Arc::clone(&ran);
    let (done_tx, done_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        remote.emit(&1);
        // `emit` waited for the slot: it has run by the time it returns.
        done_tx.send(ran_in_worker.load(Ordering::SeqCst)).unwrap();
    });

    // Pump this thread's loop until the worker's emit has returned. No timing is assumed: the
    // worker cannot return before this thread runs the slot.
    let seen_by_worker = loop {
        event_loop.process_events(false);
        if let Ok(count) = done_rx.try_recv() {
            break count;
        }
        std::thread::yield_now();
    };
    worker.join().unwrap();
    assert_eq!(
        seen_by_worker, 1,
        "the emitter returned only after the slot ran"
    );
    assert_eq!(ran.load(Ordering::SeqCst), 1);
    assert_eq!(slot_thread.get(), Some(owner_thread));
}

#[test]
fn blocking_queued_on_the_context_thread_runs_in_place_where_qt_would_deadlock() {
    let context = Receiver::registered();
    let signal: Signal<i32> = Signal::new();
    let total = Rc::new(Cell::new(0));
    let sink = Rc::clone(&total);
    context
        .with(|c| {
            signal.connect_local_to(c, ConnectionType::BlockingQueued, move |v| {
                sink.set(sink.get() + *v)
            })
        })
        .unwrap()
        .unwrap();

    signal.emit(&6); // no loop is running, and nothing waits
    assert_eq!(total.get(), 6);
}

#[test]
fn a_context_destroyed_before_delivery_drops_the_posted_call_and_retires_the_closure() {
    let event_loop = EventLoop::new();
    let context = Receiver::registered();
    let signal: Signal<i32> = Signal::new();
    let total = Rc::new(Cell::new(0));
    let sink = Rc::clone(&total);
    context
        .with(|c| {
            signal.connect_local_to(c, ConnectionType::Auto, move |v| sink.set(sink.get() + *v))
        })
        .unwrap()
        .unwrap();

    let remote = signal.clone();
    std::thread::spawn(move || remote.emit(&1000))
        .join()
        .unwrap();
    assert_eq!(Rc::strong_count(&total), 2);

    drop(context); // purges the posted call and cuts the connection
    event_loop.process_events(false);
    assert_eq!(total.get(), 0, "a call to a destroyed context never runs");
    assert_eq!(Rc::strong_count(&total), 1, "the closure was retired");
    assert_eq!(signal.subscriber_count(), 0);
}

#[test]
fn a_destroyed_context_disconnects_a_direct_local_slot() {
    let context = Receiver::registered();
    let signal: Signal<i32> = Signal::new();
    let total = Rc::new(Cell::new(0));
    let sink = Rc::clone(&total);
    context
        .with(|c| {
            signal.connect_local_to(c, ConnectionType::Direct, move |v| {
                sink.set(sink.get() + *v)
            })
        })
        .unwrap()
        .unwrap();
    signal.emit(&1);
    assert_eq!(total.get(), 1);

    drop(context);
    signal.emit(&1);
    assert_eq!(total.get(), 1);
    assert_eq!(Rc::strong_count(&total), 1);
    assert_eq!(signal.subscriber_count(), 0);
}

#[test]
fn unique_rejects_a_second_identical_connection() {
    let context = Receiver::registered();
    let signal: Signal<i32> = Signal::new();
    let total = Rc::new(Cell::new(0));
    let key = SlotId::next();

    let sink = Rc::clone(&total);
    let first = context
        .with(|c| signal.connect_local_unique(c, key, move |v| sink.set(sink.get() + *v)))
        .unwrap()
        .unwrap();
    assert!(first.is_some());

    let rejected_capture = Rc::clone(&total);
    let second = context
        .with(|c| signal.connect_local_unique(c, key, move |_| rejected_capture.set(-1)))
        .unwrap()
        .unwrap();
    assert_eq!(second, None);
    assert_eq!(
        Rc::strong_count(&total),
        2,
        "the rejected closure was dropped at once"
    );

    let sink = Rc::clone(&total);
    let other_key = context
        .with(|c| {
            signal.connect_local_unique(c, SlotKey::Named("other"), move |v| {
                sink.set(sink.get() + *v * 10)
            })
        })
        .unwrap()
        .unwrap();
    assert!(other_key.is_some(), "a different key is a different slot");

    signal.emit(&1);
    assert_eq!(total.get(), 11);
    assert_eq!(signal.subscriber_count(), 2);
}

#[test]
fn a_rejected_unique_closure_may_use_the_signal_when_it_is_dropped() {
    // The duplicate's closure is dropped after the signal's lock is released.
    struct UsesSignal(Signal<i32>);
    impl Drop for UsesSignal {
        fn drop(&mut self) {
            self.0.connect(|_| {});
        }
    }
    let context = Receiver::registered();
    let signal: Signal<i32> = Signal::new();
    let key = SlotId::next();
    context
        .with(|c| signal.connect_local_unique(c, key, |_| {}))
        .unwrap()
        .unwrap();

    let guard = UsesSignal(signal.clone());
    let second = context
        .with(|c| {
            signal.connect_local_unique(c, key, move |_| {
                let _ = &guard;
            })
        })
        .unwrap()
        .unwrap();
    assert_eq!(second, None);
    assert_eq!(
        signal.subscriber_count(),
        2,
        "the original and the one the drop connected"
    );
    signal.disconnect_all();
}

#[test]
fn single_shot_to_runs_once_and_a_queued_emission_consumes_it() {
    let event_loop = EventLoop::new();
    let context = Receiver::registered();
    let signal: Signal<i32> = Signal::new();
    let runs = Rc::new(Cell::new(0));
    let sink = Rc::clone(&runs);
    context
        .with(|c| signal.connect_local_single_shot_to(c, move |_| sink.set(sink.get() + 1)))
        .unwrap()
        .unwrap();

    let remote = signal.clone();
    std::thread::spawn(move || {
        remote.emit(&1);
        remote.emit(&1);
    })
    .join()
    .unwrap();
    assert_eq!(
        signal.subscriber_count(),
        0,
        "the first queued emission consumed it, as in Qt"
    );
    assert_eq!(runs.get(), 0);
    event_loop.process_events(false);
    assert_eq!(
        runs.get(),
        1,
        "the one call posted before it was consumed runs, on this thread"
    );

    signal.emit(&1);
    assert_eq!(runs.get(), 1);
    assert_eq!(Rc::strong_count(&runs), 1);
}

#[test]
fn single_shot_to_on_the_context_thread_runs_once_even_when_emitted_inside_the_slot() {
    let context = Receiver::registered();
    let signal: Signal<i32> = Signal::new();
    let runs = Rc::new(Cell::new(0));
    let (sink, again) = (Rc::clone(&runs), signal.clone());
    context
        .with(|c| {
            signal.connect_local_single_shot_to(c, move |_| {
                sink.set(sink.get() + 1);
                again.emit(&2);
            })
        })
        .unwrap()
        .unwrap();

    signal.emit(&1);
    signal.emit(&1);
    assert_eq!(runs.get(), 1);
}

#[test]
fn a_movable_context_is_refused_and_the_closure_is_not_kept() {
    // SAFETY: the object holds no state that is not `Send`; it is never moved.
    let data = unsafe { ObjectData::with_auto_id().with_thread_moves_allowed() };
    let context = Receiver { data };
    let signal: Signal<i32> = Signal::new();
    let capture = Rc::new(());
    let held = Rc::clone(&capture);
    let result = signal.connect_local_to(&context, ConnectionType::Direct, move |_| {
        let _ = &held;
    });
    assert_eq!(result, Err(LocalConnectError::ContextMovable));
    assert_eq!(Rc::strong_count(&capture), 1);
    assert_eq!(signal.subscriber_count(), 0);
}

#[test]
fn a_context_of_another_thread_is_refused() {
    let context = Receiver {
        data: ObjectData::with_thread(ObjectId::next(), other_thread_id()),
    };
    let signal: Signal<i32> = Signal::new();
    let result = signal.connect_local_to(&context, ConnectionType::Direct, |_| {});
    assert_eq!(result, Err(LocalConnectError::WrongThread));
    assert_eq!(signal.subscriber_count(), 0);
}

#[test]
fn queued_kinds_need_a_registered_context_and_direct_does_not() {
    let context = Receiver {
        data: ObjectData::with_auto_id(), // never registered, as a widget
    };
    let signal: Signal<i32> = Signal::new();
    for conn_type in [
        ConnectionType::Auto,
        ConnectionType::Queued,
        ConnectionType::BlockingQueued,
    ] {
        assert_eq!(
            signal.connect_local_to(&context, conn_type, |_| {}),
            Err(LocalConnectError::ContextNotRegistered),
            "{conn_type:?}"
        );
    }
    assert_eq!(
        signal.connect_local_unique(&context, SlotId::next(), |_| {}),
        Err(LocalConnectError::ContextNotRegistered)
    );
    assert_eq!(
        signal.connect_local_single_shot_to(&context, |_| {}),
        Err(LocalConnectError::ContextNotRegistered)
    );
    assert_eq!(
        signal.connect_local_blocking_queued(&context, |_| {}),
        Err(LocalConnectError::ContextNotRegistered)
    );
    assert_eq!(signal.subscriber_count(), 0);

    let total = Rc::new(Cell::new(0));
    let sink = Rc::clone(&total);
    signal
        .connect_local_to(&context, ConnectionType::Direct, move |v| {
            sink.set(sink.get() + *v)
        })
        .unwrap();
    signal.emit(&2);
    assert_eq!(total.get(), 2);
}

#[test]
fn a_blocked_blocking_queued_emitter_is_released_when_the_context_is_destroyed() {
    let event_loop = EventLoop::new();
    let queue = Arc::clone(&event_loop.sender().queue);
    assert_eq!(queue.lock().unwrap().len(), 0);
    let context = Receiver::registered();
    let signal: Signal<i32> = Signal::new();
    let ran = Arc::new(AtomicUsize::new(0));
    let in_slot = Arc::clone(&ran);
    let guard = Rc::new(());
    let held = Rc::clone(&guard);
    context
        .with(|c| {
            signal.connect_local_blocking_queued(c, move |_| {
                let _ = &held;
                in_slot.fetch_add(1, Ordering::SeqCst);
            })
        })
        .unwrap()
        .unwrap();

    let remote = signal.clone();
    let (done_tx, done_rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        remote.emit(&1);
        done_tx.send(()).unwrap();
    });

    // The call is in this thread's queue once the worker has posted it; from then on the worker
    // is blocked, because this thread does not run its loop.
    let deadline = Instant::now() + WATCHDOG;
    while queue.lock().unwrap().len() == 0 {
        assert!(
            Instant::now() < deadline,
            "the worker never posted its call"
        );
        thread::yield_now();
    }
    assert!(
        done_rx.try_recv().is_err(),
        "the emitter returned before anything ran its call"
    );

    drop(context); // purges the posted call without running it
    done_rx
        .recv_timeout(WATCHDOG)
        .expect("the emitter is still blocked after its context was destroyed");
    worker.join().unwrap();
    assert_eq!(ran.load(Ordering::SeqCst), 0, "the slot never ran");

    event_loop.process_events(false); // runs the closure's retire job, if it was posted
    assert_eq!(Rc::strong_count(&guard), 1, "the closure was retired");
}

#[test]
fn auto_from_another_thread_waits_for_an_owner_that_has_no_loop_yet() {
    let context = Receiver::registered(); // this thread has no event loop yet
    let signal: Signal<i32> = Signal::new();
    let total = Rc::new(Cell::new(0));
    let sink = Rc::clone(&total);
    context
        .with(|c| {
            signal.connect_local_to(c, ConnectionType::Auto, move |v| sink.set(sink.get() + *v))
        })
        .unwrap()
        .unwrap();

    let remote = signal.clone();
    thread::spawn(move || remote.emit(&3)).join().unwrap();
    assert_eq!(total.get(), 0);

    let event_loop = EventLoop::new(); // the held call is handed to the loop that registers
    assert!(event_loop.process_events(false));
    assert_eq!(total.get(), 3);
}

#[test]
fn a_direct_local_slot_of_an_unregistered_context_is_cut_when_the_context_is_destroyed() {
    let context = Receiver {
        data: ObjectData::with_auto_id(), // never registered
    };
    let signal: Signal<i32> = Signal::new();
    let total = Rc::new(Cell::new(0));
    let sink = Rc::clone(&total);
    signal
        .connect_local_to(&context, ConnectionType::Direct, move |v| {
            sink.set(sink.get() + *v)
        })
        .unwrap();
    signal.emit(&1);
    assert_eq!(total.get(), 1);

    drop(context);
    signal.emit(&1);
    assert_eq!(total.get(), 1, "the destroyed context's connection is gone");
    assert_eq!(signal.subscriber_count(), 0);
    assert_eq!(Rc::strong_count(&total), 1);
}

#[test]
fn local_and_ordinary_queued_slots_run_in_connection_order_on_the_context_thread() {
    let event_loop = EventLoop::new();
    let context = Receiver::registered();
    let signal: Signal<i32> = Signal::new();
    let log: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
    let thread = ThreadId::current();

    let l = Arc::clone(&log);
    let rc_in_slot = Rc::new(Cell::new(0));
    let r = Rc::clone(&rc_in_slot);
    context
        .with(|c| {
            signal.connect_local_to(c, ConnectionType::Auto, move |_| {
                r.set(r.get() + 1);
                l.lock().unwrap().push("local first");
            })
        })
        .unwrap()
        .unwrap();
    let l = Arc::clone(&log);
    let id = OwnedObject::object_id(&context);
    signal.connect_object(id, thread, ConnectionType::Auto, move |_| {
        l.lock().unwrap().push("ordinary second");
    });
    let l = Arc::clone(&log);
    context
        .with(|c| {
            signal.connect_local_to(c, ConnectionType::Auto, move |_| {
                l.lock().unwrap().push("local third")
            })
        })
        .unwrap()
        .unwrap();

    let remote = signal.clone();
    std::thread::spawn(move || remote.emit(&1)).join().unwrap();
    event_loop.process_events(false);
    assert_eq!(
        *log.lock().unwrap(),
        vec!["local first", "ordinary second", "local third"]
    );
    assert_eq!(rc_in_slot.get(), 1);
}

#[test]
fn many_emitters_reach_the_local_slot_exactly_once_each() {
    const THREADS: usize = 3;
    const EMITS: usize = 8;
    let event_loop = EventLoop::new();
    let context = Receiver::registered();
    let signal: Signal<usize> = Signal::new();
    let total = Rc::new(Cell::new(0usize));
    let sink = Rc::clone(&total);
    context
        .with(|c| {
            signal.connect_local_to(c, ConnectionType::Auto, move |v| sink.set(sink.get() + *v))
        })
        .unwrap()
        .unwrap();

    let started = Arc::new(AtomicBool::new(false));
    let workers: Vec<_> = (0..THREADS)
        .map(|_| {
            let remote = signal.clone();
            let started = Arc::clone(&started);
            std::thread::spawn(move || {
                started.store(true, Ordering::SeqCst);
                for _ in 0..EMITS {
                    remote.emit(&1);
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    while event_loop.process_events(false) {}
    assert_eq!(total.get(), THREADS * EMITS);
}

#[test]
fn a_local_slot_connected_to_a_context_is_dropped_on_its_thread_when_another_thread_disconnects() {
    let event_loop = EventLoop::new();
    let context = Receiver::registered();
    let signal: Signal<i32> = Signal::new();
    let total = Rc::new(Cell::new(0));
    let sink = Rc::clone(&total);
    let id = context
        .with(|c| {
            signal.connect_local_to(c, ConnectionType::Auto, move |v| sink.set(sink.get() + *v))
        })
        .unwrap()
        .unwrap();

    let remote = signal.clone();
    assert!(std::thread::spawn(move || remote.disconnect(id))
        .join()
        .unwrap());
    assert_eq!(Rc::strong_count(&total), 2);
    assert!(event_loop.process_events(false));
    assert_eq!(Rc::strong_count(&total), 1);
}
