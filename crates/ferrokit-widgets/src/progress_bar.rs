// SPDX-License-Identifier: LGPL-3.0-only
//! Horizontal or vertical progress indicator (`QProgressBar`).
//!
//! Value semantics follow `qprogressbar.cpp`: out-of-range values are ignored
//! rather than clamped, `reset()` moves the value to `minimum - 1` ("no
//! progress"), and the text is built from the `%p` / `%v` / `%m` format
//! placeholders. When `minimum == maximum` the bar is in busy (indeterminate)
//! mode and animates a moving chunk driven by [`ProgressBar::advance_busy_indicator`].

use ferrokit_core::event::{Event, EventKind, FocusReason};
use ferrokit_core::event_loop::notify_borrowed;
use ferrokit_core::object::QObject;
use ferrokit_core::signal::Signal;
use ferrokit_gui::Alignment;
use ferrokit_gui::geometry::primitives::{PointF, Rect, RectF, Size};
use ferrokit_gui::paint::brush::Brush;
use ferrokit_gui::paint::painter::{Painter, Pen, RenderHint};
use ferrokit_gui::text::{Font, FontMetrics};
use ferrokit_gui::tiny_skia::Color;

use crate::focus::FocusPolicy;
use crate::input_common;
use crate::scroll::Orientation;
use crate::size_policy::{Policy, QSizePolicy, transposed_policy};
use crate::widget::{Widget, WidgetBase};

/// Reading direction of the text in a vertical progress bar (`QProgressBar::Direction`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProgressTextDirection {
    #[default]
    TopToBottom,
    BottomToTop,
}

/// Progress indicator widget (`QProgressBar`).
pub struct ProgressBar {
    base: WidgetBase,
    minimum: i32,
    maximum: i32,
    value: i32,
    format: String,
    text_visible: bool,
    inverted_appearance: bool,
    orientation: Orientation,
    /// `QProgressBar::alignment`, `Qt::AlignLeft` by default (qprogressbar.cpp:49).
    alignment: Alignment,
    text_direction: ProgressTextDirection,
    busy_phase: f32,
    font: Font,

    track_color: Color,
    chunk_color: Color,
    border_color: Color,
    text_color: Color,

    /// Emitted when the value changes (`valueChanged(int)`).
    pub value_changed: Signal<i32>,
}

pub type QProgressBar = ProgressBar;

const DEFAULT_FORMAT: &str = "%p%";

impl ProgressBar {
    /// Creates a horizontal bar with range `0..=100` and no progress (`value == -1`).
    pub fn new() -> Self {
        let base = WidgetBase::with_geometry(Rect::new(0, 0, 160, 22));
        base.set_focus_policy(FocusPolicy::NoFocus);
        base.set_size_policy(QSizePolicy::new(Policy::Expanding, Policy::Fixed));
        let value_changed = Signal::with_owner(&base.object_data);
        Self {
            base,
            minimum: 0,
            maximum: 100,
            value: -1,
            format: DEFAULT_FORMAT.to_string(),
            text_visible: true,
            inverted_appearance: false,
            orientation: Orientation::Horizontal,
            alignment: Alignment::LEFT,
            text_direction: ProgressTextDirection::TopToBottom,
            busy_phase: 0.0,
            font: Font::new("Segoe UI", 12.0),
            track_color: Color::from_rgba8(230, 230, 230, 255),
            chunk_color: Color::from_rgba8(6, 176, 37, 255),
            border_color: Color::from_rgba8(188, 188, 188, 255),
            text_color: Color::from_rgba8(20, 20, 20, 255),
            value_changed,
        }
    }

    pub fn minimum(&self) -> i32 {
        self.minimum
    }

    pub fn maximum(&self) -> i32 {
        self.maximum
    }

    pub fn value(&self) -> i32 {
        self.value
    }

    pub fn set_minimum(&mut self, minimum: i32) {
        let max = self.maximum.max(minimum);
        self.set_range(minimum, max);
    }

    pub fn set_maximum(&mut self, maximum: i32) {
        let min = self.minimum.min(maximum);
        self.set_range(min, maximum);
    }

    /// Sets the range (`maximum` raised to `minimum` if smaller); resets if the value falls outside.
    pub fn set_range(&mut self, minimum: i32, maximum: i32) {
        if minimum == self.minimum && maximum == self.maximum {
            return;
        }
        self.minimum = minimum;
        self.maximum = maximum.max(minimum);
        if i64::from(self.value) < i64::from(self.minimum) - 1 || self.value > self.maximum {
            self.reset();
        } else {
            self.update();
        }
    }

    /// Sets the value; values outside `[minimum, maximum]` are ignored unless the range is `0..=0`.
    pub fn set_value(&mut self, value: i32) {
        let out_of_range = value > self.maximum || value < self.minimum;
        if self.value == value || (out_of_range && (self.maximum != 0 || self.minimum != 0)) {
            return;
        }
        self.value = value;
        self.value_changed.emit(&value);
        self.update();
    }

    /// Rewinds to "no progress": `minimum - 1` (or `i32::MIN` when minimum is `i32::MIN`).
    pub fn reset(&mut self) {
        self.value = if self.minimum == i32::MIN {
            i32::MIN
        } else {
            self.minimum - 1
        };
        self.update();
    }

    /// Busy (indeterminate) mode is active whenever `minimum == maximum`.
    pub fn is_busy(&self) -> bool {
        self.minimum == self.maximum
    }

    pub fn format(&self) -> &str {
        &self.format
    }

    /// Sets the text format; `%p` = percentage, `%v` = value, `%m` = total steps.
    pub fn set_format(&mut self, format: impl Into<String>) {
        let format = format.into();
        if self.format != format {
            self.format = format;
            self.update();
        }
    }

    pub fn reset_format(&mut self) {
        self.set_format(DEFAULT_FORMAT);
    }

    pub fn is_text_visible(&self) -> bool {
        self.text_visible
    }

    pub fn set_text_visible(&mut self, visible: bool) {
        if self.text_visible != visible {
            self.text_visible = visible;
            self.update();
        }
    }

    pub fn inverted_appearance(&self) -> bool {
        self.inverted_appearance
    }

    pub fn set_inverted_appearance(&mut self, inverted: bool) {
        self.inverted_appearance = inverted;
        self.update();
    }

    pub fn orientation(&self) -> Orientation {
        self.orientation
    }

    pub fn set_orientation(&mut self, orientation: Orientation) {
        if self.orientation == orientation {
            return;
        }
        self.orientation = orientation;
        self.base
            .set_size_policy(transposed_policy(self.base.size_policy()));
        let g = self.base.geometry();
        self.base
            .set_geometry(Rect::new(g.x, g.y, g.height, g.width));
        self.update();
    }

    pub fn alignment(&self) -> Alignment {
        self.alignment
    }

    pub fn set_alignment(&mut self, alignment: Alignment) {
        self.alignment = alignment;
        self.update();
    }

    pub fn text_direction(&self) -> ProgressTextDirection {
        self.text_direction
    }

    pub fn set_text_direction(&mut self, direction: ProgressTextDirection) {
        self.text_direction = direction;
        self.update();
    }

    pub fn font(&self) -> &Font {
        &self.font
    }

    pub fn set_font(&mut self, font: Font) {
        self.font = font;
        // FontChange (qwidget.cpp:4894-4895) precedes the update work (qwidget.cpp:9502).
        notify_borrowed(self, &mut Event::new(EventKind::FontChange));
        self.update_geometry();
        self.update();
    }

    pub fn set_chunk_color(&mut self, color: Color) {
        self.chunk_color = color;
        self.update();
    }

    /// Formatted progress text (`QProgressBar::text`); empty when there is no progress.
    pub fn text(&self) -> String {
        if (self.maximum == 0 && self.minimum == 0)
            || self.value < self.minimum
            || (self.value == i32::MIN && self.minimum == i32::MIN)
        {
            return String::new();
        }
        let total_steps = i64::from(self.maximum) - i64::from(self.minimum);
        let percent = if total_steps == 0 {
            100
        } else {
            ((i64::from(self.value) - i64::from(self.minimum)) as f64 * 100.0 / total_steps as f64)
                as i64
        };
        let mut out = String::with_capacity(self.format.len() + 8);
        let mut chars = self.format.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '%' {
                match chars.peek() {
                    Some('p') => {
                        chars.next();
                        out.push_str(&percent.to_string());
                        continue;
                    }
                    Some('v') => {
                        chars.next();
                        out.push_str(&self.value.to_string());
                        continue;
                    }
                    Some('m') => {
                        chars.next();
                        out.push_str(&total_steps.to_string());
                        continue;
                    }
                    _ => {}
                }
            }
            out.push(c);
        }
        out
    }

    /// Fraction of the bar that is filled, in `[0, 1]` (0 for busy mode and "no progress").
    pub fn progress_fraction(&self) -> f32 {
        if self.is_busy() || self.value < self.minimum {
            return 0.0;
        }
        let total = i64::from(self.maximum) - i64::from(self.minimum);
        ((i64::from(self.value) - i64::from(self.minimum)) as f64 / total as f64).clamp(0.0, 1.0)
            as f32
    }

    /// Current busy animation phase in `[0, 1)`.
    pub fn busy_phase(&self) -> f32 {
        self.busy_phase
    }

    /// Advances the busy-indicator animation by `delta` (fraction of one sweep).
    ///
    /// Does nothing unless the bar is busy; drive it from a timer (~30 ms ticks with delta ≈ 0.02).
    pub fn advance_busy_indicator(&mut self, delta: f32) {
        if !self.is_busy() {
            return;
        }
        self.busy_phase = (self.busy_phase + delta).rem_euclid(1.0);
        self.update();
    }

    /// Filled chunk rectangle(s) along the bar (widget-local), for determinate or busy mode.
    fn chunk_rects(&self, inner: RectF) -> Vec<RectF> {
        let horizontal = self.orientation == Orientation::Horizontal;
        let length = if horizontal {
            inner.width
        } else {
            inner.height
        };
        // Horizontal bars fill from the left, vertical ones from the bottom; inversion flips it.
        let from_start = horizontal != self.inverted_appearance;
        let segment = |start: f32, len: f32| {
            let start = if from_start {
                start
            } else {
                length - start - len
            };
            if horizontal {
                RectF::new(inner.x + start, inner.y, len, inner.height)
            } else {
                // Offset measured from the bottom edge.
                RectF::new(
                    inner.x,
                    inner.y + inner.height - start - len,
                    inner.width,
                    len,
                )
            }
        };
        if self.is_busy() {
            let chunk = length * 0.3;
            let start = self.busy_phase * (length + chunk) - chunk;
            let clipped_start = start.max(0.0);
            let clipped_end = (start + chunk).min(length);
            if clipped_end > clipped_start {
                return vec![segment(clipped_start, clipped_end - clipped_start)];
            }
            return Vec::new();
        }
        let filled = length * self.progress_fraction();
        if filled > 0.0 {
            vec![segment(0.0, filled)]
        } else {
            Vec::new()
        }
    }
    fn styled_font(&self, style: &crate::style::stylesheet::ResolvedStyle) -> Font {
        let mut font = self.font.clone();
        if let Some(sz) = style.font_size {
            // `QCss` applies `font-size: Npx` with `QFont::setPixelSize(int)`.
            font.size = sz.round();
        }
        if let Some(fam) = style.font_family() {
            font.family = fam;
        }
        font
    }

    pub fn resolved_groove_style(&self) -> crate::style::stylesheet::ResolvedStyle {
        let mut buf = [""; 4];
        let props = self.base.properties.borrow();
        let ctx = crate::style::stylesheet::WidgetStyleContext {
            type_name: "QProgressBar",
            object_name: self.base.object_data.object_name().unwrap_or(""),
            pseudo_states: self.base.style_pseudo_states(false, false, &mut buf),
            sub_control: None,
            attributes: props.as_slice(),
        };
        self.base.resolve_style(&ctx)
    }

    pub fn resolved_chunk_style(&self) -> crate::style::stylesheet::ResolvedStyle {
        let mut buf = [""; 4];
        let props = self.base.properties.borrow();
        let ctx = crate::style::stylesheet::WidgetStyleContext {
            type_name: "QProgressBar",
            object_name: self.base.object_data.object_name().unwrap_or(""),
            pseudo_states: self.base.style_pseudo_states(false, false, &mut buf),
            sub_control: Some("chunk"),
            attributes: props.as_slice(),
        };
        self.base.resolve_style(&ctx)
    }
}

impl Default for ProgressBar {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for ProgressBar {
    fn drop(&mut self) {
        // Registry cleanup only touches thread-locals through `try_with`.
        // SAFETY: `unregister_qobject` needs this memory to stay valid until every running
        // callback of the bar has returned. A bar can be dropped while one of its callbacks runs
        // (from one of its own slots, or with its parent), and what makes that sound depends on
        // who registered it:
        // - an `OwnedObject` (`register_owned_qobject`, `add_owned_child`) unregisters the bar
        //   when it is dropped and parks the allocation until the running callback returns, so
        //   this drop runs after the callback, and `unregister_qobject` finds nothing left to do;
        // - a bar registered with `register_qobject_ptr` (or the deprecated functions) is dropped
        //   by an owner whose registration contract forbids dropping it during a callback of it,
        //   on the registration thread;
        // - a bar that was never registered has nothing to unregister.
        // `test_progress_bar_drop_in_callback` runs the first case, and shows by a Miri control
        // that the second must not be dropped in a callback.
        unsafe { ferrokit_core::object::unregister_qobject(self.base.object_data.id()) };
    }
}

impl QObject for ProgressBar {
    crate::widget::widget_object_properties!();

    leaf_qobject_common!();

    fn event(&mut self, event: &mut Event) -> bool {
        input_common::dispatch_input_event(self, event)
    }
}

impl Widget for ProgressBar {
    leaf_widget_common!();

    /// `QProgressBar::sizeHint` through `QStyleSheetStyle::sizeFromContents(CT_ProgressBar)`
    /// (`qprogressbar.cpp:396-406`, `qstylesheetstyle.cpp:5311-5320,5485-5490`): the content size is
    /// transposed for a vertical bar, clamped to `max-*` and expanded to `min-*` (both content-box,
    /// `QRenderRule::adjustSize`), then the border and padding are added.
    fn size_hint(&self) -> Size {
        let style = self.resolved_groove_style();
        let font = self.styled_font(&style);
        let metrics = FontMetrics::from_font(&font);
        let chunk = self.resolved_chunk_style().width.unwrap_or(0).max(9);
        let zero = metrics.horizontal_advance_exact("0", &font).round() as i32;
        let (mut w, mut h) = (
            chunk * 7 + zero * 4,
            FontMetrics::layout_height(&font).ceil() as i32 + 8,
        );
        if self.orientation == Orientation::Vertical {
            std::mem::swap(&mut w, &mut h);
        }
        (w, h) = style.adjust_size((w, h));
        // `CT_ProgressBar` with a contents size: `rule.size(sz)` takes `width`/`height` back
        // over the adjusted size (`qstylesheetstyle.cpp:5487-5489`).
        w = style.width.unwrap_or(w);
        h = style.height.unwrap_or(h);
        let (ex, ey) = style.box_extra();
        Size::new(w + ex, h + ey)
    }

    fn minimum_size(&self) -> Size {
        let style = self.resolved_groove_style();
        // QSS `min-*` are widget coordinates; Qt does not transpose them for a vertical bar.
        let (w, h) = style.min_box_size();
        Size::new(w, h)
    }

    /// `QProgressBar::minimumSizeHint` (`qprogressbar.cpp:411-418`): the styled `sizeHint` along the
    /// bar, `fontMetrics().height() + 2` across it.
    fn minimum_size_hint(&self) -> Size {
        let hint = self.size_hint();
        let font = self.styled_font(&self.resolved_groove_style());
        let across = FontMetrics::layout_height(&font).ceil() as i32 + 2;
        match self.orientation {
            Orientation::Horizontal => Size::new(hint.width, across),
            Orientation::Vertical => Size::new(across, hint.height),
        }
    }

    fn maximum_size(&self) -> Size {
        let style = self.resolved_groove_style();
        // QSS `max-*` are widget coordinates; Qt does not transpose them for a vertical bar.
        let (w, h) = style.max_box_size();
        Size::new(w, h)
    }

    fn update(&self) {
        self.base.update();
    }

    fn focus_in_event(&mut self, _reason: FocusReason) {}

    fn focus_out_event(&mut self, _reason: FocusReason) {}

    fn paint_event(&mut self, painter: &mut Painter) {
        let g = self.base.geometry();
        let w = g.width as f32;
        let h = g.height as f32;
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let frame = RectF::new(0.0, 0.0, w, h);
        let groove_style = self.resolved_groove_style();
        let chunk_style = self.resolved_chunk_style();

        let track_bg = groove_style.background_color.unwrap_or(self.track_color);
        let groove_radius = groove_style.border_radius.unwrap_or(3.0);
        let border_pen = if let Some(bw) = groove_style.border_width {
            if bw <= 0.0 || groove_style.border_style.as_deref() == Some("none") {
                None
            } else {
                Some(Pen::new(
                    groove_style.border_color.unwrap_or(self.border_color),
                    bw,
                ))
            }
        } else {
            Some(Pen::new(self.border_color, 1.0))
        };

        // 1. Track
        // Groove and chunks are anti-aliased (`qstylesheetstyle.cpp:4175-4190`, `qfusionstyle.cpp:1118-1142`).
        painter.set_render_hint(RenderHint::Antialiasing, true);
        painter.set_brush(Brush::Color(track_bg));
        painter.set_pen(border_pen);
        painter.draw_rounded_rect(frame, groove_radius, groove_radius);

        // 2. Chunk(s)
        let chunk_bg = if self.base.is_enabled() {
            chunk_style.background_color.unwrap_or(self.chunk_color)
        } else {
            self.border_color
        };
        let chunk_radius = chunk_style.border_radius.unwrap_or(2.0);
        let inner = RectF::new(0.0, 0.0, w, h);
        painter.set_pen(None);
        painter.set_brush(Brush::Color(chunk_bg));
        for chunk in self.chunk_rects(inner) {
            painter.draw_rounded_rect(chunk, chunk_radius, chunk_radius);
        }

        // 3. Text (never drawn in busy mode, matching QStyle's indeterminate rendering)
        if !self.text_visible || self.is_busy() {
            return;
        }
        let text = self.text();
        if text.is_empty() {
            return;
        }
        let metrics = FontMetrics::from_font(&self.font);
        let text_w = metrics.horizontal_advance(&text, &self.font);
        painter.set_pen(Pen::new(self.text_color, 1.0));
        match self.orientation {
            Orientation::Horizontal => {
                // The aligned offset is not clamped (qpainter.cpp:7426-7429); overflow is clipped
                // to the bar rect (qpainter.cpp:7442-7446).
                let x = if self.alignment.contains(Alignment::RIGHT) {
                    w - text_w - 4.0
                } else if self.alignment.contains(Alignment::H_CENTER) {
                    (w - text_w) / 2.0
                } else {
                    4.0
                };
                let top = (h - metrics.height) / 2.0;
                let overflow = x < 0.0 || top < 0.0 || x + text_w > w || top + metrics.height > h;
                if overflow {
                    painter.save();
                    painter.clip_to_rect(RectF::new(0.0, 0.0, w, h));
                }
                painter.draw_text(PointF::new(x, top + metrics.ascent), &text, &self.font);
                if overflow {
                    painter.restore();
                }
            }
            Orientation::Vertical => {
                // Rotate so the text runs along the bar.
                painter.save();
                let (angle, tx, ty) = match self.text_direction {
                    ProgressTextDirection::TopToBottom => (90.0, w, 0.0),
                    ProgressTextDirection::BottomToTop => (-90.0, 0.0, h),
                };
                painter.translate(tx, ty);
                painter.rotate(angle);
                // In the rotated space the bar is h wide and w high.
                let x = (h - text_w) / 2.0;
                let top = (w - metrics.height) / 2.0;
                let overflow = x < 0.0 || top < 0.0 || x + text_w > h || top + metrics.height > w;
                if overflow {
                    painter.save();
                    painter.clip_to_rect(RectF::new(0.0, 0.0, h, w));
                }
                painter.draw_text(PointF::new(x, top + metrics.ascent), &text, &self.font);
                if overflow {
                    painter.restore();
                }
                painter.restore();
            }
        }
    }
}
