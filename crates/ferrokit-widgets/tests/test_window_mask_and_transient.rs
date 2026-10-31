// SPDX-License-Identifier: LGPL-3.0-only
//! `QWidget::setMask` / `clearMask` / `mask` (`qwidget.cpp:13027-13129`) and top-level windows
//! that have a parent (`QWidget(parent, Qt::Tool | ...)`, `QWindow::setTransientParent`).
//!
//! The windows are a fake platform that records what the widget layer hands the platform window:
//! the window region (`QPlatformWindow::setMask`) and the owner (`updateTransientParent`).

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::object::QObject;
use ferrokit_gui::geometry::primitives::{Point, Rect};
use ferrokit_gui::geometry::Region;
use ferrokit_gui::paint::Pixmap;
use ferrokit_gui::tiny_skia::Color;
use ferrokit_platform::platform_window::PlatformWindow;
use ferrokit_platform::size_limits::WINDOW_SIZE_MAX;
use ferrokit_platform::window_system_interface::WindowSystemEventHandler;
use ferrokit_platform::{
    set_platform_integration, GenericPlatformIntegration, PlatformIntegration, WindowFlags,
};
use ferrokit_widgets::application::Application;
use ferrokit_widgets::widget::{EmptyWidget, WidgetAttribute, WidgetRef};
use ferrokit_widgets::window::Window;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicIsize, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Log {
    /// Every `set_mask`, with the native handle of the window it went to.
    masks: Vec<(isize, Region)>,
    /// Every `set_transient_parent`, with the handle of the window it went to.
    transient: Vec<(isize, Option<isize>)>,
    /// The flags of every window the platform was asked for.
    created: Vec<WindowFlags>,
}

impl Log {
    fn masks_of(&self, handle: isize) -> Vec<Region> {
        self.masks
            .iter()
            .filter(|(h, _)| *h == handle)
            .map(|(_, r)| r.clone())
            .collect()
    }
}

thread_local! {
    /// Run by the fake `set_geometry`, which the widget layer calls with the platform window
    /// borrowed: whatever the hook does to the window happens "inside a native call".
    static IN_SET_GEOMETRY: RefCell<Option<Box<dyn Fn()>>> = const { RefCell::new(None) };
}

static NEXT_HANDLE: AtomicIsize = AtomicIsize::new(0x1000);
/// The ratio of the fake screen, as `f32` bits.
static SCREEN_DPR: AtomicU32 = AtomicU32::new(0);

fn set_screen_dpr(dpr: f32) {
    SCREEN_DPR.store(dpr.to_bits(), Ordering::SeqCst);
}

struct FakeWindow {
    handle: isize,
    geometry: Rect,
    opacity: f32,
    min: (i32, i32),
    max: (i32, i32),
    transient_parent: Option<isize>,
    log: Arc<Mutex<Log>>,
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
        IN_SET_GEOMETRY.with(|hook| {
            if let Some(hook) = hook.borrow().as_ref() {
                hook();
            }
        });
    }
    fn set_stays_on_top(&mut self, _enabled: bool) {}
    fn set_click_through(&mut self, _enabled: bool) {}
    fn set_opacity(&mut self, opacity: f32) {
        self.opacity = opacity;
    }
    fn opacity(&self) -> f32 {
        self.opacity
    }
    fn set_minimum_size(&mut self, w: i32, h: i32) {
        self.min = (w, h);
    }
    fn minimum_size(&self) -> (i32, i32) {
        self.min
    }
    fn set_maximum_size(&mut self, w: i32, h: i32) {
        self.max = (w, h);
    }
    fn maximum_size(&self) -> (i32, i32) {
        self.max
    }
    fn device_pixel_ratio(&self) -> f32 {
        f32::from_bits(SCREEN_DPR.load(Ordering::SeqCst))
    }
    fn present(&mut self, _pixmap: &mut Pixmap, _opacity: f32) -> Result<(), &'static str> {
        Ok(())
    }
    fn set_event_handler(&mut self, _handler: Box<dyn WindowSystemEventHandler>) {}
    fn native_handle(&self) -> isize {
        self.handle
    }
    fn set_mask(&mut self, region: &Region) {
        self.log
            .lock()
            .unwrap()
            .masks
            .push((self.handle, region.clone()));
    }
    fn set_transient_parent(&mut self, parent: Option<isize>) {
        self.transient_parent = parent;
        self.log
            .lock()
            .unwrap()
            .transient
            .push((self.handle, parent));
    }
    fn transient_parent(&self) -> Option<isize> {
        self.transient_parent
    }
}

struct FakePlatform {
    inner: GenericPlatformIntegration,
    log: Arc<Mutex<Log>>,
}

impl PlatformIntegration for FakePlatform {
    fn create_window(
        &self,
        _title: &str,
        rect: Rect,
        flags: WindowFlags,
    ) -> Result<Box<dyn PlatformWindow>, &'static str> {
        self.log.lock().unwrap().created.push(flags);
        Ok(Box::new(FakeWindow {
            handle: NEXT_HANDLE.fetch_add(1, Ordering::SeqCst),
            geometry: rect,
            opacity: 1.0,
            min: (0, 0),
            max: (WINDOW_SIZE_MAX, WINDOW_SIZE_MAX),
            transient_parent: None,
            log: Arc::clone(&self.log),
        }))
    }
    fn create_tray_icon(
        &self,
        tooltip: &str,
        icon: &ferrokit_gui::image::Icon,
    ) -> Result<Box<dyn ferrokit_platform::platform_tray::PlatformTrayIcon>, &'static str> {
        self.inner.create_tray_icon(tooltip, icon)
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
    fn cursor(&self) -> Box<dyn ferrokit_platform::cursor::PlatformCursor> {
        self.inner.cursor()
    }
}

/// The platform integration is process-global: tests take turns.
static PLATFORM_LOCK: Mutex<()> = Mutex::new(());

fn install(dpr: f32) -> (Arc<Mutex<Log>>, std::sync::MutexGuard<'static, ()>) {
    let guard = PLATFORM_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let log = Arc::new(Mutex::new(Log::default()));
    set_screen_dpr(dpr);
    IN_SET_GEOMETRY.with(|hook| *hook.borrow_mut() = None);
    set_platform_integration(Arc::new(FakePlatform {
        inner: GenericPlatformIntegration::default(),
        log: Arc::clone(&log),
    }));
    (log, guard)
}

fn window(rect: Rect, flags: WindowFlags) -> Window {
    Window::new("mask", rect, flags).expect("window")
}

fn colored(rect: Rect, color: Color) -> WidgetRef {
    let mut widget = EmptyWidget::with_geometry(rect);
    widget.set_background_color(Some(color));
    Rc::new(RefCell::new(Box::new(widget)))
}

fn alpha_at(window: &Window, x: u32, y: u32) -> u8 {
    window
        .backing_store()
        .pixmap()
        .pixel(x, y)
        .expect("pixel inside the store")
        .alpha()
}

fn red() -> Color {
    Color::from_rgba8(255, 0, 0, 255)
}

// ---- QWidget::setMask / clearMask / mask on a window ------------------------------------------

#[test]
fn a_window_mask_is_handed_to_the_platform_window_as_its_region() {
    let (log, _g) = install(1.0);
    let mut win = window(Rect::new(0, 0, 100, 60), WindowFlags::FRAMELESS);
    let handle = win.native_handle();
    let mask = Region::from_rects(&[Rect::new(2, 0, 96, 1), Rect::new(0, 1, 100, 59)]);

    win.set_mask(&mask);

    assert_eq!(win.mask(), mask, "QWidget::mask returns what setMask set");
    assert_eq!(log.lock().unwrap().masks_of(handle), vec![mask.clone()]);
    assert_eq!(
        win.root_widget().borrow().mask(),
        mask,
        "the window widget holds the mask"
    );
}

#[test]
fn clear_mask_removes_the_window_region_and_is_a_noop_without_a_mask() {
    let (log, _g) = install(1.0);
    let mut win = window(Rect::new(0, 0, 100, 60), WindowFlags::FRAMELESS);
    let handle = win.native_handle();

    win.clear_mask();
    assert!(
        log.lock().unwrap().masks_of(handle).is_empty(),
        "no mask, nothing to clear"
    );

    win.set_mask(&Region::from_rect(Rect::new(0, 0, 50, 50)));
    win.clear_mask();

    assert!(win.mask().is_empty());
    let masks = log.lock().unwrap().masks_of(handle);
    assert_eq!(masks.len(), 2);
    assert!(
        masks[1].is_empty(),
        "an empty region is how the platform window drops its mask"
    );
}

#[test]
fn setting_an_equal_mask_again_does_nothing() {
    let (log, _g) = install(1.0);
    let mut win = window(Rect::new(0, 0, 100, 60), WindowFlags::FRAMELESS);
    let handle = win.native_handle();
    // Same area, different decomposition: `QRegion::operator==` compares the area.
    win.set_mask(&Region::from_rects(&[Rect::new(0, 0, 50, 50)]));
    win.set_mask(&Region::from_rects(&[
        Rect::new(0, 0, 25, 50),
        Rect::new(25, 0, 25, 50),
    ]));

    assert_eq!(log.lock().unwrap().masks_of(handle).len(), 1);
}

/// `QWindow::setMask` takes device-independent pixels; the platform window gets native ones.
#[test]
fn the_window_region_is_scaled_to_native_pixels() {
    let (log, _g) = install(2.0);
    let mut win = window(Rect::new(0, 0, 100, 60), WindowFlags::FRAMELESS);
    let handle = win.native_handle();

    win.set_mask(&Region::from_rect(Rect::new(1, 2, 30, 20)));

    let masks = log.lock().unwrap().masks_of(handle);
    assert_eq!(masks, vec![Region::from_rect(Rect::new(2, 4, 60, 40))]);
}

#[test]
fn the_window_region_follows_a_ratio_change() {
    let (log, _g) = install(2.0);
    let mut win = window(Rect::new(0, 0, 100, 60), WindowFlags::FRAMELESS);
    let handle = win.native_handle();
    win.show();
    win.set_mask(&Region::from_rect(Rect::new(0, 0, 40, 40)));

    let mut ev = Event::new_spontaneous(EventKind::DpiChanged {
        dpi_x: 144,
        dpi_y: 144,
    }); // 1.5
    assert!(win.event(&mut ev));
    win.render_and_present();

    let masks = log.lock().unwrap().masks_of(handle);
    assert_eq!(
        masks.last().unwrap(),
        &Region::from_rect(Rect::new(0, 0, 60, 60)),
        "the region is in native pixels of the new ratio"
    );
}

/// The mask is often set from a resize handler, which runs inside the native `set_geometry` while
/// the platform window is busy. The window region must not be lost.
#[test]
fn a_mask_set_inside_a_native_call_reaches_the_platform_window_on_the_next_render() {
    let (log, _g) = install(1.0);
    let mut win = window(Rect::new(0, 0, 100, 60), WindowFlags::FRAMELESS);
    let handle = win.native_handle();
    win.show();
    let root = win.root_widget();
    let mask = Region::from_rect(Rect::new(0, 0, 90, 50));
    let hook_mask = mask.clone();
    IN_SET_GEOMETRY.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || root.borrow().set_mask(&hook_mask)));
    });

    win.set_geometry(Rect::new(0, 0, 120, 80));
    IN_SET_GEOMETRY.with(|hook| *hook.borrow_mut() = None);

    assert_eq!(win.mask(), mask);
    assert_eq!(
        log.lock().unwrap().masks_of(handle),
        vec![mask],
        "applied by the render that follows the geometry change, exactly once"
    );
}

#[test]
fn a_new_root_widget_does_not_keep_the_old_roots_region() {
    let (log, _g) = install(1.0);
    let mut win = window(Rect::new(0, 0, 100, 60), WindowFlags::FRAMELESS);
    let handle = win.native_handle();
    win.show();
    win.set_mask(&Region::from_rect(Rect::new(0, 0, 50, 50)));

    win.set_root_widget(colored(Rect::new(0, 0, 100, 60), red()));
    win.render_and_present();

    assert!(win.mask().is_empty());
    let masks = log.lock().unwrap().masks_of(handle);
    assert!(
        masks.last().unwrap().is_empty(),
        "the platform window is released from the old mask"
    );
}

#[test]
fn a_window_is_not_painted_outside_its_mask() {
    let (_log, _g) = install(1.0);
    let mut win = window(Rect::new(0, 0, 40, 40), WindowFlags::FRAMELESS);
    win.set_root_widget(colored(Rect::new(0, 0, 40, 40), red()));
    win.set_mask(&Region::from_rect(Rect::new(0, 0, 20, 40)));
    win.show();

    assert_eq!(alpha_at(&win, 10, 10), 255, "inside the mask");
    assert_eq!(
        alpha_at(&win, 30, 10),
        0,
        "outside the mask nothing is drawn"
    );

    win.clear_mask();
    win.render_and_present();
    assert_eq!(
        alpha_at(&win, 30, 10),
        255,
        "the area the mask uncovered is repainted"
    );
}

// ---- QWidget::setMask on a child widget ------------------------------------------------------

#[test]
fn a_child_is_painted_only_inside_its_mask_and_repainted_when_it_changes() {
    let (_log, _g) = install(1.0);
    let mut win = window(Rect::new(0, 0, 100, 100), WindowFlags::FRAMELESS);
    let child = colored(Rect::new(10, 10, 60, 60), red());
    win.root_widget().borrow_mut().add_child(child.clone());
    child
        .borrow()
        .set_mask(&Region::from_rect(Rect::new(0, 0, 30, 60)));
    win.show();

    assert_eq!(alpha_at(&win, 20, 20), 255, "inside the child's mask");
    assert_eq!(
        alpha_at(&win, 50, 20),
        0,
        "inside the child's rect but outside its mask"
    );

    child.borrow().clear_mask();
    win.render_and_present();
    assert_eq!(
        alpha_at(&win, 50, 20),
        255,
        "clearing the mask repaints the exposed area"
    );

    child
        .borrow()
        .set_mask(&Region::from_rect(Rect::new(0, 0, 30, 60)));
    win.render_and_present();
    assert_eq!(
        alpha_at(&win, 50, 20),
        0,
        "the area a new mask leaves out is repainted by the parent"
    );
}

#[test]
fn a_masked_child_receives_the_mouse_only_inside_its_mask() {
    let (_log, _g) = install(1.0);
    let win = window(Rect::new(0, 0, 100, 100), WindowFlags::FRAMELESS);
    let child = colored(Rect::new(10, 10, 60, 60), red());
    let root = win.root_widget();
    root.borrow_mut().add_child(child.clone());
    child
        .borrow()
        .set_mask(&Region::from_rect(Rect::new(0, 0, 30, 60)));
    let child_id = child.borrow().id();
    let root_id = root.borrow().id();

    let hit = |x, y| {
        let (widget, _) = ferrokit_widgets::hit_test::hit_test(&root, Point::new(x, y))
            .expect("inside the window");
        let id = widget.borrow().id();
        id
    };
    assert_eq!(hit(20, 20), child_id, "inside the mask");
    assert_eq!(
        hit(60, 20),
        root_id,
        "inside the child's rect, outside its mask: the parent"
    );

    // WA_MouseNoMask: the whole rect takes the mouse again.
    child
        .borrow()
        .set_attribute(WidgetAttribute::MouseNoMask, true);
    assert_eq!(hit(60, 20), child_id);
    assert!(child.borrow().test_attribute(WidgetAttribute::MouseNoMask));
}

#[test]
fn a_child_mask_is_not_a_window_region() {
    let (log, _g) = install(1.0);
    let win = window(Rect::new(0, 0, 100, 100), WindowFlags::FRAMELESS);
    let handle = win.native_handle();
    let child = colored(Rect::new(10, 10, 60, 60), red());
    win.root_widget().borrow_mut().add_child(child.clone());
    ferrokit_widgets::adopt_tree(&win.root_widget());

    child
        .borrow()
        .set_mask(&Region::from_rect(Rect::new(0, 0, 30, 60)));

    assert!(log.lock().unwrap().masks_of(handle).is_empty());
    assert_eq!(
        child.borrow().mask(),
        Region::from_rect(Rect::new(0, 0, 30, 60))
    );
}

fn channels_at(window: &Window, x: u32, y: u32) -> (u8, u8, u8, u8) {
    let p = window
        .backing_store()
        .pixmap()
        .pixel(x, y)
        .expect("pixel inside the store");
    (p.red(), p.green(), p.blue(), p.alpha())
}

/// A masked child is painted once however many rectangles its mask has, and the parent's pixels
/// inside the child's rect but outside its mask are left as they were.
#[test]
fn a_scanline_mask_paints_the_child_once_and_keeps_the_parent_beneath() {
    use ferrokit_gui::geometry::primitives::RectF;
    let (_log, _g) = install(1.0);
    let mut win = window(Rect::new(0, 0, 100, 100), WindowFlags::FRAMELESS);
    win.set_root_widget(colored(
        Rect::new(0, 0, 100, 100),
        Color::from_rgba8(0, 255, 0, 255),
    ));
    let paints = Rc::new(std::cell::Cell::new(0u32));
    let counter = Rc::clone(&paints);
    let child: WidgetRef = Rc::new(RefCell::new(Box::new(
        EmptyWidget::with_geometry(Rect::new(10, 10, 60, 60)).with_paint_handler(move |p| {
            counter.set(counter.get() + 1);
            p.fill_rect(RectF::new(0.0, 0.0, 60.0, 60.0), red());
        }),
    )));
    win.root_widget().borrow_mut().add_child(child.clone());
    // A rounded rectangle as pixel rows: eight rectangles.
    let mut mask = Region::new();
    for (i, inset) in [9, 6, 4, 3, 2, 1].into_iter().enumerate() {
        mask = mask.united_rect(&Rect::new(inset, i as i32, 60 - 2 * inset, 1));
    }
    mask = mask.united_rect(&Rect::new(0, 6, 60, 46));
    assert!(mask.rect_count() >= 7, "a scanline-style mask");
    child.borrow().set_mask(&mask);
    win.show();

    assert_eq!(
        paints.get(),
        1,
        "one paint event per frame, not one per mask rectangle"
    );
    assert_eq!(
        channels_at(&win, 40, 40),
        (255, 0, 0, 255),
        "the child inside its mask"
    );
    assert_eq!(
        channels_at(&win, 10, 10),
        (0, 255, 0, 255),
        "the cut corner of the child shows the parent beneath it"
    );
}

/// A mask that grows repaints the area it newly covers: the window's render cleared everything
/// outside the old mask.
#[test]
fn a_window_mask_that_grows_repaints_the_area_it_uncovers() {
    let (_log, _g) = install(1.0);
    let mut win = window(Rect::new(0, 0, 40, 40), WindowFlags::FRAMELESS);
    win.set_root_widget(colored(Rect::new(0, 0, 40, 40), red()));
    win.set_mask(&Region::from_rect(Rect::new(0, 0, 20, 40)));
    win.show();
    assert_eq!(alpha_at(&win, 30, 10), 0);

    win.set_mask(&Region::from_rect(Rect::new(0, 0, 40, 40)));
    win.render_and_present();

    assert_eq!(
        alpha_at(&win, 30, 10),
        255,
        "no blank band where the mask grew"
    );
}

/// Rects of a mask that share an edge in device-independent pixels share it in native pixels:
/// at 125% the edges are scaled and rounded, not the origins and sizes (which left gaps).
#[test]
fn the_native_region_has_no_gaps_at_a_fractional_ratio() {
    let (log, _g) = install(1.25);
    let mut win = window(Rect::new(0, 0, 100, 60), WindowFlags::FRAMELESS);
    let handle = win.native_handle();
    let mask = Region::from_rects(&[
        Rect::new(0, 0, 10, 1),
        Rect::new(1, 1, 8, 1),
        Rect::new(2, 2, 6, 1),
        Rect::new(3, 3, 4, 1),
    ]);

    win.set_mask(&mask);

    let native = log
        .lock()
        .unwrap()
        .masks_of(handle)
        .pop()
        .expect("a native region");
    // Logical rows 0..4 are native rows 0..5; columns 4..9 are inside every band.
    for y in 0..5 {
        assert!(
            native.contains_point(Point::new(6, y)),
            "native row {y} is missing from the region"
        );
    }
    assert_eq!(native.bounding_rect().bottom(), 5);
}

// ---- top-level windows with a parent ---------------------------------------------------------

fn tool_flags() -> WindowFlags {
    WindowFlags::TOOL | WindowFlags::FRAMELESS | WindowFlags::STAYS_ON_TOP
}

#[test]
fn a_tool_window_with_a_parent_is_its_own_native_window_owned_by_the_parent() {
    let (log, _g) = install(1.0);
    let parent = window(Rect::new(0, 0, 300, 200), WindowFlags::NORMAL);

    let tool = Window::with_parent(&parent, "flyout", Rect::new(20, 20, 120, 80), tool_flags())
        .expect("tool window");

    assert_ne!(
        tool.native_handle(),
        parent.native_handle(),
        "its own native window"
    );
    let log = log.lock().unwrap();
    assert_eq!(
        log.created.last().copied(),
        Some(tool_flags()),
        "created with the tool flags"
    );
    assert_eq!(
        log.transient,
        vec![(tool.native_handle(), Some(parent.native_handle()))],
        "the platform window is owned by the parent's native window"
    );
    drop(log);
    assert_eq!(tool.transient_parent(), Some(parent.id()));
    assert_eq!(parent.transient_parent(), None);
}

#[test]
fn the_transient_parent_can_be_changed_and_removed() {
    let (log, _g) = install(1.0);
    let a = window(Rect::new(0, 0, 100, 100), WindowFlags::NORMAL);
    let b = window(Rect::new(0, 0, 100, 100), WindowFlags::NORMAL);
    let mut tool = window(Rect::new(0, 0, 50, 50), tool_flags());

    tool.set_transient_parent(Some(&a));
    tool.set_transient_parent(Some(&b));
    assert_eq!(tool.transient_parent(), Some(b.id()));
    tool.set_transient_parent(None);
    assert_eq!(tool.transient_parent(), None);

    assert_eq!(
        log.lock().unwrap().transient,
        vec![
            (tool.native_handle(), Some(a.native_handle())),
            (tool.native_handle(), Some(b.native_handle())),
            (tool.native_handle(), None),
        ]
    );
}

/// `QWindow::setTransientParent` refuses a window that is its own ancestor.
#[test]
fn a_transient_cycle_is_refused() {
    let (log, _g) = install(1.0);
    let mut a = window(Rect::new(0, 0, 100, 100), WindowFlags::NORMAL);
    let mut b = window(Rect::new(0, 0, 100, 100), WindowFlags::NORMAL);
    b.set_transient_parent(Some(&a));

    a.set_transient_parent(Some(&b));

    assert_eq!(a.transient_parent(), None);
    assert_eq!(b.transient_parent(), Some(a.id()));
    assert_eq!(
        log.lock().unwrap().transient.len(),
        1,
        "the platform window was not touched"
    );
}

#[test]
fn dropping_the_parent_ends_the_relationship_and_leaves_the_tool_window() {
    let (_log, _g) = install(1.0);
    let parent = window(Rect::new(0, 0, 100, 100), WindowFlags::NORMAL);
    let tool = Window::with_parent(&parent, "t", Rect::new(0, 0, 50, 50), tool_flags()).unwrap();

    drop(parent);

    assert_eq!(tool.transient_parent(), None, "the parent is gone");
    assert!(
        Application::top_level_windows().contains(&tool.id()),
        "the tool window lives on"
    );
}

/// A window with a transient parent is a secondary window (`participatesInLastWindowClosed`):
/// closing it is not "the last window closed", and it does not keep the application alive.
#[test]
fn a_window_with_a_transient_parent_does_not_take_part_in_last_window_closed() {
    let (_log, _g) = install(1.0);
    Application::reset_for_test();
    let app = Application::new(vec!["test_app".to_string()]);
    ferrokit_gui::application::GuiApplication::set_quit_on_last_window_closed(false);
    let emitted = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&emitted);
    let _conn = app
        .gui_application()
        .last_window_closed()
        .connect(move |()| {
            counter.fetch_add(1, Ordering::SeqCst);
        });

    let mut parent = window(Rect::new(0, 0, 300, 200), WindowFlags::NORMAL);
    let mut tool =
        Window::with_parent(&parent, "t", Rect::new(0, 0, 50, 50), tool_flags()).unwrap();
    parent.show();
    tool.show();

    // Closing the tool window leaves the primary window: not the last window.
    tool.close();
    assert_eq!(emitted.load(Ordering::SeqCst), 0);

    // With the tool window shown again, the primary window closing is the last primary window.
    tool.show();
    parent.close();
    assert_eq!(
        emitted.load(Ordering::SeqCst),
        1,
        "a visible tool window does not keep the application alive"
    );
}

#[test]
fn a_tool_window_without_a_parent_is_a_primary_window() {
    let (_log, _g) = install(1.0);
    Application::reset_for_test();
    let app = Application::new(vec!["test_app".to_string()]);
    ferrokit_gui::application::GuiApplication::set_quit_on_last_window_closed(false);
    let emitted = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&emitted);
    let _conn = app
        .gui_application()
        .last_window_closed()
        .connect(move |()| {
            counter.fetch_add(1, Ordering::SeqCst);
        });

    let mut a = window(Rect::new(0, 0, 100, 100), WindowFlags::NORMAL);
    let mut tool = window(Rect::new(0, 0, 50, 50), tool_flags());
    a.show();
    tool.show();
    a.close();
    assert_eq!(
        emitted.load(Ordering::SeqCst),
        0,
        "the parentless tool window is still open"
    );
    tool.close();
    assert_eq!(emitted.load(Ordering::SeqCst), 1);
}
