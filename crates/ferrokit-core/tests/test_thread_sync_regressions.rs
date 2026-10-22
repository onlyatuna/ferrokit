// SPDX-License-Identifier: LGPL-3.0-only
//! Regression tests for the thread/sync, future, pool, task and channel primitives.
//!
//! The `GUARD` timeout below only turns a lost wakeup into a test failure instead of a hang; no
//! assertion depends on how long something takes.

use parking_lot::Mutex as PlMutex;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self as std_thread, yield_now};
use std::time::{Duration, Instant};

use ferrokit_core::event_loop::EventLoop;
use ferrokit_core::signal::Signal;
use ferrokit_core::thread::{
    bounded, channel, FutureSynchronizer, Mutex, Promise, RecvError, RecvTimeoutError, Semaphore,
    TaskError, TaskStatus, ThreadPool, TryRecvError, WaitCondition,
};

const GUARD: Duration = Duration::from_secs(10);

struct CountingWaker(AtomicUsize);

impl Wake for CountingWaker {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

fn counting_waker() -> (Arc<CountingWaker>, Waker) {
    let counter = Arc::new(CountingWaker(AtomicUsize::new(0)));
    let waker = Waker::from(Arc::clone(&counter));
    (counter, waker)
}

/// Runs `f` on a helper thread and fails (instead of hanging) if it does not return in time.
fn within<R: Send + 'static>(f: impl FnOnce() -> R + Send + 'static) -> R {
    let (tx, rx) = mpsc::channel();
    std_thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(GUARD)
        .expect("operation did not complete: lost wakeup or deadlock")
}

fn wait_until_busy(pool: &ThreadPool, workers: usize) {
    while pool.active_thread_count() < workers {
        yield_now();
    }
}

// =============================================================================
// WaitCondition
// =============================================================================

/// `wait` must release the mutex exactly once. A second unlock after another thread has taken the
/// lock lets two threads hold the same `MutexGuard` at once.
#[test]
fn wait_condition_wait_releases_mutex_exactly_once() {
    const WAITERS: usize = 2;
    const CONTENDERS: usize = 4;
    const ROUNDS: usize = 200;

    let mutex = Arc::new(Mutex::new(()));
    let cond = Arc::new(WaitCondition::new());
    let inside = Arc::new(AtomicUsize::new(0));
    let violations = Arc::new(AtomicUsize::new(0));
    let waiters_done = Arc::new(AtomicUsize::new(0));

    let enter = {
        let inside = Arc::clone(&inside);
        let violations = Arc::clone(&violations);
        move || {
            if inside.fetch_add(1, Ordering::SeqCst) != 0 {
                violations.fetch_add(1, Ordering::SeqCst);
            }
        }
    };
    let leave = {
        let inside = Arc::clone(&inside);
        move || {
            inside.fetch_sub(1, Ordering::SeqCst);
        }
    };

    let mut threads = Vec::new();
    for _ in 0..WAITERS {
        let (mutex, cond, done) = (
            Arc::clone(&mutex),
            Arc::clone(&cond),
            Arc::clone(&waiters_done),
        );
        let (enter, leave) = (enter.clone(), leave.clone());
        threads.push(std_thread::spawn(move || {
            for _ in 0..ROUNDS {
                let guard = mutex.lock().unwrap();
                enter();
                leave();
                let guard = cond.wait(guard);
                enter();
                leave();
                drop(guard);
            }
            done.fetch_add(1, Ordering::SeqCst);
        }));
    }
    for _ in 0..CONTENDERS {
        let (mutex, done) = (Arc::clone(&mutex), Arc::clone(&waiters_done));
        let (enter, leave) = (enter.clone(), leave.clone());
        threads.push(std_thread::spawn(move || {
            while done.load(Ordering::SeqCst) < WAITERS {
                let guard = mutex.lock().unwrap();
                enter();
                yield_now();
                leave();
                drop(guard);
                // `Mutex` barges: without a pause a contender re-locks before the woken waiter
                // runs, which on slow runners starves the waiters past `GUARD`.
                yield_now();
            }
        }));
    }
    {
        // Keeps every waiter supplied with wakeups until it has finished all of its rounds.
        let (cond, done) = (Arc::clone(&cond), Arc::clone(&waiters_done));
        threads.push(std_thread::spawn(move || {
            while done.load(Ordering::SeqCst) < WAITERS {
                cond.wake_all();
                yield_now();
            }
        }));
    }
    within(move || {
        for t in threads {
            t.join().unwrap();
        }
    });
    assert_eq!(
        violations.load(Ordering::SeqCst),
        0,
        "two threads held the same Mutex guard at once"
    );
}

#[test]
fn wait_condition_wait_timeout_reacquires_free_mutex() {
    let mutex = Mutex::new(5_u32);
    let cond = WaitCondition::new();
    let guard = mutex.lock().unwrap();
    let (acquired, signaled) = cond.wait_timeout(guard, Duration::from_millis(1));
    assert!(!signaled, "nobody woke the condition");
    assert_eq!(
        *acquired.expect("mutex is free, so it must be re-acquired"),
        5
    );
}

// =============================================================================
// Future / Promise
// =============================================================================

#[test]
fn future_poll_is_woken_by_cancel() {
    let promise = Promise::<i32>::new();
    let mut future = promise.future();
    let (counter, waker) = counting_waker();
    let mut cx = Context::from_waker(&waker);
    assert!(matches!(Pin::new(&mut future).poll(&mut cx), Poll::Pending));

    future.cancel();
    assert!(
        counter.0.load(Ordering::SeqCst) >= 1,
        "canceling must wake a task awaiting the future"
    );
    assert!(matches!(
        Pin::new(&mut future).poll(&mut cx),
        Poll::Ready(None)
    ));
}

#[test]
fn promise_cancel_wakes_poll() {
    let promise = Promise::<i32>::new();
    let mut future = promise.future();
    let (counter, waker) = counting_waker();
    let mut cx = Context::from_waker(&waker);
    assert!(matches!(Pin::new(&mut future).poll(&mut cx), Poll::Pending));
    promise.cancel();
    assert!(counter.0.load(Ordering::SeqCst) >= 1);
}

#[test]
fn promise_add_result_wakes_poll() {
    let promise = Promise::<i32>::new();
    let mut future = promise.future();
    let (counter, waker) = counting_waker();
    let mut cx = Context::from_waker(&waker);
    assert!(matches!(Pin::new(&mut future).poll(&mut cx), Poll::Pending));
    promise.add_result(7);
    assert!(
        counter.0.load(Ordering::SeqCst) >= 1,
        "a first result makes poll ready, so it must wake the task"
    );
    assert!(matches!(
        Pin::new(&mut future).poll(&mut cx),
        Poll::Ready(Some(7))
    ));
}

#[test]
fn poll_racing_finish_is_never_lost() {
    for _ in 0..2_000 {
        let promise = Promise::<i32>::new();
        let mut future = promise.future();
        let finisher = std_thread::spawn(move || promise.finish());
        let (counter, waker) = counting_waker();
        let mut cx = Context::from_waker(&waker);
        let polled = Pin::new(&mut future).poll(&mut cx);
        finisher.join().unwrap();
        if matches!(polled, Poll::Pending) {
            assert!(
                counter.0.load(Ordering::SeqCst) >= 1,
                "Pending poll whose waker was never woken after finish"
            );
        }
    }
}

#[test]
fn promise_dropped_without_finish_cancels_and_finishes() {
    let promise = Promise::<i32>::new();
    let future = promise.future();
    drop(promise);
    assert!(future.is_canceled());
    assert!(future.is_finished());
    assert_eq!(within(move || future.wait_result()), None);
}

#[test]
fn promise_drop_after_finish_does_not_cancel() {
    let promise = Promise::<i32>::new();
    let future = promise.future();
    promise.set_value(3);
    drop(promise);
    assert!(future.is_finished());
    assert!(!future.is_canceled());
}

#[test]
fn future_synchronizer_waits_for_finish_not_first_result() {
    for _ in 0..50 {
        let promise = Promise::<i32>::new();
        let mut sync = FutureSynchronizer::new();
        sync.add_future(promise.future());
        promise.add_result(1);

        let gate = Arc::new(Semaphore::new(0));
        let producer = {
            let gate = Arc::clone(&gate);
            std_thread::spawn(move || {
                gate.acquire(1);
                promise.finish();
            })
        };
        gate.release(1);
        sync.wait_for_finished();
        assert!(
            sync.futures()[0].is_finished(),
            "wait_for_finished returned on the first result"
        );
        producer.join().unwrap();
    }
}

#[test]
fn watcher_on_finished_future_replays_state() {
    let el = EventLoop::new();
    let promise = Promise::<i32>::new();
    promise.add_result(5);
    promise.set_value(6);
    let watcher = promise.future().watcher();

    let results = Arc::new(PlMutex::new(Vec::new()));
    let finished = Arc::new(AtomicUsize::new(0));
    {
        let results = Arc::clone(&results);
        watcher
            .result_ready()
            .connect(move |v: &i32| results.lock().push(*v));
        let finished = Arc::clone(&finished);
        watcher.finished().connect(move |_: &()| {
            finished.fetch_add(1, Ordering::SeqCst);
        });
    }
    el.process_events(false);
    assert_eq!(*results.lock(), vec![5, 6]);
    assert_eq!(finished.load(Ordering::SeqCst), 1);
}

#[test]
fn watcher_on_canceled_future_replays_cancel() {
    let el = EventLoop::new();
    let promise = Promise::<i32>::new();
    promise.cancel();
    let watcher = promise.future().watcher();
    let canceled = Arc::new(AtomicUsize::new(0));
    {
        let canceled = Arc::clone(&canceled);
        watcher.canceled().connect(move |_: &()| {
            canceled.fetch_add(1, Ordering::SeqCst);
        });
    }
    el.process_events(false);
    assert_eq!(canceled.load(Ordering::SeqCst), 1);
}

// =============================================================================
// ThreadPool / TaskHandle
// =============================================================================

#[test]
fn clear_queue_cancels_queued_tasks() {
    let pool = ThreadPool::new(1);
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let first = pool.spawn(move || {
        release_rx.recv().unwrap();
        1
    });
    wait_until_busy(&pool, 1);
    let queued = pool.spawn(|| 2);
    assert_eq!(pool.queued_task_count(), 1);

    pool.clear_queue();

    assert_eq!(
        queued.wait_timeout(GUARD),
        Err(TaskError::Canceled),
        "a task dropped by clear_queue must complete as canceled"
    );
    assert_eq!(queued.status(), TaskStatus::Canceled);
    release_tx.send(()).unwrap();
    assert_eq!(first.join(), Ok(1));
}

#[test]
fn pool_drop_cancels_queued_tasks() {
    let pool = ThreadPool::new(1);
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let _first = pool.spawn(move || {
        release_rx.recv().unwrap();
    });
    wait_until_busy(&pool, 1);
    let queued = pool.spawn(|| 2);

    let dropper = std_thread::spawn(move || drop(pool));
    // The queued task must be canceled during teardown, while the running task is still blocked.
    assert_eq!(queued.wait_timeout(GUARD), Err(TaskError::Canceled));
    release_tx.send(()).unwrap();
    dropper.join().unwrap();
}

#[test]
fn clear_queue_wakes_wait_for_done() {
    let pool = ThreadPool::new(1);
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let first = pool.spawn(move || release_rx.recv().unwrap());
    wait_until_busy(&pool, 1);
    let _queued = pool.spawn(|| ());
    pool.clear_queue();
    release_tx.send(()).unwrap();
    first.join().unwrap();
    assert!(pool.wait_for_done_timeout(GUARD));
}

#[test]
fn burst_submit_uses_all_allowed_workers() {
    const WORKERS: usize = 4;
    for _ in 0..30 {
        let pool = ThreadPool::new(WORKERS);
        let arrived = Arc::new(AtomicUsize::new(0));
        let handles: Vec<_> = (0..WORKERS)
            .map(|_| {
                let arrived = Arc::clone(&arrived);
                pool.spawn(move || {
                    arrived.fetch_add(1, Ordering::SeqCst);
                    let deadline = Instant::now() + Duration::from_secs(5);
                    while arrived.load(Ordering::SeqCst) < WORKERS {
                        if Instant::now() >= deadline {
                            return false;
                        }
                        yield_now();
                    }
                    true
                })
            })
            .collect();
        for h in handles {
            assert_eq!(
                h.join(),
                Ok(true),
                "tasks that rendezvous must run concurrently"
            );
        }
    }
}

#[test]
fn raising_max_thread_count_starts_workers_for_queued_tasks() {
    let pool = ThreadPool::new(1);
    let (tx, rx) = mpsc::channel::<()>();
    let waiter = pool.spawn(move || rx.recv_timeout(GUARD).is_ok());
    wait_until_busy(&pool, 1);
    let signaler = pool.spawn(move || tx.send(()).unwrap());
    assert_eq!(pool.queued_task_count(), 1);

    pool.set_max_thread_count(2);

    assert_eq!(
        waiter.join(),
        Ok(true),
        "the queued task never got a worker"
    );
    signaler.join().unwrap();
}

#[test]
fn canceling_a_queued_task_wakes_its_poll_waker() {
    let pool = ThreadPool::new(1);
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let first = pool.spawn(move || release_rx.recv().unwrap());
    wait_until_busy(&pool, 1);
    let mut queued = pool.spawn(|| 2);

    let (counter, waker) = counting_waker();
    let mut cx = Context::from_waker(&waker);
    assert!(matches!(Pin::new(&mut queued).poll(&mut cx), Poll::Pending));
    assert!(queued.cancel());
    assert!(
        counter.0.load(Ordering::SeqCst) >= 1,
        "cancel must wake awaiting tasks"
    );
    assert!(matches!(
        Pin::new(&mut queued).poll(&mut cx),
        Poll::Ready(Err(TaskError::Canceled))
    ));
    release_tx.send(()).unwrap();
    first.join().unwrap();
}

/// `cancel()` returning `true` means the task never runs; `false` means it already started.
#[test]
fn cancel_result_matches_whether_task_ran() {
    let pool = ThreadPool::new(2);
    for _ in 0..2_000 {
        let ran = Arc::new(AtomicBool::new(false));
        let body_ran = Arc::clone(&ran);
        let handle = pool.spawn_cancellable(move |_| {
            body_ran.store(true, Ordering::SeqCst);
            1
        });
        let canceled = handle.cancel();
        let result = handle.wait_result();
        assert!(pool.wait_for_done_timeout(GUARD));
        if canceled {
            assert_eq!(result, Err(TaskError::Canceled));
            assert!(!ran.load(Ordering::SeqCst), "canceled task still ran");
            assert_eq!(handle.status(), TaskStatus::Canceled);
        } else {
            assert_eq!(
                result,
                Ok(1),
                "cancel() reported false but the task did not run"
            );
            assert!(ran.load(Ordering::SeqCst));
        }
    }
}

#[test]
fn other_clones_do_not_hang_after_join_takes_the_result() {
    let pool = ThreadPool::new(1);
    let first = pool.spawn(|| 11);
    let clone = first.clone();
    assert_eq!(first.join(), Ok(11));
    let outcome = within(move || clone.wait_result());
    assert!(outcome.is_err(), "the result was moved out by join");
}

// =============================================================================
// Channels
// =============================================================================

#[test]
fn recv_reports_disconnected_when_all_senders_are_dropped() {
    let (tx, rx) = channel::<i32>();
    let tx2 = tx.clone();
    tx.send(1).unwrap();
    drop(tx);
    assert_eq!(rx.try_recv(), Ok(1));
    assert_eq!(
        rx.try_recv(),
        Err(TryRecvError::Empty),
        "a sender is still alive"
    );
    drop(tx2);
    assert_eq!(rx.try_recv(), Err(TryRecvError::Disconnected));
    assert_eq!(
        rx.recv_timeout(Duration::from_millis(1)),
        Err(RecvTimeoutError::Disconnected)
    );
    assert_eq!(within(move || rx.recv()), Err(RecvError));
}

#[test]
fn blocked_recv_returns_when_last_sender_drops() {
    let (tx, rx) = channel::<i32>();
    let receiver = std_thread::spawn(move || {
        let mut got = Vec::new();
        while let Ok(v) = rx.recv() {
            got.push(v);
        }
        got
    });
    tx.send(1).unwrap();
    tx.send(2).unwrap();
    drop(tx);
    assert_eq!(within(move || receiver.join().unwrap()), vec![1, 2]);
}

#[test]
fn bounded_blocked_send_fails_when_receiver_closes() {
    let (tx, rx) = bounded::<i32>(1);
    tx.send(1).unwrap();
    let sender = std_thread::spawn(move || tx.send(2));
    // The sender may or may not have parked yet; close must wake it either way.
    rx.close();
    assert!(within(move || sender.join().unwrap()).is_err());
}

#[test]
fn close_never_loses_a_wakeup() {
    for _ in 0..500 {
        let (tx, rx) = channel::<i32>();
        let rx = Arc::new(rx);
        let receiver = {
            let rx = Arc::clone(&rx);
            std_thread::spawn(move || rx.recv())
        };
        rx.close();
        assert_eq!(within(move || receiver.join().unwrap()), Err(RecvError));
        drop(tx);
    }
}

#[test]
fn signal_slot_may_use_the_channel_it_was_emitted_from() {
    let (tx, rx) = channel::<i32>();
    let rx = Arc::new(rx);
    let signal = Signal::<i32>::new();
    rx.connect_to_signal(&signal);

    let slot_rx = Arc::clone(&rx);
    let seen = Arc::new(PlMutex::new(Vec::new()));
    let slot_seen = Arc::clone(&seen);
    signal.connect(move |v: &i32| {
        // Would self-deadlock if the emission ran under the queue lock.
        let _ = slot_rx.try_recv();
        slot_seen.lock().push(*v);
    });

    within(move || {
        tx.send(1).unwrap();
        tx.send(2).unwrap();
    });
    assert_eq!(*seen.lock(), vec![1, 2]);
}

#[test]
fn signal_emits_each_sent_value_exactly_once_under_concurrency() {
    const SENDERS: i32 = 4;
    const PER_SENDER: i32 = 500;
    let (tx, rx) = channel::<i32>();
    let rx = Arc::new(rx);
    let signal = Signal::<i32>::new();
    rx.connect_to_signal(&signal);

    let emitted = Arc::new(PlMutex::new(Vec::new()));
    {
        let emitted = Arc::clone(&emitted);
        signal.connect(move |v: &i32| emitted.lock().push(*v));
    }

    let total = (SENDERS * PER_SENDER) as usize;
    let consumer = {
        let rx = Arc::clone(&rx);
        std_thread::spawn(move || {
            let mut taken = 0;
            while taken < total {
                match rx.try_recv() {
                    Ok(_) => taken += 1,
                    Err(_) => yield_now(),
                }
            }
        })
    };
    let senders: Vec<_> = (0..SENDERS)
        .map(|s| {
            let tx = tx.clone();
            std_thread::spawn(move || {
                for i in 0..PER_SENDER {
                    tx.send(s * PER_SENDER + i).unwrap();
                }
            })
        })
        .collect();
    within(move || {
        for s in senders {
            s.join().unwrap();
        }
        consumer.join().unwrap();
    });

    let mut got = emitted.lock().clone();
    got.sort_unstable();
    let expected: Vec<i32> = (0..SENDERS * PER_SENDER).collect();
    assert_eq!(
        got, expected,
        "every sent value must be emitted exactly once"
    );
}

#[test]
fn last_sender_drop_wakes_attached_event_senders() {
    use ferrokit_core::event_loop::EventQueue;
    use ferrokit_core::object::ThreadId;
    use ferrokit_core::thread::EventSender;

    let (tx, rx) = channel::<i32>();
    let woken = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&woken);
    rx.attach_event_sender(EventSender::new(
        ThreadId::current(),
        Arc::new(std::sync::Mutex::new(EventQueue::new())),
        Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }),
    ));
    let tx2 = tx.clone();
    drop(tx);
    assert_eq!(woken.load(Ordering::SeqCst), 0, "a sender is still alive");
    drop(tx2);
    assert!(
        woken.load(Ordering::SeqCst) >= 1,
        "event-loop consumers must be woken to observe Disconnected"
    );
    assert_eq!(rx.try_recv(), Err(TryRecvError::Disconnected));
}

struct PanicWaker;
impl Wake for PanicWaker {
    fn wake(self: Arc<Self>) {
        panic!("waker panics");
    }
}

#[test]
fn panicking_waker_does_not_break_pool_accounting() {
    let pool = ThreadPool::new(1);
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let mut handle = pool.spawn(move || release_rx.recv().unwrap());
    let waker = Waker::from(Arc::new(PanicWaker));
    let mut cx = Context::from_waker(&waker);
    assert!(Pin::new(&mut handle).poll(&mut cx).is_pending());
    release_tx.send(()).unwrap();
    assert!(pool.wait_for_done_timeout(GUARD));
    assert_eq!(pool.active_thread_count(), 0);
    // The worker survives and still runs jobs.
    assert_eq!(pool.spawn(|| 7).wait_timeout(GUARD), Ok(Some(7)));
}

struct FlagWaker(AtomicBool);
impl Wake for FlagWaker {
    fn wake(self: Arc<Self>) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[test]
fn panicking_waker_does_not_starve_other_wakers() {
    let pool = ThreadPool::new(1);
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let mut handle = pool.spawn(move || release_rx.recv().unwrap());
    let flag = Arc::new(FlagWaker(AtomicBool::new(false)));
    let bad = Waker::from(Arc::new(PanicWaker));
    let good = Waker::from(Arc::clone(&flag));
    assert!(Pin::new(&mut handle).poll(&mut Context::from_waker(&bad)).is_pending());
    assert!(Pin::new(&mut handle).poll(&mut Context::from_waker(&good)).is_pending());
    release_tx.send(()).unwrap();
    assert!(pool.wait_for_done_timeout(GUARD));
    assert!(flag.0.load(Ordering::SeqCst));
}
