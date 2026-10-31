// SPDX-License-Identifier: LGPL-3.0-only
//! `QMenu::hideEvent` clears the current action, which also closes the open sub-menu
//! (`qmenu.cpp` `hideEvent` -> `setCurrentAction(nullptr)`): hiding through `setVisible(false)`
//! leaves neither a highlighted item nor a visible sub-menu behind.

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::object::QObject;
use ferrokit_gui::geometry::primitives::Point;
use ferrokit_widgets::{Menu, Widget};

const VK_DOWN: u32 = 0x28;
const VK_RIGHT: u32 = 0x27;

#[test]
fn set_visible_false_closes_the_submenu_and_clears_the_active_action() {
    let top = Menu::new_ref("top");
    let sub = top.borrow_mut().add_menu("sub");
    sub.borrow_mut().add_new_action("item");
    top.borrow_mut().add_new_action("other");

    Menu::popup(&top, Point::new(10, 10));
    for key in [VK_DOWN, VK_RIGHT] {
        let mut ev = Event::new_spontaneous(EventKind::KeyPress {
            key,
            modifiers: 0,
            is_repeat: false,
        });
        top.borrow_mut().event(&mut ev);
    }
    assert!(
        sub.borrow().is_visible(),
        "precondition: the sub-menu is open"
    );
    assert!(
        top.borrow().active_action().is_some(),
        "precondition: an item is highlighted"
    );

    top.borrow().set_visible(false);

    assert!(
        !sub.borrow().is_visible(),
        "the sub-menu closes with its parent"
    );
    assert!(top.borrow().open_submenu().is_none());
    assert!(top.borrow().active_action().is_none());

    // Showing again starts with nothing highlighted and no sub-menu.
    top.borrow().set_visible(true);
    assert!(top.borrow().active_action().is_none());
    assert!(top.borrow().open_submenu().is_none());
    assert!(!sub.borrow().is_visible());
}
