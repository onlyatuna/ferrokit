// SPDX-License-Identifier: LGPL-3.0-only
//! Cursor and pointer-state APIs: `QWidget::setCursor/unsetCursor/cursor`, the application
//! override cursor stack, `QCursor::pos/setPos`, `QGuiApplication::mouseButtons`, and the
//! native side of `QWidget::grabMouse` (`QWindow::setMouseGrabEnabled`).
//!
//! The fake platform logs what a window is told (`set_cursor`, `unset_cursor`,
//! `set_mouse_grab_enabled`) and what the platform cursor is told, so no native window is needed.

use ferrokit_core::event::MouseButtons;
use ferrokit_gui::geometry::primitives::{Point, Rect};
use ferrokit_gui::paint::Pixmap;
use ferrokit_platform::cursor::{CursorShape, PlatformCursor};
use ferrokit_platform::platform_window::PlatformWindow;
use ferrokit_platform::size_limits::WINDOW_SIZE_MAX;
use ferrokit_platform::window_system_interface::{WindowSystemEvent, WindowSystemEventHandler};
use ferrokit_platform::{
    set_platform_integration, GenericPlatformIntegration, KeyboardModifiers, MouseButton,
    PlatformIntegration,
    WindowFlags,
};
use ferrokit_widgets::widget::{EmptyWidget, Widget, WidgetRef};
use ferrokit_widgets::{set_widget_enabled, Application, Cursor, Window};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::{Arc, Mutex, MutexGuard};

thread_local! {
    static HANDLERS: RefCell<Vec<Box<dyn WindowSystemEventHandler>>> = RefCell::new(Vec::new());
    /// What windows and the platform cursor were told, in order.
    static LOG: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    /// Whether the platform cursor has override cursors (Windows does; the default does not).
    static OVERRIDE_SUPPORT: Cell<bool> = const { Cell::new(true) };
    /// The native pointer position the platform cursor reports.
    static NATIVE_POS: Cell<Option<Point>> = const { Cell::new(None) };
}

fn log(entry: String) {
    LOG.with(|l| l.borrow_mut().push(entry));
}

/// Takes the log.
fn take() -> Vec<String> {
    LOG.with(|l| std::mem::take(&mut *l.borrow_mut()))
}

struct FakeWindow {
    geometry: Rect,
    grabbed: bool,
}

impl PlatformWindow for FakeWindow {
    fn show(&self) {}
    fn hide(&self) {}
    fn is_active(&self) -> bool {
        false
    }
    fn geometry(&self) -> Rect {
        self.geometry
    }
    fn set_geometry(&mut self, rect: Rect) {
        self.geometry = rect;
    }
    fn set_stays_on_top(&mut self, _enabled: bool) {}
    fn set_click_through(&mut self, _enabled: bool) {}
    fn set_opacity(&mut self, _opacity: f32) {}
    fn opacity(&self) -> f32 {
        1.0
    }
    fn set_minimum_size(&mut self, _w: i32, _h: i32) {}
    fn minimum_size(&self) -> (i32, i32) {
        (0, 0)
    }
    fn set_maximum_size(&mut self, _w: i32, _h: i32) {}
    fn maximum_size(&self) -> (i32, i32) {
        (WINDOW_SIZE_MAX, WINDOW_SIZE_MAX)
    }
    fn device_pixel_ratio(&self) -> f32 {
        1.0
    }
    fn present(&mut self, _pixmap: &mut Pixmap, _opacity: f32) -> Result<(), &'static str> {
        Ok(())
    }
    fn set_event_handler(&mut self, handler: Box<dyn WindowSystemEventHandler>) {
        HANDLERS.with(|h| h.borrow_mut().push(handler));
    }
    fn native_handle(&self) -> isize {
        0
    }
    fn set_cursor(&mut self, shape: CursorShape) {
        log(format!("window cursor {shape:?}"));
    }
    fn unset_cursor(&mut self) {
        log("window cursor unset".to_string());
    }
    fn set_mouse_grab_enabled(&mut self, grab: bool) -> bool {
        log(format!("window grab {grab}"));
        self.grabbed = grab;
        grab
    }
    fn has_mouse_grab(&self) -> bool {
        self.grabbed
    }
}

struct FakeCursor;

impl PlatformCursor for FakeCursor {
    fn change_cursor(&mut self, _shape: CursorShape) {}
    fn pos(&self) -> Option<Point> {
        NATIVE_POS.with(|p| p.get())
    }
    fn set_pos(&mut self, pos: Point) {
        log(format!("set_pos {},{}", pos.x, pos.y));
        NATIVE_POS.with(|p| p.set(Some(pos)));
    }
    fn set_override_cursor(&mut self, shape: CursorShape) -> bool {
        if OVERRIDE_SUPPORT.with(|s| s.get()) {
            log(format!("platform override {shape:?}"));
            true
        } else {
            false
        }
    }
    fn clear_override_cursor(&mut self) {
        if OVERRIDE_SUPPORT.with(|s| s.get()) {
            log("platform override cleared".to_string());
        }
    }
}

struct FakePlatform {
    inner: GenericPlatformIntegration,
}

impl PlatformIntegration for FakePlatform {
    fn create_window(
        &self,
        _title: &str,
        rect: Rect,
        _flags: WindowFlags,
    ) -> Result<Box<dyn PlatformWindow>, &'static str> {
        Ok(Box::new(FakeWindow {
            geometry: rect,
            grabbed: false,
        }))
    }
    fn create_tray_icon(
        &self,
        tooltip: &str,
        pixmap: &Pixmap,
    ) -> Result<Box<dyn ferrokit_platform::platform_tray::PlatformTrayIcon>, &'static str> {
        self.inner.create_tray_icon(tooltip, pixmap)
    }
    fn primary_screen(&self) -> Box<dyn ferrokit_platform::screen::PlatformScreen> {
        self.inner.primary_screen()
    }
    fn screens(&self) -> Vec<Box<dyn ferrokit_platform::screen::PlatformScreen>> {
        self.inner.screens()
    }
    fn screen_at(&self, pos: Point) -> Option<Box<dyn ferrokit_platform::screen::PlatformScreen>> {
        self.inner.screen_at(pos)
    }
    fn screen_changed(&self) -> &ferrokit_core::signal::Signal<()> {
        self.inner.screen_changed()
    }
    fn theme(&self) -> Arc<dyn ferrokit_platform::theme::PlatformTheme> {
        self.inner.theme()
    }
    fn clipboard(&self) -> Box<dyn ferrokit_platform::clipboard::PlatformClipboard> {
        self.inner.clipboard()
    }
    fn create_hotkey_manager(
        &self,
    ) -> Result<Box<dyn ferrokit_platform::hotkey::PlatformHotkeyManager>, &'static str> {
        self.inner.create_hotkey_manager()
    }
    fn cursor(&self) -> Box<dyn PlatformCursor> {
        Box::new(FakeCursor)
    }
}

static PLATFORM_LOCK: Mutex<()> = Mutex::new(());

/// Installs the fake platform with a clean slate; hold the guard for the whole test.
fn install() -> MutexGuard<'static, ()> {
    let guard = PLATFORM_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    set_platform_integration(Arc::new(FakePlatform {
        inner: GenericPlatformIntegration::default(),
    }));
    OVERRIDE_SUPPORT.with(|s| s.set(true));
    NATIVE_POS.with(|p| p.set(None));
    while Application::override_cursor().is_some() {
        Application::restore_override_cursor();
    }
    ferrokit_platform::window::set_app_mouse_buttons(MouseButtons::NO_BUTTON);
    take();
    guard
}

fn send(index: usize, event: WindowSystemEvent) {
    HANDLERS.with(|h| h.borrow_mut()[index].handle_window_event(event));
}

fn move_to(i: usize, x: i32, y: i32) {
    send(
        i,
        WindowSystemEvent::MouseMove {
            pos: Point::new(x, y),
            global_pos: Point::new(x + 10, y + 20),
            buttons: MouseButtons::NO_BUTTON,
        },
    );
}

fn press(i: usize, x: i32, y: i32, button: MouseButton) {
    send(
        i,
        WindowSystemEvent::MousePress {
            pos: Point::new(x, y),
            global_pos: Point::new(x + 10, y + 20),
            button,
            modifiers: KeyboardModifiers::default(),
            timestamp: 1000,
        },
    );
}

fn release(i: usize, x: i32, y: i32, button: MouseButton) {
    send(
        i,
        WindowSystemEvent::MouseRelease {
            pos: Point::new(x, y),
            global_pos: Point::new(x + 10, y + 20),
            button,
            modifiers: KeyboardModifiers::default(),
        },
    );
}

fn child(parent: &WidgetRef, rect: Rect) -> WidgetRef {
    let w = EmptyWidget::new();
    w.set_geometry(rect);
    let w: WidgetRef = Rc::new(RefCell::new(Box::new(w)));
    parent.borrow_mut().add_child(w.clone());
    w.borrow().set_parent_widget(Some(Rc::downgrade(parent)));
    w
}

/// A window of 200x100 whose root has the children `a` at (0,0,50,50) and `b` at (100,0,50,50).
struct Fixture {
    win: Window,
    index: usize,
    a: WidgetRef,
    b: WidgetRef,
}

fn fixture() -> Fixture {
    let win = Window::new("cursor", Rect::new(0, 0, 200, 100), WindowFlags::FRAMELESS).unwrap();
    let index = HANDLERS.with(|h| h.borrow().len()) - 1;
    let root = win.root_widget();
    let a = child(&root, Rect::new(0, 0, 50, 50));
    let b = child(&root, Rect::new(100, 0, 50, 50));
    Fixture { win, index, a, b }
}

#[test]
fn a_widget_cursor_shows_while_the_pointer_is_over_it() {
    let _g = install();
    let f = fixture();
    f.a.borrow().set_cursor(CursorShape::PointingHand);

    move_to(f.index, 10, 10);
    assert_eq!(take(), ["window cursor PointingHand"]);
    // The same widget again: nothing new for the platform.
    move_to(f.index, 11, 10);
    assert!(take().is_empty());
    // A widget without a cursor: the window's default (unset).
    move_to(f.index, 110, 10);
    assert_eq!(take(), ["window cursor unset"]);
    drop(f.win);
}

#[test]
fn a_child_uses_its_parents_cursor_until_it_has_its_own() {
    let _g = install();
    let f = fixture();
    let root = f.win.root_widget();
    let inner = child(&f.a, Rect::new(5, 5, 20, 20));
    f.a.borrow().set_cursor(CursorShape::IBeam);

    assert_eq!(inner.borrow().cursor(), CursorShape::IBeam);
    assert_eq!(root.borrow().cursor(), CursorShape::Arrow);
    move_to(f.index, 10, 10);
    assert_eq!(take(), ["window cursor IBeam"]);

    inner.borrow().set_cursor(CursorShape::Cross);
    assert_eq!(inner.borrow().cursor(), CursorShape::Cross);
    // The pointer is over `inner` now: its own cursor shows at once.
    assert_eq!(take(), ["window cursor Cross"]);

    inner.borrow().unset_cursor();
    assert_eq!(inner.borrow().cursor(), CursorShape::IBeam);
    assert_eq!(take(), ["window cursor IBeam"]);
}

#[test]
fn set_cursor_under_the_pointer_applies_at_once_and_unset_reverts() {
    let _g = install();
    let f = fixture();
    move_to(f.index, 10, 10);
    assert!(take().is_empty());

    f.a.borrow().set_cursor(CursorShape::SizeAll);
    assert_eq!(take(), ["window cursor SizeAll"]);
    f.a.borrow().unset_cursor();
    assert_eq!(take(), ["window cursor unset"]);
    // A widget the pointer is not over changes nothing on the screen.
    f.b.borrow().set_cursor(CursorShape::Wait);
    assert!(take().is_empty());
}

#[test]
fn set_cursor_from_the_widgets_own_handler_is_applied_after_the_dispatch() {
    let _g = install();
    let f = fixture();
    move_to(f.index, 10, 10);
    take();
    // The widget is mutably borrowed while its handler runs, so it cannot be read right away.
    {
        let a = f.a.borrow_mut();
        a.set_cursor(CursorShape::ClosedHand);
        assert!(take().is_empty());
    }
    // The next dispatch flushes the retry.
    move_to(f.index, 11, 11);
    assert_eq!(take(), ["window cursor ClosedHand"]);
}

#[test]
fn a_disabled_widget_with_a_cursor_clears_it() {
    let _g = install();
    let f = fixture();
    f.a.borrow().set_cursor(CursorShape::PointingHand);
    move_to(f.index, 10, 10);
    take();

    set_widget_enabled(&f.a, false);
    assert_eq!(take(), ["window cursor unset"]);
    set_widget_enabled(&f.a, true);
    assert_eq!(take(), ["window cursor PointingHand"]);
}

#[test]
fn the_window_cursor_shows_where_no_widget_has_one() {
    let _g = install();
    let mut f = fixture();
    f.win.set_cursor(CursorShape::UpArrow);
    assert_eq!(take(), ["window cursor UpArrow"]);

    f.a.borrow().set_cursor(CursorShape::Cross);
    move_to(f.index, 10, 10);
    assert_eq!(take(), ["window cursor Cross"]);
    move_to(f.index, 110, 10);
    assert_eq!(take(), ["window cursor UpArrow"]);

    f.win.unset_cursor();
    assert_eq!(take(), ["window cursor unset"]);
}

#[test]
fn leaving_the_window_does_not_touch_the_cursor() {
    let _g = install();
    let f = fixture();
    f.a.borrow().set_cursor(CursorShape::Cross);
    move_to(f.index, 10, 10);
    take();
    send(f.index, WindowSystemEvent::MouseLeave);
    assert!(take().is_empty());
    // With the pointer outside the window, a change does not reach it either.
    f.a.borrow().set_cursor(CursorShape::Wait);
    assert!(take().is_empty());
}

#[test]
fn the_override_cursor_is_a_stack() {
    let _g = install();
    let f = fixture();
    assert_eq!(Application::override_cursor(), None);
    // Nothing to restore or change while none is set.
    Application::restore_override_cursor();
    Application::change_override_cursor(CursorShape::Cross);
    assert_eq!(Application::override_cursor(), None);
    assert!(take().is_empty());

    Application::set_override_cursor(CursorShape::Wait);
    Application::set_override_cursor(CursorShape::Busy);
    assert_eq!(Application::override_cursor(), Some(CursorShape::Busy));
    Application::change_override_cursor(CursorShape::Forbidden);
    assert_eq!(Application::override_cursor(), Some(CursorShape::Forbidden));
    Application::restore_override_cursor();
    assert_eq!(Application::override_cursor(), Some(CursorShape::Wait));
    Application::restore_override_cursor();
    assert_eq!(Application::override_cursor(), None);

    let entries = take();
    assert!(entries.contains(&"platform override Wait".to_string()), "{entries:?}");
    assert!(entries.contains(&"platform override Busy".to_string()), "{entries:?}");
    assert!(entries.contains(&"platform override Forbidden".to_string()), "{entries:?}");
    assert_eq!(entries.last().unwrap(), "window cursor unset");
    assert!(entries.contains(&"platform override cleared".to_string()));
    drop(f);
}

#[test]
fn an_override_cursor_beats_widget_cursors_and_restores_them() {
    let _g = install();
    let f = fixture();
    f.a.borrow().set_cursor(CursorShape::PointingHand);
    move_to(f.index, 10, 10);
    take();

    Application::set_override_cursor(CursorShape::Wait);
    take();
    // Moving over another widget does not replace the override.
    move_to(f.index, 110, 10);
    move_to(f.index, 10, 10);
    assert!(take().iter().all(|e| e == "window cursor Wait"));

    Application::restore_override_cursor();
    let entries = take();
    assert_eq!(entries.last().unwrap(), "window cursor PointingHand", "{entries:?}");
}

#[test]
fn without_platform_override_support_the_shape_goes_to_every_window() {
    let _g = install();
    OVERRIDE_SUPPORT.with(|s| s.set(false));
    let f = fixture();
    let other = fixture();

    Application::set_override_cursor(CursorShape::Busy);
    let entries = take();
    assert_eq!(
        entries.iter().filter(|e| *e == "window cursor Busy").count(),
        2,
        "{entries:?}"
    );
    assert!(!entries.iter().any(|e| e.starts_with("platform")));
    Application::restore_override_cursor();
    assert_eq!(take(), ["window cursor unset", "window cursor unset"]);
    drop((f, other));
}

#[test]
fn mouse_buttons_follow_presses_and_releases() {
    let _g = install();
    let f = fixture();
    assert_eq!(Application::mouse_buttons(), MouseButtons::NO_BUTTON);
    press(f.index, 10, 10, MouseButton::Left);
    assert_eq!(Application::mouse_buttons(), MouseButtons::LEFT);
    press(f.index, 10, 10, MouseButton::Right);
    assert_eq!(
        Application::mouse_buttons(),
        MouseButtons::LEFT.union(MouseButtons::RIGHT)
    );
    release(f.index, 10, 10, MouseButton::Left);
    assert_eq!(Application::mouse_buttons(), MouseButtons::RIGHT);
    release(f.index, 10, 10, MouseButton::Right);
    assert_eq!(Application::mouse_buttons(), MouseButtons::NO_BUTTON);
}

#[test]
fn cursor_pos_falls_back_to_the_last_mouse_event() {
    let _g = install();
    let f = fixture();
    // The fake cursor cannot be queried: the last event's global position is the answer.
    move_to(f.index, 30, 40);
    assert_eq!(Cursor::pos(), Point::new(40, 60));
    press(f.index, 1, 2, MouseButton::Left);
    assert_eq!(Cursor::pos(), Point::new(11, 22));
}

#[test]
fn cursor_pos_asks_the_platform_when_it_can_answer() {
    let _g = install();
    let f = fixture();
    move_to(f.index, 30, 40);
    NATIVE_POS.with(|p| p.set(Some(Point::new(5, 6))));
    assert_eq!(Cursor::pos(), Point::new(5, 6));
}

#[test]
fn cursor_set_pos_moves_the_platform_pointer_once() {
    let _g = install();
    NATIVE_POS.with(|p| p.set(Some(Point::new(0, 0))));
    Cursor::set_pos(Point::new(70, 80));
    assert_eq!(take(), ["set_pos 70,80"]);
    // Already there: not set again (qcursor.cpp:223).
    Cursor::set_pos(Point::new(70, 80));
    assert!(take().is_empty());
}

#[test]
fn grab_mouse_grabs_the_window_and_release_lets_go() {
    let _g = install();
    let f = fixture();
    f.a.borrow().grab_mouse();
    assert_eq!(take(), ["window grab true"]);
    assert!(ferrokit_widgets::widget::mouse_grabber().is_some());

    // Another widget cannot release it.
    f.b.borrow().release_mouse();
    assert!(take().is_empty());
    f.a.borrow().release_mouse();
    assert_eq!(take(), ["window grab false"]);
    assert!(ferrokit_widgets::widget::mouse_grabber().is_none());
}

#[test]
fn a_new_grabber_releases_the_previous_one_first() {
    let _g = install();
    let f = fixture();
    f.a.borrow().grab_mouse();
    take();
    f.b.borrow().grab_mouse();
    assert_eq!(take(), ["window grab false", "window grab true"]);
    assert!(f.b.borrow().is_mouse_grabber());
    assert!(!f.a.borrow().is_mouse_grabber());
    f.b.borrow().release_mouse();
}

#[test]
fn grab_mouse_with_a_cursor_pushes_an_override_until_release() {
    let _g = install();
    let f = fixture();
    f.a.borrow().grab_mouse_with_cursor(CursorShape::ClosedHand);
    assert_eq!(Application::override_cursor(), Some(CursorShape::ClosedHand));
    assert!(take().contains(&"window grab true".to_string()));
    f.a.borrow().release_mouse();
    assert_eq!(Application::override_cursor(), None);
    assert!(take().contains(&"window grab false".to_string()));
}

#[test]
fn destroying_the_grabber_releases_the_native_grab_and_the_override_cursor() {
    let _g = install();
    let f = fixture();
    let root = f.win.root_widget();
    let doomed = child(&root, Rect::new(60, 0, 20, 20));
    doomed.borrow().grab_mouse_with_cursor(CursorShape::SizeAll);
    assert_eq!(Application::override_cursor(), Some(CursorShape::SizeAll));
    take();

    let id = doomed.borrow().id();
    root.borrow_mut().remove_child(id);
    drop(doomed);

    assert!(ferrokit_widgets::widget::mouse_grabber_id().is_none());
    assert_eq!(Application::override_cursor(), None);
    assert!(take().contains(&"window grab false".to_string()));
}

#[test]
fn a_widget_in_no_window_records_the_grab_without_a_native_one() {
    let _g = install();
    let loose: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::new())));
    loose.borrow().grab_mouse();
    assert!(loose.borrow().is_mouse_grabber());
    assert!(take().is_empty());
    loose.borrow().release_mouse();
    assert!(!loose.borrow().is_mouse_grabber());
}

#[test]
fn map_to_and_map_from_walk_up_to_the_ancestor() {
    let root: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::new())));
    root.borrow().set_geometry(Rect::new(0, 0, 400, 400));
    let mid = child(&root, Rect::new(10, 20, 200, 200));
    let leaf = child(&mid, Rect::new(5, 7, 50, 50));

    let p = Point::new(1, 2);
    assert_eq!(leaf.borrow().map_to(&mid, p), Point::new(6, 9));
    assert_eq!(leaf.borrow().map_to(&root, p), Point::new(16, 29));
    assert_eq!(leaf.borrow().map_from(&root, Point::new(16, 29)), p);
    assert_eq!(leaf.borrow().map_from(&mid, Point::new(6, 9)), p);
    // The widget itself is its own ancestor.
    assert_eq!(leaf.borrow().map_to(&leaf, p), p);
    assert_eq!(leaf.borrow().map_from(&leaf, p), p);
}

#[test]
fn a_mutably_borrowed_ancestor_can_still_be_mapped_into() {
    let root: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::new())));
    let mid = child(&root, Rect::new(10, 20, 200, 200));
    let leaf = child(&mid, Rect::new(5, 7, 50, 50));

    // A parent's own handler maps a child's point into itself.
    let _handler_scope = mid.borrow_mut();
    assert_eq!(leaf.borrow().map_to(&mid, Point::new(1, 2)), Point::new(6, 9));
    assert_eq!(leaf.borrow().map_from(&mid, Point::new(6, 9)), Point::new(1, 2));
}
