// SPDX-License-Identifier: LGPL-3.0-only
//! Rich text import/export for `TextDocument`.
//!
//! - HTML subset import (`QTextHtmlImporter`): inline styles (`<b>`, `<i>`, `<u>`, `<s>`, `<sup>`,
//!   `<sub>`, `<font color>`, `<span style>`), hyperlinks and named anchors (`<a href>`, `<a name>`,
//!   `id`), block elements (`<p>`, `<div>`, `<h1>`..`<h6>`, `<li>`, `<pre>`, `<br>`), entity decoding,
//!   HTML whitespace collapsing and `<title>` capture.
//! - HTML export (`QTextHtmlExporter`) producing markup that re-imports losslessly.
//! - CommonMark-style Markdown import/export (`QTextMarkdownImporter` / `QTextMarkdownWriter`):
//!   ATX/setext headings, paragraphs, fenced code, emphasis, strikethrough, code spans and links.

use crate::alignment::Alignment;
use crate::text::block::TextBlock;
use crate::text::font::FontWeight;
use crate::text::format::{TextBlockFormat, TextCharFormat, UnderlineStyle, VerticalAlignment};
use crate::text::fragment::TextFragment;
use std::borrow::Cow;
use tiny_skia::Color;
/// The HTML element names `QTextHtmlParser::lookupElement` knows (qtexthtmlparser.cpp:355-416),
/// sorted for binary search.
#[rustfmt::skip]
const RICH_TEXT_ELEMENTS: &[&str] = &[
    "a", "address", "b", "big", "blockquote", "body", "br", "caption", "center", "cite", "code",
    "dd", "del", "dfn", "div", "dl", "dt", "em", "font", "h1", "h2", "h3", "h4", "h5", "h6", "head",
    "hr", "html", "i", "img", "kbd", "li", "link", "meta", "nobr", "ol", "p", "pre", "qt", "s",
    "samp", "script", "small", "span", "strong", "style", "sub", "sup", "table", "tbody", "td",
    "tfoot", "th", "thead", "title", "tr", "tt", "u", "ul", "var",
];

/// `Qt::mightBeRichText` (qtextdocument.cpp:72-130): whether `text` likely holds rich text.
///
/// Looks for a tag, or a `&lt;` entity, before the first line break. The first tag must be a known
/// element name, so `value <5 and >3` is plain text and `<b>x</b>` is rich.
pub fn might_be_rich_text(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    if n == 0 {
        return false;
    }
    let mut start = 0;
    while start < n && chars[start].is_whitespace() {
        start += 1;
    }
    // A leading `<?xml ... ?>` declaration is skipped, as for XHTML.
    if chars[start..(start + 5).min(n)]
        .iter()
        .copied()
        .eq("<?xml".chars())
    {
        while start < n {
            if chars[start] == '?' && start + 2 < n && chars[start + 1] == '>' {
                start += 2;
                break;
            }
            start += 1;
        }
        while start < n && chars[start].is_whitespace() {
            start += 1;
        }
    }
    let doctype: String = chars[start..(start + 5).min(n)].iter().collect();
    if doctype.eq_ignore_ascii_case("<!doc") {
        return true;
    }
    let mut open = start;
    while open < n && chars[open] != '<' && chars[open] != '\n' {
        if chars[open] == '&'
            && chars[open + 1..(open + 4).min(n)]
                .iter()
                .copied()
                .eq("lt;".chars())
        {
            return true;
        }
        open += 1;
    }
    if open < n && chars[open] == '<' {
        if let Some(close) = (open..n).find(|&i| chars[i] == '>') {
            let mut tag = String::new();
            for i in open + 1..close {
                let c = chars[i];
                if c.is_numeric() || c.is_alphabetic() {
                    tag.extend(c.to_lowercase());
                } else if !tag.is_empty() && c.is_whitespace() {
                    break;
                } else if !tag.is_empty() && c == '/' && i + 1 == close {
                    break;
                } else if !c.is_whitespace() && (!tag.is_empty() || c != '!') {
                    return false;
                }
            }
            return RICH_TEXT_ELEMENTS.binary_search(&tag.as_str()).is_ok();
        }
    }
    false
}

/// Default hyperlink color applied to imported anchors (`QPalette::Link`).
pub fn link_color() -> Color {
    Color::from_rgba8(0, 0, 255, 255)
}

fn is_bold(fmt: &TextCharFormat) -> bool {
    fmt.font_weight
        .is_some_and(|w| w as u16 >= FontWeight::Bold as u16)
}

fn set_underline(fmt: &mut TextCharFormat, on: bool) {
    fmt.font_underline = on;
    fmt.underline_style = if on {
        UnderlineStyle::SingleUnderline
    } else {
        UnderlineStyle::NoUnderline
    };
}

fn apply_link_style(fmt: &mut TextCharFormat, href: String) {
    fmt.anchor_href = Some(href);
    fmt.foreground = Some(link_color());
    set_underline(fmt, true);
}

// ---------------------------------------------------------------------------
// HTML import
// ---------------------------------------------------------------------------

/// Result of parsing an HTML document.
pub(crate) struct ParsedHtml {
    pub(crate) blocks: Vec<TextBlock>,
    pub(crate) title: String,
}

/// Parses an HTML string into document blocks.
pub(crate) fn parse_html(html: &str) -> ParsedHtml {
    let mut importer = HtmlImporter::default();
    importer.run(html);
    importer.finish()
}

struct OpenElement {
    name: String,
    saved_format: TextCharFormat,
    preformatted: bool,
}

#[derive(Default)]
struct HtmlImporter {
    blocks: Vec<TextBlock>,
    current: TextBlock,
    /// The current block was opened by a block element and must be kept even if empty.
    block_explicit: bool,
    format: TextCharFormat,
    stack: Vec<OpenElement>,
    pre_depth: usize,
    skip_pre_newline: bool,
    last_was_space: bool,
    pending_anchor: Option<String>,
    title: String,
}

fn is_block_element(name: &str) -> bool {
    matches!(
        name,
        "p" | "div"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "li"
            | "pre"
            | "blockquote"
            | "center"
            | "tr"
            | "dt"
            | "dd"
            | "ul"
            | "ol"
            | "dl"
            | "table"
            | "body"
            | "html"
            | "section"
            | "article"
            | "header"
            | "footer"
            | "nav"
            | "aside"
            | "main"
            | "figure"
            | "figcaption"
            | "address"
    )
}

fn is_void_element(name: &str) -> bool {
    matches!(
        name,
        "meta"
            | "link"
            | "input"
            | "col"
            | "base"
            | "area"
            | "wbr"
            | "param"
            | "source"
            | "track"
            | "embed"
            | "img"
    )
}

/// Elements whose content is raw text that must not be rendered.
fn is_raw_text_element(name: &str) -> bool {
    matches!(name, "title" | "style" | "script" | "textarea")
}

impl HtmlImporter {
    fn run(&mut self, src: &str) {
        let mut rest = src;
        while !rest.is_empty() {
            let Some(lt) = rest.find('<') else {
                self.text(rest);
                break;
            };
            if lt > 0 {
                self.text(&rest[..lt]);
            }
            rest = &rest[lt..];
            if rest.starts_with("<!--") {
                rest = rest.find("-->").map_or("", |end| &rest[end + 3..]);
                continue;
            }
            let Some(end) = tag_end(rest) else {
                self.text(rest);
                break;
            };
            let inner = &rest[1..end];
            rest = &rest[end + 1..];
            if inner.starts_with('!') || inner.starts_with('?') {
                continue;
            }
            if let Some(raw_name) = self.tag(inner) {
                // Raw text element: consume everything up to its closing tag.
                // Case-insensitive search without lowercasing a copy of the rest of the
                // document, which made every raw element O(remaining length).
                let closing = format!("</{raw_name}");
                let found = rest.match_indices("</").map(|(pos, _)| pos).find(|&pos| {
                    rest.as_bytes()[pos..]
                        .get(..closing.len())
                        .is_some_and(|b| b.eq_ignore_ascii_case(closing.as_bytes()))
                });
                let (content, after) = match found {
                    Some(pos) => {
                        let after_tag = rest[pos..].find('>').map_or(rest.len(), |gt| pos + gt + 1);
                        (&rest[..pos], &rest[after_tag..])
                    }
                    None => (rest, ""),
                };
                if raw_name == "title" {
                    self.title = collapse_whitespace(&decode_entities(content))
                        .trim()
                        .to_string();
                }
                rest = after;
            }
        }
    }

    fn finish(mut self) -> ParsedHtml {
        if !self.current.is_empty() || self.blocks.is_empty() {
            self.flush_current();
        }
        ParsedHtml {
            blocks: self.blocks,
            title: self.title,
        }
    }

    /// Handles a tag. Returns the element name when it starts a raw text element.
    fn tag(&mut self, inner: &str) -> Option<String> {
        let inner = inner.trim();
        let (closing, body) = match inner.strip_prefix('/') {
            Some(b) => (true, b.trim_start()),
            None => (false, inner),
        };
        let self_closing = body.ends_with('/');
        let body = body.trim_end_matches('/');
        let name_end = body
            .find(|c: char| c.is_ascii_whitespace())
            .unwrap_or(body.len());
        let name = body[..name_end].to_ascii_lowercase();
        if name.is_empty() {
            return None;
        }
        if closing {
            self.close(&name);
            return None;
        }
        if is_raw_text_element(&name) {
            return (!self_closing).then_some(name);
        }
        let attrs = parse_attributes(&body[name_end..]);
        self.open(name, &attrs, self_closing);
        None
    }

    fn open(&mut self, name: String, attrs: &[(String, String)], self_closing: bool) {
        match name.as_str() {
            "br" => {
                self.line_break();
                return;
            }
            "hr" => {
                self.start_block(false);
                return;
            }
            _ => {}
        }
        if is_void_element(&name) {
            return;
        }
        let attr = |key: &str| {
            attrs
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.as_str())
        };

        let saved_format = self.format.clone();
        let block_level = is_block_element(&name);
        let mut preformatted = false;
        if block_level {
            self.start_block(true);
            if let Some(align) = attr("align") {
                if let Some(a) = parse_alignment(align) {
                    self.current.block_format.alignment = a;
                }
            }
        }
        match name.as_str() {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.current.block_format.heading_level = name.as_bytes()[1] - b'0';
                self.format.font_weight = Some(FontWeight::Bold);
            }
            "pre" => preformatted = true,
            "b" | "strong" => self.format.font_weight = Some(FontWeight::Bold),
            "i" | "em" | "cite" | "var" | "dfn" => self.format.font_italic = Some(true),
            "u" | "ins" => set_underline(&mut self.format, true),
            "s" | "strike" | "del" => self.format.font_strikeout = true,
            "sup" => self.format.vertical_alignment = VerticalAlignment::AlignSuperScript,
            "sub" => self.format.vertical_alignment = VerticalAlignment::AlignSubScript,
            "a" => {
                if let Some(href) = attr("href") {
                    apply_link_style(&mut self.format, href.to_string());
                }
                if let Some(anchor) = attr("name") {
                    self.pending_anchor = Some(anchor.to_string());
                }
            }
            "font" => {
                if let Some(color) = attr("color").and_then(parse_color) {
                    self.format.foreground = Some(color);
                }
            }
            _ => {}
        }
        if let Some(id) = attr("id") {
            self.pending_anchor = Some(id.to_string());
        }
        if let Some(style) = attr("style") {
            let block_format = if block_level {
                Some(&mut self.current.block_format)
            } else {
                None
            };
            preformatted |= apply_css(style, &mut self.format, block_format);
        }
        if preformatted {
            self.pre_depth += 1;
            self.skip_pre_newline = name == "pre";
        }
        if self_closing {
            self.format = saved_format;
            if preformatted {
                self.pre_depth -= 1;
            }
            if block_level {
                self.end_block();
            }
        } else {
            self.stack.push(OpenElement {
                name,
                saved_format,
                preformatted,
            });
        }
    }

    fn close(&mut self, name: &str) {
        let Some(idx) = self.stack.iter().rposition(|e| e.name == name) else {
            return;
        };
        let popped: Vec<OpenElement> = self.stack.drain(idx..).collect();
        let block_level = popped.iter().any(|e| is_block_element(&e.name));
        if block_level {
            self.end_block();
        }
        for e in &popped {
            if e.preformatted {
                self.pre_depth = self.pre_depth.saturating_sub(1);
            }
        }
        self.format = popped
            .into_iter()
            .next()
            .map(|e| e.saved_format)
            .unwrap_or_default();
    }

    /// Starts a new block if the current one already holds content.
    fn start_block(&mut self, explicit: bool) {
        if !self.current.is_empty() {
            self.flush_current();
        }
        self.block_explicit = explicit;
        self.last_was_space = false;
    }

    /// Ends the current block (keeping it when it has content or was explicitly opened).
    fn end_block(&mut self) {
        if !self.current.is_empty() || self.block_explicit {
            self.flush_current();
        }
        self.block_explicit = false;
        self.last_was_space = false;
    }

    /// `<br>`: always terminates the current line, continuing with the same paragraph format.
    fn line_break(&mut self) {
        let block_format = self.current.block_format.clone();
        self.flush_current();
        self.current.block_format = block_format;
        self.last_was_space = false;
    }

    fn flush_current(&mut self) {
        let mut block = std::mem::take(&mut self.current);
        if self.pre_depth == 0 {
            if let Some(last) = block.fragments.last_mut() {
                let trimmed = last.text.trim_end_matches(' ').len();
                last.text.truncate(trimmed);
            }
        }
        block.normalize_fragments();
        self.blocks.push(block);
    }

    fn text(&mut self, raw: &str) {
        let decoded = decode_entities(raw);
        if self.pre_depth > 0 {
            let mut s: &str = &decoded;
            if self.skip_pre_newline && !s.is_empty() {
                s = s
                    .strip_prefix("\r\n")
                    .or_else(|| s.strip_prefix('\n'))
                    .unwrap_or(s);
                self.skip_pre_newline = false;
            }
            for (i, line) in s.split('\n').enumerate() {
                if i > 0 {
                    self.line_break();
                }
                self.push_text(line.strip_suffix('\r').unwrap_or(line));
            }
            return;
        }
        let mut out = String::with_capacity(decoded.len());
        for c in decoded.chars() {
            if c.is_ascii_whitespace() {
                if !self.last_was_space && (!self.current.is_empty() || !out.is_empty()) {
                    out.push(' ');
                }
                self.last_was_space = true;
            } else {
                out.push(c);
                self.last_was_space = false;
            }
        }
        self.push_text(&out);
    }

    fn push_text(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        let format = match self.pending_anchor.take() {
            Some(anchor) => {
                let mut f = self.format.clone();
                f.anchor_name = Some(anchor);
                f
            }
            None => self.format.clone(),
        };
        match self.current.fragments.last_mut() {
            Some(last) if last.format == format => last.text.push_str(s),
            _ => self.current.fragments.push(TextFragment::new(s, format)),
        }
    }
}

/// Finds the index of the `>` closing the tag starting at `s[0] == '<'`, honouring quoted attributes.
fn tag_end(s: &str) -> Option<usize> {
    let mut quote: Option<u8> = None;
    for (i, &b) in s.as_bytes().iter().enumerate().skip(1) {
        match quote {
            Some(q) if b == q => quote = None,
            Some(_) => {}
            None if b == b'"' || b == b'\'' => quote = Some(b),
            None if b == b'>' => return Some(i),
            None => {}
        }
    }
    None
}

fn parse_attributes(s: &str) -> Vec<(String, String)> {
    let b = s.as_bytes();
    let len = b.len();
    let mut attrs = Vec::new();
    let mut i = 0;
    loop {
        while i < len && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= len {
            break;
        }
        let start = i;
        while i < len && !b[i].is_ascii_whitespace() && b[i] != b'=' {
            i += 1;
        }
        let name = s[start..i].to_ascii_lowercase();
        while i < len && b[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut value = "";
        if i < len && b[i] == b'=' {
            i += 1;
            while i < len && b[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < len && (b[i] == b'"' || b[i] == b'\'') {
                let q = b[i];
                i += 1;
                let vs = i;
                while i < len && b[i] != q {
                    i += 1;
                }
                value = &s[vs..i];
                i = (i + 1).min(len);
            } else {
                let vs = i;
                while i < len && !b[i].is_ascii_whitespace() {
                    i += 1;
                }
                value = &s[vs..i];
            }
        }
        if !name.is_empty() {
            attrs.push((name, decode_entities(value).into_owned()));
        }
    }
    attrs
}

fn parse_alignment(value: &str) -> Option<Alignment> {
    match value.trim().to_ascii_lowercase().as_str() {
        "left" | "start" => Some(Alignment::LEFT),
        "right" | "end" => Some(Alignment::RIGHT),
        "center" | "middle" => Some(Alignment::H_CENTER),
        "justify" => Some(Alignment::JUSTIFY),
        _ => None,
    }
}

fn parse_font_weight(value: &str) -> Option<FontWeight> {
    match value {
        "bold" | "bolder" => Some(FontWeight::Bold),
        "normal" | "lighter" => Some(FontWeight::Normal),
        n => {
            let w: u16 = n.parse().ok()?;
            Some(match w {
                0..=150 => FontWeight::Thin,
                151..=250 => FontWeight::ExtraLight,
                251..=350 => FontWeight::Light,
                351..=450 => FontWeight::Normal,
                451..=550 => FontWeight::Medium,
                551..=650 => FontWeight::SemiBold,
                651..=750 => FontWeight::Bold,
                751..=850 => FontWeight::ExtraBold,
                _ => FontWeight::Black,
            })
        }
    }
}

/// Applies inline CSS declarations. Returns `true` when the element requests preformatted whitespace.
fn apply_css(
    style: &str,
    fmt: &mut TextCharFormat,
    mut block: Option<&mut TextBlockFormat>,
) -> bool {
    let mut preformatted = false;
    for decl in style.split(';') {
        let Some((prop, value)) = decl.split_once(':') else {
            continue;
        };
        let prop = prop.trim().to_ascii_lowercase();
        let value = value.trim();
        let lower = value.to_ascii_lowercase();
        match prop.as_str() {
            "color" => {
                if let Some(c) = parse_color(value) {
                    fmt.foreground = Some(c);
                }
            }
            "background-color" | "background" => {
                if let Some(c) = parse_color(value) {
                    fmt.background = Some(c);
                }
            }
            "font-weight" => {
                if let Some(w) = parse_font_weight(&lower) {
                    fmt.font_weight = Some(w);
                }
            }
            "font-style" => fmt.font_italic = Some(lower == "italic" || lower == "oblique"),
            "text-decoration" | "text-decoration-line" => {
                set_underline(fmt, lower.contains("underline"));
                fmt.font_strikeout = lower.contains("line-through");
                fmt.font_overline = lower.contains("overline");
            }
            "vertical-align" => {
                fmt.vertical_alignment = match lower.as_str() {
                    "super" => VerticalAlignment::AlignSuperScript,
                    "sub" => VerticalAlignment::AlignSubScript,
                    "middle" => VerticalAlignment::AlignMiddle,
                    "top" => VerticalAlignment::AlignTop,
                    "bottom" => VerticalAlignment::AlignBottom,
                    _ => VerticalAlignment::AlignNormal,
                }
            }
            "text-align" => {
                if let (Some(b), Some(a)) = (block.as_deref_mut(), parse_alignment(&lower)) {
                    b.alignment = a;
                }
            }
            "white-space" => preformatted = lower == "pre" || lower == "pre-wrap",
            _ => {}
        }
    }
    preformatted
}

/// Parses a CSS/HTML color: `#rgb`, `#rrggbb`, `#aarrggbb` (Qt's `QColor::fromString` order),
/// `rgb(r,g,b)`, `rgba(r,g,b,a)` or a basic named color.
pub(crate) fn parse_color(value: &str) -> Option<Color> {
    let v = value.trim();
    if let Some(hex) = v.strip_prefix('#') {
        return parse_hex(hex);
    }
    let lower = v.to_ascii_lowercase();
    if let Some(args) = lower
        .strip_prefix("rgba(")
        .or_else(|| lower.strip_prefix("rgb("))
    {
        let parts: Vec<&str> = args
            .trim_end_matches(')')
            .split(',')
            .map(str::trim)
            .collect();
        if parts.len() < 3 {
            return None;
        }
        let r: u8 = parts[0].parse().ok()?;
        let g: u8 = parts[1].parse().ok()?;
        let b: u8 = parts[2].parse().ok()?;
        let a = match parts.get(3) {
            Some(a) => (a.parse::<f32>().ok()?.clamp(0.0, 1.0) * 255.0).round() as u8,
            None => 255,
        };
        return Some(Color::from_rgba8(r, g, b, a));
    }
    let (r, g, b) = match lower.as_str() {
        "black" => (0, 0, 0),
        "white" => (255, 255, 255),
        "red" => (255, 0, 0),
        "green" => (0, 128, 0),
        "lime" => (0, 255, 0),
        "blue" => (0, 0, 255),
        "yellow" => (255, 255, 0),
        "cyan" | "aqua" => (0, 255, 255),
        "magenta" | "fuchsia" => (255, 0, 255),
        "gray" | "grey" => (128, 128, 128),
        "silver" => (192, 192, 192),
        "maroon" => (128, 0, 0),
        "olive" => (128, 128, 0),
        "navy" => (0, 0, 128),
        "purple" => (128, 0, 128),
        "teal" => (0, 128, 128),
        "orange" => (255, 165, 0),
        _ => {
            // Legacy HTML allows hex colors without the leading '#'.
            return parse_hex(v);
        }
    };
    Some(Color::from_rgba8(r, g, b, 255))
}

fn parse_hex(s: &str) -> Option<Color> {
    if !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&s[i..i + 2], 16).ok();
    match s.len() {
        3 => {
            let nib = |i: usize| u8::from_str_radix(&s[i..i + 1], 16).ok().map(|v| v * 17);
            Some(Color::from_rgba8(nib(0)?, nib(1)?, nib(2)?, 255))
        }
        6 => Some(Color::from_rgba8(byte(0)?, byte(2)?, byte(4)?, 255)),
        8 => Some(Color::from_rgba8(byte(2)?, byte(4)?, byte(6)?, byte(0)?)),
        _ => None,
    }
}

fn collapse_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_space = false;
    for c in s.chars() {
        if c.is_ascii_whitespace() {
            if !last_space {
                out.push(' ');
            }
            last_space = true;
        } else {
            out.push(c);
            last_space = false;
        }
    }
    out
}

/// Decodes HTML character references (`&amp;`, `&#169;`, `&#xA9;`, common named entities).
pub(crate) fn decode_entities(s: &str) -> Cow<'_, str> {
    if !s.contains('&') {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let semi = rest
            .char_indices()
            .take(12)
            .find(|&(_, c)| c == ';')
            .map(|(i, _)| i);
        if let Some(c) = semi.and_then(|semi| entity_char(&rest[1..semi])) {
            out.push(c);
            rest = &rest[semi.unwrap_or(0) + 1..];
        } else {
            out.push('&');
            rest = &rest[1..];
        }
    }
    out.push_str(rest);
    Cow::Owned(out)
}

fn entity_char(entity: &str) -> Option<char> {
    if let Some(num) = entity.strip_prefix('#') {
        let code = match num.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => num.parse().ok()?,
        };
        return char::from_u32(code);
    }
    Some(match entity {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{a0}',
        "copy" => '©',
        "reg" => '®',
        "trade" => '™',
        "mdash" => '—',
        "ndash" => '–',
        "hellip" => '…',
        "laquo" => '«',
        "raquo" => '»',
        "lsquo" => '‘',
        "rsquo" => '’',
        "ldquo" => '“',
        "rdquo" => '”',
        "bull" => '•',
        "middot" => '·',
        "deg" => '°',
        "times" => '×',
        "divide" => '÷',
        "euro" => '€',
        "pound" => '£',
        "yen" => '¥',
        "cent" => '¢',
        "sect" => '§',
        "para" => '¶',
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// HTML export
// ---------------------------------------------------------------------------

fn escape_html_into(out: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\u{a0}' => out.push_str("&nbsp;"),
            c => out.push(c),
        }
    }
}

fn color_hex(c: Color) -> String {
    let u = c.to_color_u8();
    if u.alpha() == 255 {
        format!("#{:02x}{:02x}{:02x}", u.red(), u.green(), u.blue())
    } else {
        // QColor::fromString reads 8 hex digits as #AARRGGBB.
        format!(
            "#{:02x}{:02x}{:02x}{:02x}",
            u.alpha(),
            u.red(),
            u.green(),
            u.blue()
        )
    }
}

/// Whether HTML whitespace collapsing would alter the block text.
fn needs_preserved_whitespace(text: &str) -> bool {
    text.starts_with(' ')
        || text.ends_with(' ')
        || text.contains("  ")
        || text.contains(['\t', '\r', '\n'])
}

/// Serializes blocks to HTML that `parse_html` re-imports losslessly.
pub(crate) fn write_html(blocks: &[TextBlock], title: &str) -> String {
    let mut html = String::new();
    if !title.is_empty() {
        html.push_str("<html><head><title>");
        escape_html_into(&mut html, title);
        html.push_str("</title></head><body>\n");
    }
    for b in blocks {
        let tag = match b.block_format.heading_level {
            1 => "h1",
            2 => "h2",
            3 => "h3",
            4 => "h4",
            5 => "h5",
            6 => "h6",
            _ => "p",
        };
        html.push('<');
        html.push_str(tag);
        // `QTextHtmlExporter::emitAlignment` (qtextdocument.cpp:2833-2843): nothing for Left.
        let align = b.block_format.alignment;
        if !align.contains(Alignment::LEFT) {
            let named = [
                (Alignment::RIGHT, "right"),
                (Alignment::H_CENTER, "center"),
                (Alignment::JUSTIFY, "justify"),
            ]
            .into_iter()
            .find(|(flag, _)| align.contains(*flag));
            if let Some((_, name)) = named {
                html.push_str(" align=\"");
                html.push_str(name);
                html.push('"');
            }
        }
        if needs_preserved_whitespace(&b.text()) {
            html.push_str(" style=\"white-space:pre-wrap\"");
        }
        html.push('>');
        for f in &b.fragments {
            write_html_fragment(&mut html, f);
        }
        html.push_str("</");
        html.push_str(tag);
        html.push_str(">\n");
    }
    if !title.is_empty() {
        html.push_str("</body></html>\n");
    }
    html
}

fn write_html_fragment(html: &mut String, f: &TextFragment) {
    let fmt = &f.format;
    let mut close: Vec<&str> = Vec::new();
    if fmt.anchor_href.is_some() || fmt.anchor_name.is_some() {
        html.push_str("<a");
        if let Some(href) = &fmt.anchor_href {
            html.push_str(" href=\"");
            escape_html_into(html, href);
            html.push('"');
        }
        if let Some(name) = &fmt.anchor_name {
            html.push_str(" name=\"");
            escape_html_into(html, name);
            html.push('"');
        }
        html.push('>');
        close.push("</a>");
    }
    let in_link = fmt.anchor_href.is_some();
    let mut open = |html: &mut String, tag: &'static str, closing: &'static str| {
        html.push_str(tag);
        close.push(closing);
    };
    if is_bold(fmt) {
        open(html, "<b>", "</b>");
    }
    if fmt.font_italic.unwrap_or(false) {
        open(html, "<i>", "</i>");
    }
    if fmt.font_underline && !in_link {
        open(html, "<u>", "</u>");
    }
    if fmt.font_strikeout {
        open(html, "<s>", "</s>");
    }
    match fmt.vertical_alignment {
        VerticalAlignment::AlignSuperScript => open(html, "<sup>", "</sup>"),
        VerticalAlignment::AlignSubScript => open(html, "<sub>", "</sub>"),
        _ => {}
    }
    if let Some(fg) = fmt.foreground {
        if !(in_link && fg == link_color()) {
            html.push_str("<font color=\"");
            html.push_str(&color_hex(fg));
            html.push_str("\">");
            close.push("</font>");
        }
    }
    if let Some(bg) = fmt.background {
        html.push_str("<span style=\"background-color:");
        html.push_str(&color_hex(bg));
        html.push_str("\">");
        close.push("</span>");
    }
    escape_html_into(html, &f.text);
    for tag in close.iter().rev() {
        html.push_str(tag);
    }
}

// ---------------------------------------------------------------------------
// Markdown import
// ---------------------------------------------------------------------------

/// Parses CommonMark-style Markdown into document blocks.
pub(crate) fn parse_markdown(md: &str) -> Vec<TextBlock> {
    let mut blocks = Vec::new();
    let mut paragraph: Vec<&str> = Vec::new();
    let mut fence: Option<&str> = None;

    fn flush(blocks: &mut Vec<TextBlock>, paragraph: &mut Vec<&str>, heading: u8) {
        if paragraph.is_empty() {
            return;
        }
        let text = paragraph.join(" ");
        paragraph.clear();
        blocks.push(inline_block(&text, heading));
    }

    for line in md.lines() {
        if let Some(marker) = fence {
            if line.trim_start().starts_with(marker) {
                fence = None;
            } else {
                let mut block = TextBlock::new(0, 0);
                if !line.is_empty() {
                    block.fragments.push(TextFragment::plain(line));
                }
                blocks.push(block);
            }
            continue;
        }
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            flush(&mut blocks, &mut paragraph, 0);
            fence = Some(&trimmed[..3]);
            continue;
        }
        if trimmed.is_empty() {
            flush(&mut blocks, &mut paragraph, 0);
            continue;
        }
        if let Some((level, content)) = atx_heading(trimmed) {
            flush(&mut blocks, &mut paragraph, 0);
            blocks.push(inline_block(content, level));
            continue;
        }
        let underline = trimmed.trim_end();
        if !paragraph.is_empty() && !underline.is_empty() {
            if underline.bytes().all(|b| b == b'=') {
                flush(&mut blocks, &mut paragraph, 1);
                continue;
            }
            if underline.bytes().all(|b| b == b'-') {
                flush(&mut blocks, &mut paragraph, 2);
                continue;
            }
        }
        if is_thematic_break(underline) {
            flush(&mut blocks, &mut paragraph, 0);
            continue;
        }
        if is_list_item(trimmed) || trimmed.starts_with('>') {
            flush(&mut blocks, &mut paragraph, 0);
        }
        let hard_break = line.ends_with("  ") || (line.ends_with('\\') && !line.ends_with("\\\\"));
        let content = if hard_break {
            line.trim_end_matches('\\').trim_end()
        } else {
            line.trim()
        };
        paragraph.push(content.trim_start());
        if hard_break {
            flush(&mut blocks, &mut paragraph, 0);
        }
    }
    flush(&mut blocks, &mut paragraph, 0);
    blocks
}

fn atx_heading(line: &str) -> Option<(u8, &str)> {
    let level = line.bytes().take_while(|&b| b == b'#').count();
    if level == 0 || level > 6 {
        return None;
    }
    let rest = &line[level..];
    if !rest.is_empty() && !rest.starts_with([' ', '\t']) {
        return None;
    }
    let mut content = rest.trim();
    // Optional closing sequence of '#'.
    let without_closing = content.trim_end_matches('#');
    if without_closing.is_empty() || without_closing.ends_with([' ', '\t']) {
        content = without_closing.trim_end();
    }
    Some((level as u8, content))
}

fn is_thematic_break(line: &str) -> bool {
    let compact: Vec<u8> = line.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    compact.len() >= 3
        && matches!(compact[0], b'-' | b'*' | b'_')
        && compact.iter().all(|&b| b == compact[0])
}

fn is_list_item(line: &str) -> bool {
    if line.starts_with("- ") || line.starts_with("* ") || line.starts_with("+ ") {
        return true;
    }
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    digits > 0
        && digits <= 9
        && (line[digits..].starts_with(". ") || line[digits..].starts_with(") "))
}

fn inline_block(text: &str, heading: u8) -> TextBlock {
    let mut block = TextBlock::new(0, 0);
    block.block_format.heading_level = heading;
    parse_inline(text, &TextCharFormat::default(), &mut block.fragments);
    block.normalize_fragments();
    block
}

fn push_fragment(out: &mut Vec<TextFragment>, buf: &mut String, fmt: &TextCharFormat) {
    if buf.is_empty() {
        return;
    }
    let text = std::mem::take(buf);
    match out.last_mut() {
        Some(last) if last.format == *fmt => last.text.push_str(&text),
        _ => out.push(TextFragment::new(text, fmt.clone())),
    }
}

fn run_length(s: &str, c: char) -> usize {
    s.chars().take_while(|&x| x == c).count()
}

/// Finds a closing delimiter run of exactly `run` × `delim` in `s`, preceded by non-whitespace.
fn find_closing(s: &str, delim: char, run: usize) -> Option<usize> {
    let mut prev: Option<char> = None;
    let mut iter = s.char_indices().peekable();
    while let Some((i, c)) = iter.next() {
        if c == '\\' {
            prev = iter.next().map(|(_, n)| n);
            continue;
        }
        if c == delim {
            let len = run_length(&s[i..], delim);
            let after = s[i + len..].chars().next();
            let intraword = delim == '_' && after.is_some_and(char::is_alphanumeric);
            if len == run && prev.is_some_and(|p| !p.is_whitespace()) && !intraword {
                return Some(i);
            }
            for _ in 1..len {
                iter.next();
            }
            prev = Some(delim);
            continue;
        }
        prev = Some(c);
    }
    None
}

/// Maximum nesting of emphasis/link spans; deeper content is kept as literal text.
const MAX_INLINE_DEPTH: usize = 32;

fn parse_inline(text: &str, fmt: &TextCharFormat, out: &mut Vec<TextFragment>) {
    parse_inline_at(text, fmt, out, 0);
}

fn parse_inline_at(text: &str, fmt: &TextCharFormat, out: &mut Vec<TextFragment>, depth: usize) {
    if depth > MAX_INLINE_DEPTH {
        let mut literal = text.to_string();
        push_fragment(out, &mut literal, fmt);
        return;
    }
    let mut buf = String::new();
    let mut i = 0;
    let mut prev: Option<char> = None;
    while i < text.len() {
        let rest = &text[i..];
        let c = rest.chars().next().unwrap_or('\0');
        match c {
            '\\' => {
                if let Some(next) = rest[1..].chars().next().filter(char::is_ascii_punctuation) {
                    buf.push(next);
                    prev = Some(next);
                    i += 1 + next.len_utf8();
                    continue;
                }
            }
            '`' => {
                let run = run_length(rest, '`');
                let fence = &rest[..run];
                if let Some(close) = rest[run..].find(fence) {
                    let code = &rest[run..run + close];
                    let code = if code.len() >= 2
                        && code.starts_with(' ')
                        && code.ends_with(' ')
                        && !code.trim().is_empty()
                    {
                        &code[1..code.len() - 1]
                    } else {
                        code
                    };
                    buf.push_str(code);
                    prev = Some('`');
                    i += run + close + run;
                    continue;
                }
                buf.push_str(fence);
                prev = Some('`');
                i += run;
                continue;
            }
            '*' | '_' | '~' => {
                let run = run_length(rest, c);
                let usable = match c {
                    '~' => run == 2,
                    _ => run <= 3,
                };
                let next = rest[run..].chars().next();
                let left_flanking = next.is_some_and(|n| !n.is_whitespace());
                let intraword = c == '_' && prev.is_some_and(char::is_alphanumeric);
                if usable && left_flanking && !intraword {
                    if let Some(close) = find_closing(&rest[run..], c, run) {
                        push_fragment(out, &mut buf, fmt);
                        let mut inner = fmt.clone();
                        match (c, run) {
                            ('~', _) => inner.font_strikeout = true,
                            (_, 1) => inner.font_italic = Some(true),
                            (_, 2) => inner.font_weight = Some(FontWeight::Bold),
                            _ => {
                                inner.font_weight = Some(FontWeight::Bold);
                                inner.font_italic = Some(true);
                            }
                        }
                        parse_inline_at(&rest[run..run + close], &inner, out, depth + 1);
                        prev = Some(c);
                        i += run + close + run;
                        continue;
                    }
                }
                buf.push_str(&rest[..run * c.len_utf8()]);
                prev = Some(c);
                i += run * c.len_utf8();
                continue;
            }
            '[' => {
                if let Some((label, href, consumed)) = parse_link(rest) {
                    push_fragment(out, &mut buf, fmt);
                    let mut inner = fmt.clone();
                    apply_link_style(&mut inner, href);
                    parse_inline_at(label, &inner, out, depth + 1);
                    prev = Some(')');
                    i += consumed;
                    continue;
                }
            }
            '<' => {
                if let Some(end) = rest.find('>') {
                    let url = &rest[1..end];
                    if url.contains(':') && !url.contains(char::is_whitespace) && !url.contains('<')
                    {
                        push_fragment(out, &mut buf, fmt);
                        let mut inner = fmt.clone();
                        apply_link_style(&mut inner, url.to_string());
                        let mut label = url.to_string();
                        push_fragment(out, &mut label, &inner);
                        prev = Some('>');
                        i += end + 1;
                        continue;
                    }
                }
            }
            _ => {}
        }
        buf.push(c);
        prev = Some(c);
        i += c.len_utf8();
    }
    push_fragment(out, &mut buf, fmt);
}

/// Parses `[label](destination "title")` at the start of `s`.
/// Returns the label, the destination and the number of bytes consumed.
fn parse_link(s: &str) -> Option<(&str, String, usize)> {
    let mut depth = 0usize;
    let mut label_end = None;
    let mut iter = s.char_indices();
    while let Some((i, c)) = iter.next() {
        match c {
            '\\' => {
                iter.next();
            }
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    label_end = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let label_end = label_end?;
    let after = &s[label_end + 1..];
    let dest_src = after.strip_prefix('(')?;
    let close = dest_src.find(')')?;
    let inner = dest_src[..close].trim();
    let dest = match inner.strip_prefix('<') {
        Some(bracketed) => bracketed.split('>').next().unwrap_or(""),
        None => inner.split_whitespace().next().unwrap_or(""),
    };
    Some((
        &s[1..label_end],
        dest.to_string(),
        label_end + 2 + close + 1,
    ))
}

// ---------------------------------------------------------------------------
// Markdown export
// ---------------------------------------------------------------------------

fn escape_markdown_into(out: &mut String, text: &str, at_block_start: bool) {
    for (i, c) in text.chars().enumerate() {
        if matches!(c, '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '~')
            || (i == 0 && at_block_start && c == '#')
        {
            out.push('\\');
        }
        out.push(c);
    }
}

/// Serializes blocks to Markdown that `parse_markdown` re-imports losslessly for supported formats.
pub(crate) fn write_markdown(blocks: &[TextBlock]) -> String {
    let mut md = String::new();
    for (i, b) in blocks.iter().enumerate() {
        if i > 0 {
            md.push_str("\n\n");
        }
        let heading = b.block_format.heading_level;
        if heading > 0 {
            for _ in 0..heading {
                md.push('#');
            }
            md.push(' ');
        }
        for (j, f) in b.fragments.iter().enumerate() {
            write_markdown_fragment(&mut md, f, heading > 0, heading == 0 && j == 0);
        }
    }
    md
}

fn write_markdown_fragment(
    md: &mut String,
    f: &TextFragment,
    in_heading: bool,
    at_block_start: bool,
) {
    let fmt = &f.format;
    let core = f.text.trim_matches(' ');
    if core.is_empty() {
        md.push_str(&f.text);
        return;
    }
    let lead = &f.text[..f.text.len() - f.text.trim_start_matches(' ').len()];
    let trail = &f.text[f.text.trim_end_matches(' ').len()..];
    md.push_str(lead);
    let mut close = String::new();
    if fmt.anchor_href.is_some() {
        md.push('[');
    }
    if is_bold(fmt) && !in_heading {
        md.push_str("**");
        close.insert_str(0, "**");
    }
    if fmt.font_italic.unwrap_or(false) {
        md.push('*');
        close.insert(0, '*');
    }
    if fmt.font_strikeout {
        md.push_str("~~");
        close.insert_str(0, "~~");
    }
    escape_markdown_into(
        md,
        core,
        at_block_start && lead.is_empty() && close.is_empty() && fmt.anchor_href.is_none(),
    );
    md.push_str(&close);
    if let Some(href) = &fmt.anchor_href {
        md.push_str("](");
        md.push_str(href);
        md.push(')');
    }
    md.push_str(trail);
}
