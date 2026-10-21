// SPDX-License-Identifier: LGPL-3.0-only
//! Deferred command queue for safely decoupling tree modifications and layout updates
//! from synchronous event handlers and callbacks.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use ferrokit_core::event_loop::set_deferred_delete_hook;
use ferrokit_core::object::ObjectId;

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

    /// Enqueues an arbitrary closure to be run when the queue is flushed.
    pub fn post_task<F: FnOnce() + 'static>(task: F) {
        Self::post(WidgetCommand::Task(Box::new(task)));
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
        if let Ok(w) = widget.try_borrow() {
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
        } else {
            let weak = Rc::downgrade(widget);
            Self::post_task(move || {
                if let Some(w) = weak.upgrade() {
                    WidgetCommandQueue::delete_later(&w);
                }
            });
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
        let retries = RETRY_QUEUE.with(|q| std::mem::take(&mut *q.borrow_mut()));
        COMMAND_QUEUE.with(|q| q.borrow_mut().extend(retries));
        while COMMAND_QUEUE.with(|q| !q.borrow().is_empty()) {
            let commands = COMMAND_QUEUE.with(|q| std::mem::take(&mut *q.borrow_mut()));
            if commands.is_empty() {
                break;
            }

            let mut layout_targets: Vec<WidgetRef> = Vec::new();
            let mut seen_layout_ids = HashSet::new();
            let mut kept = Vec::new();
            let mut progress = false;

            for cmd in commands {
                match cmd {
                    WidgetCommand::RequestLayout(weak) => {
                        if let Some(target) = weak.upgrade() {
                            // A borrowed target (a nested dispatch from its own handler) keeps its
                            // request for the next flush, as `flush_layouts` does.
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
                    WidgetCommand::Task(task) => {
                        progress = true;
                        task();
                    }
                }
            }
            progress |= !layout_targets.is_empty();
            let some_kept = !kept.is_empty();
            if some_kept {
                COMMAND_QUEUE.with(|q| {
                    let mut queue = q.borrow_mut();
                    kept.append(&mut queue);
                    *queue = kept;
                });
            }

            for target in layout_targets {
                crate::layout_scheduler::LayoutScheduler::invalidate(&target);
            }
            crate::layout_scheduler::LayoutScheduler::activate_pending();
            if some_kept && !progress {
                // Only borrowed targets are left; they wait for the next flush.
                break;
            }
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
