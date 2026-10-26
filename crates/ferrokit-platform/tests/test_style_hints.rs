// SPDX-License-Identifier: LGPL-3.0-only
//! `GuiApplication::style_hints().color_scheme()` / `set_color_scheme` / `color_scheme_changed`
//! driven by a mock platform theme (`GenericTheme`): which scheme is effective, when the signal
//! fires, and what an override does. Qt: `QStyleHints::colorScheme`, `setColorScheme`,
//! `colorSchemeChanged` (qstylehints.cpp:158-178, 722-741).
//!
//! Every test makes its own application object (the thread it runs on is the main thread) and
//! delivers the queued theme notifications with `process_events`. It is a `CoreApplication`: a
//! `GuiApplication` runs the Cocoa event dispatcher on macOS, which cannot pump events on a test
//! thread (it is not the process's main thread). The thread hop is in
//! `test_style_hints_main_thread`.

use std::sync::{Arc, Mutex};

use ferrokit_core::application::CoreApplication;
use ferrokit_core::signal::Signal;
use ferrokit_gui::application::{ColorScheme, GuiApplication};
use ferrokit_platform::{
    install_style_hints_theme, set_platform_integration, GenericPlatformIntegration,
    PlatformIntegration, PlatformTheme,
};

/// The tests share the application-wide style hints.
static LOCK: Mutex<()> = Mutex::new(());

struct Fixture {
    _guard: std::sync::MutexGuard<'static, ()>,
    _app: CoreApplication,
}

fn start() -> Fixture {
    let guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    GuiApplication::reset_for_test();
    Fixture {
        _guard: guard,
        _app: CoreApplication::new(Vec::new()),
    }
}

/// Delivers the notifications the platform theme queued for the main thread.
fn pump() {
    CoreApplication::process_events(false);
}

fn record() -> Arc<Mutex<Vec<ColorScheme>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    GuiApplication::style_hints()
        .color_scheme_changed()
        .connect(move |scheme| sink.lock().unwrap().push(*scheme));
    seen
}

/// Installs a mock theme (the initial scheme is read at once, on this, the main, thread).
fn install(initial: ColorScheme) -> Arc<ferrokit_platform::GenericTheme> {
    let theme = Arc::new(ferrokit_platform::GenericTheme::new(initial));
    install_style_hints_theme(Arc::clone(&theme) as Arc<dyn PlatformTheme>);
    theme
}

#[test]
fn installing_a_theme_hands_its_scheme_to_the_style_hints() {
    let _f = start();
    let _theme = install(ColorScheme::Dark);
    assert_eq!(
        GuiApplication::style_hints().color_scheme(),
        ColorScheme::Dark
    );
}

#[test]
fn a_system_change_emits_once_with_the_new_scheme() {
    let _f = start();
    let theme = install(ColorScheme::Light);
    let seen = record();

    theme.set_color_scheme(ColorScheme::Dark);
    pump();
    assert_eq!(*seen.lock().unwrap(), [ColorScheme::Dark]);
    assert_eq!(
        GuiApplication::style_hints().color_scheme(),
        ColorScheme::Dark
    );

    // The same scheme again, and a refresh that finds nothing new, are not changes.
    theme.set_color_scheme(ColorScheme::Dark);
    theme.refresh();
    pump();
    assert_eq!(seen.lock().unwrap().len(), 1);

    theme.set_color_scheme(ColorScheme::Light);
    pump();
    assert_eq!(
        *seen.lock().unwrap(),
        [ColorScheme::Dark, ColorScheme::Light]
    );
}

#[test]
fn a_burst_of_changes_that_ends_where_it_began_emits_nothing() {
    let _f = start();
    let theme = install(ColorScheme::Light);
    let seen = record();
    theme.set_color_scheme(ColorScheme::Dark);
    theme.set_color_scheme(ColorScheme::Light);
    pump();
    assert!(
        seen.lock().unwrap().is_empty(),
        "the notifications read the theme when they run, and it is Light again"
    );
}

#[test]
fn set_color_scheme_overrides_and_unknown_follows_the_system_again() {
    let _f = start();
    let theme = install(ColorScheme::Light);
    let hints = GuiApplication::style_hints();
    let seen = record();

    hints.set_color_scheme(ColorScheme::Dark);
    assert_eq!(hints.color_scheme(), ColorScheme::Dark);
    assert_eq!(theme.color_scheme(), ColorScheme::Dark);

    // While overridden, the system moving is invisible to the application.
    theme.set_color_scheme(ColorScheme::Dark);
    theme.set_color_scheme(ColorScheme::Light);
    pump();
    assert_eq!(hints.color_scheme(), ColorScheme::Dark);
    assert_eq!(*seen.lock().unwrap(), [ColorScheme::Dark]);

    // Qt 6.8: unsetting (or setting Unknown) returns to the system's *current* scheme.
    theme.set_color_scheme(ColorScheme::Light);
    hints.unset_color_scheme();
    assert_eq!(hints.color_scheme(), ColorScheme::Light);
    assert_eq!(
        *seen.lock().unwrap(),
        [ColorScheme::Dark, ColorScheme::Light]
    );
}

#[test]
fn replacing_the_theme_stops_listening_to_the_old_one() {
    let _f = start();
    let old = install(ColorScheme::Light);
    let new = install(ColorScheme::Light);
    let seen = record();

    old.set_color_scheme(ColorScheme::Dark);
    pump();
    assert!(seen.lock().unwrap().is_empty());
    assert_eq!(
        GuiApplication::style_hints().color_scheme(),
        ColorScheme::Light
    );

    new.set_color_scheme(ColorScheme::Dark);
    pump();
    assert_eq!(*seen.lock().unwrap(), [ColorScheme::Dark]);
}

#[test]
fn a_theme_that_cannot_override_ignores_the_request() {
    struct Fixed(Signal<ColorScheme>);
    impl PlatformTheme for Fixed {
        fn color_scheme(&self) -> ColorScheme {
            ColorScheme::Light
        }
        fn theme_changed(&self) -> &Signal<ColorScheme> {
            &self.0
        }
        fn refresh(&self) {}
    }
    let _f = start();
    install_style_hints_theme(Arc::new(Fixed(Signal::new())));
    let seen = record();
    let hints = GuiApplication::style_hints();
    hints.set_color_scheme(ColorScheme::Dark);
    pump();
    assert_eq!(hints.color_scheme(), ColorScheme::Light);
    assert!(seen.lock().unwrap().is_empty());
}

#[test]
fn set_platform_integration_installs_that_integrations_theme() {
    /// The generic integration with a theme the test holds on to.
    struct Mocked {
        inner: GenericPlatformIntegration,
        theme: Arc<ferrokit_platform::GenericTheme>,
    }
    impl PlatformIntegration for Mocked {
        fn create_window(
            &self,
            title: &str,
            rect: ferrokit_gui::geometry::primitives::Rect,
            flags: ferrokit_platform::WindowFlags,
        ) -> Result<Box<dyn ferrokit_platform::PlatformWindow>, &'static str> {
            self.inner.create_window(title, rect, flags)
        }
        fn create_tray_icon(
            &self,
            tooltip: &str,
            pixmap: &ferrokit_gui::paint::Pixmap,
        ) -> Result<Box<dyn ferrokit_platform::PlatformTrayIcon>, &'static str> {
            self.inner.create_tray_icon(tooltip, pixmap)
        }
        fn primary_screen(&self) -> Box<dyn ferrokit_platform::PlatformScreen> {
            self.inner.primary_screen()
        }
        fn screens(&self) -> Vec<Box<dyn ferrokit_platform::PlatformScreen>> {
            self.inner.screens()
        }
        fn screen_at(
            &self,
            pos: ferrokit_gui::geometry::primitives::Point,
        ) -> Option<Box<dyn ferrokit_platform::PlatformScreen>> {
            self.inner.screen_at(pos)
        }
        fn screen_changed(&self) -> &Signal<()> {
            self.inner.screen_changed()
        }
        fn theme(&self) -> Arc<dyn PlatformTheme> {
            Arc::clone(&self.theme) as Arc<dyn PlatformTheme>
        }
        fn clipboard(&self) -> Box<dyn ferrokit_platform::PlatformClipboard> {
            self.inner.clipboard()
        }
        fn create_hotkey_manager(
            &self,
        ) -> Result<Box<dyn ferrokit_platform::PlatformHotkeyManager>, &'static str> {
            self.inner.create_hotkey_manager()
        }
        fn cursor(&self) -> Box<dyn ferrokit_platform::PlatformCursor> {
            self.inner.cursor()
        }
    }

    let _f = start();
    let theme = Arc::new(ferrokit_platform::GenericTheme::new(ColorScheme::Dark));
    set_platform_integration(Arc::new(Mocked {
        inner: GenericPlatformIntegration::default(),
        theme: Arc::clone(&theme),
    }));
    let seen = record();
    assert_eq!(
        GuiApplication::style_hints().color_scheme(),
        ColorScheme::Dark
    );

    theme.set_color_scheme(ColorScheme::Light);
    pump();
    assert_eq!(*seen.lock().unwrap(), [ColorScheme::Light]);
    assert_eq!(
        GuiApplication::style_hints().color_scheme(),
        ColorScheme::Light
    );
}
