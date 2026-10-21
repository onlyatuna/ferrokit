// SPDX-License-Identifier: LGPL-3.0-only
use crate::widget::{hide_children, show_children_first, EmptyWidget, WidgetRef};
use ferrokit_core::event::{ContextMenuReason, Event, EventKind};
use ferrokit_core::event_loop::filters_consume;
use ferrokit_core::object::{ObjectData, ObjectId, QObject};
use ferrokit_gui::geometry::primitives::{Point, Rect, RectF, Size};
use ferrokit_gui::paint::{BackingStore, Painter};
use ferrokit_platform::size_limits::{clamp_size_limit, SizeLimits, WINDOW_SIZE_MAX};
use ferrokit_platform::{
    platform, PlatformWindow, WindowFlags, WindowSystemEvent, WindowSystemEventHandler,
};
use crate::hit_test::EventTreeDispatcher;
type ContextMenuCallback = std::rc::Rc<std::cell::RefCell<Option<Box<dyn Fn(Point)>>>>;
type MousePressCallback = std::rc::Rc<std::cell::RefCell<Option<Box<dyn Fn(Point, ferrokit_platform::MouseButton) -> bool>>>>;
type MouseMoveCallback = std::rc::Rc<std::cell::RefCell<Option<Box<dyn Fn(Point)>>>>;
type ResizeCallback = std::rc::Rc<std::cell::RefCell<Option<Box<dyn Fn(Size)>>>>;
type WindowEventCallback = std::rc::Rc<std::cell::RefCell<Option<Box<dyn Fn(&mut Event)>>>>;

/// The state `Window::show/hide/close` and the native close request share, so that both paths run
/// the same Qt sequence: `QWidgetPrivate::handleClose`, `show_helper`, `hide_helper`
/// (qwidget.cpp:8074-8130, 8238-8260, 8604-8640).
///
/// The window widget of a Qt top-level window is the root widget here; the optional window event
/// handler stands in for the `QWidget` subclass that overrides `closeEvent`, `moveEvent`, ...
#[derive(Clone)]
struct WindowCore {
    platform_window: std::rc::Rc<std::cell::RefCell<Box<dyn PlatformWindow>>>,
    render: std::rc::Rc<RenderState>,
    event_cb: WindowEventCallback,
    visible: std::rc::Rc<std::cell::Cell<bool>>,
    id: ObjectId,
}

impl WindowCore {
    /// `QApplicationPrivate::setActiveWindow` (qapplication.cpp:1816-1880) for this window: it
    /// becomes (or stops being) `QApplication::activeWindow()`, and `WindowActivate` /
    /// `WindowDeactivate` is sent to it and, like `QWidget::event` does (qwidget.cpp:9317-9327),
    /// on to every visible child that is not a window.
    fn set_active(&self, active: bool) {
        use crate::application::Application;
        let root = self.root();
        let Some(id) = root.borrow().window_id() else {
            return;
        };
        if active {
            if Application::active_window() == Some(id) {
                return;
            }
            Application::set_active_window(Some(id));
        } else {
            if Application::active_window() != Some(id) {
                return;
            }
            Application::set_active_window(None);
        }
        fn send_to_children(widget: &WidgetRef, active: bool) {
            let children = widget.borrow().children();
            for child in children {
                if child.borrow().is_visible() {
                    let kind = if active {
                        EventKind::WindowActivate
                    } else {
                        EventKind::WindowDeactivate
                    };
                    child.borrow_mut().event(&mut Event::new_spontaneous(kind));
                    send_to_children(&child, active);
                }
            }
        }
        let kind = if active {
            EventKind::WindowActivate
        } else {
            EventKind::WindowDeactivate
        };
        self.send_to_window(&mut Event::new_spontaneous(kind));
        send_to_children(&root, active);
    }

    fn root(&self) -> WidgetRef {
        self.render.root.borrow().clone()
    }

    /// Delivers `ev` to the window widget, then to the window event handler. Stops after the root
    /// widget if it ignored the event, so a refusing widget is not overruled.
    fn send_to_window(&self, ev: &mut Event) {
        let root = self.root();
        root.borrow_mut().event(ev);
        if !ev.is_accepted() {
            return;
        }
        if let Some(cb) = self.event_cb.borrow().as_ref() {
            cb(ev);
        }
    }

    /// `QShowEvent`s: every child that is not explicitly hidden first, then the window itself.
    /// The window is shown before any of them is sent, as `QWidget::isVisible` is true while
    /// `show_helper` runs.
    fn show(&self) {
        if !self.visible.replace(true) {
            crate::application::Application::set_window_visible(self.id, true);
            let root = self.root();
            show_children_first(&root);
            self.send_to_window(&mut Event::new(EventKind::Show));
        }
        self.platform_window.borrow_mut().show();
    }

    /// `QHideEvent`s: the window itself first, then every child that is not explicitly hidden.
    fn hide(&self) {
        self.platform_window.borrow_mut().hide();
        if self.visible.replace(false) {
            crate::application::Application::set_window_visible(self.id, false);
            self.send_to_window(&mut Event::new(EventKind::Hide));
            hide_children(&self.root());
        }
    }

    /// `QWidget::close` / the system close request: the window may refuse by ignoring the
    /// `QCloseEvent`; otherwise it is hidden. The event is delivered whether or not the window is
    /// visible. Only a window that was visible can end the application when it closes
    /// (`qwindow.cpp:2722-2735`).
    fn close(&self, spontaneous: bool) -> bool {
        let was_visible = self.visible.get();
        let mut ev = if spontaneous {
            Event::new_spontaneous(EventKind::Close)
        } else {
            Event::new(EventKind::Close)
        };
        self.send_to_window(&mut ev);
        if !ev.is_accepted() {
            return false;
        }
        self.hide();
        if was_visible {
            crate::application::Application::maybe_last_window_closed();
        }
        true
    }
}

/// Resize/render counters for one window (diagnostics and tests).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RenderStats {
    /// Native `Resize` events that changed the window size (never coalesced).
    pub resize_event_count: u64,
    /// Deferred renders actually queued on the event loop.
    pub deferred_render_schedule_count: u64,
    /// `LayoutScheduler::activate_pending()` passes run by the deferred render phase.
    pub layout_activation_count: u64,
    /// Deferred render phases executed.
    pub render_count: u64,
    /// Deferred render phases that painted and presented a non-empty region.
    pub present_count: u64,
    /// Deferred renders skipped because a platform-window / backing-store / root borrow was held.
    pub borrow_skipped_count: u64,
    /// Next-turn retries queued after a borrow conflict.
    pub borrow_retry_count: u64,
    /// Synchronous renders performed while a native interactive sizing loop was active.
    pub interactive_render_count: u64,
    /// Resize callbacks invoked (any path).
    pub resize_callback_count: u64,
    /// `EventKind::Resize` delivered straight to `Window::event` (posted/programmatic, not the
    /// canonical `WindowSystemEvent::Resize` path).
    pub event_resize_count: u64,
    /// `Window::render_and_present` calls (synchronous, outside the deferred/interactive gate).
    pub direct_render_count: u64,
    /// Direct renders that painted and presented a non-empty region.
    pub direct_present_count: u64,
    /// Logical region painted and presented by the last render of either kind (`None` before any).
    pub last_painted_region: Option<Rect>,
}

thread_local! {
    static RENDER_STATES: std::cell::RefCell<std::collections::HashMap<ObjectId, std::rc::Rc<RenderState>>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// Per-window coalescing state for native-resize driven repaints.
///
/// `MetaCall` closures must be `Send`, so a queued render captures only the window's
/// `ObjectId` and resolves this state through a thread-local registry when it runs.
///
/// State machine (all flags per window, single thread):
///
/// * IDLE: `dirty=false pending=false rendering=false`
/// * REQUESTED: `dirty=true pending=true` (exactly one `MetaCall` queued)
/// * RENDERING: `dirty=false pending=false rendering=true`; invalidations only set `dirty`
/// * RENDER AGAIN: `dirty` set while rendering -> re-queued once after the phase
/// * BORROW CONFLICT: `dirty=true pending=false`; one bounded next-turn retry is queued
///   (`retry_used`). If the retry also conflicts the window is *parked* (`dirty` kept, nothing
///   queued, no spin) and is re-armed by the next invalidation (`request_render`) or when a
///   synchronous `Window::render_and_present` releases its borrows (`rearm_if_parked`).
///
/// Widget `update()` (via `schedule_widget_update`), the `UpdateRequest` fallback, and native
/// `Resize` all enter through `request_render` / `queue_render`.
struct RenderState {
    window_id: ObjectId,
    platform_window: std::rc::Weak<std::cell::RefCell<Box<dyn PlatformWindow>>>,
    backing_store: std::rc::Weak<std::cell::RefCell<BackingStore>>,
    geometry: std::rc::Rc<std::cell::Cell<Rect>>,
    root: std::cell::RefCell<WidgetRef>,
    dirty: std::cell::Cell<bool>,
    render_pending: std::cell::Cell<bool>,
    currently_rendering: std::cell::Cell<bool>,
    /// Set only while `Window::set_geometry` is inside the native `set_geometry` call; it
    /// renders synchronously itself, so native Resize must not queue another render.
    within_set_geometry: std::cell::Cell<bool>,
    /// Set while a geometry change runs that ends in a render of this window (`set_geometry`'s
    /// synchronous render, or the native Resize render). Widget `update()` raised meanwhile only
    /// leaves its dirty rect for that render to collect instead of queueing another one.
    render_follows: std::cell::Cell<bool>,
    /// Native interactive sizing loop is active (`InteractiveResizeStart`..`End`): native Resize
    /// renders synchronously instead of deferring. Never read by event/callback semantics.
    interactive_resize: std::cell::Cell<bool>,
    /// A next-turn retry after a borrow conflict was already used for this request.
    retry_used: std::cell::Cell<bool>,
    /// `QWindow::devicePixelRatio`: cached per window, set from the window's own screen when the
    /// window is created and updated only by a DPI change (`QWindowPrivate::updateDevicePixelRatio`,
    /// `qwindow.cpp:1436`). It is never re-read from the primary screen.
    device_pixel_ratio: std::cell::Cell<f32>,
    /// Top-left of the screen the window is on: the identity of `QWindow::screen()`. Qt keeps
    /// the `QScreen*`; ferrokit platform screens are rebuilt per query, so the origin, which is unique
    /// per desktop layout, stands in for it.
    screen_origin: std::cell::Cell<Point>,
    /// `QWindow::screenChanged`: emitted after the window's ratio is updated for its new screen.
    screen_changed: ferrokit_core::signal::Signal<Box<dyn ferrokit_platform::screen::PlatformScreen>>,
    stats: std::cell::Cell<RenderStats>,
}

/// `QApplication::event(Timer)` gate for showing a tool tip (qapplication.cpp:1715-1726): the tip
/// shows when the widget's window has `WA_AlwaysShowToolTips`, or is the active window. A widget
/// tree that belongs to no `Window` is not an active window.
pub(crate) fn window_shows_tool_tips(top_level: &WidgetRef) -> bool {
    if top_level.borrow().always_show_tool_tips() {
        return true;
    }
    let Some(window_id) = top_level.borrow().window_id() else {
        return false;
    };
    let Some(state) = RENDER_STATES.with(|m| m.borrow().get(&window_id).cloned()) else {
        return false;
    };
    let Some(platform_window) = state.platform_window.upgrade() else {
        return false;
    };
    let active = platform_window
        .try_borrow()
        .map(|w| w.is_active())
        .unwrap_or(false);
    active
}

/// Clears a flag on every exit path, including unwinding.
struct FlagGuard<'a>(&'a std::cell::Cell<bool>);

impl Drop for FlagGuard<'_> {
    fn drop(&mut self) {
        self.0.set(false);
    }
}

/// Sets a flag for a scope and restores its previous value on every exit path, so nested scopes
/// (a native Resize handled inside `Window::set_geometry`) keep the outer value.
struct ScopedFlag<'a>(&'a std::cell::Cell<bool>, bool);

impl<'a> ScopedFlag<'a> {
    fn set(flag: &'a std::cell::Cell<bool>) -> Self {
        Self(flag, flag.replace(true))
    }
}

impl Drop for ScopedFlag<'_> {
    fn drop(&mut self) {
        self.0.set(self.1);
    }
}

impl RenderState {
    fn register(
        window_id: ObjectId,
        platform_window: std::rc::Weak<std::cell::RefCell<Box<dyn PlatformWindow>>>,
        backing_store: std::rc::Weak<std::cell::RefCell<BackingStore>>,
        geometry: std::rc::Rc<std::cell::Cell<Rect>>,
        root: WidgetRef,
        device_pixel_ratio: f32,
        screen_origin: Point,
    ) -> std::rc::Rc<Self> {
        let state = std::rc::Rc::new(Self {
            window_id,
            platform_window,
            backing_store,
            geometry,
            root: std::cell::RefCell::new(root),
            dirty: std::cell::Cell::new(false),
            render_pending: std::cell::Cell::new(false),
            currently_rendering: std::cell::Cell::new(false),
            interactive_resize: std::cell::Cell::new(false),
            within_set_geometry: std::cell::Cell::new(false),
            render_follows: std::cell::Cell::new(false),
            retry_used: std::cell::Cell::new(false),
            device_pixel_ratio: std::cell::Cell::new(device_pixel_ratio),
            screen_origin: std::cell::Cell::new(screen_origin),
            screen_changed: ferrokit_core::signal::Signal::new(),
            stats: std::cell::Cell::new(RenderStats::default()),
        });
        RENDER_STATES.with(|m| m.borrow_mut().insert(window_id, std::rc::Rc::clone(&state)));
        state
    }

    fn unregister(window_id: ObjectId) {
        let _ = RENDER_STATES.try_with(|m| m.borrow_mut().remove(&window_id));
    }

    /// `QWindow::screen()`: the platform screen whose origin is `screen_origin`, or the primary
    /// screen if that screen is gone.
    fn current_screen(&self) -> Box<dyn ferrokit_platform::screen::PlatformScreen> {
        let origin = self.screen_origin.get();
        platform()
            .screens()
            .into_iter()
            .find(|s| s.geometry().top_left() == origin)
            .unwrap_or_else(|| platform().primary_screen())
    }

    /// After a geometry change to `rect`, moves the window to `screenForGeometry(rect)`
    /// (`qwindow.cpp:1894-1910`). On a different screen the window takes that screen's ratio
    /// (`QWindowPrivate::updateDevicePixelRatio`, `qwindow.cpp:1436`) and `screenChanged` is
    /// emitted. On the same screen nothing changes, so a DPI change already received is kept.
    fn track_screen(&self, rect: Rect) {
        let next = screen_for_geometry(rect, self.current_screen());
        let origin = screen_mapping(&*next).0;
        if origin == self.screen_origin.get() {
            return;
        }
        self.screen_origin.set(origin);
        self.apply_device_pixel_ratio(next.device_pixel_ratio());
        self.screen_changed.emit(&next);
    }

    /// Applies a new ratio to the backing store, the widget tree and their layouts. Only a
    /// changed ratio does work, so a repeated screen change to the same ratio is a no-op.
    fn apply_device_pixel_ratio(&self, new_dpr: f32) {
        let old_dpr = self.device_pixel_ratio.get();
        if old_dpr == new_dpr {
            return;
        }
        self.device_pixel_ratio.set(new_dpr);
        if let Some(bs) = self.backing_store.upgrade() {
            let geom = self.geometry.get();
            bs.borrow_mut()
                .resize(Size::new(geom.width, geom.height), new_dpr);
        }
        let root = self.root.borrow().clone();
        crate::widget::adopt_tree(&root);
        propagate_dpi_change_recursive(&root, old_dpr, new_dpr);
    }

    fn bump(&self, f: impl FnOnce(&mut RenderStats)) {
        let mut s = self.stats.get();
        f(&mut s);
        self.stats.set(s);
    }

    /// Marks the window dirty and queues at most one deferred render.
    fn request_render(&self) {
        ferrokit_platform::resize_debug::count(ferrokit_platform::resize_debug::Count::RenderRequested);
        self.dirty.set(true);
        self.retry_used.set(false);
        if self.render_pending.get() || self.currently_rendering.get() {
            // Pending: the queued render reads the latest geometry. Rendering: re-queued on completion.
            return;
        }
        if !self.queue_render() {
            // No event loop on this thread to defer to: render in place.
            self.render_phase();
        }
    }

    /// Posts the one deferred render. Events posted from inside a pump wait for the next pump
    /// (`insertion_offset`), so this never re-dispatches within the current pump.
    fn queue_render(&self) -> bool {
        self.render_pending.set(true);
        ferrokit_platform::resize_debug::count(ferrokit_platform::resize_debug::Count::RenderQueuedDeferred);
        let id = self.window_id;
        let thread = ferrokit_core::object::ThreadId::current();
        // post_event_to_thread holds events for a thread without a loop instead of dropping
        // them, so "can this render be deferred" must be asked explicitly.
        let has_loop = ferrokit_core::event_loop::get_thread_event_sender(thread).is_some();
        if has_loop {
            ferrokit_core::event_loop::post_event_to_thread(
                thread,
                ObjectId(0),
                Event::new(EventKind::MetaCall(Box::new(move |_| run_deferred_render(id)))),
            );
            self.bump(|s| s.deferred_render_schedule_count += 1);
        } else {
            self.render_pending.set(false);
        }
        has_loop
    }

    /// Re-queues a render left `dirty` by a borrow conflict that exhausted its retry.
    fn rearm_if_parked(&self) {
        if self.dirty.get() && !self.render_pending.get() && !self.currently_rendering.get() {
            self.request_render();
        }
    }

    /// True if a repaint is owed: the window flag, or any widget with a dirty rect.
    fn has_pending_invalidation(&self) -> bool {
        if self.dirty.get() {
            return true;
        }
        match self.root.try_borrow() {
            Ok(root) => tree_has_dirty(&root),
            Err(_) => true,
        }
    }

    /// A borrow was held elsewhere: keep `dirty`, queue at most one next-turn retry.
    fn on_borrow_conflict(&self) {
        ferrokit_platform::resize_debug::count(ferrokit_platform::resize_debug::Count::SkipBorrowConflict);
        ferrokit_platform::resize_debug::note(|| "render skipped: borrow conflict".to_string());
        self.bump(|s| s.borrow_skipped_count += 1);
        if self.retry_used.get() || self.render_pending.get() {
            return;
        }
        self.retry_used.set(true);
        if self.queue_render() {
            self.bump(|s| s.borrow_retry_count += 1);
        }
    }
    /// Interactive-resize policy: render the latest geometry now, through the same guards
    /// (`currently_rendering`, `dirty`, borrow-conflict retry). A `MetaCall` queued before the
    /// loop started stays queued but finds `dirty == false` and does nothing, so there is never
    /// a sync + deferred double render.
    fn render_now(&self) {
        ferrokit_platform::resize_debug::count(ferrokit_platform::resize_debug::Count::RenderNow);
        self.dirty.set(true);
        if self.currently_rendering.get() {
            // Re-queued once by the running phase's tail.
            return;
        }
        let before = self.stats.get().render_count;
        self.render_phase();
        if self.stats.get().render_count != before {
            self.bump(|s| s.interactive_render_count += 1);
        }
    }

    /// Leaves interactive mode; anything still owed (borrow conflict) is re-armed.
    fn end_interactive_resize(&self) {
        self.interactive_resize.set(false);
        self.rearm_if_parked();
    }


    /// Layout activation + paint + present, using the latest geometry.
    fn render_phase(&self) {
        if self.currently_rendering.get() {
            ferrokit_platform::resize_debug::count(ferrokit_platform::resize_debug::Count::SkipReentrant);
            ferrokit_platform::resize_debug::note(|| "render_phase skipped: already rendering".to_string());
            return;
        }
        if !self.dirty.get() {
            ferrokit_platform::resize_debug::count(ferrokit_platform::resize_debug::Count::SkipNotDirty);
            ferrokit_platform::resize_debug::note(|| "render_phase skipped: not dirty".to_string());
            return;
        }
        let (Some(pw_rc), Some(bs_rc)) = (self.platform_window.upgrade(), self.backing_store.upgrade())
        else {
            return;
        };
        let Ok(mut pw) = pw_rc.try_borrow_mut() else {
            self.on_borrow_conflict();
            return;
        };
        let Ok(mut bs) = bs_rc.try_borrow_mut() else {
            self.on_borrow_conflict();
            return;
        };
        let Ok(root) = self.root.try_borrow().map(|r| r.clone()) else {
            self.on_borrow_conflict();
            return;
        };

        self.dirty.set(false);
        self.retry_used.set(false);
        {
            self.currently_rendering.set(true);
            let _guard = FlagGuard(&self.currently_rendering);
            let dbg_lay = ferrokit_platform::resize_debug::start();
            crate::layout_scheduler::LayoutScheduler::activate_pending();
            ferrokit_platform::resize_debug::end(ferrokit_platform::resize_debug::Phase::LayoutActivate, dbg_lay);
            let g = self.geometry.get();
            let trace_hwnd = pw.native_handle() as usize;
            let dpr = self.device_pixel_ratio.get();
            let trace_dpr = dpr;
            let trace_phys = (
                (g.width as f32 * trace_dpr).round() as u32,
                (g.height as f32 * trace_dpr).round() as u32,
            );
            ferrokit_platform::resize_trace::record_at(
                ferrokit_platform::resize_trace::TraceKind::RequestedGeometry,
                trace_hwnd,
                (g.x, g.y),
                (g.width as u32, g.height as u32),
                trace_phys,
            );
            ferrokit_platform::resize_trace::record_window_rect(
                ferrokit_platform::resize_trace::TraceKind::WindowRectBeforeRender,
                trace_hwnd,
            );
            ferrokit_platform::resize_trace::record(
                ferrokit_platform::resize_trace::TraceKind::RenderStart,
                trace_hwnd,
                (g.width as u32, g.height as u32),
                trace_phys,
            );
            let presented = do_render_and_present(&mut **pw, &mut bs, &root, g, dpr);
            ferrokit_platform::resize_debug::mark_rendered();
            ferrokit_platform::resize_trace::record(
                ferrokit_platform::resize_trace::TraceKind::RenderEnd,
                trace_hwnd,
                (g.width as u32, g.height as u32),
                trace_phys,
            );
            self.bump(|s| {
                s.layout_activation_count += 1;
                s.render_count += 1;
                if presented.is_some() {
                    s.present_count += 1;
                    s.last_painted_region = presented;
                }
            });
        }
        drop(bs);
        drop(pw);

        // An invalidation arrived while rendering (a nested native resize, or a widget dirtied
        // after this pass collected the dirty region).
        if self.has_pending_invalidation() && !self.render_pending.get() {
            self.request_render();
        }
    }
}

fn tree_has_dirty(widget: &WidgetRef) -> bool {
    let Ok(w) = widget.try_borrow() else {
        return true;
    };
    if w.dirty_rect().is_some() {
        return true;
    }
    let children = w.children();
    drop(w);
    children.iter().any(tree_has_dirty)
}

/// `QWidget::update` for a widget inside a `Window`: marks the window dirty and queues its one
/// deferred render (`UpdateLater`, qwidgetrepaintmanager.cpp:356-363), so it needs no
/// `UpdateRequest` event and no QObject registration. Never paints. With no event loop on this
/// thread the window stays dirty until the next render. Returns false when this thread has no
/// render state for `window_id`.
pub(crate) fn schedule_widget_update(window_id: ObjectId) -> bool {
    let state = RENDER_STATES
        .try_with(|m| m.borrow().get(&window_id).cloned())
        .ok()
        .flatten();
    let Some(state) = state else {
        return false;
    };
    if state.currently_rendering.get() || state.render_follows.get() {
        // The running (or imminent) pass collects widget dirt after layout activation and the
        // backing-store resize, so an `update()` raised before that is painted by it. Dirt left
        // after its collection (an `update()` from a paint) is picked up by the pass's tail check.
        return true;
    }
    state.dirty.set(true);
    state.retry_used.set(false);
    if !state.render_pending.get() {
        state.queue_render();
    }
    true
}

/// Body of the queued `MetaCall`; the window may be gone by now.
fn run_deferred_render(window_id: ObjectId) {
    let state = RENDER_STATES
        .try_with(|m| m.borrow().get(&window_id).cloned())
        .ok()
        .flatten();
    let Some(state) = state else {
        return;
    };
    state.render_pending.set(false);
    state.render_phase();
}

/// `QRect::center`: the middle of the first and last pixel, `x + (w - 1) / 2` rounded toward
/// zero (`qrect.h:253-257`). ferrokit' `Rect::center` is `x + w / 2`.
fn qt_rect_centre(r: Rect) -> Point {
    let mid = |a: i32, len: i32| ((2 * i64::from(a) + i64::from(len) - 1) / 2) as i32;
    Point::new(mid(r.x, r.width), mid(r.y, r.height))
}

/// `QWindowPrivate::screenForGeometry` (`qwindow.cpp:1894-1910`): `current` if it holds the
/// rect's centre, else the first screen that does, else the last screen the rect intersects,
/// else `current`.
fn screen_for_geometry(
    rect: Rect,
    current: Box<dyn ferrokit_platform::screen::PlatformScreen>,
) -> Box<dyn ferrokit_platform::screen::PlatformScreen> {
    let centre = qt_rect_centre(rect);
    if current.geometry().contains(centre) {
        return current;
    }
    let mut fallback = current;
    for screen in platform().screens() {
        if screen.geometry().contains(centre) {
            return screen;
        }
        if screen.geometry().intersects(&rect) {
            fallback = screen;
        }
    }
    fallback
}

/// What a desktop rect on `screen` maps to native pixels with: the screen's top-left, which is
/// the same point in logical and native coordinates (`qhighdpiscaling.cpp:713-721`), and its
/// ratio.
fn screen_mapping(screen: &dyn ferrokit_platform::screen::PlatformScreen) -> (Point, f32) {
    (screen.geometry().top_left(), screen.device_pixel_ratio())
}

pub struct Window {
    object_data: ObjectData,
    platform_window: std::rc::Rc<std::cell::RefCell<Box<dyn PlatformWindow>>>,
    root_widget: WidgetRef,
    backing_store: std::rc::Rc<std::cell::RefCell<BackingStore>>,
    geometry: std::rc::Rc<std::cell::Cell<Rect>>,
    context_menu_cb: ContextMenuCallback,
    mouse_press_cb: MousePressCallback,
    mouse_move_cb: MouseMoveCallback,
    resize_cb: ResizeCallback,
    render_state: std::rc::Rc<RenderState>,
    core: WindowCore,
    /// Per-axis explicit minimum (`QWidget::setMinimumSize`, `qwidget.cpp:3971`). An axis with an
    /// explicit minimum keeps it; the other axis follows the root layout.
    explicit_min: (bool, bool),
}

impl Window {
    pub fn new(title: &str, geometry: Rect, flags: WindowFlags) -> Result<Self, &'static str> {
        let _t = ferrokit_gui::startup_trace::span(|| format!("Window::new({title:?})"));
        let p = {
            let _t = ferrokit_gui::startup_trace::span(|| "platform() first use".into());
            platform()
        };
        // QWindowPrivate::create picks the screen for the requested geometry before the native
        // window exists, starting from the primary screen (`qwindow.cpp:566-568`), and the
        // Windows plugin creates it at `toNativePixels(geometry, window)` on that screen
        // (`qwindowsintegration.cpp:299-300`).
        let (origin, placement_dpr) =
            screen_mapping(&*screen_for_geometry(geometry, p.primary_screen()));
        let native_rect =
            ferrokit_platform::high_dpi::to_native_global_rect(geometry, origin, placement_dpr);
        let mut platform_win = {
            let _t = ferrokit_gui::startup_trace::span(|| "create_window (native)".into());
            p.create_window(title, native_rect, flags)?
        };
        // QWindowPrivate::updateDevicePixelRatio: a window on another screen has that screen's
        // ratio. Keep the native origin and give the window the native size of its own ratio.
        let dpr = platform_win.device_pixel_ratio();
        if dpr != placement_dpr {
            let native_size = ferrokit_platform::high_dpi::to_native_size(
                Size::new(geometry.width, geometry.height),
                dpr,
            );
            platform_win.set_geometry(Rect::new(
                native_rect.x,
                native_rect.y,
                native_size.width,
                native_size.height,
            ));
        }
        let backing_store = BackingStore::new(Size::new(geometry.width, geometry.height), dpr)
            .ok_or("Failed to create top-level window offscreen BackingStore")?;
        let window_id = ObjectId::next();

        let root_widget: WidgetRef = std::rc::Rc::new(std::cell::RefCell::new(Box::new(
            EmptyWidget::with_geometry(Rect::new(0, 0, geometry.width, geometry.height)),
        )));
        root_widget.borrow_mut().set_window_id(Some(window_id));

        let context_menu_cb = std::rc::Rc::new(std::cell::RefCell::new(None));
        let cb_clone = std::rc::Rc::clone(&context_menu_cb);
        let mouse_press_cb = std::rc::Rc::new(std::cell::RefCell::new(None));
        let press_cb_clone = std::rc::Rc::clone(&mouse_press_cb);
        let mouse_move_cb = std::rc::Rc::new(std::cell::RefCell::new(None));
        let move_cb_clone = std::rc::Rc::clone(&mouse_move_cb);
        let resize_cb = std::rc::Rc::new(std::cell::RefCell::new(None));
        let resize_cb_clone = std::rc::Rc::clone(&resize_cb);

        let bs_rc = std::rc::Rc::new(std::cell::RefCell::new(backing_store));
        let bs_clone = std::rc::Rc::clone(&bs_rc);
        let geom_cell = std::rc::Rc::new(std::cell::Cell::new(geometry));
        let geom_clone = std::rc::Rc::clone(&geom_cell);

        let native_double_clicks = platform_win.reports_native_double_clicks();
        let pw_rc = std::rc::Rc::new(std::cell::RefCell::new(platform_win));
        let pw_clone = std::rc::Rc::clone(&pw_rc);

        let render_state = RenderState::register(
            window_id,
            std::rc::Rc::downgrade(&pw_rc),
            std::rc::Rc::downgrade(&bs_rc),
            std::rc::Rc::clone(&geom_cell),
            root_widget.clone(),
            dpr,
            origin,
        );
        let core = WindowCore {
            platform_window: std::rc::Rc::clone(&pw_rc),
            render: std::rc::Rc::clone(&render_state),
            event_cb: std::rc::Rc::new(std::cell::RefCell::new(None)),
            visible: std::rc::Rc::new(std::cell::Cell::new(false)),
            id: window_id,
        };
        let handler = WindowEventHandler {
            platform_window: pw_clone,
            backing_store: bs_clone,
            geometry: geom_clone,
            core: core.clone(),
            dispatcher: EventTreeDispatcher::new(),
            context_menu_cb: cb_clone,
            mouse_press_cb: press_cb_clone,
            mouse_move_cb: move_cb_clone,
            resize_cb: resize_cb_clone,
            render: std::rc::Rc::clone(&render_state),
            native_double_clicks,
            minimized: false,
        };
        pw_rc.borrow_mut().set_event_handler(Box::new(handler));

        let win = Self {
            object_data: ObjectData::new(window_id),
            platform_window: pw_rc,
            root_widget,
            backing_store: bs_rc,
            geometry: geom_cell,
            context_menu_cb,
            mouse_press_cb,
            mouse_move_cb,
            resize_cb,
            render_state,
            core,
            explicit_min: (false, false),
        };
        let close_core = win.core.clone();
        crate::application::Application::register_window(
            window_id,
            std::rc::Rc::new(move || close_core.close(false)),
        );
        Ok(win)
    }

    pub fn id(&self) -> ObjectId {
        self.object_data.id()
    }

    pub fn root_widget(&self) -> WidgetRef {
        std::rc::Rc::clone(&self.root_widget)
    }
    pub fn set_root_widget(&mut self, widget: WidgetRef) {
        widget.borrow_mut().set_window_id(Some(self.object_data.id()));
        let geom = self.geometry.get();
        widget
            .borrow_mut()
            .set_geometry(Rect::new(0, 0, geom.width, geom.height));
        *self.render_state.root.borrow_mut() = widget.clone();
        self.root_widget = widget;
    }

    pub fn geometry(&self) -> Rect {
        self.geometry.get()
    }

    /// `QWindow::devicePixelRatio`: the ratio of this window's physical to logical pixels, from
    /// the screen the window is on. It changes only when the window receives a DPI change.
    pub fn device_pixel_ratio(&self) -> f32 {
        self.render_state.device_pixel_ratio.get()
    }

    pub fn physical_geometry(&self) -> Rect {
        self.platform_window.borrow().geometry()
    }

    /// `QWindow::screen()`: the screen the window is on, as last tracked from its geometry.
    pub fn screen(&self) -> Box<dyn ferrokit_platform::screen::PlatformScreen> {
        self.render_state.current_screen()
    }

    /// `QWindow::screenChanged`: emitted with the new screen when the window moves to another one.
    pub fn screen_changed(&self) -> &ferrokit_core::signal::Signal<Box<dyn ferrokit_platform::screen::PlatformScreen>> {
        &self.render_state.screen_changed
    }

    /// The screen top-left and ratio a move of this window to `rect` maps with:
    /// `QWindow::setGeometry` uses `screenForGeometry(rect)` from the window's current screen
    /// (`qwindow.cpp:1867-1870`).
    pub fn native_mapping_for(&self, rect: Rect) -> (Point, f32) {
        let current = self.render_state.current_screen();
        screen_mapping(&*screen_for_geometry(rect, current))
    }

    fn native_rect_for(&self, rect: Rect) -> Rect {
        let (origin, dpr) = self.native_mapping_for(rect);
        ferrokit_platform::high_dpi::to_native_global_rect(rect, origin, dpr)
    }

    /// `rect` with its size brought inside the window's minimum and maximum, as
    /// `QWidgetPrivate::setGeometry_sys` does for every programmatic geometry (qwidget.cpp:7299-7305;
    /// minimum wins). The system's tracking limits do not do this: Windows applies them to a
    /// `WS_POPUP` window (a layered popup) only while the user drags an edge. The position is unchanged.
    fn bounded_rect(&self, rect: Rect) -> Rect {
        let limits = {
            let pw = self.platform_window.borrow();
            SizeLimits {
                min: pw.minimum_size(),
                max: pw.maximum_size(),
            }
        };
        let (w, h) = limits.bound_size(rect.width, rect.height);
        Rect::new(rect.x, rect.y, w, h)
    }

    pub fn set_geometry(&mut self, rect: Rect) {
        let rect = self.bounded_rect(rect);
        let native_rect = self.native_rect_for(rect);
        let old_geom = self.geometry.get();
        let old_size = Size::new(old_geom.width, old_geom.height);
        self.geometry.set(rect);
        let size_changed = old_size.width != rect.width || old_size.height != rect.height;

        // Steps 0-3 end in the synchronous render of step 4, which collects their widget dirt.
        let follows = ScopedFlag::set(&self.render_state.render_follows);
        {
            self.render_state.within_set_geometry.set(true);
            let _guard = FlagGuard(&self.render_state.within_set_geometry);
            self.platform_window.borrow_mut().set_geometry(native_rect);
        }
        self.render_state.track_screen(rect);

        // 1. Root geometry updated
        self.root_widget
            .borrow_mut()
            .set_geometry(Rect::new(0, 0, rect.width, rect.height));

        // 2. Dispatch ResizeEvent & resize callback
        if size_changed {
            let mut ev = Event::new_spontaneous(EventKind::Resize {
                width: rect.width,
                height: rect.height,
                old_width: old_size.width,
                old_height: old_size.height,
            });
            self.root_widget.borrow_mut().event(&mut ev);
            if let Some(cb) = self.resize_cb.borrow().as_ref() {
                self.render_state.bump(|s| s.resize_callback_count += 1);
                cb(Size::new(rect.width, rect.height));
            }
        }

        // 3. Layout Invalidation & Activation via LayoutScheduler
        if size_changed {
            crate::layout_scheduler::LayoutScheduler::request_layout(&self.root_widget);
            crate::layout_scheduler::LayoutScheduler::activate_pending();
        }

        // 4. Backing store invalidation & paint (Lazy Resize in do_render_and_present)
        drop(follows);
        self.render_and_present();
    }

    pub fn set_geometry_silent(&mut self, rect: Rect) {
        let rect = self.bounded_rect(rect);
        let native_rect = self.native_rect_for(rect);
        self.geometry.set(rect);
        self.platform_window.borrow_mut().set_geometry(native_rect);
        self.render_state.track_screen(rect);
    }

    pub fn show(&mut self) {
        let _t = ferrokit_gui::startup_trace::span(|| "Window::show".into());
        {
            let _s = ferrokit_gui::startup_trace::span(|| "platform_window.show() (ShowWindow + messages it dispatches)".into());
            self.core.show();
        }
        self.render_and_present();
    }

    pub fn hide(&mut self) {
        self.core.hide();
    }

    /// `QWidget::close`: sends a `Close` event to the window widget and the window event handler;
    /// if neither ignores it the window is hidden. Returns whether the window was closed.
    pub fn close(&mut self) -> bool {
        self.core.close(false)
    }

    /// Whether `show` has been called without a later `hide`/accepted `close`.
    pub fn is_visible(&self) -> bool {
        self.core.visible.get()
    }

    /// The handler a `QWidget` subclass would express by overriding the window-level event
    /// handlers. It receives, with the window's coordinates and `accept`/`ignore`:
    /// - `Close` (ignore it to refuse the close), `Show`, `Hide` and `Move`, after the root widget;
    /// - `WindowActivate` / `WindowDeactivate`, after the root widget, when the window becomes or
    ///   stops being `Application::active_window()`;
    /// - `MouseButtonRelease` and `MouseButtonDblClick` that no widget handled. A double click the
    ///   handler ignores falls back to the press handler, as `QWidget::mouseDoubleClickEvent` does.
    pub fn set_window_event_handler<F: Fn(&mut Event) + 'static>(&mut self, handler: F) {
        *self.core.event_cb.borrow_mut() = Some(Box::new(handler));
    }
    pub fn set_stays_on_top(&mut self, enabled: bool) {
        self.platform_window.borrow_mut().set_stays_on_top(enabled);
    }

    pub fn set_click_through(&mut self, enabled: bool) {
        self.platform_window.borrow_mut().set_click_through(enabled);
    }

    /// Sets or clears a window flag (`QWidget::setWindowFlag`, qwidget.cpp:10641-10648).
    pub fn set_window_flag(&mut self, flag: WindowFlags, on: bool) {
        self.platform_window.borrow_mut().set_window_flag(flag, on);
    }

    /// Sets window flags (`QWidget::setWindowFlags`, qwidget.cpp:10623-10627).
    pub fn set_window_flags(&mut self, flags: WindowFlags) {
        let mut pw = self.platform_window.borrow_mut();
        let cur = pw.flags();
        // Update diff for dynamically toggleable flags
        let stays_on_top = flags.contains(WindowFlags::STAYS_ON_TOP);
        if cur.contains(WindowFlags::STAYS_ON_TOP) != stays_on_top {
            pw.set_stays_on_top(stays_on_top);
        }
        let click_through = flags.contains(WindowFlags::CLICK_THROUGH);
        if cur.contains(WindowFlags::CLICK_THROUGH) != click_through {
            pw.set_click_through(click_through);
        }
    }

    /// Current window flags (`QWidget::windowFlags`).
    pub fn window_flags(&self) -> WindowFlags {
        self.platform_window.borrow().flags()
    }

    pub fn set_opacity(&mut self, opacity: f32) {
        self.platform_window.borrow_mut().set_opacity(opacity);
        self.render_and_present();
    }
    /// `QWidget::setStyleSheet` on a top-level window: the sheet
    /// applies to this window's widget tree only (the root widget is the window widget here).
    /// The application's sheet is [`Application::set_style_sheet`].
    pub fn set_style_sheet(&mut self, qss: &str) {
        self.root_widget.borrow().set_style_sheet(qss);
        self.render_and_present();
    }

    pub fn opacity(&self) -> f32 {
        self.platform_window.borrow().opacity()
    }

    /// `QWidget::setMinimumSize` (`qwidget.cpp:3942-3973`): each axis is kept in `0..=WINDOW_SIZE_MAX`
    /// and `WINDOW_SIZE_MAX` is read as 0 (`:3947-3950`, so `set_fixed_size(WINDOW_SIZE_MAX,
    /// WINDOW_SIZE_MAX)` unlocks a window), an unchanged minimum is a no-op, and the axes that are
    /// set become explicit (`:3971`). Unlike `QWindow::setMinimumSize`, which resizes a window that
    /// is outside the new bounds at once (`qwindow.cpp:649-655`), it does not:
    /// the next render of a visible window with a root layout does (`sync_top_level_minimum`).
    pub fn set_minimum_size(&mut self, min_w: i32, min_h: i32) {
        let (min_w, min_h) = clamp_size_limit(min_w, min_h);
        let axis = |v: i32| if v == WINDOW_SIZE_MAX { 0 } else { v };
        let (min_w, min_h) = (axis(min_w), axis(min_h));
        if (min_w, min_h) == self.minimum_size() {
            return;
        }
        self.explicit_min = (min_w != 0, min_h != 0);
        self.platform_window
            .borrow_mut()
            .set_minimum_size(min_w, min_h);
    }

    pub fn minimum_size(&self) -> (i32, i32) {
        self.platform_window.borrow().minimum_size()
    }

    /// `QWidget::setMaximumSize` (`qwidget.cpp:4065-4087`) with the same contract as
    /// [`set_minimum_size`](Self::set_minimum_size): kept in `0..=WINDOW_SIZE_MAX`, an unchanged
    /// maximum is a no-op, `WINDOW_SIZE_MAX` on an axis is no maximum (and removes one), and the
    /// window is not resized now.
    pub fn set_maximum_size(&mut self, max_w: i32, max_h: i32) {
        let (max_w, max_h) = clamp_size_limit(max_w, max_h);
        if (max_w, max_h) == self.maximum_size() {
            return;
        }
        self.platform_window
            .borrow_mut()
            .set_maximum_size(max_w, max_h);
    }

    pub fn maximum_size(&self) -> (i32, i32) {
        self.platform_window.borrow().maximum_size()
    }

    /// `QWidget::setFixedSize` (`qwidget.cpp:4155-4170`): the minimum and the maximum become `w x
    /// h`, and unless that changed nothing the window is resized to it (not when both are
    /// `WINDOW_SIZE_MAX`, which removes the limits).
    pub fn set_fixed_size(&mut self, w: i32, h: i32) {
        let (w, h) = clamp_size_limit(w, h);
        let before = (self.minimum_size(), self.maximum_size());
        self.set_minimum_size(w, h);
        self.set_maximum_size(w, h);
        if before == (self.minimum_size(), self.maximum_size()) {
            return;
        }
        if w != WINDOW_SIZE_MAX || h != WINDOW_SIZE_MAX {
            let geom = self.geometry.get();
            self.set_geometry(Rect::new(geom.x, geom.y, w, h));
        }
    }

    /// Top-level bounds from the root layout's size constraint (`qlayout.cpp:1006-1095`, applied
    /// through `setMinimumSize` / `setMaximumSize` at `:1096-1122`). With the default constraint an
    /// axis without an explicit minimum takes the root layout's minimum size and an explicit axis
    /// keeps its value. The bounds go to the platform window, which hands them to the window
    /// system; a layout without a maximum leaves the window's own maximum in place. Returns the
    /// size the window must take when it is outside the bounds (`qwidget.cpp:4013-4016`,
    /// `:4073-4076`); the same rule as `set_geometry` (`SizeLimits::bound_size`), so a window this
    /// returns is not changed again by the `set_geometry` that applies it.
    fn sync_top_level_minimum(&mut self) -> Option<Size> {
        let root = std::rc::Rc::clone(&self.root_widget);
        let (cur_w, cur_h) = self.minimum_size();
        let (cur_max_w, cur_max_h) = self.maximum_size();
        let (min, max) = {
            let root_ref = root.borrow();
            let layout = root_ref.layout_ref_mut()?;
            crate::widget::size_constraint_bounds(
                &**layout,
                true,
                self.explicit_min,
                Size::new(cur_w, cur_h),
            )
        };
        let (target_w, target_h) = min.map_or((cur_w, cur_h), |m| (m.width, m.height));
        if (target_w, target_h) != (cur_w, cur_h) {
            // Written straight to the platform: the explicit flags belong to the caller
            // (`qlayout.cpp:1126-1129` restores them after the layout's own `setMinimumSize`).
            self.platform_window
                .borrow_mut()
                .set_minimum_size(target_w, target_h);
        }
        let (max_w, max_h) = max.map_or((cur_max_w, cur_max_h), |m| {
            clamp_size_limit(m.width, m.height)
        });
        if (max_w, max_h) != (cur_max_w, cur_max_h) {
            self.platform_window
                .borrow_mut()
                .set_maximum_size(max_w, max_h);
        }
        let geom = self.geometry.get();
        let (w, h) = SizeLimits {
            min: (target_w, target_h),
            max: (max_w, max_h),
        }
        .bound_size(geom.width, geom.height);
        if (w, h) == (geom.width, geom.height) {
            None
        } else {
            Some(Size::new(w, h))
        }
    }

    pub fn start_system_drag(&self) {
        self.platform_window.borrow().start_system_drag();
    }
    pub fn start_system_move(&self) -> bool {
        self.platform_window.borrow().start_system_move()
    }

    pub fn start_system_resize(&self, edges: ferrokit_platform::platform_window::WindowEdges) -> bool {
        self.platform_window.borrow().start_system_resize(edges)
    }

    pub fn set_cursor(&mut self, shape: ferrokit_platform::cursor::CursorShape) {
        self.platform_window.borrow_mut().set_cursor(shape);
    }

    pub fn set_mouse_press_handler<F: Fn(Point, ferrokit_platform::MouseButton) -> bool + 'static>(&mut self, handler: F) {
        let mut cb = self.mouse_press_cb.borrow_mut();
        *cb = Some(Box::new(handler));
    }

    pub fn set_mouse_move_handler<F: Fn(Point) + 'static>(&mut self, handler: F) {
        let mut cb = self.mouse_move_cb.borrow_mut();
        *cb = Some(Box::new(handler));
    }
    pub fn set_resize_handler<F: Fn(Size) + 'static>(&mut self, handler: F) {
        let mut cb = self.resize_cb.borrow_mut();
        *cb = Some(Box::new(handler));
    }
    pub fn set_backdrop(
        &mut self,
        backdrop: ferrokit_platform::backdrop::BackdropType,
        dark_mode: bool,
    ) -> bool {
        self.platform_window.borrow_mut().set_backdrop(backdrop, dark_mode)
    }

    pub fn set_ime_focus(&mut self, pos: Point) {
        self.platform_window.borrow_mut().set_ime_focus(pos);
    }

    pub fn enable_drop_target(&mut self, enabled: bool) -> bool {
        self.platform_window.borrow_mut().enable_drop_target(enabled)
    }

    pub fn native_handle(&self) -> isize {
        self.platform_window.borrow().native_handle()
    }

    pub fn set_context_menu_handler<F: Fn(Point) + 'static>(&mut self, handler: F) {
        let mut cb = self.context_menu_cb.borrow_mut();
        *cb = Some(Box::new(handler));
    }

    #[track_caller]
    pub fn render_and_present(&mut self) {
        let geom = self.geometry.get();
        {
            let c = std::panic::Location::caller();
            ferrokit_platform::resize_debug::note(|| format!(
                "render_and_present() called from {}:{} with geometry {}x{}@({},{})",
                c.file(), c.line(), geom.width, geom.height, geom.x, geom.y
            ));
        }
        let root = self.root_widget.clone();
        let state = &self.render_state;
        // This pass paints everything owed so far: a queued deferred render finds nothing to do.
        // While it runs, `update()` raised by layout activation or the backing-store resize is
        // collected by this pass instead of queueing another (see `schedule_widget_update`).
        let was_rendering = state.currently_rendering.replace(true);
        if !was_rendering {
            state.dirty.set(false);
        }
        {
            let mut bs = self.backing_store.borrow_mut();
            let mut pw = self.platform_window.borrow_mut();
            let presented = do_render_and_present(
                &mut **pw,
                &mut bs,
                &root,
                geom,
                state.device_pixel_ratio.get(),
            );
            state.bump(|s| {
                s.direct_render_count += 1;
                if presented.is_some() {
                    s.direct_present_count += 1;
                    s.last_painted_region = presented;
                }
            });
        }
        state.currently_rendering.set(was_rendering);
        // Borrows released: anything dirtied after this pass collected the region (or a render
        // parked by a borrow conflict) is queued.
        if !was_rendering && state.has_pending_invalidation() && !state.render_pending.get() {
            state.request_render();
        }
        // After the pending-render block: it borrows `self.render_state`, and the grow needs `&mut self`.
        // Qt activates the root layout on show (`qlayout.cpp:546-548` activates a visible parent only).
        if !was_rendering && self.is_visible() {
            if let Some(size) = self.sync_top_level_minimum() {
                let geom = self.geometry.get();
                self.set_geometry(Rect::new(geom.x, geom.y, size.width, size.height));
            }
        }
    }

    pub fn present_custom<F: FnOnce(&mut Painter)>(&mut self, f: F) {
        let geom = self.geometry.get();
        let dpr = self.render_state.device_pixel_ratio.get();

        let mut bs = self.backing_store.borrow_mut();
        bs.resize(Size::new(geom.width, geom.height), dpr);
        bs.fill(ferrokit_gui::tiny_skia::Color::TRANSPARENT);
        {
            let mut painter = Painter::begin(&mut **bs);
            f(&mut painter);
        }
        let mut pw = self.platform_window.borrow_mut();
        let phys_dirty = Rect::new(
            0,
            0,
            bs.physical_width() as i32,
            bs.physical_height() as i32,
        );
        let dirty_region = ferrokit_gui::geometry::Region::from_rect(phys_dirty);
        let _ = pw.present_region(&bs, &dirty_region);
    }

    /// Atomic geometry update and presentation for layered windows (e.g. cascading popup menus).
    /// Avoids SetWindowPos prior to painting, preventing visual jumping / tearing.
    /// `rect` maps to native pixels as `set_geometry` maps it.
    pub fn present_custom_at<F: FnOnce(&mut Painter)>(&mut self, rect: Rect, f: F) {
        let target = self.native_rect_for(rect);
        self.geometry.set(rect);
        self.render_state.track_screen(rect);
        let dpr = self.render_state.device_pixel_ratio.get();
        {
            let mut pw = self.platform_window.borrow_mut();
            pw.set_target_pos(Some(Point::new(target.x, target.y)));
        }
        let mut bs = self.backing_store.borrow_mut();
        bs.resize(Size::new(rect.width, rect.height), dpr);
        bs.fill(ferrokit_gui::tiny_skia::Color::TRANSPARENT);
        {
            let mut painter = Painter::begin(&mut **bs);
            f(&mut painter);
        }
        let mut pw = self.platform_window.borrow_mut();
        let phys_dirty = Rect::new(
            0,
            0,
            bs.physical_width() as i32,
            bs.physical_height() as i32,
        );
        let dirty_region = ferrokit_gui::geometry::Region::from_rect(phys_dirty);
        let _ = pw.present_region(&bs, &dirty_region);
    }

    pub fn backing_store(&self) -> std::cell::Ref<'_, BackingStore> {
        self.backing_store.borrow()
    }
    pub fn backing_store_handle(&self) -> std::rc::Rc<std::cell::RefCell<BackingStore>> {
        std::rc::Rc::clone(&self.backing_store)
    }

    pub fn save_png(&self, path: &std::path::Path) -> Result<(), &'static str> {
        self.backing_store.borrow().save_png(path).map_err(|_| "failed to save PNG")
    }

    /// Snapshot of the resize/render counters (diagnostics and tests).
    pub fn render_stats(&self) -> RenderStats {
        self.render_state.stats.get()
    }

    /// True while a native interactive sizing loop is active.
    pub fn is_interactive_resize(&self) -> bool {
        self.render_state.interactive_resize.get()
    }

    /// True when no repaint is owed or queued (`dirty`, `render_pending`, `currently_rendering` all clear).
    pub fn render_idle(&self) -> bool {
        let s = &self.render_state;
        !s.dirty.get() && !s.render_pending.get() && !s.currently_rendering.get()
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            let hwnd = self.platform_window.borrow().native_handle() as windows_sys::Win32::Foundation::HWND;
            ferrokit_platform::unregister_window_event_binding(hwnd);
        }
        // Deferred renders already queued for this window become no-ops.
        RenderState::unregister(self.object_data.id());
        crate::application::Application::unregister_window(self.object_data.id());
    }
}

impl QObject for Window {
    fn object_data(&self) -> &ObjectData {
        &self.object_data
    }

    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.object_data)
    }

    fn as_qobject_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_qobject_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn event(&mut self, event: &mut Event) -> bool {
        match &event.kind {
            // Direct/programmatic entry only. Native `WM_SIZE` no longer posts this (see
            // `WindowSystemEvent::Resize`, the canonical path); counted in `event_resize_count`.
            EventKind::Resize { width, height, .. } => {
                self.render_state.bump(|s| s.event_resize_count += 1);
                let mut cur = self.geometry.get();
                cur.width = *width;
                cur.height = *height;
                self.geometry.set(cur);

                // 1. Root geometry
                self.root_widget
                    .borrow_mut()
                    .set_geometry(Rect::new(0, 0, *width, *height));

                // 2. Resize callback
                if let Some(cb) = self.resize_cb.borrow().as_ref() {
                    self.render_state.bump(|s| s.resize_callback_count += 1);
                    cb(Size::new(*width, *height));
                }

                // 3. Layout Invalidation & Activation via LayoutScheduler
                crate::layout_scheduler::LayoutScheduler::request_layout(&self.root_widget);
                crate::layout_scheduler::LayoutScheduler::activate_pending();

                // 4. Backing store invalidation & paint
                self.render_and_present();
                true
            }
            EventKind::DpiChanged { dpi_x, .. } => {
                let old_dpr = self.render_state.device_pixel_ratio.get();
                let new_dpr = ferrokit_gui::round_scale_factor(*dpi_x as f64 / 96.0) as f32;
                self.render_state.device_pixel_ratio.set(new_dpr);
                let cur_geom = self.geometry.get();
                let size = Size::new(cur_geom.width, cur_geom.height);
                self.backing_store.borrow_mut().resize(size, new_dpr);
                crate::widget::adopt_tree(&self.root_widget);
                propagate_dpi_change_recursive(&self.root_widget, old_dpr, new_dpr);

                // Invalidate and re-layout root widget tree
                let mut root = self.root_widget.borrow_mut();
                let root_w = cur_geom.width;
                let root_h = cur_geom.height;
                root.set_geometry(Rect::new(0, 0, root_w, root_h));
                if let Some(layout) = root.layout_mut() {
                    layout.update_layout();
                }
                root.update();
                drop(root);
                self.render_and_present();
                true
            }
            _ => false,
        }
    }
}
/// Recursively propagates DPI change to all widgets in the tree,
/// triggering `dpi_changed_event` and refreshing nested layouts.
pub fn propagate_dpi_change_recursive(widget_ref: &WidgetRef, old_dpr: f32, new_dpr: f32) {
    let mut widget = widget_ref.borrow_mut();
    widget.dpi_changed_event(old_dpr, new_dpr);
    let geom = widget.geometry();
    if let Some(layout) = widget.layout_mut() {
        layout.set_geometry(Rect::new(0, 0, geom.width, geom.height));
    }
    let children = widget.children();
    drop(widget);

    for child in children {
        propagate_dpi_change_recursive(&child, old_dpr, new_dpr);
    }
}

/// Delivers a paint request to `widget` (`QWidget::event(Paint)`, qwidget.cpp:9186-9190): the
/// filters see the `Paint` event first, and a filter that consumes it skips `paint_event`.
/// `dirty` is the widget-local region being repainted.
pub fn deliver_paint_event(
    widget: &mut dyn crate::widget::Widget,
    painter: &mut Painter,
    dirty: Rect,
) {
    let mut paint = Event::new(EventKind::Paint {
        x: dirty.x,
        y: dirty.y,
        width: dirty.width,
        height: dirty.height,
    });
    if !filters_consume(widget.object_data(), &mut paint) {
        widget.paint_event(painter);
    }
}

fn render_widget_recursive(widget_ref: &WidgetRef, painter: &mut Painter, dirty_in_parent: Rect) {
    let mut widget = widget_ref.borrow_mut();
    if !widget.is_visible() {
        return;
    }

    let geom = widget.geometry();
    if !geom.intersects(&dirty_in_parent) {
        return;
    }

    let child_dirty = dirty_in_parent
        .intersected(&geom)
        .translated(-geom.x, -geom.y);

    painter.save();
    painter.translate(geom.x as f32, geom.y as f32);
    // Each widget paints only inside its own geometry, as `QWidgetPrivate::clipRect` intersects
    // the widget rect with every ancestor (qwidget.cpp:1904-1922). The clip is intersected, so a
    // child also stays inside its parent's clip.
    painter.clip_to_rect(RectF::new(0.0, 0.0, geom.width as f32, geom.height as f32));

    let dbg_w = ferrokit_platform::resize_debug::start();
    // `QWidget::event(Paint)` delivers the paint request through the filters before
    // `paintEvent` (qwidget.cpp:9186-9190).
    deliver_paint_event(&mut **widget, painter, child_dirty);
    if let Some(t0) = dbg_w {
        let us = t0.elapsed().as_micros();
        if us >= 1500 {
            ferrokit_platform::resize_debug::note(|| format!(
                "slow paint_event {:.1}ms: widget {:?} geom {}x{}@({},{})",
                us as f64 / 1000.0, widget.object_name(), geom.width, geom.height, geom.x, geom.y
            ));
        }
    }

    let children = widget.children();
    let children_clip = widget.children_clip_rect();
    drop(widget);
    crate::widget::adopt_children(widget_ref, &children);

    if let Some(clip) = children_clip {
        painter.save();
        painter.clip_to_rect(RectF::new(
            clip.x as f32,
            clip.y as f32,
            clip.width as f32,
            clip.height as f32,
        ));
    }
    for child in children {
        render_widget_recursive(&child, painter, child_dirty);
    }
    if children_clip.is_some() {
        painter.restore();
    }
    widget_ref.borrow_mut().paint_over_children(painter);

    painter.restore();
}

/// The root widget of every live window on this thread: where `QApplication::allWidgets` starts
/// for a style-sheet change that concerns the whole application.
pub(crate) fn window_roots() -> Vec<WidgetRef> {
    RENDER_STATES
        .try_with(|m| m.borrow().values().map(|s| s.root.borrow().clone()).collect())
        .unwrap_or_default()
}

/// Recursively collects and unifies dirty rectangles across the widget tree.
pub fn collect_dirty_region(widget_ref: &WidgetRef, offset: Point) -> Option<Rect> {
    let w = widget_ref.borrow_mut();
    let geom = w.geometry();
    let current_offset = Point::new(offset.x + geom.x, offset.y + geom.y);

    let mut dirty_union = w.dirty_rect().map(|d| {
        Rect::new(
            current_offset.x + d.x,
            current_offset.y + d.y,
            d.width,
            d.height,
        )
    });
    w.clear_dirty();

    let children = w.children();
    drop(w);

    for child in children {
        if let Some(child_dirty) = collect_dirty_region(&child, current_offset) {
            dirty_union = match dirty_union {
                Some(u) => Some(u.united(&child_dirty)),
                None => Some(child_dirty),
            };
        }
    }

    dirty_union
}
fn do_render_and_present(
    platform_window: &mut dyn PlatformWindow,
    backing_store: &mut BackingStore,
    root_widget: &WidgetRef,
    geometry: Rect,
    dpr: f32,
) -> Option<Rect> {
    let _t = ferrokit_gui::startup_trace::span_min(0.5, || "do_render_and_present".into());
    // Qt delivers the LayoutRequest events that text and size-hint changes post before the paint.
    crate::command::WidgetCommandQueue::flush_layouts();
    // Layouts a slot mutated while one of their items was borrowed ran no pass; run them now.
    crate::layout_scheduler::LayoutScheduler::flush_deferred(root_widget);
    // Qt posts the `LayoutRequest` to the widget that owns the invalidated layout, parent or not.
    // ferrokit queues it on the parent (`WidgetBase::request_layout`), which the root does not have.
    crate::layout_scheduler::LayoutScheduler::activate_if_dirty(root_widget);
    crate::widget::adopt_tree(root_widget);
    let logical_size = Size::new(geometry.width, geometry.height);

    // --- Lazy Backing Store Resize (Qt QWidgetRepaintManager::paintAndFlush parity) ---
    // If the backing store dimensions or DPR differ from the required top-level size,
    // reallocate lazily here, and mark the entire window dirty so the new buffer is fully painted.
    let dbg_bs = ferrokit_platform::resize_debug::start();
    if backing_store.resize_to_native(logical_size, dpr, platform_window.native_size()) {
        ferrokit_platform::resize_debug::count(ferrokit_platform::resize_debug::Count::BackingStoreResized);
        root_widget.borrow_mut().update();
    }
    ferrokit_platform::resize_debug::end(ferrokit_platform::resize_debug::Phase::BackingStoreResize, dbg_bs);

    let root_geom = Rect::new(0, 0, geometry.width, geometry.height);
    let dbg_col = ferrokit_platform::resize_debug::start();
    let dirty = collect_dirty_region(root_widget, Point::new(0, 0))
        .unwrap_or(root_geom)
        .intersected(&root_geom);
    ferrokit_platform::resize_debug::end(ferrokit_platform::resize_debug::Phase::CollectDirty, dbg_col);
    if dirty.is_empty() {
        ferrokit_platform::resize_debug::count(ferrokit_platform::resize_debug::Count::NoDirtyRegion);
        return None;
    }

    let phys_dirty = if dpr > 1.0 {
        ferrokit_platform::high_dpi::to_native_rect(dirty, dpr)
    } else {
        dirty
    };

    let dbg_clr = ferrokit_platform::resize_debug::start();
    backing_store.clear_rect(phys_dirty);
    ferrokit_platform::resize_debug::end(ferrokit_platform::resize_debug::Phase::Clear, dbg_clr);

    let dbg_paint = ferrokit_platform::resize_debug::start();
    let _paint_span = ferrokit_gui::startup_trace::span_min(0.5, || "paint (render_widget_recursive)".into());
    {
        let mut painter = Painter::begin(&mut **backing_store);
        painter.set_clip_rect(RectF::new(
            dirty.x as f32,
            dirty.y as f32,
            dirty.width as f32,
            dirty.height as f32,
        ));
        render_widget_recursive(root_widget, &mut painter, dirty);
    }
    ferrokit_platform::resize_debug::end(ferrokit_platform::resize_debug::Phase::Paint, dbg_paint);
    drop(_paint_span);

    let dirty_region = ferrokit_gui::geometry::Region::from_rect(phys_dirty);
    let _present_span = ferrokit_gui::startup_trace::span_min(0.5, || "present_region".into());
    let _ = platform_window.present_region(backing_store, &dirty_region);
    Some(dirty)
}

/// The press a double click is measured against (`qguiapplication.cpp:2401-2426`: the statics
/// `lastPressTimestamp`, `lastPressWindow`, `mousePressButton`, `globalPressPosition`).
struct PressRecord {
    window: ObjectId,
    button: ferrokit_platform::MouseButton,
    global_pos: Point,
    timestamp: u32,
}

/// Qt keeps this in process-wide statics. Keyed by window, so presses in one window never pair
/// with presses in another.
static PENDING_PRESS: std::sync::Mutex<Option<PressRecord>> = std::sync::Mutex::new(None);

/// Qt's press rule (`qguiapplication.cpp:2419-2426`): every press updates the record, and a press
/// is a double click when it lands within the interval of the last press, with the same button, in
/// the same window. Qt has no distance check on presses; the move rule cancels instead.
fn press_is_double(
    window: ObjectId,
    button: ferrokit_platform::MouseButton,
    global_pos: Point,
    timestamp: u32,
) -> bool {
    let interval =
        ferrokit_gui::application::GuiApplication::style_hints().mouse_double_click_interval_ms;
    let mut pending = PENDING_PRESS.lock().unwrap_or_else(|e| e.into_inner());
    let double = pending.as_ref().is_some_and(|prev| {
        let delta = timestamp.wrapping_sub(prev.timestamp);
        delta > 0 && delta < interval && prev.button == button && prev.window == window
    });
    *pending = Some(PressRecord {
        window,
        button,
        global_pos,
        timestamp,
    });
    double
}

/// Qt's move rule (`qguiapplication.cpp:2407-2414`): a pointer that moved more than the double
/// click distance from the last press on either axis cancels the pending press. The test is
/// strict, so a move of exactly the distance keeps it.
fn move_cancels_press(global_pos: Point) {
    let distance = ferrokit_gui::application::GuiApplication::style_hints().mouse_double_click_distance;
    let mut pending = PENDING_PRESS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(prev) = pending.as_ref() {
        let dx = (global_pos.x - prev.global_pos.x).abs();
        let dy = (global_pos.y - prev.global_pos.y).abs();
        if dx > distance || dy > distance {
            *pending = None;
        }
    }
}

/// A native double click ends the press pair, so the next press starts a new one.
fn clear_pending_press() {
    *PENDING_PRESS.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

impl WindowEventHandler {
    /// `QWidgetWindow` delivers a double click as a MouseButtonDblClick event, and falls back to
    /// the press handler when nothing accepts it (`QWidget::mouseDoubleClickEvent`). A press the
    /// detector promotes goes through here too, so it is never delivered as a press
    /// (`qwidgetwindow.cpp:570`).
    fn deliver_double_click(&mut self, pos: Point, button: ferrokit_platform::MouseButton) {
        let root = self.core.root();
        let btn = match button {
            ferrokit_platform::MouseButton::Left => 1,
            ferrokit_platform::MouseButton::Right => 2,
            ferrokit_platform::MouseButton::Middle => 3,
            _ => 0,
        };
        let mut ev = Event::new_spontaneous(EventKind::MouseButtonDblClick {
            x: pos.x,
            y: pos.y,
            button: btn,
        });
        if !self.dispatcher.dispatch_event(&root, &mut ev) {
            let mut unhandled = Event::new_spontaneous(EventKind::MouseButtonDblClick {
                x: pos.x,
                y: pos.y,
                button: btn,
            });
            let handled = match self.core.event_cb.borrow().as_ref() {
                Some(cb) => {
                    cb(&mut unhandled);
                    unhandled.is_accepted()
                }
                None => false,
            };
            if !handled {
                // QWidget::mouseDoubleClickEvent default: mousePressEvent.
                if let Some(cb) = self.mouse_press_cb.borrow().as_ref() {
                    cb(pos, button);
                }
            }
        }
    }
}

struct WindowEventHandler {
    platform_window: std::rc::Rc<std::cell::RefCell<Box<dyn PlatformWindow>>>,
    backing_store: std::rc::Rc<std::cell::RefCell<BackingStore>>,
    geometry: std::rc::Rc<std::cell::Cell<Rect>>,
    core: WindowCore,
    dispatcher: EventTreeDispatcher,
    context_menu_cb: ContextMenuCallback,
    mouse_press_cb: MousePressCallback,
    mouse_move_cb: MouseMoveCallback,
    resize_cb: ResizeCallback,
    render: std::rc::Rc<RenderState>,
    native_double_clicks: bool,
    /// The last minimized state reported by the platform (`QWidget::event` WindowStateChange
    /// compares `oldState() & WindowMinimized` with `isMinimized()`, qwidget.cpp:9293-9294).
    minimized: bool,
}

impl WindowSystemEventHandler for WindowEventHandler {
    fn handle_window_event(&mut self, event: WindowSystemEvent) {
        let _t = ferrokit_gui::startup_trace::span_min(2.0, || {
            let d = format!("{event:?}");
            format!("handle_window_event {}", d.chars().take(60).collect::<String>())
        });
        let root = self.core.root();
        match event {
            WindowSystemEvent::MouseMove {
                pos,
                global_pos,
                buttons,
            } => {
                if !self.native_double_clicks {
                    move_cancels_press(global_pos);
                }
                self.dispatcher
                    .dispatch_mouse_move(&root, pos, global_pos, buttons);
                if let Some(cb) = self.mouse_move_cb.borrow().as_ref() {
                    cb(pos);
                }
                if self.render.has_pending_invalidation() {
                    self.render.request_render();
                }
            }
            WindowSystemEvent::MouseLeave => {
                self.dispatcher.handle_mouse_leave();
                if self.render.has_pending_invalidation() {
                    self.render.request_render();
                }
            }
            WindowSystemEvent::MousePress {
                pos,
                global_pos,
                button,
                timestamp,
                ..
            } => {
                // Platforms that report their own double clicks never synthesize one here.
                if !self.native_double_clicks
                    && press_is_double(self.core.id, button, global_pos, timestamp)
                {
                    self.deliver_double_click(pos, button);
                    return;
                }
                let btn = match button {
                    ferrokit_platform::MouseButton::Left => 1,
                    ferrokit_platform::MouseButton::Right => 2,
                    ferrokit_platform::MouseButton::Middle => 3,
                    _ => 0,
                };
                let mut ev = Event::new_spontaneous(EventKind::MouseButtonPress {
                    x: pos.x,
                    y: pos.y,
                    button: btn,
                });
                let consumed = self.dispatcher.dispatch_event(&root, &mut ev);
                if !consumed {
                    if let Some(cb) = self.mouse_press_cb.borrow().as_ref() {
                        cb(pos, button);
                    }
                }
            }
            WindowSystemEvent::MouseRelease { pos, global_pos, button, .. } => {
                let btn = match button {
                    ferrokit_platform::MouseButton::Left => 1,
                    ferrokit_platform::MouseButton::Right => 2,
                    ferrokit_platform::MouseButton::Middle => 3,
                    _ => 0,
                };
                let mut ev = Event::new_spontaneous(EventKind::MouseButtonRelease {
                    x: pos.x,
                    y: pos.y,
                    button: btn,
                });
                if !self.dispatcher.dispatch_event(&root, &mut ev) {
                    let mut unhandled = Event::new_spontaneous(EventKind::MouseButtonRelease {
                        x: pos.x,
                        y: pos.y,
                        button: btn,
                    });
                    if let Some(cb) = self.core.event_cb.borrow().as_ref() {
                        cb(&mut unhandled);
                    }
                }
                if button == ferrokit_platform::MouseButton::Right {
                    // qwidgetwindow.cpp:639-640 sends the context menu after a right-button release,
                    // as a ContextMenu event to the widget under the cursor. Qt 6 has no acceptance
                    // gate on that release (qwidgetwindow.cpp:696-702), so the window callback runs
                    // only when no widget accepted the menu event.
                    let mut menu = Event::new_spontaneous(EventKind::ContextMenu {
                        x: pos.x,
                        y: pos.y,
                        global_x: global_pos.x,
                        global_y: global_pos.y,
                        reason: ContextMenuReason::Mouse,
                    });
                    if !self.dispatcher.dispatch_event(&root, &mut menu) {
                        if let Some(cb) = self.context_menu_cb.borrow().as_ref() {
                            cb(global_pos);
                        }
                    }
                }
            }
            WindowSystemEvent::MouseDoubleClick { pos, button, .. } => {
                // A native double click ends the press pair, so the next press starts a new one.
                clear_pending_press();
                self.deliver_double_click(pos, button);
            }
            WindowSystemEvent::Wheel {
                pos,
                delta,
                modifiers,
                ..
            } => {
                let mut ev = Event::new_spontaneous(EventKind::Wheel {
                    x: pos.x,
                    y: pos.y,
                    pixel_delta_x: delta.x,
                    pixel_delta_y: delta.y,
                    angle_delta_x: delta.x,
                    angle_delta_y: delta.y,
                    modifiers: modifiers.bits(),
                });
                self.dispatcher.dispatch_event(&root, &mut ev);
            }
            WindowSystemEvent::InteractiveResizeStart => {
                self.render.interactive_resize.set(true);
            }
            WindowSystemEvent::InteractiveResizeEnd => {
                self.render.end_interactive_resize();
            }
            WindowSystemEvent::GeometryChange { geometry } => {
                let (old_pos, _) = {
                    let mut cur = self.geometry.get();
                    let old_pos = Point::new(cur.x, cur.y);
                    cur.x = geometry.x;
                    cur.y = geometry.y;
                    self.geometry.set(cur);
                    self.render.track_screen(cur);
                    (old_pos, ())
                };

                let pos_changed = old_pos.x != geometry.x || old_pos.y != geometry.y;

                if pos_changed {
                    let mut ev = Event::new_spontaneous(EventKind::Move {
                        x: geometry.x,
                        y: geometry.y,
                        old_x: old_pos.x,
                        old_y: old_pos.y,
                    });
                    // QMoveEvent goes to the window widget only, not to its children.
                    self.core.send_to_window(&mut ev);
                }
            }
            WindowSystemEvent::CloseRequest => {
                self.core.close(true);
            }
            WindowSystemEvent::Resize { size } => {
                let old_size = {
                    let mut cur = self.geometry.get();
                    let old_size = Size::new(cur.width, cur.height);
                    cur.width = size.width;
                    cur.height = size.height;
                    self.geometry.set(cur);
                    self.render.track_screen(cur);
                    old_size
                };

                let size_changed = old_size.width != size.width || old_size.height != size.height;
                ferrokit_platform::resize_debug::note(|| format!(
                    "Resize event {}x{} (logical) vs widget geometry {}x{}: {}; within_set_geometry={} interactive={}",
                    size.width, size.height, old_size.width, old_size.height,
                    if size_changed { "changed" } else { "UNCHANGED -> no layout, no render" },
                    self.render.within_set_geometry.get(),
                    self.render.interactive_resize.get(),
                ));

                if size_changed {
                    self.render.bump(|s| s.resize_event_count += 1);
                    // Steps 1-3 end in a render of this window (step 4, or set_geometry's own),
                    // which collects their widget dirt.
                    let follows = ScopedFlag::set(&self.render.render_follows);
                    // 1. Update root widget geometry (widget geometry = new size)
                    let dbg_disp = ferrokit_platform::resize_debug::start();
                    root.borrow_mut().set_geometry(Rect::new(0, 0, size.width, size.height));

                    // 2. Dispatch Resize event to widgets and callback (never coalesced)
                    let mut ev = Event::new_spontaneous(EventKind::Resize {
                        width: size.width,
                        height: size.height,
                        old_width: old_size.width,
                        old_height: old_size.height,
                    });
                    self.dispatcher.dispatch_event(&root, &mut ev);
                    ferrokit_platform::resize_debug::end(ferrokit_platform::resize_debug::Phase::ResizeDispatch, dbg_disp);

                    let dbg_cb = ferrokit_platform::resize_debug::start();
                    if let Some(cb) = self.resize_cb.borrow().as_ref() {
                        self.render.bump(|s| s.resize_callback_count += 1);
                        cb(size);
                    }
                    ferrokit_platform::resize_debug::end(ferrokit_platform::resize_debug::Phase::ResizeCallback, dbg_cb);

                    // 3. Invalidate layout (deduplicated by LayoutScheduler); activation, paint
                    //    and present happen once in the coalesced deferred render.
                    let dbg_inv = ferrokit_platform::resize_debug::start();
                    crate::layout_scheduler::LayoutScheduler::request_layout(&root);
                    ferrokit_platform::resize_debug::end(ferrokit_platform::resize_debug::Phase::LayoutInvalidate, dbg_inv);

                    drop(follows);
                    // 4. Window::set_geometry renders synchronously itself (explicit flag,
                    //    not RefCell borrow state).
                    if !self.render.within_set_geometry.get() {
                        if self.render.interactive_resize.get() {
                            // Native sizing loop: layout activation + paint + present now.
                            ferrokit_platform::resize_debug::note(|| "-> render_now".to_string());
                            self.render.render_now();
                        } else {
                            ferrokit_platform::resize_debug::note(|| "-> request_render (deferred)".to_string());
                            self.render.request_render();
                        }
                    }
                }
            }
            WindowSystemEvent::KeyPress {
                key,
                modifiers,
                is_repeat,
            } => {
                let mut ev = Event::new_spontaneous(EventKind::KeyPress {
                    key,
                    modifiers: modifiers.bits(),
                    is_repeat,
                });
                self.dispatcher.dispatch_event(&root, &mut ev);
            }
            WindowSystemEvent::KeyRelease { key, modifiers } => {
                let mut ev = Event::new_spontaneous(EventKind::KeyRelease {
                    key,
                    modifiers: modifiers.bits(),
                });
                self.dispatcher.dispatch_event(&root, &mut ev);
            }
            WindowSystemEvent::FocusIn => {
                self.core.set_active(true);
                let mut ev = Event::new_spontaneous(EventKind::FocusIn {
                    reason: ferrokit_core::event::FocusReason::ActiveWindow,
                });
                self.dispatcher.dispatch_event(&root, &mut ev);
            }
            WindowSystemEvent::FocusOut => {
                self.core.set_active(false);
                let mut ev = Event::new_spontaneous(EventKind::FocusOut {
                    reason: ferrokit_core::event::FocusReason::ActiveWindow,
                });
                self.dispatcher.dispatch_event(&root, &mut ev);
            }
            WindowSystemEvent::DpiChanged { dpi_x, .. } => {
                let old_dpr = self.render.device_pixel_ratio.get();
                let new_dpr = ferrokit_gui::round_scale_factor(dpi_x as f64 / 96.0) as f32;
                self.render.device_pixel_ratio.set(new_dpr);
                let cur_geom = self.geometry.get();
                let size = Size::new(cur_geom.width, cur_geom.height);
                self.backing_store.borrow_mut().resize(size, new_dpr);
                propagate_dpi_change_recursive(&root, old_dpr, new_dpr);

                {
                    // The borrow ends before the render below, which borrows the root again.
                    let mut root_borrow = root.borrow_mut();
                    root_borrow.set_geometry(Rect::new(0, 0, cur_geom.width, cur_geom.height));
                    if let Some(layout) = root_borrow.layout_mut() {
                        layout.update_layout();
                    }
                    root_borrow.update();
                }

                if let Ok(mut pw) = self.platform_window.try_borrow_mut() {
                    let mut bs = self.backing_store.borrow_mut();
                    do_render_and_present(
                        &mut **pw,
                        &mut bs,
                        &root,
                        cur_geom,
                        self.render.device_pixel_ratio.get(),
                    );
                }
            }
            WindowSystemEvent::InputMethod {
                commit_string,
                preedit_string,
                cursor_position,
            } => {
                let mut ev = Event::new_spontaneous(EventKind::InputMethod {
                    commit_string,
                    preedit_string,
                    cursor_position,
                });
                self.dispatcher.dispatch_event(&root, &mut ev);
            }
            WindowSystemEvent::DragEnter {
                pos,
                formats,
                drop_action,
            } => {
                let mut ev = Event::new_spontaneous(EventKind::DragEnter {
                    pos_x: pos.x,
                    pos_y: pos.y,
                    formats,
                    drop_action,
                });
                self.dispatcher.dispatch_event(&root, &mut ev);
            }
            WindowSystemEvent::DragMove { pos, drop_action } => {
                let mut ev = Event::new_spontaneous(EventKind::DragMove {
                    pos_x: pos.x,
                    pos_y: pos.y,
                    drop_action,
                });
                self.dispatcher.dispatch_event(&root, &mut ev);
            }
            WindowSystemEvent::DragLeave => {
                let mut ev = Event::new_spontaneous(EventKind::DragLeave);
                self.dispatcher.dispatch_event(&root, &mut ev);
            }
            WindowSystemEvent::Drop {
                pos,
                formats,
                data,
                drop_action,
            } => {
                let mut ev = Event::new_spontaneous(EventKind::Drop {
                    pos_x: pos.x,
                    pos_y: pos.y,
                    formats,
                    data,
                    drop_action,
                });
                self.dispatcher.dispatch_event(&root, &mut ev);
            }
            WindowSystemEvent::Minimize { minimized } => {
                // Qt acts only when the minimized bit flips (`QWidget::event` WindowStateChange,
                // qwidget.cpp:9292-9311). Minimize sends a spontaneous Hide first and then hides the
                // children; restore shows the children first and then sends a spontaneous Show.
                // `visible` is untouched, so `isVisible()` stays true while minimized.
                if minimized != self.minimized {
                    self.minimized = minimized;
                    if minimized {
                        self.core
                            .send_to_window(&mut Event::new_spontaneous(EventKind::Hide));
                        hide_children(&root);
                    } else {
                        show_children_first(&root);
                        self.core
                            .send_to_window(&mut Event::new_spontaneous(EventKind::Show));
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    #[test]
    fn test_native_geometry_change_dispatches_resize_callback_exactly_once() {
        let mut win = Window::new(
            "Single Resize Test",
            Rect::new(0, 0, 300, 200),
            WindowFlags::empty(),
        )
        .expect("create test window");

        let resize_count = Arc::new(AtomicU32::new(0));
        let count_cb = Arc::clone(&resize_count);
        win.set_resize_handler(move |_size: Size| {
            count_cb.fetch_add(1, Ordering::SeqCst);
        });

        #[cfg(windows)]
        {
            use ferrokit_platform::handle_geometry_change;
            use ferrokit_platform::window_system_interface::Delivery;

            let hwnd = win.native_handle() as windows_sys::Win32::Foundation::HWND;

            // 1. Initial geometry change (both pos and size change: 0,0,300,200 -> 50,60,500,400)
            handle_geometry_change(Delivery::Default, hwnd, Rect::new(50, 60, 500, 400));
            assert_eq!(win.geometry().x, 50);
            assert_eq!(win.geometry().y, 60);
            assert_eq!(win.geometry().width, 500);
            assert_eq!(win.geometry().height, 400);
            assert_eq!(
                resize_count.load(Ordering::SeqCst),
                1,
                "Resize callback must be executed exactly once per native resize event"
            );

            // 2. Position-only change (50,60,500,400 -> 100,120,500,400)
            handle_geometry_change(Delivery::Default, hwnd, Rect::new(100, 120, 500, 400));
            assert_eq!(win.geometry().x, 100);
            assert_eq!(win.geometry().y, 120);
            assert_eq!(win.geometry().width, 500);
            assert_eq!(win.geometry().height, 400);
            assert_eq!(
                resize_count.load(Ordering::SeqCst),
                1,
                "Position-only geometry change must NOT trigger resize callback"
            );

            // 3. Second resize change (100,120,500,400 -> 100,120,600,450)
            handle_geometry_change(Delivery::Default, hwnd, Rect::new(100, 120, 600, 450));
            assert_eq!(win.geometry().x, 100);
            assert_eq!(win.geometry().y, 120);
            assert_eq!(win.geometry().width, 600);
            assert_eq!(win.geometry().height, 450);
            assert_eq!(
                resize_count.load(Ordering::SeqCst),
                2,
                "Second resize change must increment callback execution by exactly once"
            );
        }
    }
}
