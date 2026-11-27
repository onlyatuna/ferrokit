// SPDX-License-Identifier: LGPL-3.0-only
use super::*;

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
pub(super) struct TimerControl {
    pub(super) object_id: ObjectId,
    pub(super) id: Cell<TimerId>,
    pub(super) interval_ms: Cell<u64>,
    pub(super) single_shot: Cell<bool>,
    pub(super) timer_type: Cell<TimerType>,
    pub(super) retired: Cell<bool>,
    pub(super) timeout: Signal<()>,
}

impl TimerControl {
    /// The timer's current id, following a re-id from a thread move.
    pub(super) fn current_id(&self) -> TimerId {
        let id = self.id.get();
        if id.is_valid()
            && let Some(fresh) = take_reidentified(self.object_id, id)
        {
            self.id.set(fresh);
            return fresh;
        }
        id
    }

    pub(super) fn is_active(&self) -> bool {
        self.id.get().is_valid()
    }

    /// Marks the control retired and disarms the scheduled timer. One way: a retired control
    /// starts nothing and emits nothing.
    pub(super) fn retire(&self) {
        self.retired.set(true);
        self.stop();
    }

    pub(super) fn start(&self) -> TimerId {
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
    pub(super) fn rearm(&self) -> TimerId {
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

    pub(super) fn stop(&self) {
        if self.is_active() {
            let id = self.current_id();
            let _ = THREAD_TIMER_CONTEXT.try_with(|ctx| {
                if let Ok(borrow) = ctx.try_borrow()
                    && let Some(ctx) = borrow.as_ref()
                {
                    disarm_in_context(ctx, Some(self.object_id), id);
                }
            });
            self.id.set(TimerId::INVALID);
        }
    }

    pub(super) fn set_interval(&self, ms: u64) {
        if self.is_active() {
            self.stop();
            self.interval_ms.set(ms);
            self.rearm();
        } else {
            self.interval_ms.set(ms);
        }
    }

    pub(super) fn set_single_shot(&self, v: bool) {
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

    pub(super) fn start_with_registry(&self, registry: &mut TimerRegistry) -> TimerId {
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

    pub(super) fn start_with<D: crate::event_loop::EventDispatcher + ?Sized>(
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

    pub(super) fn stop_with<D: crate::event_loop::EventDispatcher + ?Sized>(
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

    pub(super) fn stop_with_registry(&self, registry: &mut TimerRegistry) {
        if self.is_active() {
            registry.unregister(self.current_id());
            self.id.set(TimerId::INVALID);
        }
    }
}

/// High-level timer object matching a timer.
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
    pub(super) _root: TimerRoot,
    pub(super) control: Rc<TimerControl>,
    pub timeout: Signal<()>,
}

impl Default for Timer {
    fn default() -> Self {
        Self::new()
    }
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
        // SAFETY: `register_owned_qobject` boxes `core`, so the object keeps one address until
        // the owner is dropped. The owner lives only in `TimerRoot` inside this `Timer`, which is
        // `!Send` (so the registration thread is also where it is dropped), and `Timer` never
        // forms a reference to the object (see the type docs). The object does not opt in to
        // thread moves. While a callback of the object runs, `TimerRoot::drop` does not free
        // it: it hands the owner to `destroy_when_released`, which unregisters at once and frees
        // the memory only after the callback returns.
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

    /// Starts the timer with its configured interval (the timer).
    pub fn start(&self) -> TimerId {
        self.control.start()
    }

    /// Starts the timer with the specified interval (the timer).
    pub fn start_with_interval(&self, ms: u64) -> TimerId {
        self.control.interval_ms.set(ms);
        self.control.start()
    }

    /// Stops the timer (the timer).
    pub fn stop(&self) {
        self.control.stop();
    }

    pub fn is_active(&self) -> bool {
        self.control.is_active()
    }

    /// Sets the interval (the timer); an active timer is restarted with it.
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

    /// Returns remaining time in milliseconds (the timer).
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
    /// the timer: `callback` runs
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
                crate::event::Event::new(crate::event::EventKind::MetaCallNoTarget(Box::new(
                    move || {
                        if interval_ms == 0 {
                            callback();
                        } else {
                            Self::arm_context_single_shot(interval_ms, context, callback);
                        }
                    },
                ))),
            );
        } else {
            Self::arm_context_single_shot(interval_ms, context, callback);
        }
    }

    pub(super) fn arm_context_single_shot(
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

    /// Fires a single-shot timer callback (the timer).
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

    /// the timer for a functor that is not `Send`: `callback` may
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
/// no registry lock held (`QTimerInfoList::activateTimers`), so a slot
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
            && classify_early_tick(now_ms, entry.next_fire_ms, entry.interval_ms)
                == EarlyTick::Defer
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
    use crate::event_loop::{TimerDelivery, deliver_timer_event};
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
/// `timerDict` has it and removes the pending ones in
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
pub(super) struct TimerCore {
    pub(super) data: ObjectData,
    pub(super) control: Rc<TimerControl>,
}

#[cfg(test)]
thread_local! {
    /// Counts destructor runs of `TimerCore` on this thread (lifecycle tests).
    pub(super) static CORE_DROPS: Cell<usize> = const { Cell::new(0) };
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
pub(super) struct TimerRoot {
    pub(super) owner: Option<OwnedObject<TimerCore>>,
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
