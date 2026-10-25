// SPDX-License-Identifier: LGPL-3.0-only
//! Input events reach the object filters and the application filters before the widget, in Qt's
//! order (`QCoreApplicationPrivate::sendThroughApplicationEventFilters`, then
//! `sendThroughObjectEventFilters`, last installed first; qcoreapplication.cpp:1237-1268).
//! Covered: Wheel, ContextMenu, KeyPress, MouseMove, MouseButtonPress, Enter/Leave, a consuming
//! filter, and `WA_UnderMouse` being set between the application and the object filters
//! (qapplication.cpp:3246-3254).

use ferrokit_core::application::CoreApplication;
use ferrokit_core::event::{ContextMenuReason, Event, EventKind, MouseButtons};
use ferrokit_core::object::{register_owned_qobject, ObjectData, ObjectId, OwnedObject, QObject};
use ferrokit_gui::geometry::primitives::{Point, Rect};
use ferrokit_widgets::hit_test::EventTreeDispatcher;
use ferrokit_widgets::widget::{EmptyWidget, Widget, WidgetRef};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

static LOCK: Mutex<()> = Mutex::new(());

type Log = Arc<Mutex<Vec<String>>>;

fn name(kind: &EventKind) -> &'static str {
    match kind {
        EventKind::Wheel { .. } => "Wheel",
        EventKind::ContextMenu { .. } => "ContextMenu",
        EventKind::KeyPress { .. } => "KeyPress",
        EventKind::MouseMove { .. } => "MouseMove",
        EventKind::MouseButtonPress { .. } => "Press",
        EventKind::Enter { .. } => "Enter",
        EventKind::Leave => "Leave",
        _ => "other",
    }
}

/// A filter that logs `label:Kind` and consumes the kinds in `consume`.
struct Spy {
    data: ObjectData,
    label: &'static str,
    log: Log,
    consume: Vec<&'static str>,
}

impl QObject for Spy {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
    }
    fn event(&mut self, _event: &mut Event) -> bool {
        false
    }
    fn event_filter(&mut self, _watched: ObjectId, event: &mut Event) -> bool {
        let kind = name(&event.kind);
        if kind == "other" {
            return false;
        }
        self.log.lock().unwrap().push(format!("{}:{}", self.label, kind));
        self.consume.contains(&kind)
    }
}

fn spy(
    label: &'static str,
    log: &Log,
    consume: &[&'static str],
) -> (ObjectId, OwnedObject<Spy>) {
    let s = Spy {
        data: ObjectData::with_auto_id(),
        label,
        log: Arc::clone(log),
        consume: consume.to_vec(),
    };
    // SAFETY: test-only; the owner stays on this thread for the whole test and nothing touches
    // the spy while one of its callbacks runs.
    let owner = unsafe { register_owned_qobject(s) };
    (OwnedObject::object_id(&owner), owner)
}

fn widget() -> WidgetRef {
    Rc::new(RefCell::new(Box::new(EmptyWidget::new())))
}

/// root 200x200 with one child at (10,10) 100x100, mouse tracking on so buttonless moves are
/// delivered to it.
fn tree() -> (WidgetRef, WidgetRef) {
    let root = widget();
    root.borrow().set_geometry(Rect::new(0, 0, 200, 200));
    let child = widget();
    child.borrow().set_geometry(Rect::new(10, 10, 100, 100));
    child.borrow().widget_base().set_mouse_tracking(true);
    root.borrow_mut().add_child(child.clone());
    child
        .borrow()
        .set_parent_widget(Some(Rc::downgrade(&root)));
    (root, child)
}

fn install(widget: &WidgetRef, filter: ObjectId) {
    widget.borrow_mut().object_data_mut().install_event_filter(filter);
}

fn take(log: &Log) -> Vec<String> {
    std::mem::take(&mut *log.lock().unwrap())
}

fn wheel(x: i32, y: i32) -> Event {
    Event::new_spontaneous(EventKind::Wheel {
        x,
        y,
        pixel_delta_x: 0,
        pixel_delta_y: 120,
        angle_delta_x: 0,
        angle_delta_y: 120,
        modifiers: 0,
    })
}

#[test]
fn application_filters_run_before_object_filters_and_last_installed_object_filter_first() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let log: Log = Arc::default();
    let (root, child) = tree();
    let (app_id, _a) = spy("app", &log, &[]);
    let (first_id, _f) = spy("first", &log, &[]);
    let (second_id, _s) = spy("second", &log, &[]);
    CoreApplication::install_event_filter(app_id);
    install(&child, first_id);
    install(&child, second_id);

    let mut d = EventTreeDispatcher::new();
    d.dispatch_event(&root, &mut wheel(20, 20));
    CoreApplication::remove_event_filter(app_id);

    // The child gets the wheel first, then the unhandled event goes to the root: the application
    // filter sees both deliveries, the child's own filters only theirs, newest first.
    assert_eq!(
        take(&log),
        [
            "app:Wheel",
            "second:Wheel",
            "first:Wheel",
            "app:Wheel", // the same event offered to the parent
        ]
    );
}

#[test]
fn filters_see_context_menu_press_and_tracking_move() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let log: Log = Arc::default();
    let (root, child) = tree();
    let (id, _s) = spy("f", &log, &[]);
    install(&child, id);

    let mut d = EventTreeDispatcher::new();
    d.dispatch_event(
        &root,
        &mut Event::new_spontaneous(EventKind::ContextMenu {
            x: 20,
            y: 20,
            global_x: 20,
            global_y: 20,
            reason: ContextMenuReason::Mouse,
        }),
    );
    d.dispatch_event(
        &root,
        &mut Event::new_spontaneous(EventKind::MouseButtonPress {
            x: 20,
            y: 20,
            button: 1,
        }),
    );
    d.dispatch_mouse_move(&root, Point::new(30, 30), Point::new(30, 30), MouseButtons(0));

    let seen = take(&log);
    assert!(seen.contains(&"f:ContextMenu".to_string()), "{seen:?}");
    assert!(seen.contains(&"f:Press".to_string()), "{seen:?}");
    assert!(seen.contains(&"f:MouseMove".to_string()), "{seen:?}");
}

#[test]
fn consuming_filter_hides_the_event_from_the_widget_and_stops_propagation() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let log: Log = Arc::default();
    let (root, child) = tree();
    let (child_filter, _c) = spy("child", &log, &["Wheel"]);
    let (root_filter, _r) = spy("root", &log, &[]);
    install(&child, child_filter);
    install(&root, root_filter);

    let mut d = EventTreeDispatcher::new();
    let handled = d.dispatch_event(&root, &mut wheel(20, 20));

    assert!(handled, "a consumed event counts as handled");
    // The root never sees it: a consumed, still accepted event does not propagate.
    assert_eq!(take(&log), ["child:Wheel"]);
}

#[test]
fn enter_and_leave_reach_filters_and_under_mouse_follows_the_cursor() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let log: Log = Arc::default();
    let (root, child) = tree();
    let (id, _s) = spy("f", &log, &[]);
    install(&child, id);

    let mut d = EventTreeDispatcher::new();
    assert!(!child.borrow().under_mouse());
    d.dispatch_mouse_move(&root, Point::new(20, 20), Point::new(20, 20), MouseButtons(0));
    assert!(child.borrow().under_mouse());
    assert!(root.borrow().under_mouse(), "the whole chain is under the mouse");
    d.dispatch_mouse_move(&root, Point::new(150, 150), Point::new(150, 150), MouseButtons(0));
    assert!(!child.borrow().under_mouse());
    assert!(root.borrow().under_mouse());
    d.handle_mouse_leave();
    assert!(!root.borrow().under_mouse());

    let seen = take(&log);
    assert!(seen.contains(&"f:Enter".to_string()), "{seen:?}");
    assert!(seen.contains(&"f:Leave".to_string()), "{seen:?}");
}

#[test]
fn an_application_filter_that_consumes_enter_keeps_under_mouse_unset() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let log: Log = Arc::default();
    let (root, child) = tree();
    let (app_id, _a) = spy("app", &log, &["Enter"]);
    CoreApplication::install_event_filter(app_id);

    let mut d = EventTreeDispatcher::new();
    d.dispatch_mouse_move(&root, Point::new(20, 20), Point::new(20, 20), MouseButtons(0));
    CoreApplication::remove_event_filter(app_id);

    // `QApplicationPrivate::notify_helper` returns after the application filters, before it
    // toggles WA_UnderMouse.
    assert!(!child.borrow().under_mouse());
}

#[test]
fn key_press_goes_through_the_focus_widget_filters() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let log: Log = Arc::default();
    let (root, child) = tree();
    child
        .borrow()
        .set_focus_policy(ferrokit_widgets::focus::FocusPolicy::StrongFocus);
    let (id, _s) = spy("f", &log, &["KeyPress"]);
    install(&child, id);

    let mut d = EventTreeDispatcher::new();
    let child_id = child.borrow().id();
    d.focus_manager_mut()
        .set_focus(&root, Some(child_id), ferrokit_core::event::FocusReason::Other);
    let mut ev = Event::new_spontaneous(EventKind::KeyPress {
        key: 0x41,
        modifiers: 0,
        is_repeat: false,
    });
    assert!(d.dispatch_event(&root, &mut ev));
    assert_eq!(take(&log), ["f:KeyPress"]);
}
