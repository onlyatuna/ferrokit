// SPDX-License-Identifier: LGPL-3.0-only
//! A `BlockingQueuedConnection` waits for its call to be delivered or dropped, never for a call
//! nobody can deliver any more.
//!
//! Qt releases the waiting caller from the destructor of the posted `QMetaCallEvent`, so deleting
//! an event that was never delivered (the receiver is gone, the thread's posted events are
//! deleted with its `QThreadData`) wakes the caller. Here the waiting side owns the receiving end
//! of a channel whose sending end is inside the posted call, so the call must be dropped, not
//! kept, when its queue can no longer be turned.
//!
//! What stays unbounded, as in Qt: a receiver thread that is alive but does not run its loop.
//!
//! Every test that can block runs the blocking side on a helper thread and waits for it with a
//! watchdog, so a regression fails the test instead of hanging the run.

use std::any::Any;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::event_loop::EventLoop;
use ferrokit_core::meta::{
    Access, InvokeError, MetaMethod, MetaObject, MethodType, QOBJECT_META_OBJECT,
};
use ferrokit_core::object::{
    invoke_method_on_thread, register_qobject_ptr, unregister_qobject, EventSender, ObjectData,
    ObjectId, QObject, ThreadId,
};
use ferrokit_core::signal::{ConnectionType, Signal};
use ferrokit_core::variant::Variant;

const WATCHDOG: Duration = Duration::from_secs(10);
const ADD_WIDTH: &str = "addWidth(int)";

struct Widget {
    data: ObjectData,
    width: i32,
}

impl QObject for Widget {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }

    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
    }

    fn as_qobject_any(&self) -> Option<&dyn Any> {
        Some(self)
    }

    fn as_qobject_any_mut(&mut self) -> Option<&mut dyn Any> {
        Some(self)
    }

    fn meta_object(&self) -> &'static MetaObject {
        &WIDGET_META_OBJECT
    }
}

fn add_width(obj: &mut dyn Any, args: &[Variant]) -> Result<Variant, InvokeError> {
    let widget = obj
        .downcast_mut::<Widget>()
        .ok_or(InvokeError::TargetBorrowFailed)?;
    let delta =
        args.first()
            .and_then(|v| v.to_value::<i32>())
            .ok_or(InvokeError::TypeMismatch {
                index: 0,
                expected: "i32",
            })?;
    widget.width += delta;
    Ok(Variant::from(widget.width))
}

static WIDGET_METHODS: &[MetaMethod] = &[MetaMethod::new(
    "addWidth",
    ADD_WIDTH,
    "int",
    &["int"],
    &["delta"],
    MethodType::Method,
    Access::Public,
    Some(add_width),
)];

static WIDGET_META_OBJECT: MetaObject = MetaObject::new(
    "BoundWidget",
    Some(&QOBJECT_META_OBJECT),
    WIDGET_METHODS,
    &[],
    &[],
    &[],
);

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

/// A thread that registers a receiver and runs its event loop, and that leaves the loop while a
/// call that the loop never took is still queued. It then stays alive without a loop until
/// `finish`, so the emitter can only be released by dropping the call, not by the thread ending.
///
/// The call is left behind by one turn of the loop: it delivers a first call, which waits until
/// the caller's call is queued, and then a `Quit`. The caller's call is posted after the turn
/// began, so the turn does not deliver it, and the loop ends with the `Quit`.
struct LoopThatLeavesACall {
    thread: ThreadId,
    receiver: ObjectId,
    /// The first call has started: the caller may post now.
    go: Option<mpsc::Receiver<()>>,
    /// Like the sender an `EventLoopThreadHandle` keeps: a handle on the loop's queue that
    /// outlives the loop.
    _queue_handle: EventSender,
    release: Option<mpsc::Sender<()>>,
    join: Option<thread::JoinHandle<()>>,
}

impl LoopThatLeavesACall {
    fn start() -> Self {
        let (ready_tx, ready_rx) = mpsc::channel();
        let (go_tx, go_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let id = ObjectId::next();
        let join = thread::spawn(move || {
            let el = EventLoop::new();
            let mut widget = Widget {
                data: ObjectData::new(id),
                width: 0,
            };
            // SAFETY: `widget` stays on this thread, in place, until it is unregistered below.
            unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut widget)) };

            let sender = el.sender();
            let queue = Arc::clone(&sender.queue);
            sender.post_event(
                ObjectId(0),
                Event::new(EventKind::MetaCallNoTarget(Box::new(move || {
                    let _ = go_tx.send(());
                    let deadline = Instant::now() + WATCHDOG;
                    // Behind the `Quit`, which is still queued, so two events wait.
                    while queue.lock().unwrap().len() < 2 && Instant::now() < deadline {
                        thread::sleep(Duration::from_millis(1));
                    }
                }))),
            );
            sender.post_event(ObjectId(0), Event::new(EventKind::Quit { exit_code: 0 }));
            ready_tx
                .send((ThreadId::current(), sender))
                .expect("the test waits for the receiver thread");

            el.exec();
            // The loop ends with the caller's call still queued.
            drop(el);
            // Alive, no loop, until the test is done. Returns on `finish` or when the test is
            // dropped by a failed assertion.
            let _ = release_rx.recv();
            // SAFETY: registered above on this thread, and still alive.
            unsafe { unregister_qobject(id) };
        });
        let (thread, queue_handle) = ready_rx
            .recv_timeout(WATCHDOG)
            .expect("the receiver thread must report its id");
        Self {
            thread,
            receiver: id,
            go: Some(go_rx),
            _queue_handle: queue_handle,
            release: Some(release_tx),
            join: Some(join),
        }
    }

    /// Runs `blocking` on a helper thread once the loop is inside its first call, and returns
    /// what it returned. Fails, instead of waiting on, a `blocking` that does not return.
    fn call_when_ready<R: Send + 'static>(
        &mut self,
        blocking: impl FnOnce() -> R + Send + 'static,
    ) -> R {
        let go = self.go.take().expect("one blocking call per loop");
        let (done_tx, done_rx) = mpsc::channel();
        let helper = thread::spawn(move || {
            go.recv_timeout(WATCHDOG)
                .expect("the loop must reach its first call");
            let _ = done_tx.send(blocking());
        });
        let result = done_rx.recv_timeout(WATCHDOG).expect(
            "the blocking call did not return: the call was left in a queue that no loop turns, \
             and its caller was not released",
        );
        helper.join().expect("blocking helper must not panic");
        result
    }

    fn finish(mut self) {
        drop(self.release.take());
        self.join
            .take()
            .expect("finished once")
            .join()
            .expect("receiver thread must not panic");
    }
}

impl Drop for LoopThatLeavesACall {
    fn drop(&mut self) {
        // After a failed assertion: let the thread end, which also drops what is left.
        drop(self.release.take());
    }
}

/// The posted call is a closure holding a value; this reports whether the closure was dropped.
#[test]
fn undelivered_call_is_dropped_when_the_loop_ends_while_a_queue_handle_survives() {
    let el = EventLoop::new();
    let queue_handle = el.sender();
    let witness = Arc::new(());
    let held = Arc::clone(&witness);
    queue_handle.post_event(
        ObjectId(0),
        Event::new(EventKind::MetaCallNoTarget(Box::new(move || {
            drop(held);
        }))),
    );
    assert_eq!(
        Arc::strong_count(&witness),
        2,
        "the call is queued, not run"
    );

    drop(el);

    assert_eq!(
        Arc::strong_count(&witness),
        1,
        "the loop is gone, so nothing can deliver the queued call: it must be dropped \
         even though a handle on the queue is still alive"
    );
    drop(queue_handle);
}

/// Signal path: the connection's call is queued behind a `Quit` and left there when the loop ends.
#[test]
fn blocking_queued_emit_returns_when_loop_ends_with_the_call_queued() {
    let mut remote = LoopThatLeavesACall::start();
    let signal = Arc::new(Signal::<i32>::new());
    let (ran, slot) = counting_slot();
    let _c = signal.connect_blocking_queued(remote.receiver, remote.thread, slot);

    {
        let signal = Arc::clone(&signal);
        remote.call_when_ready(move || signal.emit(&1));
    }

    assert_eq!(
        ran.load(Ordering::SeqCst),
        0,
        "a call that was never delivered must not run"
    );
    remote.finish();
}

/// Invoke path: the same, through `invoke_method_on_thread`.
#[test]
fn blocking_queued_invoke_returns_when_loop_ends_with_the_call_queued() {
    let mut remote = LoopThatLeavesACall::start();
    let receiver = remote.receiver;

    let result = remote.call_when_ready(move || {
        invoke_method_on_thread(
            receiver,
            &WIDGET_META_OBJECT,
            ADD_WIDTH,
            &[Variant::from(3i32)],
            ConnectionType::BlockingQueued,
        )
    });

    assert_eq!(
        result,
        Err(InvokeError::TargetBorrowFailed),
        "a call that was never delivered reports that its target could not be reached"
    );
    remote.finish();
}

/// Runs `signal.emit` on a helper thread and waits for it with the watchdog.
fn emit_on_helper_thread(signal: &Arc<Signal<i32>>) {
    let (done_tx, done_rx) = mpsc::channel();
    let helper = {
        let signal = Arc::clone(signal);
        thread::spawn(move || {
            signal.emit(&1);
            let _ = done_tx.send(());
        })
    };
    done_rx
        .recv_timeout(WATCHDOG)
        .expect("blocking queued emit did not return: the emitter is still waiting");
    helper.join().expect("emitter thread must not panic");
}

/// The receiver's thread ended: the call is dropped when it is posted, and the emitter returns.
#[test]
fn blocking_queued_emit_returns_when_the_receiver_thread_has_ended() {
    let ended = thread::spawn(ThreadId::current)
        .join()
        .expect("the helper thread returns its id");
    let signal = Arc::new(Signal::<i32>::new());
    let (ran, slot) = counting_slot();
    let _c = signal.connect_blocking_queued(ObjectId(0), ended, slot);

    emit_on_helper_thread(&signal);

    assert_eq!(
        ran.load(Ordering::SeqCst),
        0,
        "no loop can run the call of an ended thread"
    );
}

/// Emitter and receiver on one thread: Qt reports a dead lock for this combination
/// (`QMetaObject::activate`, qobject.cpp). Here the slot runs in place and the emit returns, which
/// is what this crate has always done; the call is neither queued nor waited for.
#[test]
fn blocking_queued_emit_on_the_receiver_thread_runs_the_slot_in_place() {
    let (done_tx, done_rx) = mpsc::channel();
    let helper = thread::spawn(move || {
        let signal = Signal::<i32>::new();
        let (ran, slot) = counting_slot();
        let _c = signal.connect_blocking_queued(ObjectId(0), ThreadId::current(), slot);
        signal.emit(&1);
        let _ = done_tx.send(ran.load(Ordering::SeqCst));
    });

    let ran = done_rx
        .recv_timeout(WATCHDOG)
        .expect("a blocking queued emit on the receiver's own thread must not wait for itself");
    helper.join().expect("helper thread must not panic");
    assert_eq!(ran, 1);
}
