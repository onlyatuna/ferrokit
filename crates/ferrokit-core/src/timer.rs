// SPDX-License-Identifier: LGPL-3.0-only
//! Timer module modeled after Qt timer mechanism (QTimer, QTimerInfo).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::marker::PhantomPinned;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Instant;

#[cfg(windows)]
use windows_sys::Win32::Foundation::HWND;
#[cfg(not(windows))]
pub type HWND = *mut std::ffi::c_void;

use crate::object::{ObjectData, ObjectId, QObject};
use crate::signal::Signal;

static START_TIME: LazyLock<Instant> = LazyLock::new(Instant::now);

/// Process-wide timer id source. Ids must not repeat across thread registries, because a moved
/// object's timers keep their ids on the target thread (Qt `reRegisterTimers`, qobject.cpp:1496-1500).
static NEXT_TIMER_ID: AtomicU32 = AtomicU32::new(1);

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
}

impl TimerRegistry {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
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
        let id = allocate_timer_id();
        self.register_with_id(id, receiver, interval_ms, timer_type, single_shot);
        id
    }

    /// Registers `id` for `receiver`. A moved timer keeps its id on the target thread (Qt
    /// `registerTimer` with an existing id, qobject.cpp:1496-1500); the phase restarts from now.
    fn register_with_id(
        &mut self,
        id: TimerId,
        receiver: ObjectId,
        interval_ms: u64,
        timer_type: TimerType,
        single_shot: bool,
    ) {
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
    }

    pub fn unregister(&mut self, id: TimerId) -> Option<TimerEntry> {
        self.entries.remove(&id)
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

    pub fn expired_timers(&self, now_ms: u64) -> Vec<TimerId> {
        self.entries
            .values()
            .filter(|e| !e.in_timer_event && e.interval_ms > 0 && now_ms >= e.next_fire_ms)
            .map(|e| e.id)
            .collect()
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
}

pub fn register_thread_timer_context(
    registry: Arc<Mutex<TimerRegistry>>,
    #[cfg(windows)] internal_hwnd: HWND,
) {
    THREAD_TIMER_CONTEXT.with(|ctx| {
        *ctx.borrow_mut() = Some(ThreadTimerContext {
            registry,
            #[cfg(windows)]
            internal_hwnd,
        });
    });
    // Arm single-shot timers requested before this thread had a loop; they must not be dropped.
    let pending = PENDING_SINGLE_SHOTS.with(|p| std::mem::take(&mut *p.borrow_mut()));
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

/// Arms one single-shot timer for `receiver` on this thread's registry (Windows: also `SetTimer`
/// on the internal window, as `Timer::single_shot` always did).
fn arm_single_shot(ctx: &ThreadTimerContext, receiver: ObjectId, interval_ms: u64) {
    let _timer_id =
        ctx.registry
            .lock()
            .unwrap()
            .register(receiver, interval_ms, TimerType::Coarse, true);
    #[cfg(windows)]
    if !ctx.internal_hwnd.is_null() {
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::SetTimer(
                ctx.internal_hwnd,
                _timer_id.0 as usize,
                interval_ms.min(u32::MAX as u64) as u32,
                None,
            );
        }
    }
}

/// Stops and unregisters all active timers registered to the specified receiver object.
/// Modeled after Qt's QObjectPrivate::moveToThread_helper timer cleanup.
pub fn stop_timers_for_object(receiver: ObjectId) -> usize {
    with_thread_timer_context(|ctx| {
        let mut reg = ctx.registry.lock().unwrap();
        let to_remove: Vec<TimerId> = reg
            .entries
            .iter()
            .filter(|(_, entry)| entry.receiver == receiver)
            .map(|(&id, _)| id)
            .collect();
        let count = to_remove.len();
        for id in to_remove {
            reg.unregister(id);
        }
        count
    })
    .unwrap_or(0)
}

/// Removes the timers whose receiver is in `receivers` and disarms their delivery sources, returning
/// the entries so the target thread can re-register them under the same ids (Qt
/// `QObjectPrivate::moveToThread_helper` timer handling: `unregisterTimers` on the source,
/// qobject.cpp:1490-1502).
pub(crate) fn detach_timers_for_objects(receivers: &[ObjectId]) -> Vec<TimerEntry> {
    with_thread_timer_context(|ctx| {
        let mut reg = ctx.registry.lock().unwrap();
        let ids: Vec<TimerId> = reg
            .entries
            .iter()
            .filter(|(_, entry)| receivers.contains(&entry.receiver))
            .map(|(&id, _)| id)
            .collect();
        let mut detached = Vec::with_capacity(ids.len());
        for id in ids {
            let Some(entry) = reg.unregister(id) else {
                continue;
            };
            #[cfg(windows)]
            {
                if entry.interval_ms > 0 && !ctx.internal_hwnd.is_null() {
                    unsafe {
                        windows_sys::Win32::UI::WindowsAndMessaging::KillTimer(
                            ctx.internal_hwnd,
                            id.0 as usize,
                        );
                    }
                }
            }
            detached.push(entry);
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
pub(crate) fn adopt_timers(entries: Vec<TimerEntry>) {
    let current = crate::object::ThreadId::current();
    let mut local = Vec::with_capacity(entries.len());
    for entry in entries {
        match crate::object::qobject::registration_thread_of(entry.receiver) {
            Some(owner) if owner == current => local.push(entry),
            Some(owner) => crate::event_loop::post_event_to_thread(
                owner,
                crate::object::ObjectId(0),
                crate::event::Event::new(crate::event::EventKind::MetaCall(Box::new(move |_| {
                    adopt_timers(vec![entry])
                }))),
            ),
            None => {}
        }
    }
    if local.is_empty() {
        return;
    }
    let armed = with_thread_timer_context(|ctx| {
        let mut reg = ctx.registry.lock().unwrap();
        for entry in local {
            reg.register_with_id(
                entry.id,
                entry.receiver,
                entry.interval_ms,
                entry.timer_type,
                entry.single_shot,
            );
            arm_source(ctx, entry.id, entry.receiver, entry.interval_ms);
        }
    });
    if armed.is_none() {
        eprintln!("ferrokit: cannot re-register moved timers: this thread has no timer context");
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
    let mut reg = ctx.registry.lock().unwrap();
    let timer_id = reg.register(receiver, interval_ms, timer_type, single_shot);
    arm_source(ctx, timer_id, receiver, interval_ms);
    timer_id
}

/// Arms the delivery source of a registered timer: a zero interval posts `ZeroTimer`, otherwise
/// `SetTimer` on Windows.
fn arm_source(ctx: &ThreadTimerContext, timer_id: TimerId, receiver: ObjectId, interval_ms: u64) {
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
                let interval = interval_ms.min(u32::MAX as u64) as u32;
                unsafe {
                    windows_sys::Win32::UI::WindowsAndMessaging::SetTimer(
                        ctx.internal_hwnd,
                        timer_id.0 as usize,
                        interval,
                        None,
                    );
                }
            }
        }
    }
}

/// Removes a timer and disarms its delivery source (`QObject::killTimer` → `unregisterTimer`,
/// qeventdispatcher_win.cpp:383-392). Uses the REGISTERED interval, not the mutable `Timer` field.
fn disarm_in_context(ctx: &ThreadTimerContext, id: TimerId) -> bool {
    let mut reg = ctx.registry.lock().unwrap();
    let Some(entry) = reg.unregister(id) else {
        return false;
    };
    #[cfg(windows)]
    {
        if entry.interval_ms > 0 && !ctx.internal_hwnd.is_null() {
            unsafe {
                windows_sys::Win32::UI::WindowsAndMessaging::KillTimer(
                    ctx.internal_hwnd,
                    id.0 as usize,
                );
            }
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
) {
    SINGLE_SHOT_CALLBACKS.with(|callbacks| {
        callbacks.borrow_mut().insert(receiver, Box::new(callback));
    });
}

pub(crate) fn dispatch_single_shot_callback(receiver: ObjectId) -> bool {
    let callback = SINGLE_SHOT_CALLBACKS.with(|callbacks| callbacks.borrow_mut().remove(&receiver));
    if let Some(callback) = callback {
        callback();
        true
    } else {
        false
    }
}

/// Kills an object timer by TimerId in the current thread's timer registry.
pub fn kill_object_timer(id: TimerId) -> bool {
    with_thread_timer_context(|ctx| disarm_in_context(ctx, id)).unwrap_or(false)
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

/// High-level timer object matching `QTimer`.
///
/// A started timer is registered at its address, so a `Timer` is always heap-pinned: see
/// [`Timer::new`]. Qt: `QTimer` is a heap `QObject` whose address is stable for its lifetime.
pub struct Timer {
    data: ObjectData,
    id: Cell<TimerId>,
    interval_ms: Cell<u64>,
    single_shot: Cell<bool>,
    timer_type: Cell<TimerType>,
    pub timeout: Signal<()>,
    _pinned: PhantomPinned,
}

impl Timer {
    /// Heap-pins a new timer (`new QTimer`).
    ///
    /// The timer cannot be moved out of its box, because it may be registered at its address:
    ///
    /// ```compile_fail
    /// let t = ferrokit_core::timer::Timer::new();
    /// let _moved = std::pin::Pin::into_inner(t); // `Timer: !Unpin`, so this does not compile
    /// ```
    pub fn new() -> Pin<Box<Self>> {
        let data = ObjectData::new(ObjectId::next());
        let timeout = Signal::with_owner(&data);
        Box::pin(Self {
            data,
            id: Cell::new(TimerId::INVALID),
            interval_ms: Cell::new(0),
            single_shot: Cell::new(false),
            timer_type: Cell::new(TimerType::Coarse),
            timeout,
            _pinned: PhantomPinned,
        })
    }

    /// Starts the timer with its configured interval (`QTimer::start`).
    ///
    /// Safe: a timer is only reachable as `Pin<Box<Timer>>`, so its registered address cannot
    /// change, and `Drop` unregisters it before the memory is freed.
    pub fn start(mut self: Pin<&mut Self>) -> TimerId {
        if self.is_active() {
            self.stop();
        }

        // SAFETY: `self` is pinned and never leaves its heap box except through `Drop`, which
        // unregisters the address first. The registry therefore never holds a stale pointer.
        unsafe { crate::object::register_pinned_qobject(self.as_mut()) };

        self.rearm()
    }

    /// Registers this timer's current configuration with the thread timer context and records
    /// the new id. Callers must have stopped the timer first, or it must never have been started.
    fn rearm(&self) -> TimerId {
        let receiver = self.data.id();
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

    /// Starts the timer with the specified interval (`QTimer::start(msec)`).
    pub fn start_with_interval(self: Pin<&mut Self>, ms: u64) -> TimerId {
        self.interval_ms.set(ms);
        self.start()
    }

    /// Stops the timer (`QTimer::stop`).
    pub fn stop(&self) {
        if self.is_active() {
            let id = self.id.get();
            let _ = THREAD_TIMER_CONTEXT.try_with(|ctx| {
                if let Ok(borrow) = ctx.try_borrow() {
                    if let Some(ctx) = borrow.as_ref() {
                        disarm_in_context(ctx, id);
                    }
                }
            });
            self.id.set(TimerId::INVALID);
        }
    }

    pub fn is_active(&self) -> bool {
        self.id.get().is_valid()
    }

    /// Sets the interval (`QTimer::setInterval`); an active timer is restarted with it.
    pub fn set_interval(&self, ms: u64) {
        if self.is_active() {
            self.stop();
            self.interval_ms.set(ms);
            self.rearm();
        } else {
            self.interval_ms.set(ms);
        }
    }

    pub fn interval(&self) -> u64 {
        self.interval_ms.get()
    }

    /// Sets single-shot mode; an active timer's registry entry is updated to match.
    pub fn set_single_shot(&self, v: bool) {
        self.single_shot.set(v);
        if self.is_active() {
            let id = self.id.get();
            with_thread_timer_context(|ctx| {
                if let Some(entry) = ctx.registry.lock().unwrap().get_mut(id) {
                    entry.single_shot = v;
                }
            });
        }
    }

    pub fn is_single_shot(&self) -> bool {
        self.single_shot.get()
    }

    pub fn set_timer_type(&self, t: TimerType) {
        self.timer_type.set(t);
    }

    pub fn timer_type(&self) -> TimerType {
        self.timer_type.get()
    }

    pub fn timer_id(&self) -> TimerId {
        self.id.get()
    }

    /// Returns remaining time in milliseconds (`QTimer::remainingTime`).
    pub fn remaining_time(&self) -> i64 {
        if !self.is_active() {
            return -1;
        }
        let now = current_time_ms();
        let deadline_opt = with_thread_timer_context(|ctx| {
            ctx.registry
                .lock()
                .unwrap()
                .get(self.id.get())
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
    ///
    /// The timer is heap-pinned (see [`Timer::new`]), so its registered address stays valid
    /// until `Drop` unregisters it.
    pub fn start_with_registry(mut self: Pin<&mut Self>, registry: &mut TimerRegistry) -> TimerId {
        if self.is_active() {
            self.stop_with_registry(registry);
        }
        // SAFETY: `self` is pinned in its heap box; `Drop` unregisters the address before the
        // box is freed, so the registry never holds a stale pointer (see `Timer::start`).
        unsafe { crate::object::register_pinned_qobject(self.as_mut()) };
        let id = registry.register(
            self.data.id(),
            self.interval_ms.get(),
            self.timer_type.get(),
            self.single_shot.get(),
        );
        self.id.set(id);
        id
    }

    /// Starts a timer in a caller-supplied registry and dispatcher.
    ///
    /// The timer is heap-pinned (see [`Timer::new`]), so its registered address stays valid
    /// until `Drop` unregisters it.
    pub fn start_with<D: crate::event_loop::EventDispatcher + ?Sized>(
        mut self: Pin<&mut Self>,
        registry: &mut TimerRegistry,
        dispatcher: &mut D,
    ) -> TimerId {
        if self.is_active() {
            self.stop_with(registry, dispatcher);
        }
        // SAFETY: `self` is pinned in its heap box; `Drop` unregisters the address before the
        // box is freed, so the registry never holds a stale pointer (see `Timer::start`).
        unsafe { crate::object::register_pinned_qobject(self.as_mut()) };
        let id = registry.register(
            self.data.id(),
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

    /// Timer registry matching Qt `WinTimerDict`.
    pub fn stop_with<D: crate::event_loop::EventDispatcher + ?Sized>(
        &self,
        registry: &mut TimerRegistry,
        dispatcher: &mut D,
    ) {
        if self.is_active() {
            if let Some(entry) = registry.unregister(self.id.get()) {
                dispatcher.unregister_timer(&entry);
            }
            self.id.set(TimerId::INVALID);
        }
    }

    /// Timer registry matching Qt `WinTimerDict`.
    pub fn stop_with_registry(&self, registry: &mut TimerRegistry) {
        if self.is_active() {
            registry.unregister(self.id.get());
            self.id.set(TimerId::INVALID);
        }
    }

    pub fn timer_event_with_registry(&self, event: &TimerEvent, registry: &mut TimerRegistry) {
        if event.timer_id == self.id.get() {
            if self.single_shot.get() {
                self.stop_with_registry(registry);
            }
            self.timeout.emit(&());
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
        register_single_shot_callback(receiver, callback);
        // Keep the arm when this thread has no timer context yet; `register_thread_timer_context`
        // arms it once the thread's event loop exists, instead of silently dropping it.
        if with_thread_timer_context(|ctx| arm_single_shot(ctx, receiver, interval_ms)).is_none() {
            PENDING_SINGLE_SHOTS.with(|p| p.borrow_mut().push((receiver, interval_ms)));
        }
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
    let mut reg = registry.lock().unwrap();
    let entry = reg.get_mut(id)?;
    if entry.in_timer_event {
        return None;
    }
    entry.in_timer_event = true;
    let mut timer_type = entry.timer_type;
    let (adjusted, next_fire) = calculate_next_timeout(&mut timer_type, entry.interval_ms, now_ms);
    entry.timer_type = timer_type;
    entry.interval_ms = adjusted;
    entry.next_fire_ms = next_fire;
    Some((entry.receiver, entry.single_shot))
}

/// Re-posts a zero-interval repeating timer after its slot ran (Qt re-arms zero timers each
/// loop iteration). Skips the repost when the timer was stopped, restarted, or made single-shot
/// inside the slot, since the registry entry for `timer_id` is then gone or changed.
pub fn repost_zero_timer(receiver: ObjectId, timer_id: u64) {
    let id = TimerId(timer_id as u32);
    let still_armed = with_thread_timer_context(|ctx| {
        let reg = ctx.registry.lock().unwrap();
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

impl QObject for Timer {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }

    fn object_data_mut(&mut self) -> crate::object::ObjectDataMut<'_> {
        crate::object::ObjectDataMut::new(&mut self.data)
    }

    fn as_qobject_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_qobject_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
    /// Handles timer events for this object (`QObject::timerEvent`).
    fn timer_event(&mut self, timer_id: u64) {
        if timer_id == self.id.get().0 as u64 {
            if self.single_shot.get() {
                self.stop();
            }
            self.timeout.emit(&());
        }
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        self.stop();
        // SAFETY: Drop occurs after the timer's event callbacks have completed.
        unsafe { crate::object::unregister_qobject(self.data.id()) };
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
        let mut timer = Timer::new();
        timer.set_interval(100);
        timer.set_single_shot(true);

        let log = Arc::new(Mutex::new(Vec::<&'static str>::new()));
        let log2 = Arc::clone(&log);
        timer.timeout.connect(move |_| {
            log2.lock().unwrap().push("timeout");
        });

        timer.as_mut().start_with_registry(&mut registry);
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
        let mut timer = Timer::new();
        timer.set_interval(10);
        timer.set_single_shot(true);

        let fired = Arc::new(AtomicBool::new(false));
        let f_clone = Arc::clone(&fired);
        timer.timeout.connect(move |_| {
            f_clone.store(true, Ordering::SeqCst);
        });

        timer.as_mut().start();
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

        let mut timer = Timer::new();
        timer.set_single_shot(true);
        timer.set_interval(30);
        timer.timeout.connect(move |_| {
            counter_clone.fetch_add(1, Ordering::SeqCst);
        });

        timer
            .as_mut()
            .start_with(&mut registry.lock().unwrap(), &mut dispatcher);
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

        let mut timer = Timer::new();
        timer.set_interval(50);
        timer.timeout.connect(move |_| {
            counter_clone.fetch_add(1, Ordering::SeqCst);
        });

        timer
            .as_mut()
            .start_with(&mut registry.lock().unwrap(), &mut dispatcher);

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

        let mut timer = Timer::new();
        timer.set_interval(0);
        timer.set_single_shot(true);

        let fired = Arc::new(AtomicBool::new(false));
        let f_clone = Arc::clone(&fired);
        timer.timeout.connect(move |_| {
            f_clone.store(true, Ordering::SeqCst);
        });

        timer.as_mut().start();
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
        let mut timer = Timer::new();
        timer.set_interval(0);
        timer.set_single_shot(true);
        timer.timeout.connect(move |_| {
            f.store(true, Ordering::SeqCst);
        });
        timer.as_mut().start();
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
        let mut timer = Timer::new();
        timer.set_interval(0);
        timer.timeout.connect(move |_| {
            c.fetch_add(1, Ordering::SeqCst);
        });
        timer.as_mut().start();
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
        let mut timer = Timer::new();
        timer.set_interval(60_000);
        timer.timeout.connect(move |_| {
            c.fetch_add(1, Ordering::SeqCst);
        });
        timer.as_mut().start();
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
        let mut timer = Timer::new();
        timer.set_interval(15);
        timer.timeout.connect(move |_| {
            c.fetch_add(1, Ordering::SeqCst);
        });
        timer.as_mut().start();
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
        let mut timer = Timer::new();
        timer.set_interval(15);
        timer.set_single_shot(true);
        timer.timeout.connect(move |_| {
            c.fetch_add(1, Ordering::SeqCst);
        });
        timer.as_mut().start();
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

        let mut timer = Timer::new();
        timer.set_interval(1000);

        assert_eq!(timer.remaining_time(), -1);

        let _event_loop = EventLoop::new();
        timer.as_mut().start();
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
}
