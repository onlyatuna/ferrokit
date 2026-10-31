// SPDX-License-Identifier: LGPL-3.0-only
//! Regression reproductions for queued delivery.
//!
//! 1. Queued connections without a receiver (`connect_queued`, receiver `ObjectId(0)`) must keep
//!    delivering across event-loop turns. Delivering a fire-and-forget sentinel call must not cut
//!    them.
//! 2. A queued slot whose connection names a real registered receiver must be able to reach that
//!    receiver through `with_object_mut`, the same as a direct slot can.
//! 3. A queued call whose receiver is destroyed before delivery must not run, and reports `false`
//!    from `notify_helper`.
//! 4. A cross-thread `connect_blocking_queued` call that is skipped (receiver destroyed) or whose
//!    slot panics must still release the emitter, so `emit` returns.

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::event_loop::{notify_helper, EventLoop};
use ferrokit_core::object::{
    register_owned_qobject, with_object_mut, ObjectData, ObjectId, OwnedObject, QObject, ThreadId,
};
use ferrokit_core::signal::{ConnectionType, Signal};
use ferrokit_core::timer::Timer;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;

struct Receiver {
    data: ObjectData,
}

impl QObject for Receiver {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
    }
}

fn counting_slot() -> (Arc<AtomicUsize>, impl Fn(&i32) + Send + Sync + 'static) {
    let n = Arc::new(AtomicUsize::new(0));
    let slot = {
        let n = Arc::clone(&n);
        move |_: &i32| {
            n.fetch_add(1, Ordering::SeqCst);
        }
    };
    (n, slot)
}

/// Two `connect_queued` slots, one emit per event-loop turn: each slot must run once per emit.
#[test]
fn connect_queued_survives_sentinel_delivery_between_emits() {
    let signal: Signal<i32> = Signal::new();
    let thread = ThreadId::current();
    let (a, slot_a) = counting_slot();
    let (b, slot_b) = counting_slot();
    let _ca = signal.connect_queued(thread, slot_a);
    let _cb = signal.connect_queued(thread, slot_b);

    let event_loop = EventLoop::new();
    signal.emit(&1);
    event_loop.process_events(false);
    signal.emit(&2);
    event_loop.process_events(false);

    assert_eq!(
        (a.load(Ordering::SeqCst), b.load(Ordering::SeqCst)),
        (2, 2),
        "each queued slot must run once per emit; delivering the ObjectId(0) sentinel must not \
         disconnect connect_queued connections"
    );
}

/// Same as above, but the sentinel comes from a `Timer::single_shot(0, ..)` posted between the
/// two emits, as an application would do.
#[test]
fn connect_queued_survives_single_shot_zero_between_emits() {
    let signal: Signal<i32> = Signal::new();
    let thread = ThreadId::current();
    let (a, slot_a) = counting_slot();
    let _ca = signal.connect_queued(thread, slot_a);

    let fired = Arc::new(AtomicUsize::new(0));
    let event_loop = EventLoop::new();

    signal.emit(&1);
    {
        let fired = Arc::clone(&fired);
        Timer::single_shot(0, move || {
            fired.fetch_add(1, Ordering::SeqCst);
        });
    }
    event_loop.process_events(false);
    signal.emit(&2);
    event_loop.process_events(false);

    assert_eq!(fired.load(Ordering::SeqCst), 1, "single_shot(0) must fire");
    assert_eq!(
        a.load(Ordering::SeqCst),
        2,
        "queued slot must run for the emit made after a single_shot(0) was delivered"
    );
}

/// A queued slot bound to a registered receiver must see that receiver via `with_object_mut`,
/// as a direct slot bound to the same receiver does.
#[test]
fn queued_slot_can_access_its_registered_receiver() {
    let thread = ThreadId::current();
    // SAFETY: the `OwnedObject` stays on this thread; no callback of the receiver runs.
    let rcv = unsafe {
        register_owned_qobject(Receiver {
            data: ObjectData::with_auto_id(),
        })
    };
    let rid = OwnedObject::object_id(&rcv);

    let queued_signal: Signal<i32> = Signal::new();
    let queued_seen = Arc::new(AtomicUsize::new(0));
    {
        let queued_seen = Arc::clone(&queued_seen);
        queued_signal.connect_object(rid, thread, ConnectionType::Queued, move |_| {
            if with_object_mut(rid, |_| ()).is_some() {
                queued_seen.fetch_add(1, Ordering::SeqCst);
            }
        });
    }

    let direct_signal: Signal<i32> = Signal::new();
    let direct_seen = Arc::new(AtomicUsize::new(0));
    {
        let direct_seen = Arc::clone(&direct_seen);
        direct_signal.connect_object(rid, thread, ConnectionType::Direct, move |_| {
            if with_object_mut(rid, |_| ()).is_some() {
                direct_seen.fetch_add(1, Ordering::SeqCst);
            }
        });
    }

    let event_loop = EventLoop::new();
    queued_signal.emit(&1);
    event_loop.process_events(false);
    direct_signal.emit(&1);

    assert_eq!(
        direct_seen.load(Ordering::SeqCst),
        1,
        "control: direct slot must reach the receiver"
    );
    assert_eq!(
        queued_seen.load(Ordering::SeqCst),
        1,
        "queued slot must reach its registered receiver via with_object_mut, as the direct slot does"
    );
    drop(rcv);
}

/// Upper bound on any wait for a blocking emit. A regression that leaves `emit` blocked fails the
/// test here instead of hanging the test run.
const WATCHDOG: Duration = Duration::from_secs(10);

fn new_receiver() -> OwnedObject<Receiver> {
    // SAFETY: the owner stays on the thread that registers it and is dropped there; no callback of
    // the receiver runs while the test holds it.
    unsafe {
        register_owned_qobject(Receiver {
            data: ObjectData::with_auto_id(),
        })
    }
}

/// A queued call whose receiver was destroyed before delivery must not run, and must report that
/// it did not run: `notify_helper` returns `false` for both `MetaCallNoTarget` and `MetaCall`.
#[test]
fn dead_receiver_queued_calls_are_not_run_and_report_false() {
    let rcv = new_receiver();
    let rid = OwnedObject::object_id(&rcv);
    drop(rcv);

    let ran = Arc::new(AtomicUsize::new(0));
    let no_target_ran = Arc::clone(&ran);
    let handled = notify_helper(
        rid,
        &mut Event::new(EventKind::MetaCallNoTarget(Box::new(move || {
            no_target_ran.fetch_add(1, Ordering::SeqCst);
        }))),
    );
    assert!(
        !handled,
        "MetaCallNoTarget to a destroyed receiver must report not run"
    );

    let target_ran = Arc::clone(&ran);
    let handled = notify_helper(
        rid,
        &mut Event::new(EventKind::MetaCall(Box::new(
            move |_: &mut dyn std::any::Any| {
                target_ran.fetch_add(1, Ordering::SeqCst);
            },
        ))),
    );
    assert!(
        !handled,
        "MetaCall to a destroyed receiver must report not run"
    );
    assert_eq!(
        ran.load(Ordering::SeqCst),
        0,
        "no call may run for a destroyed receiver"
    );
}

/// A queued `connect_object` slot whose receiver is destroyed between emit and `process_events`
/// must not run. The `connect_queued` slot emitted in the same batch still runs, which shows the
/// queue was delivered.
#[test]
fn queued_object_slot_skipped_when_receiver_dropped_before_delivery() {
    let thread = ThreadId::current();
    let rcv = new_receiver();
    let rid = OwnedObject::object_id(&rcv);

    let signal: Signal<i32> = Signal::new();
    let (object_ran, object_slot) = counting_slot();
    let (control_ran, control_slot) = counting_slot();
    let _co = signal.connect_object(rid, thread, ConnectionType::Queued, object_slot);
    let _cc = signal.connect_queued(thread, control_slot);

    let event_loop = EventLoop::new();
    signal.emit(&1);
    drop(rcv);
    event_loop.process_events(false);

    assert_eq!(
        object_ran.load(Ordering::SeqCst),
        0,
        "a queued slot of a destroyed receiver must not run"
    );
    assert_eq!(
        control_ran.load(Ordering::SeqCst),
        1,
        "control: the ObjectId(0) queued slot emitted in the same batch must run"
    );
}

/// A receiver registered on its own thread, whose event loop is pumped until `finish`.
struct RemoteReceiver {
    id: ObjectId,
    thread: ThreadId,
    drop_tx: Option<mpsc::Sender<()>>,
    stop: Arc<AtomicBool>,
    join: Option<thread::JoinHandle<()>>,
}

impl RemoteReceiver {
    /// Starts a thread that registers a receiver on itself, then pumps its event loop. With
    /// `drop_first`, the thread destroys the receiver when `drop_receiver` is called, before its
    /// loop delivers anything.
    fn start(drop_first: bool) -> Self {
        let (ready_tx, ready_rx) = mpsc::channel();
        let (drop_tx, drop_rx) = mpsc::channel::<()>();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = Arc::clone(&stop);
        let join = thread::spawn(move || {
            let event_loop = EventLoop::new();
            let mut rcv = Some(new_receiver());
            let id = OwnedObject::object_id(rcv.as_ref().expect("just registered"));
            ready_tx
                .send((id, ThreadId::current()))
                .expect("test thread waits for the receiver");
            if drop_first {
                // Returns on the drop command or when the test drops the sender.
                let _ = drop_rx.recv();
                drop(rcv.take());
            }
            while !stop_thread.load(Ordering::SeqCst) {
                event_loop.process_events(false);
                thread::sleep(Duration::from_millis(1));
            }
            drop(rcv);
        });
        let (id, thread) = ready_rx
            .recv_timeout(WATCHDOG)
            .expect("receiver thread must register its object");
        Self {
            id,
            thread,
            drop_tx: Some(drop_tx),
            stop,
            join: Some(join),
        }
    }

    /// Tells the thread to destroy its receiver. Only meaningful when started with `drop_first`.
    fn drop_receiver(&self) {
        if let Some(tx) = &self.drop_tx {
            let _ = tx.send(());
        }
    }

    /// Stops the pump, joins the thread and reports whether it panicked.
    fn finish(mut self) -> thread::Result<()> {
        self.stop.store(true, Ordering::SeqCst);
        self.join.take().expect("finished once").join()
    }
}

impl Drop for RemoteReceiver {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.drop_tx.take();
    }
}

/// Emits `signal` on a helper thread and waits up to `WATCHDOG` for `emit` to return.
fn emit_and_wait(signal: &Arc<Signal<i32>>) {
    let (done_tx, done_rx) = mpsc::channel();
    let emitter = {
        let signal = Arc::clone(signal);
        thread::spawn(move || {
            signal.emit(&1);
            let _ = done_tx.send(());
        })
    };
    done_rx
        .recv_timeout(WATCHDOG)
        .expect("blocking queued emit did not return: the emitter is still waiting");
    emitter.join().expect("emitter thread must not panic");
}

/// Cross-thread `connect_blocking_queued`: the receiver is destroyed after the emitter has posted
/// its call, before the receiver's thread delivers it. The skipped call must release the emitter,
/// and the slot must not run. The sleep only makes that interleaving likely; the assertions hold
/// for any interleaving.
#[test]
fn blocking_queued_emit_returns_when_receiver_dropped_before_delivery() {
    let remote = RemoteReceiver::start(true);
    let signal = Arc::new(Signal::<i32>::new());
    let (ran, slot) = counting_slot();
    let _c = signal.connect_blocking_queued(remote.id, remote.thread, slot);

    let (done_tx, done_rx) = mpsc::channel();
    let emitter = {
        let signal = Arc::clone(&signal);
        thread::spawn(move || {
            signal.emit(&1);
            let _ = done_tx.send(());
        })
    };
    thread::sleep(Duration::from_millis(100));
    remote.drop_receiver();
    done_rx
        .recv_timeout(WATCHDOG)
        .expect("blocking queued emit did not return after its receiver was destroyed");
    emitter.join().expect("emitter thread must not panic");

    assert_eq!(
        ran.load(Ordering::SeqCst),
        0,
        "the slot of a destroyed receiver must not run"
    );
    remote.finish().expect("receiver thread must not panic");
}

/// Cross-thread `connect_blocking_queued` whose slot panics on the receiver's thread. The receiver's
/// event loop contains the panic and drops the in-flight call, so the call's sender is dropped and
/// `emit` returns. The receiver thread keeps running.
#[test]
fn blocking_queued_emit_returns_when_slot_panics() {
    let remote = RemoteReceiver::start(false);
    let signal = Arc::new(Signal::<i32>::new());
    let ran = Arc::new(AtomicUsize::new(0));
    let ran_slot = Arc::clone(&ran);
    let _c = signal.connect_blocking_queued(remote.id, remote.thread, move |_: &i32| {
        ran_slot.fetch_add(1, Ordering::SeqCst);
        panic!("slot panics on the receiver thread");
    });

    emit_and_wait(&signal);

    assert_eq!(
        ran.load(Ordering::SeqCst),
        1,
        "the slot must have run on the receiver thread"
    );
    assert!(
        remote.finish().is_ok(),
        "the receiver's event loop must contain the slot panic, not die with it"
    );
}
