// SPDX-License-Identifier: LGPL-3.0-only
//! Disabling the focus widget takes the focus away from it.
//!
//! `QWidgetPrivate::setEnabled_helper` (qwidget.cpp:3442-3446): when the widget being disabled is
//! its window's focus widget, `focusNextChild()` moves the focus on (Tab reason), and when the
//! parent is disabled or no widget is next, `clearFocus()` drops it (Other reason). All of it
//! happens before `setEnabled` returns.
//! Reference: PySide6 6.11.2, buttons `a` and `b`, `a` focused, read right after `setEnabled`:
//! - `a.setEnabled(False)` -> `a:Out:Tab`, `b:In:Tab`, focus widget `b`;
//! - with `b` NoFocus -> `a:Out:Other`, no focus widget;
//! - disabling `a`'s parent -> `a:Out:Other`, no focus widget;
//! - disabling `b` -> no event, focus stays on `a`.
//!
//! `set_widget_enabled` does the same. When a widget of the window is still borrowed, the focus
//! flag and the window's focus widget change at once and the events wait for the next event.

use ferrokit_core::event::{Event, EventKind, FocusReason};
use ferrokit_core::object::{ObjectData, ObjectId, QObject};
use ferrokit_gui::geometry::primitives::Rect;
use ferrokit_widgets::*;
use std::cell::RefCell;
use std::rc::Rc;

type Log = Rc<RefCell<Vec<String>>>;

struct Probe {
    base: WidgetBase,
    name: &'static str,
    log: Log,
}

impl QObject for Probe {
    fn object_data(&self) -> &ObjectData {
        &self.base.object_data
    }
    fn object_data_mut(&mut self) -> &mut ObjectData {
        &mut self.base.object_data
    }
}

impl Widget for Probe {
    fn widget_base(&self) -> &WidgetBase {
        &self.base
    }
    fn id(&self) -> ObjectId {
        self.base.object_data.id
    }
    fn geometry(&self) -> Rect {
        self.base.geometry()
    }
    fn set_geometry(&self, rect: Rect) {
        self.base.set_geometry(rect);
    }
    fn is_visible(&self) -> bool {
        self.base.is_visible()
    }
    fn set_visible(&self, visible: bool) {
        self.base.set_visible(visible);
    }
    fn is_enabled(&self) -> bool {
        self.base.is_enabled()
    }
    fn update(&self) {
        self.base.update();
    }
    fn dirty_rect(&self) -> Option<Rect> {
        self.base.dirty_rect()
    }
    fn clear_dirty(&self) {
        self.base.clear_dirty();
    }
    fn layout(&self) -> Option<&dyn Layout> {
        None
    }
    fn layout_mut(&mut self) -> Option<&mut Box<dyn Layout>> {
        None
    }
    fn set_layout(&mut self, _layout: Box<dyn Layout>) {}
    fn parent_widget(&self) -> Option<WidgetWeak> {
        self.base.parent_widget()
    }
    fn set_parent_widget(&self, parent: Option<WidgetWeak>) {
        self.base.set_parent_widget(parent);
    }
    fn window_id(&self) -> Option<ObjectId> {
        self.base.window_id()
    }
    fn set_window_id(&self, window_id: Option<ObjectId>) {
        self.base.set_window_id(window_id);
    }
    fn children(&self) -> Vec<WidgetRef> {
        self.base.children()
    }
    fn add_child(&mut self, child: WidgetRef) {
        self.base.children.borrow_mut().push(child);
    }
    fn remove_child(&mut self, child_id: ObjectId) {
        self.base
            .children
            .borrow_mut()
            .retain(|c| c.borrow().id() != child_id);
    }
    fn paint_event(&mut self, _painter: &mut ferrokit_gui::paint::Painter) {}
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
    fn focus_policy(&self) -> FocusPolicy {
        self.base.focus_policy()
    }
    fn has_focus(&self) -> bool {
        self.base.has_focus()
    }
    fn set_has_focus(&self, focus: bool) {
        self.base.set_has_focus(focus);
    }
    fn focus_in_event(&mut self, reason: FocusReason) {
        self.log
            .borrow_mut()
            .push(format!("{}:In:{reason:?}", self.name));
    }
    fn focus_out_event(&mut self, reason: FocusReason) {
        self.log
            .borrow_mut()
            .push(format!("{}:Out:{reason:?}", self.name));
    }
}

fn probe(name: &'static str, policy: FocusPolicy, log: &Log) -> WidgetRef {
    let base = WidgetBase::with_geometry(Rect::new(0, 0, 10, 10));
    base.set_focus_policy(policy);
    Rc::new(RefCell::new(Box::new(Probe {
        base,
        name,
        log: Rc::clone(log),
    })))
}

struct Tree {
    root: WidgetRef,
    a: WidgetRef,
    b: WidgetRef,
    c: WidgetRef,
    log: Log,
    d: EventTreeDispatcher,
}

/// `root` holds `c` and `b`; `a` sits in `c` when `nested`, else directly in `root`. `a` has focus.
fn tree(nested: bool, b_policy: FocusPolicy) -> Tree {
    let log = Log::default();
    let root = probe("root", FocusPolicy::NoFocus, &log);
    let a = probe("a", FocusPolicy::StrongFocus, &log);
    let b = probe("b", b_policy, &log);
    let c = probe("c", FocusPolicy::NoFocus, &log);
    if nested {
        c.borrow_mut().add_child(a.clone());
    } else {
        root.borrow_mut().add_child(a.clone());
    }
    root.borrow_mut().add_child(c.clone());
    root.borrow_mut().add_child(b.clone());
    adopt_tree(&root);
    let mut d = EventTreeDispatcher::new();
    let id = a.borrow().id();
    assert!(d
        .focus_manager_mut()
        .set_focus(&root, Some(id), FocusReason::Other));
    log.borrow_mut().clear();
    Tree {
        root,
        a,
        b,
        c,
        log,
        d,
    }
}

impl Tree {
    fn focus(&self) -> Option<ObjectId> {
        self.d.focus_manager().focused_widget_id()
    }
    fn next_event(&mut self) {
        self.d
            .dispatch_event(&self.root, &mut Event::new_spontaneous(EventKind::Expose));
    }
    fn events(&self) -> Vec<String> {
        self.log.borrow().clone()
    }
}

#[test]
fn disabling_the_focus_widget_moves_the_focus_to_the_next_one() {
    let t = tree(false, FocusPolicy::StrongFocus);

    set_widget_enabled(&t.a, false);
    assert_eq!(t.events(), ["a:Out:Tab", "b:In:Tab"]);
    assert!(!t.a.borrow().has_focus());
    assert_eq!(t.focus(), Some(t.b.borrow().id()));
    assert!(t.b.borrow().has_focus());
}

#[test]
fn with_no_next_widget_the_focus_is_cleared() {
    let t = tree(false, FocusPolicy::NoFocus);

    set_widget_enabled(&t.a, false);
    assert_eq!(t.events(), ["a:Out:Other"]);
    assert!(!t.a.borrow().has_focus());
    assert_eq!(t.focus(), None);
}

#[test]
fn disabling_the_parent_clears_the_focus() {
    let t = tree(true, FocusPolicy::StrongFocus);

    set_widget_enabled(&t.c, false);
    assert_eq!(t.events(), ["a:Out:Other"]);
    assert!(!t.a.borrow().has_focus());
    assert_eq!(t.focus(), None);
    assert!(!t.b.borrow().has_focus());
}

#[test]
fn disabling_another_widget_leaves_the_focus_alone() {
    let mut t = tree(false, FocusPolicy::StrongFocus);

    set_widget_enabled(&t.b, false);
    t.next_event();
    assert!(t.events().is_empty());
    assert_eq!(t.focus(), Some(t.a.borrow().id()));
    assert!(t.a.borrow().has_focus());
}

/// A borrowed ancestor (say, disabling a child from the parent's event handler) leaves the events
/// to the next event instead of panicking; the focus state still changes at once.
#[test]
fn with_a_borrowed_ancestor_the_events_wait_for_the_next_event() {
    let mut t = tree(false, FocusPolicy::StrongFocus);

    let guard = t.root.borrow_mut();
    set_widget_enabled(&t.a, false);
    drop(guard);
    assert!(t.events().is_empty());
    assert!(!t.a.borrow().has_focus());
    assert_eq!(t.focus(), None);

    t.next_event();
    assert_eq!(t.events(), ["a:Out:Tab", "b:In:Tab"]);
    assert_eq!(t.focus(), Some(t.b.borrow().id()));
}
