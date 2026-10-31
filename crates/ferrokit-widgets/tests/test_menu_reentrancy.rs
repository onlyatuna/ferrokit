// SPDX-License-Identifier: LGPL-3.0-only
//! `QMenu::aboutToShow` / `aboutToHide` of a sub-menu run with no borrow of the sub-menu held:
//! filling a menu from `aboutToShow` is the Qt idiom and needs the menu itself.
//! Slots must be `Send + Sync`, so they reach the thread-bound `Rc` through a thread-local.

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::object::QObject;
use ferrokit_gui::geometry::primitives::Point;
use ferrokit_widgets::{Action, Menu, MenuRef, Widget};
use std::cell::RefCell;
use std::sync::{Arc, Mutex};

thread_local! {
    static SUB: RefCell<Option<MenuRef>> = const { RefCell::new(None) };
}

fn key(menu: &MenuRef, key: u32) {
    let mut ev = Event::new_spontaneous(EventKind::KeyPress {
        key,
        modifiers: 0,
        is_repeat: false,
    });
    menu.borrow_mut().event(&mut ev);
}

const VK_DOWN: u32 = 0x28;
const VK_RIGHT: u32 = 0x27;

#[test]
fn a_submenu_can_be_filled_from_its_about_to_show_slot() {
    let top = Menu::new_ref("top");
    let sub = top.borrow_mut().add_menu("sub");
    SUB.with(|s| *s.borrow_mut() = Some(sub.clone()));
    sub.borrow().about_to_show.connect(|_| {
        SUB.with(|s| {
            s.borrow()
                .as_ref()
                .unwrap()
                .borrow_mut()
                .add_action(Action::new_ref("filled"));
        });
    });

    Menu::popup(&top, Point::new(10, 10));
    key(&top, VK_DOWN);
    key(&top, VK_RIGHT);

    assert_eq!(sub.borrow().actions().len(), 1, "the slot added its action");
    assert!(sub.borrow().is_visible(), "and the sub-menu opened");
}

#[test]
fn a_submenu_about_to_hide_slot_can_read_the_submenu() {
    let top = Menu::new_ref("top");
    let sub = top.borrow_mut().add_menu("sub");
    sub.borrow_mut().add_new_action("item");
    SUB.with(|s| *s.borrow_mut() = Some(sub.clone()));
    let titles = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&titles);
    sub.borrow().about_to_hide.connect(move |_| {
        SUB.with(|s| {
            log.lock()
                .unwrap()
                .push(s.borrow().as_ref().unwrap().borrow().title().to_owned());
        });
    });

    Menu::popup(&top, Point::new(10, 10));
    key(&top, VK_DOWN);
    key(&top, VK_RIGHT);
    Menu::hide_menu(&top);

    assert_eq!(*titles.lock().unwrap(), ["sub"]);
}

#[test]
fn a_borrowed_submenu_is_still_hidden_when_its_parent_hides() {
    use ferrokit_widgets::command::WidgetCommandQueue;
    WidgetCommandQueue::clear();
    let top = Menu::new_ref("top");
    let sub = top.borrow_mut().add_menu("sub");
    sub.borrow_mut().add_new_action("x");
    Menu::popup(&top, Point::new(0, 0));
    key(&top, VK_DOWN);
    key(&top, VK_RIGHT);
    assert!(sub.borrow().is_visible());
    {
        let _held = sub.borrow_mut();
        Menu::hide_menu(&top);
    }
    WidgetCommandQueue::flush();
    assert!(!sub.borrow().is_visible());
}

#[test]
fn a_borrowed_submenu_emits_about_to_hide_once() {
    use ferrokit_widgets::command::WidgetCommandQueue;
    WidgetCommandQueue::clear();
    let top = Menu::new_ref("top");
    let sub = top.borrow_mut().add_menu("sub");
    sub.borrow_mut().add_new_action("x");
    let count = Arc::new(Mutex::new(0));
    let c = Arc::clone(&count);
    sub.borrow()
        .about_to_hide
        .connect(move |_| *c.lock().unwrap() += 1);
    Menu::popup(&top, Point::new(0, 0));
    key(&top, VK_DOWN);
    key(&top, VK_RIGHT);
    {
        let _held = sub.borrow(); // immutably borrowed: hide must wait
        Menu::hide_menu(&top);
    }
    WidgetCommandQueue::flush();
    assert!(!sub.borrow().is_visible());
    assert_eq!(*count.lock().unwrap(), 1);
}
