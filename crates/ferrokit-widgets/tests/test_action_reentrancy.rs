// SPDX-License-Identifier: LGPL-3.0-only
//! `QAction::activate` emits `toggled` / `changed` / `triggered` with no borrow of the action
//! held, so a slot reads the state through its own handle (`QAction::isChecked()` in a slot).
//! Slots must be `Send + Sync`, so they reach the thread-bound `Rc` through a thread-local.

use ferrokit_widgets::action::{Action, ActionGroup, ActionRef};
use std::cell::RefCell;
use std::sync::{Arc, Mutex};

thread_local! {
    static ACTIONS: RefCell<Vec<ActionRef>> = const { RefCell::new(Vec::new()) };
}

fn is_checked(index: usize) -> bool {
    ACTIONS.with(|a| a.borrow()[index].borrow().is_checked())
}

fn publish(action: &ActionRef) -> usize {
    ACTIONS.with(|a| {
        let mut a = a.borrow_mut();
        a.push(action.clone());
        a.len() - 1
    })
}

#[test]
fn toggled_and_changed_slots_can_read_the_action_they_belong_to() {
    let action = Action::new_ref("Wrap");
    action.borrow_mut().set_checkable(true);
    let index = publish(&action);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    action.borrow().toggled.connect(move |_| log.lock().unwrap().push(("toggled", is_checked(index))));
    let log = Arc::clone(&seen);
    action.borrow().changed.connect(move |_| log.lock().unwrap().push(("changed", is_checked(index))));

    Action::trigger(&action);

    assert_eq!(*seen.lock().unwrap(), [("changed", true), ("toggled", true)]);
}

#[test]
fn a_triggered_slot_can_read_the_action() {
    let action = Action::new_ref("Wrap");
    action.borrow_mut().set_checkable(true);
    let index = publish(&action);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    action.borrow().triggered.connect(move |checked| {
        log.lock().unwrap().push(*checked == is_checked(index));
    });
    Action::trigger(&action);
    assert_eq!(*seen.lock().unwrap(), [true]);
}

#[test]
fn an_exclusive_group_unchecks_the_previous_action_and_notifies_it() {
    let mut group = ActionGroup::new();
    let a: ActionRef = group.add_new_action("A");
    let b: ActionRef = group.add_new_action("B");
    for x in [&a, &b] {
        x.borrow_mut().set_checkable(true);
    }
    let index = publish(&a);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    a.borrow().toggled.connect(move |on| {
        log.lock().unwrap().push(format!("toggled {on} now {}", is_checked(index)));
    });

    Action::trigger(&a);
    Action::trigger(&b);

    assert_eq!(*seen.lock().unwrap(), ["toggled true now true", "toggled false now false"]);
    assert!(!a.borrow().is_checked());
    assert!(b.borrow().is_checked());
}

#[test]
fn dropped_actions_do_not_pile_up_in_a_group() {
    let mut group = ActionGroup::new();
    for _ in 0..100 {
        let _ = group.add_new_action("temp"); // dropped at once
    }
    let keep = group.add_new_action("keep");
    assert_eq!(group.actions().len(), 1);
    assert!(std::rc::Rc::ptr_eq(&group.actions()[0], &keep));
    assert!(group.member_slots() <= 2, "dead entries are pruned: {}", group.member_slots());
}
