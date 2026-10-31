// SPDX-License-Identifier: LGPL-3.0-only
//! The `Precise` timer contract on Windows, checked as properties, not as latency figures:
//!
//! - a timer never fires before its expiry (1 ms is only the clock's measurement tolerance);
//! - after an overrun, a callback that blocked several intervals is followed by one callback, not
//!   one per missed interval;
//! - a loop that was not pumped for many intervals sees one callback when it comes back;
//! - `Timer::single_shot` fires once and not early;
//! - the timer of an object moved with the deprecated `move_to_thread` is armed on the target and
//!   released there, like one moved by id.
//!
//! Nothing here asserts how late a callback may be: that depends on the machine.
#![cfg(windows)]
#![allow(deprecated)]

use std::ptr::NonNull;
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use ferrokit_core::event_loop::EventLoop;
use ferrokit_core::object::{
    move_to_thread, register_qobject_ptr, unregister_qobject, ObjectData, ObjectDataMut, ObjectId,
    QObject, ThreadId,
};
use ferrokit_core::timer::precise::native_handle;
use ferrokit_core::timer::{kill_object_timer, start_object_timer, Timer, TimerType};

/// An `ObjectData` that lets `move_to_thread_by_id` move its object.
fn movable_data(id: ObjectId) -> ObjectData {
    // SAFETY: `Probe` holds an `ObjectData`, `Arc<Mutex<_>>` handles, an `Option<usize>` and a
    // `Duration`: all `Send`, with no `Rc`, no `Cell` shared with a handle left behind, and no
    // thread-local state.
    unsafe { ObjectData::new(id).with_thread_moves_allowed() }
}

const WATCHDOG: Duration = Duration::from_secs(10);
/// Clock measurement tolerance, not a latency allowance.
const TOLERANCE: Duration = Duration::from_millis(1);

static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Records when and where each tick arrives; the tick at `block_at` blocks for `block_for`.
struct Probe {
    data: ObjectData,
    ticks: Arc<Mutex<Vec<(Instant, ThreadId)>>>,
    block_at: Option<usize>,
    block_for: Duration,
    block_end: Arc<Mutex<Option<Instant>>>,
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
        let index = {
            let mut ticks = self.ticks.lock().unwrap();
            ticks.push((Instant::now(), ThreadId::current()));
            ticks.len() - 1
        };
        if self.block_at == Some(index) {
            std::thread::sleep(self.block_for);
            *self.block_end.lock().unwrap() = Some(Instant::now());
        }
    }
}

type Ticks = Arc<Mutex<Vec<(Instant, ThreadId)>>>;

/// Leaks a probe and registers it on this thread. Returns its id, tick log and block end.
fn register_probe(
    block_at: Option<usize>,
    block_for: Duration,
) -> (*mut Probe, ObjectId, Ticks, Arc<Mutex<Option<Instant>>>) {
    let ticks: Ticks = Arc::new(Mutex::new(Vec::new()));
    let block_end = Arc::new(Mutex::new(None));
    let probe: &'static mut Probe = Box::leak(Box::new(Probe {
        data: movable_data(ObjectId::next()),
        ticks: Arc::clone(&ticks),
        block_at,
        block_for,
        block_end: Arc::clone(&block_end),
    }));
    let id = probe.data.id();
    let raw: *mut Probe = probe;
    // SAFETY: `raw` comes from `Box::leak`, so the allocation is never freed or moved and outlives
    // the registration; the `&'static mut` it was made from is not used after `probe.data.id()`
    // above, and the tests reach the probe only through the registry (callbacks, on this thread)
    // and the `ticks` `Arc`.
    unsafe { register_qobject_ptr(NonNull::new_unchecked(raw)) };
    (raw, id, ticks, block_end)
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

#[test]
fn a_precise_timer_never_fires_before_its_expiry() {
    const INTERVAL: Duration = Duration::from_millis(20);
    let _serial = serial();
    let el = EventLoop::new();
    let (_, id, ticks, _) = register_probe(None, Duration::ZERO);
    let start = Instant::now();
    let timer = start_object_timer(id, INTERVAL.as_millis() as u64, TimerType::Precise);
    assert!(timer.is_valid());
    assert!(pump_until(&el, || ticks.lock().unwrap().len() >= 15));
    assert!(kill_object_timer(timer));
    // SAFETY: the probe is leaked (never freed) and this is its registration thread; `pump_until`
    // has returned, so no loop is turning and no callback of it runs, and the timer is killed so
    // none can start before the unregistration.
    unsafe { unregister_qobject(id) };

    // Tick `n` belongs to grid point `n + 1`; a late loop skips grid points, never reaches one
    // sooner.
    for (n, (at, _)) in ticks.lock().unwrap().iter().enumerate() {
        let expiry = start + INTERVAL * (n as u32 + 1);
        assert!(
            *at + TOLERANCE >= expiry,
            "tick {n} came {:?} before its expiry",
            expiry.saturating_duration_since(*at)
        );
    }
}

#[test]
fn a_callback_that_overran_is_followed_by_one_callback_not_one_per_missed_interval() {
    const INTERVAL: Duration = Duration::from_millis(50);
    let _serial = serial();
    let el = EventLoop::new();
    // The third callback blocks for four intervals.
    let (_, id, ticks, block_end) = register_probe(Some(2), INTERVAL * 4);
    let timer = start_object_timer(id, INTERVAL.as_millis() as u64, TimerType::Precise);
    assert!(timer.is_valid());
    assert!(
        pump_until(&el, || ticks.lock().unwrap().len() >= 6),
        "the timer keeps ticking after the overrun"
    );
    assert!(kill_object_timer(timer));
    // SAFETY: the probe is leaked (never freed) and this is its registration thread; `pump_until`
    // has returned, so no loop is turning and no callback of it runs, and the timer is killed so
    // none can start before the unregistration.
    unsafe { unregister_qobject(id) };

    let block_end = block_end
        .lock()
        .unwrap()
        .expect("the third callback blocked");
    // Missed grid points are skipped. A burst would put several callbacks right behind the block.
    let window = INTERVAL / 4;
    let behind = ticks
        .lock()
        .unwrap()
        .iter()
        .filter(|(at, _)| *at >= block_end && *at < block_end + window)
        .count();
    assert!(
        behind <= 1,
        "{behind} callbacks within {window:?} after the overrun"
    );
}

#[test]
fn a_loop_that_was_not_pumped_for_many_intervals_sees_one_callback_when_it_returns() {
    const INTERVAL: Duration = Duration::from_millis(50);
    let _serial = serial();
    let el = EventLoop::new();
    let (_, id, ticks, _) = register_probe(None, Duration::ZERO);
    let timer = start_object_timer(id, INTERVAL.as_millis() as u64, TimerType::Precise);
    assert!(timer.is_valid());
    // Ten intervals pass with no pumping; the native source keeps ticking meanwhile.
    std::thread::sleep(INTERVAL * 10);
    assert!(pump_until(&el, || !ticks.lock().unwrap().is_empty()));
    let first = ticks.lock().unwrap()[0].0;
    pump_for(&el, INTERVAL / 4);
    assert!(kill_object_timer(timer));
    // SAFETY: the probe is leaked (never freed) and this is its registration thread; `pump_until`
    // has returned, so no loop is turning and no callback of it runs, and the timer is killed so
    // none can start before the unregistration.
    unsafe { unregister_qobject(id) };

    let burst = ticks
        .lock()
        .unwrap()
        .iter()
        .filter(|(at, _)| *at < first + INTERVAL / 4)
        .count();
    assert_eq!(burst, 1, "the missed intervals were delivered as a burst");
}

#[test]
fn a_single_shot_fires_once_and_not_early() {
    const INTERVAL: Duration = Duration::from_millis(20);
    let _serial = serial();
    let el = EventLoop::new();
    let (tx, rx) = channel();
    let start = Instant::now();
    Timer::single_shot(INTERVAL.as_millis() as u64, move || {
        tx.send(Instant::now()).unwrap();
    });
    assert!(pump_until(&el, || start.elapsed() > Duration::from_millis(400)));
    let fired: Vec<Instant> = rx.try_iter().collect();
    assert_eq!(fired.len(), 1, "a single shot fires exactly once");
    assert!(
        fired[0] + TOLERANCE >= start + INTERVAL,
        "fired {:?} before its expiry",
        (start + INTERVAL).saturating_duration_since(fired[0])
    );
}

#[test]
fn a_timer_moved_with_the_deprecated_move_is_armed_on_the_target_and_released_there() {
    let _serial = serial();
    let source = ThreadId::current();
    let source_loop = EventLoop::new();
    let (probe, id, ticks, _) = register_probe(None, Duration::ZERO);
    let timer = start_object_timer(id, 5, TimerType::Precise);
    assert!(timer.is_valid());
    assert!(native_handle(timer).is_some());

    let (tid_tx, tid_rx) = channel();
    let (report_tx, report_rx) = channel();
    let owner_ticks = Arc::clone(&ticks);
    let owner = std::thread::spawn(move || {
        let el = EventLoop::new();
        let me = ThreadId::current();
        tid_tx.send(me).unwrap();
        let ticking = pump_until(&el, || {
            owner_ticks
                .lock()
                .unwrap()
                .iter()
                .filter(|t| t.1 == me)
                .count()
                >= 3
        });
        let while_running = native_handle(timer).is_some();
        // SAFETY: the object was moved to this thread by the test's `move_to_thread` before the
        // loop here ran, so this thread is its registration thread; `pump_until` has returned, so
        // no callback of it runs. The probe is leaked, so the memory stays valid.
        unsafe { unregister_qobject(id) };
        report_tx
            .send((ticking, while_running, native_handle(timer).is_some()))
            .unwrap();
    });
    let target = tid_rx.recv_timeout(WATCHDOG).expect("owner reports its id");
    // NOT DOCUMENTED AS SAFE (reported to the maintainer): this call breaks rule 6 of the `object`
    // module. The deprecated `move_to_thread` holds `&mut (*probe).data` while it sends
    // `ThreadChange` to the same object through the registry (`send_event` in `thread.rs`), which
    // runs a callback that makes a `&mut Probe` from the registered pointer, overlapping that
    // borrow. The test is `cfg(windows)`, so Miri does not check it.
    let moved = move_to_thread(unsafe { &mut (*probe).data }, target, source);
    assert_eq!(moved, Ok(()));

    assert_eq!(
        native_handle(timer),
        None,
        "the source keeps no native timer"
    );
    pump_for(&source_loop, Duration::from_millis(100));
    let (ticking, while_running, after_destroy) =
        report_rx.recv_timeout(WATCHDOG).expect("owner reports");
    owner.join().expect("owner thread must not panic");
    assert!(ticking, "the moved timer keeps ticking on the target");
    assert!(while_running, "and has a native timer there");
    assert!(!after_destroy, "destroying the object releases it");
    assert!(ticks.lock().unwrap().iter().all(|t| t.1 == target));
}
