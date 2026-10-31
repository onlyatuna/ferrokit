// SPDX-License-Identifier: LGPL-3.0-only
//! Destroying an object that still has timers started with `QObject::start_timer` releases the
//! native timers, as `kill_timer` does. The native timer is what keeps posting `WM_TIMER` to the
//! loop's window, so it is checked directly, not only through the callbacks that no longer run:
//!
//! - a native event filter records every `WM_TIMER` the window receives, per timer id;
//! - `KillTimer` returns zero when the native timer does not exist, so calling it on the loop's
//!   window after the object is gone tells whether the timer was released.
#![cfg(windows)]

use ferrokit_core::event::{NativeEventFilter, NativeMessage};
use ferrokit_core::event_loop::EventLoop;
use ferrokit_core::object::{
    register_owned_qobject, unregister_qobject, ObjectData, ObjectId, OwnedObject, QObject,
};
use ferrokit_core::timer::{with_thread_timer_context, TimerId, TimerType};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use windows_sys::Win32::UI::WindowsAndMessaging::{KillTimer, WM_TIMER};

struct WmTimerLog(Arc<Mutex<Vec<u32>>>);

impl NativeEventFilter for WmTimerLog {
    fn native_event_filter(
        &mut self,
        event_type: &str,
        msg: &NativeMessage,
        _result: &mut isize,
    ) -> bool {
        if event_type == "windows_generic_MSG" {
            if let NativeMessage::Windows(m) = msg {
                if m.message == WM_TIMER {
                    self.0.lock().unwrap().push(m.wParam as u32);
                }
            }
        }
        false
    }
}

struct TickObject {
    data: ObjectData,
    ticks: Arc<AtomicU32>,
}

impl QObject for TickObject {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
    }
    fn as_qobject_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn as_qobject_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
    fn timer_event(&mut self, _timer_id: u64) {
        self.ticks.fetch_add(1, Ordering::SeqCst);
    }
}

/// A loop on this thread that records `WM_TIMER`, and a counting object registered with it.
struct Fixture {
    el: EventLoop,
    seen: Arc<Mutex<Vec<u32>>>,
    ticks: Arc<AtomicU32>,
}

impl Fixture {
    fn new() -> (Self, OwnedObject<TickObject>) {
        let el = EventLoop::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        el.install_native_event_filter(Box::new(WmTimerLog(Arc::clone(&seen))));
        let ticks = Arc::new(AtomicU32::new(0));
        // SAFETY: the owner stays on this thread; nothing touches the object while a callback of
        // it runs.
        let obj = unsafe {
            register_owned_qobject(TickObject {
                data: ObjectData::new(ObjectId::next()),
                ticks: Arc::clone(&ticks),
            })
        };
        (Self { el, seen, ticks }, obj)
    }

    fn pump(&self, span: Duration) {
        let end = Instant::now() + span;
        while Instant::now() < end {
            self.el.process_events(false);
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Pumps until `WM_TIMER` has been seen for every id, so each native timer is known to run.
    fn wait_until_running(&self, ids: &[TimerId]) {
        let end = Instant::now() + Duration::from_secs(3);
        while Instant::now() < end {
            self.el.process_events(false);
            if ids
                .iter()
                .all(|id| self.seen.lock().unwrap().contains(&id.0))
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("the native timers never started delivering WM_TIMER");
    }

    fn count(&self, ids: &[TimerId]) -> usize {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|seen| ids.iter().any(|id| id.0 == **seen))
            .count()
    }

    /// Lets a `WM_TIMER` that was already queued when the timer ended drain, then reports how
    /// many arrive over the next stretch. A released timer posts none.
    fn wm_timers_after_the_end(&self, ids: &[TimerId]) -> usize {
        self.pump(Duration::from_millis(100));
        let settled = self.count(ids);
        self.pump(Duration::from_millis(400));
        self.count(ids) - settled
    }
}

/// Whether a native timer of `id` still exists: a multimedia timer (`Precise` timers) or a
/// `SetTimer` timer. `KillTimer` returns nonzero only for a `SetTimer` timer that existed; it also
/// removes that timer, so call this last.
fn native_timer_existed(id: TimerId) -> bool {
    ferrokit_core::timer::precise::native_handle(id).is_some()
        // SAFETY: `ctx.internal_hwnd` is the window of this thread's own timer context, which
        // `with_thread_timer_context` hands out on the calling thread, the thread that owns the
        // window as `KillTimer` requires. A stale id only makes the call return 0 (Win32 docs).
        || with_thread_timer_context(|ctx| unsafe {
            KillTimer(ctx.internal_hwnd, id.0 as usize) != 0
        })
        .expect("this thread has a loop")
}

#[test]
fn a_started_object_timer_exists_natively() {
    let (f, mut obj) = Fixture::new();
    let id = obj
        .with_mut(|o| o.start_timer(20, TimerType::Precise))
        .unwrap();
    f.wait_until_running(&[id]);

    assert!(
        native_timer_existed(id),
        "the probe must see a live native timer, or it proves nothing below"
    );
    // SAFETY: `obj` is an owner held by this test, so the object is alive and unmoved until the
    // end of the fn. This is the thread that registered it (`Fixture::new`) and no loop is
    // running, so no callback of it is active.
    unsafe { unregister_qobject(obj.with(|o| o.object_data().id()).unwrap()) };
}

#[test]
fn kill_timer_releases_the_native_timer() {
    let (f, mut obj) = Fixture::new();
    let id = obj
        .with_mut(|o| o.start_timer(20, TimerType::Precise))
        .unwrap();
    f.wait_until_running(&[id]);

    assert!(obj.with_mut(|o| o.kill_timer(id)).unwrap());

    assert_eq!(f.wm_timers_after_the_end(&[id]), 0);
    assert!(!native_timer_existed(id));
    // SAFETY: `obj` is an owner held by this test, so the object is alive and unmoved until the
    // end of the fn. This is the thread that registered it (`Fixture::new`) and no loop is
    // running, so no callback of it is active.
    unsafe { unregister_qobject(obj.with(|o| o.object_data().id()).unwrap()) };
}

#[test]
fn unregistering_an_object_releases_its_native_timers() {
    let (f, mut obj) = Fixture::new();
    let ids = [
        obj.with_mut(|o| o.start_timer(20, TimerType::Precise))
            .unwrap(),
        obj.with_mut(|o| o.start_timer(25, TimerType::Precise))
            .unwrap(),
    ];
    f.wait_until_running(&ids);
    let ticks_before = f.ticks.load(Ordering::SeqCst);

    // SAFETY: `obj` is an owner held by this test, so the object is alive and unmoved until the
    // end of the fn. This is the thread that registered it and no loop is running, so no
    // callback of it is active.
    unsafe { unregister_qobject(obj.with(|o| o.object_data().id()).unwrap()) };

    assert_eq!(
        f.wm_timers_after_the_end(&ids),
        0,
        "the window still receives WM_TIMER for timers of a destroyed object"
    );
    for id in ids {
        assert!(
            !native_timer_existed(id),
            "the native timer {id} of a destroyed object still exists"
        );
    }
    assert_eq!(
        f.ticks.load(Ordering::SeqCst),
        ticks_before,
        "no callback runs after the object is gone"
    );
}

#[test]
fn dropping_the_owner_releases_its_native_timers() {
    let (f, mut obj) = Fixture::new();
    let id = obj
        .with_mut(|o| o.start_timer(20, TimerType::Precise))
        .unwrap();
    f.wait_until_running(&[id]);

    drop(obj);

    assert_eq!(
        f.wm_timers_after_the_end(&[id]),
        0,
        "the window still receives WM_TIMER after the owner is dropped"
    );
    assert!(
        !native_timer_existed(id),
        "the native timer {id} of a dropped owner still exists"
    );
}
