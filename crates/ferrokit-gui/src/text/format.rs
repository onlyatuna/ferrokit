// SPDX-License-Identifier: LGPL-3.0-only
//! Qt-equivalent rich text format hierarchy (`QTextFormat` family).
//!
//! Provides granular formatting definitions:
//! - `TextCharFormat` (`QTextCharFormat`): Font, size, weight, styles, foreground/background colors, underlines.
//! - `TextBlockFormat` (`QTextBlockFormat`): Paragraph alignment, margins, first-line indent, line height, heading level.
//! - `TextListFormat` (`QTextListFormat`): Bullet / numbered list indentation and styling.
//! - `TextFrameFormat` (`QTextFrameFormat`): Container frame border, padding, and margins.
//! - `FormatRange`: Range-based formatting override for syntax highlighting and selection rendering.

use crate::alignment::Alignment;
use crate::text::font::{Font, FontStyle, FontWeight};
use tiny_skia::Color;

/// Character vertical alignment relative to the baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum VerticalAlignment {
    /// Normal baseline alignment.
    #[default]
    AlignNormal,
    /// Superscript (e.g. exponent, x²).
    AlignSuperScript,
    /// Subscript (e.g. chemical formula, H₂O).
    AlignSubScript,
    /// Middle vertical alignment.
    AlignMiddle,
    /// Top vertical alignment.
    AlignTop,
    /// Bottom vertical alignment.
    AlignBottom,
}

/// Underline drawing style (`QTextCharFormat::UnderlineStyle`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum UnderlineStyle {
    #[default]
    NoUnderline,
    SingleUnderline,
    DashUnderline,
    DotLine,
    WaveUnderline,
    SpellCheckUnderline,
}

/// List numbering or bullet style (`QTextListFormat::Style`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ListStyle {
    #[default]
    Disc,
    Circle,
    Square,
    Decimal,
    LowerAlpha,
    UpperAlpha,
    LowerRoman,
    UpperRoman,
}

/// Character formatting attributes (`QTextCharFormat` equivalent).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TextCharFormat {
    /// Explicit font family override.
    pub font_family: Option<String>,
    /// Font size in points.
    pub font_point_size: Option<f32>,
    /// Font weight.
    pub font_weight: Option<FontWeight>,
    /// Italic or normal style.
    pub font_italic: Option<bool>,
    /// Whether text is underlined.
    pub font_underline: bool,
    /// Underline styling.
    pub underline_style: UnderlineStyle,
    /// Specific underline color (defaults to foreground if None).
    pub underline_color: Option<Color>,
    /// Strikeout / line-through line.
    pub font_strikeout: bool,
    /// Overline line above text.
    pub font_overline: bool,
    /// Text foreground color.
    pub foreground: Option<Color>,
    /// Text background highlight color.
    pub background: Option<Color>,
    /// Vertical alignment (normal, super, sub).
    pub vertical_alignment: VerticalAlignment,
    /// Hyperlink target URL (if this fragment is an anchor).
    pub anchor_href: Option<String>,
    /// Named anchor target (`<a name="…">` / `id="…"`), used for in-document navigation.
    pub anchor_name: Option<String>,
    /// Tooltip text.
    pub tool_tip: Option<String>,
}

impl TextCharFormat {
    /// Creates a default, unstyled character format.
    pub fn new() -> Self {
        Self::default()
    }

    /// Merges properties from `other` into `self`.
    /// Specified properties in `other` overwrite properties in `self`.
    pub fn merge(&mut self, other: &Self) {
        if other.font_family.is_some() {
            self.font_family = other.font_family.clone();
        }
        if other.font_point_size.is_some() {
            self.font_point_size = other.font_point_size;
        }
        if other.font_weight.is_some() {
            self.font_weight = other.font_weight;
        }
        if other.font_italic.is_some() {
            self.font_italic = other.font_italic;
        }
        if other.font_underline {
            self.font_underline = true;
            self.underline_style = other.underline_style;
            self.underline_color = other.underline_color;
        }
        if other.font_strikeout {
            self.font_strikeout = true;
        }
        if other.font_overline {
            self.font_overline = true;
        }
        if other.foreground.is_some() {
            self.foreground = other.foreground;
        }
        if other.background.is_some() {
            self.background = other.background;
        }
        if other.vertical_alignment != VerticalAlignment::AlignNormal {
            self.vertical_alignment = other.vertical_alignment;
        }
        if other.anchor_href.is_some() {
            self.anchor_href = other.anchor_href.clone();
        }
        if other.anchor_name.is_some() {
            self.anchor_name = other.anchor_name.clone();
        }
        if other.tool_tip.is_some() {
            self.tool_tip = other.tool_tip.clone();
        }
    }

    /// Resolves an actual `Font` given a base fallback font.
    pub fn to_font(&self, base: &Font) -> Font {
        let family = self.font_family.as_deref().unwrap_or(base.family());
        let size = self.font_point_size.unwrap_or(base.size());
        let weight = self.font_weight.unwrap_or(base.weight());
        let style = if self
            .font_italic
            .unwrap_or(base.style() == FontStyle::Italic)
        {
            FontStyle::Italic
        } else {
            FontStyle::Normal
        };

        let mut font = Font::new(family, size);
        font.set_weight(weight);
        font.set_style(style);
        font
    }

    /// Convenience builder methods
    pub fn with_font_weight(mut self, weight: FontWeight) -> Self {
        self.font_weight = Some(weight);
        self
    }

    pub fn with_italic(mut self, italic: bool) -> Self {
        self.font_italic = Some(italic);
        self
    }

    pub fn with_underline(mut self, underline: bool) -> Self {
        self.font_underline = underline;
        if underline && self.underline_style == UnderlineStyle::NoUnderline {
            self.underline_style = UnderlineStyle::SingleUnderline;
        }
        self
    }

    pub fn with_foreground(mut self, color: Color) -> Self {
        self.foreground = Some(color);
        self
    }

    pub fn with_background(mut self, color: Color) -> Self {
        self.background = Some(color);
        self
    }

    pub fn is_anchor(&self) -> bool {
        self.anchor_href.is_some()
    }
}

/// Block (paragraph) formatting attributes (`QTextBlockFormat` equivalent).
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TextBlockFormat {
    /// Paragraph text alignment (`QTextBlockFormat::alignment`, which reads `AlignLeft` while
    /// unset).
    pub alignment: Alignment,
    /// Margin above the block in pixels.
    pub top_margin: f32,
    /// Margin below the block in pixels.
    pub bottom_margin: f32,
    /// Left margin in pixels.
    pub left_margin: f32,
    /// Right margin in pixels.
    pub right_margin: f32,
    /// First-line text indent in pixels.
    pub text_indent: f32,
    /// Line height multiplier (1.0 = standard single space).
    pub line_height: f32,
    /// Heading level (0 = normal paragraph; 1..6 = H1..H6).
    pub heading_level: u8,
}

impl Default for TextBlockFormat {
    fn default() -> Self {
        Self {
            alignment: Alignment::LEFT,
            top_margin: 0.0,
            bottom_margin: 0.0,
            left_margin: 0.0,
            right_margin: 0.0,
            text_indent: 0.0,
            line_height: 1.0,
            heading_level: 0,
        }
    }
}

impl TextBlockFormat {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn merge(&mut self, other: &Self) {
        if other.alignment != Alignment::LEFT {
            self.alignment = other.alignment;
        }
        if other.top_margin != 0.0 {
            self.top_margin = other.top_margin;
        }
        if other.bottom_margin != 0.0 {
            self.bottom_margin = other.bottom_margin;
        }
        if other.left_margin != 0.0 {
            self.left_margin = other.left_margin;
        }
        if other.right_margin != 0.0 {
            self.right_margin = other.right_margin;
        }
        if other.text_indent != 0.0 {
            self.text_indent = other.text_indent;
        }
        if other.line_height != 1.0 {
            self.line_height = other.line_height;
        }
        if other.heading_level != 0 {
            self.heading_level = other.heading_level;
        }
    }
}

/// List format definition (`QTextListFormat` equivalent).
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TextListFormat {
    pub style: ListStyle,
    pub indent: i32,
    pub number_prefix: String,
    pub number_suffix: String,
}

impl Default for TextListFormat {
    fn default() -> Self {
        Self {
            style: ListStyle::Disc,
            indent: 1,
            number_prefix: String::new(),
            number_suffix: ".".to_string(),
        }
    }
}

/// Frame / container format (`QTextFrameFormat` equivalent).
#[derive(Debug, Clone, PartialEq)]
pub struct TextFrameFormat {
    pub margin: f32,
    pub padding: f32,
    pub border_width: f32,
    pub border_color: Option<Color>,
}

impl Default for TextFrameFormat {
    fn default() -> Self {
        Self {
            margin: 0.0,
            padding: 4.0,
            border_width: 0.0,
            border_color: None,
        }
    }
}

/// Range-based formatting override (used in `QTextLayout::FormatRange` and `QSyntaxHighlighter`).
#[derive(Debug, Clone, PartialEq)]
pub struct FormatRange {
    /// Starting offset in grapheme clusters or characters.
    pub start: usize,
    /// Length of the formatted range.
    pub length: usize,
    /// Character formatting applied to this range.
    pub format: TextCharFormat,
}

impl FormatRange {
    pub fn new(start: usize, length: usize, format: TextCharFormat) -> Self {
        Self {
            start,
            length,
            format,
        }
    }
}

// --- Canonical Qt Aliases ---
pub type QTextCharFormat = TextCharFormat;
pub type QTextBlockFormat = TextBlockFormat;
pub type QTextListFormat = TextListFormat;
pub type QTextFrameFormat = TextFrameFormat;
