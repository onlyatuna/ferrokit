// SPDX-License-Identifier: LGPL-3.0-only
//! `Application::focus_widget` follows the window focus managers and holds the widget weakly;
//! `focus_changed` fires on every change, as `QApplication::focusChanged` does.

#[path = "support/probe_widget.rs"]
mod probe_widget;

use ferrokit_core::application::CoreApplication;
use ferrokit_core::event::FocusReason;
use ferrokit_core::object::ObjectId;
use ferrokit_gui::geometry::primitives::Rect;
use ferrokit_widgets::application::Application;
use ferrokit_widgets::focus::{FocusManager, FocusPolicy};
use ferrokit_widgets::widget::{EmptyWidget, WidgetRef};
use probe_widget::{Log, ProbeWidget};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

static TEST_MUTEX: Mutex<()> = Mutex::new(());

type Changes = Arc<Mutex<Vec<(Option<ObjectId>, Option<ObjectId>)>>>;

fn strong(log: &Log) -> WidgetRef {
    ProbeWidget::new(log)
        .with_focus(FocusPolicy::StrongFocus)
        .into_ref()
}

fn window(children: &[&WidgetRef]) -> WidgetRef {
    let root: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::with_geometry(
        Rect::new(0, 0, 200, 200),
    ))));
    for c in children {
        c.borrow().set_parent_widget(Some(Rc::downgrade(&root)));
        root.borrow_mut().add_child(Rc::clone(c));
    }
    root
}

fn setup() -> (Application, Changes) {
    CoreApplication::reset_for_test();
    Application::reset_for_test();
    let app = Application::new(vec!["t".to_string()]);
    let changes: Changes = Arc::default();
    let sink = Arc::clone(&changes);
    app.focus_changed()
        .connect(move |c| sink.lock().unwrap().push(*c));
    (app, changes)
}

#[test]
fn focus_manager_updates_the_focus_widget_and_emits_focus_changed() {
    let _guard = TEST_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let (_app, changes) = setup();
    let log = Log::default();
    let (a, b) = (strong(&log), strong(&log));
    let root = window(&[&a, &b]);
    let (ia, ib) = (a.borrow().id(), b.borrow().id());
    let mut fm = FocusManager::new();

    assert!(Application::focus_widget().is_none());
    fm.set_focus(&root, Some(ia), FocusReason::Tab);
    assert_eq!(
        Application::focus_widget().map(|w| w.borrow().id()),
        Some(ia)
    );
    fm.set_focus(&root, Some(ib), FocusReason::Tab);
    assert_eq!(
        Application::focus_widget().map(|w| w.borrow().id()),
        Some(ib)
    );
    fm.clear_focus(&root, FocusReason::Other);
    assert!(Application::focus_widget().is_none());

    assert_eq!(
        *changes.lock().unwrap(),
        [(None, Some(ia)), (Some(ia), Some(ib)), (Some(ib), None)]
    );
}

#[test]
fn the_focus_widget_does_not_keep_a_dropped_window_alive() {
    let _guard = TEST_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let (_app, changes) = setup();
    let log = Log::default();
    let a = strong(&log);
    let root = window(&[&a]);
    let ia = a.borrow().id();
    let mut fm = FocusManager::new();
    fm.set_focus(&root, Some(ia), FocusReason::Tab);

    let weak = Rc::downgrade(&a);
    drop(a);
    drop(root);
    assert!(
        weak.upgrade().is_none(),
        "the application kept the widget alive"
    );

    drop(fm);
    assert!(Application::focus_widget().is_none());
    assert_eq!(changes.lock().unwrap().last(), Some(&(Some(ia), None)));
}

fn in_window(children: &[&WidgetRef]) -> (WidgetRef, ObjectId) {
    let root = window(children);
    let id = root.borrow().id();
    root.borrow().set_window_id(Some(id));
    for c in children {
        c.borrow().set_window_id(Some(id));
    }
    (root, id)
}

#[test]
fn the_focus_widget_is_current_while_focus_in_runs_and_focus_changed_follows_in_order() {
    let _guard = TEST_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let (_app, changes) = setup();
    let log = Log::default();
    let c = strong(&log);
    let root2 = window(&[&c]);
    let fm2 = Rc::new(RefCell::new(FocusManager::new()));
    let ic = c.borrow().id();

    let seen = Rc::new(RefCell::new(Vec::new()));
    let (seen_hook, changes_hook) = (Rc::clone(&seen), Arc::clone(&changes));
    let mut probe = ProbeWidget::new(&log).with_focus(FocusPolicy::StrongFocus);
    probe.on_focus_in = Some(Box::new(move || {
        seen_hook.borrow_mut().push((
            Application::focus_widget().map(|w| w.try_borrow_mut().is_err()),
            changes_hook.lock().unwrap().len(),
        ));
        // A nested focus change in another window while this `FocusIn` runs.
        fm2.borrow_mut()
            .set_focus(&root2, Some(ic), FocusReason::Other);
    }));
    let a = probe.into_ref();
    let root = window(&[&a]);
    let ia = a.borrow().id();
    FocusManager::new().set_focus(&root, Some(ia), FocusReason::Tab);

    // While `FocusIn` ran, `a` was the focus widget (it is borrowed by the handler) and no
    // `focus_changed` had fired yet.
    assert_eq!(*seen.borrow(), [(Some(true), 0)]);
    assert_eq!(
        Application::focus_widget().map(|w| w.borrow().id()),
        Some(ic)
    );
    assert_eq!(
        *changes.lock().unwrap(),
        [(None, Some(ia)), (Some(ia), Some(ic))]
    );
}

#[test]
fn only_the_active_windows_focus_is_the_application_focus_widget() {
    let _guard = TEST_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let (_app, changes) = setup();
    let log = Log::default();
    let (a, b) = (strong(&log), strong(&log));
    let (root1, w1) = in_window(&[&a]);
    let (root2, w2) = in_window(&[&b]);
    let (ia, ib) = (a.borrow().id(), b.borrow().id());
    Application::set_active_window(Some(w1));
    let (mut fm1, mut fm2) = (FocusManager::new(), FocusManager::new());

    fm1.set_focus(&root1, Some(ia), FocusReason::Tab);
    fm2.set_focus(&root2, Some(ib), FocusReason::Tab);
    assert_eq!(
        Application::focus_widget().map(|w| w.borrow().id()),
        Some(ia)
    );
    fm2.clear_focus(&root2, FocusReason::Other);
    fm2.set_focus(&root2, Some(ib), FocusReason::Tab);
    assert_eq!(
        Application::focus_widget().map(|w| w.borrow().id()),
        Some(ia)
    );
    assert_eq!(
        *changes.lock().unwrap(),
        [(None, Some(ia))],
        "inactive windows are silent"
    );

    Application::set_active_window(Some(w2));
    assert_eq!(
        Application::focus_widget().map(|w| w.borrow().id()),
        Some(ib)
    );
    drop(fm1);
    assert_eq!(
        Application::focus_widget().map(|w| w.borrow().id()),
        Some(ib),
        "an inactive window dropping leaves the focus widget alone"
    );
    assert_eq!(
        *changes.lock().unwrap(),
        [(None, Some(ia)), (Some(ia), Some(ib))]
    );
    drop(root1);
    drop(fm2);
}
