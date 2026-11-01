// SPDX-License-Identifier: LGPL-3.0-only
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ferrokit_gui::geometry::primitives::Point;
use ferrokit_gui::paint::{Painter, Pixmap};
use ferrokit_widgets::Widget;
use ferrokit_widgets::action::Action;
use ferrokit_widgets::button::Button;
use ferrokit_widgets::menu::Menu;
use ferrokit_widgets::style::{ButtonStyleOption, Style};

struct CountingStyle(Rc<Cell<usize>>);

impl Style for CountingStyle {
    fn draw_button(&self, _painter: &mut Painter, _option: &ButtonStyleOption<'_>) {
        self.0.set(self.0.get() + 1);
    }
}

#[test]
fn button_and_menu_can_share_one_action() {
    let action = Action::new_ref("Save");
    let menu = Menu::new_ref("File");
    menu.borrow_mut().add_action(action.clone());
    assert!(Rc::ptr_eq(&menu.borrow().actions()[0], &action));
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let _connection = action.borrow().triggered.connect(move |_| {
        observed.fetch_add(1, Ordering::SeqCst);
    });

    let mut button = Button::new("ignored");
    button.set_action(Some(action.clone()));
    assert_eq!(button.text(), "Save");
    assert!(Rc::ptr_eq(&button.action().unwrap(), &action));

    button.mouse_press_event(Point::new(2, 2), 1, 0);
    button.mouse_release_event(Point::new(2, 2), 1, 0);
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    action.borrow_mut().set_enabled(false);
    button.mouse_press_event(Point::new(2, 2), 1, 0);
    button.mouse_release_event(Point::new(2, 2), 1, 0);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn button_delegates_rendering_to_custom_style() {
    let count = Rc::new(Cell::new(0));
    let mut button = Button::new("Styled");
    button.set_style(Rc::new(CountingStyle(count.clone())));
    let mut pixmap = Pixmap::new(120, 40).unwrap();
    let mut painter = Painter::begin(&mut pixmap);

    button.paint_event(&mut painter);

    assert_eq!(count.get(), 1);
}

#[test]
fn default_style_exposes_metrics_and_hints() {
    let style = ferrokit_widgets::DefaultStyle;
    assert_eq!(style.metrics().button_horizontal_padding, 24);
    assert!(style.hints().keyboard_focus_change_on_tab);
    assert_eq!(
        style.size_from_contents(ferrokit_gui::geometry::primitives::Size::new(20, 10), 4, 6),
        ferrokit_gui::geometry::primitives::Size::new(24, 16)
    );
}
