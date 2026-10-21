// SPDX-License-Identifier: LGPL-3.0-only
//! A slot adds a row to the layout its own widget sits in ("add on click"). The widget is
//! mutably borrowed by its event delivery, so the layout cannot place it right now; it stays
//! invalid and runs once the borrow ends (Qt posts a `LayoutRequest`, `QLayout::invalidate`).

#[path = "support/probe_widget.rs"]
mod probe_widget;

use ferrokit_core::event::{Event, EventKind};
use ferrokit_gui::geometry::primitives::Rect;
use ferrokit_widgets::hit_test::EventTreeDispatcher;
use ferrokit_widgets::layout::{BoxLayout, GridLayout, Layout};
use ferrokit_widgets::layout_scheduler::LayoutScheduler;
use ferrokit_widgets::widget::{EmptyWidget, Widget, WidgetRef};
use probe_widget::{Log, ProbeWidget};
use std::cell::RefCell;
use std::rc::Rc;

fn container_with(layout: Box<dyn Layout>) -> WidgetRef {
    let mut c = EmptyWidget::with_geometry(Rect::new(0, 0, 200, 200));
    c.set_layout(layout);
    Rc::new(RefCell::new(Box::new(c)))
}

fn adopt(container: &WidgetRef, child: &WidgetRef) {
    child.borrow().set_parent_widget(Some(Rc::downgrade(container)));
    container.borrow_mut().add_child(Rc::clone(child));
}

#[test]
fn box_layout_mutation_with_a_borrowed_item_is_deferred_not_a_panic() {
    let log = Log::default();
    let container = container_with(Box::new(BoxLayout::vertical()));
    let first = ProbeWidget::new(&log).into_ref();
    adopt(&container, &first);
    container.borrow_mut().layout_mut().unwrap().add_widget(Rc::clone(&first));
    LayoutScheduler::activate_pending();

    let second = ProbeWidget::new(&log).into_ref();
    adopt(&container, &second);
    {
        let _in_its_own_handler = first.borrow_mut();
        container.borrow_mut().layout_mut().unwrap().add_widget(Rc::clone(&second));
    }
    LayoutScheduler::flush_deferred(&container);

    let g = second.borrow().geometry();
    assert!(g.y > 0, "the new row was placed below the first: {g:?}");
}

#[test]
fn grid_layout_mutation_with_a_borrowed_item_is_deferred_not_a_panic() {
    let log = Log::default();
    let mut grid = GridLayout::new();
    let first = ProbeWidget::new(&log).into_ref();
    grid.add_widget(Rc::clone(&first), 0, 0);
    let container = container_with(Box::new(grid));
    adopt(&container, &first);
    LayoutScheduler::activate_pending();

    let second = ProbeWidget::new(&log).into_ref();
    adopt(&container, &second);
    {
        let _in_its_own_handler = first.borrow_mut();
        let mut c = container.borrow_mut();
        let grid = c.layout_mut().unwrap();
        grid.add_widget(Rc::clone(&second));
        grid.set_spacing(7);
    }
    LayoutScheduler::flush_deferred(&container);

    assert!(second.borrow().geometry().width > 0);
}

#[test]
fn a_click_slot_that_adds_a_row_is_laid_out_after_the_dispatch() {
    let log = Log::default();
    let root = container_with(Box::new(BoxLayout::vertical()));
    let button = ProbeWidget::new(&log).into_ref();
    adopt(&root, &button);
    root.borrow_mut().layout_mut().unwrap().add_widget(Rc::clone(&button));
    LayoutScheduler::activate_pending();

    let added: Rc<RefCell<Option<WidgetRef>>> = Rc::default();
    let (c, l, slot) = (Rc::clone(&root), log.clone(), Rc::clone(&added));
    button
        .borrow_mut()
        .as_any_mut()
        .downcast_mut::<ProbeWidget>()
        .unwrap()
        .on_press = Some(Box::new(move || {
        let row = ProbeWidget::new(&l).into_ref();
        row.borrow().set_parent_widget(Some(Rc::downgrade(&c)));
        c.borrow_mut().add_child(Rc::clone(&row));
        c.borrow_mut().layout_mut().unwrap().add_widget(Rc::clone(&row));
        *slot.borrow_mut() = Some(row);
    }));

    let pos = button.borrow().geometry();
    let mut dispatcher = EventTreeDispatcher::new();
    let mut ev = Event::new_spontaneous(EventKind::MouseButtonPress {
        x: pos.x + 1,
        y: pos.y + 1,
        button: 1,
    });
    dispatcher.dispatch_event(&root, &mut ev);

    let row = added.borrow().clone().expect("the slot ran");
    assert!(row.borrow().geometry().y > 0, "the added row was placed by the layout");
}
