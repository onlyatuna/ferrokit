// SPDX-License-Identifier: LGPL-3.0-only
//! A multimedia timer as the native source of `Precise` timers on Windows (as in Qt).
//!
//! `SetTimer` delivers `WM_TIMER` on the system tick (about 15.6 ms), so a 5 ms timer runs about
//! every 16 ms. A multimedia timer (`timeSetEvent`, 1 ms resolution) fires on its own thread; its
//! callback only posts a `WM_TIMER` message for the timer id to the loop's window, so the rest of
//! the delivery path is unchanged and every callback still runs on the loop thread.
//!
//! The callback never posts a second message while one is still queued, so a loop that is busy
//! sees one message when it comes back, not one per missed tick.

use super::{TimerId, TimerType};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::Media::{timeKillEvent, timeSetEvent};
use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_TIMER};

const TIME_PERIODIC: u32 = 1;
const TIME_KILL_SYNCHRONOUS: u32 = 0x100;

/// State shared with the callback thread. Boxed so its address is stable until the timer is killed.
struct Posted {
    hwnd: usize,
    id: u32,
    /// Set while a `WM_TIMER` for this timer is queued and not yet dispatched.
    queued: AtomicBool,
}

struct Active {
    handle: u32,
    hwnd: usize,
    _posted: Box<Posted>,
}

impl Drop for Active {
    /// `TIME_KILL_SYNCHRONOUS` makes `timeKillEvent` wait for a running callback, so `_posted` is
    /// not freed while the callback thread can still read it.
    fn drop(&mut self) {
        unsafe {
            timeKillEvent(self.handle);
        }
    }
}

thread_local! {
    static ACTIVE: RefCell<HashMap<u32, Active>> = RefCell::new(HashMap::new());
    static DEFERRED: RefCell<Vec<u32>> = const { RefCell::new(Vec::new()) };
}

unsafe extern "system" fn callback(_id: u32, _msg: u32, user: usize, _dw1: usize, _dw2: usize) {
    let posted = &*(user as *const Posted);
    if !posted.queued.swap(true, Ordering::AcqRel)
        && PostMessageW(posted.hwnd as HWND, WM_TIMER, posted.id as usize, 0) == 0
    {
        // Not queued after all (quota exhausted or window gone): allow the next tick to post.
        posted.queued.store(false, Ordering::Release);
    }
}

/// Whether a timer of this type and interval takes its ticks from a multimedia timer. Matches the
/// types `calculate_next_timeout` treats as precise: `Precise`, and `Coarse` up to 20 ms.
pub(crate) fn wanted(timer_type: TimerType, interval_ms: u64) -> bool {
    match timer_type {
        TimerType::Precise => interval_ms > 0,
        TimerType::Coarse => interval_ms > 0 && interval_ms <= 20,
        TimerType::VeryCoarse => false,
    }
}

/// Starts a periodic multimedia timer that posts `WM_TIMER` for `id` to `hwnd`. Returns false when
/// no multimedia timer could be created; the caller then falls back to `SetTimer`.
pub(crate) fn arm(hwnd: HWND, id: TimerId, interval_ms: u64) -> bool {
    if hwnd.is_null() {
        return false;
    }
    let delay = super::win32_timer_elapse(interval_ms).max(1);
    let posted = Box::new(Posted {
        hwnd: hwnd as usize,
        id: id.0,
        queued: AtomicBool::new(false),
    });
    let user = &*posted as *const Posted as usize;
    let handle = unsafe {
        timeSetEvent(
            delay,
            1,
            Some(callback),
            user,
            TIME_PERIODIC | TIME_KILL_SYNCHRONOUS,
        )
    };
    if handle == 0 {
        return false;
    }
    let previous = ACTIVE.with(|active| {
        active.borrow_mut().insert(
            id.0,
            Active {
                handle,
                hwnd: hwnd as usize,
                _posted: posted,
            },
        )
    });
    drop(previous);
    true
}

/// Stops the multimedia timer of `id`. Returns whether there was one.
pub(crate) fn disarm(id: TimerId) -> bool {
    let _ = DEFERRED.try_with(|deferred| deferred.borrow_mut().retain(|d| *d != id.0));
    let removed = ACTIVE
        .try_with(|active| active.borrow_mut().remove(&id.0))
        .ok()
        .flatten();
    removed.is_some()
}

/// Holds a tick that arrived a little before its timer's expiry; it is delivered once due.
pub(crate) fn defer(id: u32) {
    let _ = DEFERRED.try_with(|deferred| {
        let mut deferred = deferred.borrow_mut();
        if !deferred.contains(&id) {
            deferred.push(id);
        }
    });
}

/// The ticks held by [`defer`]; the caller re-defers those that are still early.
pub(crate) fn take_deferred() -> Vec<u32> {
    DEFERRED
        .try_with(|deferred| std::mem::take(&mut *deferred.borrow_mut()))
        .unwrap_or_default()
}

/// Whether `id` is driven by a multimedia timer on this thread.
pub(crate) fn is_precise(id: u32) -> bool {
    ACTIVE
        .try_with(|active| active.borrow().contains_key(&id))
        .unwrap_or(false)
}

/// The queued message for `id` is being handled; the callback thread may post the next one.
pub(crate) fn acknowledge(id: u32) {
    let _ = ACTIVE.try_with(|active| {
        if let Some(entry) = active.borrow().get(&id) {
            entry._posted.queued.store(false, Ordering::Release);
        }
    });
}

/// Stops every multimedia timer that posts to `hwnd`; used when the window is destroyed.
pub(crate) fn disarm_all_for(hwnd: HWND) {
    let _ = ACTIVE.try_with(|active| {
        active
            .borrow_mut()
            .retain(|_, entry| entry.hwnd != hwnd as usize);
    });
}

/// Native handle of the multimedia timer of `id`, for checks that it was released.
#[doc(hidden)]
pub fn native_handle(id: TimerId) -> Option<u32> {
    ACTIVE
        .try_with(|active| active.borrow().get(&id.0).map(|entry| entry.handle))
        .ok()
        .flatten()
}
