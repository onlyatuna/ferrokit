// SPDX-License-Identifier: LGPL-3.0-only
//! Parity tests for `setAttribute` / `testAttribute`.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::object::{ObjectData, ObjectDataMut, ObjectId, QObject};
use ferrokit_gui::geometry::primitives::{Point, Rect};
use ferrokit_widgets::hit_test::EventTreeDispatcher;
use ferrokit_widgets::layout::Layout;
use ferrokit_widgets::widget::{
    EmptyWidget, Widget, WidgetAttribute, WidgetBase, WidgetRef, WidgetWeak,
};

#[test]
fn widget_attribute_get_and_set() {
    let w = EmptyWidget::new();

    // MouseTracking
    assert!(!w.test_attribute(WidgetAttribute::MouseTracking));
    w.set_attribute(WidgetAttribute::MouseTracking, true);
    assert!(w.test_attribute(WidgetAttribute::MouseTracking));
    assert!(w.widget_base().has_mouse_tracking());

    // TransparentForMouseEvents
    assert!(!w.test_attribute(WidgetAttribute::TransparentForMouseEvents));
    w.set_attribute(WidgetAttribute::TransparentForMouseEvents, true);
    assert!(w.test_attribute(WidgetAttribute::TransparentForMouseEvents));
    assert!(w.widget_base().is_transparent_for_mouse_events());

    // NoMousePropagation
    assert!(!w.test_attribute(WidgetAttribute::NoMousePropagation));
    w.set_attribute(WidgetAttribute::NoMousePropagation, true);
    assert!(w.test_attribute(WidgetAttribute::NoMousePropagation));
    assert!(w.widget_base().has_no_mouse_propagation());

    // AlwaysShowToolTips
    assert!(!w.test_attribute(WidgetAttribute::AlwaysShowToolTips));
    w.set_attribute(WidgetAttribute::AlwaysShowToolTips, true);
    assert!(w.test_attribute(WidgetAttribute::AlwaysShowToolTips));
    assert!(w.always_show_tool_tips());

    // AcceptDrops
    assert!(!w.test_attribute(WidgetAttribute::AcceptDrops));
    assert!(!w.accept_drops());
    w.set_accept_drops(true);
    assert!(w.test_attribute(WidgetAttribute::AcceptDrops));
    assert!(w.accept_drops());
    w.set_attribute(WidgetAttribute::AcceptDrops, false);
    assert!(!w.accept_drops());

    // Disabled
    assert!(!w.test_attribute(WidgetAttribute::Disabled));
    assert!(w.is_enabled());
    w.set_attribute(WidgetAttribute::Disabled, true);
    assert!(w.test_attribute(WidgetAttribute::Disabled));
    assert!(!w.is_enabled());
}

type DropLog = Arc<Mutex<Vec<(&'static str, &'static str, Point)>>>;

struct DropTargetWidget {
    base: WidgetBase,
    name: &'static str,
    accept_drag: bool,
    /// Replaces the drop action of an incoming drag event, as a widget choosing copy over move.
    action: Option<u32>,
    /// `false`: `event` does not handle `DragMove` at all (it only implements enter and drop).
    handles_move: bool,
    log: DropLog,
}

impl QObject for DropTargetWidget {
    fn object_data(&self) -> &ObjectData {
        &self.base.object_data
    }
    fn object_data_mut(&mut self) -> ObjectDataMut<'_> {
        ObjectDataMut::new(&mut self.base.object_data)
    }
    fn event(&mut self, ev: &mut Event) -> bool {
        if let (
            Some(action),
            EventKind::DragEnter { drop_action, .. }
            | EventKind::DragMove { drop_action, .. }
            | EventKind::Drop { drop_action, .. },
        ) = (self.action, &mut ev.kind)
        {
            *drop_action = action;
        }
        match &ev.kind {
            EventKind::DragEnter { pos_x, pos_y, .. } => {
                self.log
                    .lock()
                    .unwrap()
                    .push((self.name, "enter", Point::new(*pos_x, *pos_y)));
                ev.set_accepted(self.accept_drag);
                self.accept_drag
            }
            EventKind::DragMove { .. } if !self.handles_move => false,
            EventKind::DragMove { pos_x, pos_y, .. } => {
                self.log
                    .lock()
                    .unwrap()
                    .push((self.name, "move", Point::new(*pos_x, *pos_y)));
                ev.set_accepted(self.accept_drag);
                self.accept_drag
            }
            EventKind::DragLeave => {
                self.log
                    .lock()
                    .unwrap()
                    .push((self.name, "leave", Point::new(0, 0)));
                true
            }
            EventKind::Drop { pos_x, pos_y, .. } => {
                self.log
                    .lock()
                    .unwrap()
                    .push((self.name, "drop", Point::new(*pos_x, *pos_y)));
                ev.set_accepted(self.accept_drag);
                self.accept_drag
            }
            _ => false,
        }
    }
}

impl Widget for DropTargetWidget {
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
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

#[test]
fn drag_and_drop_propagates_to_accepting_widget_in_local_coordinates_and_tracks_target_transitions()
{
    let log = Arc::new(Mutex::new(Vec::new()));
    let root: WidgetRef = Rc::new(RefCell::new(Box::new(DropTargetWidget {
        base: WidgetBase::with_geometry(Rect::new(0, 0, 200, 200)),
        name: "root",
        accept_drag: true,
        action: None,
        handles_move: true,
        log: Arc::clone(&log),
    })));
    root.borrow().set_accept_drops(true);

    let refusing_child: WidgetRef = Rc::new(RefCell::new(Box::new(DropTargetWidget {
        base: WidgetBase::with_geometry(Rect::new(10, 10, 80, 80)),
        name: "refusing",
        accept_drag: false,
        action: None,
        handles_move: true,
        log: Arc::clone(&log),
    })));
    refusing_child.borrow().set_accept_drops(true);

    let accepting_child: WidgetRef = Rc::new(RefCell::new(Box::new(DropTargetWidget {
        base: WidgetBase::with_geometry(Rect::new(100, 20, 80, 80)),
        name: "accepting",
        accept_drag: true,
        action: None,
        handles_move: true,
        log: Arc::clone(&log),
    })));
    accepting_child.borrow().set_accept_drops(true);

    let overlay_without_accept_drops: WidgetRef =
        Rc::new(RefCell::new(Box::new(DropTargetWidget {
            base: WidgetBase::with_geometry(Rect::new(0, 0, 80, 80)),
            name: "overlay",
            accept_drag: true,
            action: None,
            handles_move: true,
            log: Arc::clone(&log),
        })));
    // `overlay` does NOT set `accept_drops(true)`, so the drag passes through to `accepting_child`.
    accepting_child
        .borrow_mut()
        .add_child(overlay_without_accept_drops.clone());
    overlay_without_accept_drops
        .borrow_mut()
        .set_parent_widget(Some(Rc::downgrade(&accepting_child)));

    root.borrow_mut().add_child(refusing_child.clone());
    refusing_child
        .borrow_mut()
        .set_parent_widget(Some(Rc::downgrade(&root)));
    root.borrow_mut().add_child(accepting_child.clone());
    accepting_child
        .borrow_mut()
        .set_parent_widget(Some(Rc::downgrade(&root)));

    let mut dispatcher = EventTreeDispatcher::new();

    // 1. Enter over `refusing_child` (window 30, 40): child refuses, propagates to `root`.
    let mut enter = Event::new_spontaneous(EventKind::DragEnter {
        pos_x: 30,
        pos_y: 40,
        formats: vec!["text/plain".into()],
        drop_action: 2,
    });
    assert!(dispatcher.dispatch_event(&root, &mut enter));
    assert_eq!(
        *log.lock().unwrap(),
        vec![
            ("refusing", "enter", Point::new(20, 30)),
            ("root", "enter", Point::new(30, 40)),
        ]
    );
    log.lock().unwrap().clear();

    // 2. Move into `accepting_child` (window 125, 50 -> local 25, 30): `overlay` is skipped
    //    because it has `accept_drops() == false`, `accepting` gets `DragEnter` + `DragMove`,
    //    and `root` gets `DragLeave`.
    let mut mv = Event::new_spontaneous(EventKind::DragMove {
        pos_x: 125,
        pos_y: 50,
        drop_action: 2,
    });
    assert!(dispatcher.dispatch_event(&root, &mut mv));
    assert_eq!(
        *log.lock().unwrap(),
        vec![
            ("accepting", "enter", Point::new(25, 30)),
            ("root", "leave", Point::new(0, 0)),
            ("accepting", "move", Point::new(25, 30)),
        ]
    );
    log.lock().unwrap().clear();

    // 3. Drop at (130, 55 -> local 30, 35 on `accepting`).
    let mut drop_ev = Event::new_spontaneous(EventKind::Drop {
        pos_x: 130,
        pos_y: 55,
        formats: vec!["text/plain".into()],
        data: vec![("text/plain".into(), b"hi".to_vec())],
        drop_action: 2,
    });
    assert!(dispatcher.dispatch_event(&root, &mut drop_ev));
    assert_eq!(
        *log.lock().unwrap(),
        vec![("accepting", "drop", Point::new(30, 35))]
    );
}

fn drop_widget(rect: Rect, action: Option<u32>, handles_move: bool, log: &DropLog) -> WidgetRef {
    let w: WidgetRef = Rc::new(RefCell::new(Box::new(DropTargetWidget {
        base: WidgetBase::with_geometry(rect),
        name: "w",
        accept_drag: true,
        action,
        handles_move,
        log: Arc::clone(log),
    })));
    w.borrow().set_accept_drops(true);
    w
}

fn drag_enter(x: i32, y: i32, action: u32) -> Event {
    Event::new_spontaneous(EventKind::DragEnter {
        pos_x: x,
        pos_y: y,
        formats: vec!["text/plain".into()],
        drop_action: action,
    })
}

fn drop_action_of(ev: &Event) -> u32 {
    match &ev.kind {
        EventKind::DragEnter { drop_action, .. }
        | EventKind::DragMove { drop_action, .. }
        | EventKind::Drop { drop_action, .. } => *drop_action,
        other => panic!("not a drag event: {other:?}"),
    }
}

#[test]
fn a_drag_target_that_does_not_handle_drag_move_keeps_the_drag_and_its_action_reaches_the_caller() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let root = drop_widget(Rect::new(0, 0, 100, 100), Some(1), false, &log);
    let mut dispatcher = EventTreeDispatcher::new();

    let mut enter = drag_enter(10, 10, 2);
    assert!(dispatcher.dispatch_event(&root, &mut enter));
    assert_eq!(
        drop_action_of(&enter),
        1,
        "the widget's chosen action is reported back"
    );

    // `QDragMoveEvent` starts accepted and the widget does not touch it: the drag stays accepted.
    let mut mv = Event::new_spontaneous(EventKind::DragMove {
        pos_x: 20,
        pos_y: 20,
        drop_action: 2,
    });
    assert!(dispatcher.dispatch_event(&root, &mut mv));
    assert!(dispatcher.has_drag_target());
    assert_eq!(
        *log.lock().unwrap(),
        vec![("w", "enter", Point::new(10, 10))]
    );
}

#[test]
fn drop_goes_only_to_the_drag_target_and_a_destroyed_target_is_not_kept_alive() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let root = drop_widget(Rect::new(0, 0, 100, 100), None, true, &log);
    let child = drop_widget(Rect::new(10, 10, 50, 50), None, true, &log);
    root.borrow_mut().add_child(child.clone());
    child
        .borrow_mut()
        .set_parent_widget(Some(Rc::downgrade(&root)));
    let drop_at = |x, y| {
        Event::new_spontaneous(EventKind::Drop {
            pos_x: x,
            pos_y: y,
            formats: vec!["text/plain".into()],
            data: vec![("text/plain".into(), b"hi".to_vec())],
            drop_action: 2,
        })
    };

    // No `DragEnter` came first: nobody is handed the `Drop`.
    let mut dispatcher = EventTreeDispatcher::new();
    assert!(!dispatcher.dispatch_event(&root, &mut drop_at(20, 20)));
    assert!(log.lock().unwrap().is_empty());

    // The target is destroyed mid-drag: the dispatcher holds no strong reference to it.
    assert!(dispatcher.dispatch_event(&root, &mut drag_enter(20, 20, 2)));
    assert!(dispatcher.has_drag_target());
    root.borrow_mut().remove_child(child.borrow().id());
    drop(child);
    assert!(
        !dispatcher.has_drag_target(),
        "the drag target is held weakly"
    );
    log.lock().unwrap().clear();
    assert!(!dispatcher.dispatch_event(&root, &mut drop_at(20, 20)));
    assert!(log.lock().unwrap().is_empty());
}
