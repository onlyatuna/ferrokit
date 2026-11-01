// SPDX-License-Identifier: LGPL-3.0-only
//! Widget drawing policy and style metrics.
//!
//! Widgets pass immutable state options to a style, keeping state and input handling
//! in the widget while allowing applications to replace rendering policy.

pub mod stylesheet;
pub use stylesheet::*;

use ferrokit_gui::geometry::primitives::{RectF, Size};
use ferrokit_gui::paint::qss_box::{QssBorder, draw_qss_box};
use ferrokit_gui::paint::{Brush, Painter, Pen, RenderHint};
use ferrokit_gui::text::Font;
use ferrokit_gui::tiny_skia::Color;

use crate::button::ButtonState;

/// Common metrics queried by widgets when calculating geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StyleMetrics {
    pub button_horizontal_padding: i32,
    pub button_vertical_padding: i32,
    pub indicator_size: i32,
    pub menu_item_height: i32,
}

impl Default for StyleMetrics {
    fn default() -> Self {
        Self {
            button_horizontal_padding: 24,
            button_vertical_padding: 14,
            indicator_size: 16,
            menu_item_height: 24,
        }
    }
}

/// Non-geometric interaction hints supplied by a style.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StyleHints {
    pub keyboard_focus_change_on_tab: bool,
    pub underline_mnemonics: bool,
}

impl Default for StyleHints {
    fn default() -> Self {
        Self {
            keyboard_focus_change_on_tab: true,
            underline_mnemonics: true,
        }
    }
}

/// Button state and presentation supplied to [`Style::draw_button`].
pub struct ButtonStyleOption<'a> {
    pub rect: RectF,
    pub text: &'a str,
    pub font: &'a Font,
    pub state: ButtonState,
    pub enabled: bool,
    pub focused: bool,
    pub normal_color: Color,
    pub hover_color: Color,
    pub pressed_color: Color,
    pub text_color: Color,
    pub border_color: Color,
    pub border_radius: f32,
    /// Width of the border a style sheet gives the button (`border: Npx solid ...`). `Some` draws the
    /// box like `QStyleSheetStyle` (`QRenderRule::drawBackground` + `drawBorder`); `None` is the
    /// style's own frame.
    pub border_width: Option<f32>,
}

/// Widget-specific rendering policy.
pub trait Style {
    fn metrics(&self) -> StyleMetrics {
        StyleMetrics::default()
    }

    fn hints(&self) -> StyleHints {
        StyleHints::default()
    }

    fn size_from_contents(
        &self,
        contents: Size,
        horizontal_padding: i32,
        vertical_padding: i32,
    ) -> Size {
        Size::new(
            contents.width + horizontal_padding,
            contents.height + vertical_padding,
        )
    }

    fn draw_button(&self, painter: &mut Painter, option: &ButtonStyleOption<'_>);
}

/// Default cross-platform style used by widgets unless replaced by the application.
#[derive(Debug, Default, Clone, Copy)]
pub struct DefaultStyle;

impl Style for DefaultStyle {
    fn draw_button(&self, painter: &mut Painter, option: &ButtonStyleOption<'_>) {
        let mut background = match option.state {
            ButtonState::Normal => option.normal_color,
            ButtonState::Hovered => option.hover_color,
            ButtonState::Pressed => option.pressed_color,
        };
        if !option.enabled {
            background = Color::from_rgba8(232, 232, 232, 255);
        }

        if let Some(width) = option.border_width {
            draw_qss_box(
                painter,
                option.rect,
                Some(background),
                Some(QssBorder {
                    width,
                    color: option.border_color,
                }),
                option.border_radius,
            );
        } else {
            // Button frame and focus ring are anti-aliased (`qfusionstyle.cpp:679-754`).
            painter.set_render_hint(RenderHint::Antialiasing, true);
            painter.set_brush(Brush::Color(background));
            painter.set_pen(Pen::new(option.border_color, 1.0));
            painter.draw_rounded_rect(option.rect, option.border_radius, option.border_radius);
        }

        if option.focused {
            let focus_rect = option.rect.adjusted(2.0, 2.0, -2.0, -2.0);
            painter.set_brush(Brush::Color(Color::TRANSPARENT));
            painter.set_pen(Pen::new(Color::from_rgba8(0, 120, 215, 200), 1.5));
            painter.draw_rounded_rect(
                focus_rect,
                (option.border_radius - 1.0).max(1.0),
                (option.border_radius - 1.0).max(1.0),
            );
        }

        if !option.text.is_empty() {
            let metrics = ferrokit_gui::text::FontMetrics::from_font(option.font);
            let text_width = metrics.horizontal_advance(option.text, option.font);
            let x = option.rect.x + ((option.rect.width - text_width) / 2.0).max(0.0);
            let y = option.rect.y
                + ((option.rect.height - metrics.height) / 2.0).max(0.0)
                + metrics.ascent;
            let text_color = if option.enabled {
                option.text_color
            } else {
                Color::from_rgba8(150, 150, 150, 255)
            };
            painter.set_pen(Pen::new(text_color, 1.0));
            painter.draw_text(
                ferrokit_gui::geometry::primitives::PointF::new(x, y),
                option.text,
                option.font,
            );
        }
    }
}
