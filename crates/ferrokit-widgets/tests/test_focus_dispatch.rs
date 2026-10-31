// SPDX-License-Identifier: LGPL-3.0-only
//! Focus delivery parity: one `focusInEvent` / `focusOutEvent` per focus change, Tab navigation
//! rules of `QWidget::event` (not with Ctrl or Alt held), and click focus
//! (`giveFocusAccordingToFocusPolicy`) skipping disabled widgets.

#[path = "support/probe_widget.rs"]
mod probe_widget;

use ferrokit_core::event::{Event, EventKind, FocusReason};
use ferrokit_gui::geometry::primitives::Rect;
use ferrokit_widgets::focus::{FocusManager, FocusPolicy};
use ferrokit_widgets::hit_test::EventTreeDispatcher;
use ferrokit_widgets::widget::{set_widget_enabled, EmptyWidget, WidgetRef};
use probe_widget::{Log, ProbeWidget};
use std::cell::RefCell;
use std::rc::Rc;

const MOD_CTRL: u32 = 0x0400_0000;
const MOD_SHIFT: u32 = 0x0200_0000;
const KEY_TAB: u32 = 0x0100_0001;
const KEY_BACKTAB: u32 = 0x0100_0002;

fn tree(probes: &[&WidgetRef]) -> WidgetRef {
    let root: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::with_geometry(
        Rect::new(0, 0, 200, 200),
    ))));
    for p in probes {
        p.borrow().set_parent_widget(Some(Rc::downgrade(&root)));
        root.borrow_mut().add_child(Rc::clone(p));
    }
    root
}

fn strong(log: &Log) -> WidgetRef {
    ProbeWidget::new(log)
        .with_focus(FocusPolicy::StrongFocus)
        .into_ref()
}

fn key(dispatcher: &mut EventTreeDispatcher, root: &WidgetRef, key: u32, modifiers: u32) {
    let mut ev = Event::new_spontaneous(EventKind::KeyPress {
        key,
        modifiers,
        is_repeat: false,
    });
    dispatcher.dispatch_event(root, &mut ev);
}

#[test]
fn a_focus_change_runs_each_focus_handler_once() {
    let log = Log::default();
    let w = strong(&log);
    let root = tree(&[&w]);
    let id = w.borrow().id();
    let mut fm = FocusManager::new();

    fm.set_focus(&root, Some(id), FocusReason::Tab);
    assert_eq!(*log.borrow(), ["focus_in"]);
    fm.clear_focus(&root, FocusReason::Tab);
    assert_eq!(*log.borrow(), ["focus_in", "focus_out"]);
}

#[test]
fn ctrl_or_alt_tab_does_not_move_the_focus_but_tab_and_backtab_do() {
    let (la, lb) = (Log::default(), Log::default());
    let (a, b) = (strong(&la), strong(&lb));
    let root = tree(&[&a, &b]);
    let mut d = EventTreeDispatcher::new();

    key(&mut d, &root, KEY_TAB, 0);
    assert_eq!(*la.borrow(), ["focus_in"], "Tab focuses the first widget");

    key(&mut d, &root, KEY_TAB, MOD_CTRL);
    assert!(lb.borrow().is_empty(), "Ctrl+Tab left the focus alone");
    assert_eq!(
        *la.borrow(),
        ["focus_in", "key 0x1000001"],
        "and the widget saw the key"
    );

    key(&mut d, &root, KEY_TAB, 0);
    assert_eq!(*lb.borrow(), ["focus_in"]);
    key(&mut d, &root, KEY_BACKTAB, 0);
    assert_eq!(
        la.borrow().last().map(String::as_str),
        Some("focus_in"),
        "Backtab went back"
    );
    key(&mut d, &root, KEY_TAB, MOD_SHIFT);
    assert_eq!(
        lb.borrow().last().map(String::as_str),
        Some("focus_in"),
        "Shift+Tab wrapped"
    );
}

#[test]
fn clicking_a_disabled_widget_does_not_give_it_focus() {
    let log = Log::default();
    let w = strong(&log);
    let root = tree(&[&w]);
    set_widget_enabled(&w, false);
    let mut fm = FocusManager::new();

    assert!(!fm.handle_mouse_click(&root, &w));
    assert_eq!(fm.focused_widget_id(), None);
    assert!(!log.borrow().iter().any(|e| e.starts_with("focus")));

    set_widget_enabled(&w, true);
    assert!(fm.handle_mouse_click(&root, &w));
    assert_eq!(log.borrow().last().map(String::as_str), Some("focus_in"));
}

#[test]
fn disabling_a_container_by_attribute_notifies_its_children() {
    use ferrokit_widgets::widget::WidgetAttribute;
    let log = Log::default();
    let child = strong(&log);
    let root = tree(&[&child]);

    root.borrow().set_attribute(WidgetAttribute::Disabled, true);

    assert_eq!(*log.borrow(), ["enabled_change"]);
    assert!(!child.borrow().is_enabled());
}
