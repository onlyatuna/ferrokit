// SPDX-License-Identifier: LGPL-3.0-only
//! Tool tips: `QToolTip` and the application-level protocol that wakes one up.
//!
//! Qt shows a tool tip in two steps, and ferrokit keeps them apart as Qt does.
//!
//! 1. `QApplication::notify` (qapplication.cpp:2712-2737): a spontaneous mouse move with no button
//!    down remembers the widget under the cursor and starts the *wake-up* timer, 700 ms
//!    (`SH_ToolTip_WakeUpDelay`, qcommonstyle.cpp:5348), or 20 ms while the *fall-asleep* timer
//!    (2000 ms, `SH_ToolTip_FallAsleepDelay`) is still running, so moving to the next widget after
//!    a tip was shown does not wait again. User input, window activation and `Leave` stop the
//!    timers (qapplication.cpp:2622-2637).
//! 2. When the wake-up timer fires (`QApplication::event`, qapplication.cpp:1710-1740) and the
//!    widget's window is the active window (or has `WA_AlwaysShowToolTips`), a `QHelpEvent`
//!    `ToolTip` is sent to the widget. It bubbles to the parents until one handles and accepts it
//!    (qapplication.cpp:2940-2962). `QWidget::event` answers by `QToolTip::showText` with the
//!    widget's own `toolTip`, or ignores the event when there is none (qwidget.cpp:9381-9386). An
//!    accepted tip starts the fall-asleep timer.
//!
//! `QToolTip::showText` (qtooltip.cpp) puts the text in a top-level, non-activating window
//! (`Qt::ToolTip`), positioned by [`place_tip`] and removed by an expire timer, by user input or
//! shortly after the cursor leaves.
//!
//! The 700 ms / 2 s / 300 ms / 10 s timers are the Qt tool tip protocol, not application debounce.
//! Application state is per thread, like `QApplicationPrivate`.
use ferrokit_gui::text::might_be_rich_text;

use crate::label::Label;
use crate::widget::{Widget, WidgetRef, WidgetWeak};
use crate::window::{window_shows_tool_tips, Window};
use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::event_loop::{filters_consume, notify_borrowed};
use ferrokit_core::object::ObjectId;
use ferrokit_core::timer::Timer;
use ferrokit_gui::geometry::primitives::{Point, Rect, Size};
use ferrokit_gui::paint::palette::{ColorGroup, ColorRole, Palette};
use ferrokit_gui::text::Font;
use ferrokit_platform::window::WindowFlags;
use std::cell::{Cell, RefCell};

/// `SH_ToolTip_WakeUpDelay` (qcommonstyle.cpp:5348-5350).
pub const WAKE_UP_DELAY_MS: u64 = 700;
/// `SH_ToolTip_FallAsleepDelay` (qcommonstyle.cpp:5351-5353).
pub const FALL_ASLEEP_DELAY_MS: u64 = 2000;
/// The wake-up delay while the fall-asleep timer still runs (qapplication.cpp:2731).
pub const QUICK_WAKE_UP_DELAY_MS: u64 = 20;
/// `QTipLabel::hideTip` waits this long before hiding (qtooltip.cpp:176-180).
pub const HIDE_DELAY_MS: u64 = 300;
/// `QPlatformCursor::size` default (qplatformcursor.cpp:100-103), 16x16. The tip itself uses
/// [`cursor_size`]; this is the Qt value the hand-derived `place_tip` tests are written against.
pub const CURSOR_SIZE: Size = Size {
    width: 16,
    height: 16,
};

/// `QHighDpi::fromNativePixels(cursor->size(), screen)` (qtooltip.cpp:325-326): the platform
/// cursor's native size on the screen with `screen_dpr`, in device-independent pixels (rounded).
pub fn cursor_size(screen_dpr: f64, primary_dpr: f64) -> Size {
    let (w, h) = ferrokit_platform::integration::platform()
        .cursor()
        .size(screen_dpr, primary_dpr);
    let dpr = if screen_dpr > 0.0 { screen_dpr } else { 1.0 };
    Size::new(
        (w as f64 / dpr).round() as i32,
        (h as f64 / dpr).round() as i32,
    )
}

/// `QTipLabel::restartExpireTimer` (qtooltip.cpp:117-126): how long a tip stays, in ms. `msec > 0`
/// is the caller's duration; otherwise `10000 + 40 * max(0, length - 100)`, the length being the
/// text's UTF-16 length like `QString::size`.
pub fn expire_time_ms(text: &str, msec: i32) -> u64 {
    if msec > 0 {
        return msec as u64;
    }
    let length = text.encode_utf16().count() as i64;
    10_000 + 40 * (length - 100).max(0) as u64
}

/// `QTipLabel::placeTip` (qtooltip.cpp:296-370): where the top-left corner of a tip of `tip_size`
/// goes for a cursor at the global position `pos` on a screen covering `screen`.
///
/// The tip sits right of and below the cursor (`2`, cursor height; beside the cursor when the
/// cursor is more than twice the tip's height). A tip that overflows the right edge flips to the
/// left of the cursor (`4 + width`) and one that overflows the bottom flips above it
/// (`24 + height`); what still overflows is then clamped into the screen.
pub fn place_tip(pos: Point, tip_size: Size, cursor_size: Size, screen: Rect) -> Point {
    let (w, h) = (tip_size.width, tip_size.height);
    let mut offset = Point::new(2, cursor_size.height);
    if cursor_size.height > 2 * h {
        offset = Point::new(cursor_size.width / 2, 0);
    }
    let mut p = Point::new(pos.x + offset.x, pos.y + offset.y);
    let right = screen.x + screen.width;
    let bottom = screen.y + screen.height;
    if p.x + w > right {
        p.x -= 4 + w;
    }
    if p.y + h > bottom {
        p.y -= 24 + h;
    }
    if p.y < screen.y {
        p.y = screen.y;
    }
    if p.x + w > right {
        p.x = right - w;
    }
    if p.x < screen.x {
        p.x = screen.x;
    }
    if p.y + h > bottom {
        p.y = bottom - h;
    }
    p
}

/// The screen a tip for `pos` is placed on: the one containing the point, else the primary one
/// (`QTipLabel::getTipScreen`, qtooltip.cpp:268-273); with its device pixel ratio.
fn tip_screen(pos: Point) -> (Rect, f64) {
    let platform = ferrokit_platform::integration::platform();
    let screens = platform.screens();
    screens
        .iter()
        .find(|s| s.geometry().contains(pos))
        .map(|s| (s.geometry(), s.device_pixel_ratio() as f64))
        .unwrap_or_else(|| {
            let p = platform.primary_screen();
            (p.geometry(), p.device_pixel_ratio() as f64)
        })
}

/// `QTipLabel::instance`: the window that shows the current tip.
struct Tip {
    window: Box<Window>,
    label: WidgetRef,
    /// The widget the tip was shown for (`QTipLabel::widget`).
    widget: Option<ObjectId>,
    text: String,
    rect: Option<Rect>,
}

/// `QApplicationPrivate`'s tool tip state, and `QTipLabel`'s timers.
struct Controller {
    tool_tip_widget: RefCell<Option<WidgetWeak>>,
    tool_tip_pos: Cell<Point>,
    tool_tip_global_pos: Cell<Point>,
    wake_up: Timer,
    fall_asleep: Timer,
    tip: RefCell<Option<Tip>>,
    expire: Timer,
    hide: Timer,
    custom_font: RefCell<Option<Font>>,
    custom_palette: RefCell<Option<Palette>>,
    /// Nesting depth of `ToolTip::place`, which takes the tip out of `tip` while it works.
    placing: Cell<u32>,
    /// A hide that arrived while the tip was taken out; `place` applies it when it puts the tip back.
    pending_hide: Cell<PendingHide>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingHide {
    None,
    /// `hide_tip`: hide after [`HIDE_DELAY_MS`].
    Delayed,
    /// `hide_tip_immediately`.
    Now,
}

fn single_shot_timer(on_timeout: fn()) -> Timer {
    let timer = Timer::new();
    timer.set_single_shot(true);
    timer.timeout.connect(move |()| on_timeout());
    timer
}

// `Timer`'s methods take `&self` and a start only arms the thread's timer context (a zero
// interval posts a `ZeroTimer` instead of calling the slot), so the timers need no `RefCell`: a
// timeout that starts or stops another timer, or its own, cannot hit a nested borrow.
fn start_timer(timer: &Timer, ms: u64) {
    timer.start_with_interval(ms);
}

fn stop_timer(timer: &Timer) {
    timer.stop();
}

fn timer_active(timer: &Timer) -> bool {
    timer.is_active()
}

impl Controller {
    fn new() -> Self {
        Self {
            tool_tip_widget: RefCell::new(None),
            tool_tip_pos: Cell::new(Point::new(0, 0)),
            tool_tip_global_pos: Cell::new(Point::new(0, 0)),
            wake_up: single_shot_timer(wake_up_fired),
            fall_asleep: single_shot_timer(|| {}),
            tip: RefCell::new(None),
            expire: single_shot_timer(hide_tip_immediately),
            hide: single_shot_timer(hide_tip_immediately),
            custom_font: RefCell::new(None),
            custom_palette: RefCell::new(None),
            placing: Cell::new(0),
            pending_hide: Cell::new(PendingHide::None),
        }
    }
}
thread_local! {
    static CONTROLLER: Controller = Controller::new();
}

/// What `QApplication::notify` does with a *spontaneous* mouse move (qapplication.cpp:2712-2737):
/// remember the widget under the cursor and (re)start the wake-up timer. Only a move with no
/// button down starts it.
pub(crate) fn mouse_moved(target: &WidgetRef, pos: Point, global_pos: Point) {
    CONTROLLER.with(|c| {
        *c.tool_tip_widget.borrow_mut() = Some(std::rc::Rc::downgrade(target));
        c.tool_tip_pos.set(pos);
        c.tool_tip_global_pos.set(global_pos);
        let delay = if timer_active(&c.fall_asleep) {
            QUICK_WAKE_UP_DELAY_MS
        } else {
            WAKE_UP_DELAY_MS
        };
        start_timer(&c.wake_up, delay);
    });
}

/// `QEvent::Leave` (qapplication.cpp:2635 and `QTipLabel::eventFilter`, qtooltip.cpp:233-237): the
/// pending wake-up is cancelled and a visible tip is hidden after [`HIDE_DELAY_MS`].
pub(crate) fn leave() {
    CONTROLLER.with(|c| stop_timer(&c.wake_up));
    hide_tip();
}

/// Events after which a tip goes away at once and tool tips fall asleep (qapplication.cpp:
/// 2622-2637, `QTipLabel::eventFilter`, qtooltip.cpp:238-282): wheel, focus changes and mouse
/// buttons; key events too, which end the fall-asleep state but hide a tip only on macOS.
pub(crate) fn user_input(kind: &EventKind) {
    let hides_tip = match kind {
        EventKind::Wheel { .. }
        | EventKind::FocusIn { .. }
        | EventKind::FocusOut { .. }
        | EventKind::MouseButtonPress { .. }
        | EventKind::MouseButtonRelease { .. }
        | EventKind::MouseButtonDblClick { .. } => true,
        EventKind::KeyPress { .. } | EventKind::KeyRelease { .. } => cfg!(target_os = "macos"),
        _ => return,
    };
    let falls_asleep = matches!(
        kind,
        EventKind::Wheel { .. }
            | EventKind::FocusIn { .. }
            | EventKind::FocusOut { .. }
            | EventKind::KeyPress { .. }
            | EventKind::KeyRelease { .. }
            | EventKind::MouseButtonPress { .. }
            | EventKind::MouseButtonRelease { .. }
            | EventKind::MouseButtonDblClick { .. }
    );
    CONTROLLER.with(|c| {
        if falls_asleep {
            stop_timer(&c.fall_asleep);
        }
        stop_timer(&c.wake_up);
    });
    if hides_tip {
        hide_tip_immediately();
    }
}

/// `QApplication::event(QEvent::Timer)` for the wake-up timer (qapplication.cpp:1710-1738).
fn wake_up_fired() {
    let (widget, pos, global_pos) = CONTROLLER.with(|c| {
        (
            c.tool_tip_widget
                .borrow()
                .as_ref()
                .and_then(|w| w.upgrade()),
            c.tool_tip_pos.get(),
            c.tool_tip_global_pos.get(),
        )
    });
    let Some(widget) = widget else { return };
    if !window_shows_tool_tips(&top_level(&widget)) {
        return;
    }
    if send_tool_tip(&widget, pos, global_pos) {
        CONTROLLER.with(|c| start_timer(&c.fall_asleep, FALL_ASLEEP_DELAY_MS));
    }
}

/// `QWidget::window()`: the widget with no parent.
fn top_level(widget: &WidgetRef) -> WidgetRef {
    let mut current = widget.clone();
    loop {
        let parent = current.borrow().parent_widget().and_then(|p| p.upgrade());
        match parent {
            Some(parent) => current = parent,
            None => return current,
        }
    }
}

/// `QApplication::notify` for `ToolTip` (qapplication.cpp:2940-2962): the help event goes to the
/// widget first and, while it is not handled and accepted, to each parent in the parent's
/// coordinates, up to the window widget. Returns whether the last receiver accepted it.
///
/// A widget whose `event` does not handle the event gets `QWidget::event`'s answer
/// ([`default_tool_tip_event`]).
pub(crate) fn send_tool_tip(target: &WidgetRef, pos: Point, global_pos: Point) -> bool {
    let mut widget = target.clone();
    let mut rel = pos;
    loop {
        let mut event = Event::new_spontaneous(EventKind::ToolTip {
            x: rel.x,
            y: rel.y,
            global_x: global_pos.x,
            global_y: global_pos.y,
        });
        // `QApplication::notify` runs the application and object event filters before the
        // receiver's `event()`. A widget that is already borrowed cannot run `event()`, but its
        // filters still see the event when it is only shared-borrowed.
        let handled = match widget.try_borrow_mut() {
            Ok(mut w) => notify_borrowed(&mut **w, &mut event),
            Err(_) => match widget.try_borrow() {
                Ok(w) => filters_consume(w.object_data(), &mut event),
                Err(_) => false,
            },
        };
        let accepted = if handled {
            event.is_accepted()
        } else {
            default_tool_tip_event(&widget, global_pos, &mut event)
        };
        if accepted {
            return true;
        }
        let Ok(held) = widget.try_borrow() else { return false };
        let parent = held.parent_widget().and_then(|p| p.upgrade());
        let g = held.geometry();
        drop(held);
        let Some(parent) = parent else { return false };
        rel = Point::new(rel.x + g.x, rel.y + g.y);
        widget = parent;
    }
}

/// `QWidget::event(ToolTip)` (qwidget.cpp:9381-9386): show the widget's own tool tip, or ignore
/// the event when it has none. Returns whether the event ends up accepted.
fn default_tool_tip_event(widget: &WidgetRef, global_pos: Point, event: &mut Event) -> bool {
    // A widget that is mutably borrowed cannot answer: it shows no tip and the event moves on.
    let Ok(w) = widget.try_borrow() else {
        event.ignore();
        return false;
    };
    let (text, duration, id) = (w.tool_tip(), w.tool_tip_duration(), w.id());
    drop(w);
    if text.is_empty() {
        event.ignore();
        return false;
    }
    ToolTip::show_text(global_pos, &text, Some(id), duration);
    event.accept();
    true
}

fn hide_tip() {
    CONTROLLER.with(|c| {
        if tip_is_taken_out(c) {
            if c.pending_hide.get() == PendingHide::None {
                c.pending_hide.set(PendingHide::Delayed);
            }
            return;
        }
        if tip_is_visible(c) && !timer_active(&c.hide) {
            start_timer(&c.hide, HIDE_DELAY_MS);
        }
    });
}

/// `QTipLabel::hideTipImmediately`: the tip goes away now.
fn hide_tip_immediately() {
    CONTROLLER.with(|c| {
        stop_timer(&c.hide);
        stop_timer(&c.expire);
        if tip_is_taken_out(c) {
            c.pending_hide.set(PendingHide::Now);
            return;
        }
        // Taken out of the slot while the window hides: a `Hide` handler or event filter may call
        // the tip API, and no window operation runs with the slot borrowed.
        let Some(mut tip) = c.tip.try_borrow_mut().ok().and_then(|mut slot| slot.take()) else {
            return;
        };
        c.placing.set(c.placing.get() + 1);
        if tip.window.is_visible() {
            tip.window.hide();
        }
        tip.text.clear();
        tip.widget = None;
        c.placing.set(c.placing.get() - 1);
        if c.placing.get() == 0 {
            // A hide recorded by a re-entrant call has nothing left to apply to.
            c.pending_hide.set(PendingHide::None);
        }
        put_back(c, tip);
    });
}

/// Puts `tip` into the empty slot. When a re-entrant call installed a newer tip meanwhile, the
/// newer one stays and `tip`'s window is hidden and dropped with the slot not borrowed (a
/// `Window`'s drop may dispatch events). Returns whether `tip` went back.
fn put_back(c: &Controller, tip: Tip) -> bool {
    let rejected = {
        let mut slot = c.tip.borrow_mut();
        if slot.is_none() {
            *slot = Some(tip);
            None
        } else {
            Some(tip)
        }
    };
    match rejected {
        None => true,
        Some(mut tip) => {
            tip.window.hide();
            false
        }
    }
}

/// The tip's label (`None` while the slot is taken out or borrowed), cloned so the slot is not
/// borrowed while the label changes.
fn tip_label(c: &Controller) -> Option<WidgetRef> {
    let slot = c.tip.try_borrow().ok()?;
    slot.as_ref().map(|tip| tip.label.clone())
}

/// Whether `ToolTip::place` is working on the tip, so the slot is empty but a tip is on its way.
fn tip_is_taken_out(c: &Controller) -> bool {
    c.placing.get() > 0 && c.tip.try_borrow().map(|t| t.is_none()).unwrap_or(false)
}

fn tip_is_visible(c: &Controller) -> bool {
    c.tip
        .try_borrow()
        .map(|t| t.as_ref().is_some_and(|t| t.window.is_visible()))
        .unwrap_or(false)
}

/// The tip's label: `QPalette::ToolTipBase` and `ToolTipText` of the inactive group, since tool
/// tips are not active windows (qtooltip.cpp:64-65). Appearance beyond that is the style's.
fn new_tip_label(text: &str) -> Label {
    let palette = ToolTip::palette();
    let mut label = Label::new(text);
    label.set_background_color(Some(
        palette.color(ColorGroup::Inactive, ColorRole::ToolTipBase),
    ));
    label.set_color(palette.color(ColorGroup::Inactive, ColorRole::ToolTipText));
    label.set_font(ToolTip::font());
    label.set_type_name("QToolTip");
    // `QTipLabel` margin: `1 + PM_ToolTipLabelFrameWidth` (qtooltip.cpp:107). Windows gives 2.
    label.set_margin(2);
    label
}

/// The second half of `ToolTip::place`: puts the tip back into the controller after it was shown
/// outside the slot borrow, and applies what happened while it was out (a newer tip, a hide,
/// a palette or font change). No window operation runs while the slot (or the palette and font
/// cells) is borrowed: a `Hide` handler may call the tip API.
fn finish_place(c: &Controller, mut tip: Tip, text: &str, msec_display_time: i32) {
    c.placing.set(c.placing.get() - 1);
    let pending = c.pending_hide.replace(PendingHide::None);
    let occupied = c.tip.borrow().is_some();
    if occupied {
        // A re-entrant call installed a newer tip while this one was showing; the
        // newer one wins and this window must not stay up on its own.
        tip.window.hide();
        return;
    }
    // `set_palette` / `set_font` skip a tip that is taken out: catch up here.
    let palette = c.custom_palette.borrow().clone();
    let font = c.custom_font.borrow().clone();
    if let Ok(mut label) = tip.label.try_borrow_mut() {
        if let Some(lbl) = label.as_any_mut().downcast_mut::<Label>() {
            if let Some(palette) = palette {
                lbl.set_background_color(Some(
                    palette.color(ColorGroup::Inactive, ColorRole::ToolTipBase),
                ));
                lbl.set_color(palette.color(ColorGroup::Inactive, ColorRole::ToolTipText));
            }
            if let Some(font) = font {
                lbl.set_font(font);
            }
        }
    }
    if pending == PendingHide::Now {
        // A hide that came in while the tip was showing wins over this show. The timers stop
        // first: a `Hide` handler may show a newer tip, which starts its own.
        stop_timer(&c.hide);
        stop_timer(&c.expire);
        tip.window.hide();
        tip.text.clear();
        tip.widget = None;
        put_back(c, tip);
    } else if put_back(c, tip) {
        // `restartExpireTimer`: a new expiry cancels a pending hide.
        start_timer(&c.expire, expire_time_ms(text, msec_display_time));
        stop_timer(&c.hide);
        if pending == PendingHide::Delayed {
            start_timer(&c.hide, HIDE_DELAY_MS);
        }
    }
}

/// `QToolTip`.
pub struct ToolTip;

impl ToolTip {
    /// `QToolTip::showText(pos, text, w, QRect(), msecDisplayTime)` (qtooltip.cpp:404-460): shows
    /// `text` next to the global position `pos`. An empty `text` hides the tip. While a tip is
    /// showing, a different text or widget replaces it in place; the same text for the same widget
    /// leaves it where it is.
    ///
    /// `widget` is the widget the tip is for (it only has to identify it); `msec_display_time <= 0`
    /// derives the display time from the text length ([`expire_time_ms`]).
    pub fn show_text(pos: Point, text: &str, widget: Option<ObjectId>, msec_display_time: i32) {
        Self::show_text_rect(pos, text, widget, None, msec_display_time);
    }

    /// `QToolTip::showText(pos, text, w, rect, msecDisplayTime)` (qtooltip.cpp:414).
    pub fn show_text_rect(
        pos: Point,
        text: &str,
        widget: Option<ObjectId>,
        rect: Option<Rect>,
        msec_display_time: i32,
    ) {
        let visible = CONTROLLER.with(tip_is_visible);
        if visible {
            if text.is_empty() {
                hide_tip();
                return;
            }
            let changed = CONTROLLER.with(|c| {
                c.tip.try_borrow().is_ok_and(|slot| {
                    slot.as_ref()
                        .is_some_and(|t| t.text != text || t.widget != widget || t.rect != rect)
                })
            });
            if changed {
                Self::place(pos, text, widget, rect, msec_display_time);
            }
            return;
        }
        if !text.is_empty() {
            Self::place(pos, text, widget, rect, msec_display_time);
        }
    }

    /// Returns the active tooltip palette (`QToolTip::palette`).
    pub fn palette() -> Palette {
        CONTROLLER.with(|c| {
            c.custom_palette
                .borrow()
                .clone()
                .unwrap_or_else(crate::application::Application::palette)
        })
    }

    /// Sets the active tooltip palette (`QToolTip::setPalette`).
    pub fn set_palette(palette: Palette) {
        CONTROLLER.with(|c| {
            *c.custom_palette.borrow_mut() = Some(palette.clone());
            let Some(label) = tip_label(c) else { return };
            let Ok(mut label) = label.try_borrow_mut() else { return };
            if let Some(lbl) = label.as_any_mut().downcast_mut::<Label>() {
                lbl.set_background_color(Some(
                    palette.color(ColorGroup::Inactive, ColorRole::ToolTipBase),
                ));
                lbl.set_color(palette.color(ColorGroup::Inactive, ColorRole::ToolTipText));
            }
        });
    }

    /// Returns the active tooltip font (`QToolTip::font`).
    pub fn font() -> Font {
        CONTROLLER.with(|c| {
            c.custom_font
                .borrow()
                .clone()
                .unwrap_or_else(|| Font::new(crate::label::APP_DEFAULT_FAMILY, 12.0))
        })
    }

    /// Sets the active tooltip font (`QToolTip::setFont`).
    pub fn set_font(font: Font) {
        CONTROLLER.with(|c| {
            *c.custom_font.borrow_mut() = Some(font.clone());
            let Some(label) = tip_label(c) else { return };
            let Ok(mut label) = label.try_borrow_mut() else { return };
            if let Some(lbl) = label.as_any_mut().downcast_mut::<Label>() {
                lbl.set_font(font);
            }
        });
    }

    /// `QToolTip::hideText`: the same as `showText` with an empty text.
    pub fn hide_text() {
        Self::show_text(Point::new(0, 0), "", None, -1);
    }

    /// `QToolTip::isVisible`.
    pub fn is_visible() -> bool {
        CONTROLLER.with(tip_is_visible)
    }

    /// `QToolTip::text`: the text of the visible tip, or an empty string.
    pub fn text() -> String {
        CONTROLLER.with(|c| {
            let Ok(slot) = c.tip.try_borrow() else {
                return String::new();
            };
            slot.as_ref()
                .filter(|t| t.window.is_visible())
                .map(|t| t.text.clone())
                .unwrap_or_default()
        })
    }

    /// Where the visible tip is, in global logical coordinates (`QTipLabel::geometry`).
    pub fn geometry() -> Option<Rect> {
        CONTROLLER.with(|c| {
            let slot = c.tip.try_borrow().ok()?;
            slot.as_ref()
                .filter(|t| t.window.is_visible())
                .map(|t| t.window.geometry())
        })
    }

    /// Shows or moves the tip: `reuseTip` + `placeTip` (qtooltip.cpp:128-142, 296-370) for a tip
    /// that is up, `QTipLabel`'s constructor and `showNormal` for a new one.
    fn place(
        pos: Point,
        text: &str,
        widget: Option<ObjectId>,
        rect: Option<Rect>,
        msec_display_time: i32,
    ) {
        // The controller's slot is never borrowed across a call that can re-enter (window
        // creation, `set_geometry`, `show`): the tip is taken out, worked on, and put back.
        let taken = CONTROLLER.with(|c| {
            c.placing.set(c.placing.get() + 1);
            c.pending_hide.set(PendingHide::None);
            c.tip.borrow_mut().take()
        });
        let mut tip = match taken {
            Some(tip) => tip,
            None => {
                let mut label = new_tip_label(text);
                label.set_text(text);
                let label: WidgetRef = std::rc::Rc::new(RefCell::new(Box::new(label)));
                let Ok(mut window) =
                    Window::new("tooltip", Rect::new(0, 0, 1, 1), WindowFlags::TOOLTIP)
                else {
                    CONTROLLER.with(|c| c.placing.set(c.placing.get() - 1));
                    return;
                };
                window.set_root_widget(label.clone());
                Tip {
                    window: Box::new(window),
                    label,
                    widget,
                    rect,
                    text: String::new(),
                }
            }
        };
        tip.text = text.to_string();
        tip.widget = widget;
        tip.rect = rect;
        let (screen, screen_dpr) = tip_screen(pos);
        let size = {
            let mut label = tip.label.borrow_mut();
            let label = label
                .as_any_mut()
                .downcast_mut::<Label>()
                .expect("the tip's root widget is a Label");
            // `QTipLabel::updateSize` (qtooltip.cpp:152): rich text word-wraps.
            let rich = might_be_rich_text(text);
            label.set_rich(rich);
            label.set_word_wrap(rich);
            label.set_text(text);
            let mut size = label.size_hint();
            // qtooltip.cpp:155-157: a plain tip wider than the screen wraps to it.
            if !label.word_wrap() && size.width > screen.width {
                label.set_word_wrap(true);
                size = label.size_hint();
            }
            size
        };
        // `QTipLabel::updateSize` adds one pixel of width (qtooltip.cpp:159).
        let size = Size::new(size.width + 1, size.height);
        let primary_dpr = ferrokit_platform::integration::platform()
            .primary_screen()
            .device_pixel_ratio() as f64;
        let cursor = cursor_size(screen_dpr, primary_dpr);
        let origin = place_tip(pos, size, cursor, screen);
        tip.window
            .set_geometry(Rect::new(origin.x, origin.y, size.width, size.height));
        if !tip.window.is_visible() {
            tip.window.show();
        }
        CONTROLLER.with(|c| finish_place(c, tip, text, msec_display_time));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::EmptyWidget;
    use ferrokit_core::object::{register_owned_qobject, ObjectData, ObjectId, QObject};
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::{Arc, Mutex};

    /// Event filter that records the `ToolTip` events it sees and consumes them.
    struct ToolTipSpy {
        data: ObjectData,
        seen: Arc<Mutex<Vec<ObjectId>>>,
    }

    impl QObject for ToolTipSpy {
        fn object_data(&self) -> &ObjectData {
            &self.data
        }
        fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
            ferrokit_core::object::ObjectDataMut::new(&mut self.data)
        }
        fn event(&mut self, _event: &mut Event) -> bool {
            false
        }
        fn event_filter(&mut self, watched: ObjectId, event: &mut Event) -> bool {
            if matches!(event.kind, EventKind::ToolTip { .. }) {
                self.seen.lock().unwrap().push(watched);
                return true;
            }
            false
        }
    }

    fn filtered_widget() -> (WidgetRef, Arc<Mutex<Vec<ObjectId>>>, impl Sized) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let spy = ToolTipSpy { data: ObjectData::with_auto_id(), seen: Arc::clone(&seen) };
        // SAFETY: test-only; the owner stays on this thread for the whole test and nothing
        // touches the spy while a callback of it runs.
        let owner = unsafe { register_owned_qobject(spy) };
        let widget: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::new())));
        widget
            .borrow_mut()
            .object_data_mut()
            .install_event_filter(ferrokit_core::object::OwnedObject::object_id(&owner));
        (widget, seen, owner)
    }

    #[test]
    fn an_event_filter_sees_the_tool_tip_event() {
        let (widget, seen, _owner) = filtered_widget();
        let id = widget.borrow().id();
        let accepted = send_tool_tip(&widget, Point::new(1, 1), Point::new(1, 1));
        assert!(accepted, "the consuming filter accepted the event");
        assert_eq!(*seen.lock().unwrap(), vec![id]);
    }

    #[test]
    fn set_palette_and_font_survive_a_busy_tip_slot() {
        let held = CONTROLLER.with(|c| c.tip.borrow_mut().take());
        CONTROLLER.with(|c| {
            let _busy = c.tip.borrow_mut();
            ToolTip::set_palette(ToolTip::palette());
            ToolTip::set_font(ToolTip::font());
            assert!(!tip_is_visible(c));
        });
        CONTROLLER.with(|c| *c.tip.borrow_mut() = held);
    }

    /// Leaves a hidden tip in the controller, so the next `show_text` reuses its window.
    fn retained_hidden_tip() {
        ToolTip::show_text(Point::new(10, 10), "first", None, -1);
        assert!(ToolTip::is_visible());
        hide_tip_immediately();
        assert!(!ToolTip::is_visible());
    }

    /// The window event handler of the controller's tip window: it receives `Show` and `Hide`
    /// from inside `Window::show` / `Window::hide`, i.e. while `place`, `finish_place` or
    /// `hide_tip_immediately` is working on that window.
    fn on_tip_window_event(handler: impl Fn(&mut Event) + 'static) {
        CONTROLLER.with(|c| {
            c.tip
                .borrow_mut()
                .as_mut()
                .expect("a tip exists")
                .window
                .set_window_event_handler(handler);
        });
    }

    fn counters() -> (u32, PendingHide) {
        CONTROLLER.with(|c| (c.placing.get(), c.pending_hide.get()))
    }

    #[test]
    fn a_hide_that_re_enters_while_the_tip_is_shown_is_not_lost() {
        retained_hidden_tip();
        let during = Rc::new(Cell::new(None));
        let seen = Rc::clone(&during);
        on_tip_window_event(move |ev| {
            if matches!(ev.kind, EventKind::Show) {
                // The expire timer's callback, arriving while `place` has the tip out.
                hide_tip_immediately();
                seen.set(Some(counters()));
            }
        });
        ToolTip::show_text(Point::new(10, 10), "again", None, -1);
        assert_eq!(during.get(), Some((1, PendingHide::Now)));
        assert!(!ToolTip::is_visible(), "the re-entrant hide wins over the show");
        assert_eq!(ToolTip::text(), "");
        CONTROLLER.with(|c| {
            assert!(!timer_active(&c.expire));
            assert!(!timer_active(&c.hide));
        });
        assert_eq!(counters(), (0, PendingHide::None));
    }

    #[test]
    fn a_delayed_hide_that_re_enters_while_the_tip_is_shown_is_applied_after_the_delay() {
        let _el = ferrokit_core::event_loop::EventLoop::new(); // the timers need a loop to run on
        retained_hidden_tip();
        let during = Rc::new(Cell::new(None));
        let seen = Rc::clone(&during);
        on_tip_window_event(move |ev| {
            if matches!(ev.kind, EventKind::Show) {
                leave(); // the pointer leaves while `place` has the tip out
                seen.set(Some(counters()));
            }
        });
        ToolTip::show_text(Point::new(10, 10), "again", None, -1);
        assert_eq!(during.get(), Some((1, PendingHide::Delayed)));
        assert!(ToolTip::is_visible(), "a delayed hide leaves the tip up");
        assert_eq!(ToolTip::text(), "again");
        CONTROLLER.with(|c| {
            assert!(timer_active(&c.expire));
            assert!(timer_active(&c.hide));
        });
        assert_eq!(counters(), (0, PendingHide::None));
        hide_tip_immediately();
    }

    #[test]
    fn a_pending_hide_left_over_does_not_hide_the_next_show() {
        retained_hidden_tip();
        CONTROLLER.with(|c| c.pending_hide.set(PendingHide::Now));
        ToolTip::show_text(Point::new(10, 10), "again", None, -1);
        assert!(ToolTip::is_visible(), "`place` starts with no pending hide");
        assert_eq!(counters(), (0, PendingHide::None));
        hide_tip_immediately();
    }

    #[test]
    fn a_re_entrant_show_text_leaves_exactly_one_visible_tip() {
        retained_hidden_tip();
        let during = Rc::new(Cell::new(None));
        let seen = Rc::clone(&during);
        let nested = Rc::new(Cell::new(false));
        on_tip_window_event(move |ev| {
            if matches!(ev.kind, EventKind::Show) && !nested.replace(true) {
                ToolTip::show_text(Point::new(20, 20), "nested", None, -1);
                seen.set(Some(counters()));
            }
        });
        ToolTip::show_text(Point::new(10, 10), "outer", None, -1);
        // The nested call ran with the outer one still counted, and took no tip from the slot.
        assert_eq!(during.get(), Some((1, PendingHide::None)));
        assert!(ToolTip::is_visible());
        assert_eq!(ToolTip::text(), "nested", "the newer tip stays");
        assert_eq!(counters(), (0, PendingHide::None));
        hide_tip_immediately();
        assert!(!ToolTip::is_visible());
    }

    #[test]
    fn a_palette_set_while_the_tip_is_out_is_applied_when_it_returns() {
        retained_hidden_tip();
        let mut palette = ToolTip::palette();
        palette.set_color(
            ColorGroup::Inactive,
            ColorRole::ToolTipText,
            ferrokit_gui::tiny_skia::Color::from_rgba8(1, 2, 3, 255),
        );
        let during = palette.clone();
        on_tip_window_event(move |ev| {
            if matches!(ev.kind, EventKind::Show) {
                ToolTip::set_palette(during.clone());
            }
        });
        ToolTip::show_text(Point::new(10, 10), "x", None, -1);
        let applied = CONTROLLER.with(|c| {
            let slot = c.tip.borrow();
            let mut label = slot.as_ref().unwrap().label.borrow_mut();
            let lbl = label.as_any_mut().downcast_mut::<Label>().unwrap();
            lbl.color()
        });
        assert_eq!(
            applied,
            palette.color(ColorGroup::Inactive, ColorRole::ToolTipText)
        );
        hide_tip_immediately();
    }

    /// `finish_place` hides the tip it replaces: a `Hide` handler there may use the whole tip API.
    #[test]
    fn a_hide_handler_may_use_the_tip_api_when_a_newer_tip_replaces_the_tip() {
        retained_hidden_tip();
        let log: Rc<RefCell<Vec<String>>> = Rc::default();
        let sink = Rc::clone(&log);
        let nested = Rc::new(Cell::new(false));
        on_tip_window_event(move |ev| match ev.kind {
            EventKind::Show if !nested.replace(true) => {
                ToolTip::show_text(Point::new(20, 20), "nested", None, -1);
            }
            EventKind::Hide => {
                sink.borrow_mut()
                    .push(format!("{} {}", ToolTip::is_visible(), ToolTip::text()));
                ToolTip::show_text(Point::new(30, 30), "from hide", None, -1);
            }
            _ => {}
        });
        ToolTip::show_text(Point::new(10, 10), "outer", None, -1);
        assert_eq!(*log.borrow(), ["true nested"]);
        assert!(ToolTip::is_visible());
        assert_eq!(ToolTip::text(), "from hide");
        assert_eq!(counters(), (0, PendingHide::None));
        hide_tip_immediately();
    }

    /// `finish_place` applies a hide that came in meanwhile: a `Hide` handler there may use the
    /// whole tip API, and what it shows stays.
    #[test]
    fn a_hide_handler_may_use_the_tip_api_when_a_pending_hide_applies() {
        retained_hidden_tip();
        let log: Rc<RefCell<Vec<String>>> = Rc::default();
        let sink = Rc::clone(&log);
        on_tip_window_event(move |ev| match ev.kind {
            EventKind::Show => hide_tip_immediately(),
            EventKind::Hide => {
                sink.borrow_mut()
                    .push(format!("{} {}", ToolTip::is_visible(), ToolTip::text()));
                ToolTip::show_text(Point::new(30, 30), "revived", None, -1);
            }
            _ => {}
        });
        ToolTip::show_text(Point::new(10, 10), "outer", None, -1);
        assert_eq!(*log.borrow(), ["false "]);
        assert!(ToolTip::is_visible());
        assert_eq!(ToolTip::text(), "revived");
        assert_eq!(counters(), (0, PendingHide::None));
        hide_tip_immediately();
    }

    /// `hide_tip_immediately` hides a visible tip: a `Hide` handler there may use the whole tip API.
    #[test]
    fn a_hide_handler_may_use_the_tip_api_during_hide_tip_immediately() {
        ToolTip::show_text(Point::new(10, 10), "first", None, -1);
        let log: Rc<RefCell<Vec<String>>> = Rc::default();
        let sink = Rc::clone(&log);
        on_tip_window_event(move |ev| {
            if matches!(ev.kind, EventKind::Hide) {
                sink.borrow_mut()
                    .push(format!("{} {}", ToolTip::is_visible(), ToolTip::text()));
                ToolTip::show_text(Point::new(30, 30), "again", None, -1);
            }
        });
        hide_tip_immediately();
        assert_eq!(*log.borrow(), ["false "]);
        assert!(ToolTip::is_visible());
        assert_eq!(ToolTip::text(), "again");
        assert_eq!(counters(), (0, PendingHide::None));
        hide_tip_immediately();
    }

    #[test]
    fn a_mutably_borrowed_widget_does_not_panic_the_tool_tip_walk() {
        let widget: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::new())));
        let _held = widget.borrow_mut();
        assert!(!send_tool_tip(&widget, Point::new(1, 1), Point::new(1, 1)));
    }
}
