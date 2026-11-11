// SPDX-License-Identifier: LGPL-3.0-only
//! Deferred command queue for safely decoupling tree modifications and layout updates
//! from synchronous event handlers and callbacks.

use std::cell::{Cell, RefCell};
use std::collections::{HashSet, VecDeque};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::LocalKey;

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::event_loop::{post_event_to_thread, set_deferred_delete_hook};
use ferrokit_core::object::{ObjectId, ThreadId};
use ferrokit_core::timer::Timer;

use crate::widget::{WidgetRef, WidgetWeak};

/// A command that can be queued during event dispatch and executed later
/// when no widget in the hierarchy is borrowed.
pub enum WidgetCommand {
    /// Request layout recalculation on a container widget.
    RequestLayout(WidgetWeak),
    /// Arbitrary deferred task executed once call frames return.
    Task(Box<dyn FnOnce() + 'static>),
}

thread_local! {
    static COMMAND_QUEUE: RefCell<Vec<WidgetCommand>> = const { RefCell::new(Vec::new()) };
}

thread_local! {
    /// Tasks that found their target borrowed and try again at the next `flush` call, not in the
    /// same one (a nested flush would spin on a widget that stays borrowed).
    static RETRY_QUEUE: RefCell<Vec<WidgetCommand>> = const { RefCell::new(Vec::new()) };
}

thread_local! {
    /// Widgets whose `DeferredDelete` is posted, each with the parent it leaves on delivery.
    static PENDING_DELETES: RefCell<Vec<PendingDelete>> = const { RefCell::new(Vec::new()) };
}

struct PendingDelete {
    id: ObjectId,
    parent: Option<WidgetWeak>,
}

thread_local! {
    /// A wake-up event is queued or about to be: `post_task` posts one only when this is clear, so
    /// any number of tasks cost one event. Cleared by [`WakeGuard`] when the event runs or is
    /// dropped without running.
    static WAKE_PENDING: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
    /// The same for the timer that comes back for tasks that found their target borrowed.
    static RETRY_WAKE_PENDING: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
    /// How many `flush` calls of this thread are running (a task may dispatch an event whose
    /// end flushes again).
    static FLUSH_DEPTH: Cell<u32> = const { Cell::new(0) };
}

/// How long tasks that found their target borrowed wait before the loop looks at them again. A
/// zero delay would spin the loop for as long as the target stays borrowed (a native modal loop).
const RETRY_WAKE_MS: u64 = 20;

/// Clears a "wake pending" flag when dropped. The wake-up closure owns it, so the flag is
/// cleared whether the closure runs or is discarded with its event (loop or thread teardown). It
/// holds the flag by `Arc` and is `Send`, as the closure must be, because the event may be dropped
/// on another thread.
struct WakeGuard(Arc<AtomicBool>);

impl Drop for WakeGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Arms one wake-up that runs [`WidgetCommandQueue::flush`] on this thread, unless one is armed
/// already (`flag`). `arm` posts the closure it is given.
fn arm_wake(flag: &'static LocalKey<Arc<AtomicBool>>, arm: impl FnOnce(Box<dyn FnOnce() + Send>)) {
    let Ok(flag) = flag.try_with(Arc::clone) else {
        return;
    };
    if flag.swap(true, Ordering::AcqRel) {
        return;
    }
    let guard = WakeGuard(flag);
    arm(Box::new(move || {
        // Clear first: the flush may queue more work, which needs a new wake-up.
        drop(guard);
        WidgetCommandQueue::flush();
    }));
}

/// Makes the event loop flush the queue on its next turn, as `QCoreApplication::postEvent`
/// wakes the dispatcher (qcoreapplication.cpp:1725-1730), so a task posted while no widget event
/// is dispatched (a system setting change, a timer slot) is not left until one arrives.
fn wake_loop() {
    arm_wake(&WAKE_PENDING, |wake| {
        post_event_to_thread(
            ThreadId::current(),
            ObjectId(0),
            Event::new(EventKind::MetaCall(Box::new(move |_| wake()))),
        );
    });
}

/// Flushes again after `RETRY_WAKE_MS`, for tasks that are still waiting for a borrow.
fn wake_loop_later() {
    arm_wake(&RETRY_WAKE_PENDING, |wake| {
        Timer::single_shot(RETRY_WAKE_MS, wake);
    });
}

struct FlushDepth;

impl FlushDepth {
    fn enter() -> Self {
        FLUSH_DEPTH.with(|d| d.set(d.get() + 1));
        Self
    }
}

impl Drop for FlushDepth {
    fn drop(&mut self) {
        FLUSH_DEPTH.with(|d| d.set(d.get() - 1));
    }
}

fn flushing() -> bool {
    FLUSH_DEPTH.with(|d| d.get() > 0)
}

/// The commands of one `flush` pass while they are worked off.
struct Batch {
    /// Not looked at yet, in posting order.
    rest: VecDeque<WidgetCommand>,
    /// Layout requests whose target was borrowed.
    kept: Vec<WidgetCommand>,
    /// Layout targets found, not invalidated yet.
    layouts: VecDeque<WidgetRef>,
}

impl Drop for Batch {
    /// A task that panics unwinds through `flush` (the caller decides what a panic means). The
    /// rest of the batch must not die with it: a lost task or layout request leaves state that
    /// only that task would have reset. It goes back in front of what was queued meanwhile, and
    /// the next turn of the loop flushes it, so the commands do not wait for an event.
    fn drop(&mut self) {
        let mut back: Vec<WidgetCommand> = std::mem::take(&mut self.kept);
        back.extend(
            self.layouts
                .drain(..)
                .map(|target| WidgetCommand::RequestLayout(Rc::downgrade(&target))),
        );
        back.extend(self.rest.drain(..));
        if back.is_empty() {
            return;
        }
        requeue_first(back);
        wake_loop();
    }
}

/// Puts `commands` at the front of the queue, before what was posted since they were taken.
fn requeue_first(mut commands: Vec<WidgetCommand>) {
    COMMAND_QUEUE.with(|q| {
        let mut queue = q.borrow_mut();
        commands.append(&mut queue);
        *queue = commands;
    });
}

/// Thread-local command queue managing deferred operations.
pub struct WidgetCommandQueue;

impl WidgetCommandQueue {
    /// Enqueues a command for deferred execution.
    pub fn post(command: WidgetCommand) {
        COMMAND_QUEUE.with(|q| q.borrow_mut().push(command));
    }

    /// Enqueues a layout update for the given widget reference.
    pub fn post_layout(widget: &WidgetRef) {
        Self::post(WidgetCommand::RequestLayout(Rc::downgrade(widget)));
    }

    /// Enqueues a layout update for the given weak reference.
    pub fn post_layout_weak(widget: &WidgetWeak) {
        Self::post(WidgetCommand::RequestLayout(widget.clone()));
    }

    /// Enqueues an arbitrary closure to be run when the queue is flushed: at the end of the
    /// event being dispatched, or, with none, on the next turn of the event loop.
    pub fn post_task<F: FnOnce() + 'static>(task: F) {
        Self::post(WidgetCommand::Task(Box::new(task)));
        wake_loop();
    }

    /// Runs the queued layout requests and leaves every other command queued.
    ///
    /// Qt delivers the posted `LayoutRequest` events a text or size-hint change produced before
    /// the next paint; this is that step, for code that is about to paint. Unlike
    /// [`flush`](Self::flush) it never runs tasks or deletions, which may need the caller's
    /// borrows released first.
    ///
    /// A layout that rebuilt its metrics ends with `updateGeometry()` (qlayout.cpp:1131), which
    /// requests its parent's layout in turn; those requests are delivered too, one level per
    /// round, until a round posts none.
    pub fn flush_layouts() {
        loop {
            let commands = COMMAND_QUEUE.with(|q| std::mem::take(&mut *q.borrow_mut()));
            if commands.is_empty() {
                return;
            }

            let mut layout_targets: Vec<WidgetRef> = Vec::new();
            let mut seen_layout_ids = HashSet::new();
            let mut kept = Vec::new();
            for cmd in commands {
                match cmd {
                    WidgetCommand::RequestLayout(weak) => {
                        if let Some(target) = weak.upgrade() {
                            let Ok(widget) = target.try_borrow() else {
                                kept.push(WidgetCommand::RequestLayout(weak));
                                continue;
                            };
                            let id = widget.id();
                            drop(widget);
                            if seen_layout_ids.insert(id) {
                                layout_targets.push(target);
                            }
                        }
                    }
                    other => kept.push(other),
                }
            }
            // Commands queued while the layouts ran go after the ones that were kept.
            COMMAND_QUEUE.with(|q| {
                let mut queue = q.borrow_mut();
                kept.append(&mut queue);
                *queue = kept;
            });
            if layout_targets.is_empty() {
                return;
            }

            for target in layout_targets {
                crate::layout_scheduler::LayoutScheduler::invalidate(&target);
            }
            crate::layout_scheduler::LayoutScheduler::activate_pending();
        }
    }

    /// Safely requests deferred deletion of a `WidgetRef` (`QObject::deleteLater`).
    ///
    /// The `DeferredDelete` is posted to the core loop (qobject.cpp:2511-2565), so the widget
    /// lives until the loop reaches it. Its parent link is dropped in `on_deferred_delete`. If the
    /// widget's `RefCell` is currently borrowed (e.g. inside its own event callback), the request
    /// is deferred to a queued task that runs once the borrow is released.
    pub fn delete_later(widget: &WidgetRef) {
        match widget.try_borrow() {
            Ok(w) => {
                set_deferred_delete_hook(on_deferred_delete);
                let id = w.id();
                PENDING_DELETES.with(|pending| {
                    let mut pending = pending.borrow_mut();
                    if !pending.iter().any(|e| e.id == id) {
                        pending.push(PendingDelete {
                            id,
                            parent: w.parent_widget(),
                        });
                    }
                });
                w.delete_later();
            }
            _ => {
                let weak = Rc::downgrade(widget);
                // A retry, not a new task: a task that put itself back would keep this flush
                // running for as long as the widget stays borrowed.
                defer_retry(move || {
                    if let Some(w) = weak.upgrade() {
                        WidgetCommandQueue::delete_later(&w);
                    }
                });
            }
        }
    }

    /// Safely requests layout recalculation for a container `WidgetRef`.
    pub fn request_layout(widget: &WidgetRef) {
        Self::post_layout(widget);
    }

    /// Flushes and executes all pending commands.
    ///
    /// Must be invoked when no widget in the hierarchy is borrowed mutably.
    /// Multiple `RequestLayout` commands on the same container are deduplicated.
    pub fn flush() {
        let _depth = FlushDepth::enter();
        let retries = RETRY_QUEUE.with(|q| std::mem::take(&mut *q.borrow_mut()));
        COMMAND_QUEUE.with(|q| q.borrow_mut().extend(retries));
        while COMMAND_QUEUE.with(|q| !q.borrow().is_empty()) {
            let commands = COMMAND_QUEUE.with(|q| std::mem::take(&mut *q.borrow_mut()));
            if commands.is_empty() {
                break;
            }

            let mut seen_layout_ids = HashSet::new();
            let mut progress = false;
            // Holds the batch while it is worked off: if a task panics, what has not been run
            // goes back to the queue on the way out (`Batch::drop`).
            let mut batch = Batch {
                rest: commands.into(),
                kept: Vec::new(),
                layouts: VecDeque::new(),
            };

            while let Some(cmd) = batch.rest.pop_front() {
                match cmd {
                    WidgetCommand::RequestLayout(weak) => {
                        if let Some(target) = weak.upgrade() {
                            // A borrowed target (a nested dispatch from its own handler) keeps its
                            // request for the next flush, as `flush_layouts` does.
                            let Ok(widget) = target.try_borrow() else {
                                batch.kept.push(WidgetCommand::RequestLayout(weak));
                                continue;
                            };
                            let id = widget.id();
                            drop(widget);
                            if seen_layout_ids.insert(id) {
                                batch.layouts.push_back(target);
                            }
                        }
                    }
                    WidgetCommand::Task(task) => {
                        progress = true;
                        task();
                    }
                }
            }
            progress |= !batch.layouts.is_empty();
            let kept = std::mem::take(&mut batch.kept);
            let some_kept = !kept.is_empty();
            if some_kept {
                requeue_first(kept);
            }

            while let Some(target) = batch.layouts.pop_front() {
                crate::layout_scheduler::LayoutScheduler::invalidate(&target);
            }
            crate::layout_scheduler::LayoutScheduler::activate_pending();
            if some_kept && !progress {
                // Only borrowed targets are left; they wait for the next flush.
                break;
            }
        }
        // What found its target borrowed (a retry, a layout request) waits for the next flush;
        // make sure there is one, without spinning the loop.
        if Self::pending_count() > 0 {
            wake_loop_later();
        }
    }

    /// Returns the number of currently pending commands in this thread.
    pub fn pending_count() -> usize {
        COMMAND_QUEUE.with(|q| q.borrow().len()) + RETRY_QUEUE.with(|q| q.borrow().len())
    }

    /// Clears any pending commands (useful for test resets).
    pub fn clear() {
        COMMAND_QUEUE.with(|q| q.borrow_mut().clear());
        RETRY_QUEUE.with(|q| q.borrow_mut().clear());
    }
}

/// Hook run after each delivered `DeferredDelete` (`set_deferred_delete_hook`). Removes the
/// widget from its parent, since Qt drops a deleted child from its parent's children
/// (qcoreapplication.cpp:1870-1909 delivers the event and then runs this hook).
fn on_deferred_delete(id: ObjectId) {
    let pending = PENDING_DELETES.with(|pending| -> Option<PendingDelete> {
        let mut pending = pending.borrow_mut();
        let index = pending.iter().position(|e| e.id == id)?;
        Some(pending.swap_remove(index))
    });
    if let Some(parent) = pending.and_then(|e| e.parent).and_then(|w| w.upgrade()) {
        remove_child_from(&parent, id);
    }
}

/// Queues `task` to run at the next flush, after the current one finished.
pub(crate) fn defer_retry(task: impl FnOnce() + 'static) {
    RETRY_QUEUE.with(|q| q.borrow_mut().push(WidgetCommand::Task(Box::new(task))));
    // Inside a flush the end of that flush arms the delayed retry; arming a next-turn one here
    // would run the retry again at once, for as long as its target stays borrowed.
    if !flushing() {
        wake_loop();
    }
}

/// Removes `id` from `parent`'s children and relayouts `parent`. If `parent`, or a child that
/// cannot be told from `id`, is borrowed, the removal is retried at the next flush.
pub fn remove_child_from(parent: &WidgetRef, id: ObjectId) {
    let retry = |parent: &WidgetRef| {
        let weak = Rc::downgrade(parent);
        let task = move || {
            if let Some(parent) = weak.upgrade() {
                remove_child_from(&parent, id);
            }
        };
        RETRY_QUEUE.with(|q| q.borrow_mut().push(WidgetCommand::Task(Box::new(task))));
    };
    let Ok(mut w) = parent.try_borrow_mut() else {
        retry(parent);
        return;
    };
    w.remove_child(id);
    // A child on the stack could not be told from the one to remove: try again at the next flush.
    let incomplete = w.widget_base().removal_incomplete.replace(false);
    drop(w);
    if incomplete {
        retry(parent);
    }
    crate::layout_scheduler::LayoutScheduler::invalidate(parent);
    crate::layout_scheduler::LayoutScheduler::activate_pending();
}

/// Convenience function to request deferred deletion of a `WidgetRef`.
pub fn delete_widget_later(widget: &WidgetRef) {
    WidgetCommandQueue::delete_later(widget);
}

/// Convenience function to request deferred layout recalculation for a container `WidgetRef`.
pub fn request_widget_layout(widget: &WidgetRef) {
    WidgetCommandQueue::request_layout(widget);
}
