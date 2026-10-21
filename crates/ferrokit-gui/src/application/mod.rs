// SPDX-License-Identifier: LGPL-3.0-only
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::RwLock;

use ferrokit_core::application::CoreApplication;
use ferrokit_core::event_loop::DispatcherKind;
use ferrokit_core::meta::MetaObject;
use ferrokit_core::object::{ObjectData, ObjectId, QObject};
use ferrokit_core::signal::Signal;

use crate::paint::palette::Palette;
use crate::text::font::Font;

/// Application lifecycle and visibility states matching `Qt::ApplicationState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationState {
    ApplicationSuspended,
    ApplicationHidden,
    ApplicationInactive,
    ApplicationActive,
}

/// Layout directions matching `Qt::LayoutDirection`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutDirection {
    LeftToRight,
    RightToLeft,
    LayoutDirectionAuto,
}

/// High-DPI scale factor rounding policies matching `Qt::HighDpiScaleFactorRoundingPolicy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HighDpiScaleFactorRoundingPolicy {
    Round,
    Ceil,
    Floor,
    RoundPreferFloor,
    PassThrough,
}

/// Applies the global `HighDpiScaleFactorRoundingPolicy` to a raw screen scale factor and
/// clamps the result to at least 1, mirroring `QHighDpiScaling::roundScaleFactor`
/// (`qtbase/src/gui/kernel/qhighdpiscaling.cpp:322-361`).
pub fn round_scale_factor(raw: f64) -> f64 {
    let rounded = match GuiApplication::high_dpi_scale_factor_rounding_policy() {
        HighDpiScaleFactorRoundingPolicy::Round => raw.round(),
        HighDpiScaleFactorRoundingPolicy::Ceil => raw.ceil(),
        HighDpiScaleFactorRoundingPolicy::Floor => raw.floor(),
        HighDpiScaleFactorRoundingPolicy::RoundPreferFloor => {
            if raw - raw.floor() < 0.75 {
                raw.floor()
            } else {
                raw.ceil()
            }
        }
        HighDpiScaleFactorRoundingPolicy::PassThrough => raw,
    };
    rounded.max(1.0)
}

/// Global system style hints matching `QStyleHints`.
#[derive(Debug, Clone)]
pub struct StyleHints {
    pub cursor_flash_time_ms: u32,
    pub keyboard_input_interval_ms: u32,
    pub mouse_double_click_interval_ms: u32,
    pub mouse_double_click_distance: i32,
    pub mouse_press_and_hold_interval_ms: u32,
    pub start_drag_distance: i32,
    pub start_drag_time_ms: u32,
    pub wheel_scroll_lines: i32,
    pub show_is_maximized: bool,
    pub show_shortcuts_in_context_menus: bool,
}

impl Default for StyleHints {
    fn default() -> Self {
        Self {
            cursor_flash_time_ms: 1000,
            keyboard_input_interval_ms: 400,
            mouse_double_click_interval_ms: 400,
            mouse_double_click_distance: 5,
            mouse_press_and_hold_interval_ms: 800,
            start_drag_distance: 10,
            start_drag_time_ms: 500,
            wheel_scroll_lines: 3,
            show_is_maximized: false,
            show_shortcuts_in_context_menus: true,
        }
    }
}

static GLOBAL_PALETTE: RwLock<Option<Palette>> = RwLock::new(None);
static GLOBAL_FONT: RwLock<Option<Font>> = RwLock::new(None);
static GLOBAL_STYLE_HINTS: RwLock<Option<StyleHints>> = RwLock::new(None);
static GLOBAL_LAYOUT_DIRECTION: RwLock<LayoutDirection> = RwLock::new(LayoutDirection::LeftToRight);
static GLOBAL_APP_STATE: RwLock<ApplicationState> =
    RwLock::new(ApplicationState::ApplicationActive);
static GLOBAL_DISPLAY_NAME: RwLock<Option<String>> = RwLock::new(None);
static GLOBAL_DESKTOP_FILE_NAME: RwLock<Option<String>> = RwLock::new(None);
static GLOBAL_QUIT_ON_LAST_WINDOW_CLOSED: AtomicBool = AtomicBool::new(true);
static GLOBAL_ROUNDING_POLICY: RwLock<HighDpiScaleFactorRoundingPolicy> =
    RwLock::new(HighDpiScaleFactorRoundingPolicy::PassThrough);
static LAST_WINDOW_CLOSED_SIGNAL: RwLock<Option<Signal<()>>> = RwLock::new(None);
static FOCUS_WINDOW_CHANGED_SIGNAL: RwLock<Option<Signal<Option<ObjectId>>>> = RwLock::new(None);
/// GUI-level application class equivalent to Qt's `QGuiApplication`.
///
/// Builds upon `CoreApplication` and adds:
/// - Global Application Palette (`QGuiApplication::palette`)
/// - Global Application Font (`QGuiApplication::font`)
/// - Global Style Hints (`QGuiApplication::styleHints`)
/// - Application State & Lifecycle Tracking (`applicationState`)
/// - Layout Direction & High-DPI Rounding Policies
/// - Window lifecycle notifications (`lastWindowClosed`)
pub struct GuiApplication {
    core_app: CoreApplication,
    application_state_changed: Signal<ApplicationState>,
    last_window_closed: Signal<()>,
    focus_window_changed: Signal<Option<ObjectId>>,
}

impl GuiApplication {
    /// Initializes a new `GuiApplication` instance.
    pub fn new(args: Vec<String>) -> Self {
        // `QGuiApplication` takes its event dispatcher from the platform plugin
        // (qguiapplication.cpp:1629-1643): the GUI path, chosen when the loop is created.
        let core_app = CoreApplication::with_dispatcher_kind(args, DispatcherKind::Gui);

        // Ensure default palette and font are established
        {
            let mut pal_guard = GLOBAL_PALETTE.write().unwrap();
            if pal_guard.is_none() {
                *pal_guard = Some(Palette::dark());
            }
        }
        {
            let mut font_guard = GLOBAL_FONT.write().unwrap();
            if font_guard.is_none() {
                *font_guard = Some(Font::default());
            }
        }
        {
            let mut hints_guard = GLOBAL_STYLE_HINTS.write().unwrap();
            if hints_guard.is_none() {
                *hints_guard = Some(StyleHints::default());
            }
        }

        let last_window_closed = Signal::new();
        *LAST_WINDOW_CLOSED_SIGNAL.write().unwrap() = Some(last_window_closed.clone());
        let focus_window_changed = Signal::new();
        *FOCUS_WINDOW_CHANGED_SIGNAL.write().unwrap() = Some(focus_window_changed.clone());

        Self {
            core_app,
            application_state_changed: Signal::new(),
            last_window_closed,
            focus_window_changed,
        }
    }

    /// Access the underlying `CoreApplication` reference.
    pub fn core_application(&self) -> &CoreApplication {
        &self.core_app
    }

    /// Access the underlying mutable `CoreApplication` reference.
    pub fn core_application_mut(&mut self) -> &mut CoreApplication {
        &mut self.core_app
    }

    /// Enters the main event loop and returns the exit code.
    pub fn exec(&mut self) -> i32 {
        self.core_app.exec()
    }

    /// Stops the event loop and makes `exec()` return `return_code`.
    pub fn exit(return_code: i32) {
        CoreApplication::exit(return_code);
    }

    /// Tells the application to quit cleanly.
    pub fn quit() {
        CoreApplication::quit();
    }

    /// Signal emitted when the application is about to exit.
    pub fn about_to_quit(&self) -> &Signal<()> {
        self.core_app.about_to_quit()
    }

    /// Signal emitted when the application state changes.
    pub fn application_state_changed(&self) -> &Signal<ApplicationState> {
        &self.application_state_changed
    }

    /// Signal emitted when the last top-level window has been closed.
    pub fn last_window_closed(&self) -> &Signal<()> {
        &self.last_window_closed
    }

    /// Signal emitted when the focused window changes.
    pub fn focus_window_changed(&self) -> &Signal<Option<ObjectId>> {
        &self.focus_window_changed
    }

    // --- Palette ---

    pub fn palette() -> Palette {
        GLOBAL_PALETTE
            .read()
            .unwrap()
            .as_ref()
            .cloned()
            .unwrap_or_else(Palette::dark)
    }

    pub fn set_palette(palette: Palette) {
        *GLOBAL_PALETTE.write().unwrap() = Some(palette);
    }

    // --- Font ---

    pub fn font() -> Font {
        GLOBAL_FONT
            .read()
            .unwrap()
            .as_ref()
            .cloned()
            .unwrap_or_default()
    }

    pub fn set_font(font: Font) {
        *GLOBAL_FONT.write().unwrap() = Some(font);
    }

    // --- Style Hints ---

    pub fn style_hints() -> StyleHints {
        GLOBAL_STYLE_HINTS
            .read()
            .unwrap()
            .as_ref()
            .cloned()
            .unwrap_or_default()
    }

    pub fn set_style_hints(hints: StyleHints) {
        *GLOBAL_STYLE_HINTS.write().unwrap() = Some(hints);
    }

    // --- Layout Direction ---

    pub fn layout_direction() -> LayoutDirection {
        *GLOBAL_LAYOUT_DIRECTION.read().unwrap()
    }

    pub fn set_layout_direction(direction: LayoutDirection) {
        *GLOBAL_LAYOUT_DIRECTION.write().unwrap() = direction;
    }

    pub fn is_left_to_right() -> bool {
        Self::layout_direction() == LayoutDirection::LeftToRight
    }

    pub fn is_right_to_left() -> bool {
        Self::layout_direction() == LayoutDirection::RightToLeft
    }

    // --- Application State ---

    pub fn application_state() -> ApplicationState {
        *GLOBAL_APP_STATE.read().unwrap()
    }

    pub fn set_application_state(&self, state: ApplicationState) {
        let changed = {
            let mut guard = GLOBAL_APP_STATE.write().unwrap();
            if *guard != state {
                *guard = state;
                true
            } else {
                false
            }
        };
        if changed {
            self.application_state_changed.emit(&state);
        }
    }

    // --- Display & Desktop File Name ---

    pub fn application_display_name() -> String {
        GLOBAL_DISPLAY_NAME
            .read()
            .unwrap()
            .as_ref()
            .cloned()
            .unwrap_or_else(CoreApplication::application_name)
    }

    pub fn set_application_display_name(name: impl Into<String>) {
        *GLOBAL_DISPLAY_NAME.write().unwrap() = Some(name.into());
    }

    pub fn desktop_file_name() -> String {
        GLOBAL_DESKTOP_FILE_NAME
            .read()
            .unwrap()
            .as_ref()
            .cloned()
            .unwrap_or_default()
    }

    pub fn set_desktop_file_name(name: impl Into<String>) {
        *GLOBAL_DESKTOP_FILE_NAME.write().unwrap() = Some(name.into());
    }

    // --- Quit on Last Window Closed ---

    pub fn quit_on_last_window_closed() -> bool {
        GLOBAL_QUIT_ON_LAST_WINDOW_CLOSED.load(Ordering::SeqCst)
    }

    pub fn set_quit_on_last_window_closed(quit: bool) {
        GLOBAL_QUIT_ON_LAST_WINDOW_CLOSED.store(quit, Ordering::SeqCst);
    }

    // --- High DPI Policy ---

    pub fn high_dpi_scale_factor_rounding_policy() -> HighDpiScaleFactorRoundingPolicy {
        *GLOBAL_ROUNDING_POLICY.read().unwrap()
    }

    pub fn set_high_dpi_scale_factor_rounding_policy(policy: HighDpiScaleFactorRoundingPolicy) {
        *GLOBAL_ROUNDING_POLICY.write().unwrap() = policy;
    }

    /// Resets internal singleton and styling state for testing isolation.
    #[doc(hidden)]
    pub fn reset_for_test() {
        CoreApplication::reset_for_test();
        *GLOBAL_PALETTE.write().unwrap() = None;
        *GLOBAL_FONT.write().unwrap() = None;
        *GLOBAL_STYLE_HINTS.write().unwrap() = None;
        *GLOBAL_LAYOUT_DIRECTION.write().unwrap() = LayoutDirection::LeftToRight;
        *GLOBAL_APP_STATE.write().unwrap() = ApplicationState::ApplicationActive;
        *GLOBAL_DISPLAY_NAME.write().unwrap() = None;
        *GLOBAL_DESKTOP_FILE_NAME.write().unwrap() = None;
        GLOBAL_QUIT_ON_LAST_WINDOW_CLOSED.store(true, Ordering::SeqCst);
        *GLOBAL_ROUNDING_POLICY.write().unwrap() = HighDpiScaleFactorRoundingPolicy::PassThrough;
        *LAST_WINDOW_CLOSED_SIGNAL.write().unwrap() = None;
        *FOCUS_WINDOW_CHANGED_SIGNAL.write().unwrap() = None;
    }

    /// Emits `lastWindowClosed` if registered (`qguiapplication.cpp:3939`).
    pub fn emit_last_window_closed() {
        if let Some(sig) = LAST_WINDOW_CLOSED_SIGNAL.read().unwrap().as_ref() {
            sig.emit(&());
        }
    }

    /// Emits `focusWindowChanged` if registered (`qguiapplication.cpp:2417`).
    pub fn emit_focus_window_changed(window_id: Option<ObjectId>) {
        if let Some(sig) = FOCUS_WINDOW_CHANGED_SIGNAL.read().unwrap().as_ref() {
            sig.emit(&window_id);
        }
    }
}

static GUI_APP_META_OBJECT: MetaObject = MetaObject::new(
    "QGuiApplication",
    Some(&ferrokit_core::meta::QOBJECT_META_OBJECT),
    &[],
    &[],
    &[],
    &[],
);

impl QObject for GuiApplication {
    fn object_data(&self) -> &ObjectData {
        self.core_app.object_data()
    }

    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        self.core_app.object_data_mut()
    }

    fn as_qobject_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_qobject_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn meta_object(&self) -> &'static MetaObject {
        &GUI_APP_META_OBJECT
    }
}
#[cfg(test)]
mod hardening_tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    #[test]
    fn application_state_slot_can_read_state_reentrantly() {
        let app = GuiApplication::new(Vec::new());
        let observed = Arc::new(AtomicBool::new(false));
        let observed_in_slot = Arc::clone(&observed);
        app.application_state_changed().connect(move |state| {
            observed_in_slot.store(GuiApplication::application_state() == *state, Ordering::SeqCst);
        });
        let next = if GuiApplication::application_state() == ApplicationState::ApplicationActive {
            ApplicationState::ApplicationInactive
        } else {
            ApplicationState::ApplicationActive
        };
        app.set_application_state(next);
        assert!(observed.load(Ordering::SeqCst));
    }
}
