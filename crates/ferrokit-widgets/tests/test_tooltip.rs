// SPDX-License-Identifier: LGPL-3.0-only
//! Tool tips (`QToolTip` and the application-level wake-up protocol).
//!
//! Qt starts a 700 ms wake-up timer when the cursor moves with no button down; when it fires and
//! the window is active (or has `WA_AlwaysShowToolTips`) a `QHelpEvent` goes to the widget and
//! bubbles to its parents until one accepts (`QApplication::notify`, qapplication.cpp:2712-2737,
//! 1710-1740, 2940-2962). `QWidget::event` shows the widget's own `toolTip` (qwidget.cpp:
//! 9381-9386). An accepted tip starts a 2 s fall-asleep timer during which the next wake-up takes
//! 20 ms. User input and `Leave` cancel (qapplication.cpp:2622-2637). The tip's position is
//! `QTipLabel::placeTip` (qtooltip.cpp:296-370) and its lifetime `10000 + 40 * max(0, len - 100)`
//! (qtooltip.cpp:117-126).
//!
//! Real `ferrokit_widgets::Window`s, real HWNDs, real timers and the event loop's message pump. The
//! tip appearance (colours, font, corners) is not asserted; it is verified by eye.
#![cfg(windows)]

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use ferrokit_core::event::{Event, EventKind, MouseButtons};
use ferrokit_core::event_loop::EventLoop;
use ferrokit_core::object::{ObjectData, ObjectId, QObject};
use ferrokit_gui::geometry::primitives::{Point, Rect, Size};
use ferrokit_platform::WindowFlags;
use ferrokit_widgets::tooltip::{expire_time_ms, place_tip, CURSOR_SIZE, FALL_ASLEEP_DELAY_MS};
use ferrokit_widgets::*;

/// Every test in this binary drives process-wide state (the tip, and for the real windows the
/// foreground window and the cursor). They run one at a time, so a change of the foreground
/// window made by this process cannot come from a sibling test.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

// ---- pure functions ------------------------------------------------------------------------

#[test]
fn tip_lifetime_follows_text_length() {
    let _serial = serial();
    let text = |n: usize| "a".repeat(n);
    assert_eq!(expire_time_ms(&text(0), -1), 10_000);
    assert_eq!(
        expire_time_ms(&text(100), -1),
        10_000,
        "up to 100 characters: 10 s"
    );
    assert_eq!(
        expire_time_ms(&text(101), -1),
        10_040,
        "each further character adds 40 ms"
    );
    assert_eq!(expire_time_ms(&text(250), -1), 16_000);
    assert_eq!(
        expire_time_ms(&text(250), 1234),
        1234,
        "an explicit duration wins"
    );
    assert_eq!(
        expire_time_ms(&text(250), 0),
        16_000,
        "zero is not a duration"
    );
    // `QString::size` counts UTF-16 code units: a character outside the BMP is two.
    assert_eq!(expire_time_ms(&"\u{1F600}".repeat(51), -1), 10_000 + 40 * 2);
}

const SCREEN: Rect = Rect {
    x: 0,
    y: 0,
    width: 1000,
    height: 800,
};
const TIP: Size = Size {
    width: 120,
    height: 30,
};

#[test]
fn tip_sits_right_of_and_below_the_cursor() {
    let _serial = serial();
    let p = place_tip(Point::new(100, 200), TIP, CURSOR_SIZE, SCREEN);
    assert_eq!(p, Point::new(102, 216), "offset (2, cursor height)");
}

#[test]
fn tip_flips_left_of_the_cursor_at_the_right_edge() {
    let _serial = serial();
    // 990 + 2 + 120 overflows: x -= 4 + width, so the tip ends 2 px left of the cursor.
    let p = place_tip(Point::new(990, 200), TIP, CURSOR_SIZE, SCREEN);
    assert_eq!(p.x, 990 + 2 - (4 + 120));
    assert_eq!(p.y, 216);
}

#[test]
fn tip_flips_above_the_cursor_at_the_bottom_edge() {
    let _serial = serial();
    // 790 + 16 + 30 overflows: y -= 24 + height.
    let p = place_tip(Point::new(100, 790), TIP, CURSOR_SIZE, SCREEN);
    assert_eq!(p.y, 790 + 16 - (24 + 30));
    assert_eq!(p.x, 102);
}

#[test]
fn tip_is_clamped_into_the_screen() {
    let _serial = serial();
    // Flipped left (x -= 4 + 1100) the tip would start at 10 + 2 - 1104 < 0.
    let p = place_tip(
        Point::new(10, 790),
        Size::new(1100, 30),
        CURSOR_SIZE,
        SCREEN,
    );
    assert_eq!(p.x, 0, "wider than the screen: left edge");
    // Taller than the screen: flipped above, raised to the top, then pushed back up by the
    // bottom clamp, which is the last word (qtooltip.cpp:362-364).
    let p = place_tip(
        Point::new(100, 10),
        Size::new(120, 900),
        CURSOR_SIZE,
        SCREEN,
    );
    assert_eq!(p.y, 800 - 900);
    // A secondary screen offset from the origin.
    let second = Rect::new(1000, 0, 500, 400);
    let p = place_tip(Point::new(1495, 395), TIP, CURSOR_SIZE, second);
    // Hand-derived from qtooltip.cpp:349-361: x = 1495+2 = 1497, 1497+120 > 1500 so x -= 4+120 -> 1373;
    // y = 395+16 = 411, 411+30 > 400 so y -= 24+30 -> 357; no clamp applies.
    assert_eq!(p, Point::new(1373, 357));
}

#[test]
fn a_large_cursor_puts_the_tip_beside_it() {
    let _serial = serial();
    // A cursor taller than twice the tip moves the tip sideways, not below.
    let p = place_tip(Point::new(100, 200), TIP, Size::new(64, 64), SCREEN);
    assert_eq!(p, Point::new(100 + 32, 200));
}

// ---- fixtures --------------------------------------------------------------------------------

fn pump_until(el: &mut EventLoop, limit: Duration, cond: impl Fn() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < limit {
        el.process_events(false);
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    cond()
}

fn pump_for(el: &mut EventLoop, ms: u64) {
    pump_until(el, Duration::from_millis(ms), || false);
}

/// Pumps until `cond`, returning how long it took (None on timeout).
fn time_until(el: &mut EventLoop, limit_ms: u64, cond: impl Fn() -> bool) -> Option<Duration> {
    let start = Instant::now();
    pump_until(el, Duration::from_millis(limit_ms), &cond).then(|| start.elapsed())
}

const NO_BUTTON: MouseButtons = MouseButtons::NO_BUTTON;

fn leaf(x: i32, text: &str) -> WidgetRef {
    let w = EmptyWidget::with_geometry(Rect::new(x, 0, 100, 100));
    w.set_tool_tip(text);
    Rc::new(RefCell::new(Box::new(w)))
}

/// root(0,0 400x300, always shows tips) > a(0,0 100x100 "tip A"), b(200,0 100x100 "tip B").
fn two_leaves() -> (WidgetRef, WidgetRef, WidgetRef) {
    let root = EmptyWidget::with_geometry(Rect::new(0, 0, 400, 300));
    root.set_always_show_tool_tips(true);
    let root: WidgetRef = Rc::new(RefCell::new(Box::new(root)));
    let a = leaf(0, "tip A");
    let b = leaf(200, "tip B");
    root.borrow_mut().add_child(a.clone());
    root.borrow_mut().add_child(b.clone());
    adopt_tree(&root);
    (root, a, b)
}

fn hover(d: &mut EventTreeDispatcher, root: &WidgetRef, x: i32, y: i32) {
    d.dispatch_mouse_move(
        root,
        Point::new(x, y),
        Point::new(300 + x, 300 + y),
        NO_BUTTON,
    );
}

fn visible() -> bool {
    ToolTip::is_visible()
}

// ---- trigger, delay, cancel ------------------------------------------------------------------

#[test]
fn hover_shows_the_widgets_tool_tip_after_the_wake_up_delay() {
    let _serial = serial();
    let mut el = EventLoop::new();
    let (root, _a, _b) = two_leaves();
    let mut d = EventTreeDispatcher::new();

    hover(&mut d, &root, 10, 10);

    assert!(
        !pump_until(&mut el, Duration::from_millis(300), visible),
        "shown before the wake-up delay"
    );
    let shown = time_until(&mut el, 3000, visible).expect("the tip never appeared");
    assert!(
        shown + Duration::from_millis(300) >= Duration::from_millis(650),
        "tip appeared {shown:?} + 300 ms after the move, before the 700 ms wake-up delay"
    );
    assert_eq!(ToolTip::text(), "tip A");
}

#[test]
fn a_move_with_a_button_down_does_not_start_the_wake_up() {
    let _serial = serial();
    let mut el = EventLoop::new();
    let (root, _a, _b) = two_leaves();
    let mut d = EventTreeDispatcher::new();

    d.dispatch_mouse_move(
        &root,
        Point::new(10, 10),
        Point::new(310, 310),
        MouseButtons::LEFT,
    );

    assert!(!pump_until(&mut el, Duration::from_millis(1200), visible));
}

#[test]
fn leaving_the_window_cancels_the_pending_wake_up() {
    let _serial = serial();
    let mut el = EventLoop::new();
    let (root, _a, _b) = two_leaves();
    let mut d = EventTreeDispatcher::new();

    hover(&mut d, &root, 10, 10);
    pump_for(&mut el, 100);
    d.handle_mouse_leave();

    assert!(!pump_until(&mut el, Duration::from_millis(1200), visible));
}

#[test]
fn moving_to_another_widget_restarts_the_wake_up_for_that_widget() {
    let _serial = serial();
    let mut el = EventLoop::new();
    let (root, _a, _b) = two_leaves();
    let mut d = EventTreeDispatcher::new();

    hover(&mut d, &root, 10, 10);
    pump_for(&mut el, 500);
    hover(&mut d, &root, 210, 10);
    // 500 ms after the first move is past where A's tip would have come: the restart cancelled it.
    assert!(
        !pump_until(&mut el, Duration::from_millis(300), visible),
        "A's wake-up was not cancelled"
    );
    assert!(pump_until(&mut el, Duration::from_millis(2000), visible));
    assert_eq!(ToolTip::text(), "tip B");
}

#[test]
fn user_input_cancels_the_wake_up_and_hides_a_visible_tip() {
    let _serial = serial();
    let mut el = EventLoop::new();
    let (root, _a, _b) = two_leaves();
    let mut d = EventTreeDispatcher::new();

    hover(&mut d, &root, 10, 10);
    pump_for(&mut el, 100);
    d.dispatch_event(
        &root,
        &mut Event::new_spontaneous(EventKind::MouseButtonPress {
            x: 10,
            y: 10,
            button: 1,
        }),
    );
    assert!(
        !pump_until(&mut el, Duration::from_millis(1000), visible),
        "press did not cancel the wake-up"
    );

    hover(&mut d, &root, 12, 12);
    assert!(pump_until(&mut el, Duration::from_millis(2000), visible));
    d.dispatch_event(
        &root,
        &mut Event::new_spontaneous(EventKind::Wheel {
            x: 12,
            y: 12,
            pixel_delta_x: 0,
            pixel_delta_y: 0,
            angle_delta_x: 0,
            angle_delta_y: 120,
            modifiers: 0,
        }),
    );
    assert!(
        !visible(),
        "input hides the tip at once, not after the 300 ms hide delay"
    );
}

#[test]
fn leaving_hides_the_tip_after_the_hide_delay() {
    let _serial = serial();
    let mut el = EventLoop::new();
    let (root, _a, _b) = two_leaves();
    let mut d = EventTreeDispatcher::new();
    hover(&mut d, &root, 10, 10);
    assert!(pump_until(&mut el, Duration::from_millis(2000), visible));

    d.handle_mouse_leave();
    assert!(visible(), "hidden at once; Qt waits 300 ms");
    pump_for(&mut el, 100);
    assert!(visible(), "hidden before the 300 ms hide delay");
    assert!(
        pump_until(&mut el, Duration::from_millis(1500), || !visible()),
        "tip never hid"
    );
}

// ---- fall-asleep -----------------------------------------------------------------------------

#[test]
fn after_an_accepted_tip_the_next_widget_wakes_up_quickly() {
    let _serial = serial();
    let mut el = EventLoop::new();
    let (root, _a, _b) = two_leaves();
    let mut d = EventTreeDispatcher::new();
    hover(&mut d, &root, 10, 10);
    assert!(pump_until(&mut el, Duration::from_millis(2000), visible));

    // Within the 2 s fall-asleep period the wake-up takes 20 ms, not 700 ms.
    hover(&mut d, &root, 210, 10);
    let took = time_until(&mut el, 600, || ToolTip::text() == "tip B")
        .expect("the next widget's tip did not wake up quickly");
    assert!(took < Duration::from_millis(400), "took {took:?}");
    // The tip was reused in place: the pending hide from leaving A was cancelled.
    pump_for(&mut el, 500);
    assert!(
        visible(),
        "the tip for B was hidden by the hide timer started when leaving A"
    );
}

#[test]
fn the_tip_falls_asleep_after_two_seconds() {
    let _serial = serial();
    let mut el = EventLoop::new();
    let (root, _a, _b) = two_leaves();
    let mut d = EventTreeDispatcher::new();
    hover(&mut d, &root, 10, 10);
    assert!(pump_until(&mut el, Duration::from_millis(2000), visible));
    let shown_at = Instant::now();
    d.handle_mouse_leave();
    assert!(pump_until(&mut el, Duration::from_millis(1500), || {
        !visible()
    }));
    // The fall-asleep timer starts when the tip appears. Qt starts it as a coarse timer
    // (qbasictimer.cpp:146), which on Windows may fire up to 5% late (qeventdispatcher_win.cpp:
    // 312-326, 374); wait that long plus a timer tick past it.
    let asleep = Duration::from_millis(FALL_ASLEEP_DELAY_MS + FALL_ASLEEP_DELAY_MS / 20 + 50);
    pump_until(&mut el, asleep, || shown_at.elapsed() >= asleep);

    hover(&mut d, &root, 210, 10);
    assert!(
        !pump_until(&mut el, Duration::from_millis(300), visible),
        "still awake: woke up in under 300 ms"
    );
    assert!(pump_until(&mut el, Duration::from_millis(2000), visible));
}

#[test]
fn user_input_puts_tool_tips_to_sleep() {
    let _serial = serial();
    let mut el = EventLoop::new();
    let (root, _a, _b) = two_leaves();
    let mut d = EventTreeDispatcher::new();
    hover(&mut d, &root, 10, 10);
    assert!(pump_until(&mut el, Duration::from_millis(2000), visible));
    d.dispatch_event(
        &root,
        &mut Event::new_spontaneous(EventKind::KeyPress {
            key: 65,
            modifiers: 0,
            is_repeat: false,
        }),
    );

    // Tip A is still up (a key press hides a tip only on macOS); B's must not wake up quickly.
    hover(&mut d, &root, 210, 10);
    assert!(
        !pump_until(&mut el, Duration::from_millis(300), || ToolTip::text()
            == "tip B"),
        "a key press must end the quick wake-up"
    );
}

// ---- bubbling --------------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Resp {
    Accept,
    Ignore,
}

type Log = Rc<RefCell<Vec<String>>>;

struct Probe {
    base: WidgetBase,
    name: &'static str,
    resp: Resp,
    log: Log,
}

impl Probe {
    fn new(name: &'static str, g: Rect, resp: Resp, log: &Log) -> Self {
        Self {
            base: WidgetBase::with_geometry(g),
            name,
            resp,
            log: Rc::clone(log),
        }
    }
}

impl QObject for Probe {
    fn object_data(&self) -> &ObjectData {
        &self.base.object_data
    }
    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.base.object_data)
    }
    fn event(&mut self, event: &mut Event) -> bool {
        let EventKind::ToolTip {
            x,
            y,
            global_x,
            global_y,
        } = event.kind
        else {
            return false;
        };
        self.log.borrow_mut().push(format!(
            "{}:ToolTip({x},{y};{global_x},{global_y})",
            self.name
        ));
        match self.resp {
            Resp::Accept => {
                event.accept();
                true
            }
            Resp::Ignore => {
                event.ignore();
                true
            }
        }
    }
}

impl Widget for Probe {
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

fn widget(p: impl Widget + 'static) -> WidgetRef {
    Rc::new(RefCell::new(Box::new(p)))
}

/// root(0,0 400x300) > mid(20,10 200x150) > leaf(30,20 80x40). A point at (60,40) in root
/// coordinates is (40,30) in mid and (10,10) in leaf.
fn probe_tree(leaf: Resp, mid: Resp, root: Resp, log: &Log) -> WidgetRef {
    let root_p = Probe::new("root", Rect::new(0, 0, 400, 300), root, log);
    root_p.set_always_show_tool_tips(true);
    let root_w = widget(root_p);
    let mid_w = widget(Probe::new("mid", Rect::new(20, 10, 200, 150), mid, log));
    let leaf_w = widget(Probe::new("leaf", Rect::new(30, 20, 80, 40), leaf, log));
    mid_w.borrow_mut().add_child(Rc::clone(&leaf_w));
    root_w.borrow_mut().add_child(Rc::clone(&mid_w));
    adopt_tree(&root_w);
    root_w
}

fn wake_up(el: &mut EventLoop, d: &mut EventTreeDispatcher, root: &WidgetRef, log: &Log) {
    let before = log.borrow().len();
    d.dispatch_mouse_move(root, Point::new(60, 40), Point::new(560, 440), NO_BUTTON);
    assert!(
        pump_until(el, Duration::from_millis(3000), || log.borrow().len()
            > before),
        "no ToolTip event after a hover"
    );
}

#[test]
fn an_ignored_tool_tip_bubbles_to_each_parent_in_its_own_coordinates() {
    let _serial = serial();
    let mut el = EventLoop::new();
    let log: Log = Default::default();
    let root = probe_tree(Resp::Ignore, Resp::Ignore, Resp::Accept, &log);
    let mut d = EventTreeDispatcher::new();

    wake_up(&mut el, &mut d, &root, &log);

    assert_eq!(
        *log.borrow(),
        vec![
            "leaf:ToolTip(10,10;560,440)",
            "mid:ToolTip(40,30;560,440)",
            "root:ToolTip(60,40;560,440)",
        ],
        "each ancestor sees the help event in its own coordinates; the global position is unchanged"
    );
}

#[test]
fn an_accepted_tool_tip_stops_at_the_accepting_widget() {
    let _serial = serial();
    let mut el = EventLoop::new();
    let log: Log = Default::default();
    let root = probe_tree(Resp::Ignore, Resp::Accept, Resp::Accept, &log);
    let mut d = EventTreeDispatcher::new();

    wake_up(&mut el, &mut d, &root, &log);
    pump_for(&mut el, 100);

    assert_eq!(
        *log.borrow(),
        vec!["leaf:ToolTip(10,10;560,440)", "mid:ToolTip(40,30;560,440)"]
    );
}

#[test]
fn a_widget_that_does_not_handle_the_event_gets_the_default_tool_tip_behaviour() {
    let _serial = serial();
    // `QWidget::event(ToolTip)`: show the widget's own toolTip; with none, ignore, so it bubbles.
    let mut el = EventLoop::new();
    let (root, a, _b) = two_leaves();
    let inner = leaf(0, "");
    inner.borrow().set_geometry(Rect::new(10, 10, 50, 50));
    a.borrow_mut().add_child(inner.clone());
    adopt_tree(&root);
    let mut d = EventTreeDispatcher::new();

    hover(&mut d, &root, 20, 20); // inside `inner`, whose own tip is empty

    assert!(pump_until(&mut el, Duration::from_millis(3000), visible));
    assert_eq!(
        ToolTip::text(),
        "tip A",
        "an empty tool tip falls through to the parent's"
    );
}

#[test]
fn a_tool_tip_that_nobody_accepts_does_not_start_the_quick_wake_up() {
    let _serial = serial();
    let mut el = EventLoop::new();
    let log: Log = Default::default();
    let root = probe_tree(Resp::Ignore, Resp::Ignore, Resp::Ignore, &log);
    let mut d = EventTreeDispatcher::new();
    wake_up(&mut el, &mut d, &root, &log);
    let after_first = log.borrow().len();

    // No tip was shown, so the fall-asleep timer is not running: the next wake-up takes 700 ms.
    d.dispatch_mouse_move(&root, Point::new(61, 41), Point::new(561, 441), NO_BUTTON);
    assert!(
        !pump_until(&mut el, Duration::from_millis(300), || log.borrow().len()
            > after_first),
        "an unaccepted help event started the fall-asleep period"
    );
}

// ---- QToolTip::showText ------------------------------------------------------------------------

#[test]
fn show_text_places_and_replaces_the_tip() {
    let _serial = serial();
    let _el = EventLoop::new();
    let p1 = Point::new(300, 300);
    let p2 = Point::new(340, 330);
    ToolTip::show_text(p1, "first", None, -1);
    assert!(ToolTip::is_visible());
    assert_eq!(ToolTip::text(), "first");
    let g1 = ToolTip::geometry().expect("visible");

    ToolTip::show_text(p2, "first", None, -1);
    assert_eq!(
        ToolTip::geometry(),
        Some(g1),
        "the same text for the same widget does not move the tip"
    );

    ToolTip::show_text(p2, "second, longer", None, -1);
    assert_eq!(ToolTip::text(), "second, longer");
    let g2 = ToolTip::geometry().expect("visible");
    assert_ne!(
        (g2.x, g2.y),
        (g1.x, g1.y),
        "a changed text moves the tip to the new position"
    );

    // show_text_rect replaces tip when rect changes (qtooltip.cpp:428)
    let rect1 = Rect::new(10, 10, 50, 50);
    ToolTip::show_text_rect(p2, "rect tip", None, Some(rect1), -1);
    let gr1 = ToolTip::geometry().expect("visible");
    let rect2 = Rect::new(20, 20, 60, 60);
    ToolTip::show_text_rect(p1, "rect tip", None, Some(rect2), -1);
    let gr2 = ToolTip::geometry().expect("visible");
    assert_ne!(
        (gr2.x, gr2.y),
        (gr1.x, gr1.y),
        "different rect moves/replaces the tip"
    );

    // QToolTip font and palette API
    let f = ToolTip::font();
    assert_eq!(f.size, 12.0);
    let new_f = ferrokit_gui::text::Font::new("Consolas", 14.0);
    ToolTip::set_font(new_f.clone());
    assert_eq!(ToolTip::font().family, "Consolas");
    assert_eq!(ToolTip::font().size, 14.0);

    let pal = ToolTip::palette();
    ToolTip::set_palette(pal.clone());
    assert_eq!(ToolTip::palette(), pal);
}

#[test]
fn a_tip_shown_near_the_screen_corner_flips_inside_the_screen() {
    let _serial = serial();
    let _el = EventLoop::new();
    let screen = ferrokit_platform::integration::platform()
        .primary_screen()
        .geometry();
    let pos = Point::new(screen.x + screen.width - 5, screen.y + screen.height - 5);

    ToolTip::show_text(pos, "corner", None, -1);

    let g = ToolTip::geometry().expect("visible");
    assert!(
        g.x >= screen.x && g.x + g.width <= screen.x + screen.width,
        "{g:?} not inside {screen:?}"
    );
    assert!(
        g.y >= screen.y && g.y + g.height <= screen.y + screen.height,
        "{g:?} not inside {screen:?}"
    );
    assert!(
        g.x + g.width <= pos.x,
        "flipped to the left of the cursor: {g:?} vs {pos:?}"
    );
    assert!(
        g.y + g.height <= pos.y,
        "flipped above the cursor: {g:?} vs {pos:?}"
    );
}

#[test]
fn an_explicit_duration_ends_the_tip() {
    let _serial = serial();
    let mut el = EventLoop::new();
    ToolTip::show_text(Point::new(300, 300), "short lived", None, 400);
    assert!(ToolTip::is_visible());

    pump_for(&mut el, 200);
    assert!(ToolTip::is_visible(), "gone before its 400 ms");
    assert!(
        pump_until(&mut el, Duration::from_millis(1500), || {
            !ToolTip::is_visible()
        }),
        "never expired"
    );
    assert_eq!(ToolTip::text(), "");
}

#[test]
fn an_empty_text_hides_the_tip_after_the_hide_delay() {
    let _serial = serial();
    let mut el = EventLoop::new();
    ToolTip::show_text(Point::new(300, 300), "bye", None, -1);
    ToolTip::show_text(Point::new(300, 300), "", None, -1);
    assert!(ToolTip::is_visible(), "QTipLabel::hideTip waits 300 ms");
    assert!(pump_until(&mut el, Duration::from_millis(1500), || {
        !ToolTip::is_visible()
    }));
}
// Tip text geometry: QToolTip::updateSize (qtooltip.cpp:143-160) and QLabel::sizeForWidth.

fn tip_height(text: &str) -> i32 {
    ToolTip::show_text(Point::new(300, 300), text, None, -1);
    ToolTip::geometry().expect("visible").height
}

#[test]
fn a_long_plain_tip_word_wraps() {
    let _serial = serial();
    let _el = EventLoop::new();
    let one = tip_height("word");
    let line = one - 4;
    let h = tip_height(&"word ".repeat(100));
    assert!(
        h >= 3 * line + 4,
        "plain tip did not wrap: height {h}, one line {one}"
    );
}

#[test]
fn a_long_rich_tip_word_wraps() {
    let _serial = serial();
    let _el = EventLoop::new();
    let one = tip_height("word");
    let line = one - 4;
    let h = tip_height(&"<b>word </b>".repeat(200));
    assert!(
        h >= 3 * line + 4,
        "rich tip did not wrap: height {h}, one line {one}"
    );
}

#[test]
fn a_multi_line_tip_is_as_tall_as_its_lines() {
    let _serial = serial();
    let _el = EventLoop::new();
    let one = tip_height("a");
    let line = one - 4;
    assert_eq!(
        tip_height("a\nb\nc"),
        3 * line + 4,
        "one line of height per \\n-separated line"
    );
}

// ---- real windows: activation gate, button state, no focus stealing -------------------------

mod real_windows {
    use super::*;
    use std::cell::Cell;
    use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
    use windows_sys::Win32::Foundation::{POINT, RECT};
    use windows_sys::Win32::Graphics::Gdi::ScreenToClient;
    use windows_sys::Win32::System::Threading::{
        AttachThreadInput, GetCurrentProcessId, GetCurrentThreadId,
    };
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        keybd_event, GetLastInputInfo, TrackMouseEvent, KEYEVENTF_KEYUP, LASTINPUTINFO, TME_LEAVE,
        TME_QUERY, TRACKMOUSEEVENT, VK_MENU,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, GetCursorPos, GetForegroundWindow, GetWindowRect,
        GetWindowThreadProcessId, IsWindow, IsWindowVisible, SendMessageW, SetCursorPos,
        SetForegroundWindow, WindowFromPoint, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
    };

    /// Puts the real cursor over a window for as long as it lives, then puts it back. A native
    /// `WM_MOUSEMOVE` makes the platform layer ask Windows to report when the cursor leaves
    /// (`TrackMouseEvent`); with the cursor elsewhere Windows answers with `WM_MOUSELEAVE` at
    /// once, which is a real `Leave` that cancels the tip.
    struct CursorOver(POINT);

    impl CursorOver {
        fn new(hwnd: isize) -> Self {
            let mut saved = POINT { x: 0, y: 0 };
            let mut r = RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
            unsafe {
                GetCursorPos(&mut saved);
                GetWindowRect(hwnd as _, &mut r);
                SetCursorPos(r.left + 150, r.top + 150);
            }
            CursorOver(saved)
        }
    }

    impl Drop for CursorOver {
        fn drop(&mut self) {
            unsafe { SetCursorPos(self.0.x, self.0.y) };
        }
    }

    const MK_LBUTTON: usize = 1;

    /// Where `CursorOver` puts the cursor, in screen pixels, and how far the tests themselves
    /// nudge it from there (`settle`).
    const CURSOR_ANCHOR: i32 = 150;
    const CURSOR_NUDGE: i32 = 2;

    /// These scenarios drive the real cursor and the real foreground window, which every other
    /// program on a shared desktop can also change. When one does, the scenario cannot say
    /// anything about the tool tip and is run again. This is the payload that unwinds the
    /// scenario then (`resume_unwind` does not run the panic hook, so it is silent).
    enum Interrupted {
        /// The desktop session refused to give this process the foreground.
        Refused,
        /// Another program changed the foreground window or moved the cursor mid-scenario.
        Interfered(String),
    }

    /// How the desktop looks to the scenario, for the report of a failure with no interference.
    fn snapshot(hwnd: isize) -> String {
        unsafe {
            let fg = GetForegroundWindow();
            let mut pid = 0u32;
            if !fg.is_null() {
                GetWindowThreadProcessId(fg, &mut pid);
            }
            let mut r = RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
            let mut at = POINT { x: 0, y: 0 };
            GetWindowRect(hwnd as _, &mut r);
            GetCursorPos(&mut at);
            let (top, top_pid) = window_at(at);
            format!(
                "window exists {}, visible {}, rect ({}, {}); foreground {:#x} of process {pid} \
                 (this process is {}); cursor ({}, {}) is over window {top:#x} of process \
                 {top_pid}",
                IsWindow(hwnd as _) != 0,
                IsWindowVisible(hwnd as _) != 0,
                r.left,
                r.top,
                fg as usize,
                GetCurrentProcessId(),
                at.x,
                at.y
            )
        }
    }

    /// The window at a screen point and the process it belongs to (0 for none).
    fn window_at(at: POINT) -> (usize, u32) {
        unsafe {
            let top = WindowFromPoint(at);
            let mut pid = 0u32;
            if !top.is_null() {
                GetWindowThreadProcessId(top, &mut pid);
            }
            (top as usize, pid)
        }
    }

    /// What outside the process has changed the preconditions of a scenario: `Ok` names it when
    /// the foreground window is another program's (a window of this process, such as the tip
    /// itself, is not outside interference), the cursor is not where `CursorOver` left it over
    /// `hwnd`, or another program's window covers that spot. `Err` is the snapshot when nothing
    /// outside is to blame, which includes the test's own window being gone or hidden: nothing in
    /// a scenario destroys or hides it, so another program did not, and whatever the foreground is
    /// then says nothing about who is to blame.
    fn interference(hwnd: isize) -> Result<String, String> {
        unsafe {
            if IsWindow(hwnd as _) == 0 || IsWindowVisible(hwnd as _) == 0 {
                return Err(snapshot(hwnd));
            }
            let fg = GetForegroundWindow();
            let mut pid = 0u32;
            if !fg.is_null() {
                GetWindowThreadProcessId(fg, &mut pid);
            }
            if pid != GetCurrentProcessId() {
                return Ok(format!(
                    "the foreground window {:#x} belongs to process {pid}",
                    fg as usize
                ));
            }
            // The scenario puts exactly one of its windows in the foreground at a time; when
            // another one of its windows is there instead, something activated it. Nothing in the
            // scenario does (a click on it from outside does), so it is outside input.
            let expected = EXPECTED_FOREGROUND.get();
            let fixture = FIXTURE_WINDOWS.with_borrow(|windows| windows.contains(&(fg as isize)));
            if expected != 0 && fg as isize != expected && fixture {
                return Ok(format!(
                    "the window {:#x} of the test was activated while {expected:#x} was in the foreground",
                    fg as usize
                ));
            }
            // The system-wide time of the last keyboard or mouse input, injected input included,
            // moves on when anyone but the scenario types or clicks. Its own inputs are the Alt
            // press of `take_foreground`, after which `mark_input` takes the time again.
            let marked = INPUT_MARK.get();
            if marked != 0 && last_input() != marked {
                return Ok(format!(
                    "keyboard or mouse input arrived from outside (at tick {} after {marked})",
                    last_input()
                ));
            }
            let mut r = RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
            let mut at = POINT { x: 0, y: 0 };
            GetWindowRect(hwnd as _, &mut r);
            GetCursorPos(&mut at);
            let (dx, dy) = (
                at.x - (r.left + CURSOR_ANCHOR),
                at.y - (r.top + CURSOR_ANCHOR),
            );
            if dx.abs() > CURSOR_NUDGE || dy.abs() > CURSOR_NUDGE {
                return Ok(format!(
                    "the cursor is at ({}, {}), off its place over the window",
                    at.x, at.y
                ));
            }
            // Another program's window on top of the spot gets the cursor's messages instead.
            let (top, top_pid) = window_at(at);
            if top_pid != 0 && top_pid != GetCurrentProcessId() {
                return Ok(format!(
                    "the window {top:#x} of process {top_pid} covers the cursor at ({}, {})",
                    at.x, at.y
                ));
            }
        }
        Err(snapshot(hwnd))
    }

    /// Fails the scenario: as `Interrupted` when `why` names outside interference, otherwise as a
    /// real failure with the state of the desktop.
    fn fail(why: Result<String, String>, message: String) -> ! {
        match why {
            Ok(why) => resume_unwind(Box::new(Interrupted::Interfered(format!(
                "{why} ({message})"
            )))),
            Err(state) => panic!("{message} [no outside interference: {state}]"),
        }
    }

    /// `assert!(cond, ...)` for a check that outside interference could also fail: `$hwnd` is the
    /// window the cursor was put over.
    macro_rules! ensure {
        ($hwnd:expr, $cond:expr, $($message:tt)+) => {
            if !$cond {
                fail(interference($hwnd), format!($($message)+));
            }
        };
    }

    thread_local! {
        /// The window of the scenario that `require_foreground` last put in the foreground (0
        /// before it did), and every window the scenario has opened.
        static EXPECTED_FOREGROUND: Cell<isize> = const { Cell::new(0) };
        static FIXTURE_WINDOWS: RefCell<Vec<isize>> = const { RefCell::new(Vec::new()) };
        /// `last_input` as of the scenario's own last input (0 before the first).
        static INPUT_MARK: Cell<u32> = const { Cell::new(0) };
    }

    fn last_input() -> u32 {
        let mut info = LASTINPUTINFO {
            cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
            dwTime: 0,
        };
        unsafe { GetLastInputInfo(&mut info) };
        info.dwTime
    }

    /// Takes `last_input` as the baseline that later input is compared with.
    fn mark_input() {
        INPUT_MARK.set(last_input());
    }

    const SCENARIO_ATTEMPTS: usize = 6;

    /// Runs `scenario` until it ends without outside interference. A failure with no
    /// interference present is the test's failure at once; failing to get a clean run in every
    /// attempt is one too, except when Windows only ever refused the foreground, which is a
    /// skip as before.
    fn run_scenario(mut scenario: impl FnMut(&mut EventLoop)) {
        let mut el = EventLoop::new();
        let mut interruptions = Vec::new();
        for _ in 0..SCENARIO_ATTEMPTS {
            EXPECTED_FOREGROUND.set(0);
            FIXTURE_WINDOWS.with_borrow_mut(Vec::clear);
            INPUT_MARK.set(0);
            ToolTip::hide_text();
            pump_for(&mut el, 100);
            match catch_unwind(AssertUnwindSafe(|| scenario(&mut el))) {
                Ok(()) => return,
                Err(payload) => match payload.downcast::<Interrupted>() {
                    Ok(interrupted) => {
                        let why = match *interrupted {
                            Interrupted::Refused => None,
                            Interrupted::Interfered(why) => Some(why),
                        };
                        eprintln!("scenario interrupted by the desktop session: {why:?}");
                        interruptions.push(why);
                    }
                    Err(payload) => resume_unwind(payload),
                },
            }
        }
        if interruptions.iter().all(Option::is_none) {
            eprintln!("{SKIP}");
            return;
        }
        panic!(
            "the desktop session interfered in all {SCENARIO_ATTEMPTS} attempts: {interruptions:?}"
        );
    }

    /// Puts `hwnd` in the foreground or abandons the attempt.
    fn require_foreground(hwnd: isize) {
        if !take_foreground(hwnd) {
            resume_unwind(Box::new(Interrupted::Refused));
        }
        EXPECTED_FOREGROUND.set(hwnd);
        mark_input();
    }

    struct Fixture {
        win: Box<Window>,
        hwnd: isize,
        root: WidgetRef,
    }

    fn open(title: &str, tip: &str, x: i32) -> Fixture {
        let mut win = Box::new(
            Window::new(title, Rect::new(x, 200, 400, 300), WindowFlags::NORMAL).expect("window"),
        );
        let root = leaf(0, tip);
        root.borrow().set_geometry(Rect::new(0, 0, 400, 300));
        adopt_tree(&root);
        win.set_root_widget(root.clone());
        win.show();
        let hwnd = win.native_handle();
        FIXTURE_WINDOWS.with_borrow_mut(|windows| windows.push(hwnd));
        Fixture { win, hwnd, root }
    }

    fn foreground() -> isize {
        unsafe { GetForegroundWindow() as isize }
    }

    /// Asks Windows for the foreground three ways, as a user's own input would get it: directly;
    /// with the synthetic Alt press that lets a background process take it; and sharing the input
    /// queue of the current foreground thread. `false` if the session refused all of them.
    fn take_foreground(hwnd: isize) -> bool {
        unsafe {
            SetForegroundWindow(hwnd as _);
            if foreground() != hwnd {
                keybd_event(VK_MENU as u8, 0, 0, 0);
                keybd_event(VK_MENU as u8, 0, KEYEVENTF_KEYUP, 0);
                SetForegroundWindow(hwnd as _);
            }
            let current = GetForegroundWindow();
            if foreground() != hwnd && !current.is_null() {
                let theirs = GetWindowThreadProcessId(current, std::ptr::null_mut());
                let ours = GetCurrentThreadId();
                if AttachThreadInput(ours, theirs, 1) != 0 {
                    SetForegroundWindow(hwnd as _);
                    BringWindowToTop(hwnd as _);
                    AttachThreadInput(ours, theirs, 0);
                }
            }
        }
        foreground() == hwnd
    }

    /// Whether Windows tracks the cursor as inside `hwnd`: the window has had a real
    /// `WM_MOUSEMOVE` and the platform layer's `TrackMouseEvent(TME_LEAVE)` request is pending.
    fn tracks_the_cursor(hwnd: isize) -> bool {
        let mut tme = TRACKMOUSEEVENT {
            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: TME_QUERY,
            hwndTrack: 0 as _,
            dwHoverTime: 0,
        };
        let queried = unsafe { TrackMouseEvent(&mut tme) } != 0;
        queried && tme.dwFlags & TME_LEAVE != 0 && tme.hwndTrack as isize == hwnd
    }

    /// Moving the real cursor makes Windows send the window a real `WM_MOUSEMOVE` (sometimes more
    /// than one), after which Windows tracks the cursor as inside the window. Until then Windows
    /// answers every `WM_MOUSEMOVE` the test sends with `WM_MOUSELEAVE`, a real `Leave` that
    /// cancels the wake-up, as it would in Qt (qwindowspointerhandler.cpp:265-273, 354-357,
    /// 868-876). On CI runners the move sometimes never comes; nudge the cursor until it does.
    /// Let every move arrive, then cancel the wake-up they started the way a user would: a click.
    fn settle(el: &mut EventLoop, f: &Fixture) {
        let mut at = POINT { x: 0, y: 0 };
        unsafe { GetCursorPos(&mut at) };
        pump_for(el, 100);
        for dx in [1, 0, 1, 0, 1, 0] {
            if tracks_the_cursor(f.hwnd) {
                break;
            }
            unsafe { SetCursorPos(at.x + dx, at.y) };
            pump_for(el, 200);
        }
        ensure!(
            f.hwnd,
            tracks_the_cursor(f.hwnd),
            "precondition: Windows never tracked the cursor as inside the window"
        );
        let at = ((50u32 << 16) | 50) as isize;
        unsafe {
            SendMessageW(f.hwnd as _, WM_LBUTTONDOWN, MK_LBUTTON, at);
            SendMessageW(f.hwnd as _, WM_LBUTTONUP, 0, at);
        }
        pump_for(el, 20);
        ensure!(
            f.hwnd,
            !visible(),
            "precondition: the click did not cancel the wake-up"
        );
        mark_input();
    }

    /// A `WM_MOUSEMOVE` at the client position `(x, y)` (physical pixels).
    fn mouse_move(f: &Fixture, x: i32, y: i32, wparam: usize) {
        let lparam = ((y as u32 & 0xffff) << 16 | (x as u32 & 0xffff)) as isize;
        unsafe { SendMessageW(f.hwnd as _, WM_MOUSEMOVE, wparam, lparam) };
    }

    /// The real cursor position in `f`'s client coordinates (physical pixels).
    fn cursor_in_client(f: &Fixture) -> (i32, i32) {
        let mut pt = POINT { x: 0, y: 0 };
        unsafe {
            GetCursorPos(&mut pt);
            ScreenToClient(f.hwnd as _, &mut pt);
        }
        (pt.x, pt.y)
    }

    const SKIP: &str = "SKIPPED: the desktop session refused to give this process the foreground";

    #[test]
    fn the_button_state_of_a_native_mouse_move_reaches_the_wake_up_gate() {
        let _serial = serial();
        run_scenario(|el| {
            let f = open("tip-buttons", "tip", 200);
            require_foreground(f.hwnd);
            let _cursor = CursorOver::new(f.hwnd);
            settle(el, &f);

            // The messages carry the real cursor position. Windows also sends buttonless
            // `WM_MOUSEMOVE`s of its own at the real cursor position (e.g. after a capture is
            // released); the platform layer drops those as Qt does when they repeat the last
            // position (qwindowspointerhandler.cpp:800-809). A test move elsewhere would make
            // such a move look like real motion that starts the wake-up timer.
            let (x, y) = cursor_in_client(&f);
            mouse_move(&f, x, y, MK_LBUTTON);
            ensure!(
                f.hwnd,
                !pump_until(el, Duration::from_millis(1200), visible),
                "MK_LBUTTON move started a tip"
            );

            mouse_move(&f, x + 2, y, 0);
            ensure!(
                f.hwnd,
                pump_until(el, Duration::from_millis(3000), visible),
                "a plain native move never showed the tip"
            );
            ensure!(
                f.hwnd,
                ToolTip::text() == "tip",
                "the tip text is {:?}, not the widget's",
                ToolTip::text()
            );
        });
    }

    #[test]
    fn showing_the_tip_does_not_take_the_foreground_and_is_placed_at_the_cursor() {
        let _serial = serial();
        run_scenario(|el| {
            let f = open("tip-focus", "tip", 200);
            require_foreground(f.hwnd);
            let _cursor = CursorOver::new(f.hwnd);
            settle(el, &f);

            mouse_move(&f, 50, 50, 0);
            ensure!(
                f.hwnd,
                pump_until(el, Duration::from_millis(3000), visible),
                "the tip was not shown"
            );

            ensure!(
                f.hwnd,
                foreground() == f.hwnd,
                "showing the tool tip took the foreground from its window"
            );
            // Wiring only: the native cursor position reaches the help event and `showText`. The
            // expected value is spelled out here, not recomputed with `place_tip`; Qt-geometry
            // parity is established by the hand-derived pure `place_tip` tests above, not by this
            // test.
            let dpr = f.win.device_pixel_ratio();
            let mut pt = POINT { x: 0, y: 0 };
            unsafe { GetCursorPos(&mut pt) };
            let cursor =
                ferrokit_platform::high_dpi::from_native_point(Point::new(pt.x, pt.y), dpr);
            let Some(tip) = ToolTip::geometry() else {
                fail(
                    interference(f.hwnd),
                    "the tip has no geometry: it is not visible".to_string(),
                )
            };
            let primary = ferrokit_platform::integration::platform().primary_screen();
            let screen = primary.geometry();
            // `QHighDpi::fromNativePixels(cursor->size(), screen)`: 16 native px at dpr 1.25 is
            // 13.
            let cursor_h = ferrokit_widgets::tooltip::cursor_size(dpr as f64, dpr as f64).height;
            ensure!(
                f.hwnd,
                cursor.x + 2 + tip.width <= screen.x + screen.width
                    && cursor.y + cursor_h + tip.height <= screen.y + screen.height,
                "precondition: the cursor is far enough from the screen edge that the tip is not flipped"
            );
            ensure!(
                f.hwnd,
                Point::new(tip.x, tip.y) == Point::new(cursor.x + 2, cursor.y + cursor_h),
                "the tip sits at the cursor + (2, cursor height): tip at ({}, {}), cursor at ({}, {}), cursor height {cursor_h}",
                tip.x,
                tip.y,
                cursor.x,
                cursor.y
            );
        });
    }

    #[test]
    fn an_inactive_window_shows_tool_tips_only_with_always_show() {
        let _serial = serial();
        run_scenario(|el| {
            let owner = open("tip-owner", "owner tip", 200);
            let other = open("tip-other", "other tip", 700);
            require_foreground(other.hwnd);
            ensure!(
                owner.hwnd,
                foreground() != owner.hwnd,
                "precondition: the owner window is not in the foreground"
            );
            let _cursor = CursorOver::new(owner.hwnd);
            settle(el, &owner);

            mouse_move(&owner, 50, 50, 0);
            ensure!(
                owner.hwnd,
                !pump_until(el, Duration::from_millis(1200), visible),
                "an inactive window showed a tip"
            );

            // `WA_AlwaysShowToolTips` on the window widget.
            owner.root.borrow().set_always_show_tool_tips(true);
            mouse_move(&owner, 52, 50, 0);
            ensure!(
                owner.hwnd,
                pump_until(el, Duration::from_millis(3000), visible),
                "WA_AlwaysShowToolTips was ignored"
            );
            ensure!(
                owner.hwnd,
                foreground() == other.hwnd,
                "the tip changed the foreground window"
            );
            ToolTip::hide_text();
            ensure!(
                owner.hwnd,
                pump_until(el, Duration::from_millis(1500), || !visible()),
                "the tip was not hidden by hide_text"
            );

            // Without the attribute the window shows tips once it is active.
            owner.root.borrow().set_always_show_tool_tips(false);
            require_foreground(owner.hwnd);
            mouse_move(&owner, 54, 50, 0);
            ensure!(
                owner.hwnd,
                pump_until(el, Duration::from_millis(3000), visible),
                "the active window showed no tip"
            );
            ensure!(
                owner.hwnd,
                foreground() == owner.hwnd,
                "the tip changed the foreground window"
            );
            drop((owner.win, other.win));
        });
    }
}
