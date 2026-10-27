// SPDX-License-Identifier: LGPL-3.0-only
//! Timer module modeled after Qt timer mechanism (QTimer, QTimerInfo).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Instant;

#[cfg(windows)]
#[doc(hidden)]
pub mod precise;
#[cfg(windows)]
use windows_sys::Win32::Foundation::HWND;
#[cfg(not(windows))]
pub type HWND = *mut std::ffi::c_void;

use crate::object::{ObjectData, ObjectId, OwnedObject, QObject};
use crate::poison::lock_unpoisoned;
use crate::signal::Signal;

static START_TIME: LazyLock<Instant> = LazyLock::new(Instant::now);

/// Process-wide timer id source. Ids must not repeat across thread registries, because a moved
/// object's timers keep their ids on the target thread (Qt `reRegisterTimers`, qobject.cpp:1496-1500).
static NEXT_TIMER_ID: AtomicU32 = AtomicU32::new(1);

thread_local! {
    /// `(receiver, old id) -> new id` for moved timers that were given a fresh id on adoption.
    /// A `Timer` handle, or the owner of an `Object::start_timer` id, learns the new id from
    /// here. An entry lives only while its target timer is registered: unregistering the timer
    /// (kill, single-shot expiry, object destruction), registering a new timer under the old id,
    /// or tearing down the thread's timer context drops it.
    static REIDENTIFIED: RefCell<HashMap<(ObjectId, TimerId), TimerId>> = RefCell::new(HashMap::new());
}

/// Consumes the redirect of `receiver`'s timer `old` (a `Timer` handle adopts the new id once).
fn take_reidentified(receiver: ObjectId, old: TimerId) -> Option<TimerId> {
    REIDENTIFIED.try_with(|map| map.borrow_mut().remove(&(receiver, old))).ok().flatten()
}

/// The id `receiver`'s timer `old` was given on adoption, leaving the redirect in place: the
/// owner of an `Object::start_timer` id may still hold `old` until it kills the timer.
pub(crate) fn reidentified_as(receiver: ObjectId, old: TimerId) -> Option<TimerId> {
    REIDENTIFIED.try_with(|map| map.borrow().get(&(receiver, old)).copied()).ok().flatten()
}

/// Drops the redirect that starts at `old`: a timer registered under `old` supersedes it.
fn forget_reidentified_from(receiver: ObjectId, old: TimerId) {
    let _ = REIDENTIFIED.try_with(|map| {
        let mut map = map.borrow_mut();
        if !map.is_empty() {
            map.remove(&(receiver, old));
        }
    });
}

/// Removes and returns the old ids of `receiver` that redirect to the timer `id`.
fn take_redirects_to(receiver: ObjectId, id: TimerId) -> Vec<TimerId> {
    REIDENTIFIED
        .try_with(|map| {
            let mut map = map.borrow_mut();
            let mut olds = Vec::new();
            if !map.is_empty() {
                map.retain(|&(owner, old), fresh| {
                    let hit = owner == receiver && *fresh == id;
                    if hit {
                        olds.push(old);
                    }
                    !hit
                });
            }
            olds
        })
        .unwrap_or_default()
}

pub fn allocate_timer_id() -> TimerId {
    loop {
        let id = NEXT_TIMER_ID.fetch_add(1, Ordering::Relaxed);
        if id != 0 {
            return TimerId(id);
        }
    }
}

/// Returns monotonic timestamp in milliseconds.
#[inline]
pub fn current_time_ms() -> u64 {
    START_TIME.elapsed().as_millis() as u64
}

/// Unique timer identifier: `TimerId`. Modeled after `Qt::TimerId`.
/// Unique timer identifier: `TimerId`. Modeled after `Qt::TimerId`.

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TimerId(pub u32);

impl TimerId {
    pub const INVALID: Self = TimerId(0);

    #[inline]
    pub fn is_valid(self) -> bool {
        self.0 != 0
    }
}

impl Default for TimerId {
    fn default() -> Self {
        Self::INVALID
    }
}

impl std::fmt::Display for TimerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TimerId({})", self.0)
    }
}

/// Timer precision type: `TimerType`. Modeled after `Qt::TimerType`.
/// Timer precision type: `TimerType`. Modeled after `Qt::TimerType`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum TimerType {
    /// Precise timer (1ms resolution).
    Precise,
    /// Coarse timer (5% tolerance default).
    #[default]
    Coarse,
    /// Very coarse timer (1-second resolution).
    VeryCoarse,
}

/// Registered timer entry matching Qt `WinTimerInfo`.

#[derive(Clone, Debug)]
pub struct TimerEntry {
    pub id: TimerId,
    pub interval_ms: u64,
    pub next_fire_ms: u64,
    pub timer_type: TimerType,
    pub receiver: ObjectId,
    pub win32_timer_id: u32,
    pub in_timer_event: bool,
    pub single_shot: bool,
}

impl TimerEntry {
    pub fn new(
        id: TimerId,
        interval_ms: u64,
        next_fire_ms: u64,
        timer_type: TimerType,
        receiver: ObjectId,
    ) -> Self {
        Self {
            id,
            interval_ms,
            next_fire_ms,
            timer_type,
            receiver,
            win32_timer_id: 0,
            in_timer_event: false,
            single_shot: false,
        }
    }
}

/// Maximum millisecond timeout accepted by Win32 `SetTimer` without clamping
/// (`USER_TIMER_MAXIMUM`, `0x7FFF_FFFF` ms ≈ 24.85 days). Windows caps a larger `uElapse` at
/// `USER_TIMER_MAXIMUM`, so a timer whose deadline is further away receives an intermediate
/// `WM_TIMER` before `next_fire_ms` and re-arms `SetTimer` for the remaining wait.
pub const WIN32_TIMER_MAX_MS: u64 = 0x7FFF_FFFF;

/// Clamps `interval_ms` to [`WIN32_TIMER_MAX_MS`] (`USER_TIMER_MAXIMUM`) for Win32 `SetTimer`.
#[inline]
pub fn win32_timer_elapse(interval_ms: u64) -> u32 {
    interval_ms.min(WIN32_TIMER_MAX_MS) as u32
}

/// Rounds `interval_ms` to the nearest second (halves round up), as Qt's very coarse timers do.
/// An interval within 500 ms of `u64::MAX` rounds down to the largest multiple of 1000 that fits.
fn round_to_second(interval_ms: u64) -> u64 {
    interval_ms.saturating_add(500) / 1000 * 1000
}

/// Calculates next deadline matching calculateNextTimeout.
///
/// Returns the adjusted interval and the absolute deadline, both in milliseconds. Never panics
/// and never wraps, for any interval and clock value in any build profile: a deadline that does
/// not fit in `u64` saturates at `u64::MAX`. The clock (`current_time_ms`) counts milliseconds
/// since process start and cannot reach that value, so such a timer never fires early.
pub fn calculate_next_timeout(
    timer_type: &mut TimerType,
    interval_ms: u64,
    current_time_ms: u64,
) -> (u64, u64) {
    let mut interval = interval_ms;
    let mut current_time = current_time_ms;
    if interval == 0 {
        return (0, current_time);
    }
    match *timer_type {
        TimerType::Precise => {}
        TimerType::Coarse => {
            if interval >= 20000 {
                *timer_type = TimerType::VeryCoarse;
                if interval < 1000 {
                    interval = 1000;
                } else {
                    interval = round_to_second(interval);
                }
                current_time = current_time / 1000 * 1000;
            } else if interval <= 20 {
                *timer_type = TimerType::Precise;
            }
        }
        TimerType::VeryCoarse => {
            if interval < 1000 {
                interval = 1000;
            } else {
                interval = round_to_second(interval);
            }
            current_time = current_time / 1000 * 1000;
        }
    }
    let timeout = current_time.saturating_add(interval);
    (interval, timeout)
}

/// Timer registry matching Qt `WinTimerDict`.

#[derive(Debug, Default)]
pub struct TimerRegistry {
    entries: HashMap<TimerId, TimerEntry>,
    /// Timer ids of each receiver, so destroying or moving an object finds its timers without
    /// scanning every timer of the thread (Qt keeps a per-object timer list). `receiver` of a
    /// registered entry is never changed (`get_mut` callers change deadlines and flags only).
    by_receiver: HashMap<ObjectId, Vec<TimerId>>,
}

impl TimerRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the number of registered timers.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn register(
        &mut self,
        receiver: ObjectId,
        interval_ms: u64,
        timer_type: TimerType,
        single_shot: bool,
    ) -> TimerId {
        // The counter wraps after 2^32 allocations: skip an id that a live timer still holds.
        let id = loop {
            let candidate = allocate_timer_id();
            if !self.entries.contains_key(&candidate) {
                break candidate;
            }
        };
        let registered = self.register_with_id(id, receiver, interval_ms, timer_type, single_shot);
        debug_assert!(registered);
        id
    }

    /// Registers `id` for `receiver`. A moved timer keeps its id on the target thread (Qt
    /// `registerTimer` with an existing id, qobject.cpp:1496-1500); the phase restarts from now.
    /// Returns `false`, leaving the live timer untouched, when `id` is already registered here.
    fn register_with_id(
        &mut self,
        id: TimerId,
        receiver: ObjectId,
        interval_ms: u64,
        timer_type: TimerType,
        single_shot: bool,
    ) -> bool {
        if self.entries.contains_key(&id) {
            return false;
        }
        let now = current_time_ms();
        let mut actual_type = timer_type;
        let (adjusted_interval, next_fire_ms) =
            calculate_next_timeout(&mut actual_type, interval_ms, now);

        let entry = TimerEntry {
            id,
            interval_ms: adjusted_interval,
            next_fire_ms,
            timer_type: actual_type,
            receiver,
            win32_timer_id: 0,
            in_timer_event: false,
            single_shot,
        };

        self.entries.insert(id, entry);
        self.by_receiver.entry(receiver).or_default().push(id);
        // A live timer under `id` supersedes a stale redirect from it (after the counter wrapped).
        forget_reidentified_from(receiver, id);
        true
    }

    fn unindex(&mut self, receiver: ObjectId, id: TimerId) {
        if let Some(ids) = self.by_receiver.get_mut(&receiver) {
            if let Some(at) = ids.iter().position(|&candidate| candidate == id) {
                ids.swap_remove(at);
            }
            if ids.is_empty() {
                self.by_receiver.remove(&receiver);
            }
        }
    }

    pub fn unregister(&mut self, id: TimerId) -> Option<TimerEntry> {
        let entry = self.entries.remove(&id)?;
        self.unindex(entry.receiver, id);
        drop(take_redirects_to(entry.receiver, id));
        Some(entry)
    }

    /// The ids of the timers registered for `receiver`.
    pub(crate) fn ids_for_receiver(&self, receiver: ObjectId) -> &[TimerId] {
        self.by_receiver.get(&receiver).map_or(&[], Vec::as_slice)
    }

    pub fn get(&self, id: TimerId) -> Option<&TimerEntry> {
        self.entries.get(&id)
    }

    pub fn get_mut(&mut self, id: TimerId) -> Option<&mut TimerEntry> {
        self.entries.get_mut(&id)
    }

    pub fn next_deadline(&self) -> Option<u64> {
        self.entries.values().map(|e| e.next_fire_ms).min()
    }

    /// The timers that are due at `now_ms`, in order of expiry (ties by id), as Qt's
    /// `QTimerInfoList` keeps them sorted by timeout.
    pub fn expired_timers(&self, now_ms: u64) -> Vec<TimerId> {
        let mut due: Vec<(u64, TimerId)> = self
            .entries
            .values()
            .filter(|e| !e.in_timer_event && e.interval_ms > 0 && now_ms >= e.next_fire_ms)
            .map(|e| (e.next_fire_ms, e.id))
            .collect();
        due.sort_unstable_by_key(|&(fire, id)| (fire, id.0));
        due.into_iter().map(|(_, id)| id).collect()
    }
}

#[derive(Clone)]
pub struct ThreadTimerContext {
    pub registry: Arc<Mutex<TimerRegistry>>,
    #[cfg(windows)]
    pub internal_hwnd: HWND,
}

thread_local! {
    static THREAD_TIMER_CONTEXT: RefCell<Option<ThreadTimerContext>> = const { RefCell::new(None) };
    static SINGLE_SHOT_CALLBACKS: RefCell<HashMap<ObjectId, Box<dyn FnOnce() + Send>>> =
        RefCell::new(HashMap::new());
    static PENDING_SINGLE_SHOTS: RefCell<Vec<(ObjectId, u64)>> = RefCell::new(Vec::new());
    /// Context-bound single shots of this thread.
    static SINGLE_SHOT_CONTEXTS: RefCell<ContextShots> = RefCell::new(ContextShots::default());
}

pub fn register_thread_timer_context(
    registry: Arc<Mutex<TimerRegistry>>,
    #[cfg(windows)] internal_hwnd: HWND,
) {
    let _ = THREAD_TIMER_CONTEXT.try_with(|ctx| {
        *ctx.borrow_mut() = Some(ThreadTimerContext {
            registry,
            #[cfg(windows)]
            internal_hwnd,
        });
    });
    // Arm single-shot timers requested before this thread had a loop; they must not be dropped.
    let pending = PENDING_SINGLE_SHOTS
        .try_with(|p| std::mem::take(&mut *p.borrow_mut()))
        .unwrap_or_default();
    with_thread_timer_context(|ctx| {
        for (receiver, interval_ms) in pending {
            arm_single_shot(ctx, receiver, interval_ms);
        }
    });
}

pub fn unregister_thread_timer_context() {
    let _ = THREAD_TIMER_CONTEXT.try_with(|ctx| {
        if let Ok(mut borrow) = ctx.try_borrow_mut() {
            *borrow = None;
        }
    });
    let _ = REIDENTIFIED.try_with(|map| map.borrow_mut().clear());
}

pub fn has_thread_timer_context() -> bool {
    THREAD_TIMER_CONTEXT
        .try_with(|ctx| {
            if let Ok(borrow) = ctx.try_borrow() {
                borrow.is_some()
            } else {
                false
            }
        })
        .unwrap_or(false)
}

pub fn with_thread_timer_context<R>(f: impl FnOnce(&ThreadTimerContext) -> R) -> Option<R> {
    THREAD_TIMER_CONTEXT
        .try_with(|ctx| ctx.borrow().as_ref().map(f))
        .ok()
        .flatten()
}

/// Arms one single-shot timer for `receiver` on this thread's registry (Windows: also its native
/// source on the internal window, the same one every other start route uses).
fn arm_single_shot(ctx: &ThreadTimerContext, receiver: ObjectId, interval_ms: u64) {
    let _timer_id =
        lock_unpoisoned(&ctx.registry).register(receiver, interval_ms, TimerType::Coarse, true);
    #[cfg(windows)]
    if !ctx.internal_hwnd.is_null() {
        arm_native(ctx.internal_hwnd, _timer_id, interval_ms, TimerType::Coarse);
    }
}

/// Stops the native source of timer `id`: its multimedia timer if it has one, else its `SetTimer`.
#[cfg(windows)]
pub(crate) fn kill_native(hwnd: HWND, id: TimerId) {
    if !precise::disarm(id) {
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::KillTimer(hwnd, id.0 as usize);
        }
    }
}

/// Whether the ticks of timer `raw_id` come from a multimedia timer on this thread.
#[cfg(windows)]
pub(crate) fn is_precise_source(raw_id: u32) -> bool {
    precise::is_precise(raw_id)
}
#[cfg(not(windows))]
pub(crate) fn is_precise_source(_raw_id: u32) -> bool {
    false
}

/// A message for `raw_id` is being handled: a multimedia timer may queue the next one.
#[cfg(windows)]
pub(crate) fn acknowledge_source(raw_id: u32) {
    precise::acknowledge(raw_id);
}
#[cfg(not(windows))]
pub(crate) fn acknowledge_source(_raw_id: u32) {}

/// What to do with a multimedia tick that arrives before the timer's expiry.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum EarlyTick {
    /// It is the tick of the expiry, a little early: hold it and deliver at the expiry. A timer
    /// never fires before its expiry, and the tick is not lost.
    Defer,
    /// It belongs to a grid point a late callback already consumed (the expiry is more than half
    /// an interval away): ignore it, so an overrun gives one callback and not a second one right
    /// behind it.
    Stale,
}

/// Classifies a tick with `now_ms < expiry_ms` by whether it is nearer the expiry or the grid
/// point before it.
pub(crate) fn classify_early_tick(now_ms: u64, expiry_ms: u64, interval_ms: u64) -> EarlyTick {
    if expiry_ms.saturating_sub(now_ms) <= (interval_ms / 2).max(1) {
        EarlyTick::Defer
    } else {
        EarlyTick::Stale
    }
}

/// Holds the early tick of `raw_id` until the loop next runs the timers.
#[cfg(windows)]
pub(crate) fn defer_source(raw_id: u32) {
    precise::defer(raw_id);
}
#[cfg(not(windows))]
pub(crate) fn defer_source(_raw_id: u32) {}

/// Ticks held by [`defer_source`], to be delivered when due.
#[cfg(windows)]
pub(crate) fn take_deferred_sources() -> Vec<u32> {
    precise::take_deferred()
}
#[cfg(not(windows))]
pub(crate) fn take_deferred_sources() -> Vec<u32> {
    Vec::new()
}

/// Next expiry of a timer whose ticks come from a fixed grid: the first grid point after `now_ms`
/// that follows `expiry`. Grid points missed while the loop was busy are skipped, so an overrun
/// gives one callback and the grid resumes.
pub(crate) fn next_grid_deadline(interval_ms: u64, expiry_ms: u64, now_ms: u64) -> (u64, u64) {
    let interval = interval_ms.max(1);
    let steps = now_ms.saturating_sub(expiry_ms) / interval + 1;
    (
        interval_ms,
        expiry_ms.saturating_add(steps.saturating_mul(interval)),
    )
}

/// Stops and unregisters all active timers registered to the specified receiver object.
/// Modeled after Qt's QObjectPrivate::moveToThread_helper timer cleanup.
///
/// On Windows the native timer of each removed entry is killed as well; it would otherwise keep
/// posting `WM_TIMER` to the loop's window until the window is destroyed.
pub fn stop_timers_for_object(receiver: ObjectId) -> usize {
    cancel_single_shots_for_context(receiver);
    with_thread_timer_context(|ctx| {
        let mut reg = lock_unpoisoned(&ctx.registry);
        let to_remove: Vec<TimerId> = reg.ids_for_receiver(receiver).to_vec();
        let count = to_remove.len();
        for id in to_remove {
            let _entry = reg.unregister(id);
            #[cfg(windows)]
            if let Some(entry) = _entry {
                if entry.interval_ms > 0 && !ctx.internal_hwnd.is_null() {
                    kill_native(ctx.internal_hwnd, id);
                }
            }
        }
        count
    })
    .unwrap_or(0)
}

/// Removes the timers whose receiver is in `receivers` and disarms their delivery sources, returning
/// the entries so the target thread can re-register them under the same ids (Qt
/// `QObjectPrivate::moveToThread_helper` timer handling: `unregisterTimers` on the source,
/// qobject.cpp:1490-1502).
pub(crate) fn detach_timers_for_objects(receivers: &[ObjectId]) -> DetachedTimers {
    // Context-bound single shots move with their context (timers move with the object in Qt).
    let shots = take_context_single_shots(receivers);
    let entries = detach_registered_timers(receivers);
    DetachedTimers { entries, shots }
}

/// Timers taken from a moved subtree, to be re-armed on the target thread by [`adopt_timers`].
pub(crate) struct DetachedTimers {
    entries: Vec<MovedTimer>,
    shots: Vec<(ObjectId, u64, SingleShotFn)>,
}

/// A timer taken from its thread, with the ids its owner may still hold for it from earlier
/// re-identifications; they follow the timer, as the redirects of a thread are dropped with it.
struct MovedTimer {
    entry: TimerEntry,
    aliases: Vec<TimerId>,
}

impl DetachedTimers {
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty() && self.shots.is_empty()
    }
}

fn detach_registered_timers(receivers: &[ObjectId]) -> Vec<MovedTimer> {
    with_thread_timer_context(|ctx| {
        let mut reg = lock_unpoisoned(&ctx.registry);
        let ids: Vec<TimerId> = receivers
            .iter()
            .flat_map(|&receiver| reg.ids_for_receiver(receiver).iter().copied())
            .collect();
        let mut detached = Vec::with_capacity(ids.len());
        for id in ids {
            let aliases = reg
                .get(id)
                .map(|entry| take_redirects_to(entry.receiver, id))
                .unwrap_or_default();
            let Some(entry) = reg.unregister(id) else {
                continue;
            };
            #[cfg(windows)]
            {
                if entry.interval_ms > 0 && !ctx.internal_hwnd.is_null() {
                    kill_native(ctx.internal_hwnd, id);
                }
            }
            detached.push(MovedTimer { entry, aliases });
        }
        detached
    })
    .unwrap_or_default()
}

/// Re-registers detached timers on the calling (target) thread under their original ids and re-arms
/// their delivery sources (Qt `reRegisterTimers`, qobject.cpp:1496-1500). The phase restarts, as with
/// Qt's `registerTimer`. Runs as a `MetaCall` queued to the target after the move. An entry whose
/// receiver has moved on is forwarded to its current owner; one whose receiver was destroyed is
/// dropped.
pub(crate) fn adopt_timers(detached: DetachedTimers) {
    for (context, interval_ms, callback) in detached.shots {
        // Armed here, forwarded to the context's current thread, or dropped if it is gone.
        Timer::single_shot_with_context(interval_ms, context, callback);
    }
    adopt_entries(detached.entries);
}

fn adopt_entries(entries: Vec<MovedTimer>) {
    let current = crate::object::ThreadId::current();
    let mut local = Vec::with_capacity(entries.len());
    for moved in entries {
        match crate::object::qobject::registration_thread_of(moved.entry.receiver) {
            Some(owner) if owner == current => local.push(moved),
            Some(owner) => crate::event_loop::post_event_to_thread(
                owner,
                crate::object::ObjectId(0),
                crate::event::Event::new(crate::event::EventKind::MetaCall(Box::new(move |_| {
                    adopt_entries(vec![moved])
                }))),
            ),
            None => {}
        }
    }
    if local.is_empty() {
        return;
    }
    let lost: Vec<(ObjectId, TimerId)> = local
        .iter()
        .map(|moved| (moved.entry.receiver, moved.entry.id))
        .collect();
    let armed = with_thread_timer_context(|ctx| {
        let mut reg = lock_unpoisoned(&ctx.registry);
        for MovedTimer { entry, mut aliases } in local {
            // Ids are allocated process-wide, so a collision only happens after the counter
            // wrapped; the moved timer then gets a fresh id here instead of being dropped.
            let id = if reg.register_with_id(
                entry.id,
                entry.receiver,
                entry.interval_ms,
                entry.timer_type,
                entry.single_shot,
            ) {
                entry.id
            } else {
                let fresh = reg.register(
                    entry.receiver,
                    entry.interval_ms,
                    entry.timer_type,
                    entry.single_shot,
                );
                // The owner still holds the old id: relabel it in the object's timer list.
                crate::object::with_object_mut(entry.receiver, |obj| {
                    obj.object_data_mut().replace_timer(entry.id, fresh);
                });
                aliases.push(entry.id);
                fresh
            };
            // Tell the owner (`Timer` handle, `kill_timer` caller) the new id of each old one.
            let _ = REIDENTIFIED.try_with(|map| {
                let mut map = map.borrow_mut();
                for old in aliases {
                    map.insert((entry.receiver, old), id);
                }
            });
            arm_source(ctx, id, entry.receiver, entry.interval_ms, entry.timer_type);
        }
    });
    if armed.is_none() {
        eprintln!("ferrokit: cannot re-register moved timers: this thread has no timer context");
        // Nothing is scheduled, so the objects must not keep ids of timers that do not exist.
        for (receiver, id) in lost {
            crate::object::with_object_mut(receiver, |obj| {
                obj.object_data_mut().remove_timer(id);
            });
        }
    }
}

/// Registers a timer and arms its delivery source (`QObject::startTimer` → `registerTimer`,
/// qeventdispatcher_win.cpp:345-381). Zero interval posts `ZeroTimer` (360-363); otherwise
/// `SetTimer` on Windows. Shared by `Timer::start` and `start_object_timer`.
fn arm_in_context(
    ctx: &ThreadTimerContext,
    receiver: ObjectId,
    interval_ms: u64,
    timer_type: TimerType,
    single_shot: bool,
) -> TimerId {
    let mut reg = lock_unpoisoned(&ctx.registry);
    let timer_id = reg.register(receiver, interval_ms, timer_type, single_shot);
    arm_source(ctx, timer_id, receiver, interval_ms, timer_type);
    timer_id
}

/// Arms the native source of a timer on `hwnd`: a multimedia timer for `Precise` timers, else (or
/// when none can be created) `SetTimer`. Every route that starts a timer on Windows ends here, so
/// a timer type has one meaning whichever way the timer was started.
#[cfg(windows)]
pub(crate) fn arm_native(hwnd: HWND, timer_id: TimerId, interval_ms: u64, timer_type: TimerType) {
    if precise::wanted(timer_type, interval_ms) && precise::arm(hwnd, timer_id, interval_ms) {
        return;
    }
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::SetTimer(
            hwnd,
            timer_id.0 as usize,
            win32_timer_elapse(interval_ms),
            None,
        );
    }
}

/// Arms the delivery source of a registered timer: a zero interval posts `ZeroTimer`, otherwise
/// the native source on Windows.
fn arm_source(
    ctx: &ThreadTimerContext,
    timer_id: TimerId,
    receiver: ObjectId,
    interval_ms: u64,
    timer_type: TimerType,
) {
    #[cfg(not(windows))]
    let _ = timer_type;
    #[cfg(not(windows))]
    let _ = ctx;
    if interval_ms == 0 {
        crate::event_loop::post_event_to_thread(
            crate::object::ThreadId::current(),
            receiver,
            crate::event::Event::new(crate::event::EventKind::ZeroTimer {
                timer_id: timer_id.0 as u64,
            }),
        );
    } else {
        #[cfg(windows)]
        {
            if !ctx.internal_hwnd.is_null() {
                arm_native(ctx.internal_hwnd, timer_id, interval_ms, timer_type);
            }
        }
    }
}

/// Removes a timer and disarms its delivery source (`QObject::killTimer` → `unregisterTimer`,
/// qeventdispatcher_win.cpp:383-392). Uses the REGISTERED interval, not the mutable `Timer` field.
/// With `receiver`, a timer registered for another object is left alone.
fn disarm_in_context(ctx: &ThreadTimerContext, receiver: Option<ObjectId>, id: TimerId) -> bool {
    let mut reg = lock_unpoisoned(&ctx.registry);
    if let Some(receiver) = receiver {
        if reg.get(id).is_some_and(|entry| entry.receiver != receiver) {
            return false;
        }
    }
    let Some(entry) = reg.unregister(id) else {
        return false;
    };
    #[cfg(windows)]
    {
        if entry.interval_ms > 0 && !ctx.internal_hwnd.is_null() {
            kill_native(ctx.internal_hwnd, id);
        }
    }
    #[cfg(not(windows))]
    drop(entry);
    true
}

/// Starts an object timer registered to the current thread's timer registry.
pub fn start_object_timer(receiver: ObjectId, interval_ms: u64, timer_type: TimerType) -> TimerId {
    with_thread_timer_context(|ctx| arm_in_context(ctx, receiver, interval_ms, timer_type, false))
        .unwrap_or(TimerId::INVALID)
}

pub(crate) fn register_single_shot_callback(
    receiver: ObjectId,
    callback: impl FnOnce() + Send + 'static,
) -> bool {
    SINGLE_SHOT_CALLBACKS
        .try_with(|callbacks| {
            callbacks.borrow_mut().insert(receiver, Box::new(callback));
        })
        .is_ok()
}

type SingleShotFn = Box<dyn FnOnce() + Send + 'static>;

/// Removes this thread's pending context-bound single shots whose context is in `contexts`:
/// their timers go away and their functors are returned as `(context, interval_ms, functor)`.
fn take_context_single_shots(contexts: &[ObjectId]) -> Vec<(ObjectId, u64, SingleShotFn)> {
    let taken: Vec<(ObjectId, ObjectId)> = SINGLE_SHOT_CONTEXTS
        .try_with(|c| c.borrow_mut().take_for(contexts))
        .unwrap_or_default();
    let mut shots = Vec::with_capacity(taken.len());
    for (receiver, context) in taken {
        let pending = PENDING_SINGLE_SHOTS
            .try_with(|p| {
                let mut p = p.borrow_mut();
                let at = p.iter().position(|(r, _)| *r == receiver)?;
                Some(p.remove(at).1)
            })
            .ok()
            .flatten();
        let armed = with_thread_timer_context(|ctx| {
            let reg = lock_unpoisoned(&ctx.registry);
            reg.ids_for_receiver(receiver)
                .first()
                .and_then(|&id| reg.get(id))
                .map(|e| e.interval_ms)
        })
        .flatten();
        stop_timers_for_object(receiver);
        let callback = SINGLE_SHOT_CALLBACKS
            .try_with(|c| c.borrow_mut().remove(&receiver))
            .ok()
            .flatten();
        if let Some(callback) = callback {
            shots.push((context, pending.or(armed).unwrap_or(0), callback));
        }
    }
    shots
}

/// Cancels this thread's pending context-bound single shots of `context`: the timers go away and
/// the functors (with their captures) are dropped, as Qt's `QSingleShotTimer` is deleted with its
/// context (qsingleshottimer.cpp:46-59).
fn cancel_single_shots_for_context(context: ObjectId) {
    drop(take_context_single_shots(&[context]));
}

/// Context-bound single shots of this thread, indexed both ways: a destroyed context finds its
/// shots without a scan of every shot.
#[derive(Default)]
struct ContextShots {
    /// Timer receiver of a shot -> its context object.
    context_of: HashMap<ObjectId, ObjectId>,
    /// Context object -> the timer receivers of its shots.
    receivers_of: HashMap<ObjectId, Vec<ObjectId>>,
}

impl ContextShots {
    fn insert(&mut self, receiver: ObjectId, context: ObjectId) {
        self.remove(receiver);
        self.context_of.insert(receiver, context);
        self.receivers_of.entry(context).or_default().push(receiver);
    }

    fn remove(&mut self, receiver: ObjectId) {
        let Some(context) = self.context_of.remove(&receiver) else {
            return;
        };
        if let Some(receivers) = self.receivers_of.get_mut(&context) {
            receivers.retain(|&candidate| candidate != receiver);
            if receivers.is_empty() {
                self.receivers_of.remove(&context);
            }
        }
    }

    /// Removes and returns the `(receiver, context)` pairs of the shots bound to `contexts`.
    fn take_for(&mut self, contexts: &[ObjectId]) -> Vec<(ObjectId, ObjectId)> {
        let mut taken = Vec::new();
        for &context in contexts {
            for receiver in self.receivers_of.remove(&context).unwrap_or_default() {
                self.context_of.remove(&receiver);
                taken.push((receiver, context));
            }
        }
        taken
    }
}

pub(crate) fn dispatch_single_shot_callback(receiver: ObjectId) -> bool {
    let _ = SINGLE_SHOT_CONTEXTS.try_with(|c| c.borrow_mut().remove(receiver));
    let callback = SINGLE_SHOT_CALLBACKS
        .try_with(|callbacks| callbacks.borrow_mut().remove(&receiver))
        .ok()
        .flatten();
    if let Some(callback) = callback {
        callback();
        true
    } else {
        false
    }
}

/// Kills an object timer by TimerId in the current thread's timer registry.
pub fn kill_object_timer(id: TimerId) -> bool {
    with_thread_timer_context(|ctx| disarm_in_context(ctx, None, id)).unwrap_or(false)
}

/// Kills `receiver`'s timer `id`; a timer of another object with that id is refused.
pub(crate) fn kill_receiver_timer(receiver: ObjectId, id: TimerId) -> bool {
    with_thread_timer_context(|ctx| disarm_in_context(ctx, Some(receiver), id)).unwrap_or(false)
}

/// Timer event structure matching `QTimerEvent`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimerEvent {
    pub timer_id: TimerId,
}

impl TimerEvent {
    pub fn new(timer_id: TimerId) -> Self {
        Self { timer_id }
    }
}

/// Scheduling state of a [`Timer`], shared by the handle and the object that receives the ticks
/// (`Rc`, one thread). A slot that stops, restarts or reconfigures its own timer, or drops its
/// handle, works on this state and never reaches the object a running callback has borrowed.
struct TimerControl {
    object_id: ObjectId,
    id: Cell<TimerId>,
    interval_ms: Cell<u64>,
    single_shot: Cell<bool>,
    timer_type: Cell<TimerType>,
    retired: Cell<bool>,
    timeout: Signal<()>,
}

impl TimerControl {
    /// The timer's current id, following a re-id from a thread move.
    fn current_id(&self) -> TimerId {
        let id = self.id.get();
        if id.is_valid() {
            if let Some(fresh) = take_reidentified(self.object_id, id) {
                self.id.set(fresh);
                return fresh;
            }
        }
        id
    }

    fn is_active(&self) -> bool {
        self.id.get().is_valid()
    }

    /// Marks the control retired and disarms the scheduled timer. One way: a retired control
    /// starts nothing and emits nothing.
    fn retire(&self) {
        self.retired.set(true);
        self.stop();
    }

    fn start(&self) -> TimerId {
        if self.retired.get() {
            return TimerId::INVALID;
        }
        if self.is_active() {
            self.stop();
        }
        self.rearm()
    }

    /// Registers this timer's current configuration with the thread timer context and records
    /// the new id. Callers must have stopped the timer first, or it must never have been started.
    fn rearm(&self) -> TimerId {
        let receiver = self.object_id;
        let interval_ms = self.interval_ms.get();
        let timer_type = self.timer_type.get();
        let single_shot = self.single_shot.get();

        let id = THREAD_TIMER_CONTEXT.with(|ctx| {
            let borrow = ctx.borrow();
            let Some(ctx) = borrow.as_ref() else {
                return TimerId::INVALID;
            };
            arm_in_context(ctx, receiver, interval_ms, timer_type, single_shot)
        });

        self.id.set(id);
        id
    }

    fn stop(&self) {
        if self.is_active() {
            let id = self.current_id();
            let _ = THREAD_TIMER_CONTEXT.try_with(|ctx| {
                if let Ok(borrow) = ctx.try_borrow() {
                    if let Some(ctx) = borrow.as_ref() {
                        disarm_in_context(ctx, Some(self.object_id), id);
                    }
                }
            });
            self.id.set(TimerId::INVALID);
        }
    }

    fn set_interval(&self, ms: u64) {
        if self.is_active() {
            self.stop();
            self.interval_ms.set(ms);
            self.rearm();
        } else {
            self.interval_ms.set(ms);
        }
    }

    fn set_single_shot(&self, v: bool) {
        self.single_shot.set(v);
        if self.is_active() {
            let id = self.current_id();
            with_thread_timer_context(|ctx| {
                if let Some(entry) = lock_unpoisoned(&ctx.registry).get_mut(id) {
                    entry.single_shot = v;
                }
            });
        }
    }

    fn start_with_registry(&self, registry: &mut TimerRegistry) -> TimerId {
        if self.retired.get() {
            return TimerId::INVALID;
        }
        if self.is_active() {
            self.stop_with_registry(registry);
        }
        let id = registry.register(
            self.object_id,
            self.interval_ms.get(),
            self.timer_type.get(),
            self.single_shot.get(),
        );
        self.id.set(id);
        id
    }

    fn start_with<D: crate::event_loop::EventDispatcher + ?Sized>(
        &self,
        registry: &mut TimerRegistry,
        dispatcher: &mut D,
    ) -> TimerId {
        if self.retired.get() {
            return TimerId::INVALID;
        }
        if self.is_active() {
            self.stop_with(registry, dispatcher);
        }
        let id = registry.register(
            self.object_id,
            self.interval_ms.get(),
            self.timer_type.get(),
            self.single_shot.get(),
        );
        self.id.set(id);
        if let Some(entry) = registry.get(id) {
            dispatcher.register_timer(entry);
        }
        id
    }

    fn stop_with<D: crate::event_loop::EventDispatcher + ?Sized>(
        &self,
        registry: &mut TimerRegistry,
        dispatcher: &mut D,
    ) {
        if self.is_active() {
            if let Some(entry) = registry.unregister(self.current_id()) {
                dispatcher.unregister_timer(&entry);
            }
            self.id.set(TimerId::INVALID);
        }
    }

    fn stop_with_registry(&self, registry: &mut TimerRegistry) {
        if self.is_active() {
            registry.unregister(self.current_id());
            self.id.set(TimerId::INVALID);
        }
    }
}

/// High-level timer object matching `QTimer`.
///
/// `Timer` is the owner handle. The object the registry calls is a private `TimerCore`, registered
/// once by [`Timer::new`] at an address that never changes; the handle and the registry share the
/// one pointer to it, and the handle never forms a reference to the object. Everything the handle
/// does goes through shared scheduling state, so it is safe to call from a slot of its own
/// `timeout`. Dropping the handle from such a slot unregisters the timer at once and frees its
/// memory when the running callback returns. A `Timer` stays on the thread that created it: the
/// handle is `!Send`, and [`move_to_thread_by_id`](crate::object::move_to_thread_by_id) refuses
/// the timer's object, and any ancestor of it, with
/// [`MoveByIdError::NotMovable`](crate::object::MoveByIdError::NotMovable).
///
/// ```compile_fail
/// let t = ferrokit_core::timer::Timer::new();
/// let _ = &*t; // no `Deref`: the object is never reachable from the handle
/// ```
///
/// ```compile_fail
/// fn needs_send<T: Send>(_: T) {}
/// needs_send(ferrokit_core::timer::Timer::new()); // `Timer` stays on its thread
/// ```
pub struct Timer {
    _root: TimerRoot,
    control: Rc<TimerControl>,
    pub timeout: Signal<()>,
}

impl Timer {
    /// Creates a timer and registers its object (`new QTimer`). Nothing is scheduled until
    /// [`Timer::start`].
    pub fn new() -> Self {
        // A `Timer` stays on the thread that made it, so its object never opts in to
        // `ObjectData::with_thread_moves_allowed`: the owner handle is `!Send` and shares the
        // `Rc`-held `TimerControl` with the object. After a move the registry would run
        // `timer_event` on the target thread while the handle reads and writes that state on this
        // one, and the handle would drop the object from the wrong thread.
        let data = ObjectData::new(ObjectId::next());
        let object_id = data.id();
        let timeout = Signal::with_owner(&data);
        let control = Rc::new(TimerControl {
            object_id,
            id: Cell::new(TimerId::INVALID),
            interval_ms: Cell::new(0),
            single_shot: Cell::new(false),
            timer_type: Cell::new(TimerType::Coarse),
            retired: Cell::new(false),
            timeout: timeout.clone(),
        });
        let core = TimerCore {
            data,
            control: Rc::clone(&control),
        };
        // SAFETY: the owner is private to `Timer`, which is `!Send` and never forms a reference
        // to the object. While a callback of the object runs it is not accessed or freed by the
        // owner: dropping the owner parks the memory (`TimerRoot::drop`).
        let owner = unsafe { crate::object::register_owned_qobject(core) };
        Self {
            _root: TimerRoot { owner: Some(owner) },
            control,
            timeout,
        }
    }

    /// The identity of the timer's object (`QObject` identity). It is stable for the handle's
    /// lifetime and is not the id of a scheduled timer: see [`Timer::timer_id`].
    pub fn object_id(&self) -> ObjectId {
        self.control.object_id
    }

    /// Starts the timer with its configured interval (`QTimer::start`).
    pub fn start(&self) -> TimerId {
        self.control.start()
    }

    /// Starts the timer with the specified interval (`QTimer::start(msec)`).
    pub fn start_with_interval(&self, ms: u64) -> TimerId {
        self.control.interval_ms.set(ms);
        self.control.start()
    }

    /// Stops the timer (`QTimer::stop`).
    pub fn stop(&self) {
        self.control.stop();
    }

    pub fn is_active(&self) -> bool {
        self.control.is_active()
    }

    /// Sets the interval (`QTimer::setInterval`); an active timer is restarted with it.
    pub fn set_interval(&self, ms: u64) {
        self.control.set_interval(ms);
    }

    pub fn interval(&self) -> u64 {
        self.control.interval_ms.get()
    }

    /// Sets single-shot mode; an active timer's registry entry is updated to match.
    pub fn set_single_shot(&self, v: bool) {
        self.control.set_single_shot(v);
    }

    pub fn is_single_shot(&self) -> bool {
        self.control.single_shot.get()
    }

    pub fn set_timer_type(&self, t: TimerType) {
        self.control.timer_type.set(t);
    }

    pub fn timer_type(&self) -> TimerType {
        self.control.timer_type.get()
    }

    /// The id of the currently scheduled timer, `TimerId::INVALID` when stopped. It changes on
    /// every start. It is not the identity of the object: see [`Timer::object_id`].
    pub fn timer_id(&self) -> TimerId {
        self.control.current_id()
    }

    /// Returns remaining time in milliseconds (`QTimer::remainingTime`).
    pub fn remaining_time(&self) -> i64 {
        if !self.is_active() {
            return -1;
        }
        let now = current_time_ms();
        let deadline_opt = with_thread_timer_context(|ctx| {
            lock_unpoisoned(&ctx.registry)
                .get(self.control.current_id())
                .map(|e| e.next_fire_ms)
        })
        .flatten();

        let Some(deadline) = deadline_opt else {
            return -1;
        };

        if deadline > now {
            i64::try_from(deadline - now).unwrap_or(i64::MAX)
        } else {
            0
        }
    }

    /// Starts a timer in a caller-supplied registry.
    pub fn start_with_registry(&self, registry: &mut TimerRegistry) -> TimerId {
        self.control.start_with_registry(registry)
    }

    /// Starts a timer in a caller-supplied registry and dispatcher.
    pub fn start_with<D: crate::event_loop::EventDispatcher + ?Sized>(
        &self,
        registry: &mut TimerRegistry,
        dispatcher: &mut D,
    ) -> TimerId {
        self.control.start_with(registry, dispatcher)
    }

    /// Timer registry matching Qt `WinTimerDict`.
    pub fn stop_with<D: crate::event_loop::EventDispatcher + ?Sized>(
        &self,
        registry: &mut TimerRegistry,
        dispatcher: &mut D,
    ) {
        self.control.stop_with(registry, dispatcher);
    }

    /// Timer registry matching Qt `WinTimerDict`.
    pub fn stop_with_registry(&self, registry: &mut TimerRegistry) {
        self.control.stop_with_registry(registry);
    }

    pub fn timer_event_with_registry(&self, event: &TimerEvent, registry: &mut TimerRegistry) {
        if event.timer_id == self.control.current_id() {
            if self.control.single_shot.get() {
                self.control.stop_with_registry(registry);
            }
            self.timeout.emit(&());
        }
    }
    /// `QTimer::singleShot(msec, context, functor)` (qsingleshottimer.cpp:46-59): `callback` runs
    /// once on `context`'s thread, queued there when called from another thread, and is dropped
    /// unrun if `context` is destroyed first. An unregistered `context` drops it at once.
    pub fn single_shot_with_context(
        interval_ms: u64,
        context: ObjectId,
        callback: impl FnOnce() + Send + 'static,
    ) {
        let Some(thread_id) = crate::object::qobject::registration_thread_of(context) else {
            return;
        };
        if interval_ms == 0 || thread_id != crate::object::ThreadId::current() {
            // Posted with the context as receiver: destroying it purges the event and drops the
            // functor. For a nonzero interval the arm happens on the context's thread.
            crate::event_loop::post_event_to_thread(
                thread_id,
                context,
                crate::event::Event::new(crate::event::EventKind::MetaCallNoTarget(Box::new(move || {
                    if interval_ms == 0 {
                        callback();
                    } else {
                        Self::arm_context_single_shot(interval_ms, context, callback);
                    }
                }))),
            );
        } else {
            Self::arm_context_single_shot(interval_ms, context, callback);
        }
    }

    fn arm_context_single_shot(
        interval_ms: u64,
        context: ObjectId,
        callback: impl FnOnce() + Send + 'static,
    ) {
        let receiver = ObjectId::next();
        if !register_single_shot_callback(receiver, callback) {
            return;
        }
        let _ = SINGLE_SHOT_CONTEXTS.try_with(|c| c.borrow_mut().insert(receiver, context));
        if with_thread_timer_context(|ctx| arm_single_shot(ctx, receiver, interval_ms)).is_none() {
            let _ = PENDING_SINGLE_SHOTS.try_with(|p| p.borrow_mut().push((receiver, interval_ms)));
        }
    }

    /// Fires a single-shot timer callback (`QTimer::singleShot`).
    pub fn single_shot(interval_ms: u64, callback: impl FnOnce() + Send + 'static) {
        let thread_id = crate::object::ThreadId::current();
        if interval_ms == 0 {
            crate::event_loop::post_event_to_thread(
                thread_id,
                crate::object::ObjectId(0),
                crate::event::Event::new(crate::event::EventKind::MetaCall(Box::new(move |_| {
                    callback();
                }))),
            );
            return;
        }

        let receiver = ObjectId::next();
        if !register_single_shot_callback(receiver, callback) {
            return;
        }
        // Keep the arm when this thread has no timer context yet; `register_thread_timer_context`
        // arms it once the thread's event loop exists, instead of silently dropping it.
        if with_thread_timer_context(|ctx| arm_single_shot(ctx, receiver, interval_ms)).is_none() {
            let _ = PENDING_SINGLE_SHOTS.try_with(|p| p.borrow_mut().push((receiver, interval_ms)));
        }
    }

    /// `QTimer::singleShot(msec, functor)` for a functor that is not `Send`: `callback` may
    /// capture an `Rc`, a `RefCell` or a widget tree. It runs once, on the calling thread, from
    /// that thread's event loop, after `interval_ms` (0: at the next loop turn, queued).
    ///
    /// [`Timer::single_shot`] asks for `Send` because its timer entry is shared state. Here the
    /// closure stays in a table of the calling thread (the one `Signal::connect_local` uses) and
    /// the timer or posted event carries only a key, so no `unsafe` is involved.
    ///
    /// Differences from Qt:
    /// * A timer armed on a thread that has no event loop yet is kept and armed when the loop
    ///   starts, as for [`Timer::single_shot`].
    /// * If the loop or the thread ends before the closure runs, it never runs. It is dropped on
    ///   the calling thread when its posted event is discarded (0 ms). Dropping the
    ///   `EventLoop` does not drop the closure of an armed timer (interval > 0): it stays in the
    ///   thread's table until the thread-local destructor at thread exit. A capture whose
    ///   destructor uses another thread-local may find it already destroyed then.
    /// * A panic in `callback` is contained like the panic of any timer callback.
    pub fn single_shot_local(interval_ms: u64, callback: impl FnOnce() + 'static) {
        let handle = crate::signal::local::LocalHandle::install_once(callback);
        Self::single_shot(interval_ms, move || {
            handle.run_once();
        });
    }
}

/// Marks timer `id` as in delivery and schedules its next deadline, under a short registry lock.
/// Returns the receiver and the single-shot flag, or `None` when the timer is gone or already in
/// delivery. The lock is released before the caller sends the event: Qt sends the timer event with
/// no registry lock held (`QTimerInfoList::activateTimers`, qtimerinfo_unix.cpp:374-427), so a slot
/// may stop, restart or start timers.
pub(crate) fn begin_timer_delivery(
    registry: &Mutex<TimerRegistry>,
    id: TimerId,
    now_ms: u64,
) -> Option<(ObjectId, bool)> {
    let mut reg = lock_unpoisoned(registry);
    let entry = reg.get_mut(id)?;
    let precise = is_precise_source(id.0);
    if entry.in_timer_event {
        return None;
    }
    if now_ms < entry.next_fire_ms {
        if precise
            && classify_early_tick(now_ms, entry.next_fire_ms, entry.interval_ms) == EarlyTick::Defer
        {
            defer_source(id.0);
        }
        return None;
    }
    entry.in_timer_event = true;
    let mut timer_type = entry.timer_type;
    let (adjusted, next_fire) = if precise {
        next_grid_deadline(entry.interval_ms, entry.next_fire_ms, now_ms)
    } else {
        calculate_next_timeout(&mut timer_type, entry.interval_ms, now_ms)
    };
    entry.timer_type = timer_type;
    entry.interval_ms = adjusted;
    entry.next_fire_ms = next_fire;
    Some((entry.receiver, entry.single_shot))
}

/// Sends the tick of timer `raw_id` to `receiver`. A `Timer::single_shot` callback has no object
/// behind it: its receiver is not registered, so the callback runs instead. An event filter that
/// consumes the tick of such a receiver must not strand the callback in its thread-local map
/// either.
pub(crate) fn send_tick(receiver: ObjectId, raw_id: u64) {
    use crate::event_loop::{deliver_timer_event, TimerDelivery};
    let run_callback = match deliver_timer_event(receiver, raw_id) {
        TimerDelivery::NotDeliverable => true,
        TimerDelivery::Filtered => SINGLE_SHOT_CALLBACKS
            .try_with(|callbacks| callbacks.borrow().contains_key(&receiver))
            .unwrap_or(false),
        TimerDelivery::Delivered => false,
    };
    if run_callback {
        dispatch_single_shot_callback(receiver);
    }
}

/// Delivers the timers of `registry` that are due at `now_ms`, in order of expiry, plus the
/// `extra` ids a dispatcher found due itself. A slot that panics is contained: the timer keeps
/// its schedule and the rest of the batch is delivered, as the posted-event pump does. Returns
/// the ids of the single-shot timers that were delivered and removed.
pub(crate) fn deliver_due_timers(
    registry: &Mutex<TimerRegistry>,
    now_ms: u64,
    extra: impl IntoIterator<Item = TimerId>,
) -> Vec<TimerId> {
    let mut ids = lock_unpoisoned(registry).expired_timers(now_ms);
    let mut seen: std::collections::HashSet<TimerId> = ids.iter().copied().collect();
    ids.extend(extra.into_iter().filter(|id| seen.insert(*id)));

    let mut retired = Vec::new();
    for id in ids {
        let Some((receiver, single_shot)) = begin_timer_delivery(registry, id, now_ms) else {
            continue;
        };
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            send_tick(receiver, id.0 as u64)
        }));
        let mut reg = lock_unpoisoned(registry);
        if single_shot {
            reg.unregister(id);
            retired.push(id);
        } else if let Some(entry) = reg.get_mut(id) {
            entry.in_timer_event = false;
        }
    }
    retired
}

/// Re-posts a zero-interval repeating timer after its slot ran (Qt re-arms zero timers each
/// loop iteration). Skips the repost when the timer was stopped, restarted, or made single-shot
/// inside the slot, since the registry entry for `timer_id` is then gone or changed.
pub fn repost_zero_timer(receiver: ObjectId, timer_id: u64) {
    let id = TimerId(timer_id as u32);
    let still_armed = with_thread_timer_context(|ctx| {
        let reg = lock_unpoisoned(&ctx.registry);
        reg.get(id)
            .is_some_and(|e| e.receiver == receiver && e.interval_ms == 0 && !e.single_shot)
    })
    .unwrap_or(false);
    if still_armed {
        crate::event_loop::post_event_to_thread(
            crate::object::ThreadId::current(),
            receiver,
            crate::event::Event::new(crate::event::EventKind::ZeroTimer { timer_id }),
        );
    }
}

/// Whether the zero-interval timer `timer_id` of `receiver` is still registered. A queued
/// `ZeroTimer` of a timer that was killed since is dropped, as Qt sends a zero timer only while
/// `timerDict` has it (qeventdispatcher_win.cpp:879-899) and removes the pending ones in
/// `unregisterTimer`.
pub(crate) fn zero_timer_armed(receiver: ObjectId, timer_id: u64) -> bool {
    let id = TimerId(timer_id as u32);
    with_thread_timer_context(|ctx| {
        lock_unpoisoned(&ctx.registry)
            .get(id)
            .is_some_and(|e| e.receiver == receiver && e.interval_ms == 0)
    })
    .unwrap_or(false)
}

/// The object the registry calls for a [`Timer`]. It is private: only a registry callback ever
/// holds a `&mut` to it, and the owner side never forms a reference to it.
struct TimerCore {
    data: ObjectData,
    control: Rc<TimerControl>,
}

#[cfg(test)]
thread_local! {
    /// Counts destructor runs of `TimerCore` on this thread (lifecycle tests).
    static CORE_DROPS: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
impl Drop for TimerCore {
    fn drop(&mut self) {
        CORE_DROPS.with(|drops| drops.set(drops.get() + 1));
    }
}

impl QObject for TimerCore {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }

    fn object_data_mut(&mut self) -> crate::object::ObjectDataMut<'_> {
        crate::object::ObjectDataMut::new(&mut self.data)
    }
    /// Handles timer events for this object (`QObject::timerEvent`).
    ///
    /// A slot may drop the owning [`Timer`] while this runs. The control and the signal are
    /// therefore cloned into locals first and the emission goes through the local clone, so
    /// `emit` never borrows a field of this object; the object's memory itself is kept until
    /// this callback returns (see `TimerRoot`).
    fn timer_event(&mut self, timer_id: u64) {
        let control = Rc::clone(&self.control);
        if control.retired.get() {
            return;
        }
        if timer_id == control.current_id().0 as u64 {
            if control.single_shot.get() {
                control.stop();
            }
            let timeout = control.timeout.clone();
            timeout.emit(&());
        }
    }
}

// `TimerCore` is `Unpin`, so Miri checks that the `&mut` handed to a callback is unique.
const _: () = {
    const fn assert_unpin<T: Unpin>() {}
    assert_unpin::<TimerCore>();
};

/// Owner of the `TimerCore` allocation. It has no `Deref`: the owner side never reads or writes
/// the object. Dropping it destroys the object at once when no callback of it runs; otherwise the
/// object is unregistered at once and its memory is parked until the running callback returns
/// (`destroy_when_released`).
struct TimerRoot {
    owner: Option<OwnedObject<TimerCore>>,
}

impl Drop for TimerRoot {
    fn drop(&mut self) {
        if let Some(owner) = self.owner.take() {
            crate::object::qobject::destroy_when_released(OwnedObject::into_dyn(owner));
        }
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        // Runs before the fields drop, so before the root: mark the control retired and disarm
        // the scheduled timer. `unregister_qobject`, run by the root, removes any registry entry
        // this `stop` could not reach.
        self.control.retire();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_timer_id() {
        assert_eq!(TimerId::INVALID, TimerId(0));
        assert!(!TimerId::INVALID.is_valid());
        assert_eq!(TimerId::default(), TimerId::INVALID);

        let valid_id = TimerId(1);
        assert!(valid_id.is_valid());
        assert_ne!(valid_id, TimerId::INVALID);
        assert_eq!(valid_id.0, 1);
    }
    #[test]
    fn test_timer_id_global_monotonicity() {
        let id1 = allocate_timer_id();
        let id2 = allocate_timer_id();
        assert!(id1.is_valid());
        assert!(id2.is_valid());
        assert_ne!(
            id1, id2,
            "TimerId must be unique globally across registries"
        );
        assert!(
            id2.0 > id1.0,
            "TimerId must increment monotonically globally"
        );
    }

    #[test]
    fn test_timer_type_defaults() {
        assert_eq!(TimerType::default(), TimerType::Coarse);
        assert_ne!(TimerType::Precise, TimerType::Coarse);
        assert_ne!(TimerType::VeryCoarse, TimerType::Coarse);
    }

    #[test]
    fn test_timer_entry_creation() {
        let entry = TimerEntry::new(TimerId(10), 100, 1100, TimerType::Precise, ObjectId(99));
        assert_eq!(entry.id, TimerId(10));
        assert_eq!(entry.interval_ms, 100);
        assert_eq!(entry.next_fire_ms, 1100);
        assert_eq!(entry.timer_type, TimerType::Precise);
        assert_eq!(entry.receiver, ObjectId(99));
        assert_eq!(entry.win32_timer_id, 0);
        assert!(!entry.in_timer_event);
        assert!(!entry.single_shot);
    }

    #[test]
    fn test_calculate_next_timeout() {
        // 1. Precise
        let mut t1 = TimerType::Precise;
        let (inv1, to1) = calculate_next_timeout(&mut t1, 15, 100);
        assert_eq!(t1, TimerType::Precise);
        assert_eq!(inv1, 15);
        assert_eq!(to1, 115);

        let mut t2 = TimerType::Coarse;
        let (inv2, to2) = calculate_next_timeout(&mut t2, 10, 100);
        assert_eq!(t2, TimerType::Precise);
        assert_eq!(inv2, 10);
        assert_eq!(to2, 110);

        let mut t3 = TimerType::Coarse;
        let (inv3, to3) = calculate_next_timeout(&mut t3, 25000, 5500);
        assert_eq!(t3, TimerType::VeryCoarse);
        assert_eq!(inv3, 25000);
        assert_eq!(to3, 5000 + 25000);

        let mut t4 = TimerType::VeryCoarse;
        let (inv4, to4) = calculate_next_timeout(&mut t4, 1400, 1200);
        assert_eq!(t4, TimerType::VeryCoarse);
        assert_eq!(inv4, 1000);
        assert_eq!(to4, 1000 + 1000);
    }

    /// Expected outcome of `calculate_next_timeout`, computed in 128-bit arithmetic that cannot
    /// overflow, for the type the call settles on.
    fn expected_next_timeout(settled: TimerType, interval: u64, now: u64) -> (u64, u64) {
        let max = u64::MAX as u128;
        let (adjusted, floor_now) = match settled {
            TimerType::VeryCoarse => {
                let rounded = ((interval as u128 + 500) / 1000 * 1000).min(max / 1000 * 1000);
                (rounded.max(1000), now as u128 / 1000 * 1000)
            }
            TimerType::Precise | TimerType::Coarse => (interval as u128, now as u128),
        };
        (adjusted as u64, (floor_now + adjusted).min(max) as u64)
    }

    #[test]
    fn calculate_next_timeout_saturates_instead_of_overflowing() {
        let intervals = [
            1,
            999,
            20_000,
            u64::MAX / 2,
            i64::MAX as u64,
            u64::MAX - 501,
            u64::MAX - 500,
            u64::MAX - 499,
            u64::MAX - 1,
            u64::MAX,
        ];
        let clocks = [0, 999, 1_000_000, u64::MAX / 2, u64::MAX - 1, u64::MAX];
        for start in [TimerType::Precise, TimerType::Coarse, TimerType::VeryCoarse] {
            for interval in intervals {
                for now in clocks {
                    let mut settled = start;
                    let got = calculate_next_timeout(&mut settled, interval, now);
                    assert_eq!(
                        got,
                        expected_next_timeout(settled, interval, now),
                        "{start:?} interval {interval} clock {now}"
                    );
                }
            }
        }
    }

    #[test]
    fn an_unreachable_deadline_is_clamped_to_the_far_future() {
        // The largest multiple of 1000 that fits is the rounded very coarse interval.
        let largest_rounded = u64::MAX / 1000 * 1000;
        let mut very_coarse = TimerType::VeryCoarse;
        assert_eq!(
            calculate_next_timeout(&mut very_coarse, u64::MAX, 1000),
            (largest_rounded, u64::MAX)
        );

        let mut coarse = TimerType::Coarse;
        assert_eq!(
            calculate_next_timeout(&mut coarse, u64::MAX, 1000),
            (largest_rounded, u64::MAX)
        );
        assert_eq!(coarse, TimerType::VeryCoarse);

        let mut precise = TimerType::Precise;
        assert_eq!(
            calculate_next_timeout(&mut precise, u64::MAX, 1000),
            (u64::MAX, u64::MAX)
        );

        // A saturated deadline is far beyond every clock reading, so the timer is registered but
        // never reported as expired.
        for ty in [TimerType::Precise, TimerType::Coarse, TimerType::VeryCoarse] {
            let mut registry = TimerRegistry::new();
            let id = registry.register(ObjectId(1), u64::MAX, ty, false);
            assert!(registry.get(id).unwrap().next_fire_ms >= largest_rounded);
            assert!(registry.next_deadline().unwrap() >= largest_rounded);
            assert!(registry.expired_timers(largest_rounded - 1).is_empty());
        }
    }

    #[test]
    fn test_timer_registry_crud_and_deadline() {
        let mut registry = TimerRegistry::new();
        assert!(registry.is_empty());
        assert_eq!(registry.next_deadline(), None);

        let receiver = ObjectId(100);
        let id1 = registry.register(receiver, 500, TimerType::Precise, false);
        assert!(id1.is_valid());
        assert_eq!(registry.len(), 1);

        let entry1 = registry.get(id1).expect("entry1 should exist");
        assert_eq!(entry1.interval_ms, 500);
        assert!(!entry1.single_shot);

        let id2 = registry.register(receiver, 100, TimerType::Precise, true);
        assert!(id2.is_valid());
        assert_ne!(id1, id2);
        assert_eq!(registry.len(), 2);

        let deadline = registry.next_deadline().unwrap();
        let entry2 = registry.get(id2).unwrap();
        assert_eq!(deadline, entry2.next_fire_ms);

        if let Some(entry_mut) = registry.get_mut(id1) {
            entry_mut.in_timer_event = true;
        }
        assert!(registry.get(id1).unwrap().in_timer_event);

        let removed2 = registry.unregister(id2).expect("id2 removed");
        assert_eq!(removed2.id, id2);
        assert_eq!(registry.len(), 1);
        assert_eq!(
            registry.next_deadline(),
            Some(registry.get(id1).unwrap().next_fire_ms)
        );

        registry.unregister(id1);
        assert!(registry.is_empty());
        assert_eq!(registry.next_deadline(), None);
    }

    #[test]
    fn test_timer_start_stop_and_timeout_order() {
        use std::sync::{Arc, Mutex};

        let mut registry = TimerRegistry::new();
        let timer = Timer::new();
        timer.set_interval(100);
        timer.set_single_shot(true);

        let log = Arc::new(Mutex::new(Vec::<&'static str>::new()));
        let log2 = Arc::clone(&log);
        timer.timeout.connect(move |_| {
            log2.lock().unwrap().push("timeout");
        });

        timer.start_with_registry(&mut registry);
        assert!(timer.is_active());
        let id = timer.timer_id();
        assert!(registry.get(id).is_some());

        timer.timer_event_with_registry(&TimerEvent::new(id), &mut registry);
        assert!(!timer.is_active());
        assert!(registry.get(id).is_none());
        assert_eq!(*log.lock().unwrap(), ["timeout"]);
    }

    #[test]
    fn test_timer_ergonomic_api_with_event_loop() {
        use crate::event_loop::EventLoop;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::Duration;

        let event_loop = EventLoop::new();
        let timer = Timer::new();
        timer.set_interval(10);
        timer.set_single_shot(true);

        let fired = Arc::new(AtomicBool::new(false));
        let f_clone = Arc::clone(&fired);
        timer.timeout.connect(move |_| {
            f_clone.store(true, Ordering::SeqCst);
        });

        timer.start();
        assert!(timer.is_active());

        let deadline = std::time::Instant::now() + Duration::from_millis(500);
        while !fired.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
            event_loop.process_events(false);
            std::thread::sleep(Duration::from_millis(5));
        }

        assert!(fired.load(Ordering::SeqCst));
        assert!(!timer.is_active());
    }

    #[cfg(windows)]
    fn spin_until(
        dispatcher: &mut crate::event_loop::Win32EventDispatcher,
        registry: &Mutex<TimerRegistry>,
        timeout: std::time::Duration,
        mut condition: impl FnMut() -> bool,
    ) -> bool {
        let start = std::time::Instant::now();
        while !condition() {
            if start.elapsed() > timeout {
                return false;
            }
            dispatcher.process_events_with_timers(false, None, registry);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        true
    }

    #[cfg(windows)]
    #[test]
    fn test_single_shot_timer() {
        use crate::event_loop::Win32EventDispatcher;
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::time::Duration;

        let mut dispatcher = Win32EventDispatcher::new();
        let registry = Mutex::new(TimerRegistry::new());

        let counter = Arc::new(AtomicU32::new(0));
        let counter_clone = counter.clone();

        let timer = Timer::new();
        timer.set_single_shot(true);
        timer.set_interval(30);
        timer.timeout.connect(move |_| {
            counter_clone.fetch_add(1, Ordering::SeqCst);
        });

        timer.start_with(&mut registry.lock().unwrap(), &mut dispatcher);
        assert!(timer.is_active());

        let ok = spin_until(
            &mut dispatcher,
            &registry,
            Duration::from_millis(500),
            || counter.load(Ordering::SeqCst) >= 1,
        );

        assert!(ok, "Timer failed to fire within timeout");
        assert_eq!(counter.load(Ordering::SeqCst), 1);

        std::thread::sleep(Duration::from_millis(50));
        dispatcher.process_events_with_timers(false, None, &registry);
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        assert!(
            !timer.is_active(),
            "Single shot timer should be inactive after firing"
        );
    }

    #[cfg(windows)]
    #[test]
    fn test_timer_cancellation() {
        use crate::event_loop::Win32EventDispatcher;
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::time::Duration;

        let mut dispatcher = Win32EventDispatcher::new();
        let registry = Mutex::new(TimerRegistry::new());

        let counter = Arc::new(AtomicU32::new(0));
        let counter_clone = counter.clone();

        let timer = Timer::new();
        timer.set_interval(50);
        timer.timeout.connect(move |_| {
            counter_clone.fetch_add(1, Ordering::SeqCst);
        });

        timer.start_with(&mut registry.lock().unwrap(), &mut dispatcher);

        timer.stop_with(&mut registry.lock().unwrap(), &mut dispatcher);
        assert!(!timer.is_active());

        let start = std::time::Instant::now();
        while start.elapsed() < Duration::from_millis(100) {
            dispatcher.process_events_with_timers(false, None, &registry);
            std::thread::sleep(Duration::from_millis(5));
        }

        assert_eq!(counter.load(Ordering::SeqCst), 0);
    }
    #[test]
    fn test_zero_timer_immediate_dispatch() {
        use crate::event_loop::EventLoop;
        use std::sync::atomic::{AtomicBool, Ordering};

        let event_loop = EventLoop::new();

        let timer = Timer::new();
        timer.set_interval(0);
        timer.set_single_shot(true);

        let fired = Arc::new(AtomicBool::new(false));
        let f_clone = Arc::clone(&fired);
        timer.timeout.connect(move |_| {
            f_clone.store(true, Ordering::SeqCst);
        });

        timer.start();
        assert!(timer.is_active());

        assert!(!fired.load(Ordering::SeqCst));

        let processed = event_loop.process_events(false);
        assert!(processed);
        assert!(
            fired.load(Ordering::SeqCst),
            "Zero timer should fire immediately in the first process_events call"
        );
        assert!(
            !timer.is_active(),
            "Single shot zero timer should be inactive after firing"
        );
        assert_eq!(
            event_loop.timer_registry().lock().unwrap().len(),
            0,
            "fired single-shot zero timer must unregister from the registry"
        );
    }

    #[test]
    fn zero_single_shot_leaves_registry_empty() {
        use crate::event_loop::EventLoop;
        use std::sync::atomic::{AtomicBool, Ordering};

        let el = EventLoop::new();
        let fired = Arc::new(AtomicBool::new(false));
        let f = Arc::clone(&fired);
        let timer = Timer::new();
        timer.set_interval(0);
        timer.set_single_shot(true);
        timer.timeout.connect(move |_| {
            f.store(true, Ordering::SeqCst);
        });
        timer.start();
        el.process_events(false);
        assert!(fired.load(Ordering::SeqCst));
        assert_eq!(
            el.timer_registry().lock().unwrap().len(),
            0,
            "fired zero single-shot must unregister"
        );
        assert_eq!(
            el.next_timeout(),
            None,
            "idle loop must not spin on a fired zero timer"
        );
    }

    #[test]
    fn zero_timer_repeats_each_iteration() {
        use crate::event_loop::EventLoop;
        use std::sync::atomic::{AtomicU32, Ordering};

        let el = EventLoop::new();
        let count = Arc::new(AtomicU32::new(0));
        let c = Arc::clone(&count);
        let timer = Timer::new();
        timer.set_interval(0);
        timer.timeout.connect(move |_| {
            c.fetch_add(1, Ordering::SeqCst);
        });
        timer.start();
        for _ in 0..100 {
            if count.load(Ordering::SeqCst) >= 5 {
                break;
            }
            el.process_events(false);
        }
        assert!(
            count.load(Ordering::SeqCst) >= 5,
            "repeating zero timer must fire on successive iterations, got {}",
            count.load(Ordering::SeqCst)
        );
        timer.stop();
    }

    #[test]
    fn set_interval_on_active_timer_restarts_with_new_interval() {
        use crate::event_loop::EventLoop;
        use std::sync::atomic::{AtomicU32, Ordering};

        let el = EventLoop::new();
        let count = Arc::new(AtomicU32::new(0));
        let c = Arc::clone(&count);
        let timer = Timer::new();
        timer.set_interval(60_000);
        timer.timeout.connect(move |_| {
            c.fetch_add(1, Ordering::SeqCst);
        });
        timer.start();
        timer.set_interval(0);
        for _ in 0..100 {
            if count.load(Ordering::SeqCst) >= 1 {
                break;
            }
            el.process_events(false);
        }
        assert!(
            count.load(Ordering::SeqCst) >= 1,
            "QTimer::setInterval on an active timer must restart it with the new interval"
        );
        timer.stop();
    }

    /// Pumps `el` for `ms` milliseconds and returns how many times `count` advanced.
    fn pump_for(
        el: &mut crate::event_loop::EventLoop,
        ms: u64,
        count: &std::sync::atomic::AtomicU32,
    ) -> u32 {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(ms);
        while std::time::Instant::now() < deadline {
            el.process_events(false);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        count.load(std::sync::atomic::Ordering::SeqCst)
    }

    #[test]
    fn set_single_shot_true_on_active_timer_fires_once() {
        use std::sync::atomic::{AtomicU32, Ordering};

        let mut el = crate::event_loop::EventLoop::new();
        let count = Arc::new(AtomicU32::new(0));
        let c = Arc::clone(&count);
        let timer = Timer::new();
        timer.set_interval(15);
        timer.timeout.connect(move |_| {
            c.fetch_add(1, Ordering::SeqCst);
        });
        timer.start();
        timer.set_single_shot(true);
        // QTimer::timerEvent reads `single` at fire time (qtimer.cpp:330-331).
        assert_eq!(pump_for(&mut el, 300, &count), 1);
        assert!(!timer.is_active());
    }

    #[test]
    fn set_single_shot_false_on_active_timer_keeps_repeating() {
        use std::sync::atomic::{AtomicU32, Ordering};

        let mut el = crate::event_loop::EventLoop::new();
        let count = Arc::new(AtomicU32::new(0));
        let c = Arc::clone(&count);
        let timer = Timer::new();
        timer.set_interval(15);
        timer.set_single_shot(true);
        timer.timeout.connect(move |_| {
            c.fetch_add(1, Ordering::SeqCst);
        });
        timer.start();
        timer.set_single_shot(false);
        let fired = pump_for(&mut el, 300, &count);
        assert!(
            fired >= 2,
            "a timer made repeating while active must keep firing, got {fired}"
        );
        assert!(timer.is_active());
        timer.stop();
    }

    #[test]
    fn test_timer_remaining_time() {
        use crate::event_loop::EventLoop;

        let timer = Timer::new();
        timer.set_interval(1000);

        assert_eq!(timer.remaining_time(), -1);

        let _event_loop = EventLoop::new();
        timer.start();
        assert!(timer.is_active());

        let rem = timer.remaining_time();
        assert!(
            rem > 0 && rem <= 1000,
            "remaining_time should be positive within interval, got {}",
            rem
        );

        timer.stop();
        assert_eq!(timer.remaining_time(), -1);
    }

    #[test]
    fn test_qtimer_single_shot_static_api_zero_delay() {
        use crate::event_loop::EventLoop;
        use std::sync::atomic::{AtomicBool, Ordering};

        let event_loop = EventLoop::new();
        let executed = Arc::new(AtomicBool::new(false));
        let ex_clone = Arc::clone(&executed);

        Timer::single_shot(0, move || {
            ex_clone.store(true, Ordering::SeqCst);
        });

        assert!(!executed.load(Ordering::SeqCst));
        let processed = event_loop.process_events(false);
        assert!(processed);
        assert!(executed.load(Ordering::SeqCst));
    }

    // ---- Lifecycle of a `Timer` whose owner is dropped by one of its own slots.
    //
    // `Signal::emit` contains a panicking slot, so a slot never asserts: it records what it saw
    // and the test asserts afterwards.

    use std::sync::atomic::AtomicUsize;

    thread_local! {
        static HOLD: RefCell<Option<Timer>> = const { RefCell::new(None) };
    }

    fn core_drops() -> usize {
        CORE_DROPS.with(|drops| drops.get())
    }

    fn hold(timer: Timer) {
        HOLD.with(|h| *h.borrow_mut() = Some(timer));
    }

    fn take_held() -> Option<Timer> {
        HOLD.with(|h| h.borrow_mut().take())
    }

    /// A thread timer context with no event loop: `start` arms the returned registry.
    fn test_context() -> Arc<Mutex<TimerRegistry>> {
        let registry = Arc::new(Mutex::new(TimerRegistry::new()));
        #[cfg(windows)]
        register_thread_timer_context(Arc::clone(&registry), std::ptr::null_mut());
        #[cfg(not(windows))]
        register_thread_timer_context(Arc::clone(&registry));
        registry
    }

    fn deliver(timer_object: ObjectId, timer_id: TimerId) -> crate::event_loop::TimerDelivery {
        crate::event_loop::deliver_timer_event(timer_object, timer_id.0 as u64)
    }

    #[test]
    fn owner_dropped_by_its_own_slot_is_freed_after_the_callback_returns() {
        use crate::event_loop::TimerDelivery;
        use crate::object::qobject::registered_on_current_thread;

        let registry = test_context();
        let timer = Timer::new();
        let id = timer.object_id();
        timer.set_interval(1000);
        let timer_id = timer.start();
        assert!(timer_id.is_valid());
        assert!(registered_on_current_thread(id));
        assert_eq!(registry.lock().unwrap().len(), 1);

        let retained = timer.timeout.clone();
        let in_slot_drops = Arc::new(AtomicUsize::new(usize::MAX));
        let in_slot_registered = Arc::new(AtomicUsize::new(usize::MAX));
        let in_slot_timers = Arc::new(AtomicUsize::new(usize::MAX));
        let second = Arc::new(AtomicUsize::new(0));
        {
            let (d, r, t, reg) = (
                Arc::clone(&in_slot_drops),
                Arc::clone(&in_slot_registered),
                Arc::clone(&in_slot_timers),
                Arc::clone(&registry),
            );
            timer.timeout.connect(move |_| {
                drop(take_held());
                d.store(core_drops(), Ordering::SeqCst);
                r.store(registered_on_current_thread(id) as usize, Ordering::SeqCst);
                t.store(reg.lock().unwrap().len(), Ordering::SeqCst);
            });
            let s = Arc::clone(&second);
            timer.timeout.connect(move |_| {
                s.fetch_add(1, Ordering::SeqCst);
            });
        }
        hold(timer);

        assert!(matches!(deliver(id, timer_id), TimerDelivery::Delivered));

        assert_eq!(in_slot_drops.load(Ordering::SeqCst), 0, "object destroyed inside its callback");
        assert_eq!(in_slot_registered.load(Ordering::SeqCst), 0, "still registered after the drop");
        assert_eq!(in_slot_timers.load(Ordering::SeqCst), 0, "timer still scheduled after the drop");
        assert_eq!(second.load(Ordering::SeqCst), 0, "a slot ran for a destroyed owner");
        assert_eq!(core_drops(), 1, "object not freed once the callback returned");
        assert!(!registered_on_current_thread(id));
        assert!(matches!(deliver(id, timer_id), TimerDelivery::NotDeliverable));
        retained.emit(&());
        assert_eq!(second.load(Ordering::SeqCst), 0, "a retained signal still reaches slots");
        assert_eq!(registry.lock().unwrap().len(), 0);
        unregister_thread_timer_context();
    }

    #[test]
    fn slot_may_restart_stop_and_reconfigure_its_own_timer() {
        use crate::event_loop::TimerDelivery;

        let registry = test_context();
        let timer = Timer::new();
        let id = timer.object_id();
        timer.set_interval(1000);
        let first = timer.start();
        let calls = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&calls);
        timer.timeout.connect(move |_| {
            HOLD.with(|h| {
                let h = h.borrow();
                let t = h.as_ref().expect("held");
                t.stop();
                t.set_interval(500);
                t.set_single_shot(false);
                t.start();
            });
            c.fetch_add(1, Ordering::SeqCst);
        });
        hold(timer);

        assert!(matches!(deliver(id, first), TimerDelivery::Delivered));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let timer = take_held().expect("held");
        assert!(timer.is_active());
        assert_ne!(timer.timer_id(), first, "restart must schedule a new timer");
        assert_eq!(timer.interval(), 500);
        assert_eq!(registry.lock().unwrap().len(), 1);
        assert_eq!(core_drops(), 0);
        drop(timer);
        assert_eq!(core_drops(), 1);
        assert_eq!(registry.lock().unwrap().len(), 0);
        unregister_thread_timer_context();
    }

    #[test]
    fn single_shot_slot_may_restart_and_then_drop_its_owner() {
        use crate::event_loop::TimerDelivery;

        let registry = test_context();
        let timer = Timer::new();
        let id = timer.object_id();
        timer.set_interval(1000);
        timer.set_single_shot(true);
        let first = timer.start();
        let calls = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&calls);
        timer.timeout.connect(move |_| {
            if c.fetch_add(1, Ordering::SeqCst) == 0 {
                HOLD.with(|h| {
                    h.borrow().as_ref().expect("held").start();
                });
            } else {
                drop(take_held());
            }
        });
        hold(timer);

        assert!(matches!(deliver(id, first), TimerDelivery::Delivered));
        let second_id = HOLD.with(|h| h.borrow().as_ref().expect("held").timer_id());
        assert!(second_id.is_valid() && second_id != first);
        assert_eq!(registry.lock().unwrap().len(), 1, "single-shot stop ran before the slot");
        assert_eq!(core_drops(), 0);

        assert!(matches!(deliver(id, second_id), TimerDelivery::Delivered));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(core_drops(), 1);
        assert_eq!(registry.lock().unwrap().len(), 0);
        unregister_thread_timer_context();
    }

    #[test]
    fn owner_dropped_outside_a_callback_is_freed_at_once() {
        use crate::object::qobject::registered_on_current_thread;

        let registry = test_context();
        let never_started = Timer::new();
        let id = never_started.object_id();
        assert!(registered_on_current_thread(id), "registered by `new`");
        drop(never_started);
        assert_eq!(core_drops(), 1);
        assert!(!registered_on_current_thread(id));

        let started = Timer::new();
        started.set_interval(1000);
        assert!(started.start().is_valid());
        assert_eq!(registry.lock().unwrap().len(), 1);
        drop(started);
        assert_eq!(core_drops(), 2);
        assert_eq!(registry.lock().unwrap().len(), 0);
        unregister_thread_timer_context();
    }

    #[test]
    fn unregister_removes_the_timer_when_stop_was_blocked_by_a_context_borrow() {
        let registry = test_context();
        let timer = Timer::new();
        timer.set_interval(1000);
        assert!(timer.start().is_valid());
        assert_eq!(registry.lock().unwrap().len(), 1);

        // `stop` cannot reach the context while it is mutably borrowed: it skips the disarm and
        // still marks the timer inactive.
        THREAD_TIMER_CONTEXT.with(|ctx| {
            let _held = ctx.borrow_mut();
            timer.stop();
        });
        assert!(!timer.is_active());
        assert_eq!(registry.lock().unwrap().len(), 1, "the blocked stop must not reach the registry");

        drop(timer);
        assert_eq!(core_drops(), 1);
        assert_eq!(
            registry.lock().unwrap().len(),
            0,
            "unregistering the object must remove the entry `stop` could not"
        );
        unregister_thread_timer_context();
    }

    #[test]
    fn panicking_slot_after_the_owner_was_dropped_still_frees_the_object_once() {
        use crate::event_loop::TimerDelivery;

        let _registry = test_context();
        let timer = Timer::new();
        let id = timer.object_id();
        timer.set_interval(1000);
        let timer_id = timer.start();
        timer.timeout.connect(|_| {
            drop(take_held());
            panic!("slot panics after dropping its owner");
        });
        hold(timer);

        assert!(matches!(deliver(id, timer_id), TimerDelivery::Delivered));
        assert_eq!(core_drops(), 1);
        unregister_thread_timer_context();
    }

    #[test]
    fn win32_timer_elapse_caps_at_user_timer_maximum_and_begin_delivery_waits_for_deadline() {
        assert_eq!(WIN32_TIMER_MAX_MS, 0x7FFF_FFFF);
        assert_eq!(win32_timer_elapse(0), 0);
        assert_eq!(win32_timer_elapse(0x7FFF_FFFE), 0x7FFF_FFFE);
        assert_eq!(win32_timer_elapse(0x7FFF_FFFF), 0x7FFF_FFFF);
        assert_eq!(win32_timer_elapse(0x8000_0000), 0x7FFF_FFFF);
        assert_eq!(win32_timer_elapse(u32::MAX as u64), 0x7FFF_FFFF);
        assert_eq!(win32_timer_elapse(u64::MAX), 0x7FFF_FFFF);

        let registry = Mutex::new(TimerRegistry::new());
        let id = registry
            .lock()
            .unwrap()
            .register(ObjectId(99), u64::MAX, TimerType::Precise, true);
        let deadline = registry.lock().unwrap().get(id).unwrap().next_fire_ms;
        assert_eq!(deadline, u64::MAX);

        assert!(
            begin_timer_delivery(&registry, id, WIN32_TIMER_MAX_MS).is_none(),
            "unexpired timer must not begin delivery at USER_TIMER_MAXIMUM"
        );
        assert!(
            begin_timer_delivery(&registry, id, u64::MAX - 1).is_none(),
            "unexpired timer must not begin delivery before next_fire_ms"
        );
        let entry = registry.lock().unwrap().get(id).unwrap().clone();
        assert!(!entry.in_timer_event);
        assert_eq!(entry.next_fire_ms, u64::MAX);

        assert_eq!(
            begin_timer_delivery(&registry, id, u64::MAX),
            Some((ObjectId(99), true))
        );
    }
}

#[cfg(test)]
mod receiver_index_tests {
    use super::*;

    #[test]
    fn the_receiver_index_follows_register_replace_and_unregister() {
        let (a, b) = (ObjectId::next(), ObjectId::next());
        let mut reg = TimerRegistry::new();
        let a1 = reg.register(a, 10, TimerType::Coarse, false);
        let a2 = reg.register(a, 20, TimerType::Coarse, false);
        let b1 = reg.register(b, 30, TimerType::Coarse, false);
        let mut of_a = reg.ids_for_receiver(a).to_vec();
        of_a.sort_by_key(|id| id.0);
        assert_eq!(of_a, vec![a1, a2]);
        assert_eq!(reg.ids_for_receiver(b), &[b1]);

        // A live id is never replaced: the registration is refused and the timer stays with `a`.
        assert!(!reg.register_with_id(a1, b, 10, TimerType::Coarse, false));
        let mut of_a = reg.ids_for_receiver(a).to_vec();
        of_a.sort_by_key(|id| id.0);
        assert_eq!(of_a, vec![a1, a2]);
        assert_eq!(reg.ids_for_receiver(b), &[b1]);
        assert_eq!(reg.len(), 3);
        assert!(reg.unregister(a1).is_some());
        // A freed id registers again under a new receiver, as a moved timer does.
        assert!(reg.register_with_id(a1, b, 10, TimerType::Coarse, false));
        assert_eq!(reg.ids_for_receiver(a), &[a2]);
        let mut of_b = reg.ids_for_receiver(b).to_vec();
        of_b.sort_by_key(|id| id.0);
        assert_eq!(of_b, vec![a1, b1]);

        assert!(reg.unregister(a2).is_some());
        assert!(reg.ids_for_receiver(a).is_empty());
        assert!(reg.unregister(a2).is_none());
        assert_eq!(reg.len(), 2);
    }

    /// A moved timer whose id is already live on the target thread is re-registered under a
    /// fresh id, not dropped.
    #[test]
    fn adopting_a_timer_with_a_live_id_gives_it_a_fresh_id() {
        let registry = Arc::new(Mutex::new(TimerRegistry::new()));
        #[cfg(windows)]
        register_thread_timer_context(Arc::clone(&registry), std::ptr::null_mut());
        #[cfg(not(windows))]
        register_thread_timer_context(Arc::clone(&registry));
        let held = Timer::new();
        let mover = Timer::new();
        let moved_id = mover.start();
        // The timer leaves its registry, as `detach_timers_for_objects` does on a thread move.
        let entry = registry.lock().unwrap().unregister(moved_id).unwrap();
        // The target thread already has a live timer of another object under that id.
        assert!(registry.lock().unwrap().register_with_id(
            moved_id,
            held.object_id(),
            50,
            TimerType::Coarse,
            false
        ));
        adopt_entries(vec![MovedTimer { entry, aliases: Vec::new() }]);
        let fresh = {
            let reg = registry.lock().unwrap();
            assert_eq!(reg.len(), 2, "the moved timer was dropped");
            assert_eq!(reg.get(moved_id).unwrap().receiver, held.object_id());
            let ids = reg.ids_for_receiver(mover.object_id());
            assert_eq!(ids.len(), 1);
            assert_ne!(ids[0], moved_id);
            ids[0]
        };
        // The owner follows the re-id: it reports the new id, and stop() kills only its own timer.
        assert_eq!(mover.timer_id(), fresh);
        mover.stop();
        let reg = registry.lock().unwrap();
        assert!(reg.get(fresh).is_none(), "the moved timer was not stopped");
        assert_eq!(
            reg.get(moved_id).map(|e| e.receiver),
            Some(held.object_id()),
            "an unrelated timer was killed"
        );
        drop(reg);
        drop(held);
        drop(mover);
        unregister_thread_timer_context();
    }

    /// An object that starts its timers through `QObject::start_timer`.
    struct TimerOwner {
        data: ObjectData,
    }

    impl QObject for TimerOwner {
        fn object_data(&self) -> &ObjectData {
            &self.data
        }

        fn object_data_mut(&mut self) -> crate::object::ObjectDataMut<'_> {
            crate::object::ObjectDataMut::new(&mut self.data)
        }
    }

    fn owner_timers(owner: &OwnedObject<TimerOwner>) -> Vec<TimerId> {
        owner.with(|o| o.data.timers.clone()).unwrap()
    }

    /// Moves the object timer `started` of `receiver` away and back while `held`'s timer occupies
    /// its id, as after the id counter wrapped. Returns the id it was given on adoption.
    fn reidentify_object_timer(
        registry: &Mutex<TimerRegistry>,
        receiver: ObjectId,
        started: TimerId,
        held: &Timer,
    ) -> TimerId {
        let moved = detach_registered_timers(&[receiver]);
        assert_eq!(moved.len(), 1);
        assert!(registry.lock().unwrap().register_with_id(
            started,
            held.object_id(),
            50,
            TimerType::Coarse,
            false
        ));
        adopt_entries(moved);
        let reg = registry.lock().unwrap();
        let ids = reg.ids_for_receiver(receiver);
        assert_eq!(ids.len(), 1);
        assert_ne!(ids[0], started);
        ids[0]
    }

    /// A re-identified `QObject::start_timer` timer leaves no stale id in its object's list, the
    /// id its owner holds still kills it, and no redirect outlives the timer.
    #[test]
    fn a_reidentified_object_timer_leaves_no_stale_id_or_redirect() {
        let registry = Arc::new(Mutex::new(TimerRegistry::new()));
        #[cfg(windows)]
        register_thread_timer_context(Arc::clone(&registry), std::ptr::null_mut());
        #[cfg(not(windows))]
        register_thread_timer_context(Arc::clone(&registry));
        let held = Timer::new();
        let data = ObjectData::new(ObjectId::next());
        let receiver = data.id();
        // SAFETY: the owner is used on this thread only and no callback of it runs.
        let mut owner = unsafe { crate::object::register_owned_qobject(TimerOwner { data }) };

        // Killed through the id `start_timer` returned, which the re-id made stale.
        let started = owner.with_mut(|o| o.start_timer(50, TimerType::Coarse)).unwrap();
        assert!(started.is_valid());
        let fresh = reidentify_object_timer(&registry, receiver, started, &held);
        assert_eq!(owner_timers(&owner), vec![fresh], "stale id left in the object's list");
        assert_eq!(reidentified_as(receiver, started), Some(fresh));
        assert!(owner.with_mut(|o| o.kill_timer(started)).unwrap());
        assert!(owner_timers(&owner).is_empty());
        assert_eq!(reidentified_as(receiver, started), None, "redirect outlived the timer");
        {
            let reg = registry.lock().unwrap();
            assert!(reg.get(fresh).is_none());
            assert_eq!(reg.len(), 1, "an unrelated timer was killed");
        }
        assert!(!owner.with_mut(|o| o.kill_timer(started)).unwrap(), "killed twice");

        // Killed through the new id: the redirect goes with the timer.
        let started = owner.with_mut(|o| o.start_timer(50, TimerType::Coarse)).unwrap();
        let fresh = reidentify_object_timer(&registry, receiver, started, &held);
        assert!(owner.with_mut(|o| o.kill_timer(fresh)).unwrap());
        assert_eq!(reidentified_as(receiver, started), None, "redirect outlived the timer");
        assert!(owner_timers(&owner).is_empty());

        // The redirect follows the timer through a second move and is dropped with the timer
        // when the object's timers are removed (as on its destruction).
        let started = owner.with_mut(|o| o.start_timer(50, TimerType::Coarse)).unwrap();
        let fresh = reidentify_object_timer(&registry, receiver, started, &held);
        let moved = detach_registered_timers(&[receiver]);
        assert_eq!(reidentified_as(receiver, started), None, "redirect stayed on the old thread");
        adopt_entries(moved);
        assert_eq!(owner_timers(&owner), vec![fresh]);
        assert_eq!(reidentified_as(receiver, started), Some(fresh));
        assert_eq!(stop_timers_for_object(receiver), 1);
        assert_eq!(reidentified_as(receiver, started), None, "redirect outlived the timer");
        // The object's list is dropped with the object; here it is emptied to go on.
        assert!(!owner.with_mut(|o| o.kill_timer(fresh)).unwrap(), "already unregistered");

        // A thread without a timer context schedules nothing, so the object keeps no id.
        let started = owner.with_mut(|o| o.start_timer(50, TimerType::Coarse)).unwrap();
        let moved = detach_registered_timers(&[receiver]);
        unregister_thread_timer_context();
        adopt_entries(moved);
        assert!(owner_timers(&owner).is_empty(), "{started} left in the list");
        assert!(registry.lock().unwrap().ids_for_receiver(receiver).is_empty());
        drop(held);
        drop(owner);
    }

    #[test]
    fn context_shots_are_found_by_context_and_forgotten_once_fired() {
        let mut shots = ContextShots::default();
        let (c1, c2) = (ObjectId::next(), ObjectId::next());
        let (r1, r2, r3) = (ObjectId::next(), ObjectId::next(), ObjectId::next());
        shots.insert(r1, c1);
        shots.insert(r2, c1);
        shots.insert(r3, c2);
        shots.remove(r2);
        assert_eq!(shots.take_for(&[c1]), vec![(r1, c1)]);
        assert!(shots.take_for(&[c1]).is_empty());
        assert_eq!(shots.take_for(&[c1, c2]), vec![(r3, c2)]);
        assert!(shots.context_of.is_empty() && shots.receivers_of.is_empty());
    }
}

#[cfg(test)]
mod early_tick_tests {
    use super::{classify_early_tick, EarlyTick};

    #[test]
    fn a_tick_near_its_expiry_is_held_and_one_a_grid_point_early_is_dropped() {
        // 20 ms timer, expiry at 100.
        assert_eq!(classify_early_tick(99, 100, 20), EarlyTick::Defer);
        assert_eq!(classify_early_tick(90, 100, 20), EarlyTick::Defer);
        assert_eq!(classify_early_tick(89, 100, 20), EarlyTick::Stale);
        // The tick of a grid point a late callback already consumed.
        assert_eq!(classify_early_tick(81, 100, 20), EarlyTick::Stale);
        // A 1 ms timer still holds a tick 1 ms early.
        assert_eq!(classify_early_tick(9, 10, 1), EarlyTick::Defer);
    }
}

#[cfg(all(test, windows))]
mod single_shot_source_tests {
    use super::{precise, with_thread_timer_context, Timer};
    use crate::event_loop::EventLoop;

    /// A short `Timer::single_shot` is driven by the same native source as every other `Precise`
    /// timer, and the source is gone once the loop is.
    #[test]
    fn a_short_single_shot_is_a_multimedia_timer() {
        let el = EventLoop::new();
        Timer::single_shot(10, || {});
        let ids: Vec<_> = with_thread_timer_context(|ctx| {
            super::lock_unpoisoned(&ctx.registry)
                .entries
                .keys()
                .copied()
                .collect()
        })
        .unwrap();
        assert_eq!(ids.len(), 1);
        assert!(precise::native_handle(ids[0]).is_some());
        drop(el);
        assert!(
            precise::native_handle(ids[0]).is_none(),
            "the native timer is released with the loop"
        );
    }
}

#[cfg(all(test, windows))]
mod precise_delivery_tests {
    use super::{begin_timer_delivery, lock_unpoisoned, start_object_timer, with_thread_timer_context};
    use super::{ObjectId, TimerType};
    use crate::event_loop::EventLoop;

    /// With the clock fixed by the test: no delivery before the expiry, and after an overrun one
    /// delivery whose next deadline is the first grid point after now (missed points are skipped).
    #[test]
    fn a_precise_timer_is_not_delivered_early_and_skips_missed_grid_points() {
        const INTERVAL: u64 = 50;
        let _el = EventLoop::new();
        let id = start_object_timer(ObjectId::next(), INTERVAL, TimerType::Precise);
        assert!(id.is_valid());
        with_thread_timer_context(|ctx| {
            let expiry = lock_unpoisoned(&ctx.registry).get(id).unwrap().next_fire_ms;

            assert!(begin_timer_delivery(&ctx.registry, id, expiry - 1).is_none());
            assert_eq!(
                lock_unpoisoned(&ctx.registry).get(id).unwrap().next_fire_ms,
                expiry,
                "an early tick leaves the deadline alone"
            );

            let late = expiry + 3 * INTERVAL + 7;
            assert!(begin_timer_delivery(&ctx.registry, id, late).is_some());
            assert_eq!(
                lock_unpoisoned(&ctx.registry).get(id).unwrap().next_fire_ms,
                expiry + 4 * INTERVAL,
                "the three missed grid points are skipped"
            );
        })
        .unwrap();
    }
}
