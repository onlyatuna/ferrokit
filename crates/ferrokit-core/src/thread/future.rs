// SPDX-License-Identifier: LGPL-3.0-only
use std::future::Future as StdFuture;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use crate::event::{Event, EventKind};
use crate::event_loop::post_event_to_thread;
use crate::object::{ObjectId, ThreadId};
use crate::signal::Signal;

pub(crate) struct FutureShared<T> {
    pub(crate) results: Mutex<Vec<T>>,
    pub(crate) is_finished: AtomicBool,
    pub(crate) is_canceled: AtomicBool,
    pub(crate) progress_min: AtomicI32,
    pub(crate) progress_max: AtomicI32,
    pub(crate) progress_value: AtomicI32,
    pub(crate) cond: Condvar,
    pub(crate) wakers: Mutex<Vec<Waker>>,
    pub(crate) watchers: Mutex<Vec<Arc<FutureWatcherSignals<T>>>>,
}

pub(crate) struct FutureWatcherSignals<T> {
    /// Thread that owns the watcher (Qt: QObject thread affinity, qobject.cpp:991-993).
    thread: ThreadId,
    /// Cleared by `FutureWatcher::drop`; posted callouts check it (Qt: removePostedEvents, qfuturewatcher.cpp:480-483).
    alive: AtomicBool,
    pub(crate) finished: Signal<()>,
    pub(crate) result_ready: Signal<T>,
    pub(crate) canceled: Signal<()>,
    pub(crate) progress_changed: Signal<i32>,
    pub(crate) progress_range_changed: Signal<(i32, i32)>,
}

impl<T: Clone + Send + 'static> FutureWatcherSignals<T> {
    fn new(thread: ThreadId) -> Self {
        Self {
            thread,
            alive: AtomicBool::new(true),
            finished: Signal::new(),
            result_ready: Signal::new(),
            canceled: Signal::new(),
            progress_changed: Signal::new(),
            progress_range_changed: Signal::new(),
        }
    }

    /// Queues `deliver` to the watcher's thread (Qt: postCallOutEvent, qfuturewatcher.cpp:468-478).
    fn post(self: &Arc<Self>, deliver: impl FnOnce(&Self) + Send + 'static) {
        let w = Arc::clone(self);
        post_event_to_thread(
            self.thread,
            ObjectId(0),
            Event::new(EventKind::MetaCall(Box::new(
                move |_: &mut dyn std::any::Any| {
                    if w.alive.load(Ordering::SeqCst) {
                        deliver(&*w);
                    }
                },
            ))),
        );
    }
}

impl<T> FutureShared<T> {
    pub(crate) fn new() -> Self {
        Self {
            results: Mutex::new(Vec::new()),
            is_finished: AtomicBool::new(false),
            is_canceled: AtomicBool::new(false),
            progress_min: AtomicI32::new(0),
            progress_max: AtomicI32::new(100),
            progress_value: AtomicI32::new(0),
            cond: Condvar::new(),
            wakers: Mutex::new(Vec::new()),
            watchers: Mutex::new(Vec::new()),
        }
    }

    fn live_watchers(&self) -> Vec<Arc<FutureWatcherSignals<T>>> {
        let mut list = self.watchers.lock().unwrap();
        list.retain(|w| w.alive.load(Ordering::SeqCst));
        list.clone()
    }

    /// Wakes every task parked in `poll`.
    fn wake_wakers(&self) {
        let wakers = std::mem::take(&mut *self.wakers.lock().unwrap());
        for waker in wakers {
            waker.wake();
        }
    }

    /// Orders a just-stored `is_finished`/`is_canceled` flag against waiters. The condvar is
    /// paired with `results`, but the flags are atomics: a waiter that checked them while holding
    /// `results` and has not parked yet would otherwise miss a `notify_all` issued right after the
    /// store. Taking `results` once the flag is visible guarantees the waiter is either already
    /// parked (and gets notified) or re-checks the flag.
    fn sync_with_waiters(&self) {
        drop(self.results.lock().unwrap());
    }
}

impl<T: Clone + Send + 'static> FutureShared<T> {
    fn cancel(&self) {
        let watchers = {
            let mut list = self.watchers.lock().unwrap();
            // Qt: QFutureInterfaceBase::cancel is a no-op once canceled.
            if self.is_canceled.swap(true, Ordering::SeqCst) {
                return;
            }
            list.retain(|w| w.alive.load(Ordering::SeqCst));
            list.clone()
        };
        self.sync_with_waiters();
        self.cond.notify_all();
        for w in watchers {
            w.post(|s| s.canceled.emit(&()));
        }
        self.wake_wakers();
    }

    fn finish(&self) {
        let watchers = {
            let mut list = self.watchers.lock().unwrap();
            // Qt: reportFinished guards on !isFinished() (qfutureinterface.cpp:461).
            if self.is_finished.swap(true, Ordering::SeqCst) {
                return;
            }
            list.retain(|w| w.alive.load(Ordering::SeqCst));
            list.clone()
        };
        self.sync_with_waiters();
        self.cond.notify_all();
        for w in watchers {
            w.post(|s| s.finished.emit(&()));
        }
        self.wake_wakers();
    }
}

/// Producer side of an asynchronous computation, modeled after Qt's `QPromise`.
///
/// Dropping a promise that was never finished cancels the computation and reports it finished
/// (Qt: `~QPromise`), so no consumer is left waiting forever.
pub struct Promise<T: Clone + Send + 'static> {
    shared: Arc<FutureShared<T>>,
}

impl<T: Clone + Send + 'static> Drop for Promise<T> {
    fn drop(&mut self) {
        if !self.shared.is_finished.load(Ordering::SeqCst) {
            self.shared.cancel();
            self.shared.finish();
        }
    }
}

impl<T: Clone + Send + 'static> Default for Promise<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Clone + Send + 'static> Promise<T> {
    /// Creates a new promise.
    pub fn new() -> Self {
        Self {
            shared: Arc::new(FutureShared::new()),
        }
    }

    /// Obtains a `Future` handle associated with this promise.
    pub fn future(&self) -> crate::thread::Future<T> {
        crate::thread::Future {
            shared: Arc::clone(&self.shared),
        }
    }

    /// Reports a single result value, matching `QPromise::addResult`.
    pub fn add_result(&self, value: T) {
        if self.is_canceled() {
            return;
        }
        // Appending the result and listing the watchers under one `results` critical section
        // lets `Future::watcher` tell exactly which results a watcher still has to be sent.
        let watchers = {
            let mut res = self.shared.results.lock().unwrap();
            res.push(value.clone());
            self.shared.live_watchers()
        };
        self.shared.cond.notify_all();

        for w in watchers {
            let v = value.clone();
            w.post(move |s| s.result_ready.emit(&v));
        }
        // A pending `poll` resolves with the first result.
        self.shared.wake_wakers();
    }

    /// Sets the final result and marks the promise finished.
    pub fn set_value(&self, value: T) {
        self.add_result(value);
        self.finish();
    }

    /// Sets the progress range `(min, max)`.
    pub fn set_progress_range(&self, min: i32, max: i32) {
        self.shared.progress_min.store(min, Ordering::SeqCst);
        self.shared.progress_max.store(max, Ordering::SeqCst);

        for w in self.shared.live_watchers() {
            w.post(move |s| s.progress_range_changed.emit(&(min, max)));
        }
    }

    /// Sets the current progress value.
    pub fn set_progress_value(&self, value: i32) {
        self.shared.progress_value.store(value, Ordering::SeqCst);

        for w in self.shared.live_watchers() {
            w.post(move |s| s.progress_changed.emit(&value));
        }
    }

    /// Requests cancellation from the producer side.
    pub fn cancel(&self) {
        self.shared.cancel();
    }

    /// Whether cancellation has been requested.
    pub fn is_canceled(&self) -> bool {
        self.shared.is_canceled.load(Ordering::SeqCst)
    }

    /// Marks the computation finished.
    pub fn finish(&self) {
        self.shared.finish();
    }
}

/// Consumer handle for an asynchronous computation, modeled after Qt's `QFuture`.
pub struct Future<T> {
    pub(crate) shared: Arc<FutureShared<T>>,
}

impl<T> Clone for Future<T> {
    fn clone(&self) -> Self {
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl<T: Clone + Send + 'static> Future<T> {
    /// Returns the first result if available.
    pub fn result(&self) -> Option<T> {
        self.shared.results.lock().unwrap().first().cloned()
    }

    /// Returns all produced results.
    pub fn results(&self) -> Vec<T> {
        self.shared.results.lock().unwrap().clone()
    }

    /// Blocks until at least one result is available or computation finishes.
    pub fn wait_result(&self) -> Option<T> {
        let mut res = self.shared.results.lock().unwrap();
        while res.is_empty()
            && !self.shared.is_finished.load(Ordering::SeqCst)
            && !self.is_canceled()
        {
            res = self.shared.cond.wait(res).unwrap();
        }
        res.first().cloned()
    }

    /// Blocks until computation finishes within `timeout`.
    pub fn wait_timeout(&self, timeout: Duration) -> Option<T> {
        let deadline = Instant::now() + timeout;
        let mut res = self.shared.results.lock().unwrap();

        while res.is_empty()
            && !self.shared.is_finished.load(Ordering::SeqCst)
            && !self.is_canceled()
        {
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            let remaining = deadline - now;
            let (next_res, timeout_res) = self.shared.cond.wait_timeout(res, remaining).unwrap();
            res = next_res;
            if timeout_res.timed_out() {
                return res.first().cloned();
            }
        }

        res.first().cloned()
    }

    /// Blocks until the computation is finished (Qt: `QFuture::waitForFinished`), regardless of
    /// how many results were reported so far.
    pub fn wait_for_finished(&self) {
        let mut res = self.shared.results.lock().unwrap();
        while !self.shared.is_finished.load(Ordering::SeqCst) {
            res = self.shared.cond.wait(res).unwrap();
        }
    }

    /// Whether computation has completed.
    pub fn is_finished(&self) -> bool {
        self.shared.is_finished.load(Ordering::SeqCst)
    }

    /// Whether computation has been canceled.
    pub fn is_canceled(&self) -> bool {
        self.shared.is_canceled.load(Ordering::SeqCst)
    }

    /// Requests cancellation of computation.
    pub fn cancel(&self) {
        self.shared.cancel();
    }

    /// Current progress value.
    pub fn progress_value(&self) -> i32 {
        self.shared.progress_value.load(Ordering::SeqCst)
    }

    /// Progress range `(min, max)`.
    pub fn progress_range(&self) -> (i32, i32) {
        (
            self.shared.progress_min.load(Ordering::SeqCst),
            self.shared.progress_max.load(Ordering::SeqCst),
        )
    }

    /// Creates a `FutureWatcher` attached to this future.
    pub fn watcher(&self) -> FutureWatcher<T> {
        let signals = Arc::new(FutureWatcherSignals::new(ThreadId::current()));

        // Register and snapshot the current state atomically with respect to the producer side:
        // results are appended (and watchers listed) under `results`, and the finish/cancel flags
        // are flipped under `watchers`. Every transition is therefore either replayed here or
        // posted by the producer, never both and never neither (Qt: `QFutureWatcher::setFuture`
        // replays the current state).
        let (replay, canceled, finished) = {
            let results = self.shared.results.lock().unwrap();
            let mut watchers = self.shared.watchers.lock().unwrap();
            watchers.push(Arc::clone(&signals));
            (
                results.clone(),
                self.shared.is_canceled.load(Ordering::SeqCst),
                self.shared.is_finished.load(Ordering::SeqCst),
            )
        };
        for value in replay {
            signals.post(move |s| s.result_ready.emit(&value));
        }
        if canceled {
            signals.post(|s| s.canceled.emit(&()));
        }
        if finished {
            signals.post(|s| s.finished.emit(&()));
        }

        FutureWatcher {
            future: self.clone(),
            signals,
        }
    }

    /// The value `poll` resolves to once the computation produced a result, finished or was
    /// canceled; `None` while it is still pending.
    fn ready_output(&self) -> Option<Option<T>> {
        let first = self.result();
        if first.is_some() || self.is_finished() || self.is_canceled() {
            Some(first)
        } else {
            None
        }
    }
}

impl<T: Clone + Send + 'static> StdFuture for Future<T> {
    type Output = Option<T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if let Some(out) = self.ready_output() {
            return Poll::Ready(out);
        }

        {
            let mut wakers = self.shared.wakers.lock().unwrap();
            if !wakers.iter().any(|w| w.will_wake(cx.waker())) {
                wakers.push(cx.waker().clone());
            }
        }

        // A producer that stored its state after the first check above but before the waker was
        // registered has already taken the (empty) waker list: re-check now that we are
        // registered, so the transition cannot be missed.
        if let Some(out) = self.ready_output() {
            return Poll::Ready(out);
        }
        Poll::Pending
    }
}

/// A signal-driven watcher for `Future<T>`, modeled after Qt's `QFutureWatcher`.
pub struct FutureWatcher<T: Clone + Send + 'static> {
    pub future: Future<T>,
    pub(crate) signals: Arc<FutureWatcherSignals<T>>,
}

impl<T: Clone + Send + 'static> Drop for FutureWatcher<T> {
    fn drop(&mut self) {
        self.signals.alive.store(false, Ordering::SeqCst);
    }
}

impl<T: Clone + Send + 'static> FutureWatcher<T> {
    /// Signal emitted once when the computation finishes (Qt: `QFutureWatcher::finished`, value-less).
    pub fn finished(&self) -> &Signal<()> {
        &self.signals.finished
    }

    /// Signal emitted for each reported result, carrying the value (Rust analogue of Qt's `resultReadyAt`).
    pub fn result_ready(&self) -> &Signal<T> {
        &self.signals.result_ready
    }

    /// Signal emitted when computation is canceled.
    pub fn canceled(&self) -> &Signal<()> {
        &self.signals.canceled
    }

    /// Signal emitted when progress value changes.
    pub fn progress_changed(&self) -> &Signal<i32> {
        &self.signals.progress_changed
    }

    /// Signal emitted when progress range changes.
    pub fn progress_range_changed(&self) -> &Signal<(i32, i32)> {
        &self.signals.progress_range_changed
    }
}

/// Synchronizes multiple futures, modeled after Qt's `QFutureSynchronizer`.
#[derive(Default)]
pub struct FutureSynchronizer<T: Clone + Send + 'static> {
    futures: Vec<Future<T>>,
}

impl<T: Clone + Send + 'static> FutureSynchronizer<T> {
    /// Creates a new future synchronizer.
    pub fn new() -> Self {
        Self {
            futures: Vec::new(),
        }
    }

    /// Adds a future to synchronize.
    pub fn add_future(&mut self, future: Future<T>) {
        self.futures.push(future);
    }

    /// Returns a slice of tracked futures.
    pub fn futures(&self) -> &[Future<T>] {
        &self.futures
    }

    /// Waits for all tracked futures to finish.
    pub fn wait_for_finished(&self) {
        for f in &self.futures {
            f.wait_for_finished();
        }
    }

    /// Cancels all tracked futures.
    pub fn cancel_all(&self) {
        for f in &self.futures {
            f.cancel();
        }
    }

    /// Clears tracked futures.
    pub fn clear(&mut self) {
        self.futures.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reproduces the consumer critical section of `wait_result`: the waiter holds `results`,
    /// has seen "not done", and has not yet parked on the condvar. A producer that finishes inside
    /// that window must still wake the waiter.
    fn producer_inside_waiter_window(produce: impl FnOnce(Promise<i32>) + Send + 'static) {
        let promise = Promise::<i32>::new();
        let future = promise.future();
        let shared = Arc::clone(&future.shared);

        let guard = shared.results.lock().unwrap();
        assert!(!future.is_finished() && !future.is_canceled());
        let producer = std::thread::spawn(move || produce(promise));
        while !(future.is_finished() || future.is_canceled()) {
            std::thread::yield_now();
        }
        let (guard, timeout) = shared
            .cond
            .wait_timeout(guard, Duration::from_secs(10))
            .unwrap();
        assert!(
            !timeout.timed_out(),
            "lost wakeup: producer notified before the waiter parked"
        );
        drop(guard);
        producer.join().unwrap();
    }

    #[test]
    fn promise_finish_does_not_lose_wakeup() {
        producer_inside_waiter_window(|p| p.finish());
    }

    #[test]
    fn promise_cancel_does_not_lose_wakeup() {
        producer_inside_waiter_window(|p| p.cancel());
    }

    #[test]
    fn future_cancel_does_not_lose_wakeup() {
        producer_inside_waiter_window(|p| p.future().cancel());
    }
}
