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
    ProbeWidget::new(log).with_focus(FocusPolicy::StrongFocus).into_ref()
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
    app.focus_changed().connect(move |c| sink.lock().unwrap().push(*c));
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
    assert_eq!(Application::focus_widget().map(|w| w.borrow().id()), Some(ia));
    fm.set_focus(&root, Some(ib), FocusReason::Tab);
    assert_eq!(Application::focus_widget().map(|w| w.borrow().id()), Some(ib));
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
    assert!(weak.upgrade().is_none(), "the application kept the widget alive");

    drop(fm);
    assert!(Application::focus_widget().is_none());
    assert_eq!(changes.lock().unwrap().last(), Some(&(Some(ia), None)));
}
