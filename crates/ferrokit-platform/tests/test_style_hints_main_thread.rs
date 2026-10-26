// SPDX-License-Identifier: LGPL-3.0-only
//! `colorSchemeChanged` is emitted on the main thread, whichever thread noticed the change.
//!
//! On Windows the change arrives in the window procedure of the window that got
//! `WM_SETTINGCHANGE`; on Linux on the D-Bus listener thread. Qt delivers the theme change
//! through the window-system event queue to the main thread (`QGuiApplicationPrivate::
//! processThemeChanged`, qguiapplication.cpp:2838-2851). This is the only test of the binary, as
//! it needs the process's one application object to name the main thread.

use std::sync::{Arc, Mutex};
use std::thread::ThreadId;

use ferrokit_core::application::CoreApplication;
use ferrokit_gui::application::{ColorScheme, GuiApplication};
use ferrokit_platform::{install_style_hints_theme, GenericTheme, PlatformTheme};

#[test]
fn a_change_noticed_on_another_thread_is_emitted_on_the_main_thread() {
    // A `CoreApplication`: a `GuiApplication` runs the Cocoa event dispatcher on macOS, which
    // cannot pump events on a test thread (it is not the process's main thread).
    let _app = CoreApplication::new(Vec::new());
    let main = std::thread::current().id();

    let theme = Arc::new(GenericTheme::new(ColorScheme::Light));
    install_style_hints_theme(Arc::clone(&theme) as Arc<dyn PlatformTheme>);

    let seen: Arc<Mutex<Vec<(ColorScheme, ThreadId, ColorScheme)>>> = Arc::default();
    let sink = Arc::clone(&seen);
    GuiApplication::style_hints()
        .color_scheme_changed()
        .connect(move |scheme| {
            sink.lock().unwrap().push((
                *scheme,
                std::thread::current().id(),
                GuiApplication::style_hints().color_scheme(),
            ));
        });

    // The operating system's notification arrives on a worker thread.
    let worker_theme = Arc::clone(&theme);
    let worker = std::thread::spawn(move || {
        worker_theme.set_color_scheme(ColorScheme::Dark);
        std::thread::current().id()
    });
    let worker = worker.join().unwrap();
    assert_ne!(worker, main);

    // The theme has changed, but the application has not been told: that waits for the main
    // thread's event loop.
    assert_eq!(theme.color_scheme(), ColorScheme::Dark);
    assert!(seen.lock().unwrap().is_empty());

    CoreApplication::process_events(false);
    assert_eq!(
        *seen.lock().unwrap(),
        [(ColorScheme::Dark, main, ColorScheme::Dark)],
        "emitted once, on the main thread, with the new scheme already readable"
    );

    // Raised on the main thread itself the change is queued as well, so that a slot never runs
    // inside the platform code that noticed it (a window procedure, an AppKit callback).
    theme.set_color_scheme(ColorScheme::Light);
    assert_eq!(seen.lock().unwrap().len(), 1);
    CoreApplication::process_events(false);
    assert_eq!(seen.lock().unwrap().len(), 2);
    assert_eq!(
        seen.lock().unwrap()[1],
        (ColorScheme::Light, main, ColorScheme::Light)
    );

    // `set_color_scheme` is in place on the main thread: the new scheme is readable at once.
    GuiApplication::style_hints().set_color_scheme(ColorScheme::Dark);
    assert_eq!(
        GuiApplication::style_hints().color_scheme(),
        ColorScheme::Dark
    );
    assert_eq!(seen.lock().unwrap().len(), 3);
}
