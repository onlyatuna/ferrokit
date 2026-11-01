// SPDX-License-Identifier: LGPL-3.0-only
//! Widget helpers: `underMouse`, `raise` / `lower`, `ensurePolished`, `isAncestorOf`, `mapToGlobal`,
//! `grabMouse`, accessible name / description, `pos` / `size` / `geometry` / `windowOpacity` as
//! animation targets, the installed layout reached through `as_any_mut`, a style sheet
//! background with `:hover` on a plain widget, and the motion preference.

#[path = "support/headless.rs"]
mod headless;

use ferrokit_core::animation::{AnimationState, PropertyAnimation};
use ferrokit_core::event::{Event, EventKind, MouseButtons};
use ferrokit_core::object::{ObjectData, ObjectId, OwnedObject, QObject, register_owned_qobject};
use ferrokit_core::variant::Variant;
use ferrokit_gui::application::{GuiApplication, MotionPreference, set_system_motion_preference};
use ferrokit_gui::geometry::primitives::{Point, Rect};
use ferrokit_gui::paint::{Painter, Pixmap};
use ferrokit_platform::WindowFlags;
use ferrokit_widgets::accessibility::{AccessibleId, AccessibleTree};
use ferrokit_widgets::button::Button;
use ferrokit_widgets::hit_test::{EventTreeDispatcher, hit_path};
use ferrokit_widgets::layout::{BoxLayout, GridLayout, Layout};
use ferrokit_widgets::widget::{EmptyWidget, Widget, WidgetRef};
use ferrokit_widgets::window::{Window, deliver_paint_event};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

fn widget_ref(w: impl Widget + 'static) -> WidgetRef {
    Rc::new(RefCell::new(Box::new(w)))
}

fn plain(rect: Rect) -> WidgetRef {
    let w = EmptyWidget::new();
    w.set_geometry(rect);
    widget_ref(w)
}

fn attach(parent: &WidgetRef, child: &WidgetRef) {
    parent.borrow_mut().add_child(child.clone());
    child
        .borrow()
        .set_parent_widget(Some(Rc::downgrade(parent)));
}

type Log = Arc<Mutex<Vec<String>>>;

/// Filter logging `<label>:<Polish|ChildPolished|ZOrderChange>`.
struct Spy {
    data: ObjectData,
    label: &'static str,
    log: Log,
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
        let kind = match event.kind {
            EventKind::Polish => "Polish",
            EventKind::ChildPolished { .. } => "ChildPolished",
            EventKind::ZOrderChange => "ZOrderChange",
            _ => return false,
        };
        self.log
            .lock()
            .unwrap()
            .push(format!("{}:{}", self.label, kind));
        false
    }
}

fn watch(w: &WidgetRef, label: &'static str, log: &Log) -> OwnedObject<Spy> {
    let spy = Spy {
        data: ObjectData::with_auto_id(),
        label,
        log: Arc::clone(log),
    };
    // SAFETY: test-only; the owner stays on this thread for the whole test and nothing touches
    // the spy while one of its callbacks runs.
    let owner = unsafe { register_owned_qobject(spy) };
    w.borrow_mut()
        .object_data_mut()
        .install_event_filter(OwnedObject::object_id(&owner));
    owner
}

fn take(log: &Log) -> Vec<String> {
    std::mem::take(&mut *log.lock().unwrap())
}

fn top_at(root: &WidgetRef, p: Point) -> ObjectId {
    hit_path(root, p).last().unwrap().widget.borrow().id()
}

#[test]
fn raise_and_lower_reorder_siblings_and_send_z_order_change() {
    let root = plain(Rect::new(0, 0, 200, 200));
    let a = plain(Rect::new(0, 0, 100, 100));
    let b = plain(Rect::new(0, 0, 100, 100));
    let c = plain(Rect::new(0, 0, 100, 100));
    for w in [&a, &b, &c] {
        attach(&root, w);
    }
    let (a_id, b_id, c_id) = (a.borrow().id(), b.borrow().id(), c.borrow().id());
    let log: Log = Arc::default();
    let _spy = watch(&a, "a", &log);

    assert_eq!(top_at(&root, Point::new(10, 10)), c_id);
    a.borrow().raise();
    assert_eq!(top_at(&root, Point::new(10, 10)), a_id);
    assert_eq!(take(&log), ["a:ZOrderChange"]);
    // Already on top: nothing changes, nothing is sent.
    a.borrow().raise();
    assert!(take(&log).is_empty());

    a.borrow().lower();
    assert_eq!(top_at(&root, Point::new(10, 10)), c_id);
    c.borrow().lower();
    assert_eq!(top_at(&root, Point::new(10, 10)), b_id);
}

#[test]
fn raise_works_while_the_widget_itself_is_borrowed_mutably() {
    let root = plain(Rect::new(0, 0, 200, 200));
    let a = plain(Rect::new(0, 0, 100, 100));
    let b = plain(Rect::new(0, 0, 100, 100));
    attach(&root, &a);
    attach(&root, &b);
    let a_id = a.borrow().id();

    // As in `a`'s own event handler: `&mut self` is live.
    a.borrow_mut().raise();
    assert_eq!(top_at(&root, Point::new(10, 10)), a_id);
}

#[test]
fn ensure_polished_sends_polish_once_children_after_parent_then_child_polished() {
    let root = plain(Rect::new(0, 0, 200, 200));
    let child = plain(Rect::new(0, 0, 50, 50));
    attach(&root, &child);
    let log: Log = Arc::default();
    let _r = watch(&root, "root", &log);
    let _c = watch(&child, "child", &log);

    root.borrow().ensure_polished();
    assert_eq!(
        take(&log),
        ["root:Polish", "child:Polish", "root:ChildPolished"]
    );

    root.borrow().ensure_polished();
    child.borrow().ensure_polished();
    assert!(take(&log).is_empty(), "polish runs once");
}

#[test]
fn is_ancestor_of_follows_the_parent_chain_and_excludes_self() {
    let root = plain(Rect::new(0, 0, 200, 200));
    let mid = plain(Rect::new(0, 0, 100, 100));
    let leaf = plain(Rect::new(0, 0, 10, 10));
    let stranger = plain(Rect::new(0, 0, 10, 10));
    attach(&root, &mid);
    attach(&mid, &leaf);

    assert!(root.borrow().is_ancestor_of(&**leaf.borrow()));
    assert!(mid.borrow().is_ancestor_of(&**leaf.borrow()));
    assert!(!leaf.borrow().is_ancestor_of(&**mid.borrow()));
    assert!(!mid.borrow().is_ancestor_of(&**mid.borrow()));
    assert!(!stranger.borrow().is_ancestor_of(&**leaf.borrow()));
    assert!(!root.borrow().is_ancestor_of(&**stranger.borrow()));
}

#[test]
fn map_to_and_from_parent_and_global() {
    let _g = headless::install();
    let mut win = Window::new("map", Rect::new(100, 50, 300, 200), WindowFlags::FRAMELESS).unwrap();
    let root = plain(Rect::new(0, 0, 300, 200));
    let mid = plain(Rect::new(20, 30, 100, 100));
    let leaf = plain(Rect::new(5, 6, 10, 10));
    attach(&root, &mid);
    attach(&mid, &leaf);
    win.set_root_widget(root.clone());

    assert_eq!(
        leaf.borrow().map_to_parent(Point::new(1, 1)),
        Point::new(6, 7)
    );
    assert_eq!(
        leaf.borrow().map_from_parent(Point::new(6, 7)),
        Point::new(1, 1)
    );
    // window origin (100,50) + mid (20,30) + leaf (5,6) + point (1,1)
    assert_eq!(
        leaf.borrow().map_to_global(Point::new(1, 1)),
        Point::new(126, 87)
    );
    assert_eq!(
        leaf.borrow().map_from_global(Point::new(126, 87)),
        Point::new(1, 1)
    );
    assert_eq!(
        root.borrow().map_to_global(Point::new(0, 0)),
        Point::new(100, 50)
    );
}

#[test]
fn grab_mouse_redirects_input_until_release() {
    let root = plain(Rect::new(0, 0, 200, 200));
    let grabber = plain(Rect::new(0, 0, 40, 40));
    let other = plain(Rect::new(100, 100, 50, 50));
    attach(&root, &grabber);
    attach(&root, &other);
    let log: Log = Arc::default();
    let _filter = {
        let spy = Spy2::new(&log);
        spy.install(&grabber, "grabber");
        spy.install(&other, "other");
        spy
    };

    let mut d = EventTreeDispatcher::new();
    let press = || {
        Event::new_spontaneous(EventKind::MouseButtonPress {
            x: 120,
            y: 120,
            button: 1,
        })
    };
    d.dispatch_event(&root, &mut press());
    d.dispatch_event(
        &root,
        &mut Event::new_spontaneous(EventKind::MouseButtonRelease {
            x: 120,
            y: 120,
            button: 1,
        }),
    );
    assert_eq!(take(&log), ["other:Press", "other:Release"]);

    grabber.borrow().grab_mouse();
    assert!(grabber.borrow().is_mouse_grabber());
    d.dispatch_event(&root, &mut press());
    d.dispatch_event(
        &root,
        &mut Event::new_spontaneous(EventKind::MouseButtonRelease {
            x: 120,
            y: 120,
            button: 1,
        }),
    );
    assert_eq!(take(&log), ["grabber:Press", "grabber:Release"]);

    // Only the holder can release.
    other.borrow().release_mouse();
    assert!(grabber.borrow().is_mouse_grabber());
    grabber.borrow().release_mouse();
    assert!(!grabber.borrow().is_mouse_grabber());
    d.dispatch_event(&root, &mut press());
    assert_eq!(take(&log), ["other:Press"]);
    let _ = MouseButtons(0);
}

/// Filter logging press / release per labelled widget.
struct Spy2 {
    ids: RefCell<Vec<OwnedObject<Spy2Inner>>>,
    log: Log,
}

struct Spy2Inner {
    data: ObjectData,
    label: &'static str,
    log: Log,
}

impl QObject for Spy2Inner {
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
        let kind = match event.kind {
            EventKind::MouseButtonPress { .. } => "Press",
            EventKind::MouseButtonRelease { .. } => "Release",
            _ => return false,
        };
        self.log
            .lock()
            .unwrap()
            .push(format!("{}:{}", self.label, kind));
        false
    }
}

impl Spy2 {
    fn new(log: &Log) -> Self {
        Self {
            ids: RefCell::new(Vec::new()),
            log: Arc::clone(log),
        }
    }

    fn install(&self, w: &WidgetRef, label: &'static str) {
        let inner = Spy2Inner {
            data: ObjectData::with_auto_id(),
            label,
            log: Arc::clone(&self.log),
        };
        // SAFETY: as in `watch`.
        let owner = unsafe { register_owned_qobject(inner) };
        w.borrow_mut()
            .object_data_mut()
            .install_event_filter(OwnedObject::object_id(&owner));
        self.ids.borrow_mut().push(owner);
    }
}

#[test]
fn accessible_name_and_description_reach_the_accessibility_tree() {
    let root = plain(Rect::new(0, 0, 200, 200));
    let button = widget_ref(Button::new("OK"));
    attach(&root, &button);

    let tree = AccessibleTree::from_widget(&root);
    let id = AccessibleId(button.borrow().id().0);
    assert_eq!(
        tree.node(id).unwrap().name,
        "OK",
        "a button names itself by its text"
    );

    button.borrow().set_accessible_name("Confirm");
    button.borrow().set_accessible_description("Saves the form");
    assert_eq!(button.borrow().accessible_name(), "Confirm");
    assert_eq!(button.borrow().accessible_description(), "Saves the form");
    let tree = AccessibleTree::from_widget(&root);
    let node = tree.node(id).unwrap();
    assert_eq!(node.name, "Confirm");
    assert_eq!(node.description, "Saves the form");

    // A plain widget: explicit name first, else the object name.
    let plain_id = AccessibleId(root.borrow().id().0);
    root.borrow_mut().object_data_mut().set_object_name("panel");
    assert_eq!(
        AccessibleTree::from_widget(&root)
            .node(plain_id)
            .unwrap()
            .name,
        "panel"
    );
    root.borrow().set_accessible_name("Main panel");
    assert_eq!(
        AccessibleTree::from_widget(&root)
            .node(plain_id)
            .unwrap()
            .name,
        "Main panel"
    );
}

#[test]
fn property_animation_drives_widget_pos_size_and_geometry() {
    let w = plain(Rect::new(0, 0, 100, 50));

    let mut pos = PropertyAnimation::new("pos");
    pos.animation_mut().set_duration(100);
    pos.animation_mut().set_start_value(Variant::Point(0, 0));
    pos.animation_mut().set_end_value(Variant::Point(100, 40));
    pos.animation_mut().start();
    assert!(pos.step_target(50, &mut **w.borrow_mut()));
    assert_eq!(
        w.borrow().geometry(),
        Rect::new(50, 20, 100, 50),
        "size is kept by `pos`"
    );
    assert!(!pos.step_target(50, &mut **w.borrow_mut()));
    assert_eq!(w.borrow().geometry(), Rect::new(100, 40, 100, 50));
    assert_eq!(pos.animation().state(), AnimationState::Stopped);

    let mut size = PropertyAnimation::new("size");
    size.animation_mut().set_duration(100);
    size.animation_mut().set_start_value(Variant::Size(100, 50));
    size.animation_mut().set_end_value(Variant::Size(200, 150));
    size.animation_mut().start();
    size.step_target(100, &mut **w.borrow_mut());
    assert_eq!(
        w.borrow().geometry(),
        Rect::new(100, 40, 200, 150),
        "position is kept by `size`"
    );

    let mut geo = PropertyAnimation::new("geometry");
    geo.animation_mut().set_duration(200);
    geo.animation_mut()
        .set_start_value(Variant::Rect(100, 40, 200, 150));
    geo.animation_mut()
        .set_end_value(Variant::Rect(0, 0, 20, 10));
    geo.animation_mut().start();
    geo.step_target(100, &mut **w.borrow_mut());
    assert_eq!(w.borrow().geometry(), Rect::new(50, 20, 110, 80));

    // Reading goes through the same names.
    let mut guard = w.borrow_mut();
    let obj: &mut dyn QObject = &mut **guard;
    assert_eq!(obj.property("pos"), Some(Variant::Point(50, 20)));
    assert_eq!(obj.property("size"), Some(Variant::Size(110, 80)));
    assert_eq!(
        obj.property("geometry"),
        Some(Variant::Rect(50, 20, 110, 80))
    );
}

#[test]
fn property_animation_drives_window_opacity_and_geometry() {
    let _g = headless::install();
    let mut win = Window::new("anim", Rect::new(10, 20, 300, 200), WindowFlags::FRAMELESS).unwrap();
    win.set_opacity(1.0);

    let mut fade = PropertyAnimation::new("windowOpacity");
    fade.animation_mut().set_duration(100);
    fade.animation_mut().set_start_value(Variant::F64(1.0));
    fade.animation_mut().set_end_value(Variant::F64(0.0));
    fade.animation_mut().start();
    fade.step_target(50, &mut win);
    assert!((win.opacity() - 0.5).abs() < 1e-6);
    assert_eq!(
        QObject::property(&win, "windowOpacity"),
        Some(Variant::F64(0.5))
    );
    fade.step_target(50, &mut win);
    assert_eq!(win.opacity(), 0.0);

    // `setWindowOpacity` bounds the level.
    assert!(QObject::set_property(
        &mut win,
        "windowOpacity",
        Variant::F64(7.0)
    ));
    assert_eq!(win.opacity(), 1.0);

    let mut mv = PropertyAnimation::new("pos");
    mv.animation_mut().set_duration(100);
    mv.animation_mut().set_start_value(Variant::Point(10, 20));
    mv.animation_mut().set_end_value(Variant::Point(110, 20));
    mv.animation_mut().start();
    mv.step_target(100, &mut win);
    assert_eq!(win.geometry(), Rect::new(110, 20, 300, 200));
}

#[test]
fn button_down_click_and_signals() {
    let log: Arc<Mutex<Vec<&'static str>>> = Arc::default();
    let mut b = Button::new("Go");
    for (sig, name) in [
        (&b.pressed, "pressed"),
        (&b.released, "released"),
        (&b.clicked, "clicked"),
    ] {
        let log = Arc::clone(&log);
        sig.connect(move |_| log.lock().unwrap().push(name));
    }

    assert!(!b.is_down());
    b.set_down(true);
    assert!(b.is_down());
    b.set_down(false);
    assert!(!b.is_down());
    assert!(log.lock().unwrap().is_empty(), "setDown sends no signal");

    b.click();
    assert!(!b.is_down());
    assert_eq!(*log.lock().unwrap(), ["pressed", "released", "clicked"]);

    log.lock().unwrap().clear();
    b.base
        .set_attribute(ferrokit_widgets::widget::WidgetAttribute::Disabled, true);
    b.click();
    assert!(
        log.lock().unwrap().is_empty(),
        "a disabled button ignores click()"
    );
}

#[test]
fn layout_reached_through_as_any_mut_and_item_access() {
    let root = plain(Rect::new(0, 0, 200, 200));
    root.borrow_mut()
        .set_layout(Box::new(BoxLayout::vertical()));
    let a = plain(Rect::new(0, 0, 10, 10));
    let b = plain(Rect::new(0, 0, 10, 10));
    let c = plain(Rect::new(0, 0, 10, 10));

    let n = root.borrow().with_box_layout(|l| {
        l.add_widget_with_stretch(a.clone(), 0);
        l.add_widget_with_stretch(c.clone(), 0);
        l.insert_widget(1, b.clone(), 1);
        l.set_spacing(7);
        l.set_margins(ferrokit_gui::geometry::primitives::Margins::new(1, 2, 3, 4));
        l.count()
    });
    assert_eq!(n, Some(3));

    let layout_state = root.borrow().layout_ref_mut().map(|l| {
        (
            l.count(),
            Rc::ptr_eq(&l.item_at(1).unwrap(), &b),
            l.spacing(),
            l.item_at(3).is_none(),
        )
    });
    assert_eq!(layout_state, Some((3, true, 7, true)));

    root.borrow()
        .with_box_layout(|l| l.set_direction(ferrokit_widgets::layout::Direction::LeftToRight));
    let dir = root.borrow().with_box_layout(|l| l.direction());
    assert_eq!(dir, Some(ferrokit_widgets::layout::Direction::LeftToRight));

    let taken = root.borrow().layout_ref_mut().unwrap().take_at(1);
    assert!(Rc::ptr_eq(&taken.unwrap(), &b));
    assert_eq!(root.borrow().layout_ref_mut().unwrap().count(), 2);
    assert!(root.borrow().layout_ref_mut().unwrap().take_at(9).is_none());

    // A layout of another type does not downcast.
    assert!(
        root.borrow()
            .with_layout_as::<GridLayout, _>(|_| ())
            .is_none()
    );
    // No layout at all.
    assert!(a.borrow().with_box_layout(|_| ()).is_none());
}

#[test]
fn style_sheet_background_and_hover_on_a_plain_widget() {
    let root = plain(Rect::new(0, 0, 40, 40));
    let cell = plain(Rect::new(0, 0, 40, 40));
    cell.borrow_mut()
        .object_data_mut()
        .set_object_name("RowCell");
    attach(&root, &cell);
    root.borrow().set_style_sheet(
        "QWidget#RowCell { background-color: #0000ff; } \
         QWidget#RowCell:hover { background-color: #ff0000; border-radius: 0px; }",
    );

    let paint = |w: &WidgetRef| {
        let mut pixmap = Pixmap::with_dpr(40, 40, 1.0).unwrap();
        {
            let mut painter = Painter::begin(&mut pixmap);
            deliver_paint_event(&mut **w.borrow_mut(), &mut painter, Rect::new(0, 0, 40, 40));
        }
        let p = pixmap.pixel(20, 20).unwrap();
        (p.red(), p.green(), p.blue())
    };
    assert_eq!(paint(&cell), (0, 0, 255), "styled background without hover");

    // Hover by delivering the real Enter through the dispatcher.
    let mut d = EventTreeDispatcher::new();
    d.dispatch_mouse_move(
        &root,
        Point::new(20, 20),
        Point::new(20, 20),
        MouseButtons(0),
    );
    assert!(cell.borrow().under_mouse());
    assert_eq!(
        paint(&cell),
        (255, 0, 0),
        ":hover background while under the mouse"
    );
    assert!(
        cell.borrow().widget_base().dirty_rect().is_some(),
        "Enter repaints a :hover widget"
    );

    d.handle_mouse_leave();
    assert_eq!(paint(&cell), (0, 0, 255));
}

#[test]
fn hover_without_a_hover_rule_does_not_repaint() {
    let root = plain(Rect::new(0, 0, 40, 40));
    let cell = plain(Rect::new(0, 0, 40, 40));
    attach(&root, &cell);
    root.borrow()
        .set_style_sheet("QWidget { background-color: #00ff00; }");
    cell.borrow().widget_base().clear_dirty();

    let mut d = EventTreeDispatcher::new();
    d.dispatch_mouse_move(
        &root,
        Point::new(20, 20),
        Point::new(20, 20),
        MouseButtons(0),
    );
    assert!(cell.borrow().under_mouse());
    assert!(cell.borrow().widget_base().dirty_rect().is_none());
}

#[test]
fn motion_preference_scales_animation_duration() {
    set_system_motion_preference(MotionPreference::ReducedMotion);
    assert_eq!(
        GuiApplication::motion_preference(),
        MotionPreference::ReducedMotion
    );
    assert_eq!(GuiApplication::animation_duration_factor(), 0.0);
    set_system_motion_preference(MotionPreference::NoPreference);
    assert_eq!(GuiApplication::animation_duration_factor(), 1.0);
}

#[test]
fn layout_access_while_the_layout_is_in_use_gives_none_instead_of_panicking() {
    let root = plain(Rect::new(0, 0, 200, 200));
    root.borrow_mut()
        .set_layout(Box::new(BoxLayout::vertical()));
    let frame = widget_ref(ferrokit_widgets::frame::Frame::new());
    frame
        .borrow_mut()
        .set_layout(Box::new(BoxLayout::vertical()));

    for w in [&root, &frame] {
        let inner = w.borrow().with_box_layout(|_outer| {
            // Re-entered from inside the first closure: the layout is mutably borrowed.
            (
                w.borrow().with_box_layout(|l| l.count()),
                w.borrow().layout_ref_mut().is_none(),
            )
        });
        assert_eq!(inner, Some((None, true)));
        // Released again afterwards.
        assert_eq!(w.borrow().with_box_layout(|l| l.count()), Some(0));
    }
}

#[test]
fn dropping_the_grabber_ends_the_grab() {
    let w = plain(Rect::new(0, 0, 10, 10));
    let id = w.borrow().id();
    w.borrow().grab_mouse();
    assert_eq!(ferrokit_widgets::widget::mouse_grabber_id(), Some(id));
    drop(w);
    assert_eq!(ferrokit_widgets::widget::mouse_grabber_id(), None);
}

#[test]
fn a_widget_that_is_borrowed_still_runs_its_filters_for_input() {
    let root = plain(Rect::new(0, 0, 200, 200));
    let child = plain(Rect::new(0, 0, 100, 100));
    attach(&root, &child);
    let log: Log = Arc::default();
    let spy = Spy2::new(&log);
    spy.install(&child, "child");

    // A shared borrow is live (e.g. the widget is painting): `event()` cannot run, the filters do.
    let _held = child.borrow();
    let mut d = EventTreeDispatcher::new();
    d.dispatch_event(
        &root,
        &mut Event::new_spontaneous(EventKind::MouseButtonPress {
            x: 5,
            y: 5,
            button: 1,
        }),
    );
    assert_eq!(take(&log), ["child:Press"]);
}
