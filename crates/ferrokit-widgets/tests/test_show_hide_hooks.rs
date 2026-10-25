// SPDX-License-Identifier: LGPL-3.0-only
//! `QWidget::showEvent` / `hideEvent` as a widget can react to them: `Widget::show_event` /
//! `hide_event`, which a widget's `event` calls for `Show` / `Hide`, and the handlers of
//! `EmptyWidget`. Hiding sends the widget a non-spontaneous `Hide`; the children of a hidden
//! widget get one too, unless they were hidden explicitly before.

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::object::{ObjectData, ObjectId, QObject};
use ferrokit_gui::geometry::primitives::Rect;
use ferrokit_widgets::*;
use std::cell::RefCell;
use std::rc::Rc;

type Log = Rc<RefCell<Vec<String>>>;

/// A widget that overrides `show_event` / `hide_event` and wires them to `event`, as a widget
/// with its own `QObject::event` does. Records `<name>:<Show|Hide>:<spontaneous|normal>`.
struct Spy {
    base: WidgetBase,
    name: &'static str,
    log: Log,
    spontaneous: bool,
}

impl QObject for Spy {
    fn object_data(&self) -> &ObjectData {
        &self.base.object_data
    }
    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.base.object_data)
    }
    fn event(&mut self, event: &mut Event) -> bool {
        self.spontaneous = event.is_spontaneous();
        match event.kind {
            EventKind::Show => self.show_event(),
            EventKind::Hide => self.hide_event(),
            _ => return false,
        }
        true
    }
}

impl Widget for Spy {
    fn widget_base(&self) -> &WidgetBase {
        &self.base
    }
    fn id(&self) -> ObjectId {
        self.base.object_data.id()
    }
    fn geometry(&self) -> Rect {
        self.base.geometry()
    }
    fn set_geometry(&self, rect: Rect) {
        self.base.set_geometry(rect);
    }
    fn is_visible(&self) -> bool {
        self.base.is_visible()
    }
    fn set_visible(&self, visible: bool) {
        self.base.set_visible(visible);
    }
    fn is_enabled(&self) -> bool {
        self.base.is_enabled()
    }
    fn update(&self) {
        self.base.update();
    }
    fn dirty_rect(&self) -> Option<Rect> {
        self.base.dirty_rect()
    }
    fn clear_dirty(&self) {
        self.base.clear_dirty();
    }
    fn layout(&self) -> Option<&dyn Layout> {
        None
    }
    fn layout_mut(&mut self) -> Option<&mut Box<dyn Layout>> {
        None
    }
    fn set_layout(&mut self, _layout: Box<dyn Layout>) {}
    fn parent_widget(&self) -> Option<WidgetWeak> {
        self.base.parent_widget()
    }
    fn set_parent_widget(&self, parent: Option<WidgetWeak>) {
        self.base.set_parent_widget(parent);
    }
    fn window_id(&self) -> Option<ObjectId> {
        self.base.window_id()
    }
    fn set_window_id(&self, window_id: Option<ObjectId>) {
        self.base.set_window_id(window_id);
    }
    fn children(&self) -> Vec<WidgetRef> {
        self.base.children()
    }
    fn add_child(&mut self, child: WidgetRef) {
        self.base.children.borrow_mut().push(child);
    }
    fn remove_child(&mut self, child_id: ObjectId) {
        self.base
            .children
            .borrow_mut()
            .retain(|c| c.borrow().id() != child_id);
    }
    fn paint_event(&mut self, _painter: &mut ferrokit_gui::paint::Painter) {}
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
    fn show_event(&mut self) {
        self.log.borrow_mut().push(format!(
            "{}:Show:{}",
            self.name,
            if self.spontaneous {
                "spontaneous"
            } else {
                "normal"
            }
        ));
    }
    fn hide_event(&mut self) {
        self.log.borrow_mut().push(format!(
            "{}:Hide:{}",
            self.name,
            if self.spontaneous {
                "spontaneous"
            } else {
                "normal"
            }
        ));
    }
}

fn spy(name: &'static str, log: &Log) -> WidgetRef {
    Rc::new(RefCell::new(Box::new(Spy {
        base: WidgetBase::with_geometry(Rect::new(0, 0, 10, 10)),
        name,
        log: Rc::clone(log),
        spontaneous: true,
    })))
}

fn empty() -> WidgetRef {
    Rc::new(RefCell::new(Box::new(EmptyWidget::with_geometry(
        Rect::new(0, 0, 10, 10),
    ))))
}

fn with_empty<R>(w: &WidgetRef, f: impl FnOnce(&EmptyWidget) -> R) -> R {
    let g = w.borrow();
    f(g.as_any().downcast_ref::<EmptyWidget>().unwrap())
}

fn take(log: &Log) -> Vec<String> {
    std::mem::take(&mut *log.borrow_mut())
}

#[test]
fn an_empty_widgets_hide_and_show_handlers_run_when_it_is_hidden_and_shown() {
    let log = Log::default();
    let w = empty();
    let l = Rc::clone(&log);
    with_empty(&w, |e| {
        e.set_hide_handler(move || l.borrow_mut().push("hide".into()))
    });
    let l = Rc::clone(&log);
    with_empty(&w, |e| {
        e.set_show_handler(move || l.borrow_mut().push("show".into()))
    });

    set_widget_visible(&w, false);
    assert_eq!(take(&log), ["hide"]);
    // Hiding a hidden widget changes nothing, so it sends nothing.
    set_widget_visible(&w, false);
    assert!(take(&log).is_empty());

    set_widget_visible(&w, true);
    assert_eq!(take(&log), ["show"]);
}

#[test]
fn hiding_a_parent_sends_hide_to_it_and_to_the_children_that_were_shown() {
    let log = Log::default();
    let (parent, shown, hidden_before) =
        (spy("parent", &log), spy("shown", &log), spy("hidden", &log));
    parent.borrow_mut().add_child(shown.clone());
    parent.borrow_mut().add_child(hidden_before.clone());
    adopt_tree(&parent);
    hidden_before.borrow().set_visible(false);

    set_widget_visible(&parent, false);
    assert_eq!(
        take(&log),
        ["parent:Hide:normal", "shown:Hide:normal"],
        "the parent first, then its shown children; an explicitly hidden child gets nothing"
    );

    set_widget_visible(&parent, true);
    assert_eq!(
        take(&log),
        ["shown:Show:normal", "parent:Show:normal"],
        "the children first, then the parent"
    );
}

#[test]
fn a_hide_handler_can_hide_another_widget_whose_handler_runs_too() {
    // The shape of a sidebar that closes its flyout when it is hidden.
    let log = Log::default();
    let (sidebar, flyout) = (empty(), empty());
    let l = Rc::clone(&log);
    with_empty(&flyout, |e| {
        e.set_hide_handler(move || l.borrow_mut().push("flyout hidden".into()))
    });
    let (l, f) = (Rc::clone(&log), flyout.clone());
    with_empty(&sidebar, |e| {
        e.set_hide_handler(move || {
            l.borrow_mut().push("sidebar hidden".into());
            set_widget_visible(&f, false);
        })
    });

    set_widget_visible(&sidebar, false);
    assert_eq!(take(&log), ["sidebar hidden", "flyout hidden"]);
    assert!(!flyout.borrow().is_visible());
}
