// SPDX-License-Identifier: LGPL-3.0-only
//! Where a style sheet applies, and who is told when it changes.
//!
//! Qt (`QWidget::setStyleSheet`, qwidget.cpp:2594-2631): a widget's sheet applies to that widget
//! and its subtree; `QStyleSheetStyle::repolish(w)` sends `StyleChange` to `w` and every
//! descendant (`updateObjects`, qstylesheetstyle.cpp:2780). A top-level window is a `QWidget`, so
//! `setStyleSheet` on a window reaches only that window. `QApplication::setStyleSheet`
//! (qapplication.cpp:885) repolishes every widget of every window. `StyleChange` is
//! a repaint, a geometry update and a layout invalidation (qwidget.cpp:9502). A `QMenu` finds its sheet
//! through its parent chain (`styleRules`, qstylesheetstyle.cpp:1654).
//!
//! Every observation here is one event-loop pump away from the change; nothing calls
//! `update_layout()` or `render_and_present()` to help.
#![cfg(windows)]

use std::cell::RefCell;
use std::rc::Rc;

use ferrokit_core::event_loop::EventLoop;
use ferrokit_gui::geometry::primitives::Rect;
use ferrokit_gui::text::font::Font;
use ferrokit_platform::WindowFlags;
use ferrokit_widgets::application::Application;
use ferrokit_widgets::layout::{BoxLayout, Layout};
use ferrokit_widgets::menu::Menu;
use ferrokit_widgets::style::stylesheet::WidgetStyleContext;
use ferrokit_widgets::widget::{EmptyWidget, WidgetRef};
use ferrokit_widgets::window::Window;
use ferrokit_widgets::{Label, Policy, QSizePolicy, Widget};

const BIG: &str = "QLabel { font-size: 28px; }";

/// The application sheet is process-wide: clear it before and after every test.
struct AppSheet;

impl AppSheet {
    fn clean() -> Self {
        Application::set_style_sheet("");
        AppSheet
    }
}

impl Drop for AppSheet {
    fn drop(&mut self) {
        Application::set_style_sheet("");
    }
}

/// A window whose root holds `label` through a layout. `label` is `Fixed` wide, so its
/// geometry follows its size hint exactly.
struct Fx {
    el: EventLoop,
    win: Box<Window>,
    label: WidgetRef,
    root: WidgetRef,
}

fn fixture(title: &str) -> Fx {
    let mut label = Label::new("Alpha");
    label.set_font(Font::new("Segoe UI", 12.0));
    let label: WidgetRef = Rc::new(RefCell::new(Box::new(label)));
    label
        .borrow()
        .set_size_policy(QSizePolicy::new(Policy::Fixed, Policy::Preferred));

    let mut layout = BoxLayout::horizontal();
    layout.add_widget(Rc::clone(&label));
    let root: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::new())));
    root.borrow_mut().set_layout(Box::new(layout));

    let mut win = Box::new(
        Window::new(title, Rect::new(0, 0, 400, 120), WindowFlags::empty()).expect("create window"),
    );
    win.set_root_widget(Rc::clone(&root));

    let mut fx = Fx {
        el: EventLoop::new(),
        win,
        label,
        root,
    };
    fx.win.render_and_present();
    pump_idle(&mut fx);
    fx
}

/// One posted-event pump per turn until nothing changes (bounded).
fn pump_idle(fx: &mut Fx) {
    for _ in 0..16 {
        let before = fx.win.render_stats();
        fx.el.process_events(false);
        if fx.win.render_stats() == before {
            return;
        }
    }
    panic!("render ping-pong: still rendering after 16 turns");
}

fn label_width(fx: &Fx) -> i32 {
    fx.label.borrow().geometry().width
}

fn label_hint(fx: &Fx) -> i32 {
    fx.label.borrow().size_hint().width
}

#[test]
fn application_sheet_reaches_existing_widgets_after_one_pump() {
    let _app = AppSheet::clean();
    let mut fx = fixture("app sheet");
    let before = label_width(&fx);

    Application::set_style_sheet(BIG);
    pump_idle(&mut fx);

    assert!(
        label_hint(&fx) > before,
        "a 28px font must be wider than the 12pt label was"
    );
    assert_eq!(
        label_width(&fx),
        label_hint(&fx),
        "the existing label kept its old geometry"
    );
}

#[test]
fn application_sheet_reaches_every_window() {
    let _app = AppSheet::clean();
    let mut a = fixture("app sheet A");
    let mut b = fixture("app sheet B");
    let (a_before, b_before) = (label_width(&a), label_width(&b));

    Application::set_style_sheet(BIG);
    pump_idle(&mut a);
    pump_idle(&mut b);

    // QApplication::setStyleSheet is the application's: both windows change.
    assert_eq!(label_width(&a), label_hint(&a));
    assert_eq!(label_width(&b), label_hint(&b));
    assert!(label_width(&a) > a_before && label_width(&b) > b_before);
}

#[test]
fn window_set_style_sheet_reaches_its_existing_widgets_after_one_pump() {
    let _app = AppSheet::clean();
    let mut fx = fixture("window sheet");
    let before = label_width(&fx);

    fx.win.set_style_sheet(BIG);
    pump_idle(&mut fx);

    assert!(label_hint(&fx) > before);
    assert_eq!(
        label_width(&fx),
        label_hint(&fx),
        "the existing label kept its old geometry"
    );
}

#[test]
fn window_set_style_sheet_does_not_reach_another_window() {
    let _app = AppSheet::clean();
    let mut a = fixture("scope A");
    let mut b = fixture("scope B");
    let b_before = label_hint(&b);
    assert!(Application::style_sheet().is_none());

    a.win.set_style_sheet(BIG);
    pump_idle(&mut a);
    pump_idle(&mut b);

    assert!(label_hint(&a) > b_before, "A's own label must have changed");
    assert_eq!(
        label_hint(&b),
        b_before,
        "A's window sheet changed B's label"
    );
    assert_eq!(label_width(&b), b_before);
    assert!(
        Application::style_sheet().is_none(),
        "QWidget::setStyleSheet on a window must not become the application's sheet"
    );
}

#[test]
fn a_sheet_on_a_root_widget_without_a_parent_relayouts_its_own_subtree() {
    let _app = AppSheet::clean();
    let mut fx = fixture("root sheet");
    let before = label_width(&fx);

    fx.root.borrow().set_style_sheet(BIG);
    pump_idle(&mut fx);

    assert!(label_hint(&fx) > before);
    assert_eq!(
        label_width(&fx),
        label_hint(&fx),
        "the root has no parent layout above it, so its own layout must be scheduled"
    );
}

#[test]
fn a_sheet_on_a_nested_container_reaches_widgets_below_it() {
    let _app = AppSheet::clean();
    let mut fx = fixture("nested");
    // root -> outer container -> inner container -> label
    let mut inner_layout = BoxLayout::horizontal();
    inner_layout.add_widget(Rc::clone(&fx.label));
    let inner: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::new())));
    inner.borrow_mut().set_layout(Box::new(inner_layout));
    let mut outer_layout = BoxLayout::horizontal();
    outer_layout.add_widget(Rc::clone(&inner));
    let outer: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::new())));
    outer.borrow_mut().set_layout(Box::new(outer_layout));
    fx.win.set_root_widget(outer);
    fx.win.render_and_present();
    pump_idle(&mut fx);
    let before = label_width(&fx);

    // The outer container is two levels above the label, and the label is not its direct child.
    fx.win.root_widget().borrow().set_style_sheet(BIG);
    pump_idle(&mut fx);

    assert!(label_hint(&fx) > before);
    assert_eq!(
        label_width(&fx),
        label_hint(&fx),
        "a descendant two levels down kept its geometry"
    );
}

fn qmenu_ctx() -> WidgetStyleContext<'static> {
    WidgetStyleContext {
        type_name: "QMenu",
        object_name: "",
        pseudo_states: &[],
        sub_control: None,
        attributes: &[],
    }
}

#[test]
fn a_menu_takes_the_sheet_of_the_window_it_hangs_under_and_not_another() {
    let _app = AppSheet::clean();
    let a = fixture("menu A");
    let b = fixture("menu B");
    a.win
        .root_widget()
        .borrow()
        .set_style_sheet("QMenu { background-color: #123456; }");

    let menu_a = Menu::new("a");
    menu_a.set_parent_widget(Some(Rc::downgrade(&a.win.root_widget())));
    let menu_b = Menu::new("b");
    menu_b.set_parent_widget(Some(Rc::downgrade(&b.win.root_widget())));
    let orphan = Menu::new("c");

    assert!(
        menu_a
            .widget_base()
            .resolve_style(&qmenu_ctx())
            .background_color
            .is_some()
    );
    assert!(
        menu_b
            .widget_base()
            .resolve_style(&qmenu_ctx())
            .background_color
            .is_none()
    );
    assert!(
        orphan
            .widget_base()
            .resolve_style(&qmenu_ctx())
            .background_color
            .is_none()
    );
}

fn two_item_menu(parent: Option<&WidgetRef>) -> Menu {
    let mut menu = Menu::new("m");
    menu.add_new_action("Open");
    menu.add_new_action("Close");
    if let Some(p) = parent {
        menu.set_parent_widget(Some(Rc::downgrade(p)));
    }
    menu
}

#[test]
fn a_qmenu_padding_from_its_parent_window_sheet_changes_its_size() {
    let _app = AppSheet::clean();
    let a = fixture("menu padding");
    let root = a.win.root_widget();
    root.borrow().set_style_sheet("QMenu { padding: 4px 0px; }");
    let menu = two_item_menu(Some(&root));
    let before = menu.size_hint().height;

    root.borrow().set_style_sheet("QMenu { padding: 9px 0px; }");

    // The 9px padding replaces the 4px vertical padding on each side: 2 * (9 - 4) = 10 px.
    assert_eq!(menu.size_hint().height, before + 10);
}

#[test]
fn a_qmenu_item_font_size_from_its_parent_window_sheet_changes_its_row_height() {
    let _app = AppSheet::clean();
    let a = fixture("menu font");
    let root = a.win.root_widget();
    root.borrow()
        .set_style_sheet("QMenu::item { font-size: 11px; }");
    let menu = two_item_menu(Some(&root));
    let before = menu.size_hint().height;

    root.borrow()
        .set_style_sheet("QMenu::item { font-size: 24px; }");

    assert!(
        menu.size_hint().height > before,
        "a 24px item font left the menu height unchanged"
    );
}
