// SPDX-License-Identifier: LGPL-3.0-only
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use ferrokit_core::object::ObjectId;
use ferrokit_gui::geometry::primitives::{Rect, Size};

use crate::widget::{WidgetRef, WidgetWeak};

thread_local! {
    static DIRTY_LAYOUTS: RefCell<Vec<(ObjectId, WidgetWeak)>> = const { RefCell::new(Vec::new()) };
    static IS_ACTIVATING: Cell<bool> = const { Cell::new(false) };
    /// Counts the layout passes skipped because an item widget was borrowed up the stack.
    static DEFERRALS: Cell<u64> = const { Cell::new(0) };
}

/// Scheduler managing layout lifecycle by separating layout invalidation from activation.
///
/// Prevents O(N^2) recursive cascades and reentrant layouts during resize and tree mutations.
pub struct LayoutScheduler;

impl LayoutScheduler {
    pub fn invalidate(widget: &WidgetRef) {
        let Ok(w) = widget.try_borrow() else {
            return;
        };
        if let Some(mut layout) = w.layout_ref_mut() {
            layout.invalidate();
        }
        let id = w.id();
        drop(w);

        DIRTY_LAYOUTS.with(|q| {
            let mut list = q.borrow_mut();
            if !list.iter().any(|(existing_id, _)| *existing_id == id) {
                list.push((id, Rc::downgrade(widget)));
            }
        });
    }

    /// Marks the given weak widget reference's layout as dirty.
    pub fn invalidate_weak(weak: &WidgetWeak) {
        if let Some(w) = weak.upgrade() {
            Self::invalidate(&w);
        }
    }

    /// Requests a layout pass on `widget` (e.g. geometry/window size changed) without
    /// invalidating its child metric cache (`layout.invalidate()`).
    pub fn request_layout(widget: &WidgetRef) {
        let Ok(w) = widget.try_borrow() else {
            return;
        };
        let id = w.id();
        drop(w);

        DIRTY_LAYOUTS.with(|q| {
            let mut list = q.borrow_mut();
            if !list.iter().any(|(existing_id, _)| *existing_id == id) {
                list.push((id, Rc::downgrade(widget)));
            }
        });
    }
    /// Runs `widget`'s own layout when it has been invalidated and nothing else is going to.
    ///
    /// `QLayout::invalidate` posts a `LayoutRequest` to the widget that owns the layout. ferrokit
    /// queues the request on the parent (`WidgetBase::request_layout`); a widget without one, a
    /// window's root, would otherwise stay invalid until its size changed.
    pub fn activate_if_dirty(widget: &WidgetRef) {
        let dirty = widget
            .try_borrow()
            .ok()
            .and_then(|w| {
                let g = w.geometry();
                w.layout_ref_mut().map(|layout| {
                    layout.is_dirty() || layout.geometry().size() != Size::new(g.width, g.height)
                })
            })
            .unwrap_or(false);
        if dirty {
            Self::request_layout(widget);
            Self::activate_pending();
        }
    }

    /// A layout could not run because one of its items is borrowed further up the stack (a slot
    /// mutating its own layout). It stays dirty; [`LayoutScheduler::flush_deferred`] finds it.
    pub(crate) fn note_deferred() {
        DEFERRALS.with(|d| d.set(d.get() + 1));
    }

    /// Runs the layouts left dirty by [`LayoutScheduler::note_deferred`] in the tree under
    /// `root`. Call it where no widget of the tree is borrowed (after an event dispatch, before a
    /// paint). A root that already scanned since the last deferral is skipped cheaply; a tree
    /// that is still borrowed is scanned again next time.
    pub fn flush_deferred(root: &WidgetRef) {
        let epoch = DEFERRALS.with(|d| d.get());
        match root.try_borrow() {
            Ok(w) if w.widget_base().layout_epoch.get() == epoch => return,
            Ok(_) => {}
            Err(_) => return,
        }
        let mut complete = true;
        let mut stack = vec![Rc::clone(root)];
        while let Some(widget) = stack.pop() {
            let Ok(w) = widget.try_borrow() else {
                complete = false;
                continue;
            };
            let dirty = w.layout_ref_mut().is_some_and(|layout| layout.is_dirty());
            stack.extend(w.children());
            drop(w);
            if dirty {
                Self::request_layout(&widget);
            }
        }
        Self::activate_pending();
        if complete && let Ok(w) = root.try_borrow() {
            w.widget_base().layout_epoch.set(epoch);
        }
    }

    /// Returns whether any layout is pending activation.
    pub fn has_pending() -> bool {
        DIRTY_LAYOUTS.with(|q| !q.borrow().is_empty())
    }

    /// Activates all pending layouts iteratively without call-stack recursion.
    pub fn activate_pending() {
        if IS_ACTIVATING.with(|flag| flag.get()) {
            return;
        }
        IS_ACTIVATING.with(|flag| flag.set(true));
        let _activating = ActivatingGuard;

        while DIRTY_LAYOUTS.with(|q| !q.borrow().is_empty()) {
            let items: Vec<(ObjectId, WidgetWeak)> =
                DIRTY_LAYOUTS.with(|q| std::mem::take(&mut *q.borrow_mut()));

            for (_id, weak) in items {
                if let Some(widget_ref) = weak.upgrade() {
                    crate::widget::adopt_tree(&widget_ref);
                    let Ok(widget) = widget_ref.try_borrow() else {
                        // Borrowed on the current stack: retry once the borrow ends.
                        Self::note_deferred();
                        continue;
                    };
                    let dirty = widget
                        .layout_ref_mut()
                        .is_some_and(|layout| layout.is_dirty());
                    if dirty {
                        // `QLayout::activate` bounds the owner by the size constraint first
                        // (qlayout.cpp:996-1129); fitting an activated layout is `doResize`.
                        crate::widget::apply_size_constraint(&**widget);
                    }
                    let g = widget.geometry();
                    let mut metrics_rebuilt = false;
                    if let Some(mut layout) = widget.layout_ref_mut()
                        && (dirty || layout.geometry().size() != Size::new(g.width, g.height))
                    {
                        layout.set_geometry(Rect::new(0, 0, g.width, g.height));
                        metrics_rebuilt = dirty;
                    }
                    if metrics_rebuilt {
                        // `QLayout::activate` ends with `mw->updateGeometry()` (qlayout.cpp:1131):
                        // the owner's size hint may have changed, so its parent's layout runs
                        // next, one level per pass, until the window's root (no parent). A pass
                        // that only fits an activated layout to a new size is `doResize`
                        // (qlayout.cpp:528-533): the metrics are unchanged and nothing above is told.
                        widget.update_geometry();
                    }
                    widget.update();
                }
            }
        }
    }
}

/// Clears `IS_ACTIVATING` when a layout pass ends or unwinds; a flag left set would turn every
/// later layout request on this thread into a no-op.
struct ActivatingGuard;

impl Drop for ActivatingGuard {
    fn drop(&mut self) {
        IS_ACTIVATING.with(|flag| flag.set(false));
    }
}
