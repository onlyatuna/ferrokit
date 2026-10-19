// SPDX-License-Identifier: LGPL-3.0-only
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::{Arc, LazyLock, RwLock};

use crate::style::stylesheet::QStyleSheetStyle;

use ferrokit_core::application::CoreApplication;
use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::event_loop::filters_consume;
use ferrokit_core::meta::MetaObject;
use ferrokit_core::object::{ObjectData, ObjectId, QObject};
use ferrokit_core::signal::Signal;
use ferrokit_gui::application::GuiApplication;
use ferrokit_gui::paint::palette::Palette;
use ferrokit_gui::text::font::Font;

use crate::widget::WidgetRef;

/// Closes a top-level window (`QWidget::close`); returns false when the window refuses.
type TopLevelCloser = Rc<dyn Fn() -> bool>;

thread_local! {
    static TOP_LEVEL_WINDOWS: RefCell<Vec<(ObjectId, TopLevelCloser)>> = const { RefCell::new(Vec::new()) };
    static VISIBLE_WINDOWS: RefCell<Vec<ObjectId>> = const { RefCell::new(Vec::new()) };
    static ACTIVE_WINDOW_ID: RefCell<Option<ObjectId>> = const { RefCell::new(None) };
    static FOCUS_WIDGET: RefCell<Option<WidgetRef>> = const { RefCell::new(None) };
    static QUITTING: Cell<bool> = const { Cell::new(false) };
}

static GLOBAL_CURSOR_FLASH_TIME_MS: RwLock<u32> = RwLock::new(1000);
static GLOBAL_WHEEL_SCROLL_LINES: RwLock<i32> = RwLock::new(3);
static GLOBAL_START_DRAG_DISTANCE: RwLock<i32> = RwLock::new(10);
static GLOBAL_START_DRAG_TIME_MS: RwLock<u32> = RwLock::new(500);
static GLOBAL_STYLESHEET: LazyLock<RwLock<Option<Arc<QStyleSheetStyle>>>> =
    LazyLock::new(|| RwLock::new(None));

/// The highest device pixel ratio of any screen (`QGuiApplication::devicePixelRatio`).
fn max_device_pixel_ratio() -> f32 {
    ferrokit_platform::platform()
        .screens()
        .iter()
        .map(|screen| screen.device_pixel_ratio())
        .fold(1.0, f32::max)
}

/// Full widget application singleton equivalent to Qt's `QApplication`.
///
/// Inherits and encapsulates `GuiApplication` and `CoreApplication`, adding:
/// - Top-level window registry & management (`topLevelWindows`)
/// - Focus widget & active window tracking (`focusWidget`, `activeWindow`)
/// - Interactive timing parameters (double-click interval, wheel scroll lines, drag thresholds)
/// - Desktop integration & shutdown coordination
pub struct Application {
    gui_app: GuiApplication,
    focus_changed: Signal<(Option<ObjectId>, Option<ObjectId>)>,
}

impl Application {
    /// Initializes a new `Application` instance.
    pub fn new(args: Vec<String>) -> Self {
        let gui_app = GuiApplication::new(args);
        // Window veto for the application object's Quit (`QApplication::event(Quit)`).
        ferrokit_core::application::set_quit_gate(Self::quit_gate);

        // `QGuiApplication::devicePixelRatio()`: the highest ratio of any screen. Qt's Windows font
        // database picks its font engine, and with it the advances text is laid out with, from it.
        // Refreshed on every screen change, as Qt does when screens are added or removed.
        ferrokit_gui::text::font_database::set_application_device_pixel_ratio(max_device_pixel_ratio());
        ferrokit_platform::platform()
            .screen_changed()
            .connect(|_: &()| {
                ferrokit_gui::text::font_database::set_application_device_pixel_ratio(
                    max_device_pixel_ratio(),
                );
            });

        let focus_changed = Signal::with_owner(gui_app.object_data());
        Self {
            gui_app,
            focus_changed,
        }
    }
    /// Sets the application-wide style sheet (`QApplication::setStyleSheet`).
    ///
    /// Like `QStyleSheetStyle::repolish(qApp)`, it tells every widget of every window about the
    /// change (`StyleChange`), so existing widgets repaint and relayout.
    pub fn set_style_sheet(qss: &str) {
        let style = if qss.trim().is_empty() {
            None
        } else {
            Some(Arc::new(QStyleSheetStyle::parse(qss)))
        };
        if let Ok(mut lock) = GLOBAL_STYLESHEET.write() {
            *lock = style;
        }
        for root in crate::window::window_roots() {
            if let Ok(widget) = root.try_borrow() {
                widget.widget_base().style_changed();
                // The root is only shared-borrowed here, so its own `event()` cannot run; its
                // filters still see the StyleChange (qwidget.cpp:2724-2725).
                filters_consume(
                    widget.object_data(),
                    &mut Event::new(EventKind::StyleChange),
                );
                crate::widget::style_changed_below(widget.children());
            }
        }
    }

    /// Retrieves the application-wide style sheet if set.
    pub fn style_sheet() -> Option<Arc<QStyleSheetStyle>> {
        GLOBAL_STYLESHEET.read().ok().and_then(|lock| lock.clone())
    }

    /// Access the underlying `GuiApplication`.
    pub fn gui_application(&self) -> &GuiApplication {
        &self.gui_app
    }

    /// Access the underlying mutable `GuiApplication`.
    pub fn gui_application_mut(&mut self) -> &mut GuiApplication {
        &mut self.gui_app
    }

    /// Access the underlying `CoreApplication`.
    pub fn core_application(&self) -> &CoreApplication {
        self.gui_app.core_application()
    }

    /// Access the underlying mutable `CoreApplication`.
    pub fn core_application_mut(&mut self) -> &mut CoreApplication {
        self.gui_app.core_application_mut()
    }

    /// Enters the main event loop and returns the exit code.
    pub fn exec(&mut self) -> i32 {
        self.gui_app.exec()
    }

    /// Exits the application with an exit code.
    pub fn exit(return_code: i32) {
        GuiApplication::exit(return_code);
    }

    /// Tells the application to quit cleanly (`QCoreApplication::quit`, qcoreapplication.cpp:2138-2147).
    ///
    /// The Quit is an event to the application object, where the window veto (`quit_gate`) runs
    /// before exit (qapplication.cpp:1675-1697). `exit` is not gated.
    pub fn quit() {
        GuiApplication::quit();
    }

    /// The window veto for the application object's Quit (`QApplication::event(Quit)`,
    /// qapplication.cpp:1679-1697): closes the visible top-level windows and accepts the quit only
    /// when none refused and none remains visible. A Quit re-entered from a window's close handler
    /// is refused, and the outer call decides.
    fn quit_gate() -> bool {
        if QUITTING.try_with(|q| q.replace(true)).unwrap_or(true) {
            return false;
        }
        let closed = Self::try_close_all_top_level_windows();
        let _ = QUITTING.try_with(|q| q.set(false));
        closed && !Self::any_window_visible()
    }

    /// Closes the visible top-level windows one by one, restarting the scan after each close since
    /// closing can change the list (`QApplicationPrivate::tryCloseAllWidgetWindows`,
    /// qapplication.cpp:1591-1617). Stops at the first refusal and returns false.
    fn try_close_all_top_level_windows() -> bool {
        let mut attempted: Vec<ObjectId> = Vec::new();
        loop {
            let registered = TOP_LEVEL_WINDOWS
                .try_with(|wins| wins.borrow().clone())
                .unwrap_or_default();
            let next = registered
                .into_iter()
                .find(|(id, _)| !attempted.contains(id) && Self::is_window_visible(*id));
            let Some((id, close)) = next else {
                return true;
            };
            attempted.push(id);
            if !close() {
                return false;
            }
        }
    }

    /// Whether any top-level window is shown.
    fn any_window_visible() -> bool {
        VISIBLE_WINDOWS
            .try_with(|vis| !vis.borrow().is_empty())
            .unwrap_or(false)
    }

    /// Registers a top-level window with the application, with the function that closes it
    /// (`QWidget::close`, returning false when the window refuses). `quit` closes through it.
    pub fn register_window(window_id: ObjectId, close: TopLevelCloser) {
        let _ = TOP_LEVEL_WINDOWS.try_with(|wins| {
            let mut list = wins.borrow_mut();
            if !list.iter().any(|(id, _)| *id == window_id) {
                list.push((window_id, close));
            }
        });
    }

    /// Unregisters a top-level window when it is destroyed. Destroying a window is not closing it:
    /// Qt's `~QWindow` neither emits `lastWindowClosed` nor quits (`qwindow.cpp:175-208`).
    pub fn unregister_window(window_id: ObjectId) {
        let removed = TOP_LEVEL_WINDOWS
            .try_with(|wins| {
                let mut list = wins.borrow_mut();
                list.iter()
                    .position(|(id, _)| *id == window_id)
                    .map(|i| list.remove(i))
            })
            .ok()
            .flatten();
        drop(removed);
        let _ = VISIBLE_WINDOWS.try_with(|vis| vis.borrow_mut().retain(|&id| id != window_id));
        let _ = ACTIVE_WINDOW_ID.try_with(|act| {
            let mut curr = act.borrow_mut();
            if *curr == Some(window_id) {
                *curr = None;
            }
        });
    }

    /// Records whether a top-level window is shown (`QWindow::isVisible`).
    pub(crate) fn set_window_visible(window_id: ObjectId, visible: bool) {
        let _ = VISIBLE_WINDOWS.try_with(|vis| {
            let mut list = vis.borrow_mut();
            list.retain(|&id| id != window_id);
            if visible {
                list.push(window_id);
            }
        });
    }

    /// Whether a top-level window is shown. An unregistered window is not.
    pub(crate) fn is_window_visible(window_id: ObjectId) -> bool {
        VISIBLE_WINDOWS
            .try_with(|vis| vis.borrow().contains(&window_id))
            .unwrap_or(false)
    }

    /// `QGuiApplicationPrivate::maybeLastWindowClosed` (qguiapplication.cpp:3933-3943), run after a
    /// visible window has closed: when no visible top-level window is left, emits `lastWindowClosed`.
    /// If `quitOnLastWindowClosed` is set, the application quits. `quit` does nothing outside `exec`,
    /// as `canQuitAutomatically` requires (qcoreapplication.cpp:2074-2089).
    pub(crate) fn maybe_last_window_closed() {
        if !Self::any_window_visible() {
            GuiApplication::emit_last_window_closed();
            if GuiApplication::quit_on_last_window_closed() {
                Self::quit();
            }
        }
    }

    /// Returns a list of all active top-level window object IDs.
    pub fn top_level_windows() -> Vec<ObjectId> {
        TOP_LEVEL_WINDOWS
            .try_with(|wins| wins.borrow().iter().map(|(id, _)| *id).collect())
            .unwrap_or_default()
    }

    /// Returns the currently active window object ID.
    pub fn active_window() -> Option<ObjectId> {
        ACTIVE_WINDOW_ID
            .try_with(|act| *act.borrow())
            .unwrap_or(None)
    }

    /// Sets the active window object ID and notifies `focus_window_changed`.
    pub fn set_active_window(window_id: Option<ObjectId>) {
        let changed = ACTIVE_WINDOW_ID
            .try_with(|act| {
                let mut current = act.borrow_mut();
                if *current != window_id {
                    *current = window_id;
                    true
                } else {
                    false
                }
            })
            .unwrap_or(false);
        if changed {
            GuiApplication::emit_focus_window_changed(window_id);
        }
    }

    // --- Focus Management ---

    /// Returns the currently focused widget reference if any.
    pub fn focus_widget() -> Option<WidgetRef> {
        FOCUS_WIDGET
            .try_with(|fw| fw.borrow().clone())
            .unwrap_or(None)
    }

    /// Sets the focused widget and emits focus change notifications.
    pub fn set_focus_widget(&self, widget: Option<WidgetRef>) {
        let (old_id, new_id) = FOCUS_WIDGET
            .try_with(|fw| {
                let mut current = fw.borrow_mut();
                let old_id = current.as_ref().map(|w| w.borrow().id());
                let new_id = widget.as_ref().map(|w| w.borrow().id());
                *current = widget;
                (old_id, new_id)
            })
            .unwrap_or((None, None));

        if old_id != new_id {
            self.focus_changed.emit(&(old_id, new_id));
        }
    }

    /// Signal emitted when the focused widget changes.
    pub fn focus_changed(&self) -> &Signal<(Option<ObjectId>, Option<ObjectId>)> {
        &self.focus_changed
    }

    // --- Global Palette & Font Passthrough ---

    pub fn palette() -> Palette {
        GuiApplication::palette()
    }

    pub fn set_palette(palette: Palette) {
        GuiApplication::set_palette(palette);
    }

    pub fn font() -> Font {
        GuiApplication::font()
    }

    pub fn set_font(font: Font) {
        GuiApplication::set_font(font);
    }

    // --- Interactive Settings ---

    pub fn double_click_interval() -> u32 {
        ferrokit_gui::application::GuiApplication::style_hints().mouse_double_click_interval_ms
    }

    /// `QApplication::setDoubleClickInterval` writes the style hint (`qapplication.cpp:3486-3488`).
    pub fn set_double_click_interval(ms: u32) {
        let mut hints = ferrokit_gui::application::GuiApplication::style_hints();
        hints.mouse_double_click_interval_ms = ms;
        ferrokit_gui::application::GuiApplication::set_style_hints(hints);
    }

    pub fn cursor_flash_time() -> u32 {
        *GLOBAL_CURSOR_FLASH_TIME_MS.read().unwrap()
    }

    pub fn set_cursor_flash_time(ms: u32) {
        *GLOBAL_CURSOR_FLASH_TIME_MS.write().unwrap() = ms;
    }

    pub fn wheel_scroll_lines() -> i32 {
        *GLOBAL_WHEEL_SCROLL_LINES.read().unwrap()
    }

    pub fn set_wheel_scroll_lines(lines: i32) {
        *GLOBAL_WHEEL_SCROLL_LINES.write().unwrap() = lines;
    }

    pub fn start_drag_distance() -> i32 {
        *GLOBAL_START_DRAG_DISTANCE.read().unwrap()
    }

    pub fn set_start_drag_distance(l: i32) {
        *GLOBAL_START_DRAG_DISTANCE.write().unwrap() = l;
    }

    pub fn start_drag_time() -> u32 {
        *GLOBAL_START_DRAG_TIME_MS.read().unwrap()
    }

    pub fn set_start_drag_time(ms: u32) {
        *GLOBAL_START_DRAG_TIME_MS.write().unwrap() = ms;
    }

    /// Resets all application singletons and thread-local state for tests.
    #[doc(hidden)]
    pub fn reset_for_test() {
        GuiApplication::reset_for_test();
        let removed = TOP_LEVEL_WINDOWS.with(|wins| std::mem::take(&mut *wins.borrow_mut()));
        drop(removed);
        QUITTING.with(|q| q.set(false));
        VISIBLE_WINDOWS.with(|vis| vis.borrow_mut().clear());
        ACTIVE_WINDOW_ID.with(|act| *act.borrow_mut() = None);
        FOCUS_WIDGET.with(|fw| *fw.borrow_mut() = None);
        let mut hints = ferrokit_gui::application::GuiApplication::style_hints();
        hints.mouse_double_click_interval_ms = 400;
        ferrokit_gui::application::GuiApplication::set_style_hints(hints);
        *GLOBAL_CURSOR_FLASH_TIME_MS.write().unwrap() = 1000;
        *GLOBAL_WHEEL_SCROLL_LINES.write().unwrap() = 3;
        *GLOBAL_START_DRAG_DISTANCE.write().unwrap() = 10;
        *GLOBAL_START_DRAG_TIME_MS.write().unwrap() = 500;
        *GLOBAL_STYLESHEET.write().unwrap() = None;
    }
}

static APP_META_OBJECT: MetaObject = MetaObject::new(
    "QApplication",
    Some(&ferrokit_core::meta::QOBJECT_META_OBJECT),
    &[],
    &[],
    &[],
    &[],
);

impl QObject for Application {
    fn object_data(&self) -> &ObjectData {
        self.gui_app.object_data()
    }

    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        self.gui_app.object_data_mut()
    }

    fn as_qobject_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_qobject_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn meta_object(&self) -> &'static MetaObject {
        &APP_META_OBJECT
    }
}
