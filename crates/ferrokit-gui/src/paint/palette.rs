// SPDX-License-Identifier: LGPL-3.0-only
//! Palette and Color management (`QPalette` equivalent).
//!
//! Provides color group management (Active, Inactive, Disabled) and color roles
//! (Window, WindowText, Base, Button, Highlight, etc.) for systemic UI theming.

use std::collections::HashMap;
use tiny_skia::Color;

/// Color groups defining the interaction state of UI widgets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColorGroup {
    /// Active state when the window has focus.
    Active,
    /// Inactive state when the window does not have focus.
    Inactive,
    /// Disabled state when the widget does not accept user input.
    Disabled,
}

/// Color roles defining the semantic meaning of colors in widgets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColorRole {
    /// General background color for windows.
    Window,
    /// General foreground color for window titles and text.
    WindowText,
    /// Background color for text edit controls, list views, etc.
    Base,
    /// Background of every second row in views that stripe their rows.
    AlternateBase,
    /// Background for tooltips.
    ToolTipBase,
    /// Foreground for tooltips.
    ToolTipText,
    /// Foreground color used with Base.
    Text,
    /// General button background color.
    Button,
    /// Foreground color used with Button.
    ButtonText,
    /// Text color that contrasts strongly with Button.
    BrightText,
    /// Highlight color used to indicate selection.
    Highlight,
    /// Text color used on selected items.
    HighlightedText,
    /// Text color used for unvisited hyperlinks.
    Link,
    /// Text color used for visited hyperlinks.
    LinkVisited,
    /// Dark shade for 3D bevels.
    Dark,
    /// Medium shade for 3D bevels.
    Mid,
    /// Light shade for 3D bevels.
    Light,
    /// Between Light and Mid.
    Midlight,
    /// Very dark shadow.
    Shadow,
    /// Placeholder text color.
    PlaceholderText,
    /// Accent color for modern UI components.
    Accent,
}

/// Palette holding role-to-color mappings per color group.
#[derive(Debug, Clone, PartialEq)]
pub struct Palette {
    colors: HashMap<(ColorGroup, ColorRole), Color>,
}

impl Default for Palette {
    fn default() -> Self {
        Self::dark()
    }
}

impl Palette {
    /// Creates an empty palette.
    pub fn new() -> Self {
        Self {
            colors: HashMap::new(),
        }
    }

    /// Sets the color for a specific role across all color groups.
    pub fn set_color_for_all(&mut self, role: ColorRole, color: Color) {
        self.set_color(ColorGroup::Active, role, color);
        self.set_color(ColorGroup::Inactive, role, color);
        self.set_color(ColorGroup::Disabled, role, color);
    }

    /// Sets the color for a specific group and role.
    pub fn set_color(&mut self, group: ColorGroup, role: ColorRole, color: Color) {
        self.colors.insert((group, role), color);
    }

    /// Resolves the color for a specific group and role.
    pub fn color(&self, group: ColorGroup, role: ColorRole) -> Color {
        if let Some(c) = self.colors.get(&(group, role)) {
            return *c;
        }
        // Fall back to Active group if specified group not set
        if group != ColorGroup::Active {
            if let Some(c) = self.colors.get(&(ColorGroup::Active, role)) {
                return *c;
            }
        }
        // Fallback default
        Color::BLACK
    }

    /// Convenience helper for active window background.
    pub fn window(&self) -> Color {
        self.color(ColorGroup::Active, ColorRole::Window)
    }

    /// Convenience helper for active window text.
    pub fn window_text(&self) -> Color {
        self.color(ColorGroup::Active, ColorRole::WindowText)
    }

    /// Convenience helper for active base background.
    pub fn base(&self) -> Color {
        self.color(ColorGroup::Active, ColorRole::Base)
    }

    /// Convenience helper for active text.
    pub fn text(&self) -> Color {
        self.color(ColorGroup::Active, ColorRole::Text)
    }

    /// Convenience helper for active button.
    pub fn button(&self) -> Color {
        self.color(ColorGroup::Active, ColorRole::Button)
    }

    /// Convenience helper for active button text.
    pub fn button_text(&self) -> Color {
        self.color(ColorGroup::Active, ColorRole::ButtonText)
    }

    /// Convenience helper for active highlight.
    pub fn highlight(&self) -> Color {
        self.color(ColorGroup::Active, ColorRole::Highlight)
    }

    /// Convenience helper for active highlighted text.
    pub fn highlighted_text(&self) -> Color {
        self.color(ColorGroup::Active, ColorRole::HighlightedText)
    }

    /// Creates a modern dark theme palette (overlay / game / pro UI default).
    pub fn dark() -> Self {
        let mut p = Self::new();
        let bg = Color::from_rgba8(30, 30, 30, 255);
        let base = Color::from_rgba8(40, 40, 40, 255);
        let alt_base = Color::from_rgba8(50, 50, 50, 255);
        let fg = Color::from_rgba8(220, 220, 220, 255);
        let btn = Color::from_rgba8(55, 55, 60, 255);
        let btn_txt = Color::from_rgba8(240, 240, 240, 255);
        let highlight = Color::from_rgba8(0, 122, 255, 255);
        let h_txt = Color::from_rgba8(255, 255, 255, 255);
        let link = Color::from_rgba8(64, 156, 255, 255);
        let disabled_fg = Color::from_rgba8(120, 120, 120, 255);

        for group in [ColorGroup::Active, ColorGroup::Inactive] {
            p.set_color(group, ColorRole::Window, bg);
            p.set_color(group, ColorRole::WindowText, fg);
            p.set_color(group, ColorRole::Base, base);
            p.set_color(group, ColorRole::AlternateBase, alt_base);
            p.set_color(
                group,
                ColorRole::ToolTipBase,
                Color::from_rgba8(20, 20, 20, 240),
            );
            p.set_color(group, ColorRole::ToolTipText, Color::WHITE);
            p.set_color(group, ColorRole::Text, fg);
            p.set_color(group, ColorRole::Button, btn);
            p.set_color(group, ColorRole::ButtonText, btn_txt);
            p.set_color(group, ColorRole::BrightText, Color::WHITE);
            p.set_color(group, ColorRole::Highlight, highlight);
            p.set_color(group, ColorRole::HighlightedText, h_txt);
            p.set_color(group, ColorRole::Link, link);
            p.set_color(
                group,
                ColorRole::LinkVisited,
                Color::from_rgba8(180, 120, 240, 255),
            );
            p.set_color(group, ColorRole::Dark, Color::from_rgba8(20, 20, 20, 255));
            p.set_color(group, ColorRole::Mid, Color::from_rgba8(70, 70, 70, 255));
            p.set_color(group, ColorRole::Light, Color::from_rgba8(90, 90, 90, 255));
            p.set_color(
                group,
                ColorRole::Midlight,
                Color::from_rgba8(80, 80, 80, 255),
            );
            p.set_color(group, ColorRole::Shadow, Color::from_rgba8(10, 10, 10, 255));
            p.set_color(
                group,
                ColorRole::PlaceholderText,
                Color::from_rgba8(140, 140, 140, 255),
            );
            p.set_color(
                group,
                ColorRole::Accent,
                Color::from_rgba8(0, 150, 255, 255),
            );
        }

        // Disabled overrides
        p.set_color(ColorGroup::Disabled, ColorRole::Window, bg);
        p.set_color(ColorGroup::Disabled, ColorRole::WindowText, disabled_fg);
        p.set_color(ColorGroup::Disabled, ColorRole::Base, base);
        p.set_color(ColorGroup::Disabled, ColorRole::Text, disabled_fg);
        p.set_color(
            ColorGroup::Disabled,
            ColorRole::Button,
            Color::from_rgba8(45, 45, 48, 255),
        );
        p.set_color(ColorGroup::Disabled, ColorRole::ButtonText, disabled_fg);
        p.set_color(
            ColorGroup::Disabled,
            ColorRole::Highlight,
            Color::from_rgba8(80, 80, 80, 255),
        );
        p.set_color(
            ColorGroup::Disabled,
            ColorRole::HighlightedText,
            disabled_fg,
        );

        p
    }

    /// Creates a classic light theme palette.
    pub fn light() -> Self {
        let mut p = Self::new();
        let bg = Color::from_rgba8(240, 240, 240, 255);
        let base = Color::from_rgba8(255, 255, 255, 255);
        let alt_base = Color::from_rgba8(245, 245, 245, 255);
        let fg = Color::from_rgba8(20, 20, 20, 255);
        let btn = Color::from_rgba8(230, 230, 230, 255);
        let btn_txt = Color::from_rgba8(20, 20, 20, 255);
        let highlight = Color::from_rgba8(0, 120, 215, 255);
        let h_txt = Color::from_rgba8(255, 255, 255, 255);
        let link = Color::from_rgba8(0, 102, 204, 255);
        let disabled_fg = Color::from_rgba8(160, 160, 160, 255);

        for group in [ColorGroup::Active, ColorGroup::Inactive] {
            p.set_color(group, ColorRole::Window, bg);
            p.set_color(group, ColorRole::WindowText, fg);
            p.set_color(group, ColorRole::Base, base);
            p.set_color(group, ColorRole::AlternateBase, alt_base);
            p.set_color(
                group,
                ColorRole::ToolTipBase,
                Color::from_rgba8(255, 255, 220, 255),
            );
            p.set_color(group, ColorRole::ToolTipText, Color::BLACK);
            p.set_color(group, ColorRole::Text, fg);
            p.set_color(group, ColorRole::Button, btn);
            p.set_color(group, ColorRole::ButtonText, btn_txt);
            p.set_color(group, ColorRole::BrightText, Color::WHITE);
            p.set_color(group, ColorRole::Highlight, highlight);
            p.set_color(group, ColorRole::HighlightedText, h_txt);
            p.set_color(group, ColorRole::Link, link);
            p.set_color(
                group,
                ColorRole::LinkVisited,
                Color::from_rgba8(128, 0, 128, 255),
            );
            p.set_color(
                group,
                ColorRole::Dark,
                Color::from_rgba8(160, 160, 160, 255),
            );
            p.set_color(group, ColorRole::Mid, Color::from_rgba8(180, 180, 180, 255));
            p.set_color(
                group,
                ColorRole::Light,
                Color::from_rgba8(255, 255, 255, 255),
            );
            p.set_color(
                group,
                ColorRole::Midlight,
                Color::from_rgba8(220, 220, 220, 255),
            );
            p.set_color(
                group,
                ColorRole::Shadow,
                Color::from_rgba8(105, 105, 105, 255),
            );
            p.set_color(
                group,
                ColorRole::PlaceholderText,
                Color::from_rgba8(120, 120, 120, 255),
            );
            p.set_color(
                group,
                ColorRole::Accent,
                Color::from_rgba8(0, 120, 215, 255),
            );
        }

        p.set_color(ColorGroup::Disabled, ColorRole::Window, bg);
        p.set_color(ColorGroup::Disabled, ColorRole::WindowText, disabled_fg);
        p.set_color(ColorGroup::Disabled, ColorRole::Base, base);
        p.set_color(ColorGroup::Disabled, ColorRole::Text, disabled_fg);
        p.set_color(
            ColorGroup::Disabled,
            ColorRole::Button,
            Color::from_rgba8(220, 220, 220, 255),
        );
        p.set_color(ColorGroup::Disabled, ColorRole::ButtonText, disabled_fg);
        p.set_color(
            ColorGroup::Disabled,
            ColorRole::Highlight,
            Color::from_rgba8(190, 190, 190, 255),
        );
        p.set_color(
            ColorGroup::Disabled,
            ColorRole::HighlightedText,
            disabled_fg,
        );

        p
    }
}
