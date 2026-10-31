// SPDX-License-Identifier: LGPL-3.0-only
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Arc;

use ferrokit_core::application::{ApplicationAttribute, CoreApplication};
use ferrokit_core::event::{Event, EventFilter, EventKind, FilterResult};
use ferrokit_core::object::{register_owned_qobject, ObjectData, ObjectId, OwnedObject, QObject};
use ferrokit_gui::application::{ApplicationState, GuiApplication, LayoutDirection};
use ferrokit_gui::geometry::primitives::Rect;
use ferrokit_gui::paint::palette::{ColorGroup, ColorRole, Palette};
use ferrokit_gui::text::font::Font;
use ferrokit_platform::WindowFlags;
use ferrokit_widgets::application::Application;
use ferrokit_widgets::widget::EmptyWidget;
use ferrokit_widgets::window::Window;
static TEST_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

// -----------------------------------------------------------------------------
// Test 1: CoreApplication Lifecycle, Arguments, Metadata & Attributes
// -----------------------------------------------------------------------------

#[test]
fn test_core_application_metadata_and_attributes() {
    let _guard = TEST_MUTEX.lock().unwrap();
    CoreApplication::reset_for_test();
    assert!(!CoreApplication::instance_exists());

    let app = CoreApplication::new(vec![
        "myapp_binary".to_string(),
        "--config=production".to_string(),
        "--port=8080".to_string(),
    ]);

    assert!(CoreApplication::instance_exists());
    assert!(CoreApplication::instance_id().is_some());

    // 1. Arguments reflection
    let args = CoreApplication::arguments();
    assert_eq!(args.len(), 3);
    assert_eq!(args[1], "--config=production");
    assert_eq!(args[2], "--port=8080");

    // 2. Application metadata
    CoreApplication::set_application_name("SuperApp");
    assert_eq!(CoreApplication::application_name(), "SuperApp");

    CoreApplication::set_application_version("2.5.0");
    assert_eq!(CoreApplication::application_version(), "2.5.0");

    CoreApplication::set_organization_name("MyOrg");
    assert_eq!(CoreApplication::organization_name(), "MyOrg");

    CoreApplication::set_organization_domain("myorg.io");
    assert_eq!(CoreApplication::organization_domain(), "myorg.io");

    // 3. Application Attributes
    assert!(!CoreApplication::test_attribute(
        ApplicationAttribute::AaUseHighDpiPixmaps
    ));
    CoreApplication::set_attribute(ApplicationAttribute::AaUseHighDpiPixmaps, true);
    assert!(CoreApplication::test_attribute(
        ApplicationAttribute::AaUseHighDpiPixmaps
    ));
    CoreApplication::set_attribute(ApplicationAttribute::AaUseHighDpiPixmaps, false);
    assert!(!CoreApplication::test_attribute(
        ApplicationAttribute::AaUseHighDpiPixmaps
    ));

    // 4. Clean exit
    drop(app);
    assert!(!CoreApplication::instance_exists());
}
#[test]
fn test_second_core_application_instance_panics() {
    let _guard = TEST_MUTEX.lock().unwrap();
    CoreApplication::reset_for_test();
    let _app1 = CoreApplication::new(vec!["app1".to_string()]);
    let res = std::panic::catch_unwind(|| {
        let _app2 = CoreApplication::new(vec!["app2".to_string()]);
    });
    assert!(
        res.is_err(),
        "Second CoreApplication instance must panic (qcoreapplication.cpp:801)"
    );
    if let Err(err) = res {
        let msg = if let Some(s) = err.downcast_ref::<&str>() {
            *s
        } else if let Some(s) = err.downcast_ref::<String>() {
            s.as_str()
        } else {
            ""
        };
        assert!(
            msg.contains("there should be only one application object"),
            "Panic message must mention single application object: {msg}"
        );
    }
}

// -----------------------------------------------------------------------------
// Test 2: CoreApplication Exec, ProcessEvents, and AboutToQuit Signal
// -----------------------------------------------------------------------------

#[test]
fn test_core_application_exec_quit_and_about_to_quit() {
    let _guard = TEST_MUTEX.lock().unwrap();
    CoreApplication::reset_for_test();

    let mut app = CoreApplication::new(vec!["test_runner".to_string()]);

    let about_to_quit_count = Arc::new(AtomicI32::new(0));
    let count_clone = Arc::clone(&about_to_quit_count);
    app.about_to_quit().connect(move |_| {
        count_clone.fetch_add(1, Ordering::SeqCst);
    });

    // Qt: exit() before exec() emits aboutToQuit (qcoreapplication.cpp:1532-1535)
    // but does not end the next exec (qeventloop.cpp:157-158).
    CoreApplication::exit(42);
    assert_eq!(about_to_quit_count.load(Ordering::SeqCst), 1);

    ferrokit_core::timer::Timer::single_shot(10, || CoreApplication::exit(42));
    let code = app.exec();
    assert_eq!(code, 42);
    // aboutToQuit flag reset on exec entry (qcoreapplication.cpp:1468), emitted again in exit().
    assert_eq!(about_to_quit_count.load(Ordering::SeqCst), 2);

    // Process events test - ensure non-blocking call succeeds without panic
    let _ = CoreApplication::process_events(false);
    drop(app);
}

#[test]
fn about_to_quit_emitted_inside_exit_before_exec_returns() {
    let _guard = TEST_MUTEX.lock().unwrap();
    CoreApplication::reset_for_test();
    let mut app = CoreApplication::new(vec!["test_runner".to_string()]);

    let count = Arc::new(AtomicI32::new(0));
    let count_clone = Arc::clone(&count);
    app.about_to_quit().connect(move |_| {
        count_clone.fetch_add(1, Ordering::SeqCst);
    });

    // Qt emits aboutToQuit from QCoreApplication::exit itself (qcoreapplication.cpp:1532-1535),
    // not after exec() returns.
    let seen_inside_exit = Arc::new(AtomicI32::new(-1));
    let seen_clone = Arc::clone(&seen_inside_exit);
    let count_for_timer = Arc::clone(&count);
    ferrokit_core::timer::Timer::single_shot(10, move || {
        CoreApplication::exit(5);
        seen_clone.store(count_for_timer.load(Ordering::SeqCst), Ordering::SeqCst);
    });

    assert_eq!(app.exec(), 5);
    assert_eq!(seen_inside_exit.load(Ordering::SeqCst), 1);
    assert_eq!(count.load(Ordering::SeqCst), 1);
    drop(app);
}

#[test]
fn quit_ignored_before_exec() {
    let _guard = TEST_MUTEX.lock().unwrap();
    CoreApplication::reset_for_test();
    let mut app = CoreApplication::new(vec!["test_runner".to_string()]);

    let count = Arc::new(AtomicI32::new(0));
    let count_clone = Arc::clone(&count);
    app.about_to_quit().connect(move |_| {
        count_clone.fetch_add(1, Ordering::SeqCst);
    });

    // QCoreApplication::quit() is a no-op outside exec (qcoreapplication.cpp:2143-2144).
    CoreApplication::quit();
    assert_eq!(count.load(Ordering::SeqCst), 0);

    ferrokit_core::timer::Timer::single_shot(10, || CoreApplication::exit(3));
    assert_eq!(app.exec(), 3);
    drop(app);
}

// -----------------------------------------------------------------------------
// Test 3: Application Event Filtering & Synchronous send_event
// -----------------------------------------------------------------------------

struct SpyEventFilter {
    data: ObjectData,
    pub filtered_count: Arc<AtomicI32>,
}

impl SpyEventFilter {
    fn new(counter: Arc<AtomicI32>) -> Self {
        Self {
            data: ObjectData::with_auto_id(),
            filtered_count: counter,
        }
    }
}

impl QObject for SpyEventFilter {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
    }
    fn as_qobject_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn as_qobject_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
    fn meta_object(&self) -> &'static ferrokit_core::meta::MetaObject {
        &ferrokit_core::meta::QOBJECT_META_OBJECT
    }
    fn event(&mut self, _event: &mut Event) -> bool {
        true
    }
    fn event_filter(&mut self, _watched: ObjectId, _event: &mut Event) -> bool {
        self.filtered_count.fetch_add(1, Ordering::SeqCst);
        false
    }
}

impl EventFilter for SpyEventFilter {
    fn event_filter(&mut self, _watched: ObjectId, _event: &mut Event) -> FilterResult {
        self.filtered_count.fetch_add(1, Ordering::SeqCst);
        FilterResult::Pass
    }
}

#[test]
fn test_application_level_event_filters() {
    let _guard = TEST_MUTEX.lock().unwrap();
    CoreApplication::reset_for_test();

    let app = CoreApplication::new(vec![]);
    let count = Arc::new(AtomicI32::new(0));

    // SAFETY: test-only; the owners stay on this thread for the whole test, and nothing touches
    // either object while a callback of it runs.
    let filter_owner = unsafe { register_owned_qobject(SpyEventFilter::new(Arc::clone(&count))) };
    let filter_id = OwnedObject::object_id(&filter_owner);

    CoreApplication::install_event_filter(filter_id);

    // Create and register a test target object
    // SAFETY: as above.
    let target_owner =
        unsafe { register_owned_qobject(SpyEventFilter::new(Arc::new(AtomicI32::new(0)))) };
    let target_id = OwnedObject::object_id(&target_owner);

    let mut ev = Event::new(EventKind::UpdateRequest);
    let handled = CoreApplication::send_event(target_id, &mut ev);
    assert!(handled);
    assert_eq!(count.load(Ordering::SeqCst), 1);

    // Remove event filter
    CoreApplication::remove_event_filter(filter_id);
    let mut ev2 = Event::new(EventKind::UpdateRequest);
    let handled2 = CoreApplication::send_event(target_id, &mut ev2);
    assert!(handled2);

    assert_eq!(count.load(Ordering::SeqCst), 1); // Should not increase

    drop(app);
}

// -----------------------------------------------------------------------------
// Test 4: GuiApplication Palette, Font, StyleHints & ApplicationState
// -----------------------------------------------------------------------------

#[test]
fn test_gui_application_palette_font_and_state() {
    let _guard = TEST_MUTEX.lock().unwrap();
    GuiApplication::reset_for_test();

    let gui_app = GuiApplication::new(vec!["gui_test".to_string()]);

    // 1. Global palette
    let dark_pal = Palette::dark();
    GuiApplication::set_palette(dark_pal);
    let current_pal = GuiApplication::palette();
    let window_bg = current_pal.color(ColorGroup::Active, ColorRole::Window);
    assert_eq!(
        window_bg,
        ferrokit_gui::tiny_skia::Color::from_rgba8(30, 30, 30, 255)
    );
    // 2. Global font
    let font = Font::new("Segoe UI", 12.0);
    GuiApplication::set_font(font);
    assert_eq!(GuiApplication::font().family, "Segoe UI");
    assert_eq!(GuiApplication::font().size, 12.0);
    // 3. Style Hints
    let mut hints = GuiApplication::style_hints();
    hints.mouse_double_click_interval_ms = 600;
    GuiApplication::set_style_hints(hints);
    assert_eq!(
        GuiApplication::style_hints().mouse_double_click_interval_ms,
        600
    );

    // 4. Layout direction
    assert!(GuiApplication::is_left_to_right());
    GuiApplication::set_layout_direction(LayoutDirection::RightToLeft);
    assert!(GuiApplication::is_right_to_left());

    // 5. Application state change signal
    let state_notified = Arc::new(AtomicBool::new(false));
    let state_clone = Arc::clone(&state_notified);
    gui_app.application_state_changed().connect(move |state| {
        if *state == ApplicationState::ApplicationInactive {
            state_clone.store(true, Ordering::SeqCst);
        }
    });

    gui_app.set_application_state(ApplicationState::ApplicationInactive);
    assert!(state_notified.load(Ordering::SeqCst));
    assert_eq!(
        GuiApplication::application_state(),
        ApplicationState::ApplicationInactive
    );

    drop(gui_app);
}

// -----------------------------------------------------------------------------
// Test 5: Application Top-Level Window Lifecycle & Focus Tracking
// -----------------------------------------------------------------------------

#[test]
fn test_widget_application_window_registry_and_focus() {
    let _guard = TEST_MUTEX.lock().unwrap();
    Application::reset_for_test();

    let app = Application::new(vec!["widget_test".to_string()]);

    assert_eq!(Application::top_level_windows().len(), 0);

    // Create top-level windows
    let win1 = Window::new(
        "Window 1",
        Rect::new(100, 100, 400, 300),
        WindowFlags::NORMAL,
    )
    .expect("Failed to create win1");
    let win2 = Window::new(
        "Window 2",
        Rect::new(150, 150, 400, 300),
        WindowFlags::NORMAL,
    )
    .expect("Failed to create win2");

    let windows = Application::top_level_windows();
    assert_eq!(windows.len(), 2);
    assert!(windows.contains(&win1.id()));
    assert!(windows.contains(&win2.id()));

    // Active window tracking
    Application::set_active_window(Some(win1.id()));
    assert_eq!(Application::active_window(), Some(win1.id()));

    // Focus widget tracking
    let focus_changed_received = Arc::new(AtomicBool::new(false));
    let focus_clone = Arc::clone(&focus_changed_received);
    app.focus_changed().connect(move |(old_id, new_id)| {
        if old_id.is_none() && new_id.is_some() {
            focus_clone.store(true, Ordering::SeqCst);
        }
    });

    let widget_ref: ferrokit_widgets::widget::WidgetRef = Rc::new(RefCell::new(Box::new(
        EmptyWidget::with_geometry(Rect::new(0, 0, 50, 50)),
    )));
    app.set_focus_widget(Some(Rc::clone(&widget_ref)));
    assert!(focus_changed_received.load(Ordering::SeqCst));
    assert!(Application::focus_widget().is_some());

    // Interactive settings
    Application::set_double_click_interval(500);
    assert_eq!(Application::double_click_interval(), 500);
    assert_eq!(
        GuiApplication::style_hints().mouse_double_click_interval_ms,
        500
    );

    Application::set_wheel_scroll_lines(5);
    assert_eq!(Application::wheel_scroll_lines(), 5);

    // Closing/dropping win1 reduces registry
    drop(win1);
    let remaining = Application::top_level_windows();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0], win2.id());

    // Dropping win2 empties registry
    drop(win2);
    assert_eq!(Application::top_level_windows().len(), 0);

    drop(app);
}

// -----------------------------------------------------------------------------
// Window close and drop vs. quit-on-last-window-closed
// -----------------------------------------------------------------------------

/// Dropping a window is not a close: Qt's `~QWindow` never emits `lastWindowClosed` or quits
/// (`qwindow.cpp:175-208`). Only the close path can end the application.
/// Not on macOS: creating a window inside `exec` on a test-harness thread traps with SIGTRAP on the
/// macOS runner (CI run 38174902623; presumably AppKit's main-thread requirement, not verified).
#[cfg(not(target_os = "macos"))]
#[test]
fn dropping_a_window_while_running_does_not_quit() {
    let _guard = TEST_MUTEX.lock().unwrap();
    Application::reset_for_test();
    let mut app = Application::new(vec!["drop_test".to_string()]);
    ferrokit_core::timer::Timer::single_shot(10, || {
        let w = Window::new("w", Rect::new(0, 0, 100, 100), WindowFlags::NORMAL).expect("window");
        drop(w);
        ferrokit_core::timer::Timer::single_shot(60, || Application::exit(7));
    });
    let code = app.exec();
    assert_eq!(
        code, 7,
        "dropping a window must not quit; only the later timer may end exec"
    );
}

#[cfg(windows)]
mod close_quit {
    use super::*;
    use windows_sys::Win32::UI::WindowsAndMessaging::{SendMessageW, WM_CLOSE};

    /// Qt default: closing the last visible window quits (`qwindow.cpp:2722-2735`,
    /// `qguiapplication.cpp:3933-3943`).
    #[test]
    fn closing_last_visible_window_quits_when_quit_on_last_window_closed_is_default() {
        let _guard = TEST_MUTEX.lock().unwrap();
        Application::reset_for_test();
        let mut app = Application::new(vec!["close_test".to_string()]);
        ferrokit_core::timer::Timer::single_shot(10, || {
            let mut win =
                Window::new("w", Rect::new(0, 0, 100, 100), WindowFlags::NORMAL).expect("window");
            win.show();
            // SAFETY: `win.native_handle()` is the `HWND` of `win`, a window created and shown
            // on this thread that is alive for the call (it is forgotten only afterwards).
            // `SendMessageW` takes integers only (`WM_CLOSE` ignores both parameters) and runs
            // `native_window_proc` synchronously on this thread, which reaches window state
            // through thread-local registries, not through a pointer into `win`.
            unsafe { SendMessageW(win.native_handle() as _, WM_CLOSE, 0, 0) };
            std::mem::forget(win); // keep it alive so only the close path can end exec
        });
        ferrokit_core::timer::Timer::single_shot(500, || Application::exit(9)); // watchdog
        let code = app.exec();
        assert_eq!(code, 0, "closing the last visible window quits");
    }

    /// With `setQuitOnLastWindowClosed(False)`: an accepted close keeps exec running.
    #[test]
    fn closing_last_window_with_quit_disabled_keeps_exec_running() {
        let _guard = TEST_MUTEX.lock().unwrap();
        Application::reset_for_test();
        let mut app = Application::new(vec!["close_test".to_string()]);
        ferrokit_gui::application::GuiApplication::set_quit_on_last_window_closed(false);
        ferrokit_core::timer::Timer::single_shot(10, || {
            let mut win =
                Window::new("w", Rect::new(0, 0, 100, 100), WindowFlags::NORMAL).expect("window");
            win.show();
            // SAFETY: as for the first `WM_CLOSE` test above: `win` is a live window of this
            // thread, the call takes integers only (`WM_CLOSE` ignores both parameters), and the
            // synchronous `native_window_proc` reaches window state through thread-local
            // registries, not through a pointer into `win`, which is only read again afterwards.
            unsafe { SendMessageW(win.native_handle() as _, WM_CLOSE, 0, 0) };
            assert!(!win.is_visible(), "accepted close hides the window");
            std::mem::forget(win);
            ferrokit_core::timer::Timer::single_shot(100, || Application::exit(5));
        });
        ferrokit_core::timer::Timer::single_shot(500, || Application::exit(9)); // watchdog
        let code = app.exec();
        ferrokit_gui::application::GuiApplication::set_quit_on_last_window_closed(true);
        assert_eq!(
            code, 5,
            "quit_on_last_window_closed(false) keeps exec alive after the close"
        );
    }

    /// Qt: `QApplication::event(Quit)` leaves the application running when a top-level window
    /// ignores its close (`qapplication.cpp:1675-1697`), so exec does not return.
    #[test]
    fn quit_refused_by_window_close_keeps_exec_running() {
        let _guard = TEST_MUTEX.lock().unwrap();
        Application::reset_for_test();
        let mut app = Application::new(vec!["quit_test".to_string()]);
        GuiApplication::set_quit_on_last_window_closed(false);
        let mut win =
            Window::new("w", Rect::new(0, 0, 100, 100), WindowFlags::NORMAL).expect("window");
        win.set_window_event_handler(|ev: &mut Event| {
            if matches!(ev.kind, EventKind::Close) {
                ev.ignore();
            }
        });
        win.show();
        ferrokit_core::timer::Timer::single_shot(10, Application::quit);
        ferrokit_core::timer::Timer::single_shot(500, || Application::exit(7)); // watchdog
        let code = app.exec();
        GuiApplication::set_quit_on_last_window_closed(true);
        assert_eq!(
            code, 7,
            "a window that refuses its close vetoes quit, so exec keeps running"
        );
        assert!(win.is_visible(), "the refusing window stays visible");
    }

    /// Qt: `QApplication::event(Quit)` calls `closeAllWindows()` before quitting
    /// (`qapplication.cpp:1675-1697`, `:1635-1639`), so an accepted close hides each open window.
    #[test]
    fn quit_closes_open_windows_before_exiting() {
        let _guard = TEST_MUTEX.lock().unwrap();
        Application::reset_for_test();
        let mut app = Application::new(vec!["quit_test".to_string()]);
        GuiApplication::set_quit_on_last_window_closed(false);
        let mut win =
            Window::new("w", Rect::new(0, 0, 100, 100), WindowFlags::NORMAL).expect("window");
        win.show();
        ferrokit_core::timer::Timer::single_shot(10, Application::quit);
        ferrokit_core::timer::Timer::single_shot(500, || Application::exit(9)); // watchdog
        let code = app.exec();
        GuiApplication::set_quit_on_last_window_closed(true);
        assert_eq!(code, 0, "quit exits normally");
        assert!(
            !win.is_visible(),
            "quit closes the open window before exiting"
        );
    }

    /// Qt: a posted Quit is `QApplication::event(Quit)`, which closes the windows and is vetoed
    /// by a refusing one (qapplication.cpp:1675-1697).
    #[test]
    fn posted_quit_is_vetoed_by_refusing_window() {
        let _guard = TEST_MUTEX.lock().unwrap();
        Application::reset_for_test();
        let mut app = Application::new(vec!["quit_test".to_string()]);
        GuiApplication::set_quit_on_last_window_closed(false);
        let mut win =
            Window::new("w", Rect::new(0, 0, 100, 100), WindowFlags::NORMAL).expect("window");
        win.set_window_event_handler(|ev: &mut Event| {
            if matches!(ev.kind, EventKind::Close) {
                ev.ignore();
            }
        });
        win.show();
        ferrokit_core::timer::Timer::single_shot(10, || {
            let app_id = CoreApplication::instance_id().expect("application object");
            CoreApplication::post_event(app_id, Event::new(EventKind::Quit { exit_code: 0 }));
        });
        ferrokit_core::timer::Timer::single_shot(500, || Application::exit(7)); // watchdog
        let code = app.exec();
        GuiApplication::set_quit_on_last_window_closed(true);
        assert_eq!(
            code, 7,
            "a posted Quit that a window refuses to close must not end exec"
        );
        assert!(win.is_visible(), "the refusing window stays visible");
    }

    /// Qt: `QCoreApplication::quit` from a worker thread posts Quit to the application
    /// (qcoreapplication.cpp:2156-2157), which runs the window veto on the application thread.
    #[test]
    fn quit_from_worker_thread_is_vetoed_by_refusing_window() {
        let _guard = TEST_MUTEX.lock().unwrap();
        Application::reset_for_test();
        let mut app = Application::new(vec!["quit_test".to_string()]);
        GuiApplication::set_quit_on_last_window_closed(false);
        let mut win =
            Window::new("w", Rect::new(0, 0, 100, 100), WindowFlags::NORMAL).expect("window");
        win.set_window_event_handler(|ev: &mut Event| {
            if matches!(ev.kind, EventKind::Close) {
                ev.ignore();
            }
        });
        win.show();
        ferrokit_core::timer::Timer::single_shot(10, || {
            std::thread::spawn(Application::quit)
                .join()
                .expect("worker thread");
        });
        ferrokit_core::timer::Timer::single_shot(500, || Application::exit(7)); // watchdog
        let code = app.exec();
        GuiApplication::set_quit_on_last_window_closed(true);
        assert_eq!(
            code, 7,
            "a quit from a worker thread must still be vetoed by the app-thread window"
        );
        assert!(win.is_visible(), "the refusing window stays visible");
    }
}
