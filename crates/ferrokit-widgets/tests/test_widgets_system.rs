// SPDX-License-Identifier: LGPL-3.0-only
use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::event_loop::EventLoop;
use ferrokit_gui::geometry::primitives::{Margins, Point, Rect};
use ferrokit_gui::tiny_skia::Color;
use ferrokit_platform::WindowFlags;
use ferrokit_widgets::*;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[test]
fn test_widget_hierarchy_and_geometry() {
    let parent: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::with_geometry(
        Rect::new(0, 0, 400, 300),
    ))));

    let child1: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::with_geometry(
        Rect::new(10, 10, 100, 50),
    ))));

    let child2: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::with_geometry(
        Rect::new(120, 10, 100, 50),
    ))));

    parent.borrow_mut().add_child(Rc::clone(&child1));
    parent.borrow_mut().add_child(Rc::clone(&child2));

    assert_eq!(parent.borrow().children().len(), 2);
    assert_eq!(child1.borrow().geometry(), Rect::new(10, 10, 100, 50));
    assert_eq!(child2.borrow().geometry(), Rect::new(120, 10, 100, 50));

    let child1_id = child1.borrow().id();
    parent.borrow_mut().remove_child(child1_id);
    assert_eq!(parent.borrow().children().len(), 1);
}

#[test]
fn test_vbox_and_hbox_layout_calculation() {
    let _parent: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::with_geometry(
        Rect::new(0, 0, 200, 400),
    ))));

    let item_a: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::new())));
    let item_b: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::new())));

    let mut vbox = BoxLayout::vertical();
    vbox.set_margins(Margins::new(10, 10, 10, 10));
    vbox.set_spacing(10);
    vbox.add_widget(Rc::clone(&item_a));
    vbox.add_widget(Rc::clone(&item_b));

    vbox.set_geometry(Rect::new(0, 0, 200, 400));

    assert_eq!(item_a.borrow().geometry(), Rect::new(10, 10, 180, 185));
    assert_eq!(item_b.borrow().geometry(), Rect::new(10, 205, 180, 185));

    let item_x: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::new())));
    let item_y: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::new())));

    let mut hbox = BoxLayout::horizontal();
    hbox.set_margins(Margins::new(0, 0, 0, 0));
    hbox.set_spacing(0);
    hbox.add_widget_with_stretch(Rc::clone(&item_x), 1);
    hbox.add_widget_with_stretch(Rc::clone(&item_y), 2);

    hbox.set_geometry(Rect::new(0, 0, 300, 100));

    assert_eq!(item_x.borrow().geometry(), Rect::new(0, 0, 100, 100));
    assert_eq!(item_y.borrow().geometry(), Rect::new(100, 0, 200, 100));
}

#[test]
fn test_widget_hit_test_and_event_dispatch() {
    let parent: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::with_geometry(
        Rect::new(0, 0, 500, 400),
    ))));

    let button: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::with_geometry(
        Rect::new(50, 50, 200, 100),
    ))));

    parent.borrow_mut().add_child(Rc::clone(&button));

    let click_pos = Point::new(70, 80);
    let hit_res = hit_test(&parent, click_pos);
    assert!(hit_res.is_some());

    let (target, local_pos) = hit_res.unwrap();
    assert_eq!(target.borrow().id(), button.borrow().id());
    assert_eq!(local_pos, Point::new(20, 30));

    let background_pos = Point::new(10, 10);
    let (target_bg, local_bg) = hit_test(&parent, background_pos).unwrap();
    assert_eq!(target_bg.borrow().id(), parent.borrow().id());
    assert_eq!(local_bg, Point::new(10, 10));

    let outside_pos = Point::new(-5, 50);
    assert!(hit_test(&parent, outside_pos).is_none());
}

#[test]
fn test_top_level_window_rendering() {
    let mut win = Window::new(
        "Ferrokit Widgets Render Window",
        Rect::new(100, 100, 300, 200),
        WindowFlags::FRAMELESS | WindowFlags::LAYERED,
    )
    .expect("failed to create top-level window");

    let root = win.root_widget();
    let child: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::with_geometry(
        Rect::new(20, 20, 100, 60),
    ))));
    if let Some(empty) = child
        .borrow_mut()
        .as_any_mut()
        .downcast_mut::<EmptyWidget>()
    {
        empty.set_background_color(Some(Color::from_rgba8(0, 120, 255, 255)));
    }

    root.borrow_mut().add_child(child);

    win.set_stays_on_top(true);
    win.set_stays_on_top(false);
    win.set_click_through(true);
    win.set_click_through(false);
    win.start_system_drag();

    win.render_and_present();
    assert_eq!(win.geometry(), Rect::new(100, 100, 300, 200));
}

#[test]
fn test_asynchronous_update_request_and_event_loop_compression() {
    use ferrokit_core::object::register_owned_qobject;

    let el = EventLoop::new();

    let win = Window::new(
        "Async Update Test Window",
        Rect::new(50, 50, 250, 150),
        WindowFlags::FRAMELESS | WindowFlags::LAYERED,
    )
    .expect("failed to create top-level window");

    // SAFETY: test-only; the owner outlives this scope on this thread, and nothing touches the
    // window while a callback of it runs.
    let boxed_win = unsafe { register_owned_qobject(win) };

    let root = boxed_win
        .with(|w| w.root_widget())
        .expect("the window is not in a callback");
    let child1: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::with_geometry(
        Rect::new(10, 10, 80, 40),
    ))));
    let child2: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::with_geometry(
        Rect::new(100, 10, 80, 40),
    ))));

    root.borrow_mut().add_child(Rc::clone(&child1));
    root.borrow_mut().add_child(Rc::clone(&child2));

    child1.borrow_mut().update();
    child1.borrow_mut().update();
    child2.borrow_mut().update();
    root.borrow_mut().update();

    let queue_len = el.queue().lock().unwrap().len();
    assert_eq!(queue_len, 1);

    let had_events = el.process_events(false);
    assert!(had_events);
    // UpdateRequest now feeds the window's deferred-render gate: the render itself runs on the
    // next turn. It must settle (no ping-pong) within a couple of turns.
    for _ in 0..4 {
        if el.queue().lock().unwrap().is_empty() {
            break;
        }
        el.process_events(false);
    }
    assert_eq!(el.queue().lock().unwrap().len(), 0);
}

struct HoverButton {
    base: WidgetBase,
    is_hovered: bool,
    events_log: Rc<RefCell<Vec<String>>>,
}

impl HoverButton {
    pub fn new(geometry: Rect, events_log: Rc<RefCell<Vec<String>>>) -> Self {
        Self {
            base: WidgetBase::with_geometry(geometry),
            is_hovered: false,
            events_log,
        }
    }
}

impl ferrokit_core::object::QObject for HoverButton {
    fn object_data(&self) -> &ferrokit_core::object::ObjectData {
        &self.base.object_data
    }

    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.base.object_data)
    }

    fn event(&mut self, event: &mut Event) -> bool {
        match &event.kind {
            EventKind::Enter { x, y } => {
                self.is_hovered = true;
                self.events_log
                    .borrow_mut()
                    .push(format!("Enter({}, {})", x, y));
                true
            }
            EventKind::Leave => {
                self.is_hovered = false;
                self.events_log.borrow_mut().push("Leave".to_string());
                true
            }
            EventKind::MouseMove { x, y } => {
                self.events_log
                    .borrow_mut()
                    .push(format!("Move({}, {})", x, y));
                true
            }
            _ => false,
        }
    }
}

impl Widget for HoverButton {
    fn widget_base(&self) -> &ferrokit_widgets::widget::WidgetBase {
        &self.base
    }

    fn id(&self) -> ferrokit_core::object::ObjectId {
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

    fn window_id(&self) -> Option<ferrokit_core::object::ObjectId> {
        self.base.window_id()
    }

    fn set_window_id(&self, window_id: Option<ferrokit_core::object::ObjectId>) {
        self.base.set_window_id(window_id);
    }

    fn children(&self) -> Vec<WidgetRef> {
        self.base.children()
    }

    fn add_child(&mut self, child: WidgetRef) {
        self.base.children.borrow_mut().push(child);
    }

    fn remove_child(&mut self, child_id: ferrokit_core::object::ObjectId) {
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
}

#[test]
fn test_hover_enter_leave_events_transition() {
    let parent: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::with_geometry(
        Rect::new(0, 0, 500, 300),
    ))));

    let btn_log = Rc::new(RefCell::new(Vec::<String>::new()));
    let btn: WidgetRef = Rc::new(RefCell::new(Box::new(HoverButton::new(
        Rect::new(50, 50, 100, 50),
        Rc::clone(&btn_log),
    ))));

    parent.borrow_mut().add_child(Rc::clone(&btn));
    ferrokit_widgets::widget::adopt_tree(&parent);
    // Buttonless moves reach a widget only when it has mouse tracking (qapplication.cpp:2746-2750).
    btn.borrow().widget_base().set_mouse_tracking(true);

    let mut dispatcher = EventTreeDispatcher::new();

    let mut ev1 = Event::new_spontaneous(EventKind::MouseMove { x: 10, y: 10 });
    dispatcher.dispatch_event(&parent, &mut ev1);
    assert_eq!(*btn_log.borrow(), Vec::<String>::new());

    let mut ev2 = Event::new_spontaneous(EventKind::MouseMove { x: 60, y: 60 });
    dispatcher.dispatch_event(&parent, &mut ev2);
    assert_eq!(*btn_log.borrow(), vec!["Enter(10, 10)", "Move(10, 10)"]);

    let mut ev3 = Event::new_spontaneous(EventKind::MouseMove { x: 80, y: 70 });
    dispatcher.dispatch_event(&parent, &mut ev3);
    assert_eq!(
        *btn_log.borrow(),
        vec!["Enter(10, 10)", "Move(10, 10)", "Move(30, 20)"]
    );

    let mut ev4 = Event::new_spontaneous(EventKind::MouseMove { x: 10, y: 10 });
    dispatcher.dispatch_event(&parent, &mut ev4);
    assert_eq!(
        *btn_log.borrow(),
        vec!["Enter(10, 10)", "Move(10, 10)", "Move(30, 20)", "Leave"]
    );

    let mut ev5 = Event::new_spontaneous(EventKind::MouseMove { x: 90, y: 80 });
    dispatcher.dispatch_event(&parent, &mut ev5);
    assert_eq!(btn_log.borrow().last().unwrap(), "Move(40, 30)");
    assert_eq!(
        btn_log.borrow()[btn_log.borrow().len() - 2],
        "Enter(40, 30)"
    );

    dispatcher.handle_mouse_leave();
    assert_eq!(btn_log.borrow().last().unwrap(), "Leave");
}

#[test]
fn test_builtin_label_widget() {
    let mut label = Label::new("HUD CPU 88% 🔥");
    assert_eq!(label.text(), "HUD CPU 88% 🔥");
    assert_eq!(label.alignment(), Alignment::LEFT | Alignment::V_CENTER);

    label.set_alignment(Alignment::CENTER);
    assert_eq!(label.alignment(), Alignment::CENTER);

    label.set_color(Color::from_rgba8(255, 100, 0, 255));
    assert_eq!(label.color(), Color::from_rgba8(255, 100, 0, 255));

    let hint = label.size_hint();
    assert!(hint.width > 0);
    assert!(hint.height > 0);
}

#[test]
fn test_builtin_button_click_and_state_transition() {
    let parent: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::with_geometry(
        Rect::new(0, 0, 400, 300),
    ))));

    let btn: WidgetRef = Rc::new(RefCell::new(Box::new(Button::new("Confirm"))));
    btn.borrow_mut().set_geometry(Rect::new(50, 50, 120, 36));

    let clicked_flag = Arc::new(AtomicBool::new(false));
    let flag_clone = Arc::clone(&clicked_flag);
    let _conn = if let Some(b) = btn.borrow_mut().as_any_mut().downcast_mut::<Button>() {
        Some(b.clicked.connect(move |()| {
            flag_clone.store(true, Ordering::SeqCst);
        }))
    } else {
        None
    };

    parent.borrow_mut().add_child(Rc::clone(&btn));
    ferrokit_widgets::widget::adopt_tree(&parent);

    let mut dispatcher = EventTreeDispatcher::new();

    let mut move_ev = Event::new_spontaneous(EventKind::MouseMove { x: 70, y: 70 });
    dispatcher.dispatch_event(&parent, &mut move_ev);
    if let Some(b) = btn.borrow().as_any().downcast_ref::<Button>() {
        assert_eq!(b.state(), ButtonState::Hovered);
    }

    let mut press_ev = Event::new_spontaneous(EventKind::MouseButtonPress {
        x: 70,
        y: 70,
        button: 1,
    });
    dispatcher.dispatch_event(&parent, &mut press_ev);
    if let Some(b) = btn.borrow().as_any().downcast_ref::<Button>() {
        assert_eq!(b.state(), ButtonState::Pressed);
    }

    let mut release_ev = Event::new_spontaneous(EventKind::MouseButtonRelease {
        x: 70,
        y: 70,
        button: 1,
    });
    dispatcher.dispatch_event(&parent, &mut release_ev);
    if let Some(b) = btn.borrow().as_any().downcast_ref::<Button>() {
        assert_eq!(b.state(), ButtonState::Hovered);
    }
    assert!(clicked_flag.load(Ordering::SeqCst));
}

#[test]
fn test_dirty_region_culling_skips_non_intersecting_widgets() {
    struct TestWidget {
        base: crate::widget::WidgetBase,
        painted: Arc<AtomicBool>,
    }
    impl TestWidget {
        fn new(geom: Rect, painted: Arc<AtomicBool>) -> Self {
            Self {
                base: crate::widget::WidgetBase::with_geometry(geom),
                painted,
            }
        }
    }
    impl ferrokit_core::object::QObject for TestWidget {
        fn object_data(&self) -> &ferrokit_core::object::ObjectData {
            &self.base.object_data
        }
        fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
            ferrokit_core::object::ObjectDataMut::new(&mut self.base.object_data)
        }
    }
    impl Widget for TestWidget {
        fn widget_base(&self) -> &ferrokit_widgets::widget::WidgetBase {
            &self.base
        }

        fn id(&self) -> ferrokit_core::object::ObjectId {
            self.base.object_data.id()
        }
        fn is_visible(&self) -> bool {
            self.base.is_visible()
        }
        fn set_visible(&self, v: bool) {
            self.base.set_visible(v);
        }
        fn is_enabled(&self) -> bool {
            true
        }
        fn geometry(&self) -> Rect {
            self.base.geometry()
        }
        fn set_geometry(&self, r: Rect) {
            self.base.set_geometry(r);
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
        fn set_layout(&mut self, _: Box<dyn Layout>) {}
        fn parent_widget(&self) -> Option<WidgetWeak> {
            None
        }
        fn set_parent_widget(&self, _: Option<WidgetWeak>) {}
        fn window_id(&self) -> Option<ferrokit_core::object::ObjectId> {
            None
        }
        fn set_window_id(&self, _: Option<ferrokit_core::object::ObjectId>) {}
        fn children(&self) -> Vec<WidgetRef> {
            Vec::new()
        }
        fn add_child(&mut self, _: WidgetRef) {}
        fn remove_child(&mut self, _: ferrokit_core::object::ObjectId) {}
        fn paint_event(&mut self, _: &mut ferrokit_gui::paint::Painter) {
            self.painted.store(true, Ordering::SeqCst);
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
    }

    let mut win = Window::new(
        "Dirty Culling Test",
        Rect::new(0, 0, 400, 200),
        ferrokit_platform::WindowFlags::FRAMELESS,
    )
    .unwrap();

    let root = win.root_widget();
    let painted1 = Arc::new(AtomicBool::new(false));
    let painted2 = Arc::new(AtomicBool::new(false));

    let w1: WidgetRef = Rc::new(RefCell::new(Box::new(TestWidget::new(
        Rect::new(10, 10, 50, 50),
        Arc::clone(&painted1),
    ))));
    let w2: WidgetRef = Rc::new(RefCell::new(Box::new(TestWidget::new(
        Rect::new(200, 10, 50, 50),
        Arc::clone(&painted2),
    ))));

    root.borrow_mut().add_child(Rc::clone(&w1));
    root.borrow_mut().add_child(Rc::clone(&w2));

    // First render draws everything because root was dirty
    win.render_and_present();
    assert!(painted1.load(Ordering::SeqCst));
    assert!(painted2.load(Ordering::SeqCst));

    // Reset flags
    painted1.store(false, Ordering::SeqCst);
    painted2.store(false, Ordering::SeqCst);

    // Mark ONLY w1 dirty
    w1.borrow_mut().update();

    // Render again
    win.render_and_present();

    // w1 MUST be repainted, w2 MUST BE SKIPPED!
    assert!(painted1.load(Ordering::SeqCst), "w1 should be repainted");
    assert!(
        !painted2.load(Ordering::SeqCst),
        "w2 should NOT be repainted (culled)"
    );
}
