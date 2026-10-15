// SPDX-License-Identifier: LGPL-3.0-only
//! Qt Style Sheet (QSS) Engine matching `qtbase/src/widgets/styles/qstylesheetstyle.cpp`.
//!
//! Provides cascading style resolution, specificity scoring, and resolved style metrics
//! for widgets, labels, buttons, and progress bars.

use std::sync::Arc;

use ferrokit_gui::text::qcssparser::{
    QCssBasicSelector, QCssDeclaration, QCssProperty, QCssStyleSheet, QCssValue,
};
use ferrokit_gui::tiny_skia::Color;

/// Fully resolved concrete style properties for a widget or sub-control.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ResolvedStyle {
    pub background_color: Option<Color>,
    pub color: Option<Color>,
    pub border_color: Option<Color>,
    pub border_width: Option<f32>,
    pub border_style: Option<String>,
    pub border_radius: Option<f32>,
    pub min_height: Option<i32>,
    pub max_height: Option<i32>,
    pub min_width: Option<i32>,
    pub max_width: Option<i32>,
    /// QSS `width`/`height`: the contents size (`QRenderRule::contentsSize`); independent of
    /// `min_*`/`max_*`.
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub font_size: Option<f32>,
    pub font_weight: Option<u16>,
    /// `font-family` list in declaration order (`QFont::setFamilies`); see [`Self::font_family`].
    pub font_families: Option<Vec<String>>,
    pub letter_spacing: Option<f32>,
    pub padding: Option<[f32; 4]>, // [top, right, bottom, left]
    pub margin: Option<[f32; 4]>,
    pub text_align: Option<String>,
}

impl ResolvedStyle {
    /// The family a `font-family` list selects: the first installed one (`QFontDatabase` matches
    /// `QFont::families()` in order), else the first listed (the font database then falls back).
    pub fn font_family(&self) -> Option<String> {
        let list = self.font_families.as_ref()?;
        let installed = ferrokit_gui::text::font_database::with_global_font_database(|db| {
            list.iter().find(|f| db.has_family(f)).cloned()
        });
        installed.or_else(|| list.first().cloned())
    }

    /// Horizontal and vertical extent that `QRenderRule::boxSize` adds around a content size:
    /// border plus padding on both sides of each axis (`qstylesheetstyle.cpp:1116-1121`).
    pub fn box_extra(&self) -> (i32, i32) {
        let border = self.border_width.unwrap_or(0.0).max(0.0);
        let [pt, pr, pb, pl] = self.padding.unwrap_or([0.0; 4]);
        (((2.0 * border) + pl + pr).round() as i32, ((2.0 * border) + pt + pb).round() as i32)
    }

    /// `QWidget::minimumSize()` set by `QStyleSheetStyle::setGeometry`: `min-width`/`min-height`
    /// name the content box, so the box extent is added (`qstylesheetstyle.cpp:2595-2602`).
    /// An axis without a `min-*` declaration is 0.
    pub fn min_box_size(&self) -> (i32, i32) {
        let (ex, ey) = self.box_extra();
        // `width`/`height` only raise the minimum of an axis that also has a `min-*` declaration.
        (
            self.min_width.map_or(0, |m| m.max(self.width.unwrap_or(-1)) + ex),
            self.min_height.map_or(0, |m| m.max(self.height.unwrap_or(-1)) + ey),
        )
    }

    /// `QWidget::maximumSize()` set by `QStyleSheetStyle::setGeometry`, likewise for
    /// `max-width`/`max-height` (`qstylesheetstyle.cpp:2603-2612`). An axis without a `max-*`
    /// declaration is `QWIDGETSIZE_MAX`.
    pub fn max_box_size(&self) -> (i32, i32) {
        let (ex, ey) = self.box_extra();
        // `width`/`height` only lower the maximum of an axis that also has a `max-*` declaration.
        (
            self.max_width.map_or(16777215, |m| m.min(self.width.unwrap_or(16777215)) + ex),
            self.max_height.map_or(16777215, |m| m.min(self.height.unwrap_or(16777215)) + ey),
        )
    }

    /// `QRenderRule::adjustSize` (`qstylesheetstyle.cpp:561-574`): a content size with `width`/
    /// `height` taking the place of the given axis, clamped to `max-*`, then expanded to `min-*`.
    pub fn adjust_size(&self, (w, h): (i32, i32)) -> (i32, i32) {
        let mut w = self.width.unwrap_or(w);
        let mut h = self.height.unwrap_or(h);
        if let Some(max) = self.max_width {
            w = w.min(max);
        }
        if let Some(max) = self.max_height {
            h = h.min(max);
        }
        (w.max(self.min_width.unwrap_or(-1)), h.max(self.min_height.unwrap_or(-1)))
    }
}


/// Context information for querying matching stylesheet rules.
#[derive(Debug, Clone, Default)]
pub struct WidgetStyleContext<'a> {
    pub type_name: &'a str,
    pub object_name: &'a str,
    pub pseudo_states: &'a [&'a str],
    pub sub_control: Option<&'a str>,
    pub attributes: &'a [(String, String)],
}

/// Qt Style Sheet engine instance.
#[derive(Debug, Clone, Default)]
pub struct QStyleSheetStyle {
    sheet: Arc<QCssStyleSheet>,
}

impl QStyleSheetStyle {
    /// Creates a style from an existing parsed AST.
    pub fn new(sheet: Arc<QCssStyleSheet>) -> Self {
        Self { sheet }
    }

    /// Parses a QSS string and builds an engine instance.
    pub fn parse(qss: &str) -> Self {
        Self {
            sheet: Arc::new(QCssStyleSheet::parse(qss)),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.sheet.is_empty()
    }

    /// Resolves matching styles for a widget context.
    pub fn resolve(&self, ctx: &WidgetStyleContext) -> ResolvedStyle {
        Self::resolve_chain(&[self], ctx)
    }

    /// Resolves a widget through every style sheet that applies to it, lowest precedence first.
    ///
    /// As in `QStyleSheetStyle::styleRules`, a matching rule is weighted by the depth of the
    /// sheet it came from, then by its selector's specificity, then by its position: the
    /// application sheet, then each ancestor from the outermost inwards, then the widget's own
    /// sheet. Declarations are applied in that order, so a later sheet overrides an earlier one
    /// whatever the selector specificity, and `padding-left` composes with a `padding` set by
    /// another sheet.
    pub fn resolve_chain(sheets: &[&QStyleSheetStyle], ctx: &WidgetStyleContext) -> ResolvedStyle {
        let mut matched: Vec<(usize, u32, &QCssDeclaration)> = Vec::new();
        for (depth, sheet) in sheets.iter().enumerate() {
            for rule in &sheet.sheet.rules {
                let best = rule
                    .selectors
                    .iter()
                    .filter(|selector| selector_matches(selector, ctx))
                    .map(|selector| selector.specificity())
                    .max();
                if let Some(score) = best {
                    matched.extend(rule.declarations.iter().map(|decl| (depth, score, decl)));
                }
            }
        }

        // Stable: equal weights keep rule order, so the later rule wins.
        matched.sort_by_key(|&(depth, score, _)| (depth, score));

        let mut resolved = ResolvedStyle::default();
        for (_, _, decl) in matched {
            apply_declaration(&mut resolved, decl);
        }
        resolved
    }

    /// Cascades a local stylesheet with a fallback (the application stylesheet).
    pub fn resolve_cascaded(
        local: Option<&QStyleSheetStyle>,
        fallback: Option<&QStyleSheetStyle>,
        ctx: &WidgetStyleContext,
    ) -> ResolvedStyle {
        let sheets: Vec<&QStyleSheetStyle> = fallback.into_iter().chain(local).collect();
        Self::resolve_chain(&sheets, ctx)
    }
}

/// Checks whether a selector matches a widget context.
fn selector_matches(selector: &QCssBasicSelector, ctx: &WidgetStyleContext) -> bool {
    // 1. Sub-control match
    if selector.sub_control.as_deref() != ctx.sub_control {
        return false;
    }

    // 2. ID match (e.g. `#CentralWidget`)
    if let Some(id) = &selector.id {
        if id != ctx.object_name {
            return false;
        }
    }

    // 3. Type name match (e.g. `QLabel`, `QProgressBar`): the class or any class it inherits.
    if let Some(elem) = &selector.element_name {
        if elem != "*" && !node_name_equals(ctx.type_name, elem) {
            return false;
        }
    }

    // 4. Pseudo-states match (all selector pseudo-states must be active in ctx)
    for ps in &selector.pseudo_states {
        if !ctx.pseudo_states.contains(&ps.as_str()) {
            return false;
        }
    }

    // 5. Attributes match (all selector attributes must match)
    for (k, v) in &selector.attributes {
        let mut matched = false;
        for (ck, cv) in ctx.attributes {
            if ck == k && cv == v {
                matched = true;
                break;
            }
        }
        if !matched {
            return false;
        }
    }

    true
}

/// Whether a widget of Qt class `type_name` matches the type selector `class`, as
/// `QStyleSheetStyleSelector::nodeNameEquals` decides it (qstylesheetstyle.cpp:1579-1601). A
/// tip (`QTipLabel`) matches `QToolTip` only (1585-1586). Any other widget matches its own class,
/// every class it inherits from, `QWidget` and `QObject`. The base-class pairs are the ones ferrokit
/// models; a class ferrokit does not know matches its own name and the base classes above.
fn node_name_equals(type_name: &str, class: &str) -> bool {
    if type_name == "QToolTip" {
        return class == "QToolTip";
    }
    matches!(class, "QObject" | "QWidget")
        || type_name == class
        || matches!(
            (type_name, class),
            ("QLabel", "QFrame") | ("QPushButton", "QAbstractButton")
        )
}

/// Applies an individual declaration to a `ResolvedStyle`.
fn apply_declaration(style: &mut ResolvedStyle, decl: &QCssDeclaration) {
    match decl.property {
        QCssProperty::BackgroundColor => {
            if let QCssValue::Color(c) = decl.value {
                style.background_color = Some(c);
            }
        }
        QCssProperty::Color => {
            if let QCssValue::Color(c) = decl.value {
                style.color = Some(c);
            }
        }
        QCssProperty::Border => match &decl.value {
            QCssValue::Border(b) => {
                style.border_width = Some(b.width);
                style.border_style = Some(b.style.clone());
                style.border_color = b.color;
            }
            QCssValue::Identifier(s) if s.eq_ignore_ascii_case("none") => {
                style.border_width = Some(0.0);
                style.border_style = Some("none".to_string());
                style.border_color = None;
            }
            _ => {}
        },
        QCssProperty::BorderColor => {
            if let QCssValue::Color(c) = decl.value {
                style.border_color = Some(c);
            }
        }
        QCssProperty::BorderWidth => {
            if let QCssValue::Length(w) = decl.value {
                style.border_width = Some(w);
            }
        }
        QCssProperty::BorderStyle => {
        if let QCssValue::Identifier(s) = &decl.value {
                style.border_style = Some(s.clone());
            }
        }
        QCssProperty::BorderRadius => {
            if let QCssValue::Length(r) = decl.value {
                style.border_radius = Some(r);
            }
        }
        QCssProperty::MinHeight => {
            if let QCssValue::Length(h) = decl.value {
                style.min_height = Some(h.round() as i32);
            }
        }
        QCssProperty::MaxHeight => {
            if let QCssValue::Length(h) = decl.value {
                style.max_height = Some(h.round() as i32);
            }
        }
        QCssProperty::Height => {
            if let QCssValue::Length(h) = decl.value {
                let px = h.round() as i32;
                style.height = Some(px);
            }
        }
        QCssProperty::MinWidth => {
            if let QCssValue::Length(w) = decl.value {
                style.min_width = Some(w.round() as i32);
            }
        }
        QCssProperty::MaxWidth => {
            if let QCssValue::Length(w) = decl.value {
                style.max_width = Some(w.round() as i32);
            }
        }
        QCssProperty::Width => {
            if let QCssValue::Length(w) = decl.value {
                let px = w.round() as i32;
                style.width = Some(px);
            }
        }
        QCssProperty::FontSize => {
            if let QCssValue::Length(s) = decl.value {
                style.font_size = Some(s);
            }
        }
        QCssProperty::FontWeight => match &decl.value {
            QCssValue::Number(n) => {
                style.font_weight = Some(n.round() as u16);
            }
            QCssValue::Identifier(s) => {
                if s.eq_ignore_ascii_case("bold") {
                    style.font_weight = Some(700);
                } else if s.eq_ignore_ascii_case("normal") {
                    style.font_weight = Some(400);
                }
            }
            _ => {}
        },
        QCssProperty::FontFamily => match &decl.value {
            QCssValue::FontFamilies(list) => {
                style.font_families = Some(list.clone());
            }
            _ => {}
        },
        QCssProperty::LetterSpacing => {
            if let QCssValue::Length(ls) = decl.value {
                style.letter_spacing = Some(ls);
            }
        }
        QCssProperty::Padding => {
            if let QCssValue::Edges(e) = decl.value {
                style.padding = Some(e);
            }
        }
        QCssProperty::PaddingTop
        | QCssProperty::PaddingRight
        | QCssProperty::PaddingBottom
        | QCssProperty::PaddingLeft => {
            if let QCssValue::Length(v) = decl.value {
                // [top, right, bottom, left]
                let side = match decl.property {
                    QCssProperty::PaddingTop => 0,
                    QCssProperty::PaddingRight => 1,
                    QCssProperty::PaddingBottom => 2,
                    _ => 3,
                };
                style.padding.get_or_insert([0.0; 4])[side] = v;
            }
        }
        QCssProperty::Margin => {
            if let QCssValue::Edges(e) = decl.value {
                style.margin = Some(e);
            }
        }
        QCssProperty::MarginTop
        | QCssProperty::MarginRight
        | QCssProperty::MarginBottom
        | QCssProperty::MarginLeft => {
            if let QCssValue::Length(v) = decl.value {
                // [top, right, bottom, left]
                let side = match decl.property {
                    QCssProperty::MarginTop => 0,
                    QCssProperty::MarginRight => 1,
                    QCssProperty::MarginBottom => 2,
                    _ => 3,
                };
                style.margin.get_or_insert([0.0; 4])[side] = v;
            }
        }
        QCssProperty::TextAlign => {
        if let QCssValue::Identifier(s) = &decl.value {
                style.text_align = Some(s.clone());
            }
        }
        _ => {}
    }
}
