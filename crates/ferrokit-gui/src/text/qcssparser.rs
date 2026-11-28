// SPDX-License-Identifier: LGPL-3.0-only
//! CSS/QSS parser and abstract syntax tree.

use tiny_skia::Color;

/// Supported CSS property enumeration.
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
    /// Maps a CSS property name (trimmed, ASCII case-insensitive) to a [`QCssProperty`].
    pub fn from_name(name: &str) -> Self {
        let lower = name.trim().to_ascii_lowercase();
        match lower.as_str() {
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
            _ => Self::Custom(lower),
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

/// Parsed CSS value.
#[derive(Debug, Clone, PartialEq)]
pub enum QCssValue {
    Color(Color),
    Length(f32),
    Number(f32),
    String(String),
    FontFamilies(Vec<String>),
    Identifier(String),
    Edges([f32; 4]),
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
    pub element_name: Option<String>,
    pub id: Option<String>,
    pub pseudo_states: Vec<String>,
    pub sub_control: Option<String>,
    pub attributes: Vec<(String, String)>,
}

impl QCssBasicSelector {
    /// Computes the cascade specificity score for this basic selector.
    pub fn specificity(&self) -> u32 {
        let mut score = 0u32;
        if self.id.is_some() {
            score += 100;
        }
        score += 10 * self.pseudo_states.len() as u32;
        score += 10 * self.attributes.len() as u32;
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

/// An AST representation of a parsed style sheet.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct QCssStyleSheet {
    pub rules: Vec<QCssRule>,
}

impl QCssStyleSheet {
    pub fn parse(input: &str) -> Self {
        QCssParser::parse(input)
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }
}

/// Parser implementation for stylesheet syntax.
pub struct QCssParser;

impl QCssParser {
    /// Parses a stylesheet string into a [`QCssStyleSheet`] AST.
    pub fn parse(input: &str) -> QCssStyleSheet {
        let stripped = strip_comments(input);
        let mut rules = Vec::new();
        let bytes = stripped.as_bytes();
        let mut i = 0;
        let mut rule_start = 0;
        let mut quote: Option<u8> = None;

        while i < bytes.len() {
            let b = bytes[i];
            if let Some(q) = quote {
                if b == b'\\' && i + 1 < bytes.len() {
                    i += 2;
                    continue;
                }
                if b == q {
                    quote = None;
                }
                i += 1;
                continue;
            }

            if b == b'\'' || b == b'"' {
                quote = Some(b);
                i += 1;
                continue;
            }

            if b == b'{' {
                let selector_text = &stripped[rule_start..i];
                let block_start = i + 1;
                i += 1;
                let mut block_quote: Option<u8> = None;
                let mut block_end = bytes.len();

                while i < bytes.len() {
                    let bb = bytes[i];
                    if let Some(q) = block_quote {
                        if bb == b'\\' && i + 1 < bytes.len() {
                            i += 2;
                            continue;
                        }
                        if bb == q {
                            block_quote = None;
                        }
                        i += 1;
                        continue;
                    }
                    if bb == b'\'' || bb == b'"' {
                        block_quote = Some(bb);
                        i += 1;
                        continue;
                    }
                    if bb == b'}' {
                        block_end = i;
                        i += 1;
                        break;
                    }
                    i += 1;
                }

                let decl_text = &stripped[block_start..block_end];
                let selectors = parse_selector_list(selector_text);
                let declarations = parse_declaration_list(decl_text);
                if !selectors.is_empty() && !declarations.is_empty() {
                    rules.push(QCssRule {
                        selectors,
                        declarations,
                    });
                }
                rule_start = i;
                continue;
            }

            i += 1;
        }

        QCssStyleSheet { rules }
    }
}

/// Parses a CSS length value (`px` or `pt`, converted to logical pixels).
pub fn parse_length(input: &str) -> Option<f32> {
    let s = input.trim();
    let lower = s.to_ascii_lowercase();
    if let Some(num) = lower.strip_suffix("px") {
        num.trim().parse::<f32>().ok()
    } else if let Some(num) = lower.strip_suffix("pt") {
        num.trim().parse::<f32>().ok().map(|v| v * 1.333_333)
    } else {
        None
    }
}

/// Parses a CSS color value (`transparent`, `#hex`, `rgb(...)`, `rgba(...)`, or named colors).
pub fn parse_color(input: &str) -> Option<Color> {
    let s = input.trim();
    if s.eq_ignore_ascii_case("transparent") {
        return Some(Color::TRANSPARENT);
    }

    if let Some(hex) = s.strip_prefix('#') {
        if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        return match hex.len() {
            3 => {
                let r = u8::from_str_radix(&hex[0..1], 16).ok()? * 17;
                let g = u8::from_str_radix(&hex[1..2], 16).ok()? * 17;
                let b = u8::from_str_radix(&hex[2..3], 16).ok()? * 17;
                Some(Color::from_rgba8(r, g, b, 255))
            }
            6 => {
                let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
                let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
                let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
                Some(Color::from_rgba8(r, g, b, 255))
            }
            8 => {
                let a = u8::from_str_radix(&hex[0..2], 16).ok()?;
                let r = u8::from_str_radix(&hex[2..4], 16).ok()?;
                let g = u8::from_str_radix(&hex[4..6], 16).ok()?;
                let b = u8::from_str_radix(&hex[6..8], 16).ok()?;
                Some(Color::from_rgba8(r, g, b, a))
            }
            _ => None,
        };
    }

    if s.get(..4).is_some_and(|p| p.eq_ignore_ascii_case("rgb(")) && s.ends_with(')') {
        let inner = &s[4..s.len() - 1];
        let mut parts = inner.split(',');
        let r = parts.next()?.trim().parse::<u8>().ok()?;
        let g = parts.next()?.trim().parse::<u8>().ok()?;
        let b = parts.next()?.trim().parse::<u8>().ok()?;
        if parts.next().is_some() {
            return None;
        }
        return Some(Color::from_rgba8(r, g, b, 255));
    }

    if s.get(..5).is_some_and(|p| p.eq_ignore_ascii_case("rgba(")) && s.ends_with(')') {
        let inner = &s[5..s.len() - 1];
        let mut parts = inner.split(',');
        let r = parts.next()?.trim().parse::<u8>().ok()?;
        let g = parts.next()?.trim().parse::<u8>().ok()?;
        let b = parts.next()?.trim().parse::<u8>().ok()?;
        let a_str = parts.next()?.trim();
        if parts.next().is_some() {
            return None;
        }
        let a = if let Ok(f) = a_str.parse::<f32>() {
            if f <= 1.0 {
                (f * 255.0).clamp(0.0, 255.0) as u8
            } else {
                f.clamp(0.0, 255.0) as u8
            }
        } else {
            a_str.parse::<u8>().ok()?
        };
        return Some(Color::from_rgba8(r, g, b, a));
    }

    match s.to_ascii_lowercase().as_str() {
        "black" => Some(Color::from_rgba8(0, 0, 0, 255)),
        "white" => Some(Color::from_rgba8(255, 255, 255, 255)),
        "red" => Some(Color::from_rgba8(255, 0, 0, 255)),
        "green" => Some(Color::from_rgba8(0, 128, 0, 255)),
        "blue" => Some(Color::from_rgba8(0, 0, 255, 255)),
        _ => None,
    }
}

/// Strips `/* ... */` comments while preserving quoted `'...'` and `"..."` strings.
fn strip_comments(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    let mut quote: Option<u8> = None;

    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = quote {
            let ch = input[i..].chars().next().unwrap();
            if b == b'\\' && i + 1 < bytes.len() {
                out.push(ch);
                i += ch.len_utf8();
                let next_ch = input[i..].chars().next().unwrap();
                out.push(next_ch);
                i += next_ch.len_utf8();
                continue;
            }
            if b == q {
                quote = None;
            }
            out.push(ch);
            i += ch.len_utf8();
            continue;
        }

        if b == b'\'' || b == b'"' {
            quote = Some(b);
            out.push(b as char);
            i += 1;
            continue;
        }

        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = if i + 1 < bytes.len() {
                i + 2
            } else {
                bytes.len()
            };
            continue;
        }

        let ch = input[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }

    out
}

/// Splits `input` on `delim` outside quotes, brackets `[...]`, and parentheses `(...)`.
fn split_top_level(input: &str, delim: u8, track_brackets: bool, track_parens: bool) -> Vec<&str> {
    let mut parts = Vec::new();
    let bytes = input.as_bytes();
    let mut i = 0;
    let mut start = 0;
    let mut quote: Option<u8> = None;
    let mut bracket_depth = 0usize;
    let mut paren_depth = 0usize;

    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = quote {
            if b == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if b == q {
                quote = None;
            }
            i += 1;
            continue;
        }

        if b == b'\'' || b == b'"' {
            quote = Some(b);
            i += 1;
            continue;
        }

        if track_brackets {
            if b == b'[' {
                bracket_depth += 1;
            } else if b == b']' {
                bracket_depth = bracket_depth.saturating_sub(1);
            }
        }

        if track_parens {
            if b == b'(' {
                paren_depth += 1;
            } else if b == b')' {
                paren_depth = paren_depth.saturating_sub(1);
            }
        }

        if b == delim && bracket_depth == 0 && paren_depth == 0 {
            parts.push(&input[start..i]);
            start = i + 1;
        }

        i += 1;
    }

    parts.push(&input[start..]);
    parts
}

fn strip_matching_quotes(s: &str) -> &str {
    let trimmed = s.trim();
    if trimmed.len() >= 2
        && ((trimmed.starts_with('"') && trimmed.ends_with('"'))
            || (trimmed.starts_with('\'') && trimmed.ends_with('\'')))
    {
        &trimmed[1..trimmed.len() - 1]
    } else {
        trimmed
    }
}

fn parse_selector_list(input: &str) -> Vec<QCssBasicSelector> {
    split_top_level(input, b',', true, false)
        .into_iter()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(parse_basic_selector)
        .collect()
}

fn parse_basic_selector(raw: &str) -> Option<QCssBasicSelector> {
    let mut attributes = Vec::new();
    let mut remaining = String::with_capacity(raw.len());
    let bytes = raw.as_bytes();
    let mut i = 0;
    let mut quote: Option<u8> = None;

    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = quote {
            let ch = raw[i..].chars().next().unwrap();
            if b == q {
                quote = None;
            }
            remaining.push(ch);
            i += ch.len_utf8();
            continue;
        }
        if b == b'\'' || b == b'"' {
            quote = Some(b);
            remaining.push(b as char);
            i += 1;
            continue;
        }
        if b == b'[' {
            let attr_start = i + 1;
            i += 1;
            let mut attr_quote: Option<u8> = None;
            let mut attr_end = bytes.len();
            while i < bytes.len() {
                let ab = bytes[i];
                if let Some(q) = attr_quote {
                    if ab == b'\\' && i + 1 < bytes.len() {
                        i += 2;
                        continue;
                    }
                    if ab == q {
                        attr_quote = None;
                    }
                    i += 1;
                    continue;
                }
                if ab == b'\'' || ab == b'"' {
                    attr_quote = Some(ab);
                    i += 1;
                    continue;
                }
                if ab == b']' {
                    attr_end = i;
                    i += 1;
                    break;
                }
                i += 1;
            }
            let attr_inner = &raw[attr_start..attr_end];
            if let Some((k, v)) = attr_inner.split_once('=') {
                let key = k.trim();
                let val = strip_matching_quotes(v);
                if !key.is_empty() {
                    attributes.push((key.to_string(), val.to_string()));
                }
            }
            continue;
        }

        let ch = raw[i..].chars().next().unwrap();
        remaining.push(ch);
        i += ch.len_utf8();
    }

    let mut sub_control = None;
    let mut extra_pseudos: Vec<String> = Vec::new();
    let before_sub = if let Some((left, right)) = remaining.split_once("::") {
        let mut right_parts = right.split(':');
        if let Some(sc) = right_parts.next() {
            let sc_trimmed = sc.trim();
            if !sc_trimmed.is_empty() {
                sub_control = Some(sc_trimmed.to_string());
            }
        }
        for p in right_parts {
            let pt = p.trim().to_ascii_lowercase();
            if !pt.is_empty() {
                extra_pseudos.push(pt);
            }
        }
        left
    } else {
        remaining.as_str()
    };

    let mut colon_parts = before_sub.split(':');
    let main_part = colon_parts.next().unwrap_or("").trim();

    let mut element_name = None;
    let mut id = None;
    if !main_part.is_empty() {
        if let Some((elem_part, id_part)) = main_part.split_once('#') {
            let elem_t = elem_part.trim();
            let id_t = id_part.trim();
            if !elem_t.is_empty() {
                element_name = Some(elem_t.to_string());
            }
            if !id_t.is_empty() {
                id = Some(id_t.to_string());
            }
        } else {
            element_name = Some(main_part.to_string());
        }
    }

    let mut pseudo_states: Vec<String> = colon_parts
        .map(|p| p.trim().to_ascii_lowercase())
        .filter(|p| !p.is_empty())
        .collect();
    pseudo_states.extend(extra_pseudos);

    if element_name.is_none()
        && id.is_none()
        && pseudo_states.is_empty()
        && sub_control.is_none()
        && attributes.is_empty()
    {
        None
    } else {
        Some(QCssBasicSelector {
            element_name,
            id,
            pseudo_states,
            sub_control,
            attributes,
        })
    }
}

fn parse_declaration_list(input: &str) -> Vec<QCssDeclaration> {
    let mut decls = Vec::new();
    for item in split_top_level(input, b';', false, true) {
        let trimmed = item.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Some((prop_raw, val_raw)) = trimmed.split_once(':') else {
            continue;
        };
        let prop_trimmed = prop_raw.trim();
        let val_trimmed = val_raw.trim();
        if prop_trimmed.is_empty() || val_trimmed.is_empty() {
            continue;
        }
        let property = QCssProperty::from_name(prop_trimmed);
        let value = parse_value(&property, val_trimmed);
        decls.push(QCssDeclaration { property, value });
    }
    decls
}

fn parse_value(property: &QCssProperty, val: &str) -> QCssValue {
    match property {
        QCssProperty::Border => {
            if val.eq_ignore_ascii_case("none") {
                return QCssValue::Border(QCssBorder {
                    width: 0.0,
                    style: "none".to_string(),
                    color: None,
                });
            }
            let (extracted_color, rest) = extract_functional_color(val);
            let mut width = 1.0f32;
            let mut style = "solid".to_string();
            let mut color = extracted_color;

            for tok in rest.split_whitespace() {
                if let Some(len) = parse_length(tok) {
                    width = len;
                } else {
                    let lower = tok.to_ascii_lowercase();
                    if matches!(lower.as_str(), "solid" | "none" | "dashed" | "dotted") {
                        style = lower;
                    } else if color.is_none()
                        && let Some(c) = parse_color(tok)
                    {
                        color = Some(c);
                    }
                }
            }

            return QCssValue::Border(QCssBorder {
                width,
                style,
                color,
            });
        }
        QCssProperty::FontFamily => {
            let families: Vec<String> = split_top_level(val, b',', false, false)
                .into_iter()
                .map(|part| strip_matching_quotes(part).trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if !families.is_empty() {
                return QCssValue::FontFamilies(families);
            }
        }
        QCssProperty::Padding | QCssProperty::Margin => {
            let nums: Option<Vec<f32>> = val
                .split_whitespace()
                .map(|tok| parse_length(tok).or_else(|| tok.parse::<f32>().ok()))
                .collect();
            if let Some(v) = nums {
                let edges = match v.as_slice() {
                    [a] => Some([*a, *a, *a, *a]),
                    [a, b] => Some([*a, *b, *a, *b]),
                    [a, b, c] => Some([*a, *b, *c, *b]),
                    [a, b, c, d] => Some([*a, *b, *c, *d]),
                    _ => None,
                };
                if let Some(edges) = edges {
                    return QCssValue::Edges(edges);
                }
            }
        }
        QCssProperty::BackgroundColor | QCssProperty::Color | QCssProperty::BorderColor => {
            if let Some(c) = parse_color(val) {
                return QCssValue::Color(c);
            }
        }
        _ => {}
    }

    if let Some(len) = parse_length(val) {
        QCssValue::Length(len)
    } else if let Ok(num) = val.parse::<f32>() {
        QCssValue::Number(num)
    } else if val.len() >= 2
        && ((val.starts_with('"') && val.ends_with('"'))
            || (val.starts_with('\'') && val.ends_with('\'')))
    {
        QCssValue::String(val[1..val.len() - 1].to_string())
    } else {
        QCssValue::Identifier(val.to_string())
    }
}

/// Extracts an `rgb(...)` or `rgba(...)` substring if present, returning `(parsed_color, remaining_text)`.
fn extract_functional_color(val: &str) -> (Option<Color>, String) {
    let lower = val.to_ascii_lowercase();
    let start = lower.find("rgba(").or_else(|| lower.find("rgb("));
    if let Some(s_idx) = start
        && let Some(rel_end) = lower[s_idx..].find(')')
    {
        let e_idx = s_idx + rel_end + 1;
        if let Some(color) = parse_color(&val[s_idx..e_idx]) {
            let mut rest = String::with_capacity(val.len());
            rest.push_str(&val[..s_idx]);
            rest.push(' ');
            rest.push_str(&val[e_idx..]);
            return (Some(color), rest);
        }
    }
    (None, val.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_ascii_hex_rejected_without_panic() {
        assert_eq!(parse_color("#ééé"), None);
        assert_eq!(parse_color("#+12345"), None);
        assert_eq!(parse_color("#12345"), None);
        assert_eq!(
            parse_color("#abc"),
            Some(Color::from_rgba8(0xaa, 0xbb, 0xcc, 255))
        );
        assert_eq!(
            parse_color("#80112233"),
            Some(Color::from_rgba8(0x11, 0x22, 0x33, 0x80))
        );
    }

    #[test]
    fn lone_quote_stays_identifier() {
        let prop = QCssProperty::Custom("custom-prop".to_string());
        assert_eq!(
            parse_value(&prop, "\""),
            QCssValue::Identifier("\"".to_string())
        );
        assert_eq!(
            parse_value(&prop, "'"),
            QCssValue::Identifier("'".to_string())
        );
        assert_eq!(
            parse_value(&prop, "\"hi\""),
            QCssValue::String("hi".to_string())
        );
    }

    #[test]
    fn comments_stripped_preserving_quoted_strings() {
        let css = r#"
            /* leading comment */
            QLabel {
                /* inside rule */
                content: "/* not a comment */";
                color: red; /* trailing */
            }
        "#;
        let sheet = QCssParser::parse(css);
        assert_eq!(sheet.rules.len(), 1);
        let decls = &sheet.rules[0].declarations;
        assert_eq!(decls.len(), 2);
        assert_eq!(
            decls[0].value,
            QCssValue::String("/* not a comment */".to_string())
        );
        assert_eq!(
            decls[1].value,
            QCssValue::Color(Color::from_rgba8(255, 0, 0, 255))
        );
    }

    #[test]
    fn border_shorthand_parsing() {
        let sheet = QCssParser::parse(
            "QFrame { border: 2px dashed rgba(10, 20, 30, 0.5); } QLabel { border: none; }",
        );
        assert_eq!(sheet.rules.len(), 2);
        assert_eq!(
            sheet.rules[0].declarations[0].value,
            QCssValue::Border(QCssBorder {
                width: 2.0,
                style: "dashed".to_string(),
                color: Some(Color::from_rgba8(10, 20, 30, 127)),
            })
        );
        assert_eq!(
            sheet.rules[1].declarations[0].value,
            QCssValue::Border(QCssBorder {
                width: 0.0,
                style: "none".to_string(),
                color: None,
            })
        );
    }

    #[test]
    fn font_family_list_parsing() {
        let sheet =
            QCssParser::parse(r#"QWidget { font-family: "Segoe UI", 'Noto Sans', monospace; }"#);
        assert_eq!(
            sheet.rules[0].declarations[0].value,
            QCssValue::FontFamilies(vec![
                "Segoe UI".to_string(),
                "Noto Sans".to_string(),
                "monospace".to_string(),
            ])
        );
    }

    #[test]
    fn edges_parsing_one_to_four_values() {
        let sheet = QCssParser::parse(
            "A { padding: 4px; } B { padding: 4px 8px; } C { margin: 1px 2px 3px; } D { margin: 1px 2px 3px 4px; }",
        );
        assert_eq!(
            sheet.rules[0].declarations[0].value,
            QCssValue::Edges([4.0, 4.0, 4.0, 4.0])
        );
        assert_eq!(
            sheet.rules[1].declarations[0].value,
            QCssValue::Edges([4.0, 8.0, 4.0, 8.0])
        );
        assert_eq!(
            sheet.rules[2].declarations[0].value,
            QCssValue::Edges([1.0, 2.0, 3.0, 2.0])
        );
        assert_eq!(
            sheet.rules[3].declarations[0].value,
            QCssValue::Edges([1.0, 2.0, 3.0, 4.0])
        );
    }

    #[test]
    fn rgba_alpha_truncation() {
        // 0.5 * 255.0 = 127.5 -> truncated to 127, not rounded to 128
        assert_eq!(
            parse_color("rgba(10, 20, 30, 0.5)"),
            Some(Color::from_rgba8(10, 20, 30, 127))
        );
        assert_eq!(
            parse_color("rgba(10, 20, 30, 128)"),
            Some(Color::from_rgba8(10, 20, 30, 128))
        );
    }
}
