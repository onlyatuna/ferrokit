// SPDX-License-Identifier: LGPL-3.0-only
//! A `Precise` timer on Windows is driven by a multimedia timer owned by the thread that armed it.
//! These tests follow that native timer through the lifecycle paths that cross threads:
//!
//! - a move to another thread leaves no native timer on the source and arms exactly one on the
//!   target;
//! - a stop issued from the old thread after the move does not touch the moved timer;
//! - destroying the object on the target releases the native timer there;
//! - handoffs that overtake each other (an object moved on before its timers were adopted) still
//!   end with exactly one native timer, which keeps ticking and is released on destroy.
//!
//! "Released" is checked on the native timer itself: `timeKillEvent` returns `TIMERR_NOCANDO` for a
//! handle that no longer exists. Native handles are system-wide small integers that may be reused,
//! so the tests in this file run one at a time.
#![cfg(windows)]

use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use ferrokit_core::event_loop::EventLoop;
use ferrokit_core::object::{
    move_to_thread_by_id, query_object_thread, register_qobject_ptr, unregister_qobject,
    ObjectData, ObjectDataMut, ObjectId, QObject, ThreadId,
};
use ferrokit_core::timer::precise::native_handle;
use ferrokit_core::timer::{kill_object_timer, start_object_timer, TimerId, TimerType};
use windows_sys::Win32::Media::timeKillEvent;

/// An `ObjectData` that lets `move_to_thread_by_id` move its object.
fn movable_data(id: ObjectId) -> ObjectData {
    // SAFETY: `Probe` holds `Arc` handles and plain fields, which are `Send`.
    unsafe { ObjectData::new(id).with_thread_moves_allowed() }
}

/// Upper bound for each cross-thread wait. A regression fails here instead of hanging.
const WATCHDOG: Duration = Duration::from_secs(10);
const TIMERR_NOCANDO: u32 = 97;

static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Whether the multimedia timer behind `handle` no longer exists. Kills it if it does, so call it
/// last.
fn released(handle: u32) -> bool {
    unsafe { timeKillEvent(handle) == TIMERR_NOCANDO }
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
    // SAFETY: the probe is leaked, so it outlives its registration.
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

struct OwnerReport {
    handle_while_running: Option<u32>,
    handle_after_destroy: Option<u32>,
    ticks_after_destroy: usize,
}

#[test]
fn a_moved_timer_is_one_native_timer_on_the_target_and_none_on_the_source() {
    let _serial = serial();
    let source = ThreadId::current();
    let source_loop = EventLoop::new();
    let (id, ticks) = register_probe();
    let timer = start_object_timer(id, 5, TimerType::Precise);
    assert!(timer.is_valid());
    assert!(
        native_handle(timer).is_some(),
        "a Precise timer is a multimedia timer on its thread"
    );

    let (tid_tx, tid_rx) = channel();
    let (report_tx, report_rx) = channel();
    let owner_ticks = Arc::clone(&ticks);
    let owner = std::thread::spawn(move || {
        let el = EventLoop::new();
        let me = ThreadId::current();
        tid_tx.send(me).unwrap();
        let ticking = pump_until(&el, || ticks_on(&owner_ticks, me) >= 3);
        let handle_while_running = ticking.then(|| native_handle(timer)).flatten();
        // SAFETY: after the move this thread is the object's registration thread.
        unsafe { unregister_qobject(id) };
        let handle_after_destroy = native_handle(timer);
        let before = owner_ticks.lock().unwrap().len();
        pump_for(&el, Duration::from_millis(100));
        let ticks_after_destroy = owner_ticks.lock().unwrap().len() - before;
        report_tx
            .send(OwnerReport {
                handle_while_running,
                handle_after_destroy,
                ticks_after_destroy,
            })
            .unwrap();
    });
    let target = tid_rx.recv_timeout(WATCHDOG).expect("owner reports its id");
    assert_eq!(move_to_thread_by_id(id, target, source), Ok(()));

    assert_eq!(
        native_handle(timer),
        None,
        "the source thread keeps no native timer after the move"
    );
    // `native_handle` above is the record of the source thread's timer. The numeric handle is not
    // probed: the target may already have created a timer that reuses it.
    assert!(
        !kill_object_timer(timer),
        "the old thread no longer owns the timer, so its stop finds nothing"
    );
    // Nothing is delivered on the source, including a tick that was already queued for it.
    pump_for(&source_loop, Duration::from_millis(100));

    let report = report_rx.recv_timeout(WATCHDOG).expect("owner reports");
    owner.join().expect("owner thread must not panic");
    let running = report
        .handle_while_running
        .expect("the moved timer ticked on the target, so a native timer exists there");
    assert_eq!(report.handle_after_destroy, None);
    assert!(
        released(running),
        "destroying the object releases the target's native timer"
    );
    assert_eq!(report.ticks_after_destroy, 0);
    let ticks = ticks.lock().unwrap();
    assert!(
        ticks.iter().all(|t| *t == target),
        "ticks ran on {ticks:?}, expected only {target:?}"
    );
}

/// What one side of the handoff race saw at the end.
struct SideReport {
    native_timer: bool,
    owner: bool,
    native_after_destroy: bool,
}

/// One turn of the handoff: if the object is here, send it to the other thread.
fn hand_off(id: ObjectId, me: ThreadId, other: ThreadId, moves: &AtomicUsize, limit: usize) {
    if moves.load(Ordering::SeqCst) < limit
        && query_object_thread(id) == Some(me)
        && move_to_thread_by_id(id, other, me).is_ok()
    {
        moves.fetch_add(1, Ordering::SeqCst);
    }
}

fn finish_side(id: ObjectId, timer: TimerId, me: ThreadId) -> SideReport {
    let native_timer = native_handle(timer).is_some();
    let owner = query_object_thread(id) == Some(me);
    if owner {
        // SAFETY: this thread is the object's registration thread.
        unsafe { unregister_qobject(id) };
    }
    SideReport {
        native_timer,
        owner,
        native_after_destroy: native_handle(timer).is_some(),
    }
}

#[test]
fn handoffs_that_overtake_each_other_end_with_one_native_timer_that_keeps_ticking() {
    const HANDOFFS: usize = 200;
    let _serial = serial();
    let here = ThreadId::current();
    let el = EventLoop::new();
    let (id, ticks) = register_probe();
    let timer = start_object_timer(id, 5, TimerType::Precise);
    assert!(timer.is_valid());

    let moves = Arc::new(AtomicUsize::new(0));
    let measure = Arc::new(AtomicBool::new(false));
    let (tid_tx, tid_rx) = channel();
    let (report_tx, report_rx) = channel();
    let (other_moves, other_measure) = (Arc::clone(&moves), Arc::clone(&measure));
    let other_thread = std::thread::spawn(move || {
        let el = EventLoop::new();
        let me = ThreadId::current();
        tid_tx.send(me).unwrap();
        let end = Instant::now() + 3 * WATCHDOG;
        while !other_measure.load(Ordering::SeqCst) && Instant::now() < end {
            el.process_events(false);
            hand_off(id, me, here, &other_moves, HANDOFFS);
            std::thread::sleep(Duration::from_millis(1));
        }
        report_tx.send(finish_side(id, timer, me)).unwrap();
    });
    let other = tid_rx
        .recv_timeout(WATCHDOG)
        .expect("other thread reports its id");

    let end = Instant::now() + WATCHDOG;
    while moves.load(Ordering::SeqCst) < HANDOFFS && Instant::now() < end {
        el.process_events(false);
        hand_off(id, here, other, &moves, HANDOFFS);
        std::thread::sleep(Duration::from_millis(1));
    }
    let handoffs = moves.load(Ordering::SeqCst);

    // Let the last handoffs settle, then the timer must still tick wherever the object ended.
    pump_for(&el, Duration::from_millis(300));
    let before = ticks.lock().unwrap().len();
    pump_for(&el, Duration::from_millis(200));
    let still_ticking = ticks.lock().unwrap().len() > before;

    measure.store(true, Ordering::SeqCst);
    let mine = finish_side(id, timer, here);
    let theirs = report_rx
        .recv_timeout(WATCHDOG)
        .expect("other thread reports");
    other_thread.join().expect("other thread must not panic");

    assert_eq!(handoffs, HANDOFFS, "every handoff completed");
    assert!(still_ticking, "the timer kept ticking after the handoffs");
    assert_eq!(
        usize::from(mine.native_timer) + usize::from(theirs.native_timer),
        1,
        "exactly one thread holds the native timer"
    );
    assert_eq!(
        usize::from(mine.owner) + usize::from(theirs.owner),
        1,
        "exactly one thread owns the object"
    );
    assert_eq!(
        mine.native_timer, mine.owner,
        "the native timer is on the thread that owns the object"
    );
    assert!(!mine.native_after_destroy && !theirs.native_after_destroy);
}

/// Swallows the first `WM_TIMER` the window receives.
struct SwallowFirstTick(bool);

impl ferrokit_core::event::NativeEventFilter for SwallowFirstTick {
    fn native_event_filter(
        &mut self,
        event_type: &str,
        msg: &ferrokit_core::event::NativeMessage,
        _result: &mut isize,
    ) -> bool {
        if event_type == "windows_generic_MSG" && !self.0 {
            if let ferrokit_core::event::NativeMessage::Windows(m) = msg {
                if m.message == windows_sys::Win32::UI::WindowsAndMessaging::WM_TIMER {
                    self.0 = true;
                    return true;
                }
            }
        }
        false
    }
}

#[test]
fn a_tick_swallowed_by_a_native_filter_does_not_silence_the_timer() {
    let _serial = serial();
    let el = EventLoop::new();
    el.install_native_event_filter(Box::new(SwallowFirstTick(false)));
    let (id, ticks) = register_probe();
    let timer = start_object_timer(id, 5, TimerType::Precise);
    assert!(timer.is_valid());
    let here = ThreadId::current();
    assert!(
        pump_until(&el, || ticks_on(&ticks, here) >= 3),
        "the timer keeps ticking after one of its messages was swallowed"
    );
    assert!(kill_object_timer(timer));
}
