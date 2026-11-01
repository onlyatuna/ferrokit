// SPDX-License-Identifier: LGPL-3.0-only
//! The platform sends raw Win32 virtual keys. A menu must not read F1 (VK 0x70) as the letter
//! 'P' or numpad 1 (VK 0x61) as 'A' when it looks for a mnemonic.

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::object::QObject;
use ferrokit_gui::geometry::primitives::Point;
use ferrokit_widgets::Menu;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

fn press(menu: &mut Menu, key: u32) -> bool {
    let mut ev = Event::new_spontaneous(EventKind::KeyPress {
        key,
        modifiers: 0,
        is_repeat: false,
    });
    menu.event(&mut ev)
}

/// `Menu::popup` needs a shared menu (`aboutToShow` runs with it free); these tests drive an
/// owned one, so hand it over for the popup and take it back.
fn popped(menu: Menu, pos: Point) -> Menu {
    let shared = std::rc::Rc::new(std::cell::RefCell::new(menu));
    Menu::popup(&shared, pos);
    match std::rc::Rc::try_unwrap(shared) {
        Ok(cell) => cell.into_inner(),
        Err(_) => unreachable!("the popup keeps no handle to its menu"),
    }
}

#[test]
fn function_and_numpad_keys_do_not_match_letter_mnemonics() {
    let mut menu = Menu::new("m");
    let print = menu.add_new_action("&Print");
    menu.add_new_action("&Alpha");
    let fired = Arc::new(AtomicUsize::new(0));
    let f = Arc::clone(&fired);
    print.borrow().triggered.connect(move |_| {
        f.fetch_add(1, Ordering::SeqCst);
    });
    let f = Arc::clone(&fired);
    menu.triggered.connect(move |_| {
        f.fetch_add(100, Ordering::SeqCst);
    });
    let mut menu = popped(menu, Point::new(0, 0));

    assert!(!press(&mut menu, 0x70), "F1 is not 'P'");
    assert!(!press(&mut menu, 0x61), "numpad 1 is not 'A'");
    assert_eq!(fired.load(Ordering::SeqCst), 0);

    assert!(press(&mut menu, 0x50), "the 'P' key still activates &Print");
    // `event` runs with the menu borrowed by its caller: the activation follows at the next flush.
    ferrokit_widgets::WidgetCommandQueue::flush();
    assert_eq!(fired.load(Ordering::SeqCst), 101);
}

#[test]
fn submenu_keys_are_normalized_once() {
    // VK_OEM_7 (0xDE) is the apostrophe, Qt 0x27. A sub-menu that normalized it again would read
    // 0x27 as VK_RIGHT and open its own sub-menu; OEM minus/period (0x2D/0x2E) would likewise
    // become INSERT/DELETE.
    let top = Menu::new_ref("top");
    let sub = top.borrow_mut().add_menu("sub");
    let subsub = sub.borrow_mut().add_menu("subsub");
    subsub.borrow_mut().add_new_action("leaf");
    Menu::popup(&top, Point::new(0, 0));
    let send = |key: u32| {
        let mut ev = Event::new_spontaneous(EventKind::KeyPress {
            key,
            modifiers: 0,
            is_repeat: false,
        });
        top.borrow_mut().event(&mut ev)
    };
    send(0x28); // Down selects the sub-menu item
    send(0x27); // Right opens it and selects its first item
    for key in [0xDE, 0xBD, 0xBE] {
        assert!(!send(key), "printable key {key:#x} is ignored");
    }
    assert!(
        !ferrokit_widgets::Widget::is_visible(&*subsub.borrow()),
        "apostrophe did not act as Right"
    );
}
