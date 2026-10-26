// SPDX-License-Identifier: LGPL-3.0-only
//! `GuiApplication::style_hints().color_scheme()` / `set_color_scheme` / `color_scheme_changed`
//! driven by a mock platform theme (`GenericTheme`): which scheme is effective, when the signal
//! fires, and what an override does. Qt: `QStyleHints::colorScheme`, `setColorScheme`,
//! `colorSchemeChanged` (qstylehints.cpp:158-178, 722-741).
//!
//! No application object is created here: without one the "main thread" is whichever thread
//! emits, so every emission is synchronous. The thread hop is in `test_style_hints_main_thread`.

use std::sync::{Arc, Mutex};

use ferrokit_core::signal::Signal;
use ferrokit_gui::application::{ColorScheme, GuiApplication};
use ferrokit_platform::{install_style_hints_theme, GenericTheme, PlatformTheme};

/// The tests share the application-wide style hints.
static LOCK: Mutex<()> = Mutex::new(());

fn lock() -> std::sync::MutexGuard<'static, ()> {
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn record() -> Arc<Mutex<Vec<ColorScheme>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    GuiApplication::style_hints()
        .color_scheme_changed()
        .connect(move |scheme| sink.lock().unwrap().push(*scheme));
    seen
}

/// Installs a fresh mock theme and forgets earlier connections.
fn fresh(initial: ColorScheme) -> Arc<GenericTheme> {
    GuiApplication::reset_for_test();
    let theme = Arc::new(GenericTheme::new(initial));
    install_style_hints_theme(Arc::clone(&theme) as Arc<dyn PlatformTheme>);
    theme
}

#[test]
fn installing_a_theme_hands_its_scheme_to_the_style_hints() {
    let _guard = lock();
    let _theme = fresh(ColorScheme::Dark);
    assert_eq!(
        GuiApplication::style_hints().color_scheme(),
        ColorScheme::Dark
    );
}

#[test]
fn a_system_change_emits_once_with_the_new_scheme() {
    let _guard = lock();
    let theme = fresh(ColorScheme::Light);
    let seen = record();

    theme.set_color_scheme(ColorScheme::Dark);
    assert_eq!(*seen.lock().unwrap(), [ColorScheme::Dark]);
    assert_eq!(
        GuiApplication::style_hints().color_scheme(),
        ColorScheme::Dark
    );

    // The same scheme again, and a refresh that finds nothing new, are not changes.
    theme.set_color_scheme(ColorScheme::Dark);
    theme.refresh();
    assert_eq!(seen.lock().unwrap().len(), 1);

    theme.set_color_scheme(ColorScheme::Light);
    assert_eq!(
        *seen.lock().unwrap(),
        [ColorScheme::Dark, ColorScheme::Light]
    );
}

#[test]
fn set_color_scheme_overrides_and_unknown_follows_the_system_again() {
    let _guard = lock();
    let theme = fresh(ColorScheme::Light);
    let hints = GuiApplication::style_hints();
    let seen = record();

    hints.set_color_scheme(ColorScheme::Dark);
    assert_eq!(hints.color_scheme(), ColorScheme::Dark);
    assert_eq!(theme.color_scheme(), ColorScheme::Dark);

    // While overridden, the system moving is invisible to the application.
    theme.set_color_scheme(ColorScheme::Dark);
    theme.set_color_scheme(ColorScheme::Light);
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
    let _guard = lock();
    let old = fresh(ColorScheme::Light);
    let new = fresh(ColorScheme::Light);
    let seen = record();

    old.set_color_scheme(ColorScheme::Dark);
    assert!(seen.lock().unwrap().is_empty());
    assert_eq!(
        GuiApplication::style_hints().color_scheme(),
        ColorScheme::Light
    );

    new.set_color_scheme(ColorScheme::Dark);
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
    let _guard = lock();
    GuiApplication::reset_for_test();
    install_style_hints_theme(Arc::new(Fixed(Signal::new())));
    let seen = record();
    let hints = GuiApplication::style_hints();
    hints.set_color_scheme(ColorScheme::Dark);
    assert_eq!(hints.color_scheme(), ColorScheme::Light);
    assert!(seen.lock().unwrap().is_empty());
}

#[test]
fn the_default_platform_feeds_the_style_hints() {
    let _guard = lock();
    GuiApplication::reset_for_test();
    // `platform()` is the integration of this OS (or whatever an earlier test of this binary set).
    let theme = ferrokit_platform::platform().theme();
    assert_eq!(
        GuiApplication::style_hints().color_scheme(),
        theme.color_scheme()
    );
}
