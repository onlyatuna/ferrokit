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

use crate::widget::{WidgetRef, WidgetWeak};

/// Closes a top-level window (`QWidget::close`); returns false when the window refuses.
type TopLevelCloser = Rc<dyn Fn() -> bool>;

thread_local! {
    static TOP_LEVEL_WINDOWS: RefCell<Vec<(ObjectId, TopLevelCloser)>> = const { RefCell::new(Vec::new()) };
    static VISIBLE_WINDOWS: RefCell<Vec<ObjectId>> = const { RefCell::new(Vec::new()) };
    static ACTIVE_WINDOW_ID: RefCell<Option<ObjectId>> = const { RefCell::new(None) };
    static FOCUS_ENTRIES: RefCell<Vec<FocusEntry>> = const { RefCell::new(Vec::new()) };
    static FOCUS_SEQ: Cell<u64> = const { Cell::new(0) };
    static FOCUS_QUEUE: RefCell<Vec<Option<(Option<ObjectId>, Option<ObjectId>)>>> = const { RefCell::new(Vec::new()) };
    static FOCUS_CHANGED: RefCell<Option<FocusChangedSignal>> = const { RefCell::new(None) };
    static QUITTING: Cell<bool> = const { Cell::new(false) };
    static OVERRIDE_CURSORS: RefCell<Vec<ferrokit_platform::cursor::CursorShape>> = const { RefCell::new(Vec::new()) };
}

type FocusChangedSignal = Signal<(Option<ObjectId>, Option<ObjectId>)>;

/// The focus widget of one window's focus state, held weakly so a dropped window is not kept
/// alive. `owner` identifies the focus state (0 for `set_focus_widget`); `window` is the window it
/// belongs to, if any. `Application::focus_widget` is the one of the active window.
struct FocusEntry {
    owner: usize,
    window: Option<ObjectId>,
    focus: Option<(ObjectId, WidgetWeak)>,
    seq: u64,
}

/// A focus change reserved by `begin_focus_publish`; `finish_focus_publish` emits it.
pub(crate) struct FocusTicket {
    slot: usize,
    old: Option<ObjectId>,
    new: Option<ObjectId>,
}

/// The entry `QApplication::focusWidget` reports: among the entries of the active window (or of no
/// window, or all when no window is active), the one updated last.
fn visible_focus(entries: &[FocusEntry], active: Option<ObjectId>) -> Option<&FocusEntry> {
    entries
        .iter()
        .filter(|e| active.is_none() || e.window.is_none() || e.window == active)
        .max_by_key(|e| e.seq)
}

fn visible_focus_id(entries: &[FocusEntry]) -> Option<ObjectId> {
    let active = ACTIVE_WINDOW_ID.try_with(|a| *a.borrow()).unwrap_or(None);
    visible_focus(entries, active).and_then(|e| e.focus.as_ref().map(|(id, _)| *id))
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

impl Drop for Application {
    fn drop(&mut self) {
        let _ = FOCUS_CHANGED.try_with(|slot| *slot.borrow_mut() = None);
    }
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
        let shared = focus_changed.clone();
        let _ = FOCUS_CHANGED.try_with(|slot| *slot.borrow_mut() = Some(shared));
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
        let before = Self::visible_focus_id();
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
            let ticket = FocusTicket { slot: Self::reserve_focus_slot(), old: before, new: Self::visible_focus_id() };
            Self::finish_focus_publish(ticket);
        }
    }

    // --- Focus Management ---

    /// Returns the widget that has the keyboard focus (`QApplication::focusWidget`), if any: the
    /// focus widget of the active window.
    ///
    /// Window focus managers keep it up to date; it holds the widget weakly, so it is `None` once
    /// the widget or its window is gone.
    pub fn focus_widget() -> Option<WidgetRef> {
        let active = ACTIVE_WINDOW_ID.try_with(|a| *a.borrow()).unwrap_or(None);
        FOCUS_ENTRIES
            .try_with(|entries| {
                let entries = entries.borrow();
                visible_focus(&entries, active)
                    .and_then(|e| e.focus.as_ref())
                    .and_then(|(_, widget)| widget.upgrade())
            })
            .unwrap_or(None)
    }

    /// Sets the focused widget and emits `focus_changed`.
    ///
    /// Window focus managers do this on their own on every focus change; this is for code that
    /// tracks the focus without one. The widget is held weakly.
    pub fn set_focus_widget(&self, widget: Option<WidgetRef>) {
        let new = widget.map(|w| (w.borrow().id(), Rc::downgrade(&w)));
        let ticket = Self::begin_focus_publish(0, None, new);
        Self::finish_focus_publish(ticket);
    }

    fn visible_focus_id() -> Option<ObjectId> {
        FOCUS_ENTRIES
            .try_with(|entries| visible_focus_id(&entries.borrow()))
            .unwrap_or(None)
    }

    fn reserve_focus_slot() -> usize {
        FOCUS_QUEUE
            .try_with(|q| {
                let mut q = q.borrow_mut();
                q.push(None);
                q.len() - 1
            })
            .unwrap_or(0)
    }

    /// Records the focus widget of a window's focus state (`owner`, in `window`) now, and reserves
    /// the place of its `focus_changed` emission. The state is current while the focus events run,
    /// but `finish_focus_publish` emits only after them, and in the order the changes began even
    /// when a handler changes the focus again.
    pub(crate) fn begin_focus_publish(
        owner: usize,
        window: Option<ObjectId>,
        new: Option<(ObjectId, WidgetWeak)>,
    ) -> FocusTicket {
        let old = Self::visible_focus_id();
        let seq = FOCUS_SEQ
            .try_with(|s| {
                s.set(s.get() + 1);
                s.get()
            })
            .unwrap_or(0);
        let _ = FOCUS_ENTRIES.try_with(|entries| {
            let mut entries = entries.borrow_mut();
            let entry = FocusEntry { owner, window, focus: new, seq };
            match entries.iter_mut().find(|e| e.owner == owner) {
                Some(slot) => *slot = entry,
                None => entries.push(entry),
            }
        });
        FocusTicket { slot: Self::reserve_focus_slot(), old, new: Self::visible_focus_id() }
    }

    /// The window whose focus state is `owner` is gone: its focus widget is cleared.
    pub(crate) fn focus_owner_dropped(owner: usize) {
        let old = Self::visible_focus_id();
        let removed = FOCUS_ENTRIES
            .try_with(|entries| {
                let mut entries = entries.borrow_mut();
                let before = entries.len();
                entries.retain(|e| e.owner != owner);
                entries.len() != before
            })
            .unwrap_or(false);
        if removed {
            let ticket = FocusTicket { slot: Self::reserve_focus_slot(), old, new: Self::visible_focus_id() };
            Self::finish_focus_publish(ticket);
        }
    }

    /// Emits `focus_changed` for `ticket` once it and every change begun before it are done.
    pub(crate) fn finish_focus_publish(ticket: FocusTicket) {
        let ready = FOCUS_QUEUE
            .try_with(|q| {
                let mut q = q.borrow_mut();
                if let Some(slot) = q.get_mut(ticket.slot) {
                    *slot = Some((ticket.old, ticket.new));
                }
                if q.iter().all(Option::is_some) {
                    std::mem::take(&mut *q)
                } else {
                    Vec::new()
                }
            })
            .unwrap_or_default();
        for (old, new) in ready.into_iter().flatten() {
            Self::emit_focus_changed(old, new);
        }
    }

    fn emit_focus_changed(old_id: Option<ObjectId>, new_id: Option<ObjectId>) {
        if old_id == new_id {
            return;
        }
        let signal = FOCUS_CHANGED
            .try_with(|slot| slot.borrow().clone())
            .unwrap_or(None);
        if let Some(signal) = signal {
            signal.emit(&(old_id, new_id));
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

    /// `QGuiApplication::mouseButtons` (qguiapplication.cpp:1974): the buttons the application has
    /// seen pressed and not yet released, as of the last mouse event.
    pub fn mouse_buttons() -> ferrokit_core::event::MouseButtons {
        ferrokit_platform::window::app_mouse_buttons()
    }

    /// `QGuiApplication::overrideCursor` (qguiapplication.cpp:4344-4348): the active override
    /// cursor, `None` when none was set.
    pub fn override_cursor() -> Option<ferrokit_platform::cursor::CursorShape> {
        OVERRIDE_CURSORS.with(|s| s.borrow().last().copied())
    }

    /// `QGuiApplication::setOverrideCursor` (qguiapplication.cpp:4449-4456): pushes `shape` on the
    /// override stack; it shows in all the application's windows until `restore_override_cursor`
    /// or another `set_override_cursor`. A backend with override cursors (Windows) shows it
    /// itself; any other applies it to every window, as Qt does for plugins without them.
    pub fn set_override_cursor(shape: ferrokit_platform::cursor::CursorShape) {
        OVERRIDE_CURSORS.with(|s| s.borrow_mut().push(shape));
        Self::apply_override_cursor(Some(shape));
    }

    /// `QGuiApplication::changeOverrideCursor` (qguiapplication.cpp:4358-4365): replaces the active
    /// override cursor; nothing happens while none is set.
    pub fn change_override_cursor(shape: ferrokit_platform::cursor::CursorShape) {
        let had = OVERRIDE_CURSORS.with(|s| s.borrow_mut().pop().is_some());
        if had {
            Self::set_override_cursor(shape);
        }
    }

    /// `QGuiApplication::restoreOverrideCursor` (qguiapplication.cpp:4470-4487): pops the active
    /// override cursor, which shows the one below it, or the widgets' cursors again when it was
    /// the last. Nothing happens while none is set.
    pub fn restore_override_cursor() {
        let popped = OVERRIDE_CURSORS.with(|s| s.borrow_mut().pop().is_some());
        if popped {
            Self::apply_override_cursor(Self::override_cursor());
        }
    }

    fn apply_override_cursor(shape: Option<ferrokit_platform::cursor::CursorShape>) {
        let mut cursor = ferrokit_platform::platform().cursor();
        match shape {
            Some(shape) => {
                cursor.set_override_cursor(shape);
            }
            None => cursor.clear_override_cursor(),
        }
        // Each window shows the override, or its own cursor once the override is gone.
        crate::window::refresh_all_cursors();
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
        FOCUS_ENTRIES.with(|e| e.borrow_mut().clear());
        FOCUS_QUEUE.with(|q| q.borrow_mut().clear());
        FOCUS_CHANGED.with(|slot| *slot.borrow_mut() = None);
        let mut hints = ferrokit_gui::application::GuiApplication::style_hints();
        hints.mouse_double_click_interval_ms = 400;
        ferrokit_gui::application::GuiApplication::set_style_hints(hints);
        *GLOBAL_CURSOR_FLASH_TIME_MS.write().unwrap() = 1000;
        *GLOBAL_WHEEL_SCROLL_LINES.write().unwrap() = 3;
        *GLOBAL_START_DRAG_DISTANCE.write().unwrap() = 10;
        *GLOBAL_START_DRAG_TIME_MS.write().unwrap() = 500;
        *GLOBAL_STYLESHEET.write().unwrap() = None;
        OVERRIDE_CURSORS.with(|s| s.borrow_mut().clear());
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
