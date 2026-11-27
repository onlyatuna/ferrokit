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
/// object's timers keep their ids on the target thread (Qt `reRegisterTimers`).
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
    REIDENTIFIED
        .try_with(|map| map.borrow_mut().remove(&(receiver, old)))
        .ok()
        .flatten()
}

/// The id `receiver`'s timer `old` was given on adoption, leaving the redirect in place: the
/// owner of an `Object::start_timer` id may still hold `old` until it kills the timer.
pub(crate) fn reidentified_as(receiver: ObjectId, old: TimerId) -> Option<TimerId> {
    REIDENTIFIED
        .try_with(|map| map.borrow().get(&(receiver, old)).copied())
        .ok()
        .flatten()
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
    /// `registerTimer` with an existing id); the phase restarts from now.
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
    static PENDING_SINGLE_SHOTS: RefCell<Vec<(ObjectId, u64)>> = const { RefCell::new(Vec::new()) };
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
        .try_with(|ctx| match ctx.try_borrow() {
            Ok(borrow) => borrow.is_some(),
            _ => false,
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
        // SAFETY: `KillTimer` is given a window handle and integers only, no pointers; a `hwnd`
        // or id that does not name a live timer (including a null or already destroyed
        // window) makes it return 0, which is ignored.
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
/// Modeled after Qt's object internals timer cleanup.
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
            if let Some(entry) = _entry
                && entry.interval_ms > 0
                && !ctx.internal_hwnd.is_null()
            {
                kill_native(ctx.internal_hwnd, id);
            }
        }
        count
    })
    .unwrap_or(0)
}

/// Removes the timers whose receiver is in `receivers` and disarms their delivery sources, returning
/// the entries so the target thread can re-register them under the same ids (Qt
/// object internals timer handling: `unregisterTimers` on the source,
/// ).
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
/// their delivery sources (Qt `reRegisterTimers`). The phase restarts, as with
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
/// ). Zero interval posts `ZeroTimer`; otherwise
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
    // SAFETY: `SetTimer` is given a window handle and integers only (a `None` callback, so
    // it posts `WM_TIMER` to that window; no pointers are passed), and its failure is
    // reported by a 0 return that is deliberately ignored: the timer then simply never fires.
    // Callers pass the dispatcher window of the calling thread (or null, which `SetTimer`
    // accepts, creating a thread timer), so no handle can dangle into our memory.
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
/// ). Uses the REGISTERED interval, not the mutable `Timer` field.
/// With `receiver`, a timer registered for another object is left alone.
fn disarm_in_context(ctx: &ThreadTimerContext, receiver: Option<ObjectId>, id: TimerId) -> bool {
    let mut reg = lock_unpoisoned(&ctx.registry);
    if let Some(receiver) = receiver
        && reg.get(id).is_some_and(|entry| entry.receiver != receiver)
    {
        return false;
    }
    // Only the Windows code below reads the entry (its registered interval).
    #[cfg_attr(not(windows), allow(unused_variables))]
    let Some(entry) = reg.unregister(id) else {
        return false;
    };
    #[cfg(windows)]
    {
        if entry.interval_ms > 0 && !ctx.internal_hwnd.is_null() {
            kill_native(ctx.internal_hwnd, id);
        }
    }
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
/// context.
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

#[cfg(test)]
mod early_tick_tests {
    use super::{EarlyTick, classify_early_tick};

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
    use super::{Timer, precise, with_thread_timer_context};
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
    use super::{ObjectId, TimerType};
    use super::{
        begin_timer_delivery, lock_unpoisoned, start_object_timer, with_thread_timer_context,
    };
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

mod qtimer;
#[cfg(test)]
mod receiver_index_tests;
#[cfg(test)]
mod tests;

#[cfg(test)]
#[allow(unused_imports)]
use qtimer::CORE_DROPS;
pub use qtimer::*;
