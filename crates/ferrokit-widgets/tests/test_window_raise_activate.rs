// SPDX-License-Identifier: LGPL-3.0-only
//! `QWidget::raise` / `activateWindow` on a window, and the `Hide` a window sends its widgets.
//!
//! Real Win32 windows. Raising is checked against the system z-order, which does not depend on
//! which process is in the foreground. Activation depends on the desktop session granting this
//! process the foreground; a test that cannot get the foreground first says so and returns.
#![cfg(windows)]

use std::cell::RefCell;
use std::rc::Rc;

use ferrokit_core::event::{Event, EventKind};
use ferrokit_gui::geometry::primitives::Rect;
use ferrokit_platform::WindowFlags;
use ferrokit_widgets::widget::{EmptyWidget, WidgetRef};
use ferrokit_widgets::window::Window;
use ferrokit_widgets::{adopt_tree, Widget};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindow, GW_HWNDNEXT};

fn window(title: &str) -> Window {
    Window::new(title, Rect::new(100, 100, 300, 200), WindowFlags::NORMAL).expect("window")
}

fn hwnd(win: &Window) -> HWND {
    win.native_handle() as HWND
}

fn foreground() -> HWND {
    // SAFETY: `GetForegroundWindow` takes no arguments, has no preconditions and returns a handle
    // value (possibly null) that is only compared, never dereferenced.
    unsafe { GetForegroundWindow() }
}

/// Whether `upper` is above `lower` in the system z-order.
fn is_above(upper: HWND, lower: HWND) -> bool {
    // SAFETY: `GetWindow` takes a window handle and an integer relation and no pointer. `upper`
    // comes from `hwnd(&win)` of windows that are alive in the test; even a stale handle only
    // makes the call return null, never touches memory of ours.
    let mut cur = unsafe { GetWindow(upper, GW_HWNDNEXT) };
    while !cur.is_null() {
        if cur == lower {
            return true;
        }
        // SAFETY: as for the first `GetWindow` call above: `cur` was just returned by the system,
        // and a window destroyed by another process in the meantime makes the call return null.
        cur = unsafe { GetWindow(cur, GW_HWNDNEXT) };
    }
    false
}

const SKIP: &str = "SKIPPED: the desktop session refused to give this process the foreground";

#[test]
fn raising_a_window_puts_it_above_the_other_window_without_activating_it() {
    let (a, b) = (window("a"), window("b"));
    let (ha, hb) = (hwnd(&a), hwnd(&b));
    let (mut a, mut b) = (a, b);
    a.show();
    b.show();
    assert!(is_above(hb, ha), "control: the window shown last is on top");
    let before = foreground();

    a.raise();
    assert!(is_above(ha, hb), "Window::raise");
    assert_eq!(foreground(), before, "raising does not activate");

    // The same through the window's root widget, as `QWidget::raise` on a top-level widget.
    b.root_widget().borrow().raise();
    assert!(is_above(hb, ha), "Widget::raise on the window root");
    assert_eq!(foreground(), before, "raising does not activate");
}

#[test]
fn activate_window_does_nothing_while_the_window_is_hidden() {
    let win = window("hidden");
    win.activate_window();
    win.root_widget().borrow().activate_window();
    assert_ne!(foreground(), hwnd(&win));
}

#[test]
fn activate_window_asks_the_system_to_make_the_window_the_foreground_window() {
    let (mut a, mut b) = (window("a"), window("b"));
    a.show();
    b.show();
    if foreground() != hwnd(&b) {
        eprintln!("{SKIP}");
        return;
    }

    a.activate_window();
    assert_eq!(foreground(), hwnd(&a), "Window::activate_window");

    b.root_widget().borrow().activate_window();
    assert_eq!(
        foreground(),
        hwnd(&b),
        "Widget::activate_window on the window root"
    );

    // A child widget activates the window it is in.
    let child: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::new())));
    let root = a.root_widget();
    root.borrow_mut().add_child(child.clone());
    adopt_tree(&root);
    child.borrow().activate_window();
    assert_eq!(foreground(), hwnd(&a), "Widget::activate_window on a child");
}

fn on_hide(w: &WidgetRef, f: impl FnMut() + 'static) {
    let g = w.borrow();
    g.as_any()
        .downcast_ref::<EmptyWidget>()
        .unwrap()
        .set_hide_handler(f);
}

#[test]
fn hiding_a_window_sends_hide_to_its_root_its_event_handler_and_its_shown_children() {
    let log: Rc<RefCell<Vec<&'static str>>> = Default::default();
    let mut win = window("hide");
    let root = win.root_widget();
    let (shown, hidden) = (
        Rc::new(RefCell::new(Box::new(EmptyWidget::new()) as Box<dyn Widget>)),
        Rc::new(RefCell::new(Box::new(EmptyWidget::new()) as Box<dyn Widget>)),
    );
    root.borrow_mut().add_child(shown.clone());
    root.borrow_mut().add_child(hidden.clone());
    adopt_tree(&root);
    hidden.borrow().set_visible(false);

    let l = Rc::clone(&log);
    on_hide(&root, move || l.borrow_mut().push("root"));
    let l = Rc::clone(&log);
    on_hide(&shown, move || l.borrow_mut().push("shown child"));
    let l = Rc::clone(&log);
    on_hide(&hidden, move || l.borrow_mut().push("hidden child"));
    let l = Rc::clone(&log);
    win.set_window_event_handler(move |ev: &mut Event| {
        if matches!(ev.kind, EventKind::Hide) {
            assert!(!ev.is_spontaneous(), "Window::hide is not spontaneous");
            l.borrow_mut().push("window handler");
        }
    });

    win.show();
    assert!(log.borrow().is_empty());
    win.hide();
    assert_eq!(*log.borrow(), ["root", "window handler", "shown child"]);
}
