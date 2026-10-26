// SPDX-License-Identifier: LGPL-3.0-only
//! The colour scheme of `QStyleHints`: `colorScheme`, `setColorScheme` and `colorSchemeChanged`.
//!
//! Qt (6.11, `qtbase` tag `v6.11.2`) keeps the effective scheme in `QStyleHintsPrivate::m_colorScheme`.
//! Only the platform theme decides it: `QStyleHints::setColorScheme` does not store anything, it
//! asks the platform theme (`QPlatformTheme::requestColorScheme`, qstylehints.cpp:170-178), and the
//! theme reports back through `QGuiApplicationPrivate::handleThemeChanged`
//! (qguiapplication.cpp:2854-2856), which calls `QStyleHintsPrivate::update(theme)`
//! (qstylehints.cpp:736-741) and so `updateColorScheme(theme->colorScheme())`. That function sets
//! the cached scheme and emits `colorSchemeChanged` only when the scheme differs
//! (qstylehints.cpp:722-729).
//!
//! ferrokit mirrors that split. This crate owns the cached scheme, the signal and the
//! override-request entry point; the platform layer (`ferrokit-platform`, which this crate cannot
//! depend on) installs a [`ColorSchemeSource`] standing for `QPlatformTheme` and calls
//! [`handle_theme_changed`] when the operating system's scheme moves.
//!
//! When the change is announced is platform dependent too: see [`handle_theme_changed`].
//!
//! What overriding means is platform-engine dependent, as in Qt: the Windows, GNOME and generic
//! themes keep the request next to the system value, and the effective scheme is the request
//! unless it is [`ColorScheme::Unknown`] (`QWindowsTheme::effectiveColorScheme`,
//! qwindowstheme.cpp:655-667; `QGnomeThemePrivate::colorScheme`, qgnometheme.cpp:56-78); the
//! Cocoa theme sets `NSApp.appearance` (`QCocoaTheme::requestColorScheme`,
//! qcocoatheme.mm:478-496); a theme that cannot override ignores the request, as the base
//! `QPlatformTheme` does (qplatformtheme.cpp:480-487).

use std::sync::{Arc, Mutex, MutexGuard};

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::event_loop::post_event_to_thread;
use ferrokit_core::object::{ObjectId, ThreadContext, ThreadId};
use ferrokit_core::signal::Signal;

/// `Qt::ColorScheme`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum ColorScheme {
    /// The platform does not know, or (as a request) "follow the system".
    #[default]
    Unknown = 0,
    Light = 1,
    Dark = 2,
}

impl From<u8> for ColorScheme {
    fn from(val: u8) -> Self {
        match val {
            1 => ColorScheme::Light,
            2 => ColorScheme::Dark,
            _ => ColorScheme::Unknown,
        }
    }
}

/// The part of `QPlatformTheme` that `QStyleHints` talks to. Installed by the platform layer with
/// [`set_color_scheme_source`].
pub trait ColorSchemeSource: Send + Sync {
    /// `QPlatformTheme::colorScheme`: the effective scheme, an application override included.
    fn color_scheme(&self) -> ColorScheme;

    /// `QPlatformTheme::requestColorScheme`: override the system scheme with `scheme`, or go back
    /// to following the system when it is [`ColorScheme::Unknown`]. The source reports the
    /// resulting change through [`handle_theme_changed`], the way a platform theme does through
    /// `QWindowSystemInterface::handleThemeChange`.
    fn request_color_scheme(&self, scheme: ColorScheme);
}

struct State {
    /// `QStyleHintsPrivate::m_colorScheme`.
    scheme: ColorScheme,
    source: Option<Arc<dyn ColorSchemeSource>>,
    changed: Option<Signal<ColorScheme>>,
}

static STATE: Mutex<State> = Mutex::new(State {
    scheme: ColorScheme::Unknown,
    source: None,
    changed: None,
});

fn state() -> MutexGuard<'static, State> {
    // No slot runs while this lock is held, so a poisoned lock holds consistent data.
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

fn changed_signal() -> Signal<ColorScheme> {
    state().changed.get_or_insert_with(Signal::new).clone()
}

/// Runs `f` on the application's main thread: in place when the caller is that thread, or when no
/// application has named a main thread yet, and otherwise through the main thread's event queue.
fn run_on_main_thread(f: impl FnOnce() + Send + 'static) {
    match ThreadContext::main_thread_id() {
        Some(main) if main != ThreadId::current() => post_event_to_thread(
            main,
            ObjectId(0),
            Event::new(EventKind::MetaCallNoTarget(Box::new(f))),
        ),
        _ => f(),
    }
}

/// `QStyleHintsPrivate::updateColorScheme` (qstylehints.cpp:722-729): caches `scheme` and emits
/// `colorSchemeChanged` when it differs. The signal is emitted with no lock held.
fn update_color_scheme(scheme: ColorScheme) {
    let signal = {
        let mut state = state();
        if state.scheme == scheme {
            return;
        }
        state.scheme = scheme;
        state.changed.get_or_insert_with(Signal::new).clone()
    };
    signal.emit(&scheme);
}

/// `QStyleHintsPrivate::update` (qstylehints.cpp:736-741), the colour-scheme half: reads the
/// source's current scheme. Must run on the main thread. `GuiApplication::new` runs it too, as
/// `QGuiApplication`'s constructor does after the platform plugin is up
/// (qguiapplication.cpp:1617).
pub(super) fn update_from_source() {
    let source = state().source.clone();
    if let Some(source) = source {
        update_color_scheme(source.color_scheme());
    }
}

/// Installs the platform theme `QStyleHints` reads and overrides through, and takes over its
/// current scheme (`QGuiApplicationPrivate` does the same after the platform plugin is up,
/// qguiapplication.cpp:1617). Called by the platform layer; a later call replaces the source.
pub fn set_color_scheme_source(source: Arc<dyn ColorSchemeSource>) {
    state().source = Some(source);
    run_on_main_thread(update_from_source);
}

/// `QGuiApplicationPrivate::handleThemeChanged` (qguiapplication.cpp:2854-2856), for the colour
/// scheme: the platform theme tells that something changed. The scheme is read from the source on
/// the main thread, so `colorSchemeChanged` is emitted there whichever thread detected the change
/// (a D-Bus listener thread, the window procedure of a window on another thread).
///
/// With an application object the notification is queued even when it is raised on the main
/// thread, as Qt delivers it through the window-system event queue
/// (`QWindowSystemInterface::handleThemeChange`, asynchronous by default; Qt's Windows plugin
/// delivers it synchronously from its window procedure). So a slot never runs inside the platform
/// code that noticed the change, for instance inside a window procedure. Without an application
/// (no main thread named yet) the change is applied in place.
pub fn handle_theme_changed() {
    match ThreadContext::main_thread_id() {
        Some(main) => post_event_to_thread(
            main,
            ObjectId(0),
            Event::new(EventKind::MetaCallNoTarget(Box::new(update_from_source))),
        ),
        None => update_from_source(),
    }
}

/// Forgets the signal's connections and re-reads the scheme from the installed source (the source
/// itself belongs to the platform layer and stays installed).
pub(crate) fn reset_for_test() {
    let source = {
        let mut state = state();
        state.changed = None;
        state.scheme = ColorScheme::Unknown;
        state.source.clone()
    };
    if let Some(source) = source {
        state().scheme = source.color_scheme();
    }
}

impl super::StyleHints {
    /// `QStyleHints::colorScheme` (qstylehints.cpp:158-162): the scheme the application uses. It
    /// follows the system scheme until [`set_color_scheme`](Self::set_color_scheme) overrides it.
    /// [`ColorScheme::Unknown`] when the platform does not know (or before the platform layer has
    /// installed its theme: it does so when the first platform object is created, which
    /// `ferrokit_widgets::application::Application::new` does).
    ///
    /// Every `StyleHints` value reads the one application-wide scheme, as `QGuiApplication::styleHints()`
    /// is one object.
    pub fn color_scheme(&self) -> ColorScheme {
        state().scheme
    }

    /// `QStyleHints::setColorScheme` (Qt 6.8+, qstylehints.cpp:170-178): overrides the system
    /// colour scheme with `scheme`, or [`ColorScheme::Unknown`] to follow the system again. The
    /// request goes to the platform theme, so whether it takes effect is platform dependent;
    /// [`color_scheme`](Self::color_scheme) afterwards is the effective scheme (the system's own
    /// when the platform ignores the request), and `color_scheme_changed` fires only if that
    /// differs from before. Without an installed platform theme this warns and does nothing,
    /// like Qt without an application object.
    ///
    /// Call it on the main thread; from another thread the request is forwarded there.
    pub fn set_color_scheme(&self, scheme: ColorScheme) {
        let source = state().source.clone();
        let Some(source) = source else {
            eprintln!("Must construct a QGuiApplication before accessing a platform theme hint.");
            return;
        };
        // In place on the main thread: the scheme in effect is readable as soon as this returns,
        // as with Qt's Windows and Cocoa themes (their `requestColorScheme` delivers the theme
        // change synchronously). Qt's GNOME theme delivers it from the event queue instead.
        run_on_main_thread(move || {
            source.request_color_scheme(scheme);
            update_from_source();
        });
    }

    /// `QStyleHints::unsetColorScheme` (Qt 6.8+): back to the system's current scheme.
    pub fn unset_color_scheme(&self) {
        self.set_color_scheme(ColorScheme::Unknown);
    }

    /// `QStyleHints::colorSchemeChanged`: emitted on the main thread with the new effective
    /// scheme whenever it differs from the previous one, whether the system or
    /// [`set_color_scheme`](Self::set_color_scheme) moved it. A change the system makes is
    /// announced through the main thread's event queue (see [`handle_theme_changed`]), so it
    /// needs a running event loop (or `CoreApplication::process_events`). The returned handle shares its
    /// connections with every other one, so connecting through it connects the application's
    /// signal. A slot may read [`color_scheme`](Self::color_scheme): it already has the new value.
    pub fn color_scheme_changed(&self) -> Signal<ColorScheme> {
        changed_signal()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::GuiApplication;
    use ferrokit_core::application::CoreApplication;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A platform theme that can override, like the Windows and GNOME ones.
    struct MockTheme {
        schemes: Mutex<(ColorScheme, ColorScheme)>,
        can_override: bool,
    }

    impl MockTheme {
        fn new(system: ColorScheme, can_override: bool) -> Arc<Self> {
            Arc::new(Self {
                schemes: Mutex::new((system, ColorScheme::Unknown)),
                can_override,
            })
        }

        /// The operating system moves its scheme, and tells the application.
        fn system_changes(&self, scheme: ColorScheme) {
            self.schemes.lock().unwrap().0 = scheme;
            handle_theme_changed();
            // The notification is queued: deliver it.
            CoreApplication::process_events(false);
        }
    }

    impl ColorSchemeSource for MockTheme {
        fn color_scheme(&self) -> ColorScheme {
            let (system, requested) = *self.schemes.lock().unwrap();
            if requested != ColorScheme::Unknown {
                requested
            } else {
                system
            }
        }

        fn request_color_scheme(&self, scheme: ColorScheme) {
            if self.can_override {
                self.schemes.lock().unwrap().1 = scheme;
            }
        }
    }

    /// Takes the test lock, makes this thread the main thread and starts from `theme`.
    fn start(theme: &Arc<MockTheme>) -> (MutexGuard<'static, ()>, GuiApplication) {
        // The tests share one application-wide scheme, one source and one main thread.
        let guard = crate::application::APPLICATION_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        GuiApplication::reset_for_test();
        let app = GuiApplication::new(Vec::new());
        set_color_scheme_source(Arc::clone(theme) as Arc<dyn ColorSchemeSource>);
        (guard, app)
    }

    fn record(hints: &super::super::StyleHints) -> Arc<Mutex<Vec<ColorScheme>>> {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        hints
            .color_scheme_changed()
            .connect(move |scheme| sink.lock().unwrap().push(*scheme));
        seen
    }

    #[test]
    fn the_scheme_is_the_platform_themes_once_installed() {
        let theme = MockTheme::new(ColorScheme::Dark, true);
        let (_guard, _app) = start(&theme);
        assert_eq!(
            GuiApplication::style_hints().color_scheme(),
            ColorScheme::Dark
        );
    }

    #[test]
    fn the_signal_fires_on_an_effective_change_only() {
        let theme = MockTheme::new(ColorScheme::Light, true);
        let (_guard, _app) = start(&theme);
        let hints = GuiApplication::style_hints();
        let seen = record(&hints);

        // The platform says "changed" but the scheme is the same: no signal.
        handle_theme_changed();
        theme.system_changes(ColorScheme::Light);
        assert!(seen.lock().unwrap().is_empty());

        theme.system_changes(ColorScheme::Dark);
        theme.system_changes(ColorScheme::Dark);
        theme.system_changes(ColorScheme::Light);
        assert_eq!(
            *seen.lock().unwrap(),
            [ColorScheme::Dark, ColorScheme::Light]
        );
        assert_eq!(hints.color_scheme(), ColorScheme::Light);
    }

    #[test]
    fn an_override_wins_over_the_system_until_unset() {
        let theme = MockTheme::new(ColorScheme::Light, true);
        let (_guard, _app) = start(&theme);
        let hints = GuiApplication::style_hints();
        let seen = record(&hints);

        hints.set_color_scheme(ColorScheme::Dark);
        assert_eq!(hints.color_scheme(), ColorScheme::Dark);
        // Overriding with the scheme already in effect changes nothing.
        hints.set_color_scheme(ColorScheme::Dark);
        // System changes are ignored while the override is set.
        theme.system_changes(ColorScheme::Dark);
        theme.system_changes(ColorScheme::Light);
        assert_eq!(hints.color_scheme(), ColorScheme::Dark);
        assert_eq!(*seen.lock().unwrap(), [ColorScheme::Dark]);

        // Unknown removes the override: back to the system's current scheme.
        hints.set_color_scheme(ColorScheme::Unknown);
        assert_eq!(hints.color_scheme(), ColorScheme::Light);
        assert_eq!(
            *seen.lock().unwrap(),
            [ColorScheme::Dark, ColorScheme::Light]
        );

        hints.set_color_scheme(ColorScheme::Light);
        hints.unset_color_scheme();
        assert_eq!(hints.color_scheme(), ColorScheme::Light);
        assert_eq!(
            seen.lock().unwrap().len(),
            2,
            "Light over Light is no change"
        );
    }

    #[test]
    fn a_platform_that_cannot_override_keeps_its_scheme() {
        let theme = MockTheme::new(ColorScheme::Light, false);
        let (_guard, _app) = start(&theme);
        let hints = GuiApplication::style_hints();
        let seen = record(&hints);
        hints.set_color_scheme(ColorScheme::Dark);
        assert_eq!(hints.color_scheme(), ColorScheme::Light);
        assert!(seen.lock().unwrap().is_empty());
    }

    #[test]
    fn a_slot_sees_the_new_scheme_and_may_override_again() {
        let theme = MockTheme::new(ColorScheme::Light, true);
        let (_guard, _app) = start(&theme);
        let hints = GuiApplication::style_hints();
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = Arc::new(Mutex::new(Vec::new()));
        let (count, sink) = (Arc::clone(&calls), Arc::clone(&observed));
        hints.color_scheme_changed().connect(move |scheme| {
            let hints = GuiApplication::style_hints();
            sink.lock().unwrap().push((*scheme, hints.color_scheme()));
            // Re-entering (a lock held across the emission would deadlock here).
            if count.fetch_add(1, Ordering::SeqCst) == 0 {
                hints.set_color_scheme(ColorScheme::Dark);
            }
        });
        theme.system_changes(ColorScheme::Unknown);
        assert_eq!(
            *observed.lock().unwrap(),
            [
                (ColorScheme::Unknown, ColorScheme::Unknown),
                (ColorScheme::Dark, ColorScheme::Dark)
            ]
        );
    }

    #[test]
    fn without_a_platform_theme_a_request_is_refused() {
        let theme = MockTheme::new(ColorScheme::Light, true);
        let (_guard, _app) = start(&theme);
        state().source = None;
        let hints = GuiApplication::style_hints();
        let seen = record(&hints);
        hints.set_color_scheme(ColorScheme::Dark);
        assert_eq!(hints.color_scheme(), ColorScheme::Light);
        assert!(seen.lock().unwrap().is_empty());
    }

    #[test]
    fn color_scheme_converts_from_its_numeric_value() {
        assert_eq!(ColorScheme::from(0), ColorScheme::Unknown);
        assert_eq!(ColorScheme::from(1), ColorScheme::Light);
        assert_eq!(ColorScheme::from(2), ColorScheme::Dark);
        assert_eq!(ColorScheme::from(7), ColorScheme::Unknown);
        assert_eq!(ColorScheme::default(), ColorScheme::Unknown);
    }
}
