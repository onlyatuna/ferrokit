// SPDX-License-Identifier: LGPL-3.0-only
//! Qt-compatible CSS/QSS Parser and Abstract Syntax Tree.
//!
//! Mirrors `qtbase/src/gui/text/qcssparser.cpp` and `qcssparser_p.h`.
//! Parses CSS 2.1 subset used by Qt Style Sheets (QSS) including:
//! - Selectors: Type (`QProgressBar`), ID (`#HeaderTitle`), Pseudo-state (`:hover`),
//!   Sub-control (`::chunk`), Attribute (`[state="muted"]`).
//! - Box Model: `min-height`, `max-height`, `min-width`, `max-width`, `margin`, `padding`,
//!   `border`, `border-radius`.
//! - Appearance: `background-color`, `color`, `font-size`, `font-weight`, `font-family`,
//!   `letter-spacing`, `text-align`.

use tiny_skia::Color;

/// Supported CSS Property enumeration.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum QCssProperty {
    BackgroundColor,
    Color,
    Border,
    BorderColor,
    BorderWidth,
    BorderStyle,
    BorderRadius,
    MinHeight,
    MaxHeight,
    Height,
    MinWidth,
    MaxWidth,
    Width,
    Padding,
    PaddingTop,
    PaddingRight,
    PaddingBottom,
    PaddingLeft,
    Margin,
    MarginTop,
    MarginRight,
    MarginBottom,
    MarginLeft,
    FontSize,
    FontWeight,
    FontFamily,
    LetterSpacing,
    TextAlign,
    Custom(String),
}

impl std::str::FromStr for QCssProperty {
    type Err = std::convert::Infallible;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Ok(Self::from_name(name))
    }
}

impl QCssProperty {
    pub fn from_name(name: &str) -> Self {
        let name = name.trim().to_ascii_lowercase();
        match name.as_str() {
            "background-color" | "background" => Self::BackgroundColor,
            "color" => Self::Color,
            "border" => Self::Border,
            "border-color" => Self::BorderColor,
            "border-width" => Self::BorderWidth,
            "border-style" => Self::BorderStyle,
            "border-radius" => Self::BorderRadius,
            "min-height" => Self::MinHeight,
            "max-height" => Self::MaxHeight,
            "height" => Self::Height,
            "min-width" => Self::MinWidth,
            "max-width" => Self::MaxWidth,
            "width" => Self::Width,
            "padding" => Self::Padding,
            "padding-top" => Self::PaddingTop,
            "padding-right" => Self::PaddingRight,
            "padding-bottom" => Self::PaddingBottom,
            "padding-left" => Self::PaddingLeft,
            "margin" => Self::Margin,
            "margin-top" => Self::MarginTop,
            "margin-right" => Self::MarginRight,
            "margin-bottom" => Self::MarginBottom,
            "margin-left" => Self::MarginLeft,
            "font-size" => Self::FontSize,
            "font-weight" => Self::FontWeight,
            "font-family" => Self::FontFamily,
            "letter-spacing" => Self::LetterSpacing,
            "text-align" => Self::TextAlign,
            other => Self::Custom(other.to_string()),
        }
    }
}

/// Border specification for border shorthand properties.
#[derive(Debug, Clone, PartialEq)]
pub struct QCssBorder {
    pub width: f32,
    pub style: String,
    pub color: Option<Color>,
}

/// Parsed CSS Value.
#[derive(Debug, Clone, PartialEq)]
pub enum QCssValue {
    Color(Color),
    Length(f32),
    Number(f32),
    String(String),
    /// `font-family: a, 'b c', d` (`setFontFamilyFromValues`, `qcssparser.cpp:1252`): the families in order.
    FontFamilies(Vec<String>),
    Identifier(String),
    Edges([f32; 4]), // [top, right, bottom, left]
    Border(QCssBorder),
}

/// A CSS declaration associating a property with a value.
#[derive(Debug, Clone, PartialEq)]
pub struct QCssDeclaration {
    pub property: QCssProperty,
    pub value: QCssValue,
}

/// Basic selector component matching an individual element.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct QCssBasicSelector {
    /// Type selector (e.g. `QProgressBar`, `QLabel`, `QPushButton`, `QWidget`).
    pub element_name: Option<String>,
    /// ID selector (e.g. `CentralWidget`, `HeaderTitle`, `LayoutToggleBtn`).
    pub id: Option<String>,
    /// Pseudo-states (e.g. `hover`, `pressed`, `selected`, `disabled`).
    pub pseudo_states: Vec<String>,
    /// Sub-control (e.g. `chunk`, `groove`, `item`, `separator`).
    pub sub_control: Option<String>,
    /// Attribute selectors (e.g. `[state="muted"]`).
    pub attributes: Vec<(String, String)>,
}

impl QCssBasicSelector {
    /// Computes selector specificity (ID = 100, Attribute/Pseudo = 10, Type = 1).
    pub fn specificity(&self) -> u32 {
        let mut score = 0;
        if self.id.is_some() {
            score += 100;
        }
        score += (self.pseudo_states.len() as u32) * 10;
        score += (self.attributes.len() as u32) * 10;
        if self.sub_control.is_some() {
            score += 10;
        }
        if self.element_name.is_some() {
            score += 1;
        }
        score
    }
}

/// A CSS rule containing one or more selectors and declarations.
#[derive(Debug, Clone, PartialEq)]
pub struct QCssRule {
    pub selectors: Vec<QCssBasicSelector>,
    pub declarations: Vec<QCssDeclaration>,
}

/// An AST representation of a parsed Qt Style Sheet.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct QCssStyleSheet {
    pub rules: Vec<QCssRule>,
}

impl QCssStyleSheet {
    /// Parses a QSS string into an AST.
    pub fn parse(input: &str) -> Self {
        QCssParser::parse(input)
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }
}

/// Parser implementation for QSS stylesheet syntax.
pub struct QCssParser;

impl QCssParser {
    pub fn parse(input: &str) -> QCssStyleSheet {
        let clean = strip_comments(input);
        let mut rules = Vec::new();

        let mut pos = 0;
        let bytes = clean.as_bytes();
        let len = bytes.len();

        while pos < len {
            // Find opening brace '{'
            let Some(brace_open) = clean[pos..].find('{') else {
                break;
            };
            let selector_str = clean[pos..pos + brace_open].trim();
            pos += brace_open + 1;

            // Find closing brace '}'
            let Some(brace_close) = clean[pos..].find('}') else {
                break;
            };
            let body_str = clean[pos..pos + brace_close].trim();
            pos += brace_close + 1;

            if selector_str.is_empty() && body_str.is_empty() {
                continue;
            }

            let selectors = parse_selectors(selector_str);
            let declarations = parse_declarations(body_str);

            if !selectors.is_empty() && !declarations.is_empty() {
                rules.push(QCssRule {
                    selectors,
                    declarations,
                });
            }
        }

        QCssStyleSheet { rules }
    }
}

/// Strips `/* ... */` comments from CSS input.
fn strip_comments(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '/' && chars.peek() == Some(&'*') {
            chars.next(); // consume '*'
            while let Some(c) = chars.next() {
                if c == '*' && chars.peek() == Some(&'/') {
                    chars.next(); // consume '/'
                    break;
                }
            }
        } else {
            out.push(ch);
        }
    }

    out
}

/// Parses comma-separated selectors (e.g. `QLabel#HeaderTitle, QLabel#MetricTitle`).
fn parse_selectors(input: &str) -> Vec<QCssBasicSelector> {
    let mut result = Vec::new();
    for part in input.split(',') {
        let part = part.trim();
        if !part.is_empty() {
            result.push(parse_basic_selector(part));
        }
    }
    result
}

/// Parses an individual selector item (e.g. `QPushButton#LayoutToggleBtn:hover`).
fn parse_basic_selector(input: &str) -> QCssBasicSelector {
    let mut sel = QCssBasicSelector::default();
    let mut remaining = input.trim().to_string();

    // 1. Extract attribute selectors `[key="val"]`
    while let Some(start) = remaining.find('[') {
        if let Some(end) = remaining[start..].find(']') {
            let full_end = start + end;
            let attr_part = &remaining[start + 1..full_end];
            if let Some((k, v)) = attr_part.split_once('=') {
                let key = k.trim().to_string();
                let val = v.trim().trim_matches('"').trim_matches('\'').to_string();
                sel.attributes.push((key, val));
            }
            remaining.replace_range(start..=full_end, "");
        } else {
            break;
        }
    }

    let s = remaining.trim();
    // 2. Extract sub-control (e.g. `::chunk`, `::item`)
    let (s, sub_control) = if let Some(idx) = s.find("::") {
        let (left, right) = s.split_at(idx);
        let sub = right.trim_start_matches("::").trim();
        (left.trim(), Some(sub.to_string()))
    } else {
        (s, None)
    };
    sel.sub_control = sub_control;

    // 3. Extract pseudo-states (e.g. `:hover`, `:disabled`)
    let mut parts: Vec<&str> = s.split(':').collect();
    let main_part = parts.remove(0).trim();
    for p in parts {
        let p = p.trim();
        if !p.is_empty() {
            sel.pseudo_states.push(p.to_ascii_lowercase());
        }
    }

    // 4. Extract ID (e.g. `#CentralWidget`) and Type (e.g. `QLabel`)
    if let Some((elem, id)) = main_part.split_once('#') {
        let elem = elem.trim();
        let id = id.trim();
        if !elem.is_empty() {
            sel.element_name = Some(elem.to_string());
        }
        if !id.is_empty() {
            sel.id = Some(id.to_string());
        }
    } else if main_part.starts_with('#') {
        let id = main_part.trim_start_matches('#').trim();
        if !id.is_empty() {
            sel.id = Some(id.to_string());
        }
    } else if !main_part.is_empty() {
        sel.element_name = Some(main_part.to_string());
    }

    sel
}

/// Parses declarations inside `{ ... }`.
fn parse_declarations(input: &str) -> Vec<QCssDeclaration> {
    let mut decls = Vec::new();
    for item in input.split(';') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        let Some((prop_str, val_str)) = item.split_once(':') else {
            continue;
        };
        let prop = QCssProperty::from_name(prop_str.trim());
        let val = parse_value(&prop, val_str.trim());
        decls.push(QCssDeclaration {
            property: prop,
            value: val,
        });
    }
    decls
}

/// Parses value string according to property context.
fn parse_value(property: &QCssProperty, val_str: &str) -> QCssValue {
    let val_str = val_str.trim();

    // Check border shorthand
    if matches!(property, QCssProperty::Border) {
        if val_str.eq_ignore_ascii_case("none") {
            return QCssValue::Border(QCssBorder {
                width: 0.0,
                style: "none".to_string(),
                color: None,
            });
        }
        return parse_border(val_str);
    }

    // `font-family` is a comma separated list (`QFont::setFamilies`), each name optionally quoted.
    if matches!(property, QCssProperty::FontFamily) {
        let families: Vec<String> = split_font_families(val_str);
        if !families.is_empty() {
            return QCssValue::FontFamilies(families);
        }
    }

    // Check padding / margin multi-values
    if matches!(
        property,
        QCssProperty::Padding
            | QCssProperty::Margin
    ) {
        if let Some(edges) = parse_edges(val_str) {
            return QCssValue::Edges(edges);
        }
    }

    // Try color
    if matches!(
        property,
        QCssProperty::BackgroundColor
            | QCssProperty::Color
            | QCssProperty::BorderColor
    ) {
        if let Some(c) = parse_color(val_str) {
            return QCssValue::Color(c);
        }
    }

    // Try length / numeric
    if let Some(len) = parse_length(val_str) {
        return QCssValue::Length(len);
    }

    // Try pure number
    if let Ok(num) = val_str.parse::<f32>() {
        return QCssValue::Number(num);
    }

    // Quoted string (e.g. font family names)
    if (val_str.starts_with('\'') && val_str.ends_with('\''))
        || (val_str.starts_with('"') && val_str.ends_with('"'))
    {
        return QCssValue::String(val_str[1..val_str.len() - 1].to_string());
    }

    QCssValue::Identifier(val_str.to_string())
}

/// Splits a `font-family` value at the commas outside quotes and strips one pair of quotes per name.
fn split_font_families(val_str: &str) -> Vec<String> {
    let mut families = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for ch in val_str.chars() {
        match quote {
            Some(q) if ch == q => quote = None,
            Some(_) => current.push(ch),
            None if ch == '\'' || ch == '"' => quote = Some(ch),
            None if ch == ',' => {
                families.push(std::mem::take(&mut current));
            }
            None => current.push(ch),
        }
    }
    families.push(current);
    families
        .into_iter()
        .map(|f| f.trim().to_string())
        .filter(|f| !f.is_empty())
        .collect()
}

/// Parses border shorthand: `1px solid rgba(255, 255, 255, 0.14)`.
fn parse_border(val_str: &str) -> QCssValue {
    let mut width = 1.0;
    let mut style = "solid".to_string();

    // Extract rgba/rgb function if present
    let (rem, extracted_color) = if let Some(rgb_idx) = val_str.find("rgb") {
        if let Some(close_idx) = val_str[rgb_idx..].find(')') {
            let col_str = &val_str[rgb_idx..rgb_idx + close_idx + 1];
            let col = parse_color(col_str);
            let mut remaining = val_str[..rgb_idx].to_string();
            remaining.push_str(&val_str[rgb_idx + close_idx + 1..]);
            (remaining, col)
        } else {
            (val_str.to_string(), None)
        }
    } else {
        (val_str.to_string(), None)
    };
    let mut color = extracted_color;

    for tok in rem.split_whitespace() {
        let tok = tok.trim();
        if let Some(l) = parse_length(tok) {
            width = l;
        } else if tok.eq_ignore_ascii_case("solid")
            || tok.eq_ignore_ascii_case("none")
            || tok.eq_ignore_ascii_case("dashed")
            || tok.eq_ignore_ascii_case("dotted")
        {
            style = tok.to_ascii_lowercase();
        } else if color.is_none() {
            color = parse_color(tok);
        }
    }

    QCssValue::Border(QCssBorder {
        width,
        style,
        color,
    })
}

/// Parses edge boxes: 1, 2, or 4 numbers (e.g. `1px 4px` -> [1.0, 4.0, 1.0, 4.0]).
fn parse_edges(val_str: &str) -> Option<[f32; 4]> {
    let tokens: Vec<f32> = val_str
        .split_whitespace()
        .filter_map(|t| parse_length(t).or_else(|| t.parse::<f32>().ok()))
        .collect();

    match tokens.len() {
        1 => Some([tokens[0], tokens[0], tokens[0], tokens[0]]),
        2 => Some([tokens[0], tokens[1], tokens[0], tokens[1]]), // top/bottom, left/right
        3 => Some([tokens[0], tokens[1], tokens[2], tokens[1]]), // top, left/right, bottom
        4 => Some([tokens[0], tokens[1], tokens[2], tokens[3]]), // top, right, bottom, left
        _ => None,
    }
}

/// Parses CSS length like `5px`, `9.5px`, `10.5px`, `12pt`.
pub fn parse_length(input: &str) -> Option<f32> {
    let s = input.trim();
    if let Some(val) = s.strip_suffix("px") {
        val.trim().parse::<f32>().ok()
    } else if let Some(val) = s.strip_suffix("pt") {
        val.trim().parse::<f32>().ok().map(|pt| pt * 1.333_333)
    } else {
        None
    }
}

/// Parses standard CSS color strings: `#RGB`, `#RRGGBB`, `#AARRGGBB`, `rgb(...)`, `rgba(...)`, `transparent`.
pub fn parse_color(input: &str) -> Option<Color> {
    let s = input.trim();
    if s.eq_ignore_ascii_case("transparent") {
        return Some(Color::TRANSPARENT);
    }

    if let Some(hex) = s.strip_prefix('#') {
        return match hex.len() {
            3 => {
                let r = u8::from_str_radix(&hex[0..1].repeat(2), 16).ok()?;
                let g = u8::from_str_radix(&hex[1..2].repeat(2), 16).ok()?;
                let b = u8::from_str_radix(&hex[2..3].repeat(2), 16).ok()?;
                Some(Color::from_rgba8(r, g, b, 255))
            }
            6 => {
                let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
                let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
                let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
                Some(Color::from_rgba8(r, g, b, 255))
            }
            8 => {
                // Qt QCss::Parser::parseHexColor -> QColor::fromString (qcssparser.cpp:3026):
                // 8-digit hex colors are in #AARRGGBB format.
                let a = u8::from_str_radix(&hex[0..2], 16).ok()?;
                let r = u8::from_str_radix(&hex[2..4], 16).ok()?;
                let g = u8::from_str_radix(&hex[4..6], 16).ok()?;
                let b = u8::from_str_radix(&hex[6..8], 16).ok()?;
                Some(Color::from_rgba8(r, g, b, a))
            }
            _ => None,
        };
    }

    if s.starts_with("rgb(") && s.ends_with(')') {
        let inside = &s[4..s.len() - 1];
        let parts: Vec<&str> = inside.split(',').collect();
        if parts.len() == 3 {
            let r = parts[0].trim().parse::<u8>().ok()?;
            let g = parts[1].trim().parse::<u8>().ok()?;
            let b = parts[2].trim().parse::<u8>().ok()?;
            return Some(Color::from_rgba8(r, g, b, 255));
        }
    }

    if s.starts_with("rgba(") && s.ends_with(')') {
        let inside = &s[5..s.len() - 1];
        let parts: Vec<&str> = inside.split(',').collect();
        if parts.len() == 4 {
            let r = parts[0].trim().parse::<u8>().ok()?;
            let g = parts[1].trim().parse::<u8>().ok()?;
            let b = parts[2].trim().parse::<u8>().ok()?;
            let a_str = parts[3].trim();
            let a = if let Ok(f) = a_str.parse::<f32>() {
                if f <= 1.0 {
                    // `int alpha = toReal() * 255.` (qcssparser.cpp:826-828): truncated, not rounded.
                    (f * 255.0).clamp(0.0, 255.0) as u8
                } else {
                    f.clamp(0.0, 255.0) as u8
                }
            } else {
                a_str.parse::<u8>().ok()?
            };
            return Some(Color::from_rgba8(r, g, b, a));
        }
    }

    // Common named colors
    match s.to_ascii_lowercase().as_str() {
        "black" => Some(Color::BLACK),
        "white" => Some(Color::WHITE),
        "red" => Some(Color::from_rgba8(255, 0, 0, 255)),
        "green" => Some(Color::from_rgba8(0, 128, 0, 255)),
        "blue" => Some(Color::from_rgba8(0, 0, 255, 255)),
        _ => None,
    }
}
