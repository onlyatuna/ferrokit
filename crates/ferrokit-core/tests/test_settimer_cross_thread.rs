// SPDX-License-Identifier: LGPL-3.0-only
//! A timer that is not a multimedia timer (a `Coarse` timer above 20 ms) is a `SetTimer` timer on
//! its thread's message window. These tests follow it through the paths that cross threads:
//!
//! - a move leaves no timer on the source window, arms one on the target window, and the callbacks
//!   run on the target only, including when the source had an overdue tick waiting at the move;
//! - moving the object back leaves no timer on the other window, and the ticks that follow are on
//!   the owner only, in the order source, target, source;
//! - an object moved on, or destroyed, before the thread it was sent to has adopted its timers
//!   ends with the timer on the last owner only, or on no thread.
//!
//! "No timer on a window" is checked with `KillTimer` on that window from its own thread, which
//! fails for a timer that does not exist. A timer is identified by the id the `WM_TIMER` message
//! carries, which a native filter on each loop records.
#![cfg(windows)]

use std::ptr::NonNull;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use ferrokit_core::event::{NativeEventFilter, NativeMessage};
use ferrokit_core::event_loop::EventLoop;
use ferrokit_core::object::{
    ObjectData, ObjectDataMut, ObjectId, QObject, ThreadId, move_to_thread_by_id,
    register_qobject_ptr, unregister_qobject,
};
use ferrokit_core::timer::precise::native_handle;
use ferrokit_core::timer::{TimerId, TimerType, kill_object_timer, start_object_timer};
use windows_sys::Win32::UI::WindowsAndMessaging::{KillTimer, WM_TIMER};

/// An `ObjectData` that lets `move_to_thread_by_id` move its object.
fn movable_data(id: ObjectId) -> ObjectData {
    // SAFETY: `Probe` holds `Arc` handles and plain fields, which are `Send`.
    unsafe { ObjectData::new(id).with_thread_moves_allowed() }
}

/// Above the 20 ms bound up to which `Coarse` timers use a multimedia timer, so `SetTimer` drives
/// the timer.
const INTERVAL: u64 = 30;
/// Upper bound for each wait. A regression fails here instead of hanging.
const WATCHDOG: Duration = Duration::from_secs(10);

static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The `(window, timer id)` of every `WM_TIMER` a loop's window received.
type Messages = Arc<Mutex<Vec<(usize, usize)>>>;

struct Recorder(Messages);

impl NativeEventFilter for Recorder {
    fn native_event_filter(
        &mut self,
        event_type: &str,
        msg: &NativeMessage,
        _result: &mut isize,
    ) -> bool {
        if event_type == "windows_generic_MSG"
            && let NativeMessage::Windows(m) = msg
            && m.message == WM_TIMER
        {
            self.0.lock().unwrap().push((m.hwnd as usize, m.wParam));
        }
        false
    }
}

/// A loop on this thread whose `WM_TIMER` messages are recorded.
fn new_loop() -> (EventLoop, Messages) {
    let messages = Messages::default();
    let el = EventLoop::new();
    el.install_native_event_filter(Box::new(Recorder(Arc::clone(&messages))));
    (el, messages)
}

/// The window that received a `WM_TIMER` for timer `id`.
fn window_of(messages: &Messages, id: TimerId) -> Option<usize> {
    messages
        .lock()
        .unwrap()
        .iter()
        .find(|(_, timer)| *timer == id.0 as usize)
        .map(|(window, _)| *window)
}

fn messages_for(messages: &Messages, id: TimerId) -> usize {
    messages
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, timer)| *timer == id.0 as usize)
        .count()
}

/// Whether `window`, which the calling thread owns, has no timer `id`. Kills the timer if there is
/// one, so call it last.
fn no_timer_on(window: usize, id: TimerId) -> bool {
    // SAFETY: `KillTimer` takes an `HWND` (an integer handle) and an integer id, and no pointer
    // is dereferenced by us. A window the calling thread does not own, or a destroyed one, makes
    // the call fail with 0 (Win32 docs) instead of faulting; the callers pass a window of an
    // event loop that was alive when the ticks were recorded.
    unsafe { KillTimer(window as _, id.0 as usize) == 0 }
}

/// Records the thread of every tick it receives.
struct Probe {
    data: ObjectData,
    ticks: Arc<Mutex<Vec<ThreadId>>>,
}

impl QObject for Probe {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ObjectDataMut<'_> {
        ObjectDataMut::new(&mut self.data)
    }
    fn as_qobject_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn as_qobject_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
    fn timer_event(&mut self, _timer_id: u64) {
        self.ticks.lock().unwrap().push(ThreadId::current());
    }
}

/// Registers a probe on this thread. The probe is leaked, so a thread that outlives a failing test
/// never touches freed memory.
fn register_probe() -> (ObjectId, Arc<Mutex<Vec<ThreadId>>>) {
    let ticks = Arc::new(Mutex::new(Vec::new()));
    let probe: &'static mut Probe = Box::leak(Box::new(Probe {
        data: movable_data(ObjectId::next()),
        ticks: Arc::clone(&ticks),
    }));
    let id = probe.data.id();
    // SAFETY: the probe is leaked, so it is never freed or moved, which covers the lifetime
    // requirement for the whole registration. `NonNull::from(probe)` consumes the only reference
    // (`id` was read from it before, and `probe` is not used afterwards), so no later direct use
    // invalidates the registry's pointer. The registration thread is this one, and the leaked
    // probe is touched only by registry callbacks. Its `ObjectData` allows moves, and `Probe`
    // holds only `Arc<Mutex<..>>`, so moving the registration to another thread is sound.
    unsafe { register_qobject_ptr(NonNull::from(probe)) };
    (id, ticks)
}

fn pump_for(el: &EventLoop, duration: Duration) {
    let end = Instant::now() + duration;
    while Instant::now() < end {
        el.process_events(false);
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn pump_until(el: &EventLoop, mut done: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + WATCHDOG;
    while Instant::now() < end {
        if done() {
            return true;
        }
        el.process_events(false);
        std::thread::sleep(Duration::from_millis(1));
    }
    done()
}

fn ticks_on(ticks: &Mutex<Vec<ThreadId>>, thread: ThreadId) -> usize {
    ticks
        .lock()
        .unwrap()
        .iter()
        .filter(|t| **t == thread)
        .count()
}

/// The ticks in order, with runs of the same thread folded into one.
fn runs(ticks: &Mutex<Vec<ThreadId>>) -> Vec<ThreadId> {
    let mut runs: Vec<ThreadId> = Vec::new();
    for thread in ticks.lock().unwrap().iter() {
        if runs.last() != Some(thread) {
            runs.push(*thread);
        }
    }
    runs
}

/// Starts a thread with a recorded loop, reports its id, and runs `body` there.
fn spawn_loop_thread<R: Send + 'static>(
    body: impl FnOnce(EventLoop, Messages, ThreadId) -> R + Send + 'static,
) -> (ThreadId, std::thread::JoinHandle<R>) {
    let (tid_tx, tid_rx) = channel();
    let handle = std::thread::spawn(move || {
        let (el, messages) = new_loop();
        let me = ThreadId::current();
        tid_tx.send(me).unwrap();
        body(el, messages, me)
    });
    let tid = tid_rx
        .recv_timeout(WATCHDOG)
        .expect("thread reports its id");
    (tid, handle)
}

fn wait_for<T>(rx: &Receiver<T>) -> T {
    rx.recv_timeout(WATCHDOG).expect("signal arrives")
}

fn start_coarse(id: ObjectId) -> TimerId {
    let timer = start_object_timer(id, INTERVAL, TimerType::Coarse);
    assert!(timer.is_valid());
    assert!(
        native_handle(timer).is_none(),
        "a Coarse timer above 20 ms is a SetTimer timer"
    );
    timer
}

struct TargetReport {
    ticked: bool,
    window: Option<usize>,
    none_after_destroy: bool,
    ticks_after_destroy: usize,
}

#[test]
fn a_moved_timer_leaves_the_source_window_and_ticks_only_on_the_target() {
    let _serial = serial();
    let source = ThreadId::current();
    let (el, source_messages) = new_loop();
    let (id, ticks) = register_probe();
    let timer = start_coarse(id);
    assert!(
        pump_until(&el, || ticks_on(&ticks, source) >= 2),
        "the timer ticks on the source before the move"
    );
    let source_window =
        window_of(&source_messages, timer).expect("the source window got the ticks");

    let target_ticks = Arc::clone(&ticks);
    let (target, target_thread) = spawn_loop_thread(move |el, messages, me| {
        let ticked = pump_until(&el, || ticks_on(&target_ticks, me) >= 3);
        let window = window_of(&messages, timer);
        // SAFETY: `unregister_qobject` needs the object alive, on its registration thread, with no
        // callback active. The probe is leaked (`register_probe`), so its memory stays valid.
        // `pump_until` returned only after ticks arrived on `me`, which happens only after the
        // test body's `move_to_thread_by_id` moved the object here, so this is the registration
        // thread. This code is not inside a callback, so none is active.
        unsafe { unregister_qobject(id) };
        let none_after_destroy = window.is_some_and(|w| no_timer_on(w, timer));
        let before = ticks_on(&target_ticks, me);
        pump_for(&el, Duration::from_millis(4 * INTERVAL));
        TargetReport {
            ticked,
            window,
            none_after_destroy,
            ticks_after_destroy: ticks_on(&target_ticks, me) - before,
        }
    });

    // Leave the loop unpumped until the timer is overdue, so a tick is waiting at the move.
    std::thread::sleep(Duration::from_millis(3 * INTERVAL));
    let on_source = ticks_on(&ticks, source);
    assert_eq!(move_to_thread_by_id(id, target, source), Ok(()));

    assert!(
        no_timer_on(source_window, timer),
        "the source window keeps no timer after the move"
    );
    assert!(
        !kill_object_timer(timer),
        "the old thread no longer owns the timer, so its stop finds nothing"
    );
    pump_for(&el, Duration::from_millis(4 * INTERVAL));
    assert_eq!(
        ticks_on(&ticks, source),
        on_source,
        "no callback ran on the source after the move"
    );

    let report = target_thread.join().expect("target thread must not panic");
    assert!(report.ticked, "the moved timer ticked on the target");
    let window = report.window.expect("the target window got the ticks");
    assert_ne!(window, source_window);
    assert!(
        report.none_after_destroy,
        "destroying the object released the timer on the target window"
    );
    assert_eq!(report.ticks_after_destroy, 0);
    assert_eq!(runs(&ticks), vec![source, target]);
}

struct RoundTripReport {
    ticked: bool,
    none_after_return: bool,
    ticks_after_return: usize,
}

#[test]
fn a_timer_moved_there_and_back_is_one_timer_that_ticks_on_the_owner_only() {
    let _serial = serial();
    let source = ThreadId::current();
    let (el, source_messages) = new_loop();
    let (id, ticks) = register_probe();
    let timer = start_coarse(id);
    assert!(pump_until(&el, || ticks_on(&ticks, source) >= 2));
    let source_window =
        window_of(&source_messages, timer).expect("the source window got the ticks");

    let other_ticks = Arc::clone(&ticks);
    let (other, other_thread) = spawn_loop_thread(move |el, messages, me| {
        let ticked = pump_until(&el, || ticks_on(&other_ticks, me) >= 3);
        let window = window_of(&messages, timer);
        assert_eq!(move_to_thread_by_id(id, source, me), Ok(()));
        let none_after_return = window.is_some_and(|w| no_timer_on(w, timer));
        let at_return = ticks_on(&other_ticks, me);
        // A tick that was queued for this window when the object left must not be delivered.
        pump_for(&el, Duration::from_millis(4 * INTERVAL));
        RoundTripReport {
            ticked,
            none_after_return,
            ticks_after_return: ticks_on(&other_ticks, me) - at_return,
        }
    });
    let on_source = ticks_on(&ticks, source);
    assert_eq!(move_to_thread_by_id(id, other, source), Ok(()));
    assert!(
        pump_until(&el, || ticks_on(&ticks, source) >= on_source + 3),
        "the timer ticks on the source again after it came back"
    );

    let report = other_thread.join().expect("other thread must not panic");
    assert!(report.ticked);
    assert!(
        report.none_after_return,
        "the other window keeps no timer once the object went back"
    );
    assert_eq!(report.ticks_after_return, 0);
    assert_eq!(runs(&ticks), vec![source, other, source]);

    // One timer on the source window, and nothing else ticks for the object.
    assert!(
        !no_timer_on(source_window, timer),
        "the timer is on the source window again"
    );
    assert!(no_timer_on(source_window, timer), "and it is one timer");
    let settled = ticks.lock().unwrap().len();
    pump_for(&el, Duration::from_millis(4 * INTERVAL));
    assert_eq!(ticks.lock().unwrap().len(), settled);
    // SAFETY: `unregister_qobject` needs the object alive, on its registration thread, with no
    // callback active. The probe is leaked (`register_probe`), so its memory stays valid. The
    // `pump_until` after the move back saw ticks on the source again, so the source is the
    // registration thread. This code is not inside a callback, so none is active.
    unsafe { unregister_qobject(id) };
}

struct ForwardReport {
    ticks_here: usize,
    messages_here: usize,
}

#[test]
fn an_object_moved_on_before_adoption_ticks_on_its_last_owner_only() {
    let _serial = serial();
    let main = ThreadId::current();
    let (id, ticks) = register_probe();
    let (el, _main_messages) = new_loop();
    let timer = start_coarse(id);
    assert!(pump_until(&el, || ticks_on(&ticks, main) >= 1));

    let (go_tx, go_rx) = channel::<ThreadId>();
    let middle_ticks = Arc::clone(&ticks);
    let (middle, middle_thread) = spawn_loop_thread(move |el, messages, me| {
        // The timer has not been adopted here: the loop has not run since the object arrived.
        let last = wait_for(&go_rx);
        assert_eq!(move_to_thread_by_id(id, last, me), Ok(()));
        pump_for(&el, Duration::from_millis(6 * INTERVAL));
        ForwardReport {
            ticks_here: ticks_on(&middle_ticks, me),
            messages_here: messages_for(&messages, timer),
        }
    });
    let last_ticks = Arc::clone(&ticks);
    let (last, last_thread) = spawn_loop_thread(move |el, messages, me| {
        let ticked = pump_until(&el, || ticks_on(&last_ticks, me) >= 3);
        let window = window_of(&messages, timer);
        // SAFETY: `unregister_qobject` needs the object alive, on its registration thread, with no
        // callback active. The probe is leaked (`register_probe`), so its memory stays valid.
        // `pump_until` returned after ticks arrived on `me`, which only happens once the object
        // was moved here (the `middle` thread's `move_to_thread_by_id(id, last, ..)`), so this
        // is the registration thread. This code is not inside a callback, so none is active.
        unsafe { unregister_qobject(id) };
        let none_after_destroy = window.is_some_and(|w| no_timer_on(w, timer));
        (ticked, none_after_destroy)
    });

    assert_eq!(move_to_thread_by_id(id, middle, main), Ok(()));
    go_tx.send(last).unwrap();

    let forward = middle_thread.join().expect("middle thread must not panic");
    let (ticked, none_after_destroy) = last_thread.join().expect("last thread must not panic");
    assert!(ticked, "the timer ticks on the last owner");
    assert!(none_after_destroy, "and is released there on destroy");
    assert_eq!(
        forward.ticks_here, 0,
        "nothing ticked on the thread the object passed through"
    );
    assert_eq!(
        forward.messages_here, 0,
        "the thread the object passed through never armed the timer"
    );
    assert_eq!(runs(&ticks), vec![main, last]);
}

#[test]
fn an_object_destroyed_before_adoption_leaves_no_timer_behind() {
    let _serial = serial();
    let main = ThreadId::current();
    let (id, ticks) = register_probe();
    let (el, _main_messages) = new_loop();
    let timer = start_coarse(id);
    assert!(pump_until(&el, || ticks_on(&ticks, main) >= 1));
    let on_main = ticks.lock().unwrap().len();

    let (go_tx, go_rx): (Sender<()>, Receiver<()>) = channel();
    let (target, target_thread) = spawn_loop_thread(move |el, messages, _me| {
        wait_for(&go_rx);
        // SAFETY: `unregister_qobject` needs the object alive, on its registration thread, with no
        // callback active. The probe is leaked (`register_probe`), so its memory stays valid. The
        // `go` message is sent only after the test body's `move_to_thread_by_id(id, target, ..)`
        // returned `Ok`, so this thread is the registration thread. This code is not inside a
        // callback, so none is active.
        unsafe { unregister_qobject(id) };
        let stopped = kill_object_timer(timer);
        pump_for(&el, Duration::from_millis(6 * INTERVAL));
        (stopped, messages_for(&messages, timer))
    });

    assert_eq!(move_to_thread_by_id(id, target, main), Ok(()));
    go_tx.send(()).unwrap();
    pump_for(&el, Duration::from_millis(6 * INTERVAL));

    let (stopped, messages_on_target) = target_thread.join().expect("target thread must not panic");
    assert!(!stopped, "the destroyed object's timer is already gone");
    assert_eq!(
        messages_on_target, 0,
        "the target window never got a tick for it"
    );
    assert_eq!(
        ticks.lock().unwrap().len(),
        on_main,
        "no callback ran anywhere after the move"
    );
}
